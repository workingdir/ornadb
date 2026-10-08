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

// The same linked rows as the forward-order fixture must give the same
// per-author membership, whatever order the rows were written in.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }
pub table Book(id: Int) { author: Int, title: Str, }

fn main() {
    assert (Book | count) == 2;
    assert (Book | filter(book => book.author == 1) | count) == 1;
    assert (Book | filter(book => book.author == 2) | count) == 1;
}
";

#[test]
fn linked_relation_membership_does_not_depend_on_write_order() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let written = evaluator.execute_source(&source(
        "relation-join-order-reversed",
        include_str!("fixtures/relation-join-order-reversed.orna"),
    ));
    assert!(
        matches!(written, StageOutcome::Passed),
        "reversed-order source failed: {written:?}"
    );

    let checked = evaluator.execute_source(&source("relation-join-order-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "membership check failed: {checked:?}"
    );
}
