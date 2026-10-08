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

// Orna v1 has no implicit cascade (ORNA-MUT-008): the caller removes dependent
// rows in the same activation. After the delete, author 1 and its book are
// gone, author 2 and its book remain, and no book points at a missing author.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }
pub table Book(id: Int) { author: Int, title: Str, }

fn main() {
    assert (Author | count) == 1;
    assert (Book | count) == 1;
    assert (Book | filter(book => book.author == 1) | count) == 0;
    assert (Book | filter(book => book.author == 2) | count) == 1;
}
";

#[test]
fn deleting_an_author_after_its_dependent_book_leaves_no_dangling_rows() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let deleted = evaluator.execute_source(&source(
        "relation-delete-dependent-first",
        include_str!("fixtures/relation-delete-dependent-first.orna"),
    ));
    assert!(
        matches!(deleted, StageOutcome::Passed),
        "dependent-first delete failed: {deleted:?}"
    );

    let checked = evaluator.execute_source(&source("relation-delete-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "post-delete check failed: {checked:?}"
    );
}
