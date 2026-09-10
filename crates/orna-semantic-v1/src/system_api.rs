//! The checked-in `api/sys.json` contract, decoded without a second schema.
//!
//! This module is deliberately a descriptor boundary: it validates portable
//! names and type references before the semantic layer uses them, but it does
//! not manufacture runtime values or make unavailable operations callable.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const EMBEDDED_SYSTEM_API: &str = include_str!("../../../api/sys.json");
const MAX_JSON_NESTING_DEPTH: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemApi {
    inventory: Inventory,
    singletons: BTreeMap<String, SingletonDescriptor>,
    types: BTreeMap<String, TypeDescriptor>,
    relations: BTreeMap<String, RelationDescriptor>,
    functions: BTreeMap<String, Vec<FunctionDescriptor>>,
    grouped_relations: BTreeMap<String, String>,
    removed: BTreeMap<String, RemovedName>,
    enums: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Inventory {
    pub singletons: usize,
    pub opaque_identifiers: usize,
    pub reference_aliases: usize,
    pub value_types: usize,
    pub enums: usize,
    pub relations: usize,
    pub functions: usize,
    pub failure_codes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SingletonDescriptor {
    pub name: String,
    pub ty: SystemType,
    pub availability: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypeDescriptor {
    pub name: String,
    /// The exact ordered parameters declared by the value-type application.
    /// Keeping this metadata prevents `sys.Foo<T>` and a disconnected JSON
    /// `type_parameters` list from silently describing different contracts.
    pub type_parameters: Vec<String>,
    pub fields: BTreeMap<String, SystemType>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelationDescriptor {
    pub name: String,
    pub grouped_handle: String,
    pub fields: BTreeMap<String, SystemType>,
    pub reference_type: SystemType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FunctionDescriptor {
    pub name: String,
    pub type_parameters: BTreeSet<String>,
    pub parameters: Vec<ParameterDescriptor>,
    pub result: SystemType,
    pub effect: SystemEffect,
}

/// The portable API has a deliberately closed effect vocabulary.  Treating a
/// misspelled effect as pure would make a corrupted descriptor silently grant
/// a less restrictive semantic contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemEffect {
    Read,
    Invoke,
    Admin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParameterDescriptor {
    pub name: String,
    pub ty: SystemType,
    pub has_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemovedName {
    pub replacement: String,
    pub diagnostic: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SystemType {
    Named(String),
    Applied {
        base: String,
        arguments: Vec<SystemType>,
    },
    List(Box<SystemType>),
    Optional(Box<SystemType>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathResolution<'a> {
    ReadOnly(&'a SystemType),
    Removed(&'a RemovedName),
    KnownUnsupported,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemApiError {
    InvalidJson,
    InvalidInventory,
    DuplicateName,
    InvalidName,
    InvalidType,
    UnresolvedType,
    InvalidFunction,
    InvalidEffect,
    InvalidDefault,
    InvalidFailureCode,
    WritableRelation,
    InvalidRelationAlias,
    InvalidRemovedName,
}

impl SystemApi {
    pub(crate) fn embedded() -> Result<Self, SystemApiError> {
        Self::from_json(EMBEDDED_SYSTEM_API)
    }

    pub(crate) fn from_json(source: &str) -> Result<Self, SystemApiError> {
        let raw = raw_document(source)?;
        let inventory = Inventory {
            singletons: raw.counts.singletons,
            opaque_identifiers: raw.counts.opaque_identifiers,
            reference_aliases: raw.counts.reference_aliases,
            value_types: raw.counts.value_types,
            enums: raw.counts.enums,
            relations: raw.counts.relations,
            functions: raw.counts.functions,
            failure_codes: raw.counts.failure_codes,
        };
        if inventory.singletons != raw.singletons.len()
            || inventory.opaque_identifiers != raw.opaque_identifiers.len()
            || inventory.reference_aliases != raw.reference_aliases.len()
            || inventory.value_types != raw.value_types.len()
            || inventory.enums != raw.enums.len()
            || inventory.relations != raw.relations.len()
            || inventory.functions != raw.functions.len()
            || inventory.failure_codes != raw.failure_codes.len()
        {
            return Err(SystemApiError::InvalidInventory);
        }

        let mut type_arities = language_type_arities();
        let mut types = BTreeMap::new();
        for name in raw.opaque_identifiers {
            insert_type_arity(&mut type_arities, &name, 0)?;
            insert_descriptor(
                &mut types,
                name.clone(),
                TypeDescriptor {
                    name,
                    type_parameters: Vec::new(),
                    fields: BTreeMap::new(),
                },
            )?;
        }
        for alias in &raw.reference_aliases {
            let name = plain_type_name(&alias.name)?;
            insert_type_arity(&mut type_arities, &name, 0)?;
            insert_descriptor(
                &mut types,
                name.clone(),
                TypeDescriptor {
                    name,
                    type_parameters: Vec::new(),
                    fields: BTreeMap::new(),
                },
            )?;
        }
        for (name, variants) in &raw.enums {
            insert_type_arity(&mut type_arities, name, 0)?;
            let mut unique_variants = BTreeSet::new();
            for variant in variants {
                if !valid_identifier(variant) {
                    return Err(SystemApiError::InvalidName);
                }
                if !unique_variants.insert(variant) {
                    return Err(SystemApiError::DuplicateName);
                }
            }
        }
        for value in &raw.value_types {
            let (name, parameters) = value_type_declaration(&value.name, &value.type_parameters)?;
            insert_type_arity(&mut type_arities, &name, parameters.len())?;
        }
        for relation in &raw.relations {
            insert_type_arity(&mut type_arities, &relation.name, 0)?;
        }

        let mut enums = BTreeMap::new();
        for (name, variants) in raw.enums {
            if enums.insert(name, variants.into_iter().collect()).is_some() {
                return Err(SystemApiError::DuplicateName);
            }
        }
        for value in raw.value_types {
            let (name, type_parameters) =
                value_type_declaration(&value.name, &value.type_parameters)?;
            let parameters = type_parameters.iter().cloned().collect();
            let fields = fields(value.fields, &type_arities, &parameters)?;
            insert_descriptor(
                &mut types,
                name.clone(),
                TypeDescriptor {
                    name,
                    type_parameters,
                    fields,
                },
            )?;
        }

        let mut relation_aliases = BTreeMap::new();
        for alias in &raw.reference_aliases {
            let name = plain_type_name(&alias.name)?;
            if relation_aliases
                .insert(name, alias.target.clone())
                .is_some()
            {
                return Err(SystemApiError::DuplicateName);
            }
        }

        let mut relations = BTreeMap::new();
        let mut grouped_relations = BTreeMap::new();
        for relation in raw.relations {
            if relation.writable {
                return Err(SystemApiError::WritableRelation);
            }
            validate_path(&relation.name)?;
            validate_path(&relation.grouped_handle)?;
            let fields = fields(relation.fields, &type_arities, &BTreeSet::new())?;
            let reference_type =
                parse_and_validate_type(&relation.reference_type, &type_arities, &BTreeSet::new())?;
            if relation_aliases.get(&relation.reference_type) != Some(&relation.name) {
                return Err(SystemApiError::InvalidRelationAlias);
            }
            if grouped_relations
                .insert(relation.grouped_handle.clone(), relation.name.clone())
                .is_some()
            {
                return Err(SystemApiError::DuplicateName);
            }
            insert_descriptor(
                &mut relations,
                relation.name.clone(),
                RelationDescriptor {
                    name: relation.name,
                    grouped_handle: relation.grouped_handle,
                    fields,
                    reference_type,
                },
            )?;
        }
        for alias in &raw.reference_aliases {
            if !relations.contains_key(&alias.target) {
                return Err(SystemApiError::UnresolvedType);
            }
            let definition =
                parse_and_validate_type(&alias.definition, &type_arities, &BTreeSet::new())?;
            let expected = SystemType::Applied {
                base: "sys.RowRef".into(),
                arguments: vec![SystemType::Named(alias.target.clone())],
            };
            if definition != expected {
                return Err(SystemApiError::InvalidRelationAlias);
            }
        }

        let mut singletons = BTreeMap::new();
        for singleton in raw.singletons {
            validate_path(&singleton.name)?;
            let ty = parse_and_validate_type(&singleton.ty, &type_arities, &BTreeSet::new())?;
            insert_descriptor(
                &mut singletons,
                singleton.name.clone(),
                SingletonDescriptor {
                    name: singleton.name,
                    ty,
                    availability: singleton.availability,
                },
            )?;
        }

        let mut functions = BTreeMap::<String, Vec<FunctionDescriptor>>::new();
        let mut parsed_functions = Vec::new();
        let mut function_signatures = BTreeSet::new();
        for raw_function in raw.functions {
            let descriptor = parse_function(
                &raw_function.signature,
                raw_function.effect,
                &type_arities,
                &singletons,
                &types,
                &enums,
            )?;
            let identity = function_identity(&descriptor);
            if !function_signatures.insert(identity) {
                return Err(SystemApiError::DuplicateName);
            }
            parsed_functions.push((raw_function.name, descriptor));
        }
        for (label, descriptor) in &parsed_functions {
            if !valid_function_label(label, descriptor, &parsed_functions) {
                return Err(SystemApiError::InvalidFunction);
            }
        }
        for (_, descriptor) in parsed_functions {
            functions
                .entry(descriptor.name.clone())
                .or_default()
                .push(descriptor);
        }

        let mut failure_codes = BTreeSet::new();
        for code in raw.failure_codes {
            if !valid_failure_code(&code) || !failure_codes.insert(code) {
                return Err(SystemApiError::InvalidFailureCode);
            }
        }

        let mut removed = BTreeMap::new();
        for (name, descriptor) in raw.removed_names {
            validate_path(&name)?;
            validate_path(&descriptor.replacement)?;
            if !valid_diagnostic(&descriptor.diagnostic)
                || !portable_path_exists(
                    &descriptor.replacement,
                    &singletons,
                    &types,
                    &relations,
                    &functions,
                    &grouped_relations,
                    &enums,
                )
                || name == descriptor.replacement
                || singletons.contains_key(&name)
                || functions.contains_key(&name)
                || relations.contains_key(&name)
                || grouped_relations.contains_key(&name)
                || types.contains_key(&name)
                || enums.contains_key(&name)
                || removed
                    .insert(
                        name,
                        RemovedName {
                            replacement: descriptor.replacement,
                            diagnostic: descriptor.diagnostic,
                        },
                    )
                    .is_some()
            {
                return Err(SystemApiError::InvalidRemovedName);
            }
        }

        Ok(Self {
            inventory,
            singletons,
            types,
            relations,
            functions,
            grouped_relations,
            removed,
            enums,
        })
    }

    #[cfg(test)]
    pub(crate) fn inventory(&self) -> Inventory {
        self.inventory
    }

    #[cfg(test)]
    pub(crate) fn singleton(&self, name: &str) -> Option<&SingletonDescriptor> {
        self.singletons.get(name)
    }

    pub(crate) fn function(&self, name: &str) -> Option<&[FunctionDescriptor]> {
        self.functions.get(name).map(Vec::as_slice)
    }

    pub(crate) fn relation(&self, path: &str) -> Option<&RelationDescriptor> {
        self.relations.get(path).or_else(|| {
            self.grouped_relations
                .get(path)
                .and_then(|name| self.relations.get(name))
        })
    }

    pub(crate) fn historical_relation(&self, path: &[&str]) -> Option<&RelationDescriptor> {
        path.last()
            .is_some_and(|segment| *segment == "as_of")
            .then(|| &path[..path.len().saturating_sub(1)])
            .and_then(|prefix| self.relation(&prefix.join(".")))
    }

    pub(crate) fn historical_singleton(&self, path: &[&str]) -> Option<&SingletonDescriptor> {
        path.last()
            .is_some_and(|segment| *segment == "as_of")
            .then(|| &path[..path.len().saturating_sub(1)])
            .and_then(|prefix| self.singletons.get(&prefix.join(".")))
    }

    pub(crate) fn enum_value(&self, path: &[&str]) -> Option<String> {
        let name = path.join(".");
        if self.enums.contains_key(&name) {
            return Some(name);
        }
        let (enum_name, variant) = name.rsplit_once('.')?;
        self.enums
            .get(enum_name)
            .filter(|variants| variants.contains(variant))
            .map(|_| enum_name.to_owned())
    }

    pub(crate) fn describes_type(&self, name: &str) -> bool {
        self.types.contains_key(name)
            || self.relations.contains_key(name)
            || self.enums.contains_key(name)
    }

    pub(crate) fn field(&self, type_name: &str, field: &str) -> Option<&SystemType> {
        self.types
            .get(type_name)
            .map(|descriptor| &descriptor.fields)
            .or_else(|| {
                self.relations
                    .get(type_name)
                    .map(|descriptor| &descriptor.fields)
            })?
            .get(field)
    }

    pub(crate) fn resolve(&self, path: &[&str]) -> PathResolution<'_> {
        let name = path.join(".");
        if let Some(removed) = self.removed.get(&name) {
            return PathResolution::Removed(removed);
        }
        for index in (2..path.len()).rev() {
            if let Some(removed) = self.removed.get(&path[..index].join(".")) {
                return PathResolution::Removed(removed);
            }
        }
        if let Some(singleton) = self.singletons.get(&name) {
            return PathResolution::ReadOnly(&singleton.ty);
        }
        if self.functions.contains_key(&name)
            || self.relation(&name).is_some()
            || self.enum_value(path).is_some()
        {
            return PathResolution::KnownUnsupported;
        }
        if self.types.contains_key(&name)
            || self.relations.contains_key(&name)
            || self.grouped_relations.contains_key(&name)
            || self.enums.contains_key(&name)
        {
            return PathResolution::KnownUnsupported;
        }

        let Some((root, tail)) = path.split_first() else {
            return PathResolution::Unknown;
        };
        if *root != "sys" {
            return PathResolution::Unknown;
        }
        for index in (2..=path.len()).rev() {
            let prefix = path[..index].join(".");
            if let Some(singleton) = self.singletons.get(&prefix) {
                return resolve_fields(&self.types, &singleton.ty, &tail[index - 1..]);
            }
        }
        PathResolution::KnownUnsupported
    }
}

pub(crate) fn embedded_system_api() -> &'static SystemApi {
    static API: OnceLock<SystemApi> = OnceLock::new();
    API.get_or_init(|| SystemApi::embedded().expect("checked-in sys API must remain valid"))
}

fn resolve_fields<'a>(
    types: &'a BTreeMap<String, TypeDescriptor>,
    ty: &'a SystemType,
    fields: &[&str],
) -> PathResolution<'a> {
    let mut current = ty;
    for field in fields {
        let SystemType::Named(name) = current else {
            return PathResolution::KnownUnsupported;
        };
        let Some(next) = types
            .get(name)
            .and_then(|descriptor| descriptor.fields.get(*field))
        else {
            return PathResolution::KnownUnsupported;
        };
        current = next;
    }
    PathResolution::ReadOnly(current)
}

fn fields(
    raw: Vec<RawField>,
    type_arities: &BTreeMap<String, usize>,
    parameters: &BTreeSet<String>,
) -> Result<BTreeMap<String, SystemType>, SystemApiError> {
    let mut result = BTreeMap::new();
    for field in raw {
        if !valid_identifier(&field.name) {
            return Err(SystemApiError::InvalidName);
        }
        let ty = parse_and_validate_type(&field.ty, type_arities, parameters)?;
        if result.insert(field.name, ty).is_some() {
            return Err(SystemApiError::DuplicateName);
        }
    }
    Ok(result)
}

fn insert_type_arity(
    arities: &mut BTreeMap<String, usize>,
    name: &str,
    arity: usize,
) -> Result<(), SystemApiError> {
    validate_path(name)?;
    if arities.insert(name.to_owned(), arity).is_some() {
        return Err(SystemApiError::DuplicateName);
    }
    Ok(())
}

fn insert_descriptor<T>(
    descriptors: &mut BTreeMap<String, T>,
    name: String,
    descriptor: T,
) -> Result<(), SystemApiError> {
    if descriptors.insert(name, descriptor).is_some() {
        return Err(SystemApiError::DuplicateName);
    }
    Ok(())
}

fn language_type_arities() -> BTreeMap<String, usize> {
    [
        ("Bool", 0),
        ("Blob", 0),
        ("Date", 0),
        ("Decimal", 0),
        ("Digest", 0),
        ("Duration", 0),
        ("Float", 0),
        ("Instant", 0),
        ("Int", 0),
        ("Locale", 0),
        ("Null", 0),
        ("Path", 0),
        ("PresentContext", 0),
        ("PresentTree", 0),
        ("Query", 1),
        ("Relation", 1),
        ("Str", 0),
        ("TimeZone", 0),
    ]
    .into_iter()
    .map(|(name, arity)| (name.to_owned(), arity))
    .collect()
}

fn parse_function(
    signature: &str,
    effect: String,
    type_arities: &BTreeMap<String, usize>,
    singletons: &BTreeMap<String, SingletonDescriptor>,
    types: &BTreeMap<String, TypeDescriptor>,
    enums: &BTreeMap<String, BTreeSet<String>>,
) -> Result<FunctionDescriptor, SystemApiError> {
    let effect = parse_effect(&effect)?;
    let signature = signature
        .strip_prefix("fn ")
        .ok_or(SystemApiError::InvalidFunction)?;
    let open = signature.find('(').ok_or(SystemApiError::InvalidFunction)?;
    let close = matching(signature, open, '(', ')').ok_or(SystemApiError::InvalidFunction)?;
    let header = &signature[..open];
    let (name, type_parameters) = parse_function_header(header)?;
    let tail = signature[close + 1..]
        .strip_prefix(": ")
        .ok_or(SystemApiError::InvalidFunction)?;
    let result = parse_and_validate_type(tail, type_arities, &type_parameters)?;
    let mut parameters = Vec::new();
    let mut names = BTreeSet::new();
    let mut saw_default = false;
    for parameter in split_top_level(&signature[open + 1..close], ',') {
        if parameter.is_empty() {
            continue;
        }
        let (parameter, default) = match parameter.split_once(" = ") {
            Some((parameter, default)) => (parameter, Some(default)),
            None => (parameter, None),
        };
        let (parameter_name, ty) = parameter
            .split_once(": ")
            .ok_or(SystemApiError::InvalidFunction)?;
        if !valid_identifier(parameter_name) || !names.insert(parameter_name.to_owned()) {
            return Err(SystemApiError::InvalidFunction);
        }
        let ty = parse_and_validate_type(ty, type_arities, &type_parameters)?;
        if default.is_none() && saw_default {
            return Err(SystemApiError::InvalidDefault);
        }
        if let Some(default) = default {
            validate_default(default, &ty, singletons, types, enums)?;
            saw_default = true;
        }
        parameters.push(ParameterDescriptor {
            name: parameter_name.to_owned(),
            ty,
            has_default: default.is_some(),
        });
    }
    Ok(FunctionDescriptor {
        name,
        type_parameters,
        parameters,
        result,
        effect,
    })
}

fn parse_effect(effect: &str) -> Result<SystemEffect, SystemApiError> {
    match effect {
        "read" => Ok(SystemEffect::Read),
        "invoke" => Ok(SystemEffect::Invoke),
        "admin" => Ok(SystemEffect::Admin),
        _ => Err(SystemApiError::InvalidEffect),
    }
}

/// Overload identity is the callable input shape.  Return type and effect do
/// not distinguish callable overloads, so neither may hide a duplicate.
fn function_identity(descriptor: &FunctionDescriptor) -> String {
    format!(
        "{}<{:?}>({:?})",
        descriptor.name,
        descriptor.type_parameters,
        descriptor
            .parameters
            .iter()
            .map(|parameter| (&parameter.ty, parameter.has_default))
            .collect::<Vec<_>>(),
    )
}

fn parse_function_header(header: &str) -> Result<(String, BTreeSet<String>), SystemApiError> {
    match header.split_once('<') {
        None => {
            validate_path(header)?;
            Ok((header.to_owned(), BTreeSet::new()))
        }
        Some((name, parameters)) => {
            validate_path(name)?;
            let parameters = parameters
                .strip_suffix('>')
                .ok_or(SystemApiError::InvalidFunction)?;
            let mut result = BTreeSet::new();
            for parameter in parameters.split(',') {
                if !valid_identifier(parameter) || !result.insert(parameter.to_owned()) {
                    return Err(SystemApiError::InvalidFunction);
                }
            }
            if result.is_empty() {
                return Err(SystemApiError::InvalidFunction);
            }
            Ok((name.to_owned(), result))
        }
    }
}

fn parse_and_validate_type(
    source: &str,
    type_arities: &BTreeMap<String, usize>,
    parameters: &BTreeSet<String>,
) -> Result<SystemType, SystemApiError> {
    let ty = parse_type(source)?;
    validate_type(&ty, type_arities, parameters)?;
    Ok(ty)
}

fn parse_type(source: &str) -> Result<SystemType, SystemApiError> {
    if let Some(inner) = source.strip_suffix('?') {
        return Ok(SystemType::Optional(Box::new(parse_type(inner)?)));
    }
    if let Some(inner) = source
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        return Ok(SystemType::List(Box::new(parse_type(inner)?)));
    }
    if let Some(open) = source.find('<') {
        let close = matching(source, open, '<', '>').ok_or(SystemApiError::InvalidType)?;
        if close + 1 != source.len() {
            return Err(SystemApiError::InvalidType);
        }
        let base = &source[..open];
        validate_path(base)?;
        let arguments = split_top_level(&source[open + 1..close], ',')
            .into_iter()
            .map(parse_type)
            .collect::<Result<Vec<_>, _>>()?;
        if arguments.is_empty() {
            return Err(SystemApiError::InvalidType);
        }
        return Ok(SystemType::Applied {
            base: base.to_owned(),
            arguments,
        });
    }
    validate_path(source)?;
    Ok(SystemType::Named(source.to_owned()))
}

fn validate_type(
    ty: &SystemType,
    type_arities: &BTreeMap<String, usize>,
    parameters: &BTreeSet<String>,
) -> Result<(), SystemApiError> {
    match ty {
        SystemType::Named(name) => {
            if parameters.contains(name) {
                return Ok(());
            }
            match type_arities.get(name) {
                Some(0) => {}
                Some(_) => return Err(SystemApiError::InvalidType),
                None => return Err(SystemApiError::UnresolvedType),
            }
        }
        SystemType::Applied { base, arguments } => {
            match type_arities.get(base) {
                Some(arity) if *arity == arguments.len() => {}
                Some(_) => return Err(SystemApiError::InvalidType),
                None if parameters.contains(base) => return Err(SystemApiError::InvalidType),
                None => return Err(SystemApiError::UnresolvedType),
            }
            for argument in arguments {
                validate_type(argument, type_arities, parameters)?;
            }
        }
        SystemType::List(inner) | SystemType::Optional(inner) => {
            validate_type(inner, type_arities, parameters)?;
        }
    }
    Ok(())
}

fn plain_type_name(name: &str) -> Result<String, SystemApiError> {
    match parse_type(name)? {
        SystemType::Named(name) => Ok(name),
        _ => Err(SystemApiError::InvalidType),
    }
}

/// Parses a JSON value-type declaration as one coherent generic contract.
///
/// `name` carries the public application (`sys.RowRef<T>`), while
/// `type_parameters` supplies the metadata used to validate fields. They must
/// be byte-for-byte equivalent in declaration order: accepting either side on
/// its own would let a malformed descriptor alter the portable type contract.
fn value_type_declaration(
    name: &str,
    declared_parameters: &[String],
) -> Result<(String, Vec<String>), SystemApiError> {
    let declared_parameters = validated_type_parameters(declared_parameters)?;
    match parse_type(name)? {
        SystemType::Named(base) if declared_parameters.is_empty() => Ok((base, Vec::new())),
        SystemType::Named(_) => Err(SystemApiError::InvalidType),
        SystemType::Applied { base, arguments } => {
            let application_parameters = arguments
                .into_iter()
                .map(|argument| match argument {
                    SystemType::Named(parameter) if valid_identifier(&parameter) => Ok(parameter),
                    _ => Err(SystemApiError::InvalidType),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let application_parameters = validated_type_parameters(&application_parameters)?;
            (application_parameters == declared_parameters)
                .then_some((base, declared_parameters))
                .ok_or(SystemApiError::InvalidType)
        }
        SystemType::List(_) | SystemType::Optional(_) => Err(SystemApiError::InvalidType),
    }
}

fn validated_type_parameters(parameters: &[String]) -> Result<Vec<String>, SystemApiError> {
    let mut seen = BTreeSet::new();
    for parameter in parameters {
        if !valid_identifier(parameter) {
            return Err(SystemApiError::InvalidName);
        }
        if !seen.insert(parameter) {
            return Err(SystemApiError::DuplicateName);
        }
    }
    Ok(parameters.to_vec())
}

fn validate_path(name: &str) -> Result<(), SystemApiError> {
    if name.split('.').all(valid_identifier) {
        Ok(())
    } else {
        Err(SystemApiError::InvalidName)
    }
}

fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn valid_diagnostic(name: &str) -> bool {
    name.starts_with("ORNA")
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '-'
        })
}

fn valid_failure_code(name: &str) -> bool {
    let mut segments = name.split('.');
    matches!(segments.next(), Some("sys"))
        && segments.clone().count() >= 2
        && segments.all(|segment| {
            let mut characters = segment.chars();
            characters
                .next()
                .is_some_and(|character| character.is_ascii_lowercase())
                && characters.all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
                })
        })
}

fn valid_function_label(
    label: &str,
    descriptor: &FunctionDescriptor,
    all_functions: &[(String, FunctionDescriptor)],
) -> bool {
    let Some(suffix) = label.strip_prefix(&descriptor.name) else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    if let Some(labels) = suffix
        .strip_prefix('(')
        .and_then(|suffix| suffix.strip_suffix(')'))
    {
        let labels = labels.split(',').map(str::trim).collect::<Vec<_>>();
        return labels.len() == 1
            && !labels[0].is_empty()
            && (label_matches_input_parameter(labels[0], descriptor)
                || label_matches_erased_generic_input(labels[0], descriptor, all_functions));
    }
    suffix
        .strip_prefix('<')
        .and_then(|suffix| suffix.strip_suffix('>'))
        .is_some_and(|parameters| {
            let parameters = parameters.split(',').collect::<Vec<_>>();
            !parameters.is_empty()
                && parameters.len() == descriptor.type_parameters.len()
                && parameters
                    .iter()
                    .all(|parameter| valid_identifier(parameter))
                && parameters.iter().collect::<BTreeSet<_>>().len() == parameters.len()
                && parameters
                    .iter()
                    .all(|parameter| descriptor.type_parameters.contains(*parameter))
        })
}

/// Parenthesised labels identify the type of an actual input parameter that
/// distinguishes a callable overload. A result type, or a type nested within
/// an input, must not accidentally validate an unrelated label.
fn label_matches_input_parameter(label: &str, descriptor: &FunctionDescriptor) -> bool {
    descriptor
        .parameters
        .iter()
        .any(|parameter| type_label(&parameter.ty) == label)
}

/// `sys.invoke(Value)` and `sys.start(Value)` are the erased forms of a
/// sibling generic callable. Their label is still checked against an input:
/// it is the concrete substitute for that sibling's explicit type-witness
/// parameter. The remaining input signatures must match exactly, so this is
/// not a general result-type escape hatch.
fn label_matches_erased_generic_input(
    label: &str,
    concrete: &FunctionDescriptor,
    all_functions: &[(String, FunctionDescriptor)],
) -> bool {
    all_functions
        .iter()
        .map(|(_, candidate)| candidate)
        .filter(|candidate| candidate.name == concrete.name)
        .any(|generic| erased_generic_input_matches(label, concrete, generic))
}

fn erased_generic_input_matches(
    label: &str,
    concrete: &FunctionDescriptor,
    generic: &FunctionDescriptor,
) -> bool {
    if !concrete.type_parameters.is_empty() || generic.type_parameters.len() != 1 {
        return false;
    }
    let parameter = generic.type_parameters.first().expect("one type parameter");
    let witness_indices = generic
        .parameters
        .iter()
        .enumerate()
        .filter_map(|(index, input)| {
            (input.ty == SystemType::Named(parameter.clone())).then_some(index)
        })
        .collect::<Vec<_>>();
    if witness_indices.len() != 1 {
        return false;
    }
    let witness_index = witness_indices[0];
    let remaining = generic
        .parameters
        .iter()
        .enumerate()
        .filter_map(|(index, input)| (index != witness_index).then_some(input));
    if !remaining.eq(concrete.parameters.iter()) {
        return false;
    }
    type_parameter_substitution(&generic.result, &concrete.result, parameter)
        .is_some_and(|substitution| type_label(&substitution) == label)
}

fn type_parameter_substitution(
    template: &SystemType,
    concrete: &SystemType,
    parameter: &str,
) -> Option<SystemType> {
    match (template, concrete) {
        (SystemType::Named(name), _) if name == parameter => Some(concrete.clone()),
        (SystemType::Named(left), SystemType::Named(right)) if left == right => None,
        (
            SystemType::Applied {
                base: left_base,
                arguments: left_arguments,
            },
            SystemType::Applied {
                base: right_base,
                arguments: right_arguments,
            },
        ) if left_base == right_base && left_arguments.len() == right_arguments.len() => {
            merge_type_substitutions(
                left_arguments
                    .iter()
                    .zip(right_arguments)
                    .map(|(left, right)| type_parameter_substitution(left, right, parameter)),
            )
        }
        (SystemType::List(left), SystemType::List(right))
        | (SystemType::Optional(left), SystemType::Optional(right)) => {
            type_parameter_substitution(left, right, parameter)
        }
        _ => None,
    }
}

fn merge_type_substitutions(
    substitutions: impl IntoIterator<Item = Option<SystemType>>,
) -> Option<SystemType> {
    let mut found = None;
    for substitution in substitutions {
        match (found.as_ref(), substitution) {
            (None, Some(substitution)) => found = Some(substitution),
            (Some(previous), Some(substitution)) if previous != &substitution => return None,
            _ => {}
        }
    }
    found
}

fn type_label(ty: &SystemType) -> String {
    match ty {
        SystemType::Named(name) | SystemType::Applied { base: name, .. } => {
            name.rsplit('.').next().unwrap_or(name).to_owned()
        }
        SystemType::List(inner) | SystemType::Optional(inner) => type_label(inner),
    }
}

fn validate_default(
    source: &str,
    expected: &SystemType,
    singletons: &BTreeMap<String, SingletonDescriptor>,
    types: &BTreeMap<String, TypeDescriptor>,
    enums: &BTreeMap<String, BTreeSet<String>>,
) -> Result<(), SystemApiError> {
    let actual = match source {
        "null" => {
            return matches!(expected, SystemType::Optional(_))
                .then_some(())
                .ok_or(SystemApiError::InvalidDefault);
        }
        "true" | "false" => SystemType::Named("Bool".into()),
        _ => resolve_default_path(source, singletons, types, enums)?,
    };
    (actual == *expected)
        .then_some(())
        .ok_or(SystemApiError::InvalidDefault)
}

fn resolve_default_path(
    source: &str,
    singletons: &BTreeMap<String, SingletonDescriptor>,
    types: &BTreeMap<String, TypeDescriptor>,
    enums: &BTreeMap<String, BTreeSet<String>>,
) -> Result<SystemType, SystemApiError> {
    validate_path(source).map_err(|_| SystemApiError::InvalidDefault)?;
    let parts = source.split('.').collect::<Vec<_>>();
    for end in (2..=parts.len()).rev() {
        if let Some(singleton) = singletons.get(&parts[..end].join(".")) {
            return resolve_default_fields(&singleton.ty, &parts[end..], types);
        }
    }
    let (enum_name, variant) = source
        .rsplit_once('.')
        .ok_or(SystemApiError::InvalidDefault)?;
    enums
        .get(enum_name)
        .filter(|variants| variants.contains(variant))
        .map(|_| SystemType::Named(enum_name.to_owned()))
        .ok_or(SystemApiError::InvalidDefault)
}

fn resolve_default_fields(
    ty: &SystemType,
    fields: &[&str],
    types: &BTreeMap<String, TypeDescriptor>,
) -> Result<SystemType, SystemApiError> {
    let mut current = ty;
    for field in fields {
        let SystemType::Named(name) = current else {
            return Err(SystemApiError::InvalidDefault);
        };
        current = types
            .get(name)
            .and_then(|descriptor| descriptor.fields.get(*field))
            .ok_or(SystemApiError::InvalidDefault)?;
    }
    Ok(current.clone())
}

fn portable_path_exists(
    path: &str,
    singletons: &BTreeMap<String, SingletonDescriptor>,
    types: &BTreeMap<String, TypeDescriptor>,
    relations: &BTreeMap<String, RelationDescriptor>,
    functions: &BTreeMap<String, Vec<FunctionDescriptor>>,
    grouped_relations: &BTreeMap<String, String>,
    enums: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    singletons.contains_key(path)
        || types.contains_key(path)
        || relations.contains_key(path)
        || functions.contains_key(path)
        || grouped_relations.contains_key(path)
        || enums.contains_key(path)
        || path.rsplit_once('.').is_some_and(|(enum_name, variant)| {
            enums
                .get(enum_name)
                .is_some_and(|variants| variants.contains(variant))
        })
}

fn matching(source: &str, open: usize, opening: char, closing: char) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in source.char_indices().skip_while(|(index, _)| *index < open) {
        match character {
            character if character == opening => depth += 1,
            character if character == closing => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(source: &str, separator: char) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (index, character) in source.char_indices() {
        match character {
            '<' | '[' | '(' => depth += 1,
            '>' | ']' | ')' => depth = depth.saturating_sub(1),
            character if character == separator && depth == 0 => {
                pieces.push(source[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    pieces.push(source[start..].trim());
    pieces
}

struct RawSystemApi {
    counts: RawCounts,
    singletons: Vec<RawSingleton>,
    opaque_identifiers: Vec<String>,
    reference_aliases: Vec<RawReferenceAlias>,
    value_types: Vec<RawValueType>,
    enums: BTreeMap<String, Vec<String>>,
    relations: Vec<RawRelation>,
    functions: Vec<RawFunction>,
    failure_codes: Vec<String>,
    removed_names: BTreeMap<String, RawRemovedName>,
}

struct RawCounts {
    singletons: usize,
    opaque_identifiers: usize,
    reference_aliases: usize,
    value_types: usize,
    enums: usize,
    relations: usize,
    functions: usize,
    failure_codes: usize,
}

struct RawSingleton {
    name: String,
    ty: String,
    availability: String,
}

struct RawReferenceAlias {
    name: String,
    target: String,
    definition: String,
}

struct RawValueType {
    name: String,
    type_parameters: Vec<String>,
    fields: Vec<RawField>,
}

struct RawRelation {
    name: String,
    grouped_handle: String,
    fields: Vec<RawField>,
    writable: bool,
    reference_type: String,
}

struct RawField {
    name: String,
    ty: String,
}

struct RawFunction {
    name: String,
    signature: String,
    effect: String,
}

struct RawRemovedName {
    replacement: String,
    diagnostic: String,
}

fn raw_document(source: &str) -> Result<RawSystemApi, SystemApiError> {
    reject_duplicate_json_members(source)?;
    let value: serde_json::Value =
        serde_json::from_str(source).map_err(|_| SystemApiError::InvalidJson)?;
    let document = object(&value)?;
    let counts = raw_counts(value_at(document, "counts")?)?;
    let singletons = values(value_at(document, "singletons")?)?
        .iter()
        .map(raw_singleton)
        .collect::<Result<_, _>>()?;
    let opaque_identifiers = strings(value_at(document, "opaque_identifiers")?)?;
    let reference_aliases = values(value_at(document, "reference_aliases")?)?
        .iter()
        .map(|value| {
            Ok(RawReferenceAlias {
                name: text(value_at(object(value)?, "name")?)?.to_owned(),
                target: text(value_at(object(value)?, "target")?)?.to_owned(),
                definition: text(value_at(object(value)?, "definition")?)?.to_owned(),
            })
        })
        .collect::<Result<_, SystemApiError>>()?;
    let value_types = values(value_at(document, "value_types")?)?
        .iter()
        .map(raw_value_type)
        .collect::<Result<_, _>>()?;
    let enums = object(value_at(document, "enums")?)?
        .iter()
        .map(|(name, variants)| Ok((name.clone(), strings(variants)?)))
        .collect::<Result<_, SystemApiError>>()?;
    let relations = values(value_at(document, "relations")?)?
        .iter()
        .map(raw_relation)
        .collect::<Result<_, _>>()?;
    let functions = values(value_at(document, "functions")?)?
        .iter()
        .map(raw_function)
        .collect::<Result<_, _>>()?;
    let failure_codes = strings(value_at(document, "failure_codes")?)?;
    let removed_names = object(value_at(document, "removed_names")?)?
        .iter()
        .map(|(name, value)| {
            let value = object(value)?;
            Ok((
                name.clone(),
                RawRemovedName {
                    replacement: text(value_at(value, "replacement")?)?.to_owned(),
                    diagnostic: text(value_at(value, "diagnostic")?)?.to_owned(),
                },
            ))
        })
        .collect::<Result<_, SystemApiError>>()?;
    Ok(RawSystemApi {
        counts,
        singletons,
        opaque_identifiers,
        reference_aliases,
        value_types,
        enums,
        relations,
        functions,
        failure_codes,
        removed_names,
    })
}

/// `serde_json::Value` stores object members in a map and therefore cannot
/// report duplicate keys after decoding.  The API artifact is a closed
/// descriptor, so accepting a duplicate would make validation depend on which
/// occurrence the decoder retained.  Scan the JSON structure first and
/// compare decoded member names (including escaped spellings); serde_json
/// remains responsible for the complete syntax and value validation below.
fn reject_duplicate_json_members(source: &str) -> Result<(), SystemApiError> {
    let mut parser = JsonMemberParser {
        source: source.as_bytes(),
        position: 0,
        depth: 0,
    };
    parser.value()?;
    Ok(())
}

struct JsonMemberParser<'a> {
    source: &'a [u8],
    position: usize,
    depth: usize,
}

impl JsonMemberParser<'_> {
    fn value(&mut self) -> Result<(), SystemApiError> {
        self.whitespace();
        match self.source.get(self.position).copied() {
            Some(b'{') => {
                self.enter_container()?;
                let result = self.object();
                self.depth -= 1;
                result
            }
            Some(b'[') => {
                self.enter_container()?;
                let result = self.array();
                self.depth -= 1;
                result
            }
            Some(b'"') => {
                self.string()?;
                Ok(())
            }
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => {
                self.primitive();
                Ok(())
            }
            _ => Err(SystemApiError::InvalidJson),
        }
    }

    fn enter_container(&mut self) -> Result<(), SystemApiError> {
        if self.depth >= MAX_JSON_NESTING_DEPTH {
            return Err(SystemApiError::InvalidJson);
        }
        self.depth += 1;
        Ok(())
    }

    fn object(&mut self) -> Result<(), SystemApiError> {
        self.position += 1;
        self.whitespace();
        let mut names = BTreeSet::new();
        if self.take(b'}') {
            return Ok(());
        }
        loop {
            self.whitespace();
            let name = self.string()?;
            if !names.insert(name) {
                return Err(SystemApiError::InvalidJson);
            }
            self.whitespace();
            if !self.take(b':') {
                return Err(SystemApiError::InvalidJson);
            }
            self.value()?;
            self.whitespace();
            if self.take(b'}') {
                return Ok(());
            }
            if !self.take(b',') {
                return Err(SystemApiError::InvalidJson);
            }
        }
    }

    fn array(&mut self) -> Result<(), SystemApiError> {
        self.position += 1;
        self.whitespace();
        if self.take(b']') {
            return Ok(());
        }
        loop {
            self.value()?;
            self.whitespace();
            if self.take(b']') {
                return Ok(());
            }
            if !self.take(b',') {
                return Err(SystemApiError::InvalidJson);
            }
        }
    }

    fn string(&mut self) -> Result<String, SystemApiError> {
        let start = self.position;
        if !self.take(b'"') {
            return Err(SystemApiError::InvalidJson);
        }
        let mut escaped = false;
        while let Some(byte) = self.source.get(self.position).copied() {
            self.position += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return serde_json::from_slice(&self.source[start..self.position])
                    .map_err(|_| SystemApiError::InvalidJson);
            }
        }
        Err(SystemApiError::InvalidJson)
    }

    fn literal(&mut self, literal: &[u8]) -> Result<(), SystemApiError> {
        let end = self
            .position
            .checked_add(literal.len())
            .ok_or(SystemApiError::InvalidJson)?;
        if self.source.get(self.position..end) == Some(literal) {
            self.position = end;
            Ok(())
        } else {
            Err(SystemApiError::InvalidJson)
        }
    }

    fn primitive(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| !matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}'))
        {
            self.position += 1;
        }
    }

    fn whitespace(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.source.get(self.position) == Some(&expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
}

fn raw_counts(value: &serde_json::Value) -> Result<RawCounts, SystemApiError> {
    let value = object(value)?;
    Ok(RawCounts {
        singletons: natural(value_at(value, "singletons")?)?,
        opaque_identifiers: natural(value_at(value, "opaque_identifiers")?)?,
        reference_aliases: natural(value_at(value, "reference_aliases")?)?,
        value_types: natural(value_at(value, "value_types")?)?,
        enums: natural(value_at(value, "enums")?)?,
        relations: natural(value_at(value, "relations")?)?,
        functions: natural(value_at(value, "functions")?)?,
        failure_codes: natural(value_at(value, "failure_codes")?)?,
    })
}

fn raw_singleton(value: &serde_json::Value) -> Result<RawSingleton, SystemApiError> {
    let value = object(value)?;
    Ok(RawSingleton {
        name: text(value_at(value, "name")?)?.to_owned(),
        ty: text(value_at(value, "type")?)?.to_owned(),
        availability: text(value_at(value, "availability")?)?.to_owned(),
    })
}

fn raw_value_type(value: &serde_json::Value) -> Result<RawValueType, SystemApiError> {
    let value = object(value)?;
    Ok(RawValueType {
        name: text(value_at(value, "name")?)?.to_owned(),
        type_parameters: optional_strings(value.get("type_parameters"))?,
        fields: optional_values(value.get("fields"))?
            .iter()
            .map(raw_field)
            .collect::<Result<_, _>>()?,
    })
}

fn raw_relation(value: &serde_json::Value) -> Result<RawRelation, SystemApiError> {
    let value = object(value)?;
    Ok(RawRelation {
        name: text(value_at(value, "name")?)?.to_owned(),
        grouped_handle: text(value_at(value, "grouped_handle")?)?.to_owned(),
        fields: values(value_at(value, "fields")?)?
            .iter()
            .map(raw_field)
            .collect::<Result<_, _>>()?,
        writable: boolean(value_at(value, "writable")?)?,
        reference_type: text(value_at(value, "reference_type")?)?.to_owned(),
    })
}

fn raw_field(value: &serde_json::Value) -> Result<RawField, SystemApiError> {
    let value = object(value)?;
    Ok(RawField {
        name: text(value_at(value, "name")?)?.to_owned(),
        ty: text(value_at(value, "type")?)?.to_owned(),
    })
}

fn raw_function(value: &serde_json::Value) -> Result<RawFunction, SystemApiError> {
    let value = object(value)?;
    Ok(RawFunction {
        name: text(value_at(value, "name")?)?.to_owned(),
        signature: text(value_at(value, "signature")?)?.to_owned(),
        effect: text(value_at(value, "effect")?)?.to_owned(),
    })
}

fn object(
    value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, SystemApiError> {
    value.as_object().ok_or(SystemApiError::InvalidJson)
}

fn values(value: &serde_json::Value) -> Result<&Vec<serde_json::Value>, SystemApiError> {
    value.as_array().ok_or(SystemApiError::InvalidJson)
}

fn text(value: &serde_json::Value) -> Result<&str, SystemApiError> {
    value.as_str().ok_or(SystemApiError::InvalidJson)
}

fn natural(value: &serde_json::Value) -> Result<usize, SystemApiError> {
    value
        .as_u64()
        .and_then(|value| value.try_into().ok())
        .ok_or(SystemApiError::InvalidJson)
}

fn boolean(value: &serde_json::Value) -> Result<bool, SystemApiError> {
    value.as_bool().ok_or(SystemApiError::InvalidJson)
}

fn value_at<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a serde_json::Value, SystemApiError> {
    object.get(key).ok_or(SystemApiError::InvalidJson)
}

fn strings(value: &serde_json::Value) -> Result<Vec<String>, SystemApiError> {
    values(value)?
        .iter()
        .map(|value| Ok(text(value)?.to_owned()))
        .collect()
}

fn optional_values(
    value: Option<&serde_json::Value>,
) -> Result<&[serde_json::Value], SystemApiError> {
    match value {
        Some(value) => Ok(values(value)?.as_slice()),
        None => Ok(&[]),
    }
}

fn optional_strings(value: Option<&serde_json::Value>) -> Result<Vec<String>, SystemApiError> {
    match value {
        Some(value) => strings(value),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> serde_json::Value {
        serde_json::from_str(EMBEDDED_SYSTEM_API).unwrap()
    }

    fn value_type(document: &mut serde_json::Value) -> &mut serde_json::Value {
        document["value_types"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|value| value["name"] == "sys.RowRef<T>")
            .unwrap()
    }

    #[test]
    fn complete_checked_in_inventory_is_loaded_exactly() {
        let api = SystemApi::embedded().unwrap();
        assert_eq!(
            api.inventory(),
            Inventory {
                singletons: 4,
                opaque_identifiers: 21,
                reference_aliases: 78,
                value_types: 34,
                enums: 44,
                relations: 78,
                functions: 66,
                failure_codes: 46,
            }
        );
        assert_eq!(api.function("sys.rt.info").unwrap().len(), 1);
        assert_eq!(
            api.singleton("sys.database").unwrap().ty,
            SystemType::Named("sys.DatabaseView".into())
        );
        assert_eq!(
            api.types.get("sys.RowRef").unwrap().type_parameters,
            vec!["T"]
        );
    }

    #[test]
    fn names_are_exact_case_sensitive_and_removed_names_remain_rejected() {
        let api = SystemApi::embedded().unwrap();
        assert!(matches!(
            api.resolve(&["sys", "rt"]),
            PathResolution::ReadOnly(SystemType::Named(name)) if name == "sys.RuntimeView"
        ));
        assert_eq!(
            api.resolve(&["sys", "RT"]),
            PathResolution::KnownUnsupported
        );
        assert!(matches!(
            api.resolve(&["sys", "runtime"]),
            PathResolution::Removed(RemovedName { replacement, diagnostic })
                if replacement == "sys.rt" && diagnostic == "ORNA100-E-SYS-RUNTIME"
        ));
    }

    #[test]
    fn supported_read_only_fields_and_known_unimplemented_members_are_distinct() {
        let api = SystemApi::embedded().unwrap();
        assert!(matches!(
            api.resolve(&["sys", "database", "cwd"]),
            PathResolution::ReadOnly(SystemType::Named(name)) if name == "sys.SnapshotRef"
        ));
        assert!(matches!(
            api.resolve(&["sys", "current", "snapshot"]),
            PathResolution::ReadOnly(SystemType::Named(name)) if name == "sys.SnapshotRef"
        ));
        assert!(matches!(
            api.resolve(&["sys", "rt", "id"]),
            PathResolution::ReadOnly(SystemType::Named(name)) if name == "sys.RuntimeId"
        ));
        assert!(matches!(
            api.resolve(&["sys", "repl", "width"]),
            PathResolution::ReadOnly(SystemType::Optional(_))
        ));
        assert_eq!(
            api.resolve(&["sys", "catalog", "databases"]),
            PathResolution::KnownUnsupported
        );
    }

    #[test]
    fn malformed_duplicate_and_unresolved_entries_fail_without_echoing_input() {
        let mut duplicate = document();
        duplicate["singletons"][1]["name"] = duplicate["singletons"][0]["name"].clone();
        assert_eq!(
            SystemApi::from_json(&duplicate.to_string()),
            Err(SystemApiError::DuplicateName)
        );

        let mut malformed = document();
        malformed["singletons"][0]["type"] = serde_json::Value::String("not a type!".into());
        assert_eq!(
            SystemApi::from_json(&malformed.to_string()),
            Err(SystemApiError::InvalidName)
        );

        let mut unresolved = document();
        let protected_name = "sys.CredentialMaterial";
        unresolved["singletons"][0]["type"] = serde_json::Value::String(protected_name.into());
        let error = SystemApi::from_json(&unresolved.to_string()).unwrap_err();
        assert_eq!(error, SystemApiError::UnresolvedType);
        assert!(!format!("{error:?}").contains(protected_name));
    }

    #[test]
    fn duplicate_json_members_are_rejected_at_the_sys_api_boundary() {
        let mut nested_object = EMBEDDED_SYSTEM_API.to_owned();
        let insertion = nested_object
            .find(r#""counts": {"#)
            .expect("the API document has a counts object")
            + r#""counts": {"#.len();
        nested_object.insert_str(insertion, r#""\u0073ingletons":0,"#);
        assert_eq!(
            SystemApi::from_json(&nested_object),
            Err(SystemApiError::InvalidJson)
        );

        let mut nested_array = EMBEDDED_SYSTEM_API.to_owned();
        let array = nested_array
            .find(r#""singletons": ["#)
            .expect("the API document has a singleton array");
        let insertion = array
            + nested_array[array..]
                .find('{')
                .expect("the singleton array has an object")
            + 1;
        nested_array.insert_str(insertion, r#""\u006eame":"shadow","#);
        assert_eq!(
            SystemApi::from_json(&nested_array),
            Err(SystemApiError::InvalidJson)
        );

        for malformed in [
            r#"{"counts":{"singletons":1}"#,
            r#"{"counts":{"singletons":1},"unterminated":"x}"#,
        ] {
            assert_eq!(
                SystemApi::from_json(malformed),
                Err(SystemApiError::InvalidJson)
            );
        }
    }

    #[test]
    fn deeply_nested_json_is_rejected_before_stack_exhaustion() {
        let opening = "[".repeat(MAX_JSON_NESTING_DEPTH + 1);
        let closing = "]".repeat(MAX_JSON_NESTING_DEPTH + 1);
        let mut deeply_nested = EMBEDDED_SYSTEM_API.to_owned();
        let insertion = deeply_nested
            .rfind('}')
            .expect("the API document has a root object");
        let field = format!(",\"unknown_nested_field\":{opening}null{closing}");
        deeply_nested.insert_str(insertion, &field);

        assert_eq!(
            SystemApi::from_json(&deeply_nested),
            Err(SystemApiError::InvalidJson)
        );
        assert!(SystemApi::embedded().is_ok());
    }

    #[test]
    fn generic_value_type_metadata_must_match_its_declared_application() {
        let mut invalid_parameter = document();
        value_type(&mut invalid_parameter)["type_parameters"] =
            serde_json::json!(["not a parameter"]);
        assert_eq!(
            SystemApi::from_json(&invalid_parameter.to_string()),
            Err(SystemApiError::InvalidName)
        );

        let mut duplicate_parameter = document();
        value_type(&mut duplicate_parameter)["type_parameters"] = serde_json::json!(["T", "T"]);
        assert_eq!(
            SystemApi::from_json(&duplicate_parameter.to_string()),
            Err(SystemApiError::DuplicateName)
        );

        let mut missing_parameter = document();
        value_type(&mut missing_parameter)["type_parameters"] = serde_json::json!([]);
        assert_eq!(
            SystemApi::from_json(&missing_parameter.to_string()),
            Err(SystemApiError::InvalidType)
        );

        let mut extra_parameter = document();
        value_type(&mut extra_parameter)["type_parameters"] = serde_json::json!(["T", "U"]);
        assert_eq!(
            SystemApi::from_json(&extra_parameter.to_string()),
            Err(SystemApiError::InvalidType)
        );

        let mut mismatched_parameter = document();
        value_type(&mut mismatched_parameter)["type_parameters"] = serde_json::json!(["U"]);
        assert_eq!(
            SystemApi::from_json(&mismatched_parameter.to_string()),
            Err(SystemApiError::InvalidType)
        );

        let mut malformed_application = document();
        value_type(&mut malformed_application)["name"] =
            serde_json::Value::String("sys.RowRef<T, [U]>".into());
        assert_eq!(
            SystemApi::from_json(&malformed_application.to_string()),
            Err(SystemApiError::InvalidType)
        );
    }

    #[test]
    fn every_applied_core_and_system_type_uses_its_declared_arity() {
        let mut core_arity = document();
        core_arity["relations"][0]["fields"][0]["type"] =
            serde_json::Value::String("Relation<sys.Database, sys.Database>".into());
        assert_eq!(
            SystemApi::from_json(&core_arity.to_string()),
            Err(SystemApiError::InvalidType)
        );

        let mut system_arity = document();
        system_arity["reference_aliases"][0]["definition"] =
            serde_json::Value::String("sys.RowRef<sys.Database, sys.Database>".into());
        assert_eq!(
            SystemApi::from_json(&system_arity.to_string()),
            Err(SystemApiError::InvalidType)
        );
    }

    #[test]
    fn duplicate_enum_variants_are_rejected_before_set_construction() {
        let mut duplicate = document();
        let variants = duplicate["enums"]["sys.FailureStatus"]
            .as_array_mut()
            .expect("checked-in enum variants are an array");
        variants.push(variants[0].clone());
        assert_eq!(
            SystemApi::from_json(&duplicate.to_string()),
            Err(SystemApiError::DuplicateName)
        );
    }

    #[test]
    fn descriptor_mutations_fail_closed_without_retaining_schema_escape_hatches() {
        let mut writable = document();
        writable["relations"][0]["writable"] = serde_json::Value::Bool(true);
        assert_eq!(
            SystemApi::from_json(&writable.to_string()),
            Err(SystemApiError::WritableRelation)
        );

        let mut alias_target = document();
        alias_target["reference_aliases"][0]["target"] =
            serde_json::Value::String("sys.UnknownRelation".into());
        assert_eq!(
            SystemApi::from_json(&alias_target.to_string()),
            Err(SystemApiError::InvalidRelationAlias)
        );

        let mut alias_definition = document();
        alias_definition["reference_aliases"][0]["definition"] =
            serde_json::Value::String("sys.RowRef<sys.File>".into());
        assert_eq!(
            SystemApi::from_json(&alias_definition.to_string()),
            Err(SystemApiError::InvalidRelationAlias)
        );

        let mut relation_reference = document();
        relation_reference["relations"][0]["reference_type"] =
            serde_json::Value::String("sys.FileRef".into());
        assert_eq!(
            SystemApi::from_json(&relation_reference.to_string()),
            Err(SystemApiError::InvalidRelationAlias)
        );

        let mut removed = document();
        removed["removed_names"]["sys.runtime"]["replacement"] =
            serde_json::Value::String("sys.not_a_portable_member".into());
        assert_eq!(
            SystemApi::from_json(&removed.to_string()),
            Err(SystemApiError::InvalidRemovedName)
        );
    }

    #[test]
    fn function_effects_and_labels_are_closed_and_identity_ignores_effect_text() {
        let mut unknown_effect = document();
        let protected_effect = "private-effect-token";
        unknown_effect["functions"][0]["effect"] =
            serde_json::Value::String(protected_effect.into());
        let error = SystemApi::from_json(&unknown_effect.to_string()).unwrap_err();
        assert_eq!(error, SystemApiError::InvalidEffect);
        assert!(!format!("{error:?}").contains(protected_effect));

        let mut mismatched_label = document();
        mismatched_label["functions"][0]["name"] =
            serde_json::Value::String("sys.meta(NotItsSignature)".into());
        assert_eq!(
            SystemApi::from_json(&mismatched_label.to_string()),
            Err(SystemApiError::InvalidFunction)
        );

        let mut result_type_label = document();
        let source = result_type_label["functions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|function| {
                function["signature"]
                    == serde_json::Value::String(
                        "fn sys.source(object: sys.ObjectRef): sys.SourceDocument".into(),
                    )
            })
            .unwrap();
        source["name"] = serde_json::Value::String("sys.source(SourceDocument)".into());
        assert_eq!(
            SystemApi::from_json(&result_type_label.to_string()),
            Err(SystemApiError::InvalidFunction)
        );

        let mut duplicate = document();
        let duplicate_function = duplicate["functions"][0].clone();
        duplicate["functions"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_function);
        duplicate["counts"]["functions"] = serde_json::Value::from(67);
        duplicate["functions"].as_array_mut().unwrap()[66]["effect"] =
            serde_json::Value::String("admin".into());
        assert_eq!(
            SystemApi::from_json(&duplicate.to_string()),
            Err(SystemApiError::DuplicateName)
        );
    }
}
