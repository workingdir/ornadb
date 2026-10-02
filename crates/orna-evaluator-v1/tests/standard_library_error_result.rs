use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).unwrap()
}

#[test]
fn pinned_error_and_result_modules_load_from_the_captured_snapshot() {
    let sources = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/error.orna" || path == "std/result.orna")
        .collect::<Vec<_>>();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/typdl-error-result",
        sources.clone(),
    )
    .unwrap();
    let error_source = sources
        .iter()
        .find(|(path, _)| path == "std/error.orna")
        .unwrap();
    profile
        .verify_source(&error_source.0, &error_source.1)
        .expect("the selected Error module bytes match the captured dependency snapshot");
    let mut changed = error_source.1.clone();
    changed.push_str("\n// post-snapshot edit\n");
    assert!(profile.verify_source(&error_source.0, &changed).is_err());

    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .unwrap();
    let mut session =
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
            .unwrap_or_else(|error| panic!("{}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-error-typdl.orna"))
        .unwrap_or_else(|error| panic!("error import failed: {}", error.code()));
    session
        .submit(include_str!("fixtures/stdlib-use-result-typdl.orna"))
        .unwrap_or_else(|error| panic!("result import failed: {}", error.code()));
}

#[test]
fn core_failure_recovery_works_without_std_and_optional_error_module_stays_missing() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let core = session
        .submit(include_str!("fixtures/stdlib-core-error-without-std-typdl.orna"))
        .unwrap_or_else(|error| panic!("core recovery failed: {}", error.code()));
    assert_eq!(core, Some(canonical(Raw::Bool(true))));
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-error-without-snapshot-typdl.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
