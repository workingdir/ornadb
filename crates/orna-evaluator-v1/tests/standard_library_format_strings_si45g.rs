use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 3] = [
    "std/collection.orna",
    "std/text.orna",
    "std/format/strings.orna",
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
        "orna.std/si45g-string-formatting-utilities",
        sources.clone(),
    )
    .expect("captured collection, text, and string formatting sources form a profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("string formatting imports resolve against pinned text modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| panic!("captured profile failed to load: {}", error.code()));
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed source matches its pinned profile");
    }
    assert_eq!(
        session.submit("use std.format.strings as strings;"),
        Ok(None)
    );
    assert_eq!(session.submit("use std.text as text;"), Ok(None));
    assert_eq!(
        session.submit("use std.collection as collection;"),
        Ok(None)
    );
    session
}

#[test]
fn string_formatting_helpers_preserve_scalar_width_and_edge_cases() {
    let mut session = pinned_session();
    for expression in [
        "text.split(\"ha\", \"\") == [\"h\", \"a\"]",
        "collection.count([\"h\", \"a\"]) == 2",
        "collection.union([\"h\"], [\"a\"]) == [\"h\", \"a\"]",
    ] {
        assert_eq!(
            session.submit(expression).unwrap_or_else(|error| {
                panic!(
                    "pinned dependency smoke check failed: {} ({expression})",
                    error.code()
                )
            }),
            Some(boolean(true)),
            "pinned dependency smoke check: {expression}"
        );
    }
    let fixture = include_str!("fixtures/stdlib-format-strings-si45g.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "string formatting behavior {index} failed: {} ({expression})",
                error.code()
            )
        });
        assert_eq!(
            actual,
            Some(boolean(true)),
            "string formatting behavior {index}: {expression}"
        );
    }
}
