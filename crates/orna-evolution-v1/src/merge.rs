//! Pure semantic three-way merge primitives shared by evolution and storage.
//!
//! A merge always needs a common base. Stable object IDs identify schema
//! entities and row fields; opaque checkpoint positions are compared only for
//! equality. A returned conflict contains enough identity to report the
//! decision without choosing a branch or mutating repository state.
//! No type-specific value combination is installed in v1: same-field edits
//! merge only when the edits agree or one branch retains the base value.

use super::{
    CanonicalValue, Field, ObjectId, Schema, Table,
    VersionFence, validate_schema, SchemaSide,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchemaMergeConflict {
    Version,
    InvalidInput,
    DuplicateTableName { name: String, tables: Vec<ObjectId> },
    DuplicateFieldName { table: ObjectId, field: ObjectId, name: String },
    TableAddition { table: ObjectId },
    TableDeletionAndEdit { table: ObjectId },
    TableProperty { table: ObjectId, property: &'static str },
    FieldAddition { table: ObjectId, field: ObjectId },
    FieldDeletionAndEdit { table: ObjectId, field: ObjectId },
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
        Self { conflicts: Vec::new(), count: 0, limit }
    }

    fn push(&mut self, conflict: SchemaMergeConflict) {
        if self.exceeded() { return; }
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
        self.conflicts.sort_by_key(|conflict| format!("{conflict:?}"));
        self.conflicts.dedup();
        SchemaMergeFailure { conflicts: self.conflicts, conflicts_lower_bound: self.count }
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

fn choose<T: Clone + Eq>(base: &T, left: &T, right: &T) -> Result<T, ()> {
    if left == right {
        Ok(left.clone())
    } else if left == base {
        Ok(right.clone())
    } else if right == base {
        Ok(left.clone())
    } else {
        Err(())
    }
}

fn merge_optional<T: Clone + Eq>(base: Option<&T>, left: Option<&T>, right: Option<&T>) -> Option<T> {
    if left == right {
        left.cloned()
    } else if left == base {
        right.cloned()
    } else if right == base {
        left.cloned()
    } else {
        // A caller must record the delete/edit conflict before using this
        // fallback. Retaining the base value only makes candidate construction
        // total; conflicted candidates are never returned to the caller.
        base.cloned()
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
pub fn merge_schema(base: &Schema, left: &Schema, right: &Schema) -> Result<Schema, Vec<SchemaMergeConflict>> {
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
        !unique_schema(schema) || validate_schema(schema, SchemaSide::From, VersionFence::V1).is_err()
    }) {
        conflicts.push(SchemaMergeConflict::InvalidInput);
        return Err(conflicts.failure());
    }

    let version = match choose(&base.version, &left.version, &right.version) {
        Ok(version) => version,
        Err(()) => {
            conflicts.push(SchemaMergeConflict::Version);
            return Err(conflicts.failure());
        }
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
        let b = base_tables.get(&id).copied();
        let l = left_tables.get(&id).copied();
        let r = right_tables.get(&id).copied();
        match (b, l, r) {
            (None, None, None) => {}
            (None, Some(table), None) | (None, None, Some(table)) => tables.push(table.clone()),
            (None, Some(l), Some(r)) if l == r => tables.push(l.clone()),
            (None, Some(_), Some(_)) => conflicts.push(SchemaMergeConflict::TableAddition { table: id }),
            (Some(_), None, None) => {}
            (Some(base), Some(left), Some(right)) => {
                if let Some(table) = merge_table(base, left, right, &mut conflicts) {
                    tables.push(table);
                }
            }
            (Some(base), Some(left), None) if left == base => {}
            (Some(base), None, Some(right)) if right == base => {}
            (Some(_), Some(_), None) | (Some(_), None, Some(_)) => {
                conflicts.push(SchemaMergeConflict::TableDeletionAndEdit { table: id })
            }
        }
        if conflicts.exceeded() { return Err(conflicts.failure()); }
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
        if conflicts.exceeded() { return Err(conflicts.failure()); }
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
    schema.tables.iter().map(|table| (table.id, table)).collect()
}

fn merge_table(
    base: &Table,
    left: &Table,
    right: &Table,
    conflicts: &mut SchemaConflictCollector,
) -> Option<Table> {
    let name = match choose(&base.name, &left.name, &right.name) {
        Ok(value) => value,
        Err(()) => {
            conflicts.push(SchemaMergeConflict::TableProperty {
                table: base.id,
                property: "name",
            });
            base.name.clone()
        }
    };
    let explicit_key = match choose(&base.explicit_key, &left.explicit_key, &right.explicit_key) {
        Ok(value) => value,
        Err(()) => {
            conflicts.push(SchemaMergeConflict::TableProperty {
                table: base.id,
                property: "explicit_key",
            });
            base.explicit_key
        }
    };
    if conflicts.exceeded() {
        return Some(Table { id: base.id, name, explicit_key, fields: Vec::new() });
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
        let b = base_fields.get(&id).copied();
        let l = left_fields.get(&id).copied();
        let r = right_fields.get(&id).copied();
        match (b, l, r) {
            (None, None, None) => {}
            (None, Some(field), None) | (None, None, Some(field)) => fields.push(field.clone()),
            (None, Some(left), Some(right)) if left == right => fields.push(left.clone()),
            (None, Some(_), Some(_)) => conflicts.push(SchemaMergeConflict::FieldAddition {
                table: base.id,
                field: id,
            }),
            (Some(_), None, None) => {}
            (Some(base_field), Some(left_field), Some(right_field)) => {
                if let Some(field) = merge_field(base.id, base_field, left_field, right_field, conflicts) {
                    fields.push(field);
                }
            }
            (Some(base_field), Some(left_field), None) if left_field == base_field => {}
            (Some(base_field), None, Some(right_field)) if right_field == base_field => {}
            (Some(_), Some(_), None) | (Some(_), None, Some(_)) => {
                conflicts.push(SchemaMergeConflict::FieldDeletionAndEdit {
                    table: base.id,
                    field: id,
                })
            }
        }
        if conflicts.exceeded() { break; }
    }
    Some(Table { id: base.id, name, explicit_key, fields })
}

fn fields_by_id(table: &Table) -> BTreeMap<ObjectId, &Field> {
    table.fields.iter().map(|field| (field.id, field)).collect()
}

fn property<T: Clone + Eq>(
    table: ObjectId,
    field: ObjectId,
    name: &'static str,
    base: &T,
    left: &T,
    right: &T,
    conflicts: &mut SchemaConflictCollector,
) -> T {
    match choose(base, left, right) {
        Ok(value) => value,
        Err(()) => {
            conflicts.push(SchemaMergeConflict::FieldProperty { table, field, property: name });
            base.clone()
        }
    }
}

fn merge_field(
    table: ObjectId,
    base: &Field,
    left: &Field,
    right: &Field,
    conflicts: &mut SchemaConflictCollector,
) -> Option<Field> {
    Some(Field {
        id: base.id,
        name: property(table, base.id, "name", &base.name, &left.name, &right.name, conflicts),
        ty: property(table, base.id, "type", &base.ty, &left.ty, &right.ty, conflicts),
        role: property(table, base.id, "role", &base.role, &left.role, &right.role, conflicts),
        optional: property(table, base.id, "optional", &base.optional, &left.optional, &right.optional, conflicts),
        introduction_fallback: property(
            table,
            base.id,
            "introduction_fallback",
            &base.introduction_fallback,
            &left.introduction_fallback,
            &right.introduction_fallback,
            conflicts,
        ),
    })
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
    Identity { table: ObjectId, key: CanonicalValue },
    DeleteAndEdit { table: ObjectId, key: CanonicalValue },
    KeyCollision { table: ObjectId, key: CanonicalValue },
    Fields {
        table: ObjectId,
        key: CanonicalValue,
        fields: Vec<ObjectId>,
    },
    AutomaticKeyCollision { table: ObjectId, key: CanonicalValue },
}

/// Merge one logical key. This is a pure value operation; storage supplies
/// rows in key order and owns resource limits and the no-partial-write rule.
pub fn merge_keyed_row(
    base: Option<&KeyedRow>,
    left: Option<&KeyedRow>,
    right: Option<&KeyedRow>,
) -> Result<Option<KeyedRow>, RowMergeConflict> {
    let exemplar = base.or(left).or(right);
    let Some(exemplar) = exemplar else { return Ok(None) };
    let identity_matches = |row: &KeyedRow| row.table == exemplar.table && row.key == exemplar.key;
    if [base, left, right].into_iter().flatten().any(|row| !identity_matches(row)) {
        return Err(RowMergeConflict::Identity { table: exemplar.table, key: exemplar.key.clone() });
    }
    if let (Some(base), Some(left), Some(right)) = (base, left, right) {
        if base.key_kind != left.key_kind || base.key_kind != right.key_kind {
            return Err(RowMergeConflict::Identity { table: base.table, key: base.key.clone() });
        }
    }
    match (base, left, right) {
        (None, None, _) | (None, _, None) => Ok(left.or(right).cloned()),
        (None, Some(left), Some(right)) if left.key_kind == RowKeyKind::Automatic || right.key_kind == RowKeyKind::Automatic => {
            Err(RowMergeConflict::AutomaticKeyCollision { table: left.table, key: left.key.clone() })
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
            Err(RowMergeConflict::DeleteAndEdit { table: base.table, key: base.key.clone() })
        }
        (Some(base), Some(left), Some(right)) => {
            if left.key_kind != base.key_kind || right.key_kind != base.key_kind {
                return Err(RowMergeConflict::Identity { table: base.table, key: base.key.clone() });
            }
            let ids: BTreeSet<_> = base.fields.keys().chain(left.fields.keys()).chain(right.fields.keys()).copied().collect();
            let mut fields = BTreeMap::new();
            let mut conflicts = Vec::new();
            for id in ids {
                let value = merge_optional(base.fields.get(&id), left.fields.get(&id), right.fields.get(&id));
                if left.fields.get(&id) != right.fields.get(&id)
                    && left.fields.get(&id) != base.fields.get(&id)
                    && right.fields.get(&id) != base.fields.get(&id)
                {
                    conflicts.push(id);
                } else if let Some(value) = value {
                    fields.insert(id, value);
                }
            }
            if conflicts.is_empty() {
                Ok(Some(KeyedRow { table: base.table, key: base.key.clone(), key_kind: base.key_kind, fields }))
            } else {
                Err(RowMergeConflict::Fields { table: base.table, key: base.key.clone(), fields: conflicts })
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
pub fn merge_checkpoint_generation(
    base: Option<&CheckpointGeneration>,
    left: Option<&CheckpointGeneration>,
    right: Option<&CheckpointGeneration>,
) -> Result<Option<CheckpointGeneration>, CheckpointMergeConflict> {
    if left == right {
        Ok(left.cloned())
    } else if left == base {
        Ok(right.cloned())
    } else if right == base {
        Ok(left.cloned())
    } else {
        Err(CheckpointMergeConflict { base: base.cloned(), left: left.cloned(), right: right.cloned() })
    }
}
