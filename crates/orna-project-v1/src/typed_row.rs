//! Type-directed lowering of row values to the ROV-3 stored-field tuple of a
//! closed storage descriptor (ORNA-FORMAT-004; 29-formats.md).
//!
//! Rows reach storage from two producers that do not know the stored
//! encoding of their fields: the evaluator (REPL and source writes), whose
//! optional absence is a bare `null` and whose records are field-name maps,
//! and source row documents lowered without a schema by
//! `orna_semantic_v1::lower_row_unit`, which spell an enum value as its
//! variant name and a stored reference as the target key. The descriptor
//! carries every field's type node, so it directs the lowering:
//!
//! - optional absence becomes `Option` none and a present value is wrapped;
//! - a record literal becomes the record value of its structural or nominal
//!   type, with nominal fields keyed by their field ObjectIds;
//! - a variant name, or a variant constructor with its payload record,
//!   becomes the enum value of its declared type;
//! - a refined value is tagged with its refined type around the base value;
//! - a key becomes a stored reference to the declared database and table
//!   (ORNA-REF-001);
//! - an exact numeral becomes the money or quantity value of its declared
//!   currency or unit, never through an intermediate `Float` (ORNA-LIT-003);
//! - a decimal numeral checked against `Float` is rounded once to binary64
//!   (ORNA-LIT-002).
//!
//! Every lowering accepts its own output, so a stored row read back and
//! patched lowers to the same bytes. A value whose shape its type cannot
//! accept is refused; nothing is guessed. The caller still validates the
//! encoded tuple with [`SchemaDescriptor::validate_stored_row`].

use std::collections::BTreeMap;

use num_bigint::BigInt;
use orna_repository_v1::Uuid;
use orna_value_v1::{Decimal, Raw, SchemaDescriptor};

const UUID_TAG: u64 = 37;
const DECIMAL_TAG: u64 = 60_000;
const QUANTITY_TAG: u64 = 60_006;
const MONEY_TAG: u64 = 60_007;
const ENUM_TAG: u64 = 60_008;
const RECORD_TAG: u64 = 60_009;
const OPTION_TAG: u64 = 60_013;
const UNIT_TAG: u64 = 60_014;
const TUPLE_TAG: u64 = 60_015;
const RANGE_TAG: u64 = 60_019;
const REFINED_TAG: u64 = 60_020;
const STORED_REFERENCE_TAG: u64 = 60_021;
const STORED_BLOB_TAG: u64 = 60_111;

const RECORD_DEFINITION: u64 = 0;
const ENUM_DEFINITION: u64 = 1;
const REFINED_DEFINITION: u64 = 2;

/// Nesting bound for one lowered value; deeper values are refused rather
/// than recursed into.
const MAX_DEPTH: usize = 128;

/// Why a row could not be lowered to its stored-field tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypedRowError {
    /// The descriptor is not a closed storage descriptor.
    InvalidDescriptor,
    /// A stored field has no value.
    MissingField(String),
    /// The row supplies a field the table does not store, such as a computed
    /// field (ORNA-FIELD-006).
    UnknownField(String),
    /// A field's value does not inhabit the field's declared type.
    TypeMismatch(String),
}

impl std::fmt::Display for TypedRowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDescriptor => formatter.write_str("the table descriptor is not closed"),
            Self::MissingField(field) => write!(formatter, "stored field `{field}` has no value"),
            Self::UnknownField(field) => {
                write!(formatter, "`{field}` is not a stored field of the table")
            }
            Self::TypeMismatch(field) => {
                write!(
                    formatter,
                    "the value of `{field}` does not have its declared type"
                )
            }
        }
    }
}

impl std::error::Error for TypedRowError {}

/// One field descriptor `[field_id, name, type_node, role, fallback]`.
#[derive(Clone, Debug)]
pub struct DescriptorField<'a> {
    id: [u8; 16],
    name: &'a str,
    type_node: &'a Raw,
    role: u64,
}

impl<'a> DescriptorField<'a> {
    pub const fn id(&self) -> &[u8; 16] {
        &self.id
    }

    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub const fn type_node(&self) -> &'a Raw {
        self.type_node
    }

    /// 0 for a key field, 1 for a stored field, 2 for a computed field.
    pub const fn role(&self) -> u64 {
        self.role
    }

    /// Whether the field is a `Blob` or `Blob?`, whose stored value is a
    /// repository-issued reference rather than a lowered literal.
    pub fn is_blob(&self) -> bool {
        let node = match array(self.type_node) {
            Some([Raw::Int(code), inner]) if *code == BigInt::from(2) => inner,
            _ => self.type_node,
        };
        matches!(
            array(node),
            Some([Raw::Int(code), Raw::Text(name)]) if *code == BigInt::from(0) && name == "Blob"
        )
    }
}

