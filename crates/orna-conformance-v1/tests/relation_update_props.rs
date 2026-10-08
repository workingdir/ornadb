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

// Orna has no in-place update: a property change is delete-then-insert on the
// same key. Changing the name to "eve" and back to "ada" must leave exactly one
// row for id 1, named "ada", with no residue of the intermediate value.
const CHECK: &str = "pub table Author(id: Int) { name: Str, }

fn main() {
    assert (Author | count) == 1;
    assert (Author | filter(author => author.id == 1 && author.name == \"ada\") | count) == 1;
    assert (Author | filter(author => author.name == \"eve\") | count) == 0;
}
";

#[test]
fn property_update_round_trip_restores_the_original_row() {
    let mut evaluator = TransactionalEvaluator::new("main", Limits::default());
    let original = evaluator.execute_source(&source(
        "relation-update-props-original",
        "pub table Author(id: Int) { name: Str, }\n\nfn main() {\n    Author.insert({ id: 1, name: \"ada\" });\n}\n",
    ));
    assert!(
        matches!(original, StageOutcome::Passed),
        "original insert failed: {original:?}"
    );

    let round_trip = evaluator.execute_source(&source(
        "relation-update-round-trip",
        include_str!("fixtures/relation-update-round-trip.orna"),
    ));
    assert!(
        matches!(round_trip, StageOutcome::Passed),
        "update round trip failed: {round_trip:?}"
    );

    let checked = evaluator.execute_source(&source("relation-update-props-check", CHECK));
    assert!(
        matches!(checked, StageOutcome::Passed),
        "round-trip check failed: {checked:?}"
    );
}
