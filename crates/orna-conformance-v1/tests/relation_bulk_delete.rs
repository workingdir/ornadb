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

// The bulk delete removes book 10 and then fails on the missing book 99. The
// whole source is atomic: book 10 must still be stored afterwards.
const CHECK: &str = "pub table Book(id: Int) { author: Int, title: Str, }

fn main() {
    assert (Book | count) == 2;
    assert (Book | filter(book => book.id == 10) | count) == 1;
}
";

#[test]
fn bulk_delete_with_a_missing_key_leaves_every_row_in_place() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let seeded = evaluator.execute_source(&source(
        "relation-bulk-delete-seed",
        "pub table Book(id: Int) { author: Int, title: Str, }\n\nfn main() {\n    Book.insert({ id: 10, author: 1, title: \"first\" });\n    Book.insert({ id: 11, author: 2, title: \"second\" });\n}\n",
    ));
    assert!(
        matches!(seeded, StageOutcome::Passed),
        "seed failed: {seeded:?}"
    );

    let bulk = evaluator.execute_source(&source(
        "relation-bulk-delete-with-missing",
        include_str!("fixtures/relation-bulk-delete-with-missing.orna"),
    ));
    assert!(
        !matches!(bulk, StageOutcome::Passed),
        "bulk delete with a missing key must not succeed: {bulk:?}"
    );

    let checked = evaluator.execute_source(&source("relation-bulk-delete-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "bulk delete was not atomic: {checked:?}"
    );
}
