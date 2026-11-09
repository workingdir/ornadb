//! Catalogue-to-Rust generator for `orna-sys-id`.
//!
//! The Orna 1.1.0 catalogue (`catalogue/sys-1.1.0.json`, a byte copy of
//! `reference/Orna-1.1.0/api/sys.json`) is the only authority for portable identity.
//! `catalogue/engine-1.1.0.json` records the engine-private names and durable ids
//! that the 1.1.0 specification does not contain.
//!
//! This module is included by both `build.rs` and the crate's tests. The build only
//! writes generated files after [`generate`] has verified the catalogue bytes
//! against [`SYS_API_1_1_SHA256`] and every structural invariant below.

use crate::sha256;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write as _},
};

/// SHA-256 of `reference/Orna-1.1.0/api/sys.json`.
///
/// `crates/orna-conformance/tests/fixtures/reference/api/sys.json` carries the same
/// bytes; `tests/catalogue_bijection.rs` checks both the digest and the byte equality.
pub const SYS_API_1_1_SHA256: &str =
    "10ef7dab9665de4e065ee2b96797b2751c7c3f98f9886241011aee0cc8c0d40e";

/// SHA-256 of `catalogue/engine-1.1.0.json`.
///
/// The engine inventory is derived once and then frozen: its `FunctionId` bytes are
/// durable catalogue state, so an edit must be deliberate.
pub const ENGINE_1_1_0_SHA256: &str =
    "a0c5625c122b9e4c08d62c2c3eef3f3a09e1bbcd785e2c66a873a3c8ef10d254";

/// The catalogue counts fixed by `api/sys.json` `counts`.
pub const SYS_FUNCTION_COUNT: usize = 82;
pub const SYS_FAILURE_CODE_COUNT_PORTABLE: usize = 67;
pub const SYS_RELATION_COUNT: usize = 78;
pub const SYS_GROUPED_HANDLE_COUNT: usize = 78;
pub const SYS_SINGLETON_COUNT: usize = 4;
pub const SYS_ENUM_COUNT: usize = 42;
pub const SYS_VALUE_TYPE_COUNT: usize = 35;
pub const SYS_REFERENCE_ALIAS_COUNT: usize = 78;
pub const SYS_OPAQUE_IDENTIFIER_COUNT: usize = 21;
pub const SYS_REMOVED_NAME_COUNT: usize = 4;

/// Engine names that the 1.1.0 specification does not contain.
///
/// The inventory is the 88 spec-absent three-segment names measured in
/// `docs/design/sys-identity.md` section 1.2: 29 functions, 4 types, 2 retired 1.0.0
/// functions, 34 failure codes and 19 test-fixture names.
pub const ENGINE_SEALED_FUNCTION_COUNT: usize = 27;
pub const ENGINE_ONLY_FUNCTION_COUNT: usize = 2;
pub const ENGINE_ONLY_TYPE_COUNT: usize = 4;
pub const ENGINE_RETIRED_FUNCTION_COUNT: usize = 2;
pub const ENGINE_FAILURE_CODE_COUNT: usize = 34;
pub const ENGINE_TEST_FIXTURE_NAME_COUNT: usize = 19;
pub const ENGINE_NAME_COUNT: usize = 88;

/// The generated files, keyed by their file name below `OUT_DIR`.
#[derive(Debug)]
pub struct Generated {
    pub functions: String,
    pub codes: String,
    pub relations: String,
    pub removed: String,
    pub tables: String,
}

impl Generated {
    /// The generated files in a stable order.
    #[must_use]
    pub fn files(&self) -> [(&'static str, &str); 5] {
        [
            ("functions.rs", &self.functions),
            ("codes.rs", &self.codes),
            ("relations.rs", &self.relations),
            ("removed.rs", &self.removed),
            ("tables.rs", &self.tables),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenError(String);

impl GenError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for GenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GenError {}

type Result<T, E = GenError> = std::result::Result<T, E>;

/// Generates the crate tables, after checking the catalogue byte pin.
pub fn generate(sys_json: &str, engine_json: &str) -> Result<Generated> {
    let digest = sha256::hex(&sha256::digest(sys_json.as_bytes()));
    if digest != SYS_API_1_1_SHA256 {
        return Err(GenError::new(format!(
            "catalogue/sys-1.1.0.json sha256 is {digest}, pinned {SYS_API_1_1_SHA256}"
        )));
    }
    let digest = sha256::hex(&sha256::digest(engine_json.as_bytes()));
    if digest != ENGINE_1_1_0_SHA256 {
        return Err(GenError::new(format!(
            "catalogue/engine-1.1.0.json sha256 is {digest}, pinned {ENGINE_1_1_0_SHA256}"
        )));
    }
    generate_unchecked(sys_json, engine_json)
}

/// Generates the crate tables without the byte pin.
///
/// Only the mutation tests call this, to prove that structural catalogue edits are
/// rejected by validation rather than silently regenerated.
pub fn generate_unchecked(sys_json: &str, engine_json: &str) -> Result<Generated> {
    let catalogue = Catalogue::parse(sys_json)?;
    let engine = Engine::parse(engine_json)?;
    catalogue.validate_against(&engine)?;
    Ok(Generated {
        functions: render_functions(&catalogue),
        codes: render_codes(&catalogue, &engine),
        relations: render_relations(&catalogue, &engine),
        removed: render_removed(&catalogue),
        tables: render_tables(&catalogue, &engine),
    })
}

// ---------------------------------------------------------------------------
// JSON access helpers
// ---------------------------------------------------------------------------

fn json_value(sys_json: &str) -> Result<Value> {
    serde_json::from_str(sys_json).map_err(|error| GenError::new(format!("invalid JSON: {error}")))
}

fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| GenError::new(format!("{context} must be a JSON object")))
}

fn array<'a>(value: &'a Value, context: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| GenError::new(format!("{context} must be a JSON array")))
}

fn string<'a>(value: &'a Value, context: &str) -> Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| GenError::new(format!("{context} must be a JSON string")))
}

fn boolean(value: &Value, context: &str) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| GenError::new(format!("{context} must be a JSON boolean")))
}

fn count(value: &Value, context: &str) -> Result<usize> {
    value
        .as_u64()
        .map(|number| number as usize)
        .ok_or_else(|| GenError::new(format!("{context} must be a JSON integer")))
}

fn member<'a>(object: &'a Map<String, Value>, key: &str, context: &str) -> Result<&'a Value> {
    object
        .get(key)
        .ok_or_else(|| GenError::new(format!("{context} is missing `{key}`")))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(GenError::new(format!("`{key}` must be a string or null"))),
    }
}

fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    context: &str,
) -> Result<()> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(GenError::new(format!(
                "{context} has unknown field `{key}`"
            )));
        }
    }
    Ok(())
}

fn pascal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut capitalise = true;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            if capitalise {
                out.push(character.to_ascii_uppercase());
                capitalise = false;
            } else {
                out.push(character);
            }
        } else {
            capitalise = true;
        }
    }
    out
}

fn require_identifier(identifier: &str, context: &str) -> Result<()> {
    let mut characters = identifier.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => {
            return Err(GenError::new(format!(
                "{context} does not produce a Rust identifier: `{identifier}`"
            )));
        }
    }
    if !characters.all(|character| character.is_ascii_alphanumeric()) {
        return Err(GenError::new(format!(
            "{context} does not produce a Rust identifier: `{identifier}`"
        )));
    }
    Ok(())
}

fn insert_variant(
    variants: &mut BTreeSet<String>,
    identifier: String,
    context: &str,
) -> Result<()> {
    require_identifier(&identifier, context)?;
    if !variants.insert(identifier.clone()) {
        return Err(GenError::new(format!(
            "{context} produces duplicate variant `{identifier}`"
        )));
    }
    Ok(())
}

fn split_label(label: &str) -> Result<(&str, Option<&str>)> {
    if let Some(open) = label.find('(') {
        let overload = label[open + 1..].strip_suffix(')').ok_or_else(|| {
            GenError::new(format!("label `{label}` has an unterminated overload"))
        })?;
        if open == 0 || overload.is_empty() {
            return Err(GenError::new(format!("label `{label}` has an empty part")));
        }
        return Ok((&label[..open], Some(overload)));
    }
    if let Some(open) = label.find('<') {
        let overload = label[open + 1..].strip_suffix('>').ok_or_else(|| {
            GenError::new(format!("label `{label}` has an unterminated overload"))
        })?;
        if open == 0 || overload.is_empty() {
            return Err(GenError::new(format!("label `{label}` has an empty part")));
        }
        return Ok((&label[..open], Some(overload)));
    }
    Ok((label, None))
}

fn variant_from_label(label: &str) -> Result<String> {
    let (path, overload) = split_label(label)?;
    let mut segments = path.split('.');
    if segments.next() != Some("sys") {
        return Err(GenError::new(format!(
            "callable label `{label}` is not in the sys namespace"
        )));
    }
    let rest: Vec<&str> = segments.collect();
    if rest.is_empty() {
        return Err(GenError::new(format!(
            "callable label `{label}` has no name"
        )));
    }
    let mut variant = String::new();
    for segment in rest {
        variant.push_str(&pascal(segment));
    }
    if let Some(overload) = overload {
        variant.push_str(&pascal(overload));
    }
    Ok(variant)
}

fn variant_from_code(code: &str) -> Result<String> {
    let mut segments = code.split('.');
    if segments.next() != Some("sys") {
        return Err(GenError::new(format!(
            "failure code `{code}` is not in the sys namespace"
        )));
    }
    let mut variant = String::new();
    for segment in segments {
        variant.push_str(&pascal(segment));
    }
    if variant.is_empty() {
        return Err(GenError::new(format!("failure code `{code}` has no name")));
    }
    Ok(variant)
}

fn variant_from_name(name: &str) -> Result<String> {
    let stripped = name
        .strip_prefix("sys.")
        .ok_or_else(|| GenError::new(format!("member `{name}` is not in the sys namespace")))?;
    let mut variant = String::new();
    for segment in stripped.split(['.', '<', '>']) {
        variant.push_str(&pascal(segment));
    }
    if variant.is_empty() {
        return Err(GenError::new(format!("member `{name}` has no name")));
    }
    Ok(variant)
}

fn variant_from_field(field: &str) -> String {
    pascal(field)
}

fn hex_id(text: &str, context: &str) -> Result<[u8; 16]> {
    if text.len() != 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GenError::new(format!(
            "{context} must be 32 hexadecimal characters, found `{text}`"
        )));
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|error| GenError::new(format!("{context}: {error}")))?;
    }
    Ok(bytes)
}

fn render_id(id: [u8; 16]) -> String {
    let mut out = String::with_capacity(50);
    out.push('[');
    for (index, byte) in id.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write!(out, "0x{byte:02x}").expect("writing to a String cannot fail");
    }
    out.push(']');
    out
}

fn repr_for(len: usize) -> &'static str {
    if len <= u8::MAX as usize { "u8" } else { "u16" }
}

// ---------------------------------------------------------------------------
// Catalogue model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Effect {
    Read,
    Invoke,
    Admin,
}

