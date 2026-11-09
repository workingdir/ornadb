//! Pure semantic three-way merge primitives shared by evolution and storage.
//!
//! A merge always needs a common base. Stable object IDs identify schema
//! entities and row fields; opaque checkpoint positions are compared only for
//! equality. A returned conflict contains enough identity to report the
//! decision without choosing a branch or mutating repository state.
//! No type-specific value combination is installed in v1: same-field edits
//! merge only when the edits agree or one branch retains the base value.

use super::{
    CanonicalValue, Field, ObjectId, Schema, SchemaSide, Table, VersionFence, validate_schema,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaMergeConflict {
    Version,
    InvalidInput,
    DuplicateTableName {
        name: String,
        tables: Vec<ObjectId>,
    },
    DuplicateFieldName {
        table: ObjectId,
        field: ObjectId,
        name: String,
    },
    TableAddition {
        table: ObjectId,
    },
    TableDeletionAndEdit {
        table: ObjectId,
    },
    TableProperty {
        table: ObjectId,
        property: &'static str,
    },
    FieldAddition {
        table: ObjectId,
        field: ObjectId,
    },
    FieldDeletionAndEdit {
        table: ObjectId,
        field: ObjectId,
    },
    FieldProperty {
        table: ObjectId,
        field: ObjectId,
        property: &'static str,
    },
    InvalidMergedSchema,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaMergeFailure {
    /// At most `max_conflicts` materialized conflict values.
    pub conflicts: Vec<SchemaMergeConflict>,
    /// Exact until the supplied conflict budget is exceeded; then it is the
    /// first proven lower bound above that budget.
    pub conflicts_lower_bound: usize,
}

struct SchemaConflictCollector {
    conflicts: Vec<SchemaMergeConflict>,
    count: usize,
    limit: usize,
}

impl SchemaConflictCollector {
    fn new(limit: usize) -> Self {
        Self {
            conflicts: Vec::new(),
            count: 0,
            limit,
        }
    }

    fn push(&mut self, conflict: SchemaMergeConflict) {
        if self.exceeded() {
            return;
        }
        self.count = self.count.saturating_add(1);
        if self.conflicts.len() < self.limit {
            self.conflicts.push(conflict);
        }
    }

    fn exceeded(&self) -> bool {
        self.count > self.limit
    }

    fn has_conflicts(&self) -> bool {
        self.count != 0
    }

    fn failure(mut self) -> SchemaMergeFailure {
        self.conflicts
            .sort_by_cached_key(|conflict| format!("{conflict:?}"));
        self.conflicts.dedup();
        SchemaMergeFailure {
            conflicts: self.conflicts,
            conflicts_lower_bound: self.count,
        }
    }
}

impl SchemaMergeConflict {
    pub fn affected_tables(&self) -> Vec<ObjectId> {
        match self {
            Self::DuplicateTableName { tables, .. } => tables.clone(),
            Self::DuplicateFieldName { table, .. }
            | Self::TableAddition { table }
            | Self::TableDeletionAndEdit { table }
            | Self::TableProperty { table, .. }
            | Self::FieldAddition { table, .. }
            | Self::FieldDeletionAndEdit { table, .. }
            | Self::FieldProperty { table, .. } => vec![*table],
            Self::Version | Self::InvalidInput | Self::InvalidMergedSchema => Vec::new(),
        }
    }
}

/// Three-way pick of one value: when both branches agree, or only one of them
/// changed the base, the surviving side wins. `None` means both branches
/// changed the value differently, which is a conflict.
fn three_way_pick<'a, T: PartialEq + ?Sized>(base: &T, left: &'a T, right: &'a T) -> Option<&'a T> {
    if left == right {
        Some(left)
    } else if left == base {
        Some(right)
    } else if right == base {
        Some(left)
    } else {
        None
    }
}

/// Picks one schema property, recording the conflict `conflict` builds when
/// both branches changed it differently. Retaining the base value only makes
/// candidate construction total; conflicted candidates are never returned.
fn resolve<T: Clone + PartialEq>(
    base: &T,
    left: &T,
    right: &T,
    conflict: impl FnOnce() -> SchemaMergeConflict,
    conflicts: &mut SchemaConflictCollector,
) -> T {
    match three_way_pick(base, left, right) {
        Some(value) => value.clone(),
        None => {
            conflicts.push(conflict());
            base.clone()
        }
    }
}

/// How the presence of one stable identity across base, left and right
/// decides its outcome before any contents are compared.
enum Presence<'a, T> {
    /// The identity does not survive: it never existed, or one branch deleted
    /// it while the other left it unchanged.
    Absent,
    /// Presence alone settles the outcome and this entry survives.
    Retain(&'a T),
    /// Present on all three sides: the contents must be merged.
    Merge {
        base: &'a T,
        left: &'a T,
        right: &'a T,
    },
    /// Both branches added different entries under one identity.
    AdditionConflict,
    /// One branch deleted an entry the other edited.
    DeletionEditConflict,
}

fn classify_presence<'a, T: PartialEq>(
    base: Option<&'a T>,
    left: Option<&'a T>,
    right: Option<&'a T>,
) -> Presence<'a, T> {
    match (base, left, right) {
        (None, None, None) | (Some(_), None, None) => Presence::Absent,
        (None, Some(only), None) | (None, None, Some(only)) => Presence::Retain(only),
        (None, Some(left), Some(right)) if left == right => Presence::Retain(left),
        (None, Some(_), Some(_)) => Presence::AdditionConflict,
        (Some(base), Some(left), Some(right)) => Presence::Merge { base, left, right },
        (Some(base), Some(left), None) if left == base => Presence::Absent,
        (Some(base), None, Some(right)) if right == base => Presence::Absent,
        (Some(_), Some(_), None) | (Some(_), None, Some(_)) => Presence::DeletionEditConflict,
    }
}

