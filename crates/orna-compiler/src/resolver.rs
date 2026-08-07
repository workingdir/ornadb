//! Semantic resolution for parsed source bundles.
//!
//! The resolver consumes the `Parse` values retained by [`super::parse_bundle`].
//! It does not parse source text or expose syntax implementation values.

use std::collections::{HashMap, HashSet};

use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, TypeId,
    catalogue::{
        CatalogueSnapshot, FieldDefinition, ObjectTypeDefinition, OnDeleteAction,
        QualifiedSemanticName,
    },
    source::SourceBundle,
    types::{ResolvedType, StandardScalar},
};
use orna_syntax::{
    NamePart, ObjectTypeDeclaration, OnDeletePolicy, QualifiedName, SourceSlice, SourceSpan,
    StandardLargeObjectKind, TypeSpecification,
};

use crate::{
    ByteSpan, CompilerDiagnostic, DiagnosticCode, ParseReport, SourceLocation, parse_bundle,
};

/// Checks one source bundle against an immutable catalogue snapshot.
///
/// This function parses the bundle exactly once. Resolution consumes the owned
/// `Parse` values that [`parse_bundle`] retains in the resulting report.
pub fn check(bundle: &SourceBundle, base: &CatalogueSnapshot) -> CheckReport {
    check_parsed(parse_bundle(bundle), base)
}

/// The value of a default expression accepted in this first compiler slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstantValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Text(String),
}

/// A checked constant default expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedDefault {
    id: ExpressionId,
    value: ConstantValue,
    location: SourceLocation,
}

impl CheckedDefault {
    /// Returns the stable identity of this checked expression.
    pub const fn id(&self) -> ExpressionId {
        self.id
    }

    /// Returns the checked constant value.
    pub fn value(&self) -> &ConstantValue {
        &self.value
    }

    /// Returns the location of the source expression.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked field definition without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedField {
    id: FieldId,
    name: String,
    ordinal: u32,
    resolved_type: ResolvedType,
    nullable: bool,
    unique: bool,
    default: Option<CheckedDefault>,
    on_delete: Option<OnDeleteAction>,
    location: SourceLocation,
}

impl CheckedField {
    /// Returns the stable identity of the field.
    pub const fn id(&self) -> FieldId {
        self.id
    }
    /// Returns the resolved field name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the declaration ordinal.
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
    /// Returns the resolved type.
    pub const fn resolved_type(&self) -> ResolvedType {
        self.resolved_type
    }
    /// Reports whether the field permits null.
    pub const fn nullable(&self) -> bool {
        self.nullable
    }
    /// Reports whether the field is unique.
    pub const fn unique(&self) -> bool {
        self.unique
    }
    /// Returns the checked default expression, when declared.
    pub fn default(&self) -> Option<&CheckedDefault> {
        self.default.as_ref()
    }
    /// Returns the resolved delete action, when declared.
    pub const fn on_delete(&self) -> Option<OnDeleteAction> {
        self.on_delete
    }
    /// Returns the source location of the field declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked object type declaration without parser implementation values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedObjectType {
    id: TypeId,
    name: QualifiedSemanticName,
    fields: Vec<CheckedField>,
    location: SourceLocation,
}

impl CheckedObjectType {
    /// Returns the stable identity of the object type.
    pub const fn id(&self) -> TypeId {
        self.id
    }
    /// Returns the resolved qualified type name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }
    /// Returns checked fields in declaration order.
    pub fn fields(&self) -> &[CheckedField] {
        &self.fields
    }
    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// A checked source bundle ready for a later semantic-diff and apply stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedBundle {
    schemas: Vec<CheckedSchema>,
    object_types: Vec<CheckedObjectType>,
}

impl CheckedBundle {
    /// Returns submitted schema declarations in source order.
    pub fn schemas(&self) -> &[CheckedSchema] {
        &self.schemas
    }

    /// Returns submitted object declarations in source order.
    pub fn object_types(&self) -> &[CheckedObjectType] {
        &self.object_types
    }
}

/// A checked logical schema declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedSchema {
    name: QualifiedSemanticName,
    location: SourceLocation,
}

impl CheckedSchema {
    /// Returns the resolved logical schema name.
    pub fn name(&self) -> &QualifiedSemanticName {
        &self.name
    }

    /// Returns the source location of the declaration.
    pub fn location(&self) -> &SourceLocation {
        &self.location
    }
}