impl Effect {
    fn parse(text: &str, context: &str) -> Result<Self> {
        match text {
            "read" => Ok(Self::Read),
            "invoke" => Ok(Self::Invoke),
            "admin" => Ok(Self::Admin),
            _ => Err(GenError::new(format!(
                "{context} has unknown effect `{text}`"
            ))),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Invoke => "invoke",
            Self::Admin => "admin",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeKind {
    Opaque,
    OpaqueGeneric,
    Record,
    RecordGeneric,
    Alias,
}

impl TypeKind {
    fn parse(text: &str, context: &str) -> Result<Self> {
        match text {
            "opaque" => Ok(Self::Opaque),
            "opaque-generic" => Ok(Self::OpaqueGeneric),
            "record" => Ok(Self::Record),
            "record-generic" => Ok(Self::RecordGeneric),
            "alias" => Ok(Self::Alias),
            _ => Err(GenError::new(format!(
                "{context} has unknown value-type kind `{text}`"
            ))),
        }
    }

    const fn variant(self) -> &'static str {
        match self {
            Self::Opaque => "Opaque",
            Self::OpaqueGeneric => "OpaqueGeneric",
            Self::Record => "Record",
            Self::RecordGeneric => "RecordGeneric",
            Self::Alias => "Alias",
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::OpaqueGeneric => "opaque-generic",
            Self::Record => "record",
            Self::RecordGeneric => "record-generic",
            Self::Alias => "alias",
        }
    }
}

#[derive(Debug)]
struct FunctionModel {
    variant: String,
    label: String,
    path: String,
    overload: Option<String>,
    signature: String,
    effect: Effect,
    purpose: String,
    snapshot_rule: Option<String>,
    ownership: Option<String>,
    contract: Option<String>,
    preconditions: Option<String>,
    since: Option<String>,
}

#[derive(Debug)]
struct CodeModel {
    variant: String,
    code: String,
    origin: OriginKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OriginKind {
    Portable,
    Engine,
}

impl OriginKind {
    const fn variant(self) -> &'static str {
        match self {
            Self::Portable => "Origin::Portable",
            Self::Engine => "Origin::Engine",
        }
    }
}

#[derive(Debug)]
struct RelationModel {
    variant: String,
    name: String,
    handle_variant: String,
    handle: String,
    kind: String,
    availability: String,
    key: String,
    reference_type: String,
    purpose: String,
    writable: bool,
    fields: Vec<(String, String)>,
}

#[derive(Debug)]
struct SingletonModel {
    variant: String,
    name: String,
    view_type: String,
    availability: String,
    fields: Vec<String>,
}

#[derive(Debug)]
struct EnumModel {
    variant: String,
    name: String,
    values: Vec<(String, String)>,
}

#[derive(Debug)]
struct TypeModel {
    variant: String,
    name: String,
    kind: TypeKind,
    target: Option<String>,
    purpose: Option<String>,
    type_parameters: Vec<String>,
}

#[derive(Debug)]
struct RemovedModel {
    variant: String,
    name: String,
    replacement: Option<String>,
    diagnostic: String,
    reason: Option<String>,
}

#[derive(Debug)]
enum MemberPathModel {
    Callable(String),
    Relation(String),
    GroupedHandle(String),
    Singleton(String),
    SingletonField(String, String),
}

#[derive(Debug)]
enum DiagnosticModel {
    Rule(String),
    Failure(String),
}

#[derive(Debug)]
enum RemovalModel {
    Replaced {
        by: MemberPathModel,
        diagnostic: DiagnosticModel,
    },
    Retired {
        diagnostic: DiagnosticModel,
        reason: String,
    },
}

#[derive(Debug)]
struct RuleModel {
    constant: String,
    text: String,
}

#[derive(Debug)]
struct Catalogue {
    language_version: String,
    sys_version: String,
    functions: Vec<FunctionModel>,
    portable_codes: Vec<CodeModel>,
    relations: Vec<RelationModel>,
    singletons: Vec<SingletonModel>,
    enums: Vec<EnumModel>,
    types: Vec<TypeModel>,
    fields: Vec<(String, String)>,
    removed: Vec<RemovedModel>,
    removals: Vec<(String, RemovalModel)>,
    rules: Vec<RuleModel>,
    function_paths: BTreeMap<String, Vec<String>>,
    code_by_text: BTreeMap<String, String>,
}

impl Catalogue {
    fn parse(sys_json: &str) -> Result<Self> {
        let root = json_value(sys_json)?;
        let root = object(&root, "catalogue")?;

        let counts = object(member(root, "counts", "catalogue")?, "catalogue.counts")?;
        let expected = [
            ("functions", SYS_FUNCTION_COUNT),
            ("failure_codes", SYS_FAILURE_CODE_COUNT_PORTABLE),
            ("relations", SYS_RELATION_COUNT),
            ("singletons", SYS_SINGLETON_COUNT),
            ("enums", SYS_ENUM_COUNT),
            ("value_types", SYS_VALUE_TYPE_COUNT),
            ("reference_aliases", SYS_REFERENCE_ALIAS_COUNT),
            ("opaque_identifiers", SYS_OPAQUE_IDENTIFIER_COUNT),
        ];

        let language_version = string(
            member(root, "language_version", "catalogue")?,
            "catalogue.language_version",
        )?
        .to_owned();
        let sys_version = string(
            member(root, "sys_version", "catalogue")?,
            "catalogue.sys_version",
        )?
        .to_owned();
        if language_version != "1.1.0" || sys_version != "1.1" {
            return Err(GenError::new(format!(
                "catalogue versions changed: language `{language_version}`, sys `{sys_version}`"
            )));
        }

        let function_values = array(member(root, "functions", "catalogue")?, "functions")?;
        let relation_values = array(member(root, "relations", "catalogue")?, "relations")?;
        let singleton_values = array(member(root, "singletons", "catalogue")?, "singletons")?;
        let enum_map = object(member(root, "enums", "catalogue")?, "enums")?;
        let value_type_values = array(member(root, "value_types", "catalogue")?, "value_types")?;
        let alias_values = array(
            member(root, "reference_aliases", "catalogue")?,
            "reference_aliases",
        )?;
        let opaque_values = array(
            member(root, "opaque_identifiers", "catalogue")?,
            "opaque_identifiers",
        )?;
        let removed_map = object(member(root, "removed_names", "catalogue")?, "removed_names")?;

        let measured = [
            ("functions", function_values.len()),
            (
                "failure_codes",
                array(member(root, "failure_codes", "catalogue")?, "failure_codes")?.len(),
            ),
            ("relations", relation_values.len()),
            ("singletons", singleton_values.len()),
            ("enums", enum_map.len()),
            ("value_types", value_type_values.len()),
            ("reference_aliases", alias_values.len()),
            ("opaque_identifiers", opaque_values.len()),
        ];
        for (key, actual) in measured {
            let declared = count(member(counts, key, "counts")?, key)?;
            let expected = expected
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, expected)| *expected)
                .expect("every measured set has an expected count");
            if declared != expected || actual != expected {
                return Err(GenError::new(format!(
                    "count `{key}` is declared {declared}, measured {actual}, expected {expected}"
                )));
            }
        }

        let mut function_variants = BTreeSet::new();
        let mut functions = Vec::with_capacity(function_values.len());
        let mut function_by_label = BTreeMap::new();
        let mut function_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (index, value) in function_values.iter().enumerate() {
            let context = format!("functions[{index}]");
            let entry = object(value, &context)?;
            reject_unknown_fields(
                entry,
                &[
                    "name",
                    "effect",
                    "signature",
                    "purpose",
                    "snapshot_rule",
                    "ownership",
                    "contract",
                    "preconditions",
                    "since",
                ],
                &context,
            )?;
            let label = string(member(entry, "name", &context)?, &context)?.to_owned();
            let variant = variant_from_label(&label)?;
            insert_variant(&mut function_variants, variant.clone(), &context)?;
            let (path, overload) = split_label(&label)?;
            let path = path.to_owned();
            let overload = overload.map(str::to_owned);
            let signature = string(member(entry, "signature", &context)?, &context)?.to_owned();
            let effect = Effect::parse(
                string(member(entry, "effect", &context)?, &context)?,
                &context,
            )?;
            let purpose = string(member(entry, "purpose", &context)?, &context)?.to_owned();
            if function_by_label
                .insert(label.clone(), variant.clone())
                .is_some()
            {
                return Err(GenError::new(format!("duplicate callable label `{label}`")));
            }
            function_paths
                .entry(path.clone())
                .or_default()
                .push(variant.clone());
            functions.push(FunctionModel {
                variant,
                label,
                path,
                overload,
                signature,
                effect,
                purpose,
                snapshot_rule: optional_string(entry, "snapshot_rule")?,
                ownership: optional_string(entry, "ownership")?,
                contract: optional_string(entry, "contract")?,
                preconditions: optional_string(entry, "preconditions")?,
                since: optional_string(entry, "since")?,
            });
        }

        let mut code_variants = BTreeSet::new();
        let mut code_by_text = BTreeMap::new();
        let mut portable_codes = Vec::new();
        let code_values = array(member(root, "failure_codes", "catalogue")?, "failure_codes")?;
        for (index, value) in code_values.iter().enumerate() {
            let context = format!("failure_codes[{index}]");
            let code = string(value, &context)?.to_owned();
            let variant = variant_from_code(&code)?;
            insert_variant(&mut code_variants, variant.clone(), &context)?;
            if code_by_text.insert(code.clone(), variant.clone()).is_some() {
                return Err(GenError::new(format!("duplicate failure code `{code}`")));
            }
            portable_codes.push(CodeModel {
                variant,
                code,
                origin: OriginKind::Portable,
            });
        }

        let mut type_records: Vec<TypeModel> = Vec::new();
        let mut type_variants = BTreeSet::new();
        for (index, value) in value_type_values.iter().enumerate() {
            let context = format!("value_types[{index}]");
            let entry = object(value, &context)?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            let variant = variant_from_name(&name)?;
            insert_variant(&mut type_variants, variant.clone(), &context)?;
            let kind = TypeKind::parse(
                string(member(entry, "kind", &context)?, &context)?,
                &context,
            )?;
            let type_parameters = array(
                member(entry, "type_parameters", &context)?,
                &format!("{context}.type_parameters"),
            )?
            .iter()
            .map(|value| string(value, &context).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
            type_records.push(TypeModel {
                variant,
                name,
                kind,
                target: None,
                purpose: Some(string(member(entry, "purpose", &context)?, &context)?.to_owned()),
                type_parameters,
            });
        }
        for (index, value) in alias_values.iter().enumerate() {
            let context = format!("reference_aliases[{index}]");
            let entry = object(value, &context)?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            let variant = variant_from_name(&name)?;
            insert_variant(&mut type_variants, variant.clone(), &context)?;
            type_records.push(TypeModel {
                variant,
                name,
                kind: TypeKind::Alias,
                target: Some(string(member(entry, "target", &context)?, &context)?.to_owned()),
                purpose: None,
                type_parameters: Vec::new(),
            });
        }
        for (index, value) in opaque_values.iter().enumerate() {
            let context = format!("opaque_identifiers[{index}]");
            let name = string(value, &context)?.to_owned();
            let variant = variant_from_name(&name)?;
            insert_variant(&mut type_variants, variant.clone(), &context)?;
            type_records.push(TypeModel {
                variant,
                name,
                kind: TypeKind::Opaque,
                target: None,
                purpose: None,
                type_parameters: Vec::new(),
            });
        }
        let type_by_name: BTreeMap<String, usize> = type_records
            .iter()
            .enumerate()
            .map(|(index, record)| (record.name.clone(), index))
            .collect();
        if type_by_name.len() != type_records.len() {
            return Err(GenError::new("duplicate system type name"));
        }

        let mut field_by_name: BTreeMap<String, String> = BTreeMap::new();
        let mut fields = Vec::new();
        let mut field_names = BTreeSet::new();
        let mut relation_fields_by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (index, value) in relation_values.iter().enumerate() {
            let context = format!("relations[{index}]");
            let entry = object(value, &context)?;
            let field_values = array(member(entry, "fields", &context)?, &context)?;
            let mut relation_fields = Vec::new();
            for (field_index, field_value) in field_values.iter().enumerate() {
                let field_context = format!("{context}.fields[{field_index}]");
                let field = object(field_value, &field_context)?;
                let field_name =
                    string(member(field, "name", &field_context)?, &field_context)?.to_owned();
                let type_name =
                    string(member(field, "type", &field_context)?, &field_context)?.to_owned();
                let variable = variant_from_field(&field_name);
                if field_names.insert(field_name.clone()) {
                    fields.push((variable.clone(), field_name.clone()));
                }
                field_by_name
                    .entry(field_name.clone())
                    .or_insert_with(|| variable.clone());
                relation_fields.push((variable, type_name));
            }
            relation_fields_by_name.insert(
                string(member(entry, "name", &context)?, &context)?.to_owned(),
                relation_fields,
            );
        }
        for (index, value) in value_type_values.iter().enumerate() {
            let context = format!("value_types[{index}].fields");
            let entry = object(value, &context)?;
            let field_values = array(member(entry, "fields", &context)?, &context)?;
            for (field_index, field_value) in field_values.iter().enumerate() {
                let field_context = format!("{context}[{field_index}]");
                let field = object(field_value, &field_context)?;
                let field_name =
                    string(member(field, "name", &field_context)?, &field_context)?.to_owned();
                let variable = variant_from_field(&field_name);
                if field_names.insert(field_name.clone()) {
                    fields.push((variable.clone(), field_name.clone()));
                }
                field_by_name
                    .entry(field_name.clone())
                    .or_insert_with(|| variable.clone());
            }
        }

        let mut relation_variants = BTreeSet::new();
        let mut handle_variants = BTreeSet::new();
        let mut relation_by_name = BTreeMap::new();
        let mut handle_by_name = BTreeMap::new();
        let mut relations = Vec::new();
        for (index, value) in relation_values.iter().enumerate() {
            let context = format!("relations[{index}]");
            let entry = object(value, &context)?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            let handle = string(member(entry, "grouped_handle", &context)?, &context)?.to_owned();
            let variant = variant_from_name(&name)?;
            let handle_variant = variant_from_name(&handle)?;
            insert_variant(&mut relation_variants, variant.clone(), &context)?;
            insert_variant(&mut handle_variants, handle_variant.clone(), &context)?;
            if relation_by_name
                .insert(name.clone(), variant.clone())
                .is_some()
            {
                return Err(GenError::new(format!("duplicate relation `{name}`")));
            }
            if handle_by_name
                .insert(handle.clone(), handle_variant.clone())
                .is_some()
            {
                return Err(GenError::new(format!(
                    "duplicate grouped handle `{handle}`"
                )));
            }
            relations.push(RelationModel {
                variant,
                name,
                handle_variant,
                handle,
                kind: string(member(entry, "kind", &context)?, &context)?.to_owned(),
                availability: string(member(entry, "availability", &context)?, &context)?
                    .to_owned(),
                key: string(member(entry, "key", &context)?, &context)?.to_owned(),
                reference_type: string(member(entry, "reference_type", &context)?, &context)?
                    .to_owned(),
                purpose: string(member(entry, "purpose", &context)?, &context)?.to_owned(),
                writable: boolean(member(entry, "writable", &context)?, &context)?,
                fields: relation_fields_by_name
                    .remove(string(member(entry, "name", &context)?, &context)?)
                    .expect("relation fields were collected in the first pass"),
            });
        }

        let mut singleton_variants = BTreeSet::new();
        let mut singleton_by_name = BTreeMap::new();
        let mut singleton_field_variants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut singletons = Vec::new();
        for (index, value) in singleton_values.iter().enumerate() {
            let context = format!("singletons[{index}]");
            let entry = object(value, &context)?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            let view_type = string(member(entry, "type", &context)?, &context)?.to_owned();
            let variant = variant_from_name(&name)?;
            insert_variant(&mut singleton_variants, variant.clone(), &context)?;
            if singleton_by_name
                .insert(name.clone(), variant.clone())
                .is_some()
            {
                return Err(GenError::new(format!("duplicate singleton `{name}`")));
            }
            let type_index = type_by_name.get(&view_type).ok_or_else(|| {
                GenError::new(format!(
                    "singleton `{name}` names undeclared view type `{view_type}`"
                ))
            })?;
            let view = &type_records[*type_index];
            if view.kind != TypeKind::Record {
                return Err(GenError::new(format!(
                    "singleton `{name}` view type `{view_type}` is not a record"
                )));
            }
            let view_entry = object(&value_type_values[*type_index], "value type")?;
            let view_fields = array(member(view_entry, "fields", "value type")?, "fields")?;
            let mut declared = BTreeSet::new();
            let mut field_variants = Vec::new();
            for field_value in view_fields {
                let field = object(field_value, "value type field")?;
                let field_name = string(member(field, "name", "value type field")?, "field")?;
                let variable = field_by_name.get(field_name).ok_or_else(|| {
                    GenError::new(format!("undeclared singleton field `{field_name}`"))
                })?;
                declared.insert(variable.clone());
                field_variants.push(variable.clone());
            }
            singleton_field_variants.insert(variant.clone(), declared);
            singletons.push(SingletonModel {
                variant,
                name,
                view_type,
                availability: string(member(entry, "availability", &context)?, &context)?
                    .to_owned(),
                fields: field_variants,
            });
        }

        let mut enum_variants = BTreeSet::new();
        let mut enum_value_variants = BTreeSet::new();
        let mut enums = Vec::new();
        let mut enum_names: Vec<&String> = enum_map.keys().collect();
        enum_names.sort();
        for name in enum_names {
            let values = array(&enum_map[name], &format!("enums.{name}"))?;
            let variant = variant_from_name(name)?;
            insert_variant(
                &mut enum_variants,
                variant.clone(),
                &format!("enums.{name}"),
            )?;
            let mut value_models = Vec::new();
            let mut seen_values = BTreeSet::new();
            for (index, value) in values.iter().enumerate() {
                let text = string(value, &format!("enums.{name}[{index}]"))?;
                if !seen_values.insert(text) {
                    return Err(GenError::new(format!(
                        "enum `{name}` repeats value `{text}`"
                    )));
                }
                let value_variant = format!("{variant}{}", pascal(text));
                insert_variant(
                    &mut enum_value_variants,
                    value_variant.clone(),
                    &format!("enums.{name}[{index}]"),
                )?;
                value_models.push((value_variant, text.to_owned()));
            }
            if value_models.is_empty() {
                return Err(GenError::new(format!("enum `{name}` has no values")));
            }
            enums.push(EnumModel {
                variant,
                name: name.clone(),
                values: value_models,
            });
        }

        let mut removed_variants = BTreeSet::new();
        let mut removed = Vec::new();
        let mut removed_names: Vec<&String> = removed_map.keys().collect();
        removed_names.sort();
        for name in removed_names {
            let context = format!("removed_names.{name}");
            let entry = object(&removed_map[name], &context)?;
            reject_unknown_fields(entry, &["replacement", "diagnostic", "reason"], &context)?;
            let variant = variant_from_name(name)?;
            insert_variant(&mut removed_variants, variant.clone(), &context)?;
            removed.push(RemovedModel {
                variant,
                name: name.clone(),
                replacement: optional_string(entry, "replacement")?,
                diagnostic: string(member(entry, "diagnostic", &context)?, &context)?.to_owned(),
                reason: optional_string(entry, "reason")?,
            });
        }

        let mut rules: Vec<RuleModel> = Vec::new();
        let mut removals = Vec::new();
        for entry in &removed {
            let diagnostic = resolve_diagnostic(&entry.diagnostic, &code_by_text)?;
            if let DiagnosticModel::Rule(text) = &diagnostic
                && !rules.iter().any(|rule| &rule.text == text)
            {
                rules.push(RuleModel {
                    constant: text.replace('-', "_"),
                    text: text.clone(),
                });
            }
            let removal = match &entry.replacement {
                Some(replacement) => RemovalModel::Replaced {
                    by: resolve_member_path(
                        replacement,
                        &function_by_label,
                        &relation_by_name,
                        &handle_by_name,
                        &singleton_by_name,
                        &singleton_field_variants,
                    )?,
                    diagnostic,
                },
                None => RemovalModel::Retired {
                    diagnostic,
                    reason: entry.reason.clone().ok_or_else(|| {
                        GenError::new(format!(
                            "removed name `{}` is retired without a reason",
                            entry.name
                        ))
                    })?,
                },
            };
            removals.push((entry.variant.clone(), removal));
        }

        Ok(Self {
            language_version,
            sys_version,
            functions,
            portable_codes,
            relations,
            singletons,
            enums,
            types: type_records,
            fields,
            removed,
            removals,
            rules,
            function_paths,
            code_by_text,
        })
    }

    fn validate_against(&self, engine: &Engine) -> Result<()> {
        let function_labels: BTreeSet<&str> = self
            .functions
            .iter()
            .map(|function| function.label.as_str())
            .collect();
        for name in engine.sealed_names() {
            if function_labels.contains(name) {
                return Err(GenError::new(format!(
                    "engine name `{name}` duplicates a catalogue callable"
                )));
            }
        }
        let mut code_variants: BTreeSet<String> = self
            .portable_codes
            .iter()
            .map(|code| code.variant.clone())
            .collect();
        for code in &engine.failure_codes {
            if self.code_by_text.contains_key(code) {
                return Err(GenError::new(format!(
                    "engine failure code `{code}` is also a portable code"
                )));
            }
            let variant = variant_from_code(code)?;
            if !code_variants.insert(variant.clone()) {
                return Err(GenError::new(format!(
                    "engine failure code `{code}` produces duplicate variant `{variant}`"
                )));
            }
        }
        Ok(())
    }
}

fn resolve_diagnostic(
    text: &str,
    code_by_text: &BTreeMap<String, String>,
) -> Result<DiagnosticModel> {
    if text.starts_with("ORNA") {
        if !text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(GenError::new(format!(
                "diagnostic rule `{text}` is not a plain ORNA rule code"
            )));
        }
        return Ok(DiagnosticModel::Rule(text.to_owned()));
    }
    let variant = code_by_text.get(text).ok_or_else(|| {
        GenError::new(format!(
            "diagnostic `{text}` is neither an ORNA rule nor a closed sys failure code"
        ))
    })?;
    Ok(DiagnosticModel::Failure(variant.clone()))
}