fn unique_schema(schema: &Schema) -> bool {
    let mut table_ids = BTreeSet::new();
    schema.tables.iter().all(|table| {
        let mut field_ids = BTreeSet::new();
        table_ids.insert(table.id) && table.fields.iter().all(|field| field_ids.insert(field.id))
    })
}

/// Three-way merge schema metadata by stable object identity.
///
/// Different optional fields introduced on separate branches are retained.
/// Different edits to one property, including a column type, are reported as
/// schema conflicts. Required-field compatibility remains the migration
/// planner's responsibility; this function combines declarations only.
pub fn merge_schema(
    base: &Schema,
    left: &Schema,
    right: &Schema,
) -> Result<Schema, Vec<SchemaMergeConflict>> {
    merge_schema_bounded(base, left, right, usize::MAX).map_err(|failure| failure.conflicts)
}

/// Bounded form used by the storage operation so a large schema conflict does
/// not materialize an unbounded diagnostic vector.
pub fn merge_schema_bounded(
    base: &Schema,
    left: &Schema,
    right: &Schema,
    max_conflicts: usize,
) -> Result<Schema, SchemaMergeFailure> {
    let mut conflicts = SchemaConflictCollector::new(max_conflicts);
    if [base, left, right].into_iter().any(|schema| {
        !unique_schema(schema)
            || validate_schema(schema, SchemaSide::From, VersionFence::V1).is_err()
    }) {
        conflicts.push(SchemaMergeConflict::InvalidInput);
        return Err(conflicts.failure());
    }

    let Some(&version) = three_way_pick(&base.version, &left.version, &right.version) else {
        conflicts.push(SchemaMergeConflict::Version);
        return Err(conflicts.failure());
    };
    let base_tables = tables_by_id(base);
    let left_tables = tables_by_id(left);
    let right_tables = tables_by_id(right);
    let ids: BTreeSet<_> = base_tables
        .keys()
        .chain(left_tables.keys())
        .chain(right_tables.keys())
        .copied()
        .collect();
    let mut tables = Vec::new();
    for id in ids {
        match classify_presence(
            base_tables.get(&id).copied(),
            left_tables.get(&id).copied(),
            right_tables.get(&id).copied(),
        ) {
            Presence::Absent => {}
            Presence::Retain(table) => tables.push(table.clone()),
            Presence::Merge { base, left, right } => {
                tables.push(merge_table(base, left, right, &mut conflicts));
            }
            Presence::AdditionConflict => {
                conflicts.push(SchemaMergeConflict::TableAddition { table: id });
            }
            Presence::DeletionEditConflict => {
                conflicts.push(SchemaMergeConflict::TableDeletionAndEdit { table: id });
            }
        }
        if conflicts.exceeded() {
            return Err(conflicts.failure());
        }
    }

    let mut table_names = BTreeMap::new();
    for table in &tables {
        if let Some(previous) = table_names.insert(table.name.clone(), table.id) {
            conflicts.push(SchemaMergeConflict::DuplicateTableName {
                name: table.name.clone(),
                tables: vec![previous, table.id],
            });
        }
        let mut field_names = BTreeSet::new();
        for field in &table.fields {
            if !field_names.insert(field.name.clone()) {
                conflicts.push(SchemaMergeConflict::DuplicateFieldName {
                    table: table.id,
                    field: field.id,
                    name: field.name.clone(),
                });
            }
        }
        if conflicts.exceeded() {
            return Err(conflicts.failure());
        }
    }
    let merged = Schema { version, tables };
    if validate_schema(&merged, SchemaSide::To, VersionFence::V1).is_err() {
        conflicts.push(SchemaMergeConflict::InvalidMergedSchema);
    }
    if !conflicts.has_conflicts() {
        Ok(merged)
    } else {
        Err(conflicts.failure())
    }
}

