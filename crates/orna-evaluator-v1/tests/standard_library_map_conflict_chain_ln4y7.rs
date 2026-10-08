use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

const MODULES: [&str; 2] = ["std/collection.orna", "std/map.orna"];

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
        "orna.std/ln4y7-map-conflict-chain",
        sources.clone(),
    )
    .expect("captured collection and map sources form a profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("map imports resolve against pinned collection modules");
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources.clone(), Limits::default())
            .unwrap_or_else(|error| panic!("captured profile failed to load: {}", error.code()));
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("executed source matches its pinned profile");
    }
    assert_eq!(session.submit("use std.map as map;"), Ok(None));
    assert_eq!(session.submit("use std.collection as collection;"), Ok(None));
    session
}

#[test]
fn map_conflict_chains_are_associative_and_idempotent() {
    let mut session = pinned_session();
    let fixture = include_str!("fixtures/stdlib-map-conflict-chain-ln4y7.orna");
    for (index, expression) in fixture.split("\n&& ").enumerate() {
        let expression = expression.trim_start_matches("&& ").trim();
        let actual = session.submit(expression).unwrap_or_else(|error| {
            panic!(
                "map conflict chain behavior {index} failed: {} ({expression})",
                error.code()
            )
        });
        assert_eq!(
            actual,
            Some(boolean(true)),
            "map conflict chain behavior {index}: {expression}"
        );
    }
}