fn resolve_member_path(
    text: &str,
    function_by_label: &BTreeMap<String, String>,
    relation_by_name: &BTreeMap<String, String>,
    handle_by_name: &BTreeMap<String, String>,
    singleton_by_name: &BTreeMap<String, String>,
    singleton_field_variants: &BTreeMap<String, BTreeSet<String>>,
) -> Result<MemberPathModel> {
    if let Some(variant) = function_by_label.get(text) {
        return Ok(MemberPathModel::Callable(variant.clone()));
    }
    if let Some(variant) = relation_by_name.get(text) {
        return Ok(MemberPathModel::Relation(variant.clone()));
    }
    if let Some(variant) = handle_by_name.get(text) {
        return Ok(MemberPathModel::GroupedHandle(variant.clone()));
    }
    if let Some(variant) = singleton_by_name.get(text) {
        return Ok(MemberPathModel::Singleton(variant.clone()));
    }
    for (singleton_name, singleton_variant) in singleton_by_name {
        let prefix = format!("{singleton_name}.");
        if let Some(field) = text.strip_prefix(&prefix) {
            let variable = variant_from_field(field);
            let declared = singleton_field_variants
                .get(singleton_variant)
                .expect("every singleton records its declared fields");
            if declared.contains(&variable) {
                return Ok(MemberPathModel::SingletonField(
                    singleton_variant.clone(),
                    variable,
                ));
            }
            return Err(GenError::new(format!(
                "`{text}` names field `{field}` that singleton `{singleton_name}` does not declare"
            )));
        }
    }
    Err(GenError::new(format!(
        "removed-name replacement `{text}` is not a closed catalogue member"
    )))
}

