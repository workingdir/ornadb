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

// Table creation is insert-only, not idempotent: replaying the same create
// after the row exists is rejected, and the stored row stays single.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }

fn main() {
    assert (Author | count) == 1;
    assert (Author | filter(author => author.name == \"ada\") | count) == 1;
}
";

#[test]
fn replaying_a_relation_create_is_rejected_and_keeps_one_row() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let create = include_str!("fixtures/relation-create-author.orna");

    let first = evaluator.execute_source(&source("relation-create-author", create));
    assert!(
        matches!(first, StageOutcome::Passed),
        "first create failed: {first:?}"
    );

    let replay = evaluator.execute_source(&source("relation-create-author-replay", create));
    assert!(
        !matches!(replay, StageOutcome::Passed),
        "replayed create must not succeed: {replay:?}"
    );

    let checked = evaluator.execute_source(&source("relation-create-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "post-replay check failed: {checked:?}"
    );
}
