//! Stored table references and their referential constraints (ORNA-REF-001,
//! ORNA-REF-002, ORNA-REF-003, ORNA-MUT-007, ORNA-MUT-008).
//!
//! A table type used as a field denotes a stored reference to a row of that
//! table: the database identity, the table ObjectId and the primary key, never
//! an embedded copy of the row (ORNA-REF-001). The checker types a bound row of
//! the target table as the field type, so a write reaches an effect boundary
//! holding the target's row as a record. [`ReferenceCatalogue::bind_row`]
//! replaces that record by the stored identity (tag 60021), which is the form
//! the committed descriptor lowers and publication validates.
//!
//! Referential constraints are checked when an activation commits, over the
//! activation's candidate state, never statement by statement (ORNA-MUT-007):
//! a program may update dependent references and re-key the target in one
//! activation, and no intermediate state is observable outside it. Every
//! reference a written row holds must name a live row, and a row that the
//! activation removed or re-keyed away must not be named by a live reference
//! (ORNA-MUT-008, ORNA-REF-003: `restrict`, no implicit cascade). An optional
//! reference is validated whenever it is present, because weak and dangling
//! references are outside version 1.0.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use num_bigint::BigInt;
use orna_check::{TableSchema, Type};
use orna_runtime::TableObjectId;
use orna_value::{Raw, Value};

use crate::ProjectLoadError;

const UUID_TAG: u64 = 37;
const OPTION_TAG: u64 = 60_013;
const TUPLE_TAG: u64 = 60_015;
const STORED_REFERENCE_TAG: u64 = 60_021;

/// The diagnostic code of a referential constraint that failed at activation
/// commit. The specification names no code for a dangling reference, so this
/// one is catalogued by the implementation (`orna explain ORNA-REF-DANGLING`).
pub const DANGLING_REFERENCE_CODE: &str = "ORNA-REF-DANGLING";

/// Why a stored reference could not be bound or validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceError {
    /// A reference column holds a value that is neither a row of its target
    /// table nor a stored reference.
    NotARow,
    /// The target row lacks one of its key fields, or the key is not a
    /// permitted primary key (ORNA-KEY-003, ORNA-KEY-005).
    InvalidKey,
    /// A stored reference names another database or table than its column.
    WrongTarget,
    /// A referenced table has no committed identity, so no stored reference to
    /// it can be formed.
    Unbound,
    /// The candidate state could not be read, so existence cannot be proven.
    State,
    /// A present reference names no live row of the candidate state, or a live
    /// reference still names a row the activation removed (ORNA-MUT-007,
    /// ORNA-MUT-008).
    Dangling,
}

impl ReferenceError {
    /// The public diagnostic code of this failure.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotARow | Self::WrongTarget => "ORNA-EVAL-TABLE-ROW",
            Self::InvalidKey => "ORNA-EVAL-TABLE-KEY",
            Self::Unbound => "ORNA-EVAL-TABLE-UNADMITTED",
            Self::State => "ORNA-EVAL-TABLE-SNAPSHOT",
            Self::Dangling => DANGLING_REFERENCE_CODE,
        }
    }
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotARow => "a reference column holds neither a row nor a stored reference",
            Self::InvalidKey => "a referenced row has no permitted primary key",
            Self::WrongTarget => "a stored reference names another table than its column",
            Self::Unbound => "a referenced table has no committed identity",
            Self::State => "the candidate state could not be read",
            Self::Dangling => "a stored reference names no live row",
        })
    }
}

impl std::error::Error for ReferenceError {}

/// One column of a table whose type is a table.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ReferenceColumn {
    column: String,
    target: String,
    optional: bool,
}

/// What identifies a row of a referenced table.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TargetShape {
    object_id: [u8; 16],
    key_fields: Vec<String>,
}

/// The reference columns of every table in `schemas`, keyed by the table that
/// holds them. A column is a reference when its type, or the type it makes
/// optional, is the identity of another admitted table.
fn reference_columns(
    schemas: &BTreeMap<String, TableSchema>,
) -> BTreeMap<String, Vec<ReferenceColumn>> {
    let mut columns: BTreeMap<String, Vec<ReferenceColumn>> = BTreeMap::new();
    for (table, schema) in schemas {
        for (column, ty) in &schema.fields {
            let (target, optional) = match ty {
                Type::Named(target) => (target, false),
                Type::Optional(inner) => match inner.as_ref() {
                    Type::Named(target) => (target, true),
                    _ => continue,
                },
                _ => continue,
            };
            // A nominal record, enum or refined type is not a table.
            if !schemas.contains_key(target) {
                continue;
            }
            columns
                .entry(table.clone())
                .or_default()
                .push(ReferenceColumn {
                    column: column.clone(),
                    target: target.clone(),
                    optional,
                });
        }
    }
    columns
}

