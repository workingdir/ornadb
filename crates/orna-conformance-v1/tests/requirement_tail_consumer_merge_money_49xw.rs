use orna_semantic_v1::{Catalogue, DIAG_TYPE, ModuleInput, Type, analyze_with_catalogue};
use orna_stream_v1::{
    CheckpointPositionMerge, Component, Position, merge_checkpoint_position,
};

fn analyze(source: &str) -> orna_semantic_v1::Analysis {
    analyze_with_catalogue(
        &[ModuleInput::new("main.orna", source)],
        &Catalogue::authoritative_fixture(),
    )
}

fn position(token: &str) -> Position {
    Position {
        token: Component::new(token).unwrap(),
    }
}

// Specified: ORNA-CONSUMER-005, source/11-streams.md:40.
// Exists: checked-in multi-root consumer fixture and this semantic test.
// Passed: the required extraction diagnostic for this fixture only.
#[test]
fn durable_consumer_with_multiple_checkpointed_roots_gets_guidance() {
    let result = analyze(include_str!("fixtures/streams-pa0p-multiple-roots.orna"));
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message().starts_with(
                "a durable consumer function may own only one checkpointed source root",
            )
            && diagnostic.message().contains("extract separate named consumer functions")
    }), "unexpected diagnostics: {:?}", result.diagnostics);
}

// Specified: ORNA-MONEY-001/002, source/05-types.md:276,278.
// Exists: checked-in exact-literal and mixed-currency fixtures, analyzed by the
// retained semantic analyzer. Passed is bounded to the inferred exact result
// type and rejection of unconverted GBP + USD addition.
#[test]
fn money_literal_is_typed_exactly_and_cross_currency_addition_is_rejected() {
    let exact = analyze(include_str!(
        "../../orna-semantic-v1/tests/fixtures/traceability-money-exact-decimal.orna"
    ));
    assert!(exact.is_ok(), "{:?}", exact.diagnostics);
    let amount = &exact.modules.values().next().unwrap().exports["amount"];
    assert!(matches!(
        &amount.ty,
        Type::Function { result, .. }
            if result.as_ref() == &Type::Applied {
                base: "Money".into(),
                arguments: vec![Type::Named("GBP".into())],
            }
    ), "unexpected exact-money result type: {:?}", amount.ty);

    let mixed = analyze(include_str!(
        "../../orna-semantic-v1/tests/fixtures/traceability-money-cross-currency-add.orna"
    ));
    assert!(mixed.diagnostics.iter().any(|diagnostic| {
        diagnostic.code() == DIAG_TYPE
            && diagnostic.message() == "cannot add different currencies without conversion"
    }), "unexpected diagnostics: {:?}", mixed.diagnostics);
}

// Specified: ORNA-CONSUMER-008, source/11-streams.md:56, and ORNA-MERGE-011,
// source/25-evolution.md:97. Exists: the public checkpoint merge function.
// Passed: equal positions merge, while two changed opaque positions remain a
// conflict with base/left/right values preserved; this is not a database merge.
#[test]
fn equal_opaque_checkpoints_merge_and_divergent_positions_conflict() {
    let base = position("base-token");
    let left = position("opaque-z");
    let right = position("opaque-a");
    assert_eq!(
        merge_checkpoint_position(Some(&base), Some(&left), Some(&left)),
        CheckpointPositionMerge::Merged(Some(left.clone()))
    );
    assert_eq!(
        merge_checkpoint_position(Some(&base), Some(&left), Some(&right)),
        CheckpointPositionMerge::Conflict(orna_stream_v1::CheckpointPositionConflict {
            base: Some(base),
            left: Some(left),
            right: Some(right),
        })
    );
}
