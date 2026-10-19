include!("reference_schema_conformance.rs");
use orna_evolution_v1::RekeyIntent;

/// ORNA-EVOL-001 — stable identity, not a textual name match, establishes field continuity.
#[test]
fn req_orna_evol_001_stable_field_id_renames_through_the_schema_planner() {
    let case = case("stable_identity_rename");
    let plan = plan(&schema(&case["from"]), &schema(&case["to"]), &request()).unwrap();

    assert_eq!(
        plan.operations(),
        &[MigrationOperation::RenameField {
            table: ObjectId::new([1; 16]),
            field: ObjectId::new([2; 16]),
            from: "name".into(),
            to: "display_name".into(),
        }]
    );
}

/// ORNA-EVOL-009 — an introduction fallback is frozen by the accepted schema revision.
#[test]
fn req_orna_evol_009_frozen_required_field_fallback_cannot_change() {
    let case = case("frozen_fallback");
    let initial = plan(&schema(&case["from"]), &schema(&case["to"]), &request()).unwrap();
    assert!(matches!(
        initial.operations(),
        [MigrationOperation::AddRequiredFieldWithFallback { field, .. }]
            if field.introduction_fallback.is_some()
    ));

    assert!(plan(&schema(&case["to"]), &schema(&case["edited"]), &request()).is_err());
}

/// ORNA-EVOL-007 — explicit rekey planning emits one logical old/new key operation.
#[test]
fn req_orna_evol_007_explicit_rekey_keeps_old_and_new_addresses() {
    let case = case("stable_identity_rename");
    let old_key = CanonicalValue::new(OvbRaw::Text("old-address".into())).unwrap();
    let new_key = CanonicalValue::new(OvbRaw::Text("new-address".into())).unwrap();
    let rekey_request = PlanningRequest {
        fence: VersionFence::V1,
        rekeys: vec![RekeyIntent {
            table: ObjectId::new([1; 16]),
            old_key: old_key.clone(),
            new_key: new_key.clone(),
        }],
    };
    let plan = plan(&schema(&case["from"]), &schema(&case["from"]), &rekey_request).unwrap();

    assert_eq!(
        plan.operations(),
        &[MigrationOperation::RekeyRow {
            table: ObjectId::new([1; 16]),
            old_key,
            new_key,
        }]
    );
}