/// The reference columns of every admitted table and the identity of every
/// table they name, for one database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceCatalogue {
    database_id: [u8; 16],
    columns: BTreeMap<String, Vec<ReferenceColumn>>,
    targets: BTreeMap<String, TargetShape>,
    by_object_id: BTreeMap<[u8; 16], String>,
}

/// A stored reference decoded against a catalogue: the table it names and the
/// components of the primary key it carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceIdentity<'a> {
    pub table: &'a str,
    pub key: Vec<Raw>,
}

/// The candidate state of an activation: the rows a commit would make live.
///
/// Rows are evaluator records, a map from field name to value, with every
/// stored reference in its identity form.
pub trait CandidateState {
    /// The live candidate row of `table` at the primary key `key`.
    fn row(&mut self, table: &str, key: &[Raw]) -> Result<Option<Raw>, ReferenceError>;

    /// Every live candidate row of `table`.
    fn rows(&mut self, table: &str) -> Result<Vec<Raw>, ReferenceError>;
}

impl ReferenceCatalogue {
    /// Builds the catalogue of the tables in `schemas`, keyed by table identity.
    ///
    /// A column is a reference when its type, or the type it makes optional, is
    /// the identity of another admitted table. Returns `Ok(None)` when no table
    /// has such a column, or when `ids` is empty: a name-only admission has no
    /// committed table identity, so there is nothing a reference could name.
    ///
    /// # Errors
    ///
    /// [`ReferenceError::Unbound`] when some referenced table has no entry in a
    /// non-empty `ids`, or no declared primary key.
    pub fn new(
        database_id: [u8; 16],
        schemas: &BTreeMap<String, TableSchema>,
        ids: &BTreeMap<String, TableObjectId>,
    ) -> Result<Option<Self>, ReferenceError> {
        let columns = reference_columns(schemas);
        if columns.is_empty() || ids.is_empty() {
            return Ok(None);
        }
        let mut targets = BTreeMap::new();
        let mut by_object_id = BTreeMap::new();
        for column in columns.values().flatten() {
            if targets.contains_key(&column.target) {
                continue;
            }
            let object_id = ids
                .get(&column.target)
                .ok_or(ReferenceError::Unbound)?
                .bytes();
            let keys = schemas
                .get(&column.target)
                .and_then(|schema| schema.admission.as_ref())
                .map(|admission| &admission.keys)
                .filter(|keys| !keys.is_empty())
                .ok_or(ReferenceError::Unbound)?;
            targets.insert(
                column.target.clone(),
                TargetShape {
                    object_id,
                    key_fields: keys.iter().map(|(name, _)| name.clone()).collect(),
                },
            );
            by_object_id.insert(object_id, column.target.clone());
        }
        Ok(Some(Self {
            database_id,
            columns,
            targets,
            by_object_id,
        }))
    }

    /// Whether any table of `schemas` has a reference column, so that a
    /// committed write to it is checked against stored references.
    #[must_use]
    pub fn declares_references(schemas: &BTreeMap<String, TableSchema>) -> bool {
        !reference_columns(schemas).is_empty()
    }

    /// Every table the catalogue names: each referencing table and each target,
    /// in name order.
    #[must_use]
    pub fn tables(&self) -> Vec<String> {
        self.columns
            .keys()
            .chain(self.targets.keys())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The database every stored reference of this catalogue names.
    #[must_use]
    pub const fn database_id(&self) -> [u8; 16] {
        self.database_id
    }

    /// The number of primary-key components of the referenced table `table`.
    #[must_use]
    pub fn key_arity(&self, table: &str) -> Option<usize> {
        self.targets.get(table).map(|shape| shape.key_fields.len())
    }

    /// Decodes the stored reference `reference` (tag 60021) when it names this
    /// catalogue's database and a referenced table.
    #[must_use]
    pub fn identity(&self, reference: &Raw) -> Option<ReferenceIdentity<'_>> {
        let (database, table, key) = reference_parts(reference)?;
        if database != self.database_id {
            return None;
        }
        let table = self.by_object_id.get(&table)?;
        let arity = self.targets.get(table)?.key_fields.len();
        Some(ReferenceIdentity {
            table,
            key: key_components(key, arity)?,
        })
    }

