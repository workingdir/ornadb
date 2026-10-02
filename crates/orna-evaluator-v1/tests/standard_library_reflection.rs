use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_reflection_modules_are_snapshot_bound_and_importable() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/introspection.orna" || path == "std/reflection.orna"
        })
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/y3xo7-reflection-introspection",
        sources.clone(),
    )
    .expect("reflection and introspection sources are captured in a pinned snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("published module bytes match their captured snapshot");
        let mut changed = source.clone();
        changed.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(path, &changed).is_err());
    }

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the modules resolve against core and the mandatory sys surface");
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured reflection modules failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-introspection-y3xo7.orna"))
        .unwrap_or_else(|error| panic!("captured introspection import failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-reflection-y3xo7.orna"))
        .unwrap_or_else(|error| panic!("captured module import failed: {}", error.code()));
}

#[test]
fn core_assertions_work_without_std_and_reflection_stays_optional() {
    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-core-without-reflection-y3xo7.orna")),
        Ok(None)
    );
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-core-call-without-reflection-y3xo7.orna")),
        Ok(Some(bool_value(true)))
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-reflection-without-snapshot-y3xo7.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