/// The result of parsing and checking a source bundle.
#[derive(Clone, Debug)]
pub struct CheckReport {
    parse_report: ParseReport,
    diagnostics: Vec<CompilerDiagnostic>,
    checked_bundle: Option<CheckedBundle>,
    candidate: Option<CatalogueSnapshot>,
}

impl CheckReport {
    /// Returns the retained parse report on both success and failure.
    pub fn parse_report(&self) -> &ParseReport {
        &self.parse_report
    }
    /// Returns syntax and semantic diagnostics in source order.
    pub fn diagnostics(&self) -> &[CompilerDiagnostic] {
        &self.diagnostics
    }
    /// Returns checked Orna-owned definitions when checking succeeds.
    pub fn checked_bundle(&self) -> Option<&CheckedBundle> {
        self.checked_bundle.as_ref()
    }
    /// Returns the immutable candidate catalogue when checking succeeds.
    pub fn candidate(&self) -> Option<&CatalogueSnapshot> {
        self.candidate.as_ref()
    }
}

#[derive(Clone, Copy)]
struct Header<'a> {
    declaration: &'a ObjectTypeDeclaration,
    logical_path: &'a str,
    id: TypeId,
}

fn check_parsed(parse_report: ParseReport, base: &CatalogueSnapshot) -> CheckReport {
    let mut diagnostics = parse_report.diagnostics().to_vec();
    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

    let mut checked_schemas = Vec::new();
    let mut known_schemas = base
        .object_types()
        .iter()
        .filter_map(|object_type| namespace_of(object_type.name()))
        .collect::<HashSet<_>>();
    let mut submitted_schemas = HashSet::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().schemas() {
            let name = semantic_name(&declaration.name);
            if !submitted_schemas.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate schema definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            known_schemas.insert(name.clone());
            checked_schemas.push(CheckedSchema {
                name,
                location: location(unit.logical_path(), &declaration.span),
            });
        }
    }

    let mut headers = Vec::new();
    let mut declarations_by_name = HashMap::<QualifiedSemanticName, usize>::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().object_types() {
            let name = semantic_name(&declaration.name);
            let Some(namespace) = namespace_of(&name) else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("object type {name} has no declared schema"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            };
            if !known_schemas.contains(&namespace) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown schema {namespace} for object type {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            if declarations_by_name.contains_key(&name) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate object type definition {name}"),
                    unit.logical_path(),
                    &declaration.name.span,
                ));
                continue;
            }
            declarations_by_name.insert(name.clone(), headers.len());
            let id = base
                .object_type_by_name(&name)
                .map_or_else(TypeId::new, ObjectTypeDefinition::id);
            headers.push(Header {
                declaration,
                logical_path: unit.logical_path(),
                id,
            });
        }
    }

    let mut submitted_ids = HashMap::new();
    for header in &headers {
        submitted_ids.insert(semantic_name(&header.declaration.name), header.id);
    }

    let mut checked_types = Vec::with_capacity(headers.len());
    for header in headers {
        let base_type = base.object_type_by_id(header.id);
        let mut field_names = HashSet::new();
        let mut checked_fields = Vec::with_capacity(header.declaration.fields.len());

        for field in &header.declaration.fields {
            let name = semantic_part(&field.name);
            if !field_names.insert(name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!(
                        "duplicate field definition {name} in {}",
                        semantic_name(&header.declaration.name)
                    ),
                    header.logical_path,
                    &field.name.span,
                ));
                continue;
            }

            let resolved_type = resolve_type(
                &field.type_specification,
                &submitted_ids,
                base,
                header.logical_path,
                &mut diagnostics,
            );
            let on_delete = map_on_delete(field.on_delete);
            if on_delete.is_some()
                && !matches!(
                    field.type_specification,
                    TypeSpecification::Reference { .. }
                )
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "ON DELETE is only valid for REF fields",
                    header.logical_path,
                    &field.span,
                ));
            }
            if matches!(on_delete, Some(OnDeleteAction::SetNull)) && !field.nullable {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "ON DELETE SET NULL requires a nullable field",
                    header.logical_path,
                    &field.span,
                ));
            }

            let id = base_type
                .and_then(|object_type| object_type.field_by_name(&name))
                .map_or_else(FieldId::new, FieldDefinition::id);
            let existing_default = base_type
                .and_then(|object_type| object_type.field_by_name(&name))
                .and_then(FieldDefinition::default_expression);
            let default = match (field.default_expression.as_ref(), resolved_type) {
                (Some(source), Some(resolved_type)) => checked_default(
                    source,
                    resolved_type,
                    field.nullable,
                    existing_default,
                    header.logical_path,
                    &mut diagnostics,
                ),
                _ => None,
            };

            if let Some(resolved_type) = resolved_type {
                checked_fields.push(CheckedField {
                    id,
                    name,
                    ordinal: field.order as u32,
                    resolved_type,
                    nullable: field.nullable,
                    unique: field.unique,
                    default,
                    on_delete,
                    location: location(header.logical_path, &field.span),
                });
            }
        }

        checked_types.push(CheckedObjectType {
            id: header.id,
            name: semantic_name(&header.declaration.name),
            fields: checked_fields,
            location: location(header.logical_path, &header.declaration.span),
        });
    }

    if parse_report
        .units()
        .iter()
        .any(|unit| !unit.parsed().server_functions().is_empty())
    {
        for unit in parse_report.units() {
            for function in unit.parsed().server_functions() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "SERVER function resolution is not implemented in this compiler slice",
                    unit.logical_path(),
                    &function.span,
                ));
            }
        }
    }

    if !diagnostics.is_empty() {
        return failed(parse_report, diagnostics);
    }

    let mut object_types = base.object_types().to_vec();
    for checked_type in &checked_types {
        let definition = ObjectTypeDefinition::new(
            checked_type.id,
            checked_type.name.clone(),
            checked_type
                .fields
                .iter()
                .map(as_field_definition)
                .collect(),
        );
        if let Some(index) = object_types
            .iter()
            .position(|candidate| candidate.id() == checked_type.id)
        {
            object_types[index] = definition;
        } else {
            object_types.push(definition);
        }
    }
    let candidate = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::new(),
        object_types,
        base.functions().to_vec(),
    )
    .expect("checked definitions satisfy catalogue invariants");
    CheckReport {
        parse_report,
        diagnostics,
        checked_bundle: Some(CheckedBundle {
            schemas: checked_schemas,
            object_types: checked_types,
        }),
        candidate: Some(candidate),
    }
}