// ---------------------------------------------------------------------------
// Engine model
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum SignatureModel {
    Health,
    SourceIntrospection,
    Invocation {
        parameter_id: [u8; 16],
        parameter_name: String,
        request_type: [u8; 16],
        request_required: bool,
        request_non_null: bool,
        event_stream_item_type: [u8; 16],
    },
    Inspect {
        parameter_count: usize,
        parameter_types: Vec<[u8; 16]>,
        result_type: Option<[u8; 16]>,
        stream_item_type: Option<[u8; 16]>,
    },
    Security {
        parameter_count: usize,
        returns_set: bool,
        returns_ref_principal: bool,
        returns_boolean: bool,
        stream_item_type: Option<[u8; 16]>,
    },
    State {
        parameter_count: usize,
        returns_set: bool,
    },
}

#[derive(Debug)]
struct EngineFunctionModel {
    name: String,
    name_parts: Vec<String>,
    function_id: Option<[u8; 16]>,
    sealed: bool,
    signature: Option<SignatureModel>,
}

#[derive(Debug)]
struct EngineTypeModel {
    name: String,
    type_id: [u8; 16],
    representation_contract: String,
}

#[derive(Debug)]
struct RetiredFunctionModel {
    name: String,
    since: String,
    signature: String,
    reason: String,
}

#[derive(Debug)]
struct Engine {
    sealed: Vec<EngineFunctionModel>,
    only: Vec<EngineFunctionModel>,
    types: Vec<EngineTypeModel>,
    retired: Vec<RetiredFunctionModel>,
    failure_codes: Vec<String>,
    test_fixture_names: Vec<String>,
}

impl Engine {
    fn parse(engine_json: &str) -> Result<Self> {
        let root = json_value(engine_json)?;
        let root = object(&root, "engine catalogue")?;
        if string(member(root, "origin", "engine catalogue")?, "origin")? != "engine" {
            return Err(GenError::new(
                "engine catalogue must declare origin `engine`",
            ));
        }
        if string(
            member(root, "spec_version", "engine catalogue")?,
            "spec_version",
        )? != "1.1.0"
        {
            return Err(GenError::new(
                "engine catalogue must target spec version 1.1.0",
            ));
        }

        let mut sealed = Vec::new();
        let mut names = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for (index, value) in array(
            member(root, "sealed_functions", "engine catalogue")?,
            "sealed_functions",
        )?
        .iter()
        .enumerate()
        {
            let context = format!("sealed_functions[{index}]");
            let entry = object(value, &context)?;
            reject_unknown_fields(
                entry,
                &["name", "name_parts", "function_id", "signature"],
                &context,
            )?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            let name_parts = array(member(entry, "name_parts", &context)?, &context)?
                .iter()
                .map(|part| string(part, &context).map(str::to_owned))
                .collect::<Result<Vec<_>>>()?;
            if name_parts.join(".") != name {
                return Err(GenError::new(format!(
                    "{context} name parts do not spell `{name}`"
                )));
            }
            let function_id = hex_id(
                string(member(entry, "function_id", &context)?, &context)?,
                &context,
            )?;
            if !ids.insert(function_id) {
                return Err(GenError::new(format!(
                    "{context} repeats a durable function id"
                )));
            }
            if !names.insert(name.clone()) {
                return Err(GenError::new(format!("engine name `{name}` appears twice")));
            }
            let signature = parse_signature(member(entry, "signature", &context)?, &context)?;
            sealed.push(EngineFunctionModel {
                name,
                name_parts,
                function_id: Some(function_id),
                sealed: true,
                signature: Some(signature),
            });
        }
        if sealed.len() != ENGINE_SEALED_FUNCTION_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue has {} sealed functions, expected {ENGINE_SEALED_FUNCTION_COUNT}",
                sealed.len()
            )));
        }

        let mut only = Vec::new();
        for (index, value) in array(
            member(root, "engine_only_functions", "engine catalogue")?,
            "engine_only_functions",
        )?
        .iter()
        .enumerate()
        {
            let context = format!("engine_only_functions[{index}]");
            let entry = object(value, &context)?;
            reject_unknown_fields(entry, &["name", "function_id", "note"], &context)?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            if !names.insert(name.clone()) {
                return Err(GenError::new(format!("engine name `{name}` appears twice")));
            }
            if !matches!(entry.get("function_id"), None | Some(Value::Null)) {
                return Err(GenError::new(format!(
                    "{context} must not claim a durable function id"
                )));
            }
            only.push(EngineFunctionModel {
                name_parts: name.split('.').map(str::to_owned).collect(),
                name,
                function_id: None,
                sealed: false,
                signature: None,
            });
        }
        if only.len() != ENGINE_ONLY_FUNCTION_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue has {} engine-only functions, expected {ENGINE_ONLY_FUNCTION_COUNT}",
                only.len()
            )));
        }

        let mut types = Vec::new();
        let mut type_ids = BTreeSet::new();
        for (index, value) in array(
            member(root, "engine_only_types", "engine catalogue")?,
            "engine_only_types",
        )?
        .iter()
        .enumerate()
        {
            let context = format!("engine_only_types[{index}]");
            let entry = object(value, &context)?;
            reject_unknown_fields(
                entry,
                &["name", "type_id", "representation_contract"],
                &context,
            )?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            if !names.insert(name.clone()) {
                return Err(GenError::new(format!("engine name `{name}` appears twice")));
            }
            let type_id = hex_id(
                string(member(entry, "type_id", &context)?, &context)?,
                &context,
            )?;
            if !type_ids.insert(type_id) {
                return Err(GenError::new(format!(
                    "{context} repeats a durable type id"
                )));
            }
            types.push(EngineTypeModel {
                name,
                type_id,
                representation_contract: string(
                    member(entry, "representation_contract", &context)?,
                    &context,
                )?
                .to_owned(),
            });
        }
        if types.len() != ENGINE_ONLY_TYPE_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue has {} engine-only types, expected {ENGINE_ONLY_TYPE_COUNT}",
                types.len()
            )));
        }

        let mut retired = Vec::new();
        for (index, value) in array(
            member(root, "retired_functions", "engine catalogue")?,
            "retired_functions",
        )?
        .iter()
        .enumerate()
        {
            let context = format!("retired_functions[{index}]");
            let entry = object(value, &context)?;
            reject_unknown_fields(entry, &["name", "since", "signature", "reason"], &context)?;
            let name = string(member(entry, "name", &context)?, &context)?.to_owned();
            retired.push(RetiredFunctionModel {
                name,
                since: string(member(entry, "since", &context)?, &context)?.to_owned(),
                signature: string(member(entry, "signature", &context)?, &context)?.to_owned(),
                reason: string(member(entry, "reason", &context)?, &context)?.to_owned(),
            });
        }
        if retired.len() != ENGINE_RETIRED_FUNCTION_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue has {} retired functions, expected {ENGINE_RETIRED_FUNCTION_COUNT}",
                retired.len()
            )));
        }

        let mut failure_codes = Vec::new();
        for (index, value) in array(
            member(root, "failure_codes", "engine catalogue")?,
            "failure_codes",
        )?
        .iter()
        .enumerate()
        {
            let context = format!("failure_codes[{index}]");
            let code = string(value, &context)?.to_owned();
            if code.split('.').count() != 3 {
                return Err(GenError::new(format!(
                    "engine failure code `{code}` must have three segments"
                )));
            }
            if failure_codes.contains(&code) {
                return Err(GenError::new(format!(
                    "engine failure code `{code}` appears twice"
                )));
            }
            failure_codes.push(code);
        }
        if failure_codes.len() != ENGINE_FAILURE_CODE_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue has {} failure codes, expected {ENGINE_FAILURE_CODE_COUNT}",
                failure_codes.len()
            )));
        }

        let mut test_fixture_names = Vec::new();
        for (index, value) in array(
            member(root, "test_fixture_names", "engine catalogue")?,
            "test_fixture_names",
        )?
        .iter()
        .enumerate()
        {
            let context = format!("test_fixture_names[{index}]");
            let name = string(value, &context)?.to_owned();
            if test_fixture_names.contains(&name) {
                return Err(GenError::new(format!(
                    "engine fixture name `{name}` appears twice"
                )));
            }
            test_fixture_names.push(name);
        }
        if test_fixture_names.len() != ENGINE_TEST_FIXTURE_NAME_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue has {} fixture names, expected {ENGINE_TEST_FIXTURE_NAME_COUNT}",
                test_fixture_names.len()
            )));
        }

        let all_names: BTreeSet<&String> = names
            .iter()
            .chain(retired.iter().map(|entry| &entry.name))
            .chain(failure_codes.iter())
            .chain(test_fixture_names.iter())
            .collect();
        if all_names.len() != ENGINE_NAME_COUNT {
            return Err(GenError::new(format!(
                "engine catalogue records {} distinct names, expected {ENGINE_NAME_COUNT}",
                all_names.len()
            )));
        }
        for family in ["inspect", "security", "state"] {
            let prefix = format!("sys.{family}.");
            let measured = all_names
                .iter()
                .filter(|name| name.starts_with(&prefix))
                .count();
            let expected = match family {
                "inspect" => 13,
                "security" => 13,
                _ => 2,
            };
            if measured != expected {
                return Err(GenError::new(format!(
                    "engine family sys.{family}.* has {measured} names, expected {expected}"
                )));
            }
        }

        Ok(Self {
            sealed,
            only,
            types,
            retired,
            failure_codes,
            test_fixture_names,
        })
    }

    fn sealed_names(&self) -> impl Iterator<Item = &str> {
        self.sealed
            .iter()
            .chain(self.only.iter())
            .map(|function| function.name.as_str())
    }
}

fn parse_optional_id(
    object: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<[u8; 16]>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(hex_id(string(value, context)?, context)?)),
    }
}

