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

// Three pages of two edges cover all six stored edges exactly once: the first
// page holds ids 1-2, the second 3-4, the third 5-6, and nothing follows them.
const CHECK: &str = "pub table Edge(id: Int) { from: Int, to: Int, }

fn main() {
    assert (Edge | count) == 6;
    assert (Edge | take(2) | count) == 2;
    assert (Edge | drop(2) | take(2) | count) == 2;
    assert (Edge | drop(4) | take(2) | count) == 2;
    assert (Edge | drop(6) | count) == 0;
    assert (Edge | drop(2) | take(2) | filter(edge => edge.id == 3) | count) == 1;
}
";

#[test]
fn six_edges_paginate_into_three_disjoint_pages() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let written = evaluator.execute_source(&source(
        "relation-many-edges",
        include_str!("fixtures/relation-many-edges.orna"),
    ));
    assert!(
        matches!(written, StageOutcome::Passed),
        "six-edge source failed: {written:?}"
    );

    let checked = evaluator.execute_source(&source("relation-many-edges-pages", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "pagination check failed: {checked:?}"
    );
}
