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

// The check redeclares the same tables, as the decimal cursor pages fixture
// does. After the edit, book 10 is gone, book 12 belongs to author 1, and book
// 11 still belongs to author 2, so each author's membership follows the edit.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }
pub table Book(id: Int) { author: Int, title: Str, }

fn main() {
    assert (Book | count) == 2;
    assert (Book | filter(book => book.author == 1) | count) == 1;
    assert (Book | filter(book => book.author == 2) | count) == 1;
}
";

#[test]
fn relation_membership_follows_a_row_edited_across_linked_tables() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let edited = evaluator.execute_source(&source(
        "relation-edit-follow-linked-rows",
        include_str!("fixtures/relation-edit-follow-linked-rows.orna"),
    ));
    assert!(
        matches!(edited, StageOutcome::Passed),
        "edit source failed: {edited:?}"
    );

    let checked = evaluator.execute_source(&source("relation-edit-follow-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "membership check failed: {checked:?}"
    );
}
