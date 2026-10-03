use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
use orna_value_v1::Raw;

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

fn captured_time_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| {
            path == "std/time.orna"
                || path == "std/time/calendar.orna"
                || path.starts_with("std/time/duration/")
        })
        .collect()
}

fn pinned_time_session() -> (AdmittedReplSession, StandardDependencyProfile) {
    let sources = captured_time_sources();
    assert_eq!(sources.len(), 6, "the selected time surface has its root, calendar and four formatter modules");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/5rysm-time-duration",
        sources.clone(),
    )
    .expect("the captured time and duration sources form one pinned dependency snapshot");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("time and duration module imports resolve from the selected snapshot");
    let session = AdmittedReplSession::from_catalogue(
        &[],
        catalogue,
        sources,
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("captured time profile failed to load: {}", error.code()));
    (session, profile)
}

#[test]
fn time_and_duration_helpers_compute_exact_values_from_the_pinned_module() {
    let (mut session, profile) = pinned_time_session();
    for (path, source) in captured_time_sources() {
        profile
            .verify_source(&path, &source)
            .expect("the executed time source is the one captured by the selected profile");
        assert!(profile
            .verify_source(&path, &format!("{source}\n// changed after capture"))
            .is_err());
    }
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-use-5rysm.orna")),
        Ok(None)
    );
    for fixture in [
        include_str!("fixtures/stdlib-time-duration-helper-values-5rysm.orna"),
        include_str!("fixtures/stdlib-time-instant-helper-values-5rysm.orna"),
    ] {
        for assertion in fixture.split("&&").map(str::trim) {
            let result = session.submit(assertion);
            assert_eq!(
                result,
                Ok(Some(boolean(true))),
                "time helper should compute the documented value: {assertion}; diagnostic={}",
                result.as_ref().err().map_or("none", |error| error.code())
            );
        }
    }
}

#[test]
fn elapsed_core_operations_remain_available_without_the_optional_time_module() {
    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-core-time-without-std-5rysm.orna")),
        Ok(Some(boolean(true)))
    );
    assert_eq!(
        core.submit(include_str!("fixtures/stdlib-time-without-snapshot-5rysm.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S010-IMPORT"
    );
}