fn tables_by_id(schema: &Schema) -> BTreeMap<ObjectId, &Table> {
    schema
        .tables
        .iter()
        .map(|table| (table.id, table))
        .collect()
}

fn merge_table(
    base: &Table,
    left: &Table,
    right: &Table,
    conflicts: &mut SchemaConflictCollector,
) -> Table {
    let table_conflict = |property: &'static str| {
        move || SchemaMergeConflict::TableProperty {
            table: base.id,
            property,
        }
    };
    let name = resolve(
        &base.name,
        &left.name,
        &right.name,
        table_conflict("name"),
        conflicts,
    );
    let explicit_key = resolve(
        &base.explicit_key,
        &left.explicit_key,
        &right.explicit_key,
        table_conflict("explicit_key"),
        conflicts,
    );
    if conflicts.exceeded() {
        return Table {
            id: base.id,
            name,
            explicit_key,
            fields: Vec::new(),
        };
    }
    let base_fields = fields_by_id(base);
    let left_fields = fields_by_id(left);
    let right_fields = fields_by_id(right);
    let ids: BTreeSet<_> = base_fields
        .keys()
        .chain(left_fields.keys())
        .chain(right_fields.keys())
        .copied()
        .collect();
    let mut fields = Vec::new();
    for id in ids {
        match classify_presence(
            base_fields.get(&id).copied(),
            left_fields.get(&id).copied(),
            right_fields.get(&id).copied(),
        ) {
            Presence::Absent => {}
            Presence::Retain(field) => fields.push(field.clone()),
            Presence::Merge {
                base: base_field,
                left: left_field,
                right: right_field,
            } => {
                fields.push(merge_field(
                    base.id,
                    base_field,
                    left_field,
                    right_field,
                    conflicts,
                ));
            }
            Presence::AdditionConflict => {
                conflicts.push(SchemaMergeConflict::FieldAddition {
                    table: base.id,
                    field: id,
                });
            }
            Presence::DeletionEditConflict => {
                conflicts.push(SchemaMergeConflict::FieldDeletionAndEdit {
                    table: base.id,
                    field: id,
                });
            }
        }
        if conflicts.exceeded() {
            break;
        }
    }
    Table {
        id: base.id,
        name,
        explicit_key,
        fields,
    }
}

fn fields_by_id(table: &Table) -> BTreeMap<ObjectId, &Field> {
    table.fields.iter().map(|field| (field.id, field)).collect()
}

