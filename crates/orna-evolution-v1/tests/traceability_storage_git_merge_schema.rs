//! Bounded executable evidence for selected traceability requirements.
//! Orna fixtures are parsed as source witnesses. The evolution planner accepts
//! resolved typed snapshots, so those test inputs are constructed separately.

use orna_evolution_v1::{
    CanonicalValue, EvolutionVersion, Field, FieldRole, FieldType, MigrationOperation,
    ObjectId, PlanningError, PlanningRequest, Schema, Table, VersionFence, plan,
};
use orna_foundation_v1::OvbRaw;
use orna_syntax_v1::parse_module;

const BASE: &str = include_str!("fixtures/traceability-schema-base.orna");
const RENAMED: &str = include_str!("fixtures/traceability-schema-renamed.orna");
const OPTIONAL: &str = include_str!("fixtures/traceability-schema-optional.orna");
const REQUIRED: &str = include_str!("fixtures/traceability-schema-required.orna");
const DEFAULT_GB: &str = include_str!("fixtures/traceability-schema-default-gb.orna");
const DEFAULT_US: &str = include_str!("fixtures/traceability-schema-default-us.orna");

fn id(n: u8) -> ObjectId {
    ObjectId::new([n; 16])
}

fn field(n: u8, name: &str, ty: FieldType, role: FieldRole, optional: bool) -> Field {
    Field {
        id: id(n),
        name: name.into(),
        ty,
        role,
        optional,
        introduction_fallback: None,
    }
}

fn base_table(name: &str, table_id: u8, label: &str) -> Table {
    Table {
        id: id(table_id),
        name: name.into(),
        explicit_key: true,
        fields: vec![
            field(2, "id", FieldType::Int, FieldRole::Key, false),
            field(3, label, FieldType::Str, FieldRole::Stored, false),
        ],
    }
}

fn schema(table: Table) -> Schema {
    Schema {
        version: EvolutionVersion::V1_0,
        tables: vec![table],
    }
}

fn request() -> PlanningRequest {
    PlanningRequest {
        fence: VersionFence::V1,
        rekeys: Vec::new(),
    }
}

fn parse_fixture(source: &str) {
    let parsed = parse_module(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
}

// ORNA-MERGE-001 (source/25-evolution.md:5): declarative schema delta
// produces a migration plan without a separate migration language.
#[test]
fn declarative_schema_delta_yields_a_migration_plan() {
    parse_fixture(BASE);
    parse_fixture(OPTIONAL);
    let mut next = base_table("Contact", 1, "name");
    next.fields.push(field(4, "email", FieldType::Str, FieldRole::Stored, true));
    assert!(matches!(
        plan(&schema(base_table("Contact", 1, "name")), &schema(next), &request())
            .unwrap()
            .operations(),
        [MigrationOperation::AddOptionalField { .. }]
    ));
}

// ORNA-SCHEMA-001 (source/25-evolution.md:11): stable table/field ObjectIds
// preserve identity across a semantic rename.
#[test]
fn stable_identity_turns_name_changes_into_rename_operations() {
    parse_fixture(BASE);
    parse_fixture(RENAMED);
    let before = schema(base_table("Contact", 1, "name"));
    let after = schema(base_table("Person", 1, "label"));
    assert_eq!(
        plan(&before, &after, &request()).unwrap().operations(),
        &[
            MigrationOperation::RenameTable {
                table: id(1),
                from: "Contact".into(),
                to: "Person".into(),
            },
            MigrationOperation::RenameField {
                table: id(1),
                field: id(3),
                from: "name".into(),
                to: "label".into(),
            },
        ]
    );
}

// ORNA-SCHEMA-003 (source/25-evolution.md:45): optional additions are
// metadata-compatible and produce no per-row rewrite operation.
#[test]
fn optional_addition_plans_only_metadata_change() {
    parse_fixture(BASE);
    parse_fixture(OPTIONAL);
    let mut next = base_table("Contact", 1, "name");
    next.fields.push(field(4, "email", FieldType::Str, FieldRole::Stored, true));
    assert_eq!(
        plan(&schema(base_table("Contact", 1, "name")), &schema(next), &request())
            .unwrap()
            .operations(),
        &[MigrationOperation::AddOptionalField {
            table: id(1),
            field: field(4, "email", FieldType::Str, FieldRole::Stored, true),
        }]
    );
}

// ORNA-SCHEMA-004 (source/25-evolution.md:47): a committed introduction
// fallback cannot change when the declaration changes later.
#[test]
fn introduction_fallback_is_immutable_after_schema_creation() {
    parse_fixture(DEFAULT_GB);
    parse_fixture(DEFAULT_US);
    let from = schema(Table {
        fields: vec![
            field(2, "id", FieldType::Int, FieldRole::Key, false),
            Field {
                introduction_fallback: Some(
                    CanonicalValue::new(OvbRaw::Text("GB".into())).unwrap(),
                ),
                ..field(3, "country", FieldType::Str, FieldRole::Stored, false)
            },
        ],
        ..base_table("Contact", 1, "name")
    });
    let mut to = from.clone();
    to.tables[0].fields[1].introduction_fallback = Some(
        CanonicalValue::new(OvbRaw::Text("US".into())).unwrap(),
    );
    assert!(matches!(
        plan(&from, &to, &request()),
        Err(PlanningError::IncompatibleField {
            reason: "field introduction fallback changed",
            ..
        })
    ));
}

// ORNA-SCHEMA-005 (source/25-evolution.md:49): only supported closed scalar
// constants may become introduction fallbacks.
#[test]
fn fallback_must_match_the_closed_field_scalar_type() {
    parse_fixture(REQUIRED);
    let mut country = field(4, "country", FieldType::Custom("Country".into()), FieldRole::Stored, false);
    country.introduction_fallback = Some(CanonicalValue::new(OvbRaw::Text("GB".into())).unwrap());
    let mut next = base_table("Contact", 1, "name");
    next.fields.push(country);
    assert!(matches!(
        plan(&schema(base_table("Contact", 1, "name")), &schema(next), &request()),
        Err(PlanningError::IncompatibleField {
            reason: "field introduction fallback does not match field type",
            ..
        })
    ));
}

// ORNA-SCHEMA-006 (source/25-evolution.md:51): a required stored field
// without a complete fallback/backfill is rejected.
#[test]
fn required_addition_without_fallback_is_rejected() {
    parse_fixture(BASE);
    parse_fixture(REQUIRED);
    let mut next = base_table("Contact", 1, "name");
    next.fields.push(field(4, "country", FieldType::Str, FieldRole::Stored, false));
    assert!(matches!(
        plan(&schema(base_table("Contact", 1, "name")), &schema(next), &request()),
        Err(PlanningError::RequiredFieldNeedsBackfill {
            table,
            field: added,
        }) if table == id(1) && added == id(4)
    ));
}