    /// Replaces the target row held by each reference column of `row` with the
    /// stored reference to it (ORNA-REF-001). `row` is an evaluator record of
    /// `table`. A column that already holds a stored reference is checked
    /// against its declaration and kept, and an absent optional reference stays
    /// absent. Returns `Ok(None)` when `row` needs no change.
    ///
    /// A reference column may instead spell its target by key: the target
    /// row's key, or the key tuple of a composite key (29-formats.md).
    ///
    /// # Errors
    ///
    /// [`ReferenceError::NotARow`], [`ReferenceError::InvalidKey`] or
    /// [`ReferenceError::WrongTarget`] when a reference column holds a value
    /// that cannot be a reference to its target.
    pub fn bind_row(&self, table: &str, row: &Raw) -> Result<Option<Raw>, ReferenceError> {
        let Some(columns) = self.columns.get(table) else {
            return Ok(None);
        };
        let Raw::Map(entries) = row else {
            return Err(ReferenceError::NotARow);
        };
        let mut changed = false;
        let mut bound = Vec::with_capacity(entries.len());
        for (name, value) in entries {
            let column = match name {
                Raw::Text(name) => columns.iter().find(|column| &column.column == name),
                _ => None,
            };
            let replacement = column
                .map(|column| self.bind_value(column, value))
                .transpose()?
                .flatten();
            if replacement.is_some() {
                changed = true;
            }
            bound.push((name.clone(), replacement.unwrap_or_else(|| value.clone())));
        }
        Ok(changed.then_some(Raw::Map(bound)))
    }

    fn bind_value(
        &self,
        column: &ReferenceColumn,
        value: &Raw,
    ) -> Result<Option<Raw>, ReferenceError> {
        match value {
            Raw::Null => Ok(None),
            Raw::Map(entries) => self.identity_of_row(&column.target, entries).map(Some),
            Raw::Tag(STORED_REFERENCE_TAG, _) => {
                let shape = self.shape(&column.target)?;
                match reference_parts(value) {
                    Some((database, table, _))
                        if database == self.database_id && table == shape.object_id =>
                    {
                        Ok(None)
                    }
                    _ => Err(ReferenceError::WrongTarget),
                }
            }
            Raw::Tag(OPTION_TAG, payload) if column.optional => {
                let Raw::Array(items) = payload.as_ref() else {
                    return Err(ReferenceError::NotARow);
                };
                match items.as_slice() {
                    [state] if option_state(state) == Some(0) => Ok(None),
                    [state, inner] if option_state(state) == Some(1) => {
                        Ok(self.bind_value(column, inner)?.map(option_present))
                    }
                    _ => Err(ReferenceError::NotARow),
                }
            }
            Raw::Tag(..) => Err(ReferenceError::NotARow),
            key => self.identity_of_key(&column.target, key).map(Some),
        }
    }

    fn shape(&self, target: &str) -> Result<&TargetShape, ReferenceError> {
        self.targets.get(target).ok_or(ReferenceError::Unbound)
    }

    /// The stored reference to the row `entries` of the table `target`.
    fn identity_of_row(&self, target: &str, entries: &[(Raw, Raw)]) -> Result<Raw, ReferenceError> {
        let shape = self.shape(target)?;
        let mut components = Vec::with_capacity(shape.key_fields.len());
        for field in &shape.key_fields {
            let value = entries
                .iter()
                .find_map(|(name, value)| match name {
                    Raw::Text(name) if name == field => Some(value),
                    _ => None,
                })
                .ok_or(ReferenceError::InvalidKey)?;
            components.push(value.clone());
        }
        self.identity_of_components(shape, components)
    }

    /// The stored reference to the row of `target` whose key is spelled `key`:
    /// the key itself, or the key tuple of a composite key (29-formats.md).
    fn identity_of_key(&self, target: &str, key: &Raw) -> Result<Raw, ReferenceError> {
        let shape = self.shape(target)?;
        let components = match shape.key_fields.len() {
            0 => return Err(ReferenceError::InvalidKey),
            1 => vec![key.clone()],
            arity => match key {
                Raw::Array(items) if items.len() == arity => items.clone(),
                _ => return Err(ReferenceError::InvalidKey),
            },
        };
        self.identity_of_components(shape, components)
    }

    /// The stored reference to the row of `shape` whose key is `components`.
    fn identity_of_components(
        &self,
        shape: &TargetShape,
        components: Vec<Raw>,
    ) -> Result<Raw, ReferenceError> {
        Value::table_reference(self.database_id, shape.object_id, key_of(components))
            .map(|reference| reference.raw().clone())
            .map_err(|_| ReferenceError::InvalidKey)
    }