fn merge_field(
    table: ObjectId,
    base: &Field,
    left: &Field,
    right: &Field,
    conflicts: &mut SchemaConflictCollector,
) -> Field {
    let field_conflict = |property: &'static str| {
        move || SchemaMergeConflict::FieldProperty {
            table,
            field: base.id,
            property,
        }
    };
    Field {
        id: base.id,
        name: resolve(
            &base.name,
            &left.name,
            &right.name,
            field_conflict("name"),
            conflicts,
        ),
        ty: resolve(
            &base.ty,
            &left.ty,
            &right.ty,
            field_conflict("type"),
            conflicts,
        ),
        role: resolve(
            &base.role,
            &left.role,
            &right.role,
            field_conflict("role"),
            conflicts,
        ),
        optional: resolve(
            &base.optional,
            &left.optional,
            &right.optional,
            field_conflict("optional"),
            conflicts,
        ),
        introduction_fallback: resolve(
            &base.introduction_fallback,
            &left.introduction_fallback,
            &right.introduction_fallback,
            field_conflict("introduction_fallback"),
            conflicts,
        ),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowKeyKind {
    Explicit,
    Automatic,
}

/// A logical row with a stable table/key identity and stable field identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyedRow {
    pub table: ObjectId,
    pub key: CanonicalValue,
    pub key_kind: RowKeyKind,
    pub fields: BTreeMap<ObjectId, CanonicalValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowMergeConflict {
    Identity {
        table: ObjectId,
        key: CanonicalValue,
    },
    DeleteAndEdit {
        table: ObjectId,
        key: CanonicalValue,
    },
    KeyCollision {
        table: ObjectId,
        key: CanonicalValue,
    },
    Fields {
        table: ObjectId,
        key: CanonicalValue,
        fields: Vec<ObjectId>,
    },
    AutomaticKeyCollision {
        table: ObjectId,
        key: CanonicalValue,
    },
}

/// Availability of one key in a snapshot supplied to a row merge.
///
/// `Absent` is a complete observation. Use `Pruned` when history needed to
/// establish the row state is unavailable; treating that as `Absent` could
/// manufacture a deletion tombstone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowSnapshotState<'a> {
    Present(&'a KeyedRow),
    Absent,
    /// A retained storage deletion marker. It is logically absent for merge
    /// comparison; the output needs a marker only when the common base had a
    /// live row that must remain deleted. Its identity must match the key
    /// being reconciled; retaining it here prevents a marker for one key from
    /// being mistaken for an unrelated missing row.
    Tombstone {
        table: ObjectId,
        key: &'a CanonicalValue,
    },
    Pruned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowSnapshotSide {
    /// The common ancestor snapshot.
    Base,
    /// The first branch being merged.
    Left,
    /// The second branch being merged.
    Right,
}

/// Logical operation selected for a single key by three-way reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowMergeOperation {
    Upsert(KeyedRow),
    /// A versioned deletion of a key visible in the common base. This
    /// preserves the deletion effect without authorizing Git-history pruning.
    Tombstone {
        table: ObjectId,
        key: CanonicalValue,
    },
    /// The key is absent from a complete base and both complete branches; no
    /// deletion record is needed for a row that never existed in the base.
    Absent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowSnapshotMergeError {
    PrunedInput { sides: Vec<RowSnapshotSide> },
    Conflict(RowMergeConflict),
}

/// Canonical order of row identities: the table first, then the canonical
/// encoding of the key. Row keys are validated canonical values, so encoding
/// them cannot fail.
fn compare_row_identity(
    left: (ObjectId, &CanonicalValue),
    right: (ObjectId, &CanonicalValue),
) -> Ordering {
    let encoded = |key: &CanonicalValue| key.encode().expect("validated row keys remain encodable");
    left.0
        .cmp(&right.0)
        .then_with(|| encoded(left.1).cmp(&encoded(right.1)))
}

/// Reconcile one key while retaining the distinction between known absence
/// and unavailable history. Manifest digest equality can still bypass this
/// function for an unchanged subtree; if a pruned side must be inspected, the
/// caller gets an error rather than a fabricated delete.
pub fn merge_keyed_row_states(
    base: RowSnapshotState<'_>,
    left: RowSnapshotState<'_>,
    right: RowSnapshotState<'_>,
) -> Result<RowMergeOperation, RowSnapshotMergeError> {
    // Conflict sequencing is conservative: unavailable history wins first,
    // then key identity is checked before interpreting deletion or field
    // edits. The reference requires typed conflicts but does not order them;
    // this avoids reporting actionable edits for an unknown or wrong key.
    let mut pruned_sides = Vec::new();
    // Keep diagnostics stable in merge-argument order even when callers
    // discovered unavailable ranges in a different physical order.
    for (side, state) in [
        (RowSnapshotSide::Base, base),
        (RowSnapshotSide::Left, left),
        (RowSnapshotSide::Right, right),
    ] {
        if matches!(state, RowSnapshotState::Pruned) {
            pruned_sides.push(side);
        }
    }
    if !pruned_sides.is_empty() {
        return Err(RowSnapshotMergeError::PrunedInput {
            sides: pruned_sides,
        });
    }

    let identity = |state| match state {
        RowSnapshotState::Present(row) => Some((row.table, &row.key)),
        RowSnapshotState::Tombstone { table, key } => Some((table, key)),
        RowSnapshotState::Absent | RowSnapshotState::Pruned => None,
    };
    let identities = [identity(base), identity(left), identity(right)];
    let anchor = identities
        .iter()
        .flatten()
        .min_by(|left, right| compare_row_identity(**left, **right))
        .copied();
    // A supplied base is authoritative for conflict reporting. Without one,
    // use canonical identity order so swapping branch arguments cannot change
    // the identity carried by the error. The reference specifies conflicts
    // but leaves their payload anchor unspecified.
    let anchor = identity(base).or(anchor);
    if let Some((table, key)) = anchor
        && identities
            .iter()
            .flatten()
            .any(|(other_table, other_key)| *other_table != table || *other_key != key)
    {
        return Err(RowSnapshotMergeError::Conflict(
            RowMergeConflict::Identity {
                table,
                key: key.clone(),
            },
        ));
    }

    let row = |state| match state {
        RowSnapshotState::Present(row) => Some(row),
        RowSnapshotState::Absent
        | RowSnapshotState::Tombstone { .. }
        | RowSnapshotState::Pruned => None,
    };
    let base_row = row(base);
    let merged = merge_keyed_row(base_row, row(left), row(right))
        .map_err(RowSnapshotMergeError::Conflict)?;
    match merged {
        Some(row) => Ok(RowMergeOperation::Upsert(row)),
        None => match base_row {
            Some(row) => Ok(RowMergeOperation::Tombstone {
                table: row.table,
                key: row.key.clone(),
            }),
            None => Ok(RowMergeOperation::Absent),
        },
    }
}

/// Merge one logical key. This is a pure value operation; storage supplies
/// rows in key order and owns resource limits and the no-partial-write rule.
pub fn merge_keyed_row(
    base: Option<&KeyedRow>,
    left: Option<&KeyedRow>,
    right: Option<&KeyedRow>,
) -> Result<Option<KeyedRow>, RowMergeConflict> {
    let stable_branch_identity = [left, right].into_iter().flatten().min_by(|left, right| {
        compare_row_identity((left.table, &left.key), (right.table, &right.key))
    });
    // Keep a supplied base as the diagnostic anchor; where there is no base,
    // canonical table/key order makes identity conflicts invariant under
    // swapping the two branches. ORNA-MERGE leaves this payload choice open.
    let exemplar = base.or(stable_branch_identity);
    let Some(exemplar) = exemplar else {
        return Ok(None);
    };
    let identity_matches = |row: &KeyedRow| row.table == exemplar.table && row.key == exemplar.key;
    if [base, left, right]
        .into_iter()
        .flatten()
        .any(|row| !identity_matches(row))
    {
        return Err(RowMergeConflict::Identity {
            table: exemplar.table,
            key: exemplar.key.clone(),
        });
    }
    if let (Some(base), Some(left), Some(right)) = (base, left, right)
        && (base.key_kind != left.key_kind || base.key_kind != right.key_kind)
    {
        return Err(RowMergeConflict::Identity {
            table: base.table,
            key: base.key.clone(),
        });
    }
    match (base, left, right) {
        (None, None, _) | (None, _, None) => Ok(left.or(right).cloned()),
        // Generated IDs are identities allocated independently by branch
        // histories. Even equal payloads do not prove these are one insert;
        // keep the collision visible instead of silently renumbering a row.
        (None, Some(left), Some(right))
            if left.key_kind == RowKeyKind::Automatic
                || right.key_kind == RowKeyKind::Automatic =>
        {
            Err(RowMergeConflict::AutomaticKeyCollision {
                table: left.table,
                key: left.key.clone(),
            })
        }
        (None, Some(left), Some(right)) if left == right => Ok(Some(left.clone())),
        (None, Some(left), Some(_right)) => Err(RowMergeConflict::KeyCollision {
            table: left.table,
            key: left.key.clone(),
        }),
        (Some(_), None, None) => Ok(None),
        (Some(base), Some(left), None) if left == base => Ok(None),
        (Some(base), None, Some(right)) if right == base => Ok(None),
        (Some(base), None, Some(_)) | (Some(base), Some(_), None) => {
            Err(RowMergeConflict::DeleteAndEdit {
                table: base.table,
                key: base.key.clone(),
            })
        }
        (Some(base), Some(left), Some(right)) => {
            if left.key_kind != base.key_kind || right.key_kind != base.key_kind {
                return Err(RowMergeConflict::Identity {
                    table: base.table,
                    key: base.key.clone(),
                });
            }
            // Stable field IDs define both merge order and conflict detail
            // order, independent of row or source-fixture field ordering.
            let ids: BTreeSet<_> = base
                .fields
                .keys()
                .chain(left.fields.keys())
                .chain(right.fields.keys())
                .copied()
                .collect();
            let mut fields = BTreeMap::new();
            let mut conflicts = Vec::new();
            for id in ids {
                match three_way_pick(
                    &base.fields.get(&id),
                    &left.fields.get(&id),
                    &right.fields.get(&id),
                ) {
                    Some(Some(value)) => {
                        fields.insert(id, CanonicalValue::clone(value));
                    }
                    Some(None) => {}
                    None => conflicts.push(id),
                }
            }
            if conflicts.is_empty() {
                Ok(Some(KeyedRow {
                    table: base.table,
                    key: base.key.clone(),
                    key_kind: base.key_kind,
                    fields,
                }))
            } else {
                Err(RowMergeConflict::Fields {
                    table: base.table,
                    key: base.key.clone(),
                    fields: conflicts,
                })
            }
        }
    }
}

/// A checkpoint generation is kept opaque: only equality and base retention
/// can prove compatibility. The position bytes are never sorted or compared
/// lexicographically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointGeneration {
    pub generation: u64,
    pub position: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointMergeConflict {
    pub base: Option<CheckpointGeneration>,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
}

/// Merge checkpoint state only when one side is unchanged or both agree.
/// Generations and positions are opaque state: there is no safe “take the
/// newest” ordering for two independently advanced values.
pub fn merge_checkpoint_generation(
    base: Option<&CheckpointGeneration>,
    left: Option<&CheckpointGeneration>,
    right: Option<&CheckpointGeneration>,
) -> Result<Option<CheckpointGeneration>, CheckpointMergeConflict> {
    // The outer Option represents checkpoint existence; `position: None` is
    // still a present opaque checkpoint (for example, after a cursor reset).
    // Keep that state distinct from deletion so delete-versus-change conflicts.
    three_way_pick(&base, &left, &right)
        .map(|picked| (*picked).cloned())
        .ok_or_else(|| CheckpointMergeConflict {
            base: base.cloned(),
            left: left.cloned(),
            right: right.cloned(),
        })
}

/// Stable checkpoint row identity copied into portable snapshot metadata.
/// `consumer_identity` is its canonical identity encoding; `None` and an
/// empty-string partition remain distinct natural keys.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct CheckpointIdentity {
    pub consumer_identity: Vec<u8>,
    pub source_identity: String,
    pub partition: Option<String>,
}

impl CheckpointIdentity {
    pub fn new(
        consumer_identity: Vec<u8>,
        source_identity: impl Into<String>,
        partition: Option<String>,
    ) -> Self {
        Self {
            consumer_identity,
            source_identity: source_identity.into(),
            partition,
        }
    }
}

impl fmt::Debug for CheckpointIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointIdentity")
            .field("consumer_identity", &"<opaque>")
            .field("source_identity", &self.source_identity)
            .field("partition", &self.partition)
            .finish()
    }
}

