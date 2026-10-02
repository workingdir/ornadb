use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_generics_and_type_utils_are_snapshot_bound_and_core_stays_optional() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/generics.orna" || path == "std/type_utils.orna"
        })
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources("orna.std/9g7my-type-utilities", sources.clone())
        .expect("the selected optional sources form a captured dependency snapshot");
    for (path, source) in &sources {
        profile
            .verify_source(path, source)
            .expect("captured generic utility source matches its snapshot");
    }
    let mut changed_source = sources
        .iter()
        .find(|(path, _)| path == "std/type_utils.orna")
        .expect("type utility source")
        .1
        .clone();
    changed_source.push_str("\n// modified after capture\n");
    assert!(profile.verify_source("std/type_utils.orna", &changed_source).is_err());

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the captured optional modules resolve against core");
    let mut with_std = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured std failed to load: {}", error.code()));
    with_std
        .submit(include_str!("fixtures/stdlib-use-generics-9g7my.orna"))
        .unwrap_or_else(|error| panic!("generic utility imports failed: {}", error.code()));
    with_std
        .submit(include_str!("fixtures/stdlib-use-type-utils-9g7my.orna"))
        .unwrap_or_else(|error| panic!("type utility imports failed: {}", error.code()));
    let values = with_std
        .submit(include_str!("fixtures/stdlib-generics-type-utils-9g7my.orna"))
        .unwrap_or_else(|error| panic!("generic value helpers failed: {}", error.code()));
    assert_eq!(values, Some(bool_value(true)));

    let mut without_std = AdmittedReplSession::new(Limits::default());
    without_std
        .submit(include_str!("fixtures/stdlib-core-assertions-without-generics-9g7my.orna"))
        .unwrap_or_else(|error| panic!("core assertion declaration failed: {}", error.code()));
    let core = without_std
        .submit(include_str!("fixtures/stdlib-core-assertions-call-without-generics-9g7my.orna"))
        .unwrap_or_else(|error| panic!("core assertion failed without std: {}", error.code()));
    assert_eq!(core, Some(bool_value(true)));
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-optional-generics-9g7my.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-use-optional-type-utils-9g7my.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
    assert_eq!(
        without_std
            .submit(include_str!("fixtures/stdlib-core-assertions-call-without-generics-9g7my.orna"))
            .unwrap_or_else(|error| panic!("core assertion after missing optional import: {}", error.code())),
        Some(bool_value(true))
    );
}