/// The fields and nominal definitions of one closed storage descriptor.
#[derive(Clone, Debug)]
pub struct StoredSchema<'a> {
    fields: Vec<DescriptorField<'a>>,
    definitions: &'a [Raw],
}

impl<'a> StoredSchema<'a> {
    pub fn new(descriptor: &'a SchemaDescriptor) -> Result<Self, TypedRowError> {
        let Raw::Map(entries) = descriptor.raw() else {
            return Err(TypedRowError::InvalidDescriptor);
        };
        let entry = |number: u8| {
            entries
                .iter()
                .find(|(key, _)| *key == Raw::Int(number.into()))
                .map(|(_, value)| value)
                .ok_or(TypedRowError::InvalidDescriptor)
        };
        let Raw::Array(declared) = entry(3)? else {
            return Err(TypedRowError::InvalidDescriptor);
        };
        let Raw::Array(definitions) = entry(4)? else {
            return Err(TypedRowError::InvalidDescriptor);
        };
        let fields = declared
            .iter()
            .map(|field| match array(field) {
                Some([id, Raw::Text(name), type_node, Raw::Int(role), _]) => Ok(DescriptorField {
                    id: uuid(id).ok_or(TypedRowError::InvalidDescriptor)?,
                    name,
                    type_node,
                    role: u64::try_from(role).map_err(|_| TypedRowError::InvalidDescriptor)?,
                }),
                _ => Err(TypedRowError::InvalidDescriptor),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            fields,
            definitions,
        })
    }

    /// Every field in descriptor (field-ObjectId) order.
    pub fn fields(&self) -> &[DescriptorField<'a>] {
        &self.fields
    }

    /// The field named `name`, of any role.
    pub fn field(&self, name: &str) -> Option<&DescriptorField<'a>> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Lowers a complete row, given as field name to value, to its stored
    /// fields in descriptor order. Key fields may be present and are not
    /// stored here; every stored field must be present, and no other field
    /// may be supplied.
    pub fn lower_stored_fields(
        &self,
        values: &BTreeMap<&str, &Raw>,
    ) -> Result<Vec<Raw>, TypedRowError> {
        if let Some(unknown) = values.keys().find(|name| {
            !self
                .fields
                .iter()
                .any(|field| field.name == **name && field.role != 2)
        }) {
            return Err(TypedRowError::UnknownField((*unknown).to_owned()));
        }
        self.fields
            .iter()
            .filter(|field| field.role == 1)
            .map(|field| {
                let value = values
                    .get(field.name)
                    .ok_or_else(|| TypedRowError::MissingField(field.name.to_owned()))?;
                self.lower_field(field, value)
            })
            .collect()
    }

    /// Lowers one value of `field` to its stored representation.
    pub fn lower_field(
        &self,
        field: &DescriptorField<'_>,
        value: &Raw,
    ) -> Result<Raw, TypedRowError> {
        self.lower(field.type_node, value, 0)
            .map_err(|failure| failure.for_field(field.name))
    }