/// Failure while constructing a portable, canonically encoded position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointPositionError {
    InvalidFormat,
    InvalidCanonicalPayload,
}

impl fmt::Display for CheckpointPositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFormat => "checkpoint position format must be non-empty and versioned",
            Self::InvalidCanonicalPayload => "checkpoint position payload is not canonical OVB-1",
        })
    }
}

impl std::error::Error for CheckpointPositionError {}

/// A typed provider cursor. Payload bytes are a canonical OVB-1 value (usually
/// a byte string); generic merge code compares the full value and format tuple
/// only. It never sorts, decodes, or guesses cursor progress.
#[derive(Clone, Eq, PartialEq)]
pub struct CheckpointPosition {
    format: String,
    format_version: u32,
    canonical_payload: Vec<u8>,
}

impl CheckpointPosition {
    pub fn new(
        format: impl Into<String>,
        format_version: u32,
        canonical_payload: Vec<u8>,
    ) -> Result<Self, CheckpointPositionError> {
        let format = format.into();
        // The provider owns format compatibility and migration. This portable
        // layer requires an explicit nonzero version and compares it exactly.
        if format.is_empty() || format_version == 0 {
            return Err(CheckpointPositionError::InvalidFormat);
        }
        CanonicalValue::decode(&canonical_payload)
            .map_err(|_| CheckpointPositionError::InvalidCanonicalPayload)?;
        Ok(Self {
            format,
            format_version,
            canonical_payload,
        })
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub const fn format_version(&self) -> u32 {
        self.format_version
    }

    pub fn canonical_payload(&self) -> &[u8] {
        &self.canonical_payload
    }
}

impl fmt::Debug for CheckpointPosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointPosition")
            .field("format", &self.format)
            .field("format_version", &self.format_version)
            .field("canonical_payload", &"<opaque>")
            .finish()
    }
}

