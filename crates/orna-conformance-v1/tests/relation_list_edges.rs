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

// Edges were written in descending id order. A listing sorted by key must put
// book 10 first, regardless of the order the rows were inserted.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }
pub table Book(id: Int) { author: Int, title: Str, }

fn main() {
    assert (Book | count) == 2;
    assert (Book | sort_by(book => book.id) | take(1) | filter(book => book.id == 10) | count) == 1;
    assert (Book | sort_by(book => book.id) | take(1) | filter(book => book.id == 11) | count) == 0;
}
";

#[test]
fn sorted_edge_listing_is_ordered_by_key_not_insert_order() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let written = evaluator.execute_source(&source(
        "relation-list-edges-reversed",
        include_str!("fixtures/relation-list-edges-reversed.orna"),
    ));
    assert!(
        matches!(written, StageOutcome::Passed),
        "reversed-insert source failed: {written:?}"
    );

    let checked = evaluator.execute_source(&source("relation-list-edges-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "edge ordering check failed: {checked:?}"
    );
}
