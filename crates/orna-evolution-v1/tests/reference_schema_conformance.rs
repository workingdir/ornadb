use orna_evolution_v1::{
    CanonicalValue, EvolutionVersion, Field, FieldRole, FieldType, MigrationOperation, ObjectId,
    PlanningRequest, Schema, Table, VersionFence, plan,
};
use orna_foundation_v1::OvbRaw;
use serde_json::Value;

const CASES: &str = include_str!("fixtures/reference-schema-cases.json");

fn id(value: &Value) -> ObjectId {
    ObjectId::new([value.as_u64().expect("numeric fixture id") as u8; 16])
}

fn schema(value: &Value) -> Schema {
    let table = &value["table"];
    let fields = value["fields"]
        .as_array()
        .expect("schema fixture fields")
        .iter()
        .map(|field| {
            let ty = match field["type"].as_str().expect("field type") {
                "Bool" => FieldType::Bool,
                "Int" => FieldType::Int,
                "Float" => FieldType::Float,
                "Str" => FieldType::Str,
                "Uuid" => FieldType::Uuid,
                other => FieldType::Custom(other.to_owned()),
            };
            let role = match field["role"].as_str().expect("field role") {
                "key" => FieldRole::Key,
                "computed" => FieldRole::Computed,
                "stored" => FieldRole::Stored,
                other => panic!("unknown fixture field role: {other}"),
            };
            let introduction_fallback = field["fallback"].as_str().map(|text| {
                CanonicalValue::new(OvbRaw::Text(text.to_owned())).expect("canonical text")
            });
            Field {
                id: id(&field["id"]),
                name: field["name"].as_str().expect("field name").to_owned(),
                ty,
                role,
                optional: field["optional"].as_bool().expect("optional flag"),
                introduction_fallback,
            }
        })
        .collect();

    Schema {
        version: EvolutionVersion::V1_0,
        tables: vec![Table {
            id: id(&table["id"]),
            name: table["name"].as_str().expect("table name").to_owned(),
            explicit_key: table["explicit_key"]
                .as_bool()
                .expect("explicit-key flag"),
            fields,
        }],
    }
}

fn case(name: &str) -> Value {
    let fixture: Value = serde_json::from_str(CASES).expect("valid checked-in fixture");
    fixture["cases"][name].clone()
}

fn request() -> PlanningRequest {
    PlanningRequest {
        fence: VersionFence::V1,
        rekeys: Vec::new(),
    }
}

#[test]
fn stable_object_identity_renames_the_field() {
    let case = case("stable_identity_rename");
    let result = plan(&schema(&case["from"]), &schema(&case["to"]), &request()).unwrap();

    assert_eq!(
        result.operations(),
        &[MigrationOperation::RenameField {
            table: ObjectId::new([1; 16]),
            field: ObjectId::new([2; 16]),
            from: "name".into(),
            to: "display_name".into(),
        }]
    );
}

#[test]
fn rename_without_identity_continuity_is_delete_and_add() {
    let case = case("plain_rename");
    let result = plan(&schema(&case["from"]), &schema(&case["to"]), &request()).unwrap();

    assert_eq!(
        result.operations(),
        &[
            MigrationOperation::DeleteField {
                table: ObjectId::new([1; 16]),
                field: Field {
                    id: ObjectId::new([2; 16]),
                    name: "country".into(),
                    ty: FieldType::Str,
                    role: FieldRole::Stored,
                    optional: false,
                    introduction_fallback: None,
                },
            },
            MigrationOperation::AddOptionalField {
                table: ObjectId::new([1; 16]),
                field: Field {
                    id: ObjectId::new([3; 16]),
                    name: "country".into(),
                    ty: FieldType::Str,
                    role: FieldRole::Stored,
                    optional: true,
                    introduction_fallback: None,
                },
            },
        ]
    );
}

#[test]
fn optional_field_addition_plans_no_row_backfill_operation() {
    let case = case("optional_addition");
    let result = plan(&schema(&case["from"]), &schema(&case["to"]), &request()).unwrap();

    assert_eq!(result.operations().len(), 1);
    assert!(matches!(
        &result.operations()[0],
        MigrationOperation::AddOptionalField { field, .. }
            if field.name == "email" && field.optional
    ));
}

#[test]
fn frozen_introduction_fallback_cannot_be_revised() {
    let case = case("frozen_fallback");
    let initial = plan(&schema(&case["from"]), &schema(&case["to"]), &request()).unwrap();
    assert!(matches!(
        &initial.operations()[..],
        [MigrationOperation::AddRequiredFieldWithFallback { field, .. }]
            if field.introduction_fallback.is_some()
    ));

    assert!(plan(&schema(&case["to"]), &schema(&case["edited"]), &request()).is_err());
}

#[test]
fn required_field_without_fallback_is_rejected() {
    let case = case("required_without_fallback");

    assert!(plan(&schema(&case["from"]), &schema(&case["to"]), &request()).is_err());
}
