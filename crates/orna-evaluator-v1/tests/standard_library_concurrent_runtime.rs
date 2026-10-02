use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::ClockProvider;
use orna_value_v1::Raw;
use std::time::{Duration, Instant};

fn int(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn list(values: impl IntoIterator<Item = Raw>) -> Raw {
    Raw::Array(values.into_iter().collect())
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

#[test]
fn structured_combinators_return_computed_values() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );

    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-parallel-m45oc.orna"
            ))
            .unwrap_or_else(|error| panic!("parallel callbacks failed with {}", error.code())),
        Some(canonical(list([int(2), int(3), int(5)])))
    );
    let raced = session
        .submit(include_str!("fixtures/stdlib-concurrent-race-m45oc.orna"))
        .unwrap_or_else(|error| panic!("race callbacks failed with {}", error.code()));
    assert!(
        raced == Some(canonical(int(29))) || raced == Some(canonical(int(31))),
        "race must return a successful callback's computed integer, got {raced:?}"
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-timeout-m45oc.orna"
            ))
            .unwrap_or_else(|error| panic!("timeout callback failed with {}", error.code())),
        Some(canonical(int(37)))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-captured-value-m45oc.orna"
            ))
            .unwrap_or_else(|error| panic!("captured callback failed with {}", error.code())),
        Some(canonical(list([int(41)])))
    );
}

#[test]
fn empty_and_timeout_edges_follow_the_documented_results() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );

    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-parallel-empty-m45oc.orna"
            ))
            .unwrap_or_else(|error| panic!("empty parallel failed with {}", error.code())),
        Some(canonical(list([])))
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-race-empty-m45oc.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!(
                "fixtures/stdlib-concurrent-timeout-cancels-m45oc.orna"
            ))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-TIMEOUT"
    );
    assert_eq!(
        session
            .submit("concurrent.timeout(() => 7, -1.s)")
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn structured_children_fork_host_effects_and_timeout_joins_cancellation() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile loads");
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );
    let mut bindings = SysHostBindingRegistry::default()
        .with_clock_provider(ClockProvider::new(Duration::from_secs(5)));
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-concurrent-child-effects-m45oc.orna"),
                &mut bindings,
            )
            .unwrap_or_else(|error| panic!("child host effects failed with {}", error.code())),
        Some(canonical(list([Raw::Null, Raw::Null])))
    );

    let started = Instant::now();
    assert_eq!(
        session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/stdlib-concurrent-timeout-host-effect-m45oc.orna"),
                &mut bindings,
            )
            .unwrap_err()
            .code(),
        "ORNA-EVAL-TIMEOUT"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "timeout must cancel and join the clock wait promptly"
    );
}
