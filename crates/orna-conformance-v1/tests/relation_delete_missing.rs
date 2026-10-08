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

// Deleting a key that is not stored is an error (the runtime rejects a missing
// delete as InvalidTableMutation and does not advance state). The stored row
// for id 1 must survive the failed delete unchanged.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }

fn main() {
    assert (Author | count) == 1;
    assert (Author | filter(author => author.id == 1 && author.name == \"ada\") | count) == 1;
}
";

#[test]
fn deleting_a_missing_key_fails_and_leaves_the_stored_row_unchanged() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let original = evaluator.execute_source(&source(
        "relation-delete-missing-original",
        "pub table Author(id: Int) { name: Str, }\n\nfn main() {\n    Author.insert({ id: 1, name: \"ada\" });\n}\n",
    ));
    assert!(
        matches!(original, StageOutcome::Passed),
        "original insert failed: {original:?}"
    );

    let missing = evaluator.execute_source(&source(
        "relation-delete-missing-key",
        include_str!("fixtures/relation-delete-missing-key.orna"),
    ));
    assert!(
        !matches!(missing, StageOutcome::Passed),
        "delete of a missing key must not succeed: {missing:?}"
    );

    let checked = evaluator.execute_source(&source("relation-delete-missing-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "stored row changed after failed delete: {checked:?}"
    );
}
