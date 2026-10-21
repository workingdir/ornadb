//! Format-3 schema evolution of one committed relation (ORNA-EVOL-001,
//! ORNA-EVOL-002, ORNA-EVOL-009; ORNA-FIELD-003, ORNA-FIELD-005).
//!
//! `orna commit` stages the store for the staged source. When a table's
//! source-derived descriptor differs from the committed one, its relation
//! evolves in the same staging. Fields are matched by their stable field
//! ObjectId and never by name, so a rename that kept its identity (`orna mv`)
//! keeps its values (ORNA-EVOL-001). Every committed row is rewritten to the
//! new descriptor and admitted against it before anything is staged
//! (ORNA-EVOL-002): one row that does not fit refuses the whole change. The
//! primary map's domain names the schema digest, so a new schema is always a
//! new primary map.
//!
//! What a committed row receives for each stored field of the new schema:
//!
//! - a field it already stores (by ObjectId) keeps its value, converted when
//!   the type changed losslessly: `T` to `T?`, `T?` to `T` for a row that
//!   holds a value, `Int` to `Decimal`, and `Int` to `Float` when exact;
//! - a new optional field is absent;
//! - a new required field takes the value of its closed constant default,
//!   frozen into the row at the introducing commit (ORNA-EVOL-009). A
//!   default that needs evaluation cannot be introduced to existing rows; the
//!   field must be optional or backfilled explicitly (ORNA-FIELD-005).
//!
//! A field the new schema no longer stores is dropped from every row. The
//! primary key is the rows' address and cannot change here; an explicit
//! rekey is a row operation (ORNA-EVOL-007).

use std::collections::BTreeMap;

use num_bigint::BigInt;

use orna_value_v1::{
    Decimal, Raw, SchemaDescriptor, decode_canonical_storage_value,
    encode_canonical_storage_value,
};

const DECIMAL_TAG: u64 = 60_000;
const OPTION_TAG: u64 = 60_013;

/// Why a committed relation cannot evolve to the staged schema. Nothing is
/// staged when evolution is refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvolutionRefusal {
    /// A descriptor is not a closed storage descriptor.
    InvalidDescriptor,
    /// The primary key changed. Re-addressing every row is an explicit rekey,
    /// not a schema change (ORNA-EVOL-007).
    KeyChanged,
    /// A stored field's type changed and no lossless conversion exists.
    UnsupportedConversion { field: String },
    /// A new required field has no closed constant default to give the rows
    /// that already exist (ORNA-FIELD-003, ORNA-FIELD-005).
    FieldNeedsValue { field: String },
    /// A committed row does not fit the new schema, for example an optional
    /// field made required holds no value, or a stored enum value names a
    /// removed variant.
    RowDoesNotFit { field: Option<String> },
    /// A rewritten row exceeds the 8 KiB inline row size, and this store
    /// writer has no large-row overflow encoding.
    RowTooLarge,
    /// The relation has a secondary index map, which evolution does not
    /// rebuild.
    IndexedRelation,
}

impl std::fmt::Display for EvolutionRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDescriptor => formatter.write_str("a table descriptor is not closed"),
            Self::KeyChanged => formatter.write_str(
                "the primary key changed; rows cannot be re-addressed by a schema change",
            ),
            Self::UnsupportedConversion { field } => write!(
                formatter,
                "the type of `{field}` changed and its stored values cannot be converted"
            ),
            Self::FieldNeedsValue { field } => write!(
                formatter,
                "new required field `{field}` has no constant default for the existing rows"
            ),
            Self::RowDoesNotFit { field: Some(field) } => write!(
                formatter,
                "a committed row's `{field}` does not fit the new schema"
            ),
            Self::RowDoesNotFit { field: None } => {
                formatter.write_str("a committed row does not fit the new schema")
            }
            Self::RowTooLarge => formatter.write_str(
                "a rewritten row exceeds the 8 KiB inline row size this store writer supports",
            ),
            Self::IndexedRelation => formatter
                .write_str("the relation has a secondary index, which evolution does not rebuild"),
        }
    }
}