    fn lower(&self, type_node: &Raw, value: &Raw, depth: usize) -> Result<Raw, Failure> {
        if depth > MAX_DEPTH {
            return Err(Failure::Mismatch);
        }
        let node = array(type_node).ok_or(Failure::Descriptor)?;
        let code = node
            .first()
            .and_then(small_int)
            .ok_or(Failure::Descriptor)?;
        match (code, node) {
            (0, [_, Raw::Text(primitive)]) => lower_primitive(primitive, value),
            (1, [_, element]) => {
                let Raw::Array(items) = value else {
                    return Err(Failure::Mismatch);
                };
                items
                    .iter()
                    .map(|item| self.lower(element, item, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Raw::Array)
            }
            (2, [_, inner]) => self.lower_option(inner, value, depth),
            (3, [_, Raw::Array(components)]) => self.lower_tuple(components, value, depth),
            (4, [_, Raw::Array(fields)]) => self.lower_structural_record(fields, value, depth),
            (5, [_, id]) => self.lower_nominal(id, value, depth),
            (6, [_, database, table, Raw::Array(key_types)]) => {
                self.lower_reference(database, table, key_types, value, depth)
            }
            (7, [_, number_type, unit]) => {
                let number = match tagged(value, QUANTITY_TAG) {
                    Some([number, stored_unit]) if stored_unit == unit => number,
                    Some(_) => return Err(Failure::Mismatch),
                    None => value,
                };
                Ok(tag(
                    QUANTITY_TAG,
                    vec![self.lower(number_type, number, depth + 1)?, unit.clone()],
                ))
            }
            (8, [_, currency]) => {
                let amount = match tagged(value, MONEY_TAG) {
                    Some([amount, stored_currency]) if stored_currency == currency => amount,
                    Some(_) => return Err(Failure::Mismatch),
                    None => value,
                };
                Ok(tag(
                    MONEY_TAG,
                    vec![exact_decimal(amount)?, currency.clone()],
                ))
            }
            (9, [_, endpoint]) => {
                let Some([lower, upper, inclusive @ Raw::Bool(_)]) = tagged(value, RANGE_TAG)
                else {
                    return Err(Failure::Mismatch);
                };
                Ok(tag(
                    RANGE_TAG,
                    vec![
                        self.lower_option(endpoint, lower, depth)?,
                        self.lower_option(endpoint, upper, depth)?,
                        inclusive.clone(),
                    ],
                ))
            }
            _ => Err(Failure::Descriptor),
        }
    }

    fn lower_option(&self, inner: &Raw, value: &Raw, depth: usize) -> Result<Raw, Failure> {
        match value {
            Raw::Null => Ok(option(None)),
            Raw::Tag(OPTION_TAG, _) => match tagged(value, OPTION_TAG) {
                Some([Raw::Int(state)]) if *state == BigInt::from(0) => Ok(option(None)),
                Some([Raw::Int(state), present]) if *state == BigInt::from(1) => {
                    Ok(option(Some(self.lower(inner, present, depth + 1)?)))
                }
                _ => Err(Failure::Mismatch),
            },
            present => Ok(option(Some(self.lower(inner, present, depth + 1)?))),
        }
    }

    fn lower_tuple(&self, components: &[Raw], value: &Raw, depth: usize) -> Result<Raw, Failure> {
        let items = match value {
            Raw::Array(items) => items.as_slice(),
            Raw::Tag(UNIT_TAG, payload) if components.is_empty() => {
                array(payload).ok_or(Failure::Mismatch)?
            }
            Raw::Tag(TUPLE_TAG, payload) if !components.is_empty() => {
                array(payload).ok_or(Failure::Mismatch)?
            }
            _ => return Err(Failure::Mismatch),
        };
        if items.len() != components.len() {
            return Err(Failure::Mismatch);
        }
        if components.is_empty() {
            return Ok(tag(UNIT_TAG, Vec::new()));
        }
        Ok(tag(
            TUPLE_TAG,
            components
                .iter()
                .zip(items)
                .map(|(component, item)| self.lower(component, item, depth + 1))
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    /// `[4, [[name, type]...]]`: the stored record is `[null, [[name, value]...]]`.
    fn lower_structural_record(
        &self,
        fields: &[Raw],
        value: &Raw,
        depth: usize,
    ) -> Result<Raw, Failure> {
        let mut entries = record_entries(value, None)?;
        let mut stored = Vec::with_capacity(fields.len());
        for field in fields {
            let Some([name @ Raw::Text(text), field_type]) = array(field) else {
                return Err(Failure::Descriptor);
            };
            let value = entries.remove(text.as_str()).ok_or(Failure::Mismatch)?;
            stored.push(Raw::Array(vec![
                name.clone(),
                self.lower(field_type, value, depth + 1)?,
            ]));
        }
        if !entries.is_empty() {
            return Err(Failure::Mismatch);
        }
        Ok(tag(RECORD_TAG, vec![Raw::Null, Raw::Array(canonical_record_fields(stored)?)]))
    }

    fn lower_nominal(&self, id: &Raw, value: &Raw, depth: usize) -> Result<Raw, Failure> {
        let type_id = uuid(id).ok_or(Failure::Descriptor)?;
        let definition = self
            .definitions
            .iter()
            .find_map(|definition| match array(definition) {
                Some([candidate, kind, body]) if uuid(candidate) == Some(type_id) => {
                    Some((small_int(kind)?, body))
                }
                _ => None,
            })
            .ok_or(Failure::Descriptor)?;
        match definition {
            (RECORD_DEFINITION, Raw::Array(members)) => {
                if let Some([stored_type, _]) = tagged(value, RECORD_TAG) {
                    if !owner_matches(stored_type, id) {
                        return Err(Failure::Mismatch);
                    }
                }
                self.lower_member_record(id, members, value, depth)
            }
            (ENUM_DEFINITION, Raw::Array(variants)) => self.lower_enum(id, variants, value, depth),
            (REFINED_DEFINITION, Raw::Array(body)) => {
                let base_type = body.first().ok_or(Failure::Descriptor)?;
                let base = match tagged(value, REFINED_TAG) {
                    Some([stored_type, base]) if uuid(stored_type) == Some(type_id) => base,
                    Some(_) => return Err(Failure::Mismatch),
                    None => value,
                };
                Ok(tag(
                    REFINED_TAG,
                    vec![id.clone(), self.lower(base_type, base, depth + 1)?],
                ))
            }
            _ => Err(Failure::Mismatch),
        }
    }

    /// A nominal record or a variant payload: `[owner_id, [[field_id, value]...]]`
    /// in member-descriptor order. Entries are matched by member name or by
    /// member ObjectId.
    fn lower_member_record(
        &self,
        owner: &Raw,
        members: &[Raw],
        value: &Raw,
        depth: usize,
    ) -> Result<Raw, Failure> {
        let mut entries = record_entries(value, Some(members))?;
        let mut stored = Vec::with_capacity(members.len());
        for member in members {
            let Some([member_id, Raw::Text(name), member_type, ..]) = array(member) else {
                return Err(Failure::Descriptor);
            };
            let value = entries.remove(name.as_str()).ok_or(Failure::Mismatch)?;
            stored.push(Raw::Array(vec![
                member_id.clone(),
                self.lower(member_type, value, depth + 1)?,
            ]));
        }
        if !entries.is_empty() {
            return Err(Failure::Mismatch);
        }
        Ok(tag(RECORD_TAG, vec![owner.clone(), Raw::Array(canonical_record_fields(stored)?)]))
    }

    fn lower_enum(
        &self,
        id: &Raw,
        variants: &[Raw],
        value: &Raw,
        depth: usize,
    ) -> Result<Raw, Failure> {
        let variant_named = |name: &str| {
            variants.iter().find_map(|variant| match array(variant) {
                Some([variant_id, Raw::Text(variant_name), Raw::Array(fields)])
                    if variant_name == name =>
                {
                    Some((variant_id, fields.as_slice()))
                }
                _ => None,
            })
        };
        let variant_with_id = |stored: &Raw| {
            variants.iter().find_map(|variant| match array(variant) {
                Some([variant_id, _, Raw::Array(fields)]) if variant_id == stored => {
                    Some((variant_id, fields.as_slice()))
                }
                _ => None,
            })
        };
        let (variant, payload) = match value {
            // A row document spells a variant by name, possibly qualified by
            // its type: `red` or `Color.red`.
            Raw::Text(name) => {
                let leaf = name.rsplit('.').next().unwrap_or(name);
                (variant_named(leaf).ok_or(Failure::Mismatch)?, &Raw::Null)
            }
            Raw::Tag(ENUM_TAG, _) => {
                let Some([stored_type, stored_variant, payload]) = tagged(value, ENUM_TAG) else {
                    return Err(Failure::Mismatch);
                };
                if !owner_matches(stored_type, id) {
                    return Err(Failure::Mismatch);
                }
                let variant = match stored_variant {
                    Raw::Text(name) => variant_named(name),
                    other => variant_with_id(other),
                }
                .ok_or(Failure::Mismatch)?;
                (variant, payload)
            }
            // A variant constructor with its payload record: `circle { radius: 1 }`.
            Raw::Tag(RECORD_TAG, _) => {
                let Some([Raw::Text(name), _]) = tagged(value, RECORD_TAG) else {
                    return Err(Failure::Mismatch);
                };
                let leaf = name.rsplit('.').next().unwrap_or(name);
                (variant_named(leaf).ok_or(Failure::Mismatch)?, value)
            }
            _ => return Err(Failure::Mismatch),
        };
        let (variant_id, fields) = variant;
        let payload = if fields.is_empty() {
            if !matches!(payload, Raw::Null) {
                return Err(Failure::Mismatch);
            }
            Raw::Null
        } else {
            if let Some([stored_owner, _]) = tagged(payload, RECORD_TAG) {
                if !owner_matches(stored_owner, variant_id) {
                    return Err(Failure::Mismatch);
                }
            }
            self.lower_member_record(variant_id, fields, payload, depth)?
        };
        Ok(tag(ENUM_TAG, vec![id.clone(), variant_id.clone(), payload]))
    }

    fn lower_reference(
        &self,
        database: &Raw,
        table: &Raw,
        key_types: &[Raw],
        value: &Raw,
        depth: usize,
    ) -> Result<Raw, Failure> {
        let key = match tagged(value, STORED_REFERENCE_TAG) {
            Some([stored_database, stored_table, key])
                if stored_database == database && stored_table == table =>
            {
                key
            }
            Some(_) => return Err(Failure::Mismatch),
            // A row document and canonical text spell a stored reference as
            // the target's schema-directed key (29-formats.md).
            None => value,
        };
        let key = match key_types {
            [] => self.lower_tuple(&[], key, depth)?,
            [single] => self.lower(single, key, depth + 1)?,
            composite => self.lower_tuple(composite, key, depth)?,
        };
        Ok(tag(
            STORED_REFERENCE_TAG,
            vec![database.clone(), table.clone(), key],
        ))
    }
}

/// A lowering failure before it is attributed to a field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    Descriptor,
    Mismatch,
}

impl Failure {
    fn for_field(self, field: &str) -> TypedRowError {
        match self {
            Self::Descriptor => TypedRowError::InvalidDescriptor,
            Self::Mismatch => TypedRowError::TypeMismatch(field.to_owned()),
        }
    }
}

fn lower_primitive(primitive: &str, value: &Raw) -> Result<Raw, Failure> {
    match (primitive, value) {
        // ORNA-LIT-002: an unsuffixed fractional numeral checked against
        // `Float` is converted once, correctly rounded to nearest-even.
        ("Float", Raw::Tag(DECIMAL_TAG, _)) => {
            let Some([Raw::Int(coefficient), Raw::Int(exponent)]) = tagged(value, DECIMAL_TAG)
            else {
                return Err(Failure::Mismatch);
            };
            let number = format!("{coefficient}e{exponent}")
                .parse::<f64>()
                .map_err(|_| Failure::Mismatch)?;
            if !number.is_finite() {
                return Err(Failure::Mismatch);
            }
            Ok(Raw::Float(number.to_bits()))
        }
        // Canonical text spells a Uuid as its lowercase hyphenated text
        // under the type witness.
        ("Uuid", Raw::Text(text)) => {
            let id = Uuid::parse_str(text).map_err(|_| Failure::Mismatch)?;
            if id.hyphenated().to_string() != *text {
                return Err(Failure::Mismatch);
            }
            Ok(Raw::Tag(
                UUID_TAG,
                Box::new(Raw::Bytes(id.as_bytes().to_vec())),
            ))
        }
        ("Unit", Raw::Array(items)) if items.is_empty() => Ok(tag(UNIT_TAG, Vec::new())),
        // A stored field's Blob is a repository-issued reference; a row
        // re-lowered from storage carries it unchanged.
        ("Blob", Raw::Tag(STORED_BLOB_TAG, _)) => Ok(value.clone()),
        ("Blob", _) => Err(Failure::Mismatch),
        // Every other primitive is already in its canonical encoding; the
        // stored-row validation checks it against the primitive.
        _ => Ok(value.clone()),
    }
}

/// An exact decimal for a money amount: a Decimal, or an integer numeral as
/// its normalised Decimal. A Float is never an intermediate (ORNA-LIT-003).
fn exact_decimal(value: &Raw) -> Result<Raw, Failure> {
    match value {
        Raw::Tag(DECIMAL_TAG, _) => Ok(value.clone()),
        Raw::Int(integer) => {
            let decimal = Decimal::try_new(integer.clone(), BigInt::from(0))
                .map_err(|_| Failure::Mismatch)?;
            Ok(tag(
                DECIMAL_TAG,
                vec![
                    Raw::Int(decimal.coefficient().clone()),
                    Raw::Int(decimal.exponent10().clone()),
                ],
            ))
        }
        _ => Err(Failure::Mismatch),
    }
}

/// The entries of a record value by field name: an evaluator or row-document
/// map, or a stored record `[owner, [[name_or_member_id, value]...]]`. With
/// `members`, a member ObjectId key is resolved to its member name.
fn record_entries<'v>(
    value: &'v Raw,
    members: Option<&'v [Raw]>,
) -> Result<BTreeMap<&'v str, &'v Raw>, Failure> {
    let mut entries = BTreeMap::new();
    let mut insert = |name: &'v str, value: &'v Raw| {
        if entries.insert(name, value).is_some() {
            Err(Failure::Mismatch)
        } else {
            Ok(())
        }
    };
    match value {
        Raw::Map(fields) => {
            for (key, value) in fields {
                let Raw::Text(name) = key else {
                    return Err(Failure::Mismatch);
                };
                insert(name, value)?;
            }
        }
        Raw::Tag(RECORD_TAG, _) => {
            let Some([_, Raw::Array(fields)]) = tagged(value, RECORD_TAG) else {
                return Err(Failure::Mismatch);
            };
            for field in fields {
                let Some([key, value]) = array(field) else {
                    return Err(Failure::Mismatch);
                };
                let name = match key {
                    Raw::Text(name) => name.as_str(),
                    member_id => members
                        .and_then(|members| {
                            members.iter().find_map(|member| match array(member) {
                                Some([id, Raw::Text(name), ..]) if id == member_id => {
                                    Some(name.as_str())
                                }
                                _ => None,
                            })
                        })
                        .ok_or(Failure::Mismatch)?,
                };
                insert(name, value)?;
            }
        }
        _ => return Err(Failure::Mismatch),
    }
    Ok(entries)
}

/// Whether the owner slot of a record or enum value names `id`: the stored
/// ObjectId itself, the declaration's name as a row document or the
/// evaluator spells it, or no owner.
fn owner_matches(stored: &Raw, id: &Raw) -> bool {
    matches!(stored, Raw::Null | Raw::Text(_)) || stored == id
}

fn option(value: Option<Raw>) -> Raw {
    tag(
        OPTION_TAG,
        match value {
            Some(value) => vec![Raw::Int(1.into()), value],
            None => vec![Raw::Int(0.into())],
        },
    )
}

fn tag(number: u64, items: Vec<Raw>) -> Raw {
    Raw::Tag(number, Box::new(Raw::Array(items)))
}

/// The array payload of `value` when it carries tag `number`.
fn tagged(value: &Raw, number: u64) -> Option<&[Raw]> {
    match value {
        Raw::Tag(found, payload) if *found == number => array(payload),
        _ => None,
    }
}

fn array(value: &Raw) -> Option<&[Raw]> {
    match value {
        Raw::Array(items) => Some(items),
        _ => None,
    }
}

fn small_int(value: &Raw) -> Option<u64> {
    match value {
        Raw::Int(number) => u64::try_from(number).ok(),
        _ => None,
    }
}

fn uuid(value: &Raw) -> Option<[u8; 16]> {
    match value {
        Raw::Tag(UUID_TAG, payload) => match payload.as_ref() {
            Raw::Bytes(bytes) => bytes.as_slice().try_into().ok(),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectIdentityManifest, ObjectKind};
    use orna_semantic_v1::ModuleInput;
    use orna_value_v1::Value;

    const DATABASE: [u8; 16] = [0x0d; 16];

    struct Table {
        identities: ObjectIdentityManifest,
        descriptor: SchemaDescriptor,
        table_id: [u8; 16],
    }

    impl Table {
        fn new(source: &str, table: &str) -> Self {
            let modules = [ModuleInput::new("main.orna", source)];
            let analysis = orna_semantic_v1::analyze(&modules);
            assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
            let identities = ObjectIdentityManifest::reconcile(&modules, None).unwrap();
            let artifact = identities
                .source_table_schema(&modules, &analysis, Some(DATABASE), "", "main.orna", table)
                .unwrap();
            Self {
                descriptor: artifact.descriptor().clone(),
                table_id: *artifact.table_object_id(),
                identities,
            }
        }

        fn id(&self, kind: ObjectKind, name: &str) -> Raw {
            uuid_raw(self.identities.id(kind, name).unwrap())
        }

        /// Lowers `values`, checks the tuple against the descriptor, and
        /// checks that lowering the stored values again changes nothing.
        fn lower(&self, key: Raw, values: &[(&str, Raw)]) -> BTreeMap<String, Raw> {
            let schema = StoredSchema::new(&self.descriptor).unwrap();
            let row = values
                .iter()
                .map(|(name, value)| (*name, value))
                .collect::<BTreeMap<_, _>>();
            let stored = schema.lower_stored_fields(&row).unwrap();
            self.descriptor
                .validate_stored_row(
                    self.table_id,
                    &canonical(key),
                    &canonical(Raw::Array(stored.clone())),
                )
                .expect("the lowered tuple is a stored row of the descriptor");
            let names = schema
                .fields()
                .iter()
                .filter(|field| field.role() == 1)
                .map(|field| field.name().to_owned())
                .collect::<Vec<_>>();
            let by_name = names
                .into_iter()
                .zip(stored.iter().cloned())
                .collect::<BTreeMap<_, _>>();
            let again = by_name
                .iter()
                .map(|(name, value)| (name.as_str(), value))
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                schema.lower_stored_fields(&again).unwrap(),
                stored,
                "stored values lower to themselves"
            );
            by_name
        }
    }

    fn uuid_raw(id: &[u8; 16]) -> Raw {
        Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(id.to_vec())))
    }

