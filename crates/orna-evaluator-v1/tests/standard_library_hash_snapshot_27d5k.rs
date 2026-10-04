use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.into())).unwrap()
}

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn captured_hash_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/hash.orna")
        .collect()
}

#[test]
fn hash_helpers_compute_values_from_the_captured_hash_module() {
    let sources = captured_hash_sources();
    assert_eq!(sources.len(), 1, "the pinned hash module is captured once");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/27d5k-hash",
        sources.clone(),
    )
    .expect("the exact std.hash source forms a dependency snapshot");
    let (path, source) = &sources[0];
    profile
        .verify_source(path, source)
        .expect("the selected source matches the captured hash snapshot");
    assert!(profile
        .verify_source(path, &format!("{source}\n// changed after capture"))
        .is_err());
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the selected hash module resolves against the core catalogue");
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured hash profile failed to load: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-hash-27d5k.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-hash-text-value-27d5k.orna")),
        Ok(Some(text(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        )))
    );
}

#[test]
fn absent_random_module_does_not_replace_core_or_hash_dependencies() {
    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-core-without-random-hash-27d5k.orna")),
        Ok(Some(boolean(true)))
    );
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-random-without-snapshot-27d5k.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-use-hash-27d5k.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
