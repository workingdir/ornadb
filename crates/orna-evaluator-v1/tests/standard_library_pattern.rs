use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pattern_module_import_is_bound_to_its_captured_snapshot() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/pattern.orna")
        .collect::<Vec<_>>();
    let profile =
        StandardDependencyProfile::from_sources("orna.std/xt7ge-pattern", sources.clone())
            .expect("pattern source is captured in an immutable dependency snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("pinned pattern source matches the captured snapshot");
    }
    let mut changed = sources[0].1.clone();
    changed.push_str("\n// changed after snapshot capture\n");
    assert!(profile.verify_source("std/pattern.orna", &changed).is_err());

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the optional pattern module resolves against core");
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured pattern source failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-pattern-xt7ge.orna"))
        .unwrap_or_else(|error| panic!("pattern import failed: {}", error.code()));
}

#[test]
fn core_assertions_work_without_std_and_pattern_module_stays_optional() {
    let mut session = AdmittedReplSession::new(Limits::default());
    session
        .submit(include_str!("fixtures/stdlib-core-assertions-without-pattern-xt7ge.orna"))
        .unwrap_or_else(|error| panic!("core assertion declaration failed without std: {}", error.code()));
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-core-assertions-call-without-pattern-xt7ge.orna"))
            .unwrap_or_else(|error| panic!("core assertion failed without std: {}", error.code())),
        Some(bool_value(true))
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-pattern-without-snapshot-xt7ge.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
