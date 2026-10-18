use orna_evaluator_v1::SysHostBindingRegistry;
use orna_foundation_v1::CanonicalValue;
use orna_sys_v1::ClockProvider;
use orna_value_v1::Raw;
use std::time::{Duration, Instant};

#[path = "support/pinned_time_text_std.rs"]
mod pinned_time_text_std;

fn int(value: i64) -> Raw {
    Raw::Int(value.into())
}

fn list(values: impl IntoIterator<Item = Raw>) -> Raw {
    Raw::Array(values.into_iter().collect())
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

/// ORNA-CONCUR-002: parallel returns successful results in callback input order.
#[test]
fn structured_combinators_return_computed_values() {
    let mut session = pinned_time_text_std::concurrent_session();
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

/// ORNA-CONCUR-002: a slower first callback still contributes the first result in input order.
#[test]
fn parallel_results_follow_input_order_when_completion_is_reversed() {
    let mut session = pinned_time_text_std::concurrent_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );
    let mut bindings = SysHostBindingRegistry::default()
        .with_clock_provider(ClockProvider::new(Duration::from_secs(5)));
    let result = session
        .submit_with_sys_host_bindings(
            "concurrent.parallel([() => { concurrent.sleep(1.s); 2 }, () => 3])",
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("ordered parallel failed with {}", error.code()));
    assert_eq!(result, Some(canonical(list([int(2), int(3)]))));
}

/// ORNA-CONCUR-005: race selects the first success and joins its cancelled, sleeping loser.
#[test]
fn race_cancels_and_joins_a_slower_loser() {
    let mut session = pinned_time_text_std::concurrent_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-concurrent-zhw5h.orna")),
        Ok(None)
    );
    let mut bindings = SysHostBindingRegistry::default()
        .with_clock_provider(ClockProvider::new(Duration::from_secs(5)));
    let started = Instant::now();
    let result = session
        .submit_with_sys_host_bindings(
            "concurrent.race([() => { concurrent.sleep(2.s); 29 }, () => 31])",
            &mut bindings,
        )
        .unwrap_or_else(|error| panic!("race cancellation failed with {}", error.code()));
    assert_eq!(result, Some(canonical(int(31))));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "race must join the cancelled sleeper without awaiting its full duration"
    );
}

#[test]
fn empty_and_timeout_edges_follow_the_documented_results() {
    let mut session = pinned_time_text_std::concurrent_session();
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

/// ORNA-CONCUR-006: timeout requests cancellation and joins the timed-out host-effect callback before returning.
#[test]
fn structured_children_fork_host_effects_and_timeout_joins_cancellation() {
    let mut session = pinned_time_text_std::concurrent_session();
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