    /// Validates the referential constraints of an activation against its
    /// candidate state (ORNA-MUT-007, ORNA-MUT-008).
    ///
    /// `touched` lists the primary key of every row the activation wrote,
    /// removed or re-keyed, as `(table, key components)`. The candidate state
    /// decides what each key finally holds, so an intermediate state of the
    /// activation is never judged.
    ///
    /// # Errors
    ///
    /// [`ReferenceError::Dangling`] when a written row holds a reference to a
    /// row that is not live, or when a live reference names a row the activation
    /// removed. Other errors mean the state could not be read.
    pub fn validate(
        &self,
        touched: &[(String, Vec<Raw>)],
        state: &mut dyn CandidateState,
    ) -> Result<(), ReferenceError> {
        let mut gone: BTreeMap<&str, BTreeSet<Vec<u8>>> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for (table, components) in touched {
            let identity = encoded(&key_of(components.clone()))?;
            if !seen.insert((table.as_str(), identity.clone())) {
                continue;
            }
            match state.row(table, components)? {
                Some(row) => self.check_written(table, &row, state)?,
                None => {
                    if let Some((target, _)) = self.targets.get_key_value(table.as_str()) {
                        gone.entry(target.as_str()).or_default().insert(identity);
                    }
                }
            }
        }
        for (target, keys) in gone {
            self.check_unreferenced(target, &keys, state)?;
        }
        Ok(())
    }

    /// Every reference the written `row` holds names a live row.
    fn check_written(
        &self,
        table: &str,
        row: &Raw,
        state: &mut dyn CandidateState,
    ) -> Result<(), ReferenceError> {
        let Some(columns) = self.columns.get(table) else {
            return Ok(());
        };
        let Raw::Map(entries) = row else {
            return Err(ReferenceError::State);
        };
        for column in columns {
            let Some(reference) = entries.iter().find_map(|(name, value)| match name {
                Raw::Text(name) if name == &column.column => present_reference(value),
                _ => None,
            }) else {
                continue;
            };
            let identity = self
                .identity(reference)
                .ok_or(ReferenceError::WrongTarget)?;
            if identity.table != column.target {
                return Err(ReferenceError::WrongTarget);
            }
            if state.row(identity.table, &identity.key)?.is_none() {
                return Err(ReferenceError::Dangling);
            }
        }
        Ok(())
    }

    /// No live row of any referencing table names a key of `target` in `keys`,
    /// the canonical encodings of keys the activation left without a row.
    fn check_unreferenced(
        &self,
        target: &str,
        keys: &BTreeSet<Vec<u8>>,
        state: &mut dyn CandidateState,
    ) -> Result<(), ReferenceError> {
        for (table, columns) in &self.columns {
            let columns = columns
                .iter()
                .filter(|column| column.target == target)
                .collect::<Vec<_>>();
            if columns.is_empty() {
                continue;
            }
            for row in state.rows(table)? {
                let Raw::Map(entries) = &row else {
                    return Err(ReferenceError::State);
                };
                for column in &columns {
                    let Some(reference) = entries.iter().find_map(|(name, value)| match name {
                        Raw::Text(name) if name == &column.column => present_reference(value),
                        _ => None,
                    }) else {
                        continue;
                    };
                    let Some(identity) = self.identity(reference) else {
                        continue;
                    };
                    if identity.table == target && keys.contains(&encoded(&key_of(identity.key))?) {
                        return Err(ReferenceError::Dangling);
                    }
                }
            }
        }
        Ok(())
    }
}

/// The primary key a reference carries: the single component itself, or the
/// tuple of a composite key.
fn key_of(mut components: Vec<Raw>) -> Raw {
    if components.len() == 1 {
        components.remove(0)
    } else {
        Raw::Tag(TUPLE_TAG, Box::new(Raw::Array(components)))
    }
}

