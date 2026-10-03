use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_semantic_v1::StandardDependencyProfile;
use orna_sys_v1::ClockProvider;
use orna_value_v1::Raw;
use std::time::Duration;

#[path = "support/pinned_time_text_std.rs"]
mod pinned_time_text_std;

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected result is canonical")
}

fn captured_concurrent_sources() -> Vec<(String, String)> {
    orna_standard::reference_standard_sources_v1()
        .into_iter()
        .filter(|(path, _)| path == "std/concurrent/main.orna")
        .collect()
}

#[test]
fn concurrent_contract_uses_a_captured_optional_standard_snapshot() {
    let sources = captured_concurrent_sources();
    assert_eq!(sources.len(), 1, "the pinned concurrent module is present");
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/x6aj2-concurrent-time-contracts",
        sources.clone(),
    )
    .expect("concurrent source bytes form a dependency snapshot");
    let (path, source) = &sources[0];
    profile
        .verify_source(path, source)
        .expect("captured source matches the dependency snapshot");
    assert!(
        profile
            .verify_source(path, &format!("{source}\n// changed after capture"))
            .is_err()
    );

    let mut core = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        core.submit(include_str!(
            "fixtures/stdlib-core-without-concurrent-x6aj2.orna"
        )),
        Ok(Some(canonical(Raw::Bool(true))))
    );
}

#[test]
fn elapsed_concurrency_edges_produce_documented_values_and_errors() {
    let mut session = pinned_time_text_std::concurrent_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );
    let value = session
        .submit(include_str!(
            "fixtures/stdlib-concurrent-time-edge-values-x6aj2.orna"
        ))
        .unwrap_or_else(|error| panic!("elapsed concurrency case failed: {}", error.code()));
    assert_eq!(
        value,
        Some(canonical(Raw::Array(vec![
            Raw::Int(17.into()),
            Raw::Int(23.into()),
            Raw::Int(41.into()),
        ])))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-timeout-value-x6aj2.orna"
            ))
            .unwrap_or_else(|error| panic!("positive timeout failed: {}", error.code())),
        Some(canonical(Raw::Int(47.into())))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-timeout-negative-x6aj2.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn zero_elapsed_sleep_uses_an_admitted_clock_and_returns_null() {
    let mut session = pinned_time_text_std::concurrent_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );
    let mut bindings =
        SysHostBindingRegistry::default().with_clock_provider(ClockProvider::new(Duration::ZERO));
    assert_eq!(
        session.submit_with_sys_host_bindings(
            include_str!("fixtures/stdlib-concurrent-sleep-zero-x6aj2.orna"),
            &mut bindings,
        ),
        Ok(Some(canonical(Raw::Null)))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-sleep-zero-x6aj2.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-UNSUPPORTED"
    );
}
