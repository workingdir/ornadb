use orna_conformance_v1::{SourceUnit, StageOutcome, TransactionalEvaluator};
use orna_evaluator_v1::Limits;

fn source(fixture_id: &str, text: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: text.into(),
    }
}

// The edges form a cycle (1 -> 2, 2 -> 1) plus a self-loop (2 -> 2). Listing
// and filtering must terminate and count each stored edge exactly once.
const CHECK: &str = "pub table Edge(id: Int) { from: Int, to: Int, }

fn main() {
    assert (Edge | count) == 3;
    assert (Edge | filter(edge => edge.from == 1) | count) == 1;
    assert (Edge | filter(edge => edge.from == 2) | count) == 2;
    assert (Edge | sort_by(edge => edge.id) | count) == 3;
}
";

#[test]
fn cyclic_edge_relation_lists_and_filters_each_edge_once() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let written = evaluator.execute_source(&source(
        "relation-list-edges-cycle",
        include_str!("fixtures/relation-list-edges-cycle.orna"),
    ));
    assert!(
        matches!(written, StageOutcome::Passed),
        "cyclic edge source failed: {written:?}"
    );

    let checked = evaluator.execute_source(&source("relation-list-edges-cycle-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "cyclic edge listing check failed: {checked:?}"
    );
}