impl std::error::Error for EvolutionRefusal {}

/// The schema changes of one evolved relation, by field name, for review.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchemaChanges {
    added: Vec<String>,
    removed: Vec<String>,
    renamed: Vec<(String, String)>,
    converted: Vec<String>,
}

impl SchemaChanges {
    /// Stored fields the new schema adds.
    pub fn added(&self) -> &[String] {
        &self.added
    }

    /// Stored fields the new schema no longer stores.
    pub fn removed(&self) -> &[String] {
        &self.removed
    }

    /// Fields, by ObjectId, whose name changed: `(old, new)`.
    pub fn renamed(&self) -> &[(String, String)] {
        &self.renamed
    }

    /// Stored fields whose values were converted to a new type.
    pub fn converted(&self) -> &[String] {
        &self.converted
    }
}

/// One field descriptor `[field_id, name, type_node, role, fallback]`.
struct Field<'a> {
    id: [u8; 16],
    name: &'a str,
    type_node: &'a Raw,
    role: u64,
}

/// Where a committed row's value for one new stored field comes from.
enum Source {
    /// The old stored value at `index`, converted from `from` to `to`.
    Carried { index: usize, from: Raw, to: Raw },
    /// A new optional field: absent.
    Absent,
    /// A new required field: the frozen introduction value.
    Introduced(Raw),
    /// A new required field of a table with no rows: never read.
    Unreachable,
}

/// The validated rewrite of one relation's rows from its committed schema to
/// the staged one.
pub(crate) struct EvolutionPlan {
    relation_id: [u8; 16],
    descriptor: SchemaDescriptor,
    sources: Vec<(String, Source)>,
    changes: SchemaChanges,
}

impl EvolutionPlan {
    /// Plans the evolution of `relation_id` from `old` to `new`. `rows` is
    /// the committed row count; `introductions` holds the stored value of each
    /// new field's closed constant default, keyed by field ObjectId. A
    /// relation with no rows has nothing to rewrite, so any change of its
    /// schema is admitted.
    pub(crate) fn new(
        relation_id: [u8; 16],
        old: &SchemaDescriptor,
        new: &SchemaDescriptor,
        rows: u64,
        introductions: &BTreeMap<[u8; 16], Raw>,
    ) -> Result<Self, EvolutionRefusal> {
        let (old_keys, old_fields) = fields(old)?;
        let (new_keys, new_fields) = fields(new)?;
        let key_types = |keys: &[[u8; 16]], fields: &[Field<'_>]| {
            keys.iter()
                .map(|key| {
                    fields
                        .iter()
                        .find(|field| field.id == *key && field.role == 0)
                        .map(|field| field.type_node.clone())
                })
                .collect::<Option<Vec<_>>>()
        };
        let rewrites_rows = rows > 0;
        if rewrites_rows
            && (old_keys != new_keys
                || key_types(&old_keys, &old_fields).ok_or(EvolutionRefusal::InvalidDescriptor)?
                    != key_types(&new_keys, &new_fields)
                        .ok_or(EvolutionRefusal::InvalidDescriptor)?)
        {
            return Err(EvolutionRefusal::KeyChanged);
        }

        let mut changes = SchemaChanges::default();
        for field in &new_fields {
            if let Some(old_field) = old_fields.iter().find(|old| old.id == field.id) {
                if old_field.name != field.name {
                    changes
                        .renamed
                        .push((old_field.name.to_owned(), field.name.to_owned()));
                }
            }
        }
        let old_stored = old_fields
            .iter()
            .filter(|field| field.role == 1)
            .collect::<Vec<_>>();
        let mut sources = Vec::new();
        for field in new_fields.iter().filter(|field| field.role == 1) {
            let carried = old_stored
                .iter()
                .position(|old_field| old_field.id == field.id);
            let source = match carried {
                Some(index) => {
                    let from = old_stored[index].type_node;
                    if from != field.type_node {
                        if rewrites_rows && !convertible(from, field.type_node) {
                            return Err(EvolutionRefusal::UnsupportedConversion {
                                field: field.name.to_owned(),
                            });
                        }
                        changes.converted.push(field.name.to_owned());
                    }
                    Source::Carried {
                        index,
                        from: from.clone(),
                        to: field.type_node.clone(),
                    }
                }
                None => {
                    changes.added.push(field.name.to_owned());
                    if is_option(field.type_node) {
                        Source::Absent
                    } else if let Some(value) = introductions.get(&field.id) {
                        Source::Introduced(value.clone())
                    } else if !rewrites_rows {
                        Source::Unreachable
                    } else {
                        return Err(EvolutionRefusal::FieldNeedsValue {
                            field: field.name.to_owned(),
                        });
                    }
                }
            };
            sources.push((field.name.to_owned(), source));
        }
        for old_field in &old_stored {
            if !new_fields
                .iter()
                .any(|field| field.id == old_field.id && field.role == 1)
            {
                changes.removed.push(old_field.name.to_owned());
            }
        }
        Ok(Self {
            relation_id,
            descriptor: new.clone(),
            sources,
            changes,
        })
    }

