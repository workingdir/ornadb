use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_iterator_and_lazy_modules_are_snapshot_bound_and_importable() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/collection.orna"
                || path == "std/iterator.orna"
                || path == "std/lazy.orna"
        })
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/s58ir-iterator-lazy",
        sources.clone(),
    )
    .expect("iterator and lazy modules are captured in an immutable std snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("selected std module bytes match their captured snapshot");
    }
    for path in ["std/iterator.orna", "std/lazy.orna"] {
        let mut changed = sources
            .iter()
            .find(|(source_path, _)| source_path == path)
            .expect("selected module source")
            .1
            .clone();
        changed.push_str("\n// changed after snapshot capture\n");
        assert!(profile.verify_source(path, &changed).is_err());
    }

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("selected iterator and lazy modules resolve in the captured source bundle");
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured iterator/lazy source failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-iterator-lazy-s58ir.orna"))
        .unwrap_or_else(|error| panic!("iterator/lazy imports failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-lazy-s58ir.orna"))
        .unwrap_or_else(|error| panic!("lazy import failed: {}", error.code()));
}

#[test]
fn core_assertions_work_without_std_and_both_modules_remain_optional() {
    let mut session = AdmittedReplSession::new(Limits::default());
    session
        .submit(include_str!("fixtures/stdlib-core-assertions-without-iterator-lazy-s58ir.orna"))
        .unwrap_or_else(|error| panic!("core assertion declaration failed without std: {}", error.code()));
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-core-assertions-call-without-iterator-lazy-s58ir.orna"))
            .unwrap_or_else(|error| panic!("core assertion failed without std: {}", error.code())),
        Some(bool_value(true))
    );

    let mut without_iterator = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_iterator
            .submit(include_str!("fixtures/stdlib-iterator-without-snapshot-s58ir.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    let mut without_lazy = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_lazy
            .submit(include_str!("fixtures/stdlib-lazy-without-snapshot-s58ir.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
