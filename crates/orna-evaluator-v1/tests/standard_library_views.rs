use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_views_module_is_snapshot_bound_and_importable() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/collection.orna" || path == "std/views.orna")
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/wc6kr-collection-views",
        sources.clone(),
    )
    .expect("collection and views sources are captured in an immutable std snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("selected source bytes match their captured snapshot");
    }
    let mut changed_views = sources
        .iter()
        .find(|(path, _)| path == "std/views.orna")
        .expect("the captured std.views source")
        .1
        .clone();
    changed_views.push_str("\n// changed after snapshot capture\n");
    assert!(profile.verify_source("std/views.orna", &changed_views).is_err());

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("std.views resolves its collection dependency in the captured bundle");
    let mut session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured views source failed to load: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-views-wc6kr.orna"))
        .unwrap_or_else(|error| panic!("views import failed: {}", error.code()));
}

#[test]
fn core_assertions_work_without_std_and_views_remain_optional() {
    let mut session = AdmittedReplSession::new(Limits::default());
    session
        .submit(include_str!("fixtures/stdlib-core-assertions-without-views-wc6kr.orna"))
        .unwrap_or_else(|error| panic!("core assertion declaration failed without std: {}", error.code()));
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-core-assertions-call-without-views-wc6kr.orna"))
            .unwrap_or_else(|error| panic!("core assertion failed without std: {}", error.code())),
        Some(bool_value(true))
    );

    let mut without_views = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_views
            .submit(include_str!("fixtures/stdlib-views-without-snapshot-wc6kr.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