fn failed(parse_report: ParseReport, diagnostics: Vec<CompilerDiagnostic>) -> CheckReport {
    CheckReport {
        parse_report,
        diagnostics,
        checked_bundle: None,
        candidate: None,
    }
}

fn resolve_type(
    specification: &TypeSpecification,
    submitted_ids: &HashMap<QualifiedSemanticName, TypeId>,
    base: &CatalogueSnapshot,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<ResolvedType> {
    match specification {
        TypeSpecification::Named(name) => {
            if let Some(scalar) = resolve_closed_scalar(name) {
                return Some(ResolvedType::scalar(scalar));
            }
            let semantic_name = semantic_name(name);
            if submitted_ids.contains_key(&semantic_name)
                || base.object_type_by_name(&semantic_name).is_some()
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("object type {semantic_name} must be declared with REF"),
                    logical_path,
                    &name.span,
                ));
            } else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown type name {semantic_name}"),
                    logical_path,
                    &name.span,
                ));
            }
            None
        }
        TypeSpecification::StandardLargeObject { kind, .. } => {
            let scalar = match kind {
                StandardLargeObjectKind::Character => StandardScalar::CharacterLargeObject,
                StandardLargeObjectKind::Binary => StandardScalar::BinaryLargeObject,
            };
            Some(ResolvedType::scalar(scalar))
        }
        TypeSpecification::Reference { target, .. } => {
            if resolve_closed_scalar(target).is_some() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::InvalidReferenceTarget,
                    format!("REF target {} is a scalar type", semantic_name(target)),
                    logical_path,
                    &target.span,
                ));
                return None;
            }
            let name = semantic_name(target);
            if let Some(id) = submitted_ids.get(&name).copied().or_else(|| {
                base.object_type_by_name(&name)
                    .map(ObjectTypeDefinition::id)
            }) {
                Some(ResolvedType::reference(id))
            } else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown object type {name}"),
                    logical_path,
                    &target.span,
                ));
                None
            }
        }
    }
}

fn resolve_closed_scalar(name: &QualifiedName) -> Option<StandardScalar> {
    if name.parts.len() != 1 || name.parts[0].text.starts_with('"') {
        return None;
    }
    StandardScalar::from_source_spelling(&name.parts[0].text).ok()
}

