use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 4] = [
    "std/collection.orna",
    "std/text.orna",
    "std/encoding/base64.orna",
    "std/encoding/utilities.orna",
];

fn session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/unicode-scalar-boundaries",
        sources.clone(),
    )
    .expect("text, collection and encoding sources form a pinned profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("unicode utilities resolve against the pinned text module");
    let session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!("unicode scalar modules failed to load: {}", error.code())
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed unicode source matches its captured profile");
    }
    session
}

/// Every `&&`-separated expression in the fixture must evaluate to `true`.
fn assert_true_fixture(session: &mut AdmittedReplSession, fixture: &str) {
    for expression in fixture.split("&&").map(str::trim) {
        let parsed = orna_syntax_v1::parse_repl(expression);
        assert!(parsed.is_ok(), "{expression}: {:#?}", parsed.diagnostics);
        let result = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "unicode scalar proof failed for `{expression}`: {}",
                error.code()
            )
        });
        assert_eq!(
            result,
            Some(CanonicalValue::new(Raw::Bool(true)).expect("true is canonical")),
            "unicode scalar fixture failed: {expression}",
        );
    }
}

#[test]
fn scalar_helpers_count_and_split_combining_and_joined_sequences_as_scalars() {
    let mut session = session();
    assert_true_fixture(
        &mut session,
        include_str!("fixtures/std-unicode-scalar-boundaries.orna"),
    );
}
