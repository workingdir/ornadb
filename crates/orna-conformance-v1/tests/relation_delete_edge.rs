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

// Deleting one edge row (book 10 linking author 1) removes that edge only: both
// endpoint authors remain, and the remaining edge still points at author 2.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }
pub table Book(id: Int) { author: Int, title: Str, }

fn main() {
    assert (Author | count) == 2;
    assert (Book | count) == 1;
    assert (Book | filter(book => book.author == 1) | count) == 0;
    assert (Book | filter(book => book.author == 2) | count) == 1;
}
";

#[test]
fn deleting_an_edge_row_removes_only_that_edge() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let deleted = evaluator.execute_source(&source(
        "relation-delete-edge-only",
        include_str!("fixtures/relation-delete-edge-only.orna"),
    ));
    assert!(
        matches!(deleted, StageOutcome::Passed),
        "edge delete failed: {deleted:?}"
    );

    let checked = evaluator.execute_source(&source("relation-delete-edge-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "edge removal check failed: {checked:?}"
    );
}