    fn canonical(raw: Raw) -> Vec<u8> {
        Value::new(raw).unwrap().encode().unwrap()
    }

    fn text(value: &str) -> Raw {
        Raw::Text(value.to_owned())
    }

    fn decimal(coefficient: i64, exponent: i64) -> Raw {
        tag(
            DECIMAL_TAG,
            vec![Raw::Int(coefficient.into()), Raw::Int(exponent.into())],
        )
    }

    #[test]
    fn structural_values_lower_to_their_declared_encodings() {
        let table = Table::new(
            "pub table Probe(id: Str) {
    note: Str?,
    later: Str?,
    pair: (Int, Str),
    point: { x: Int, y: Str },
    ratio: Float,
    tags: [Str],
    marker: Uuid,
}
",
            "Probe",
        );
        let marker = "0190c1f2-6a4e-7c3b-9d2e-1f3a4b5c6d7e";
        let stored = table.lower(
            text("k"),
            &[
                ("id", text("k")),
                ("note", Raw::Null),
                ("later", text("soon")),
                ("pair", Raw::Array(vec![Raw::Int(1.into()), text("x")])),
                (
                    "point",
                    Raw::Map(vec![
                        (text("x"), Raw::Int(2.into())),
                        (text("y"), text("z")),
                    ]),
                ),
                ("ratio", decimal(15, -1)),
                ("tags", Raw::Array(vec![text("a"), text("b")])),
                ("marker", text(marker)),
            ],
        );
        assert_eq!(stored["note"], option(None));
        assert_eq!(stored["later"], option(Some(text("soon"))));
        assert_eq!(
            stored["pair"],
            tag(TUPLE_TAG, vec![Raw::Int(1.into()), text("x")])
        );
        assert_eq!(
            stored["point"],
            tag(
                RECORD_TAG,
                vec![
                    Raw::Null,
                    Raw::Array(vec![
                        Raw::Array(vec![text("x"), Raw::Int(2.into())]),
                        Raw::Array(vec![text("y"), text("z")]),
                    ]),
                ],
            )
        );
        // ORNA-LIT-002: the unsuffixed numeral is rounded once to binary64.
        assert_eq!(stored["ratio"], Raw::Float(1.5_f64.to_bits()));
        assert_eq!(
            stored["marker"],
            uuid_raw(Uuid::parse_str(marker).unwrap().as_bytes())
        );
    }

    #[test]
    fn nominal_enum_and_refined_values_lower_by_object_identity() {
        let table = Table::new(
            "pub type Username = Str {
    assert != \"\";
}

pub type EmailAddress {
    value: Str,
}

pub enum Role {
    admin,
    member,
}

pub enum Availability {
    ready,
    waiting { reason: Str },
}

pub table User(id: Str) {
    username: Username,
    email: EmailAddress?,
    role: Role,
    backup: Role,
    availability: Availability,
}
",
            "User",
        );
        let stored = table.lower(
            text("ada"),
            &[
                ("username", text("ada")),
                (
                    "email",
                    Raw::Map(vec![(text("value"), text("ada@example.com"))]),
                ),
                ("role", text("admin")),
                ("backup", text("Role.member")),
                (
                    "availability",
                    tag(
                        RECORD_TAG,
                        vec![
                            text("waiting"),
                            Raw::Array(vec![Raw::Array(vec![text("reason"), text("busy")])]),
                        ],
                    ),
                ),
            ],
        );
        let id = |kind, name| table.id(kind, name);
        assert_eq!(
            stored["username"],
            tag(
                REFINED_TAG,
                vec![id(ObjectKind::Type, "Username"), text("ada")]
            )
        );
        assert_eq!(
            stored["email"],
            option(Some(tag(
                RECORD_TAG,
                vec![
                    id(ObjectKind::Type, "EmailAddress"),
                    Raw::Array(vec![Raw::Array(vec![
                        id(ObjectKind::TypeField, "EmailAddress.value"),
                        text("ada@example.com"),
                    ])]),
                ],
            )))
        );
        assert_eq!(
            stored["role"],
            tag(
                ENUM_TAG,
                vec![
                    id(ObjectKind::Type, "Role"),
                    id(ObjectKind::Variant, "Role.admin"),
                    Raw::Null,
                ],
            )
        );
        assert_eq!(
            stored["backup"],
            tag(
                ENUM_TAG,
                vec![
                    id(ObjectKind::Type, "Role"),
                    id(ObjectKind::Variant, "Role.member"),
                    Raw::Null,
                ],
            )
        );
        let waiting = id(ObjectKind::Variant, "Availability.waiting");
        assert_eq!(
            stored["availability"],
            tag(
                ENUM_TAG,
                vec![
                    id(ObjectKind::Type, "Availability"),
                    waiting.clone(),
                    tag(
                        RECORD_TAG,
                        vec![
                            waiting,
                            Raw::Array(vec![Raw::Array(vec![
                                id(ObjectKind::VariantField, "Availability.waiting.reason"),
                                text("busy"),
                            ])]),
                        ],
                    ),
                ],
            )
        );
    }

    #[test]
    fn money_quantity_and_references_lower_to_their_declared_targets() {
        let table = Table::new(
            "pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub type GBP {
    impl Currency {
        static code = \"GBP\";
        static minor_digits = 2;
    }
}

pub dim Illuminance;
pub unit lx : Illuminance base;

pub table Contact(id: Str) {
    name: Str,
}

pub table Invoice(id: Str) {
    total: Money<GBP>,
    deposit: Money<GBP>,
    light: Float<lx>,
    owner: Contact,
}
",
            "Invoice",
        );
        let stored = table.lower(
            text("inv-1"),
            &[
                ("total", decimal(125, -1)),
                ("deposit", Raw::Int(120.into())),
                ("light", Raw::Float(2.5_f64.to_bits())),
                ("owner", text("c1")),
            ],
        );
        let gbp = table.id(ObjectKind::Type, "GBP");
        assert_eq!(
            stored["total"],
            tag(MONEY_TAG, vec![decimal(125, -1), gbp.clone()])
        );
        // An exact integer amount, as its normalised Decimal and never an
        // intermediate Float (ORNA-LIT-003).
        assert_eq!(stored["deposit"], tag(MONEY_TAG, vec![decimal(12, 1), gbp]));
        assert_eq!(
            stored["light"],
            tag(
                QUANTITY_TAG,
                vec![
                    Raw::Float(2.5_f64.to_bits()),
                    table.id(ObjectKind::Unit, "lx")
                ],
            )
        );
        // ORNA-REF-001: database identity, table ObjectId and the key.
        assert_eq!(
            stored["owner"],
            tag(
                STORED_REFERENCE_TAG,
                vec![
                    uuid_raw(&DATABASE),
                    table.id(ObjectKind::Table, "Contact"),
                    text("c1")
                ],
            )
        );
    }

    #[test]
    fn rows_that_do_not_fit_the_descriptor_are_refused() {
        let table = Table::new(
            "pub enum Role {
    admin,
}

pub table Account(id: Str) {
    first: Str,
    last: Str,
    full: Str => \"{first} {last}\",
    role: Role,
}
",
            "Account",
        );
        let schema = StoredSchema::new(&table.descriptor).unwrap();
        let lower = |values: &[(&str, Raw)]| {
            let row = values
                .iter()
                .map(|(name, value)| (*name, value))
                .collect::<BTreeMap<_, _>>();
            schema.lower_stored_fields(&row)
        };
        let complete = [
            ("first", text("Ada")),
            ("last", text("Lovelace")),
            ("role", text("admin")),
        ];
        assert!(lower(&complete).is_ok());
        assert_eq!(
            lower(&complete[..2]),
            Err(TypedRowError::MissingField("role".to_owned()))
        );
        // ORNA-FIELD-006: a computed field cannot be supplied.
        let mut with_computed = complete.to_vec();
        with_computed.push(("full", text("Ada Lovelace")));
        assert_eq!(
            lower(&with_computed),
            Err(TypedRowError::UnknownField("full".to_owned()))
        );
        let mut unknown_variant = complete.to_vec();
        unknown_variant[2] = ("role", text("owner"));
        assert_eq!(
            lower(&unknown_variant),
            Err(TypedRowError::TypeMismatch("role".to_owned()))
        );
    }
}

/// Orders stored record fields by their encoded field keys (ORNA-FORMAT-004).
/// Declaration order and name order are not canonical for the stored value.
fn canonical_record_fields(stored: Vec<Raw>) -> Result<Vec<Raw>, Failure> {
    let mut keyed = stored
        .into_iter()
        .map(|field| {
            let key = array(&field)
                .and_then(|parts| parts.first())
                .ok_or(Failure::Descriptor)?;
            let encoded = orna_value_v1::Value::new(key.clone())
                .and_then(|value| value.encode())
                .map_err(|_| Failure::Mismatch)?;
            Ok((encoded, field))
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(keyed.into_iter().map(|(_, field)| field).collect())
}