fn parse_signature(value: &Value, context: &str) -> Result<SignatureModel> {
    let signature = object(value, &format!("{context}.signature"))?;
    let kind = string(
        member(signature, "kind", &format!("{context}.signature"))?,
        "signature kind",
    )?;
    reject_unknown_fields(
        signature,
        &[
            "kind",
            "parameter_id",
            "parameter_name",
            "request_type",
            "request_required",
            "request_non_null",
            "event_stream_item_type",
            "parameter_count",
            "parameter_types",
            "result_type",
            "stream_item_type",
            "returns_set",
            "returns_ref_principal",
            "returns_boolean",
        ],
        &format!("{context}.signature"),
    )?;
    match kind {
        "health" => Ok(SignatureModel::Health),
        "source_introspection" => Ok(SignatureModel::SourceIntrospection),
        "invocation" => Ok(SignatureModel::Invocation {
            parameter_id: hex_id(
                string(member(signature, "parameter_id", context)?, context)?,
                context,
            )?,
            parameter_name: string(member(signature, "parameter_name", context)?, context)?
                .to_owned(),
            request_type: hex_id(
                string(member(signature, "request_type", context)?, context)?,
                context,
            )?,
            request_required: boolean(member(signature, "request_required", context)?, context)?,
            request_non_null: boolean(member(signature, "request_non_null", context)?, context)?,
            event_stream_item_type: hex_id(
                string(
                    member(signature, "event_stream_item_type", context)?,
                    context,
                )?,
                context,
            )?,
        }),
        "inspect" => {
            let parameter_count = count(member(signature, "parameter_count", context)?, context)?;
            let parameter_types = array(
                member(signature, "parameter_types", context)?,
                &format!("{context}.parameter_types"),
            )?
            .iter()
            .map(|value| hex_id(string(value, context)?, context))
            .collect::<Result<Vec<_>>>()?;
            if parameter_types.len() != parameter_count {
                return Err(GenError::new(format!(
                    "{context} inspect signature declares {parameter_count} parameters but lists {}",
                    parameter_types.len()
                )));
            }
            Ok(SignatureModel::Inspect {
                parameter_count,
                parameter_types,
                result_type: parse_optional_id(signature, "result_type", context)?,
                stream_item_type: parse_optional_id(signature, "stream_item_type", context)?,
            })
        }
        "security" => Ok(SignatureModel::Security {
            parameter_count: count(member(signature, "parameter_count", context)?, context)?,
            returns_set: boolean(member(signature, "returns_set", context)?, context)?,
            returns_ref_principal: boolean(
                member(signature, "returns_ref_principal", context)?,
                context,
            )?,
            returns_boolean: boolean(member(signature, "returns_boolean", context)?, context)?,
            stream_item_type: parse_optional_id(signature, "stream_item_type", context)?,
        }),
        "state" => Ok(SignatureModel::State {
            parameter_count: count(member(signature, "parameter_count", context)?, context)?,
            returns_set: boolean(member(signature, "returns_set", context)?, context)?,
        }),
        _ => Err(GenError::new(format!(
            "{context} has unknown sealed signature kind `{kind}`"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

const HEADER: &str = "\
// @generated by crates/orna-sys-id/build.rs from catalogue/sys-1.1.0.json and
// catalogue/engine-1.1.0.json. Do not edit; the catalogue bytes are pinned.
";

fn render_enum(out: &mut String, name: &str, variants: &[(&str, usize)]) {
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]"
    )
    .unwrap();
    writeln!(out, "#[repr({})]", repr_for(variants.len())).unwrap();
    writeln!(out, "pub enum {name} {{").unwrap();
    for (variant, index) in variants {
        writeln!(out, "    {variant} = {index},").unwrap();
    }
    writeln!(out, "}}").unwrap();
    out.push('\n');
}

fn render_functions(catalogue: &Catalogue) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    writeln!(
        out,
        "// Generated callable identity for the Orna 1.1.0 `sys` surface."
    )
    .unwrap();
    out.push('\n');
    writeln!(out, "use core::fmt;").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "use crate::{{descriptor::SysCallableDescriptor, Origin, SysEffect, UnknownSysMember}};"
    )
    .unwrap();
    out.push('\n');

    let variants: Vec<(&str, usize)> = catalogue
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| (function.variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysFunction", &variants);

    writeln!(
        out,
        "/// The number of callable identities fixed by `api/sys.json` 1.1.0."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_FUNCTION_COUNT: usize = {SYS_FUNCTION_COUNT};"
    )
    .unwrap();
    out.push('\n');

    writeln!(out, "impl SysFunction {{").unwrap();
    writeln!(
        out,
        "    /// Every catalogue callable, in `api/sys.json` order."
    )
    .unwrap();
    writeln!(
        out,
        "    pub const ALL: [SysFunction; SYS_FUNCTION_COUNT] = ["
    )
    .unwrap();
    for function in &catalogue.functions {
        writeln!(out, "        SysFunction::{},", function.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The dense discriminant used to index the descriptor table."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn index(self) -> usize {{").unwrap();
    writeln!(out, "        self as usize").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The exact `functions[].name`, overload label included."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn label(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for function in &catalogue.functions {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            function.variant, function.label
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The resolved path without the overload disambiguator."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn path(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for (path, variants) in &catalogue.function_paths {
        let patterns: Vec<String> = variants
            .iter()
            .map(|variant| format!("Self::{variant}"))
            .collect();
        writeln!(out, "            {} => \"{path}\",", patterns.join(" | ")).unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The overload disambiguator, or `None` for an unoverloaded callable."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub const fn overload(self) -> Option<&'static str> {{"
    )
    .unwrap();
    writeln!(out, "        match self {{").unwrap();
    for function in &catalogue.functions {
        if let Some(overload) = &function.overload {
            writeln!(
                out,
                "            Self::{} => Some(\"{}\"),",
                function.variant, overload
            )
            .unwrap();
        }
    }
    let unoverloaded: Vec<String> = catalogue
        .functions
        .iter()
        .filter(|function| function.overload.is_none())
        .map(|function| format!("Self::{}", function.variant))
        .collect();
    if !unoverloaded.is_empty() {
        writeln!(out, "            {} => None,", unoverloaded.join(" | ")).unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The catalogue origin; every entry here is portable."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn origin(self) -> Origin {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    let all: Vec<String> = catalogue
        .functions
        .iter()
        .map(|function| format!("Self::{}", function.variant))
        .collect();
    writeln!(out, "            {} => Origin::Portable,", all.join(" | ")).unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(out, "    /// The declared callable effect.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn effect(self) -> SysEffect {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for effect in [Effect::Read, Effect::Invoke, Effect::Admin] {
        let patterns: Vec<String> = catalogue
            .functions
            .iter()
            .filter(|function| function.effect == effect)
            .map(|function| format!("Self::{}", function.variant))
            .collect();
        if !patterns.is_empty() {
            writeln!(
                out,
                "            {} => SysEffect::{},",
                patterns.join(" | "),
                pascal(effect.as_str())
            )
            .unwrap();
        }
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The static descriptor, indexed by discriminant."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn descriptor(self) -> &'static SysCallableDescriptor {{"
    )
    .unwrap();
    writeln!(
        out,
        "        crate::descriptor::sys_function_descriptor(self)"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The exact, case-sensitive inverse of [`Self::label`]."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn from_label(label: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match label {{").unwrap();
    for function in &catalogue.functions {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            function.label, function.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// Name-only lookup; refuses a path with more than one callable."
    )
    .unwrap();
    writeln!(
        out,
        "    pub fn from_path(path: &str) -> Result<Self, crate::AmbiguousPath<'_>> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        let candidates: &'static [Self] = match path {{"
    )
    .unwrap();
    for (path, variants) in &catalogue.function_paths {
        let list: Vec<String> = variants
            .iter()
            .map(|variant| format!("Self::{variant}"))
            .collect();
        writeln!(out, "            \"{path}\" => &[{}],", list.join(", ")).unwrap();
    }
    writeln!(
        out,
        "            _ => return Err(crate::AmbiguousPath::Unknown(path)),"
    )
    .unwrap();
    writeln!(out, "        }};").unwrap();
    writeln!(out, "        match candidates {{").unwrap();
    writeln!(out, "            [only] => Ok(*only),").unwrap();
    writeln!(
        out,
        "            _ => Err(crate::AmbiguousPath::Overloaded {{ path, candidates }}),"
    )
    .unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');

    writeln!(out, "impl fmt::Display for SysFunction {{").unwrap();
    writeln!(
        out,
        "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{"
    )
    .unwrap();
    writeln!(out, "        formatter.write_str(self.label())").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl std::str::FromStr for SysFunction {{").unwrap();
    writeln!(out, "    type Err = UnknownSysMember;").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    fn from_str(text: &str) -> Result<Self, Self::Err> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        Self::from_label(text).ok_or_else(|| UnknownSysMember::new(\"sys callable\", text))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out
}

fn render_codes(catalogue: &Catalogue, engine: &Engine) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    writeln!(
        out,
        "// Generated failure-code vocabulary: 67 portable codes plus engine codes."
    )
    .unwrap();
    out.push('\n');
    writeln!(out, "use core::fmt;").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "use crate::{{descriptor::SysFailureCodeDescriptor, Origin, UnknownSysMember}};"
    )
    .unwrap();
    out.push('\n');

    let engine_codes: Vec<CodeModel> = engine
        .failure_codes
        .iter()
        .map(|code| CodeModel {
            variant: variant_from_code(code).expect("engine codes were validated"),
            code: code.clone(),
            origin: OriginKind::Engine,
        })
        .collect();
    let mut variants: Vec<(&str, usize)> = catalogue
        .portable_codes
        .iter()
        .enumerate()
        .map(|(index, code)| (code.variant.as_str(), index))
        .collect();
    for (offset, code) in engine_codes.iter().enumerate() {
        variants.push((
            code.variant.as_str(),
            catalogue.portable_codes.len() + offset,
        ));
    }
    let all_codes: Vec<&CodeModel> = catalogue
        .portable_codes
        .iter()
        .chain(engine_codes.iter())
        .collect();
    render_enum(&mut out, "SysFailureCode", &variants);

    writeln!(
        out,
        "/// The portable codes fixed by `api/sys.json`, in catalogue order."
    )
    .unwrap();
    writeln!(
        out,
        "pub const PORTABLE_FAILURE_CODE_COUNT: usize = {SYS_FAILURE_CODE_COUNT_PORTABLE};"
    )
    .unwrap();
    writeln!(
        out,
        "/// Every closed failure code: portable first, engine codes after."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_FAILURE_CODE_COUNT: usize = {};",
        all_codes.len()
    )
    .unwrap();
    out.push('\n');

    writeln!(out, "impl SysFailureCode {{").unwrap();
    writeln!(
        out,
        "    /// Every closed code, portable then engine, in dense order."
    )
    .unwrap();
    writeln!(out, "    pub const ALL: &'static [SysFailureCode] = &[").unwrap();
    for code in &all_codes {
        writeln!(out, "        SysFailureCode::{},", code.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// Exactly the `api/sys.json` `failure_codes` set."
    )
    .unwrap();
    writeln!(
        out,
        "    pub const PORTABLE: &'static [SysFailureCode] = &["
    )
    .unwrap();
    for code in &catalogue.portable_codes {
        writeln!(out, "        SysFailureCode::{},", code.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The dense discriminant used to index the descriptor table."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn index(self) -> usize {{").unwrap();
    writeln!(out, "        self as usize").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(out, "    /// The exact wire and storage bytes.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn as_str(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for code in &all_codes {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            code.variant, code.code
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// Whether the name is fixed by the specification or by this engine."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn origin(self) -> Origin {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    let portable: Vec<String> = catalogue
        .portable_codes
        .iter()
        .map(|code| format!("Self::{}", code.variant))
        .collect();
    writeln!(
        out,
        "            {} => Origin::Portable,",
        portable.join(" | ")
    )
    .unwrap();
    let engine_patterns: Vec<String> = engine_codes
        .iter()
        .map(|code| format!("Self::{}", code.variant))
        .collect();
    if !engine_patterns.is_empty() {
        writeln!(
            out,
            "            {} => Origin::Engine,",
            engine_patterns.join(" | ")
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// Whether this code is part of the portable specification set."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn is_portable(self) -> bool {{").unwrap();
    writeln!(out, "        matches!(self, {})", portable.join(" | ")).unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// Engine provider-ABI failures, replacing the `sys.abi.` prefix test."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn is_provider_abi(self) -> bool {{").unwrap();
    let abi: Vec<String> = all_codes
        .iter()
        .filter(|code| code.code.starts_with("sys.abi."))
        .map(|code| format!("Self::{}", code.variant))
        .collect();
    if abi.is_empty() {
        writeln!(out, "        false").unwrap();
    } else {
        writeln!(out, "        matches!(self, {})", abi.join(" | ")).unwrap();
    }
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(
        out,
        "    /// The static descriptor, indexed by discriminant."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn descriptor(self) -> &'static SysFailureCodeDescriptor {{"
    )
    .unwrap();
    writeln!(
        out,
        "        crate::descriptor::sys_failure_code_descriptor(self)"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');

    writeln!(out, "    /// The exact inverse of [`Self::as_str`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(code: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match code {{").unwrap();
    for code in &all_codes {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            code.code, code.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');

    writeln!(out, "impl fmt::Display for SysFailureCode {{").unwrap();
    writeln!(
        out,
        "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{"
    )
    .unwrap();
    writeln!(out, "        formatter.write_str(self.as_str())").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl std::str::FromStr for SysFailureCode {{").unwrap();
    writeln!(out, "    type Err = UnknownSysMember;").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    fn from_str(text: &str) -> Result<Self, Self::Err> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        Self::parse(text).ok_or_else(|| UnknownSysMember::new(\"sys failure code\", text))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out
}

fn render_relations(catalogue: &Catalogue, _engine: &Engine) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    writeln!(
        out,
        "// Generated relation, singleton, enum, type and field identity."
    )
    .unwrap();
    out.push('\n');
    writeln!(out, "use core::fmt;").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "use crate::{{descriptor::{{SysEnumDescriptor, SysRelationDescriptor, SysRelationField, SysSingletonDescriptor, SysTypeDescriptor}}, UnknownSysMember}};"
    )
    .unwrap();
    // SysEffect is not used here directly; keep the import list minimal below.
    out.push('\n');

    let relation_variants: Vec<(&str, usize)> = catalogue
        .relations
        .iter()
        .enumerate()
        .map(|(index, relation)| (relation.variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysRelation", &relation_variants);
    writeln!(out, "/// The `relations[]` count fixed by `api/sys.json`.").unwrap();
    writeln!(
        out,
        "pub const SYS_RELATION_COUNT: usize = {SYS_RELATION_COUNT};"
    )
    .unwrap();
    out.push('\n');

    let handle_variants: Vec<(&str, usize)> = catalogue
        .relations
        .iter()
        .enumerate()
        .map(|(index, relation)| (relation.handle_variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysGroupedHandle", &handle_variants);
    writeln!(
        out,
        "/// The `grouped_handle` count fixed by `api/sys.json`."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_GROUPED_HANDLE_COUNT: usize = {SYS_GROUPED_HANDLE_COUNT};"
    )
    .unwrap();
    out.push('\n');

    let singleton_variants: Vec<(&str, usize)> = catalogue
        .singletons
        .iter()
        .enumerate()
        .map(|(index, singleton)| (singleton.variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysSingleton", &singleton_variants);
    writeln!(out, "/// The `singletons[]` count fixed by `api/sys.json`.").unwrap();
    writeln!(
        out,
        "pub const SYS_SINGLETON_COUNT: usize = {SYS_SINGLETON_COUNT};"
    )
    .unwrap();
    out.push('\n');

    let enum_variants: Vec<(&str, usize)> = catalogue
        .enums
        .iter()
        .enumerate()
        .map(|(index, enumeration)| (enumeration.variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysEnum", &enum_variants);
    writeln!(out, "/// The `enums` count fixed by `api/sys.json`.").unwrap();
    writeln!(out, "pub const SYS_ENUM_COUNT: usize = {SYS_ENUM_COUNT};").unwrap();
    out.push('\n');

    let enum_values: Vec<(&EnumModel, &(String, String))> = catalogue
        .enums
        .iter()
        .flat_map(|enumeration| {
            enumeration
                .values
                .iter()
                .map(move |value| (enumeration, value))
        })
        .collect();
    let enum_value_variants: Vec<(&str, usize)> = enum_values
        .iter()
        .enumerate()
        .map(|(index, (_, (variant, _)))| (variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysEnumValue", &enum_value_variants);
    writeln!(
        out,
        "/// The total number of enum constants fixed by `api/sys.json`."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_ENUM_VALUE_COUNT: usize = {};",
        enum_values.len()
    )
    .unwrap();
    out.push('\n');

    let type_variants: Vec<(&str, usize)> = catalogue
        .types
        .iter()
        .enumerate()
        .map(|(index, system_type)| (system_type.variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysType", &type_variants);
    writeln!(
        out,
        "/// The value types, aliases and opaque identifiers fixed by `api/sys.json`."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_TYPE_COUNT: usize = {};",
        catalogue.types.len()
    )
    .unwrap();
    out.push('\n');

    let kinds = [
        (TypeKind::Opaque, 0usize),
        (TypeKind::OpaqueGeneric, 1),
        (TypeKind::Record, 2),
        (TypeKind::RecordGeneric, 3),
        (TypeKind::Alias, 4),
    ];
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]"
    )
    .unwrap();
    writeln!(out, "#[repr(u8)]").unwrap();
    writeln!(out, "pub enum SysTypeKind {{").unwrap();
    for (kind, index) in kinds {
        writeln!(out, "    {} = {index},", kind.variant()).unwrap();
    }
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl SysTypeKind {{").unwrap();
    writeln!(out, "    /// Every declared type kind.").unwrap();
    writeln!(out, "    pub const ALL: [SysTypeKind; 5] = [").unwrap();
    for (kind, _) in kinds {
        writeln!(out, "        SysTypeKind::{},", kind.variant()).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact catalogue spelling.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn as_str(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for (kind, _) in kinds {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            kind.variant(),
            kind.as_str()
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::as_str`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(text: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match text {{").unwrap();
    for (kind, _) in kinds {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            kind.as_str(),
            kind.variant()
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl fmt::Display for SysTypeKind {{").unwrap();
    writeln!(
        out,
        "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{"
    )
    .unwrap();
    writeln!(out, "        formatter.write_str(self.as_str())").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');

    let field_variants: Vec<(&str, usize)> = catalogue
        .fields
        .iter()
        .enumerate()
        .map(|(index, (variant, _))| (variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysField", &field_variants);
    writeln!(
        out,
        "/// The distinct field names declared by catalogue relations and value types."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_FIELD_COUNT: usize = {};",
        catalogue.fields.len()
    )
    .unwrap();
    out.push('\n');

    // Relation implementation.
    writeln!(out, "impl SysRelation {{").unwrap();
    writeln!(
        out,
        "    /// Every canonical relation, in `api/sys.json` order."
    )
    .unwrap();
    writeln!(
        out,
        "    pub const ALL: [SysRelation; SYS_RELATION_COUNT] = ["
    )
    .unwrap();
    for relation in &catalogue.relations {
        writeln!(out, "        SysRelation::{},", relation.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact `relations[].name`.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for relation in &catalogue.relations {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            relation.variant, relation.name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The grouped handle that identifies the same rows (ORNA-SYS-009)."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn handle(self) -> SysGroupedHandle {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for relation in &catalogue.relations {
        writeln!(
            out,
            "            Self::{} => SysGroupedHandle::{},",
            relation.variant, relation.handle_variant
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The static relation descriptor, indexed by discriminant."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn descriptor(self) -> &'static SysRelationDescriptor {{"
    )
    .unwrap();
    writeln!(
        out,
        "        crate::descriptor::sys_relation_descriptor(self)"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The declared fields in catalogue order.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn fields(self) -> &'static [SysRelationField] {{"
    )
    .unwrap();
    writeln!(out, "        self.descriptor().fields").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for relation in &catalogue.relations {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            relation.name, relation.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    push_display_from_str(&mut out, "SysRelation", "name", "sys relation");

    // Grouped-handle implementation.
    writeln!(out, "impl SysGroupedHandle {{").unwrap();
    writeln!(
        out,
        "    /// Every grouped handle, in `api/sys.json` relation order."
    )
    .unwrap();
    writeln!(
        out,
        "    pub const ALL: [SysGroupedHandle; SYS_GROUPED_HANDLE_COUNT] = ["
    )
    .unwrap();
    for relation in &catalogue.relations {
        writeln!(
            out,
            "        SysGroupedHandle::{},",
            relation.handle_variant
        )
        .unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact `relations[].grouped_handle`.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for relation in &catalogue.relations {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            relation.handle_variant, relation.handle
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for relation in &catalogue.relations {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            relation.handle, relation.handle_variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    push_display_from_str(&mut out, "SysGroupedHandle", "name", "sys grouped handle");

    // Singleton implementation.
    writeln!(out, "impl SysSingleton {{").unwrap();
    writeln!(out, "    /// Every singleton, in `api/sys.json` order.").unwrap();
    writeln!(
        out,
        "    pub const ALL: [SysSingleton; SYS_SINGLETON_COUNT] = ["
    )
    .unwrap();
    for singleton in &catalogue.singletons {
        writeln!(out, "        SysSingleton::{},", singleton.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact `singletons[].name`.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for singleton in &catalogue.singletons {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            singleton.variant, singleton.name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The static singleton descriptor, indexed by discriminant."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn descriptor(self) -> &'static SysSingletonDescriptor {{"
    )
    .unwrap();
    writeln!(
        out,
        "        crate::descriptor::sys_singleton_descriptor(self)"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The fields the singleton's view record declares."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn fields(self) -> &'static [SysField] {{").unwrap();
    writeln!(out, "        self.descriptor().fields").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for singleton in &catalogue.singletons {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            singleton.name, singleton.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    push_display_from_str(&mut out, "SysSingleton", "name", "sys singleton");

    // Enum implementation.
    writeln!(out, "impl SysEnum {{").unwrap();
    writeln!(out, "    /// Every enum type fixed by the catalogue.").unwrap();
    writeln!(out, "    pub const ALL: [SysEnum; SYS_ENUM_COUNT] = [").unwrap();
    for enumeration in &catalogue.enums {
        writeln!(out, "        SysEnum::{},", enumeration.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact enum name (`sys.RuntimeMode`).").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for enumeration in &catalogue.enums {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            enumeration.variant, enumeration.name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The static enum descriptor, indexed by discriminant."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn descriptor(self) -> &'static SysEnumDescriptor {{"
    )
    .unwrap();
    writeln!(out, "        crate::descriptor::sys_enum_descriptor(self)").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The declared constants in catalogue order.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn values(self) -> &'static [SysEnumValue] {{").unwrap();
    writeln!(out, "        self.descriptor().values").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for enumeration in &catalogue.enums {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            enumeration.name, enumeration.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    push_display_from_str(&mut out, "SysEnum", "name", "sys enum");

    // Flat enum-value implementation.
    writeln!(out, "impl SysEnumValue {{").unwrap();
    writeln!(
        out,
        "    /// Every enum constant, grouped by enum in catalogue order."
    )
    .unwrap();
    writeln!(
        out,
        "    pub const ALL: [SysEnumValue; SYS_ENUM_VALUE_COUNT] = ["
    )
    .unwrap();
    for (_, (variant, _)) in &enum_values {
        writeln!(out, "        SysEnumValue::{variant},").unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The enum that declares this constant.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn enum_(self) -> SysEnum {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for enumeration in &catalogue.enums {
        let patterns: Vec<String> = enumeration
            .values
            .iter()
            .map(|(variant, _)| format!("Self::{variant}"))
            .collect();
        writeln!(
            out,
            "            {} => SysEnum::{},",
            patterns.join(" | "),
            enumeration.variant
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact declared constant spelling.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn value(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for (_, (variant, value)) in &enum_values {
        writeln!(out, "            Self::{variant} => \"{value}\",").unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The `<enum name>.<value>` identity used by diagnostics."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn label(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for (enumeration, (variant, value)) in &enum_values {
        writeln!(
            out,
            "            Self::{variant} => \"{}.{value}\",",
            enumeration.name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::label`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(label: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match label {{").unwrap();
    for (enumeration, (variant, value)) in &enum_values {
        writeln!(
            out,
            "            \"{}.{value}\" => Some(Self::{variant}),",
            enumeration.name
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl fmt::Display for SysEnumValue {{").unwrap();
    writeln!(
        out,
        "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{"
    )
    .unwrap();
    writeln!(out, "        formatter.write_str(self.label())").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl std::str::FromStr for SysEnumValue {{").unwrap();
    writeln!(out, "    type Err = UnknownSysMember;").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    fn from_str(text: &str) -> Result<Self, Self::Err> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        Self::parse(text).ok_or_else(|| UnknownSysMember::new(\"sys enum constant\", text))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');

    // Type implementation.
    writeln!(out, "impl SysType {{").unwrap();
    writeln!(out, "    /// Every system type fixed by the catalogue.").unwrap();
    writeln!(out, "    pub const ALL: [SysType; SYS_TYPE_COUNT] = [").unwrap();
    for system_type in &catalogue.types {
        writeln!(out, "        SysType::{},", system_type.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The exact catalogue name, generic parameters included."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for system_type in &catalogue.types {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            system_type.variant, system_type.name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The declared kind.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn kind(self) -> SysTypeKind {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for system_type in &catalogue.types {
        writeln!(
            out,
            "            Self::{} => SysTypeKind::{},",
            system_type.variant,
            system_type.kind.variant()
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The alias target, when this name is a reference alias."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub const fn target(self) -> Option<&'static str> {{"
    )
    .unwrap();
    writeln!(out, "        match self {{").unwrap();
    for system_type in &catalogue.types {
        match &system_type.target {
            Some(target) => writeln!(
                out,
                "            Self::{} => Some(\"{target}\"),",
                system_type.variant
            )
            .unwrap(),
            None => writeln!(out, "            Self::{} => None,", system_type.variant).unwrap(),
        }
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The static type descriptor, indexed by discriminant."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(
        out,
        "    pub fn descriptor(self) -> &'static SysTypeDescriptor {{"
    )
    .unwrap();
    writeln!(out, "        crate::descriptor::sys_type_descriptor(self)").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for system_type in &catalogue.types {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            system_type.name, system_type.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    push_display_from_str(&mut out, "SysType", "name", "sys type");

    // Field implementation.
    writeln!(out, "impl SysField {{").unwrap();
    writeln!(out, "    /// Every distinct declared field name.").unwrap();
    writeln!(out, "    pub const ALL: [SysField; SYS_FIELD_COUNT] = [").unwrap();
    for (variant, _) in &catalogue.fields {
        writeln!(out, "        SysField::{variant},").unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact catalogue field spelling.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for (variant, field) in &catalogue.fields {
        writeln!(out, "            Self::{variant} => \"{field}\",").unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for (variant, field) in &catalogue.fields {
        writeln!(out, "            \"{field}\" => Some(Self::{variant}),").unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    push_display_from_str(&mut out, "SysField", "name", "sys field");

    out
}

fn push_display_from_str(out: &mut String, type_name: &str, accessor: &str, noun: &str) {
    writeln!(out, "impl fmt::Display for {type_name} {{").unwrap();
    writeln!(
        out,
        "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{"
    )
    .unwrap();
    writeln!(out, "        formatter.write_str(self.{accessor}())").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl std::str::FromStr for {type_name} {{").unwrap();
    writeln!(out, "    type Err = UnknownSysMember;").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    fn from_str(text: &str) -> Result<Self, Self::Err> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        Self::parse(text).ok_or_else(|| UnknownSysMember::new(\"{noun}\", text))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
}

fn render_removed(catalogue: &Catalogue) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    writeln!(
        out,
        "// Generated `removed_names` identity and ORNA rule codes."
    )
    .unwrap();
    out.push('\n');
    for rule in &catalogue.rules {
        writeln!(out, "/// The catalogue's `{}` diagnostic rule.", rule.text).unwrap();
        writeln!(
            out,
            "pub const {}: RuleCode = RuleCode::new(\"{}\");",
            rule.constant, rule.text
        )
        .unwrap();
        out.push('\n');
    }

    let variants: Vec<(&str, usize)> = catalogue
        .removed
        .iter()
        .enumerate()
        .map(|(index, removed)| (removed.variant.as_str(), index))
        .collect();
    render_enum(&mut out, "SysRemovedName", &variants);
    writeln!(
        out,
        "/// The `removed_names` count fixed by `api/sys.json`."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_REMOVED_NAME_COUNT: usize = {SYS_REMOVED_NAME_COUNT};"
    )
    .unwrap();
    out.push('\n');

    writeln!(out, "impl SysRemovedName {{").unwrap();
    writeln!(
        out,
        "    /// Every catalogue removal, in sorted name order."
    )
    .unwrap();
    writeln!(
        out,
        "    pub const ALL: [SysRemovedName; SYS_REMOVED_NAME_COUNT] = ["
    )
    .unwrap();
    for removed in &catalogue.removed {
        writeln!(out, "        SysRemovedName::{},", removed.variant).unwrap();
    }
    writeln!(out, "    ];").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact removed spelling.").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn name(self) -> &'static str {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for removed in &catalogue.removed {
        writeln!(
            out,
            "            Self::{} => \"{}\",",
            removed.variant, removed.name
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    /// The catalogue's replacement or retirement decision."
    )
    .unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub const fn removal(self) -> PortableRemoval {{").unwrap();
    writeln!(out, "        match self {{").unwrap();
    for (variant, removal) in &catalogue.removals {
        writeln!(
            out,
            "            Self::{variant} => {},",
            render_removal(removal)
        )
        .unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    out.push('\n');
    writeln!(out, "    /// The exact inverse of [`Self::name`].").unwrap();
    writeln!(out, "    #[must_use]").unwrap();
    writeln!(out, "    pub fn parse(name: &str) -> Option<Self> {{").unwrap();
    writeln!(out, "        match name {{").unwrap();
    for removed in &catalogue.removed {
        writeln!(
            out,
            "            \"{}\" => Some(Self::{}),",
            removed.name, removed.variant
        )
        .unwrap();
    }
    writeln!(out, "            _ => None,").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl fmt::Display for SysRemovedName {{").unwrap();
    writeln!(
        out,
        "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{"
    )
    .unwrap();
    writeln!(out, "        formatter.write_str(self.name())").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out.push('\n');
    writeln!(out, "impl std::str::FromStr for SysRemovedName {{").unwrap();
    writeln!(out, "    type Err = UnknownSysMember;").unwrap();
    out.push('\n');
    writeln!(
        out,
        "    fn from_str(text: &str) -> Result<Self, Self::Err> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        Self::parse(text).ok_or_else(|| UnknownSysMember::new(\"removed sys name\", text))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    out
}

fn render_removal(removal: &RemovalModel) -> String {
    match removal {
        RemovalModel::Replaced { by, diagnostic } => format!(
            "PortableRemoval::Replaced {{ by: {}, diagnostic: {} }}",
            render_member_path(by),
            render_diagnostic(diagnostic)
        ),
        RemovalModel::Retired { diagnostic, reason } => format!(
            "PortableRemoval::Retired {{ diagnostic: {}, reason: \"{}\" }}",
            render_diagnostic(diagnostic),
            reason
        ),
    }
}

fn render_member_path(path: &MemberPathModel) -> String {
    match path {
        MemberPathModel::Callable(variant) => {
            format!("SysMemberPath::Callable(SysFunction::{variant})")
        }
        MemberPathModel::Relation(variant) => {
            format!("SysMemberPath::Relation(SysRelation::{variant})")
        }
        MemberPathModel::GroupedHandle(variant) => {
            format!("SysMemberPath::GroupedHandle(SysGroupedHandle::{variant})")
        }
        MemberPathModel::Singleton(variant) => {
            format!("SysMemberPath::Singleton(SysSingleton::{variant})")
        }
        MemberPathModel::SingletonField(singleton, field) => {
            format!("SysMemberPath::SingletonField(SysSingleton::{singleton}, SysField::{field})")
        }
    }
}

fn render_diagnostic(diagnostic: &DiagnosticModel) -> String {
    match diagnostic {
        DiagnosticModel::Rule(text) => {
            format!("DiagnosticIdentity::Rule({})", text.replace('-', "_"))
        }
        DiagnosticModel::Failure(variant) => {
            format!("DiagnosticIdentity::Failure(SysFailureCode::{variant})")
        }
    }
}

fn render_tables(catalogue: &Catalogue, engine: &Engine) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    writeln!(
        out,
        "// Generated static descriptor tables, indexed by enum discriminant."
    )
    .unwrap();
    out.push('\n');
    out.push('\n');

    writeln!(
        out,
        "/// The `api/sys.json` language version the tables were generated from."
    )
    .unwrap();
    writeln!(
        out,
        "pub const SYS_API_LANGUAGE_VERSION: &str = \"{}\";",
        catalogue.language_version
    )
    .unwrap();
    writeln!(out, "/// The `api/sys.json` `sys_version` coordinate.").unwrap();
    writeln!(
        out,
        "pub const SYS_API_SYS_VERSION: &str = \"{}\";",
        catalogue.sys_version
    )
    .unwrap();
    out.push('\n');

    writeln!(
        out,
        "pub(crate) static SYS_FUNCTION_DESCRIPTORS: [SysCallableDescriptor; SYS_FUNCTION_COUNT] = ["
    )
    .unwrap();
    for function in &catalogue.functions {
        writeln!(out, "    SysCallableDescriptor {{").unwrap();
        writeln!(out, "        function: SysFunction::{},", function.variant).unwrap();
        writeln!(out, "        label: \"{}\",", function.label).unwrap();
        writeln!(out, "        path: \"{}\",", function.path).unwrap();
        match &function.overload {
            Some(overload) => writeln!(out, "        overload: Some(\"{overload}\"),").unwrap(),
            None => writeln!(out, "        overload: None,").unwrap(),
        }
        writeln!(out, "        signature: {:?},", function.signature).unwrap();
        writeln!(
            out,
            "        effect: SysEffect::{},",
            pascal(function.effect.as_str())
        )
        .unwrap();
        writeln!(out, "        origin: Origin::Portable,").unwrap();
        writeln!(out, "        purpose: {:?},", function.purpose).unwrap();
        writeln!(
            out,
            "        snapshot_rule: {},",
            render_optional(function.snapshot_rule.as_deref())
        )
        .unwrap();
        writeln!(
            out,
            "        ownership: {},",
            render_optional(function.ownership.as_deref())
        )
        .unwrap();
        writeln!(
            out,
            "        contract: {},",
            render_optional(function.contract.as_deref())
        )
        .unwrap();
        writeln!(
            out,
            "        preconditions: {},",
            render_optional(function.preconditions.as_deref())
        )
        .unwrap();
        writeln!(
            out,
            "        since: {},",
            render_optional(function.since.as_deref())
        )
        .unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    let mut all_codes: Vec<(&str, &str, OriginKind)> = catalogue
        .portable_codes
        .iter()
        .map(|code| (code.variant.as_str(), code.code.as_str(), code.origin))
        .collect();
    let engine_code_variants: Vec<(String, String)> = engine
        .failure_codes
        .iter()
        .map(|code| {
            (
                variant_from_code(code).expect("engine code validated"),
                code.clone(),
            )
        })
        .collect();
    all_codes.extend(
        engine_code_variants
            .iter()
            .map(|(variant, code)| (variant.as_str(), code.as_str(), OriginKind::Engine)),
    );
    writeln!(
        out,
        "pub(crate) static SYS_FAILURE_CODE_DESCRIPTORS: [SysFailureCodeDescriptor; SYS_FAILURE_CODE_COUNT] = ["
    )
    .unwrap();
    for (variant, code, origin) in all_codes {
        writeln!(
            out,
            "    SysFailureCodeDescriptor {{ code: SysFailureCode::{variant}, bytes: \"{code}\", origin: {} }},",
            origin.variant()
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "pub(crate) static SYS_RELATION_DESCRIPTORS: [SysRelationDescriptor; SYS_RELATION_COUNT] = ["
    )
    .unwrap();
    for relation in &catalogue.relations {
        writeln!(out, "    SysRelationDescriptor {{").unwrap();
        writeln!(out, "        relation: SysRelation::{},", relation.variant).unwrap();
        writeln!(out, "        name: \"{}\",", relation.name).unwrap();
        writeln!(
            out,
            "        grouped_handle: SysGroupedHandle::{},",
            relation.handle_variant
        )
        .unwrap();
        writeln!(out, "        kind: \"{}\",", relation.kind).unwrap();
        writeln!(out, "        availability: \"{}\",", relation.availability).unwrap();
        writeln!(out, "        key: \"{}\",", relation.key).unwrap();
        writeln!(
            out,
            "        reference_type: \"{}\",",
            relation.reference_type
        )
        .unwrap();
        writeln!(out, "        purpose: {:?},", relation.purpose).unwrap();
        writeln!(out, "        writable: {},", relation.writable).unwrap();
        writeln!(out, "        fields: &[").unwrap();
        for (field, type_name) in &relation.fields {
            writeln!(
                out,
                "            SysRelationField {{ field: SysField::{field}, type_name: {:?} }},",
                type_name
            )
            .unwrap();
        }
        writeln!(out, "        ],").unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "pub(crate) static SYS_SINGLETON_DESCRIPTORS: [SysSingletonDescriptor; SYS_SINGLETON_COUNT] = ["
    )
    .unwrap();
    for singleton in &catalogue.singletons {
        writeln!(out, "    SysSingletonDescriptor {{").unwrap();
        writeln!(
            out,
            "        singleton: SysSingleton::{},",
            singleton.variant
        )
        .unwrap();
        writeln!(out, "        name: \"{}\",", singleton.name).unwrap();
        writeln!(out, "        view_type: \"{}\",", singleton.view_type).unwrap();
        writeln!(out, "        availability: {:?},", singleton.availability).unwrap();
        writeln!(out, "        fields: &[").unwrap();
        for field in &singleton.fields {
            writeln!(out, "            SysField::{field},").unwrap();
        }
        writeln!(out, "        ],").unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "pub(crate) static SYS_ENUM_DESCRIPTORS: [SysEnumDescriptor; SYS_ENUM_COUNT] = ["
    )
    .unwrap();
    for enumeration in &catalogue.enums {
        writeln!(out, "    SysEnumDescriptor {{").unwrap();
        writeln!(
            out,
            "        enumeration: SysEnum::{},",
            enumeration.variant
        )
        .unwrap();
        writeln!(out, "        name: \"{}\",", enumeration.name).unwrap();
        writeln!(out, "        values: &[").unwrap();
        for (variant, _) in &enumeration.values {
            writeln!(out, "            SysEnumValue::{variant},").unwrap();
        }
        writeln!(out, "        ],").unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "pub(crate) static SYS_TYPE_DESCRIPTORS: [SysTypeDescriptor; SYS_TYPE_COUNT] = ["
    )
    .unwrap();
    for system_type in &catalogue.types {
        writeln!(out, "    SysTypeDescriptor {{").unwrap();
        writeln!(
            out,
            "        system_type: SysType::{},",
            system_type.variant
        )
        .unwrap();
        writeln!(out, "        name: \"{}\",", system_type.name).unwrap();
        writeln!(
            out,
            "        kind: SysTypeKind::{},",
            system_type.kind.variant()
        )
        .unwrap();
        writeln!(
            out,
            "        target: {},",
            render_optional(system_type.target.as_deref())
        )
        .unwrap();
        writeln!(
            out,
            "        purpose: {},",
            render_optional(system_type.purpose.as_deref())
        )
        .unwrap();
        writeln!(out, "        type_parameters: &[").unwrap();
        for parameter in &system_type.type_parameters {
            writeln!(out, "            \"{parameter}\",").unwrap();
        }
        writeln!(out, "        ],").unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "/// The sealed engine functions followed by the two engine-only functions."
    )
    .unwrap();
    writeln!(
        out,
        "pub static ENGINE_FUNCTIONS: [EngineFunctionDescriptor; {}] = [",
        engine.sealed.len() + engine.only.len()
    )
    .unwrap();
    for function in engine.sealed.iter().chain(engine.only.iter()) {
        writeln!(out, "    EngineFunctionDescriptor {{").unwrap();
        writeln!(out, "        name: \"{}\",", function.name).unwrap();
        writeln!(out, "        name_parts: &[").unwrap();
        for part in &function.name_parts {
            writeln!(out, "            \"{part}\",").unwrap();
        }
        writeln!(out, "        ],").unwrap();
        match function.function_id {
            Some(id) => writeln!(out, "        function_id: Some({}),", render_id(id)).unwrap(),
            None => writeln!(out, "        function_id: None,").unwrap(),
        }
        writeln!(out, "        sealed: {},", function.sealed).unwrap();
        match &function.signature {
            Some(signature) => {
                writeln!(
                    out,
                    "        signature: Some({}),",
                    render_signature(signature)
                )
                .unwrap();
            }
            None => writeln!(out, "        signature: None,").unwrap(),
        }
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "/// The engine-only type carriers the specification does not contain."
    )
    .unwrap();
    writeln!(
        out,
        "pub static ENGINE_TYPES: [EngineTypeDescriptor; ENGINE_ONLY_TYPE_COUNT] = ["
    )
    .unwrap();
    for system_type in &engine.types {
        writeln!(out, "    EngineTypeDescriptor {{").unwrap();
        writeln!(out, "        name: \"{}\",", system_type.name).unwrap();
        writeln!(out, "        type_id: {},", render_id(system_type.type_id)).unwrap();
        writeln!(
            out,
            "        representation_contract: \"{}\",",
            system_type.representation_contract
        )
        .unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "/// The 1.0.0 function names retained for historical readers."
    )
    .unwrap();
    writeln!(
        out,
        "pub static RETIRED_ENGINE_FUNCTIONS: [RetiredEngineFunction; ENGINE_RETIRED_FUNCTION_COUNT] = ["
    )
    .unwrap();
    for function in &engine.retired {
        writeln!(out, "    RetiredEngineFunction {{").unwrap();
        writeln!(out, "        name: \"{}\",", function.name).unwrap();
        writeln!(out, "        since: \"{}\",", function.since).unwrap();
        writeln!(out, "        signature: {:?},", function.signature).unwrap();
        writeln!(out, "        reason: {:?},", function.reason).unwrap();
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    out.push('\n');

    writeln!(
        out,
        "/// Test-fixture names that are not part of the production vocabulary."
    )
    .unwrap();
    writeln!(
        out,
        "pub static ENGINE_TEST_FIXTURE_NAMES: [&str; ENGINE_TEST_FIXTURE_NAME_COUNT] = ["
    )
    .unwrap();
    for name in &engine.test_fixture_names {
        writeln!(out, "    \"{name}\",").unwrap();
    }
    writeln!(out, "];").unwrap();
    out
}

fn render_optional(value: Option<&str>) -> String {
    match value {
        Some(text) => format!("Some({text:?})"),
        None => "None".to_owned(),
    }
}

fn render_signature(signature: &SignatureModel) -> String {
    match signature {
        SignatureModel::Health => "EngineSignature::Health".to_owned(),
        SignatureModel::SourceIntrospection => "EngineSignature::SourceIntrospection".to_owned(),
        SignatureModel::Invocation {
            parameter_id,
            parameter_name,
            request_type,
            request_required,
            request_non_null,
            event_stream_item_type,
        } => format!(
            "EngineSignature::Invocation {{ parameter_id: {}, parameter_name: {:?}, request_type: {}, request_required: {request_required}, request_non_null: {request_non_null}, event_stream_item_type: {} }}",
            render_id(*parameter_id),
            parameter_name,
            render_id(*request_type),
            render_id(*event_stream_item_type)
        ),
        SignatureModel::Inspect {
            parameter_count,
            parameter_types,
            result_type,
            stream_item_type,
        } => {
            let types: Vec<String> = parameter_types.iter().map(|id| render_id(*id)).collect();
            format!(
                "EngineSignature::Inspect {{ parameter_count: {parameter_count}, parameter_types: &[{}], result_type: {}, stream_item_type: {} }}",
                types.join(", "),
                render_optional_id(*result_type),
                render_optional_id(*stream_item_type)
            )
        }
        SignatureModel::Security {
            parameter_count,
            returns_set,
            returns_ref_principal,
            returns_boolean,
            stream_item_type,
        } => format!(
            "EngineSignature::Security {{ parameter_count: {parameter_count}, returns_set: {returns_set}, returns_ref_principal: {returns_ref_principal}, returns_boolean: {returns_boolean}, stream_item_type: {} }}",
            render_optional_id(*stream_item_type)
        ),
        SignatureModel::State {
            parameter_count,
            returns_set,
        } => format!(
            "EngineSignature::State {{ parameter_count: {parameter_count}, returns_set: {returns_set} }}"
        ),
    }
}

fn render_optional_id(value: Option<[u8; 16]>) -> String {
    match value {
        Some(id) => format!("Some({})", render_id(id)),
        None => "None".to_owned(),
    }
}
