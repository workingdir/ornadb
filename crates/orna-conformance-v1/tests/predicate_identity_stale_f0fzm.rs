use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_semantic_v1::{Catalogue, ModuleInput, analyze_with_catalogue};

#[test]
fn paired_depth_continuations_reject_a_stale_parent_identity() {
    let source = include_str!("fixtures/txn-paired-depth-continuation-stale-f0fzm.orna");
    let analysis = analyze_with_catalogue(
        &[ModuleInput::new(
            "paired-depth-continuation-stale.orna",
            source,
        )],
        &Catalogue::authoritative_fixture(),
    );
    assert!(
        analysis.is_ok(),
        "stale-pair fixture must type-check: {:?}",
        analysis
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message()))
            .collect::<Vec<_>>()
    );

    let source_unit = SourceUnit {
        fixture_id: "txn-paired-depth-continuation-stale-f0fzm".into(),
        source_id: "txn-paired-depth-continuation-stale-f0fzm.orna".into(),
        parse_as: "module_unit".into(),
        source: source.into(),
    };
    let mut runtime = TransactionalEvaluator::new("parent", Limits::default());
    let outcome = runtime.execute_source(&source_unit);
    assert!(
        matches!(
            &outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-MODULE-ASSERT"
        ),
        "pair 2 must reject evidence carrying pair 1's captured value: {outcome:?}"
    );
    for (table, id) in [
        ("Pair", 1),
        ("Pair", 2),
        ("Left", 10),
        ("Left", 20),
        ("Right", 100),
        ("Right", 200),
        ("Evidence", 1000),
        ("Evidence", 2000),
    ] {
        assert_eq!(
            runtime.committed_row(table, &Value::int(id.into())),
            None,
            "failed paired continuation published {table} row {id}"
        );
    }
}
