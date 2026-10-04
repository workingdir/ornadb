use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const SORTING_MODULES: [&str; 2] = ["std/collection.orna", "std/sorting.orna"];

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).expect("proof result is canonical")
}

fn pinned_sorting_session() -> (AdmittedReplSession, StandardDependencyProfile) {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| SORTING_MODULES.contains(&path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), SORTING_MODULES.len());
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/mk858-sorting-utilities",
        sources.clone(),
    )
    .expect("captured collection and sorting sources form a dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("sorting utility imports resolve against the pinned collection module");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| {
                panic!("captured sorting profile failed to load: {}", error.code())
            });
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed sorting source matches the captured profile");
    }
    assert_eq!(session.submit("use std.sorting as sorting;"), Ok(None));
    (session, profile)
}

#[test]
fn sorting_and_ordering_helpers_preserve_stability_and_edge_cases() {
    let (mut session, _) = pinned_sorting_session();
    let fixture = include_str!("fixtures/stdlib-sorting-utilities-mk858.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        assert_eq!(
            session.submit(expression).unwrap_or_else(|error| panic!(
                "sorting behavior {index} failed: {} ({expression})",
                error.code()
            )),
            Some(boolean(true)),
            "sorting behavior {index}: {expression}"
        );
    }
}