/// One selected snapshot's portable checkpoint relation, ordered by its
/// natural composite identity rather than by provider position.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheckpointSnapshot {
    checkpoints: BTreeMap<CheckpointIdentity, CheckpointPosition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointSnapshotError {
    DuplicateIdentity,
}

impl fmt::Display for CheckpointSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("checkpoint snapshot repeats a natural identity")
    }
}

impl std::error::Error for CheckpointSnapshotError {}

impl CheckpointSnapshot {
    pub fn new(
        checkpoints: impl IntoIterator<Item = (CheckpointIdentity, CheckpointPosition)>,
    ) -> Result<Self, CheckpointSnapshotError> {
        let mut indexed = BTreeMap::new();
        for (identity, position) in checkpoints {
            if indexed.insert(identity, position).is_some() {
                return Err(CheckpointSnapshotError::DuplicateIdentity);
            }
        }
        Ok(Self {
            checkpoints: indexed,
        })
    }

    pub fn checkpoints(&self) -> &BTreeMap<CheckpointIdentity, CheckpointPosition> {
        &self.checkpoints
    }

    pub fn get(&self, identity: &CheckpointIdentity) -> Option<&CheckpointPosition> {
        self.checkpoints.get(identity)
    }
}

/// Snapshot references attached to a user-facing `sys.CheckpointConflict`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointSnapshotRefs {
    pub base: Vec<u8>,
    pub left: Vec<u8>,
    pub right: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointConflictReason {
    DivergentPosition,
    IncompatibleFormat,
    DeleteUpdate,
}