fn checked_default(
    source: &SourceSlice,
    resolved_type: ResolvedType,
    nullable: bool,
    existing_id: Option<ExpressionId>,
    logical_path: &str,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<CheckedDefault> {
    let value = match parse_constant(&source.text) {
        Some(value) => value,
        None => {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "only constant NULL, TRUE, FALSE, text, and integer defaults are supported",
                logical_path,
                &source.span,
            ));
            return None;
        }
    };
    let valid = match (&value, resolved_type) {
        (ConstantValue::Null, _) => nullable,
        (ConstantValue::Boolean(_), ResolvedType::Scalar(StandardScalar::Boolean)) => true,
        (
            ConstantValue::Integer(_),
            ResolvedType::Scalar(StandardScalar::Integer | StandardScalar::BigInt),
        ) => true,
        (ConstantValue::Text(_), ResolvedType::Scalar(StandardScalar::CharacterLargeObject)) => {
            true
        }
        _ => false,
    };
    if !valid {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "default constant does not match the field type and nullability",
            logical_path,
            &source.span,
        ));
        return None;
    }
    Some(CheckedDefault {
        id: existing_id.unwrap_or_default(),
        value,
        location: location(logical_path, &source.span),
    })
}

fn parse_constant(source: &str) -> Option<ConstantValue> {
    let source = source.trim();
    if source.eq_ignore_ascii_case("NULL") {
        return Some(ConstantValue::Null);
    }
    if source.eq_ignore_ascii_case("TRUE") {
        return Some(ConstantValue::Boolean(true));
    }
    if source.eq_ignore_ascii_case("FALSE") {
        return Some(ConstantValue::Boolean(false));
    }
    if source.len() >= 2 && source.starts_with('\'') && source.ends_with('\'') {
        return Some(ConstantValue::Text(
            source[1..source.len() - 1].replace("''", "'"),
        ));
    }
    source.parse::<i64>().ok().map(ConstantValue::Integer)
}

fn map_on_delete(policy: Option<OnDeletePolicy>) -> Option<OnDeleteAction> {
    match policy {
        Some(OnDeletePolicy::Restrict) => Some(OnDeleteAction::Restrict),
        Some(OnDeletePolicy::SetNull) => Some(OnDeleteAction::SetNull),
        Some(OnDeletePolicy::Cascade) => Some(OnDeleteAction::Cascade),
        None => None,
    }
}

fn as_field_definition(field: &CheckedField) -> FieldDefinition {
    FieldDefinition::new(
        field.id,
        field.name.clone(),
        field.ordinal,
        field.resolved_type,
        field.nullable,
        field.unique,
        field.default.as_ref().map(CheckedDefault::id),
        field.on_delete,
    )
}

fn semantic_name(name: &QualifiedName) -> QualifiedSemanticName {
    QualifiedSemanticName::new(name.parts.iter().map(semantic_part))
        .expect("parser produced a non-empty qualified name")
}

fn semantic_part(part: &NamePart) -> String {
    if part.text.starts_with('"') {
        part.text[1..part.text.len() - 1].replace("\"\"", "\"")
    } else {
        part.text.to_lowercase()
    }
}

fn namespace_of(name: &QualifiedSemanticName) -> Option<QualifiedSemanticName> {
    let namespace_parts = name.parts().get(..name.parts().len().checked_sub(1)?)?;
    if namespace_parts.is_empty() {
        return None;
    }
    QualifiedSemanticName::new(namespace_parts.iter().cloned()).ok()
}

fn location(logical_path: &str, span: &SourceSpan) -> SourceLocation {
    SourceLocation {
        logical_path: logical_path.to_owned(),
        span: ByteSpan::from_syntax_span(span),
    }
}

fn diagnostic(
    code: DiagnosticCode,
    message: impl Into<String>,
    logical_path: &str,
    span: &SourceSpan,
) -> CompilerDiagnostic {
    DiagnosticCode::semantic(code, message, location(logical_path, span))
}

#[cfg(test)]
mod tests {
    use orna_core::{
        CatalogueRevisionId, FunctionId, FunctionRevisionId,
        catalogue::{
            CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn,
            FunctionSecurity, FunctionVolatility, OnDeleteAction, QualifiedSemanticName,
        },
        source::{SourceBundle, SourceUnit},
        types::{ResolvedType, StandardScalar},
    };

    use super::{ConstantValue, DiagnosticCode, check};