/// The components of a reference's key, given the target's key arity.
fn key_components(key: &Raw, arity: usize) -> Option<Vec<Raw>> {
    if arity == 1 {
        return Some(vec![key.clone()]);
    }
    match key {
        Raw::Tag(TUPLE_TAG, payload) => match payload.as_ref() {
            Raw::Array(items) if items.len() == arity => Some(items.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The database, table and key of a stored reference.
fn reference_parts(reference: &Raw) -> Option<([u8; 16], [u8; 16], &Raw)> {
    let Raw::Tag(STORED_REFERENCE_TAG, payload) = reference else {
        return None;
    };
    let Raw::Array(items) = payload.as_ref() else {
        return None;
    };
    let [database, table, key] = items.as_slice() else {
        return None;
    };
    Some((uuid(database)?, uuid(table)?, key))
}

fn uuid(raw: &Raw) -> Option<[u8; 16]> {
    match raw {
        Raw::Tag(UUID_TAG, payload) => match payload.as_ref() {
            Raw::Bytes(bytes) => bytes.as_slice().try_into().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// The stored reference a column value holds, if any: a reference itself, or
/// the present payload of an optional one.
fn present_reference(value: &Raw) -> Option<&Raw> {
    match value {
        Raw::Tag(STORED_REFERENCE_TAG, _) => Some(value),
        Raw::Tag(OPTION_TAG, payload) => match payload.as_ref() {
            Raw::Array(items) => match items.as_slice() {
                [state, inner] if option_state(state) == Some(1) => present_reference(inner),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn option_state(state: &Raw) -> Option<u8> {
    match state {
        Raw::Int(state) if *state == BigInt::from(0) => Some(0),
        Raw::Int(state) if *state == BigInt::from(1) => Some(1),
        _ => None,
    }
}

fn option_present(inner: Raw) -> Raw {
    Raw::Tag(
        OPTION_TAG,
        Box::new(Raw::Array(vec![Raw::Int(BigInt::from(1)), inner])),
    )
}

/// The canonical encoding of a key, which compares keys by value.
fn encoded(key: &Raw) -> Result<Vec<u8>, ReferenceError> {
    Value::new(key.clone())
        .and_then(|value| value.encode())
        .map_err(|_| ReferenceError::InvalidKey)
}

/// Binds the reference columns of a lowered source row (ORNA-REF-001). With a
/// catalogue, each key spelling names the row it refers to and is replaced by
/// that row's stored reference. `module_name` and `declaration` name the table in
/// the qualified form the catalogue is keyed by.
pub(crate) fn bind_source_row_references(
    references: Option<&ReferenceCatalogue>,
    module_name: &str,
    declaration: &str,
    reference_columns: &BTreeSet<String>,
    fields: &mut BTreeMap<String, Raw>,
) -> Result<(), ProjectLoadError> {
    let Some(references) = references else {
        // Without a reference catalogue the key spelling cannot name a target
        // row, so a row holding one is refused rather than stored as text.
        if reference_columns.iter().any(|column| {
            fields
                .get(column)
                .is_some_and(|value| !matches!(value, Raw::Null))
        }) {
            return Err(ProjectLoadError::InvalidSourceRow);
        }
        return Ok(());
    };
    let table = if module_name.is_empty() {
        declaration.to_owned()
    } else {
        format!("{module_name}.{declaration}")
    };
    let row = Raw::Map(
        fields
            .iter()
            .map(|(name, value)| (Raw::Text(name.clone()), value.clone()))
            .collect(),
    );
    let Some(Raw::Map(bound)) = references
        .bind_row(&table, &row)
        .map_err(|_| ProjectLoadError::InvalidSourceRow)?
    else {
        return Ok(());
    };
    for (name, value) in bound {
        let Raw::Text(name) = name else {
            return Err(ProjectLoadError::InvalidSourceRow);
        };
        *fields
            .get_mut(&name)
            .ok_or(ProjectLoadError::InvalidSourceRow)? = value;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_check::TableAdmission;

    const DATABASE: [u8; 16] = [7; 16];
    const OWNER_ID: [u8; 16] = [1; 16];

    fn text(value: &str) -> Raw {
        Raw::Text(value.to_owned())
    }

    fn record(fields: Vec<(&str, Raw)>) -> Raw {
        let mut fields = fields;
        fields.sort_by(|(left, _), (right, _)| {
            left.len().cmp(&right.len()).then_with(|| left.cmp(right))
        });
        Raw::Map(
            fields
                .into_iter()
                .map(|(name, value)| (text(name), value))
                .collect(),
        )
    }

    fn schema(fields: &[(&str, Type)], keys: &[&str]) -> TableSchema {
        TableSchema {
            fields: fields
                .iter()
                .map(|(name, ty)| ((*name).to_owned(), ty.clone()))
                .collect(),
            references: Vec::new(),
            declaration_order: fields.iter().map(|(name, _)| (*name).to_owned()).collect(),
            admission: Some(TableAdmission {
                required: BTreeSet::new(),
                private_required: false,
                computed: BTreeSet::new(),
                keys: keys
                    .iter()
                    .map(|name| ((*name).to_owned(), Type::Text))
                    .collect(),
                automatic_key: false,
            }),
            conversions: Vec::new(),
            currency: false,
        }
    }

    fn catalogue(spare: bool) -> ReferenceCatalogue {
        let mut vehicle = vec![("id", Type::Text), ("owner", Type::Named("Owner".into()))];
        if spare {
            vehicle.push((
                "spare",
                Type::Optional(Box::new(Type::Named("Owner".into()))),
            ));
        }
        let schemas = BTreeMap::from([
            (
                "Owner".to_owned(),
                schema(&[("id", Type::Text), ("name", Type::Text)], &["id"]),
            ),
            ("Vehicle".to_owned(), schema(&vehicle, &["id"])),
        ]);
        let ids = BTreeMap::from([
            ("Owner".to_owned(), TableObjectId::new(OWNER_ID)),
            ("Vehicle".to_owned(), TableObjectId::new([2; 16])),
        ]);
        ReferenceCatalogue::new(DATABASE, &schemas, &ids)
            .expect("catalogue builds")
            .expect("the schemas hold a reference")
    }

    fn reference(key: &str) -> Raw {
        Value::table_reference(DATABASE, OWNER_ID, text(key))
            .expect("a text key is permitted")
            .raw()
            .clone()
    }

    #[derive(Default)]
    struct Rows(BTreeMap<String, BTreeMap<Vec<u8>, Raw>>);

    impl Rows {
        fn put(&mut self, table: &str, key: &str, row: Raw) {
            self.0
                .entry(table.to_owned())
                .or_default()
                .insert(encoded(&text(key)).unwrap(), row);
        }

        fn remove(&mut self, table: &str, key: &str) {
            self.0
                .entry(table.to_owned())
                .or_default()
                .remove(&encoded(&text(key)).unwrap());
        }
    }

    impl CandidateState for Rows {
        fn row(&mut self, table: &str, key: &[Raw]) -> Result<Option<Raw>, ReferenceError> {
            let key = encoded(&key_of(key.to_vec()))?;
            Ok(self.0.get(table).and_then(|rows| rows.get(&key)).cloned())
        }

        fn rows(&mut self, table: &str) -> Result<Vec<Raw>, ReferenceError> {
            Ok(self
                .0
                .get(table)
                .map(|rows| rows.values().cloned().collect())
                .unwrap_or_default())
        }
    }

    fn touched(table: &str, key: &str) -> (String, Vec<Raw>) {
        (table.to_owned(), vec![text(key)])
    }

    #[test]
    fn a_bound_row_becomes_the_stored_identity_of_its_key() {
        let catalogue = catalogue(false);
        let owner = record(vec![("id", text("ada")), ("name", text("Ada"))]);
        let vehicle = record(vec![("id", text("v1")), ("owner", owner)]);

        let bound = catalogue
            .bind_row("Vehicle", &vehicle)
            .expect("the row binds")
            .expect("the owner is replaced");

        assert_eq!(
            bound,
            record(vec![("id", text("v1")), ("owner", reference("ada"))]),
            "the field holds the identity, never a copy of the target row"
        );
        let identity = catalogue.identity(&reference("ada")).expect("identity");
        assert_eq!(identity.table, "Owner");
        assert_eq!(identity.key, vec![text("ada")]);
    }

    #[test]
    fn binding_is_idempotent_and_leaves_other_tables_alone() {
        let catalogue = catalogue(false);
        let vehicle = record(vec![("id", text("v1")), ("owner", reference("ada"))]);
        assert_eq!(catalogue.bind_row("Vehicle", &vehicle), Ok(None));
        let owner = record(vec![("id", text("ada")), ("name", text("Ada"))]);
        assert_eq!(catalogue.bind_row("Owner", &owner), Ok(None));
    }

    #[test]
    fn a_reference_to_another_table_or_database_is_refused() {
        let catalogue = catalogue(false);
        let other_table = Value::table_reference(DATABASE, [9; 16], text("ada"))
            .unwrap()
            .raw()
            .clone();
        let other_database = Value::table_reference([8; 16], OWNER_ID, text("ada"))
            .unwrap()
            .raw()
            .clone();
        for owner in [other_table, other_database] {
            let vehicle = record(vec![("id", text("v1")), ("owner", owner)]);
            assert_eq!(
                catalogue.bind_row("Vehicle", &vehicle),
                Err(ReferenceError::WrongTarget)
            );
        }
        let vehicle = record(vec![("id", text("v1")), ("owner", text("ada"))]);
        let owner = Value::table_reference(DATABASE, OWNER_ID, text("ada"))
            .unwrap()
            .raw()
            .clone();
        assert_eq!(
            catalogue.bind_row("Vehicle", &vehicle),
            Ok(Some(record(vec![("id", text("v1")), ("owner", owner)])))
        );
        let keyless = record(vec![
            ("id", text("v1")),
            ("owner", record(vec![("name", text("A"))])),
        ]);
        assert_eq!(
            catalogue.bind_row("Vehicle", &keyless),
            Err(ReferenceError::InvalidKey)
        );
    }

    #[test]
    fn an_optional_reference_binds_when_present_and_stays_absent() {
        let catalogue = catalogue(true);
        let owner = record(vec![("id", text("ada")), ("name", text("Ada"))]);
        let present = Raw::Tag(
            OPTION_TAG,
            Box::new(Raw::Array(vec![Raw::Int(BigInt::from(1)), owner.clone()])),
        );
        let vehicle = record(vec![
            ("id", text("v1")),
            ("owner", owner),
            ("spare", present),
        ]);
        let bound = catalogue.bind_row("Vehicle", &vehicle).unwrap().unwrap();
        assert_eq!(
            bound,
            record(vec![
                ("id", text("v1")),
                ("owner", reference("ada")),
                ("spare", option_present(reference("ada"))),
            ])
        );
        let absent = record(vec![
            ("id", text("v2")),
            ("owner", reference("ada")),
            ("spare", Raw::Null),
        ]);
        assert_eq!(catalogue.bind_row("Vehicle", &absent), Ok(None));
    }

    #[test]
    fn a_catalogue_needs_committed_identities_and_a_reference() {
        let schemas = BTreeMap::from([
            ("Owner".to_owned(), schema(&[("id", Type::Text)], &["id"])),
            (
                "Vehicle".to_owned(),
                schema(
                    &[("id", Type::Text), ("owner", Type::Named("Owner".into()))],
                    &["id"],
                ),
            ),
        ]);
        // A name-only admission carries no identity, so there is nothing to bind.
        assert_eq!(
            ReferenceCatalogue::new(DATABASE, &schemas, &BTreeMap::new()),
            Ok(None)
        );
        // An identity for the referencing table alone cannot name the target.
        let ids = BTreeMap::from([("Vehicle".to_owned(), TableObjectId::new([2; 16]))]);
        assert_eq!(
            ReferenceCatalogue::new(DATABASE, &schemas, &ids),
            Err(ReferenceError::Unbound)
        );
        // A nominal type that names no table is not a reference.
        let nominal = BTreeMap::from([(
            "Vehicle".to_owned(),
            schema(
                &[("id", Type::Text), ("kind", Type::Named("Kind".into()))],
                &["id"],
            ),
        )]);
        let ids = BTreeMap::from([("Vehicle".to_owned(), TableObjectId::new([2; 16]))]);
        assert_eq!(ReferenceCatalogue::new(DATABASE, &nominal, &ids), Ok(None));
    }

    fn vehicle_of(owner: &str) -> Raw {
        record(vec![("id", text("v1")), ("owner", reference(owner))])
    }

    fn owner_row(key: &str) -> Raw {
        record(vec![("id", text(key)), ("name", text("Name"))])
    }

    #[test]
    fn a_reference_to_a_live_row_commits() {
        let catalogue = catalogue(false);
        let mut rows = Rows::default();
        rows.put("Owner", "ada", owner_row("ada"));
        rows.put("Vehicle", "v1", vehicle_of("ada"));
        assert_eq!(
            catalogue.validate(&[touched("Vehicle", "v1")], &mut rows),
            Ok(())
        );
    }

    #[test]
    fn a_reference_to_a_missing_row_is_dangling() {
        let catalogue = catalogue(false);
        let mut rows = Rows::default();
        rows.put("Vehicle", "v1", vehicle_of("ghost"));
        assert_eq!(
            catalogue.validate(&[touched("Vehicle", "v1")], &mut rows),
            Err(ReferenceError::Dangling)
        );
    }

    #[test]
    fn removing_a_referenced_row_is_restricted() {
        let catalogue = catalogue(false);
        let mut rows = Rows::default();
        rows.put("Vehicle", "v1", vehicle_of("ada"));
        // `ada` was deleted by the activation and `v1` still names it.
        assert_eq!(
            catalogue.validate(&[touched("Owner", "ada")], &mut rows),
            Err(ReferenceError::Dangling)
        );
    }

    #[test]
    fn removing_an_unreferenced_row_commits() {
        let catalogue = catalogue(false);
        let mut rows = Rows::default();
        rows.put("Owner", "bob", owner_row("bob"));
        rows.put("Vehicle", "v1", vehicle_of("bob"));
        assert_eq!(
            catalogue.validate(&[touched("Owner", "ada")], &mut rows),
            Ok(())
        );
    }

    #[test]
    fn rekeying_the_target_needs_its_dependents_updated_in_the_same_activation() {
        let catalogue = catalogue(false);
        let touched_keys = [
            touched("Owner", "ada"),
            touched("Owner", "ada2"),
            touched("Vehicle", "v1"),
        ];

        // Re-keyed alone, the dependent still names the old key.
        let mut rows = Rows::default();
        rows.put("Owner", "ada2", owner_row("ada2"));
        rows.put("Vehicle", "v1", vehicle_of("ada"));
        assert_eq!(
            catalogue.validate(&touched_keys, &mut rows),
            Err(ReferenceError::Dangling)
        );

        // Updated in the same activation, no invalid state is observable.
        rows.put("Vehicle", "v1", vehicle_of("ada2"));
        assert_eq!(catalogue.validate(&touched_keys, &mut rows), Ok(()));
    }

    #[test]
    fn only_the_final_state_of_a_touched_key_is_judged() {
        let catalogue = catalogue(false);
        let mut rows = Rows::default();
        rows.put("Owner", "bob", owner_row("bob"));
        // `v1` first named `ada`, which was then deleted, and now names `bob`.
        rows.put("Vehicle", "v1", vehicle_of("bob"));
        rows.remove("Owner", "ada");
        let touched_keys = [
            touched("Vehicle", "v1"),
            touched("Owner", "ada"),
            touched("Vehicle", "v1"),
        ];
        assert_eq!(catalogue.validate(&touched_keys, &mut rows), Ok(()));
    }

    #[test]
    fn a_deleted_and_reinserted_key_is_live() {
        let catalogue = catalogue(false);
        let mut rows = Rows::default();
        rows.put("Owner", "ada", owner_row("ada"));
        rows.put("Vehicle", "v1", vehicle_of("ada"));
        assert_eq!(
            catalogue.validate(&[touched("Owner", "ada")], &mut rows),
            Ok(())
        );
    }

    #[test]
    fn a_present_optional_reference_is_validated_too() {
        let catalogue = catalogue(true);
        let mut rows = Rows::default();
        rows.put("Owner", "ada", owner_row("ada"));
        rows.put(
            "Vehicle",
            "v1",
            record(vec![
                ("id", text("v1")),
                ("owner", reference("ada")),
                ("spare", option_present(reference("ghost"))),
            ]),
        );
        assert_eq!(
            catalogue.validate(&[touched("Vehicle", "v1")], &mut rows),
            Err(ReferenceError::Dangling)
        );
        rows.put(
            "Vehicle",
            "v1",
            record(vec![
                ("id", text("v1")),
                ("owner", reference("ada")),
                ("spare", Raw::Null),
            ]),
        );
        assert_eq!(
            catalogue.validate(&[touched("Vehicle", "v1")], &mut rows),
            Ok(())
        );
    }

    #[test]
    fn a_composite_key_travels_as_a_tuple() {
        let schemas = BTreeMap::from([
            (
                "Seat".to_owned(),
                schema(&[("row", Type::Text), ("col", Type::Text)], &["row", "col"]),
            ),
            (
                "Ticket".to_owned(),
                schema(
                    &[("id", Type::Text), ("seat", Type::Named("Seat".into()))],
                    &["id"],
                ),
            ),
        ]);
        let ids = BTreeMap::from([
            ("Seat".to_owned(), TableObjectId::new([3; 16])),
            ("Ticket".to_owned(), TableObjectId::new([4; 16])),
        ]);
        let catalogue = ReferenceCatalogue::new(DATABASE, &schemas, &ids)
            .unwrap()
            .unwrap();
        let seat = record(vec![("row", text("A")), ("col", text("1"))]);
        let ticket = record(vec![("id", text("t1")), ("seat", seat)]);
        let bound = catalogue.bind_row("Ticket", &ticket).unwrap().unwrap();
        let Raw::Map(entries) = &bound else {
            panic!("a bound row is a record");
        };
        let reference = &entries
            .iter()
            .find(|(name, _)| name == &text("seat"))
            .unwrap()
            .1;
        let identity = catalogue.identity(reference).expect("seat identity");
        assert_eq!(identity.table, "Seat");
        assert_eq!(identity.key, vec![text("A"), text("1")]);
        assert_eq!(catalogue.key_arity("Seat"), Some(2));
    }
}