impl CheckpointConflictReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DivergentPosition => "divergent_position",
            Self::IncompatibleFormat => "incompatible_format",
            Self::DeleteUpdate => "delete_update",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointSnapshotMergeConflict {
    pub identity: CheckpointIdentity,
    pub base: Option<CheckpointPosition>,
    pub left: Option<CheckpointPosition>,
    pub right: Option<CheckpointPosition>,
    pub left_snapshot: Vec<u8>,
    pub right_snapshot: Vec<u8>,
    pub reason: CheckpointConflictReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointSnapshotMergeFailure {
    /// At most the caller's conflict budget in detail records.
    pub conflicts: Vec<CheckpointSnapshotMergeConflict>,
    /// Exact if within budget; otherwise the first proven lower bound over it.
    pub conflicts_lower_bound: usize,
}

fn checkpoint_conflict_reason(
    base: Option<&CheckpointPosition>,
    left: Option<&CheckpointPosition>,
    right: Option<&CheckpointPosition>,
) -> CheckpointConflictReason {
    // A delete-versus-update is the primary intent conflict even when the
    // surviving edit also changes cursor format. Keep that diagnosis stable;
    // format compatibility is considered only when both branches retain a
    // checkpoint value.
    if base.is_some() && left.is_none() != right.is_none() {
        return CheckpointConflictReason::DeleteUpdate;
    }
    let mut expected_format = None;
    for position in [base, left, right].into_iter().flatten() {
        let format = (position.format(), position.format_version());
        if expected_format.is_some_and(|expected| expected != format) {
            return CheckpointConflictReason::IncompatibleFormat;
        }
        expected_format = Some(format);
    }
    CheckpointConflictReason::DivergentPosition
}

/// Reconciles the complete checkpoint relation in three portable snapshots.
/// Disjoint checkpoint identities merge independently; equal positions and a
/// one-sided change are retained. Divergent cursors remain typed conflicts.
pub fn merge_checkpoint_snapshots(
    base: &CheckpointSnapshot,
    left: &CheckpointSnapshot,
    right: &CheckpointSnapshot,
    snapshots: &CheckpointSnapshotRefs,
) -> Result<CheckpointSnapshot, Vec<CheckpointSnapshotMergeConflict>> {
    merge_checkpoint_snapshots_bounded(base, left, right, snapshots, usize::MAX)
        .map_err(|failure| failure.conflicts)
}

/// Bounded snapshot reconciliation used by adapters that need to cap conflict
/// materialization before projecting conflicts into system values.
pub fn merge_checkpoint_snapshots_bounded(
    base: &CheckpointSnapshot,
    left: &CheckpointSnapshot,
    right: &CheckpointSnapshot,
    snapshots: &CheckpointSnapshotRefs,
    max_conflicts: usize,
) -> Result<CheckpointSnapshot, CheckpointSnapshotMergeFailure> {
    let identities: BTreeSet<_> = base
        .checkpoints
        .keys()
        .chain(left.checkpoints.keys())
        .chain(right.checkpoints.keys())
        .cloned()
        .collect();
    let mut merged = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut conflict_count = 0usize;
    for identity in identities {
        let base_position = base.checkpoints.get(&identity);
        let left_position = left.checkpoints.get(&identity);
        let right_position = right.checkpoints.get(&identity);
        // A unilateral format change is explicit in the changed snapshot;
        // validating its provider-specific migration remains adapter-owned.
        match three_way_pick(&base_position, &left_position, &right_position) {
            Some(Some(position)) => {
                merged.insert(identity, CheckpointPosition::clone(position));
            }
            Some(None) => {}
            None => {
                let reason =
                    checkpoint_conflict_reason(base_position, left_position, right_position);
                conflict_count = conflict_count.saturating_add(1);
                if conflicts.len() < max_conflicts {
                    conflicts.push(CheckpointSnapshotMergeConflict {
                        identity,
                        base: base_position.cloned(),
                        left: left_position.cloned(),
                        right: right_position.cloned(),
                        left_snapshot: snapshots.left.clone(),
                        right_snapshot: snapshots.right.clone(),
                        reason,
                    });
                }
                if conflict_count > max_conflicts {
                    return Err(CheckpointSnapshotMergeFailure {
                        conflicts,
                        conflicts_lower_bound: conflict_count,
                    });
                }
            }
        }
    }
    if conflict_count != 0 {
        conflicts.sort_by(|left, right| left.identity.cmp(&right.identity));
        return Err(CheckpointSnapshotMergeFailure {
            conflicts,
            conflicts_lower_bound: conflict_count,
        });
    }
    Ok(CheckpointSnapshot {
        checkpoints: merged,
    })
}