    fn empty_catalogue() -> CatalogueSnapshot {
        CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new()).unwrap()
    }

    fn bundle(units: impl IntoIterator<Item = (&'static str, &'static str)>) -> SourceBundle {
        SourceBundle::new(
            units
                .into_iter()
                .map(|(path, source)| SourceUnit::new(path, source)),
        )
        .unwrap()
    }

    fn successful_candidate(source: &'static str) -> CatalogueSnapshot {
        check(&bundle([("schema.orna", source)]), &empty_catalogue())
            .candidate()
            .cloned()
            .expect("source must check")
    }

    #[test]
    fn resolves_forward_references_across_source_units() {
        let report = check(
            &bundle([
                (
                    "tasks.orna",
                    "CREATE SCHEMA tasks; CREATE TYPE tasks.task AS OBJECT (assignee REF people.person);",
                ),
                (
                    "people.orna",
                    "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT NOT NULL);",
                ),
            ]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let checked = report.checked_bundle().unwrap();
        assert_eq!(checked.schemas().len(), 2);
        assert_eq!(checked.schemas()[0].name().to_string(), "tasks");
        assert_eq!(checked.schemas()[1].name().to_string(), "people");
        let task = &checked.object_types()[0];
        let person = &checked.object_types()[1];
        assert_eq!(
            task.fields()[0].resolved_type(),
            ResolvedType::reference(person.id())
        );
    }

    #[test]
    fn maps_alias_defaults_nullability_and_delete_policies() {
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA people; CREATE SCHEMA tasks;\
                 CREATE TYPE people.person AS OBJECT (name TEXT NOT NULL);\
                 CREATE TYPE tasks.task AS OBJECT (\
                     done BOOL NOT NULL DEFAULT FALSE,\
                     count INT DEFAULT 7,\
                     note TEXT DEFAULT 'it''s fine',\
                     owner REF people.person ON DELETE SET NULL,\
                     document CLOB,\
                     payload BLOB\
                 );",
            )]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let fields = report.checked_bundle().unwrap().object_types()[1].fields();
        assert_eq!(
            fields[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
        assert!(!fields[0].nullable());
        assert_eq!(
            fields[0].default().unwrap().value(),
            &ConstantValue::Boolean(false)
        );
        assert_eq!(
            fields[1].resolved_type(),
            ResolvedType::scalar(StandardScalar::Integer)
        );
        assert_eq!(
            fields[1].default().unwrap().value(),
            &ConstantValue::Integer(7)
        );
        assert_eq!(
            fields[2].default().unwrap().value(),
            &ConstantValue::Text("it's fine".to_owned())
        );
        assert!(fields[3].nullable());
        assert_eq!(fields[3].on_delete(), Some(OnDeleteAction::SetNull));
        assert_eq!(
            fields[4].resolved_type(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            fields[5].resolved_type(),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject)
        );
    }

    #[test]
    fn resolves_canonical_multiword_large_object_scalars() {
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE SCHEMA files; CREATE TYPE files.document AS OBJECT (body cHaRaCtEr /* retained */ LaRgE ObJeCt, content bInArY LARGE object);",
            )]),
            &empty_catalogue(),
        );

        assert!(report.diagnostics().is_empty());
        let fields = report.checked_bundle().unwrap().object_types()[0].fields();
        assert_eq!(
            fields[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject)
        );
        assert_eq!(
            fields[1].resolved_type(),
            ResolvedType::scalar(StandardScalar::BinaryLargeObject)
        );
    }

    #[test]
    fn repeated_checks_preserve_matching_ids_even_when_fields_reorder() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT, age INT DEFAULT 1);",
        );
        let original = initial.object_types()[0].clone();
        let name_id = original.field_by_name("name").unwrap().id();
        let age = original.field_by_name("age").unwrap();

        let report = check(
            &bundle([(
                "renamed-file.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (age INT DEFAULT 1, name TEXT);",
            )]),
            &initial,
        );

        assert!(report.diagnostics().is_empty());
        let revised = &report.candidate().unwrap().object_types()[0];
        assert_eq!(revised.id(), original.id());
        assert_eq!(revised.field_by_name("name").unwrap().id(), name_id);
        assert_eq!(revised.field_by_name("age").unwrap().id(), age.id());
        assert_eq!(
            revised.field_by_name("age").unwrap().default_expression(),
            age.default_expression()
        );
    }

    #[test]
    fn added_field_gets_a_new_identity() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
        );
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE TYPE people.person AS OBJECT (name TEXT, email TEXT);",
            )]),
            &initial,
        );

        let initial_name = initial.object_types()[0]
            .field_by_name("name")
            .unwrap()
            .id();
        let revised = &report.candidate().unwrap().object_types()[0];
        assert_eq!(revised.field_by_name("name").unwrap().id(), initial_name);
        assert_ne!(revised.field_by_name("email").unwrap().id(), initial_name);
    }

    #[test]
    fn rejects_duplicates_unknown_names_invalid_references_and_defaults() {
        let report = check(
            &bundle([(
                "invalid.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (\
                     duplicated TEXT, duplicated INT,\
                     unknown missing.type,\
                     ref_scalar REF TEXT,\
                     plain_person people.person ON DELETE RESTRICT,\
                     required_ref REF people.person NOT NULL ON DELETE SET NULL,\
                     bad_default INT DEFAULT TRUE\
                 );\
                 CREATE TYPE people.person AS OBJECT (name TEXT);",
            )]),
            &empty_catalogue(),
        );

        let codes = report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>();
        assert!(codes.contains(&DiagnosticCode::DuplicateDefinition));
        assert!(codes.contains(&DiagnosticCode::UnknownQualifiedName));
        assert!(codes.contains(&DiagnosticCode::InvalidReferenceTarget));
        assert!(codes.contains(&DiagnosticCode::TypeMismatch));
        assert!(report.candidate().is_none());
    }

    #[test]
    fn a_renamed_declaration_gets_a_new_identity_and_preserves_unmentioned_types() {
        let initial = successful_candidate(
            "CREATE SCHEMA people; CREATE SCHEMA tasks;\
             CREATE TYPE people.person AS OBJECT (name TEXT);\
             CREATE TYPE tasks.task AS OBJECT (title TEXT);",
        );
        let person_id = initial.object_types()[0].id();
        let task_id = initial.object_types()[1].id();
        let report = check(
            &bundle([(
                "schema.orna",
                "CREATE TYPE people.customer AS OBJECT (name TEXT);",
            )]),
            &initial,
        );

        let candidate = report.candidate().unwrap();
        let ids = candidate
            .object_types()
            .iter()
            .map(|object_type| object_type.id())
            .collect::<Vec<_>>();
        assert!(ids.contains(&task_id));
        assert!(ids.contains(&person_id));
        assert_ne!(
            candidate
                .object_types()
                .iter()
                .find(|object_type| object_type.name().to_string() == "people.customer")
                .unwrap()
                .id(),
            person_id
        );
    }

    #[test]
    fn rejects_server_functions_until_function_resolution_exists() {
        let report = check(
            &bundle([(
                "functions.orna",
                "CREATE SERVER FUNCTION people.find() RETURNS TEXT AS SELECT 'x';",
            )]),
            &empty_catalogue(),
        );

        assert_eq!(report.diagnostics()[0].code(), DiagnosticCode::TypeMismatch);
        assert!(report.candidate().is_none());
    }

    #[test]
    fn object_overlay_preserves_existing_function_definitions() {
        let function = FunctionDefinition::new(
            FunctionId::new(),
            QualifiedSemanticName::new(["sys", "health"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        );
        let function_id = function.id();
        let base = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::new(),
            vec![],
            vec![function],
        )
        .unwrap();

        let report = check(
            &bundle([(
                "people.orna",
                "CREATE SCHEMA people; CREATE TYPE people.person AS OBJECT (name TEXT);",
            )]),
            &base,
        );

        assert!(report.diagnostics().is_empty());
        assert!(
            report
                .candidate()
                .unwrap()
                .function_by_id(function_id)
                .is_some()
        );
    }

    #[test]
    fn rejects_duplicate_and_unknown_schema_names_after_normalisation() {
        let report = check(
            &bundle([(
                "schemas.orna",
                "CREATE SCHEMA People;\
                 CREATE SCHEMA people;\
                 CREATE TYPE missing.contact AS OBJECT (name TEXT);",
            )]),
            &empty_catalogue(),
        );

        let codes = report
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code())
            .collect::<Vec<_>>();
        assert!(codes.contains(&DiagnosticCode::DuplicateDefinition));
        assert!(codes.contains(&DiagnosticCode::UnknownQualifiedName));
        assert!(report.candidate().is_none());
    }
}