    pub(crate) fn changes(&self) -> &SchemaChanges {
        &self.changes
    }

    /// Rewrites one committed row's canonical stored fields to the new
    /// schema and admits the result against the new descriptor.
    pub(crate) fn rewrite_row(
        &self,
        canonical_key: &[u8],
        canonical_fields: &[u8],
    ) -> Result<Vec<u8>, EvolutionRefusal> {
        let Raw::Array(old_values) = decode_canonical_storage_value(canonical_fields)
            .map_err(|_| EvolutionRefusal::RowDoesNotFit { field: None })?
        else {
            return Err(EvolutionRefusal::RowDoesNotFit { field: None });
        };
        let values = self
            .sources
            .iter()
            .map(|(name, source)| {
                let unfit = || EvolutionRefusal::RowDoesNotFit {
                    field: Some(name.clone()),
                };
                match source {
                    Source::Carried { index, from, to } => {
                        let value = old_values.get(*index).ok_or_else(unfit)?;
                        convert(value, from, to).ok_or_else(unfit)
                    }
                    Source::Absent => Ok(option(None)),
                    Source::Introduced(value) => Ok(value.clone()),
                    Source::Unreachable => Err(unfit()),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let encoded = encode_canonical_storage_value(&Raw::Array(values))
            .map_err(|_| EvolutionRefusal::RowDoesNotFit { field: None })?;
        self.descriptor
            .validate_stored_row(self.relation_id, canonical_key, &encoded)
            .map_err(|_| EvolutionRefusal::RowDoesNotFit { field: None })?;
        Ok(encoded)
    }
}

/// The key field ObjectIds and every field of a closed descriptor.
fn fields(descriptor: &SchemaDescriptor) -> Result<(Vec<[u8; 16]>, Vec<Field<'_>>), EvolutionRefusal> {
    let Raw::Map(entries) = descriptor.raw() else {
        return Err(EvolutionRefusal::InvalidDescriptor);
    };
    let entry = |number: u8| {
        entries
            .iter()
            .find(|(key, _)| *key == Raw::Int(number.into()))
            .map(|(_, value)| value)
            .ok_or(EvolutionRefusal::InvalidDescriptor)
    };
    let Raw::Array(keys) = entry(2)? else {
        return Err(EvolutionRefusal::InvalidDescriptor);
    };
    let Raw::Array(declared) = entry(3)? else {
        return Err(EvolutionRefusal::InvalidDescriptor);
    };
    let keys = keys
        .iter()
        .map(|key| uuid(key).ok_or(EvolutionRefusal::InvalidDescriptor))
        .collect::<Result<Vec<_>, _>>()?;
    let fields = declared
        .iter()
        .map(|field| match field {
            Raw::Array(parts) => match parts.as_slice() {
                [id, Raw::Text(name), type_node, Raw::Int(role), _] => Ok(Field {
                    id: uuid(id).ok_or(EvolutionRefusal::InvalidDescriptor)?,
                    name,
                    type_node,
                    role: u64::try_from(role).map_err(|_| EvolutionRefusal::InvalidDescriptor)?,
                }),
                _ => Err(EvolutionRefusal::InvalidDescriptor),
            },
            _ => Err(EvolutionRefusal::InvalidDescriptor),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((keys, fields))
}

/// Whether every value of type `from` has a lossless value of type `to`
/// (a row-dependent conversion is checked again per row by [`convert`]).
fn convertible(from: &Raw, to: &Raw) -> bool {
    if from == to {
        return true;
    }
    match (option_inner(from), option_inner(to)) {
        (Some(from), Some(to)) => convertible(from, to),
        (None, Some(to)) => convertible(from, to),
        (Some(from), None) => convertible(from, to),
        (None, None) => match (list_element(from), list_element(to)) {
            (Some(from), Some(to)) => convertible(from, to),
            (None, None) => matches!(
                (primitive(from), primitive(to)),
                (Some("Int"), Some("Decimal" | "Float"))
            ),
            _ => false,
        },
    }
}

/// Converts one stored value of type `from` to type `to`; `None` when this
/// value has no lossless counterpart (an absent optional made required, or
/// an integer a Float cannot hold exactly).
fn convert(value: &Raw, from: &Raw, to: &Raw) -> Option<Raw> {
    if from == to {
        return Some(value.clone());
    }
    match (option_inner(from), option_inner(to)) {
        (Some(from), Some(to)) => match option_value(value)? {
            Some(present) => Some(option(Some(convert(present, from, to)?))),
            None => Some(option(None)),
        },
        (None, Some(to)) => Some(option(Some(convert(value, from, to)?))),
        (Some(from), None) => convert(option_value(value)??, from, to),
        (None, None) => match (list_element(from), list_element(to)) {
            (Some(from), Some(to)) => {
                let Raw::Array(items) = value else {
                    return None;
                };
                items
                    .iter()
                    .map(|item| convert(item, from, to))
                    .collect::<Option<Vec<_>>>()
                    .map(Raw::Array)
            }
            _ => {
                let Raw::Int(integer) = value else {
                    return None;
                };
                match primitive(to)? {
                    "Decimal" => {
                        let decimal = Decimal::try_new(integer.clone(), BigInt::from(0)).ok()?;
                        Some(Raw::Tag(
                            DECIMAL_TAG,
                            Box::new(Raw::Array(vec![
                                Raw::Int(decimal.coefficient().clone()),
                                Raw::Int(decimal.exponent10().clone()),
                            ])),
                        ))
                    }
                    "Float" => {
                        let text = integer.to_string();
                        let float = text.parse::<f64>().ok()?;
                        (float.is_finite() && format!("{float:.0}") == text)
                            .then(|| Raw::Float(float.to_bits()))
                    }
                    _ => None,
                }
            }
        },
    }
}

fn is_option(type_node: &Raw) -> bool {
    option_inner(type_node).is_some()
}

/// `T` of an option type node `[2, T]`.
fn option_inner(type_node: &Raw) -> Option<&Raw> {
    match type_node {
        Raw::Array(parts) => match parts.as_slice() {
            [Raw::Int(code), inner] if *code == BigInt::from(2) => Some(inner),
            _ => None,
        },
        _ => None,
    }
}

/// `T` of a list type node `[1, T]`.
fn list_element(type_node: &Raw) -> Option<&Raw> {
    match type_node {
        Raw::Array(parts) => match parts.as_slice() {
            [Raw::Int(code), element] if *code == BigInt::from(1) => Some(element),
            _ => None,
        },
        _ => None,
    }
}

/// The name of a primitive type node `[0, name]`.
fn primitive(type_node: &Raw) -> Option<&str> {
    match type_node {
        Raw::Array(parts) => match parts.as_slice() {
            [Raw::Int(code), Raw::Text(name)] if *code == BigInt::from(0) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// A stored option value: `Some(None)` for absent, `Some(Some(value))` for
/// present, `None` when `value` is not an option.
fn option_value(value: &Raw) -> Option<Option<&Raw>> {
    let Raw::Tag(OPTION_TAG, payload) = value else {
        return None;
    };
    let Raw::Array(parts) = payload.as_ref() else {
        return None;
    };
    match parts.as_slice() {
        [Raw::Int(state)] if *state == BigInt::from(0) => Some(None),
        [Raw::Int(state), present] if *state == BigInt::from(1) => Some(Some(present)),
        _ => None,
    }
}

fn option(value: Option<Raw>) -> Raw {
    Raw::Tag(
        OPTION_TAG,
        Box::new(Raw::Array(match value {
            Some(value) => vec![Raw::Int(1.into()), value],
            None => vec![Raw::Int(0.into())],
        })),
    )
}

fn uuid(value: &Raw) -> Option<[u8; 16]> {
    match value {
        Raw::Tag(37, payload) => match payload.as_ref() {
            Raw::Bytes(bytes) => bytes.as_slice().try_into().ok(),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_value_v1::Value;

    const TABLE: [u8; 16] = [0x40; 16];

    fn id(seed: u8) -> Raw {
        Raw::Tag(37, Box::new(Raw::Bytes(vec![seed; 16])))
    }

    fn primitive(name: &str) -> Raw {
        Raw::Array(vec![Raw::Int(0.into()), Raw::Text(name.into())])
    }

    fn optional(inner: Raw) -> Raw {
        Raw::Array(vec![Raw::Int(2.into()), inner])
    }

    fn field(seed: u8, name: &str, type_node: Raw, role: u64) -> Raw {
        Raw::Array(vec![
            id(seed),
            Raw::Text(name.into()),
            type_node,
            Raw::Int(role.into()),
            Raw::Array(vec![Raw::Int(0.into())]),
        ])
    }

    /// `(id: <key>) { ... }` with fields given in ascending ObjectId order.
    fn descriptor(key: Raw, fields: Vec<Raw>) -> SchemaDescriptor {
        let mut all = vec![field(1, "id", key, 0)];
        all.extend(fields);
        SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (
                Raw::Int(1.into()),
                Raw::Tag(37, Box::new(Raw::Bytes(TABLE.to_vec()))),
            ),
            (Raw::Int(2.into()), Raw::Array(vec![id(1)])),
            (Raw::Int(3.into()), Raw::Array(all)),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .unwrap()
    }

    fn key() -> Vec<u8> {
        Value::new(Raw::Text("k".into())).unwrap().encode().unwrap()
    }

    fn row(values: Vec<Raw>) -> Vec<u8> {
        encode_canonical_storage_value(&Raw::Array(values)).unwrap()
    }

    fn values(encoded: &[u8]) -> Vec<Raw> {
        let Raw::Array(values) = decode_canonical_storage_value(encoded).unwrap() else {
            panic!("stored fields are an array");
        };
        values
    }

    fn committed() -> SchemaDescriptor {
        descriptor(
            primitive("Str"),
            vec![
                field(2, "title", primitive("Str"), 1),
                field(3, "plays", primitive("Int"), 1),
                field(4, "rating", primitive("Int"), 1),
            ],
        )
    }

    #[test]
    fn rows_follow_fields_by_object_identity() {
        let staged = descriptor(
            primitive("Str"),
            vec![
                field(2, "name", primitive("Str"), 1),
                field(3, "plays", primitive("Decimal"), 1),
                field(5, "country", primitive("Str"), 1),
                field(6, "note", optional(primitive("Str")), 1),
            ],
        );
        let introductions = BTreeMap::from([([5; 16], Raw::Text("GB".into()))]);
        let plan = EvolutionPlan::new(TABLE, &committed(), &staged, 1, &introductions).unwrap();
        let changes = plan.changes();
        assert_eq!(changes.renamed(), &[("title".to_owned(), "name".to_owned())]);
        assert_eq!(changes.converted(), &["plays".to_owned()]);
        assert_eq!(changes.added(), &["country".to_owned(), "note".to_owned()]);
        assert_eq!(changes.removed(), &["rating".to_owned()]);

        let rewritten = plan
            .rewrite_row(
                &key(),
                &row(vec![
                    Raw::Text("Live".into()),
                    Raw::Int(70.into()),
                    Raw::Int(5.into()),
                ]),
            )
            .unwrap();
        assert_eq!(
            values(&rewritten),
            vec![
                Raw::Text("Live".into()),
                Raw::Tag(
                    DECIMAL_TAG,
                    Box::new(Raw::Array(vec![Raw::Int(7.into()), Raw::Int(1.into())])),
                ),
                Raw::Text("GB".into()),
                option(None),
            ]
        );
    }

    #[test]
    fn changes_rows_cannot_follow_are_refused() {
        let plan = |staged: &SchemaDescriptor, rows: u64| {
            EvolutionPlan::new(TABLE, &committed(), staged, rows, &BTreeMap::new())
        };
        let retyped_key = descriptor(
            primitive("Int"),
            vec![
                field(2, "title", primitive("Str"), 1),
                field(3, "plays", primitive("Int"), 1),
                field(4, "rating", primitive("Int"), 1),
            ],
        );
        assert_eq!(plan(&retyped_key, 1).err(), Some(EvolutionRefusal::KeyChanged));
        let to_text = descriptor(
            primitive("Str"),
            vec![
                field(2, "title", primitive("Str"), 1),
                field(3, "plays", primitive("Str"), 1),
                field(4, "rating", primitive("Int"), 1),
            ],
        );
        assert_eq!(
            plan(&to_text, 1).err(),
            Some(EvolutionRefusal::UnsupportedConversion {
                field: "plays".to_owned()
            })
        );
        let required = descriptor(
            primitive("Str"),
            vec![
                field(2, "title", primitive("Str"), 1),
                field(3, "plays", primitive("Int"), 1),
                field(4, "rating", primitive("Int"), 1),
                field(5, "label", primitive("Str"), 1),
            ],
        );
        // ORNA-FIELD-005: a required field without a constant default needs
        // the rows to be backfilled, unless there are none.
        assert_eq!(
            plan(&required, 1).err(),
            Some(EvolutionRefusal::FieldNeedsValue {
                field: "label".to_owned()
            })
        );
        assert!(plan(&required, 0).is_ok());
        assert!(plan(&retyped_key, 0).is_ok());
        assert!(plan(&to_text, 0).is_ok());
    }

    #[test]
    fn row_dependent_conversions_are_checked_per_row() {
        let old = descriptor(
            primitive("Str"),
            vec![
                field(2, "note", optional(primitive("Str")), 1),
                field(3, "plays", primitive("Int"), 1),
            ],
        );
        let new = descriptor(
            primitive("Str"),
            vec![
                field(2, "note", primitive("Str"), 1),
                field(3, "plays", primitive("Float"), 1),
            ],
        );
        let plan = EvolutionPlan::new(TABLE, &old, &new, 2, &BTreeMap::new()).unwrap();
        let present = plan
            .rewrite_row(
                &key(),
                &row(vec![option(Some(Raw::Text("kept".into()))), Raw::Int(3.into())]),
            )
            .unwrap();
        assert_eq!(
            values(&present),
            vec![Raw::Text("kept".into()), Raw::Float(3.0_f64.to_bits())]
        );
        // An absent value cannot become required.
        assert_eq!(
            plan.rewrite_row(&key(), &row(vec![option(None), Raw::Int(3.into())])),
            Err(EvolutionRefusal::RowDoesNotFit {
                field: Some("note".to_owned())
            })
        );
        // 2^53 + 1 has no exact binary64 value.
        assert_eq!(
            plan.rewrite_row(
                &key(),
                &row(vec![
                    option(Some(Raw::Text("kept".into()))),
                    Raw::Int(9_007_199_254_740_993_u64.into()),
                ]),
            ),
            Err(EvolutionRefusal::RowDoesNotFit {
                field: Some("plays".to_owned())
            })
        );
    }
}
