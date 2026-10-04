use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn numeric_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            matches!(
                path.as_str(),
                "std/collection.orna"
                    | "std/format.orna"
                    | "std/numeric.orna"
                    | "std/parse.orna"
                    | "std/text.orna"
            )
        })
        .collect()
}

fn session() -> AdmittedReplSession {
    let sources = numeric_sources();
    assert_eq!(
        sources.len(),
        5,
        "the proof loads only numeric dependencies"
    );
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/hti38-numeric-conversions",
        sources.clone(),
    )
    .expect("numeric conversion dependencies form a captured source profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("numeric conversions resolve against the pinned parser, formatter, text, and collection modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .expect("the selected numeric conversion modules load");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-numeric-hti38.orna")),
        Ok(None),
    );
    session
}

fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    let result = session.submit(fixture).unwrap_or_else(|error| {
        panic!(
            "numeric conversion proof failed for {fixture}: {}",
            error.code()
        )
    });
    assert_eq!(
        result,
        Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
        "numeric conversion fixture failed: {fixture}",
    );
}

#[test]
fn numeric_conversions_preserve_exact_integer_and_decimal_values() {
    let mut session = session();
    for fixture in [
        include_str!("fixtures/stdlib-numeric-integer-conversions-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-from-integer-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-from-text-integer-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-from-text-fraction-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-from-text-negative-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-from-text-small-hti38.orna"),
    ] {
        assert_true_fixture(&mut session, fixture);
    }
}

#[test]
fn numeric_text_conversions_reject_noncanonical_spellings() {
    let mut session = session();
    for fixture in [
        include_str!("fixtures/stdlib-numeric-integer-rejects-noncanonical-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-rejects-malformed-hti38.orna"),
        include_str!("fixtures/stdlib-numeric-decimal-rejects-noncanonical-hti38.orna"),
    ] {
        assert_true_fixture(&mut session, fixture);
    }
}
