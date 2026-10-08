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

// A create whose key is taken must fail even when its payload differs; the
// stored row must keep its original name rather than be overwritten.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }

fn main() {
    assert (Author | count) == 1;
    assert (Author | filter(author => author.name == \"ada\") | count) == 1;
    assert (Author | filter(author => author.name == \"eve\") | count) == 0;
}
";

#[test]
fn conflicting_duplicate_create_is_rejected_and_keeps_the_original_row() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let original = evaluator.execute_source(&source(
        "relation-create-author-original",
        "pub table Author(id: Int) { name: Str, }\n\nfn main() {\n    Author.insert({ id: 1, name: \"ada\" });\n}\n",
    ));
    assert!(
        matches!(original, StageOutcome::Passed),
        "original create failed: {original:?}"
    );

    let conflict = evaluator.execute_source(&source(
        "relation-create-author-conflict",
        include_str!("fixtures/relation-create-author-conflict.orna"),
    ));
    assert!(
        !matches!(conflict, StageOutcome::Passed),
        "conflicting duplicate create must not succeed: {conflict:?}"
    );

    let checked = evaluator.execute_source(&source("relation-create-dup-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "original row check failed: {checked:?}"
    );
}
