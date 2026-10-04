use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 4] = [
    "std/collection.orna",
    "std/text.orna",
    "std/parse.orna",
    "std/parse/utilities.orna",
];

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

fn pinned_session() -> AdmittedReplSession {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/4tf83-parsing-utilities",
        sources.clone(),
    )
    .expect("captured collection, text, parse, and utility sources form a profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("parsing utility imports resolve against pinned primitive parsers");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!("captured parse profile failed to load: {}", error.code())
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed source matches the captured profile");
    }
    assert_eq!(
        session.submit("use std.parse.utilities as parsing;"),
        Ok(None)
    );
    session
}

#[test]
fn parsing_utilities_are_strict_and_preserve_delimiter_boundaries() {
    let mut session = pinned_session();
    let fixture = include_str!("fixtures/stdlib-parse-utilities-4tf83.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "parse utility behavior {index} failed: {} ({expression})",
                error.code()
            )
        });
        assert_eq!(
            actual,
            Some(boolean(true)),
            "parse utility behavior {index}: {expression}"
        );
    }
}
