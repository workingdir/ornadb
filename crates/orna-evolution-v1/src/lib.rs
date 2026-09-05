//! Bounded, deterministic schema-evolution planning for Orna 1.0.
//!
//! This crate plans from two declarative schema snapshots; it does not read
//! rows, infer identity from names, backfill data, or mutate a CWD.  That
//! follows ORNA-MERGE-001/002 and ORNA-SCHEMA-001 through 008: stable object
//! IDs prove continuity, optional additions are metadata-compatible, and any
//! unresolved condition rejects the complete plan before approval.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

pub use orna_foundation_v1::CanonicalValue;

/// Stable semantic identity (`sys.ObjectId`), deliberately distinct from a
/// snapshot-local name.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId([u8; 16]);

impl ObjectId {
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Compatibility coordinate for this planner's closed input vocabulary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EvolutionVersion {
    pub major: u16,
    pub minor: u16,
}

impl EvolutionVersion {
    pub const V1_0: Self = Self { major: 1, minor: 0 };
}

/// The explicit version boundary required of both schema snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionFence {
    pub minimum: EvolutionVersion,
    pub maximum: EvolutionVersion,
}

impl VersionFence {
    pub const V1: Self = Self {
        minimum: EvolutionVersion::V1_0,
        maximum: EvolutionVersion::V1_0,
    };
    fn permits(self, version: EvolutionVersion) -> bool {
        self.minimum <= version && version <= self.maximum
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FieldType {
    Bool,
    Int,
    Str,
    Uuid,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldRole {
    Key,
    Stored,
    Computed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    pub id: ObjectId,
    pub name: String,
    pub ty: FieldType,
    pub role: FieldRole,
    pub optional: bool,
    /// A frozen introduction fallback, never an insert-time default.
    pub introduction_fallback: Option<CanonicalValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    pub id: ObjectId,
    pub name: String,
    /// Automatic IDs may never be rekeyed (ORNA-MUT-009).
    pub explicit_key: bool,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Schema {
    pub version: EvolutionVersion,
    pub tables: Vec<Table>,
}

/// An explicit, already-authorized semantic row rekey intent.  The planner
/// never guesses these from similarly shaped delete/add changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RekeyIntent {
    pub table: ObjectId,
    pub old_key: CanonicalValue,
    pub new_key: CanonicalValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanningRequest {
    pub fence: VersionFence,
    pub rekeys: Vec<RekeyIntent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MigrationOperation {
    RenameTable {
        table: ObjectId,
        from: String,
        to: String,
    },
    RenameField {
        table: ObjectId,
        field: ObjectId,
        from: String,
        to: String,
    },
    AddOptionalField {
        table: ObjectId,
        field: Field,
    },
    AddRequiredFieldWithFallback {
        table: ObjectId,
        field: Field,
    },
    RekeyRow {
        table: ObjectId,
        old_key: CanonicalValue,
        new_key: CanonicalValue,
    },
}

impl MigrationOperation {
    fn sort_key(&self) -> (ObjectId, u8, ObjectId, String) {
        match self {
            Self::RenameTable { table, to, .. } => (*table, 0, *table, to.clone()),
            Self::RenameField {
                table, field, to, ..
            } => (*table, 1, *field, to.clone()),
            Self::AddOptionalField { table, field } => (*table, 2, field.id, field.name.clone()),
            Self::AddRequiredFieldWithFallback { table, field } => {
                (*table, 3, field.id, field.name.clone())
            }
            Self::RekeyRow { table, old_key, .. } => (*table, 4, *table, canonical_key(old_key)),
        }
    }
}

fn canonical_key(value: &CanonicalValue) -> String {
    value
        .encode()
        .map(hex)
        .unwrap_or_else(|_| "invalid-canonical-value".into())
}
fn hex(bytes: Vec<u8>) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fully validated, immutable plan. Operations are canonicalized once at
/// construction, so identical inputs always yield equal plans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationPlan {
    operations: Vec<MigrationOperation>,
}
impl MigrationPlan {
    pub fn operations(&self) -> &[MigrationOperation] {
        &self.operations
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanningError {
    VersionFence {
        side: SchemaSide,
        found: EvolutionVersion,
        fence: VersionFence,
    },
    DuplicateTableId {
        side: SchemaSide,
        table: ObjectId,
    },
    DuplicateFieldId {
        side: SchemaSide,
        table: ObjectId,
        field: ObjectId,
    },
    MissingTableIdentity {
        table: ObjectId,
    },
    MissingFieldIdentity {
        table: ObjectId,
        field: ObjectId,
    },
    IncompatibleField {
        table: ObjectId,
        field: ObjectId,
        reason: &'static str,
    },
    RequiredFieldNeedsBackfill {
        table: ObjectId,
        field: ObjectId,
    },
    InvalidRekey {
        table: ObjectId,
        reason: &'static str,
    },
    AmbiguousRekey {
        table: ObjectId,
        reason: &'static str,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaSide {
    From,
    To,
}

impl fmt::Display for PlanningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "schema evolution rejected: {self:?}")
    }
}
impl Error for PlanningError {}

/// Computes a no-side-effect plan or one precise fail-closed diagnostic.
pub fn plan(
    from: &Schema,
    to: &Schema,
    request: &PlanningRequest,
) -> Result<MigrationPlan, PlanningError> {
    validate_schema(from, SchemaSide::From, request.fence)?;
    validate_schema(to, SchemaSide::To, request.fence)?;
    let old_tables: BTreeMap<_, _> = from.tables.iter().map(|table| (table.id, table)).collect();
    let new_tables: BTreeMap<_, _> = to.tables.iter().map(|table| (table.id, table)).collect();
    if let Some(table) = old_tables.keys().find(|id| !new_tables.contains_key(id)) {
        return Err(PlanningError::MissingTableIdentity { table: *table });
    }
    if let Some(table) = new_tables.keys().find(|id| !old_tables.contains_key(id)) {
        return Err(PlanningError::MissingTableIdentity { table: *table });
    }
    let mut operations = Vec::new();
    for (id, old) in old_tables {
        let new = new_tables[&id];
        if old.name != new.name {
            operations.push(MigrationOperation::RenameTable {
                table: id,
                from: old.name.clone(),
                to: new.name.clone(),
            });
        }
        compare_fields(id, old, new, &mut operations)?;
    }
    validate_rekeys(&new_tables, &request.rekeys, &mut operations)?;
    operations.sort_by_key(MigrationOperation::sort_key);
    Ok(MigrationPlan { operations })
}

fn validate_schema(
    schema: &Schema,
    side: SchemaSide,
    fence: VersionFence,
) -> Result<(), PlanningError> {
    if !fence.permits(schema.version) {
        return Err(PlanningError::VersionFence {
            side,
            found: schema.version,
            fence,
        });
    }
    let mut table_ids = BTreeSet::new();
    for table in &schema.tables {
        if !table_ids.insert(table.id) {
            return Err(PlanningError::DuplicateTableId {
                side,
                table: table.id,
            });
        }
        let mut field_ids = BTreeSet::new();
        for field in &table.fields {
            if !field_ids.insert(field.id) {
                return Err(PlanningError::DuplicateFieldId {
                    side,
                    table: table.id,
                    field: field.id,
                });
            }
        }
    }
    Ok(())
}

fn compare_fields(
    table: ObjectId,
    old: &Table,
    new: &Table,
    operations: &mut Vec<MigrationOperation>,
) -> Result<(), PlanningError> {
    let before: BTreeMap<_, _> = old.fields.iter().map(|field| (field.id, field)).collect();
    let after: BTreeMap<_, _> = new.fields.iter().map(|field| (field.id, field)).collect();
    if let Some(field) = before.keys().find(|id| !after.contains_key(id)) {
        return Err(PlanningError::MissingFieldIdentity {
            table,
            field: *field,
        });
    }
    for (&id, old_field) in &before {
        let new_field = after[&id];
        if old_field.ty != new_field.ty {
            return Err(PlanningError::IncompatibleField {
                table,
                field: id,
                reason: "type changed",
            });
        }
        if old_field.role != new_field.role {
            return Err(PlanningError::IncompatibleField {
                table,
                field: id,
                reason: "field role changed",
            });
        }
        if old_field.optional && !new_field.optional {
            return Err(PlanningError::IncompatibleField {
                table,
                field: id,
                reason: "optional field became required",
            });
        }
        if old_field.name != new_field.name {
            operations.push(MigrationOperation::RenameField {
                table,
                field: id,
                from: old_field.name.clone(),
                to: new_field.name.clone(),
            });
        }
    }
    for (id, field) in after {
        if before.contains_key(&id) {
            continue;
        }
        if field.role == FieldRole::Computed {
            return Err(PlanningError::IncompatibleField {
                table,
                field: id,
                reason: "computed fields are not migration steps",
            });
        }
        if field.optional {
            operations.push(MigrationOperation::AddOptionalField {
                table,
                field: field.clone(),
            });
        } else if field.introduction_fallback.is_some() {
            operations.push(MigrationOperation::AddRequiredFieldWithFallback {
                table,
                field: field.clone(),
            });
        } else {
            return Err(PlanningError::RequiredFieldNeedsBackfill { table, field: id });
        }
    }
    Ok(())
}

fn validate_rekeys(
    tables: &BTreeMap<ObjectId, &Table>,
    rekeys: &[RekeyIntent],
    operations: &mut Vec<MigrationOperation>,
) -> Result<(), PlanningError> {
    let mut old_keys = BTreeSet::new();
    let mut new_keys = BTreeSet::new();
    for rekey in rekeys {
        let Some(table) = tables.get(&rekey.table) else {
            return Err(PlanningError::InvalidRekey {
                table: rekey.table,
                reason: "unknown table identity",
            });
        };
        if !table.explicit_key {
            return Err(PlanningError::InvalidRekey {
                table: rekey.table,
                reason: "automatic keys cannot be rekeyed",
            });
        }
        if rekey.old_key == rekey.new_key {
            return Err(PlanningError::InvalidRekey {
                table: rekey.table,
                reason: "old and new key are equal",
            });
        }
        let old = (rekey.table, canonical_key(&rekey.old_key));
        let new = (rekey.table, canonical_key(&rekey.new_key));
        if !old_keys.insert(old) {
            return Err(PlanningError::AmbiguousRekey {
                table: rekey.table,
                reason: "multiple intents name one old key",
            });
        }
        if !new_keys.insert(new) {
            return Err(PlanningError::AmbiguousRekey {
                table: rekey.table,
                reason: "multiple intents target one new key",
            });
        }
        operations.push(MigrationOperation::RekeyRow {
            table: rekey.table,
            old_key: rekey.old_key.clone(),
            new_key: rekey.new_key.clone(),
        });
    }
    Ok(())
}

/// The execution seam makes approval observable and guarantees that a failed
/// plan or denied approval invokes no mutating operation.
pub trait MigrationTarget {
    type Error;
    fn approve(&mut self, plan: &MigrationPlan) -> Result<bool, Self::Error>;
    fn apply(&mut self, operation: &MigrationOperation) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum ExecutionError<E> {
    Approval(E),
    Denied,
    Mutation(E),
}

/// Applies only an already-valid plan and only after the target approves it.
pub fn execute<T: MigrationTarget>(
    target: &mut T,
    plan: &MigrationPlan,
) -> Result<(), ExecutionError<T::Error>> {
    if !target.approve(plan).map_err(ExecutionError::Approval)? {
        return Err(ExecutionError::Denied);
    }
    for operation in plan.operations() {
        target.apply(operation).map_err(ExecutionError::Mutation)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> ObjectId {
        ObjectId::new([n; 16])
    }
    fn field(n: u8, name: &str, optional: bool) -> Field {
        Field {
            id: id(n),
            name: name.into(),
            ty: FieldType::Str,
            role: FieldRole::Stored,
            optional,
            introduction_fallback: None,
        }
    }
    fn table(explicit_key: bool, fields: Vec<Field>) -> Table {
        Table {
            id: id(1),
            name: "Contact".into(),
            explicit_key,
            fields,
        }
    }
    fn schema(table: Table) -> Schema {
        Schema {
            version: EvolutionVersion::V1_0,
            tables: vec![table],
        }
    }
    fn request(rekeys: Vec<RekeyIntent>) -> PlanningRequest {
        PlanningRequest {
            fence: VersionFence::V1,
            rekeys,
        }
    }

    #[test]
    fn table_driven_compatible_changes_are_planned() {
        let old = schema(table(true, vec![field(2, "name", false)]));
        let cases = [
            (
                "optional add",
                schema(table(
                    true,
                    vec![field(2, "name", false), field(3, "email", true)],
                )),
                1,
            ),
            (
                "stable-id rename",
                schema(Table {
                    id: id(1),
                    name: "Person".into(),
                    explicit_key: true,
                    fields: vec![field(2, "display_name", false)],
                }),
                2,
            ),
        ];
        for (_, next, count) in cases {
            assert_eq!(
                plan(&old, &next, &request(vec![]))
                    .unwrap()
                    .operations()
                    .len(),
                count
            );
        }
    }

    #[test]
    fn table_driven_incompatible_changes_reject_without_plan() {
        let old = schema(table(true, vec![field(2, "name", true)]));
        let type_changed = Field {
            ty: FieldType::Int,
            ..field(2, "name", true)
        };
        let cases = [
            schema(table(true, vec![field(2, "name", false)])),
            schema(table(
                true,
                vec![field(2, "name", true), field(3, "required", false)],
            )),
            schema(table(true, vec![type_changed])),
        ];
        for next in cases {
            assert!(plan(&old, &next, &request(vec![])).is_err());
        }
    }

    #[test]
    fn ambiguous_rekeys_fail_closed() {
        let source = schema(table(true, vec![field(2, "name", false)]));
        let rekeys = vec![
            RekeyIntent {
                table: id(1),
                old_key: CanonicalValue::uuid([2; 16]),
                new_key: CanonicalValue::uuid([3; 16]),
            },
            RekeyIntent {
                table: id(1),
                old_key: CanonicalValue::uuid([4; 16]),
                new_key: CanonicalValue::uuid([3; 16]),
            },
        ];
        assert!(matches!(
            plan(&source, &source, &request(rekeys)),
            Err(PlanningError::AmbiguousRekey { .. })
        ));
    }

    #[test]
    fn plan_is_idempotent_across_declaration_order() {
        let old = schema(table(true, vec![field(2, "name", false)]));
        let forward = schema(table(
            true,
            vec![
                field(4, "z", true),
                field(3, "a", true),
                field(2, "renamed", false),
            ],
        ));
        let expected = plan(&old, &forward, &request(vec![])).unwrap();
        let orderings = [
            vec![
                field(3, "a", true),
                field(2, "renamed", false),
                field(4, "z", true),
            ],
            vec![
                field(2, "renamed", false),
                field(4, "z", true),
                field(3, "a", true),
            ],
            vec![
                field(4, "z", true),
                field(3, "a", true),
                field(2, "renamed", false),
            ],
        ];
        for fields in orderings {
            let permuted = schema(table(true, fields));
            assert_eq!(plan(&old, &permuted, &request(vec![])).unwrap(), expected);
        }
        assert!(
            expected
                .operations()
                .windows(2)
                .all(|pair| pair[0].sort_key() <= pair[1].sort_key())
        );
    }

    #[derive(Default)]
    struct Target {
        approved: bool,
        applied: usize,
    }
    impl MigrationTarget for Target {
        type Error = ();
        fn approve(&mut self, _: &MigrationPlan) -> Result<bool, Self::Error> {
            Ok(self.approved)
        }
        fn apply(&mut self, _: &MigrationOperation) -> Result<(), Self::Error> {
            self.applied += 1;
            Ok(())
        }
    }
    #[test]
    fn no_mutation_occurs_before_approval() {
        let source = schema(table(true, vec![field(2, "name", false)]));
        let next = schema(table(
            true,
            vec![field(2, "name", false), field(3, "email", true)],
        ));
        let planned = plan(&source, &next, &request(vec![])).unwrap();
        let mut target = Target::default();
        assert!(matches!(
            execute(&mut target, &planned),
            Err(ExecutionError::Denied)
        ));
        assert_eq!(target.applied, 0);
        target.approved = true;
        execute(&mut target, &planned).unwrap();
        assert_eq!(target.applied, planned.operations().len());
    }

    #[test]
    fn version_fence_rejects_before_planning() {
        let source = schema(table(true, vec![]));
        let mut newer = source.clone();
        newer.version = EvolutionVersion { major: 1, minor: 1 };
        assert!(matches!(
            plan(&source, &newer, &request(vec![])),
            Err(PlanningError::VersionFence {
                side: SchemaSide::To,
                ..
            })
        ));
    }
}
