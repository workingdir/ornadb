use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::{Raw, Value};

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected value is canonical")
}

fn optional_int(value: i64) -> Raw {
    Value::option(Some(Value::int(value.into())))
        .expect("integer option is valid")
        .raw()
        .clone()
}

fn optional_ints(values: &[i64]) -> CanonicalValue {
    canonical(Raw::Array(values.iter().copied().map(optional_int).collect()))
}

fn session() -> AdmittedReplSession {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted");
    for import in [
        include_str!("fixtures/stdlib-use-query-2213.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None), "import: {import}");
    }
    session
}

#[test]
fn query_windows_compute_rate_and_trapezoidal_integral_per_overlapping_window() {
    let mut session = session();
    let rate = session
        .submit(include_str!("fixtures/stdlib-query-window-rate-pkbay.orna"))
        .unwrap_or_else(|error| panic!("window rate failed with {}", error.code()));
    assert_eq!(rate, Some(optional_ints(&[2, 2])));

    let integral = session
        .submit(include_str!("fixtures/stdlib-query-window-integrate-pkbay.orna"))
        .unwrap_or_else(|error| panic!("window integration failed with {}", error.code()));
    assert_eq!(integral, Some(optional_ints(&[20, 36])));
}

#[test]
fn query_windows_preserve_elapsed_time_across_sparse_nonoverlapping_windows() {
    let mut session = session();
    let rate = session
        .submit(include_str!("fixtures/stdlib-query-window-rate-sparse-pkbay.orna"))
        .unwrap_or_else(|error| panic!("sparse window rate failed with {}", error.code()));
    assert_eq!(rate, Some(optional_ints(&[2, 2])));

    let integral = session
        .submit(include_str!("fixtures/stdlib-query-window-integrate-sparse-pkbay.orna"))
        .unwrap_or_else(|error| panic!("sparse window integration failed with {}", error.code()));
    assert_eq!(integral, Some(optional_ints(&[2, 30])));
}

#[test]
fn query_window_derivative_uses_each_pair_right_endpoint_and_exact_slope() {
    let actual = session()
        .submit(include_str!("fixtures/stdlib-query-window-derivative-pkbay.orna"))
        .unwrap_or_else(|error| panic!("window derivative failed with {}", error.code()));
    assert_eq!(actual, Some(canonical(Raw::Bool(true))));
}

#[test]
fn time_series_windows_omit_short_tails_and_keep_strict_time_order() {
    let short = session()
        .submit(include_str!("fixtures/stdlib-query-window-time-short-pkbay.orna"))
        .unwrap_or_else(|error| panic!("short time-series window failed with {}", error.code()));
    assert_eq!(short, Some(canonical(Raw::Array(Vec::new()))));

    let error = session()
        .submit(include_str!("fixtures/stdlib-query-window-time-duplicate-pkbay.orna"))
        .expect_err("equal timestamps in a complete window must fail");
    assert_eq!(error.code(), "ORNA-EVAL-VALUE");
}

#[test]
fn time_series_rejects_reversal_between_disjoint_windows() {
    let error = session()
        .submit(include_str!(
            "fixtures/stdlib-query-window-time-boundary-reversal-pkbay.orna"
        ))
        .expect_err("window boundaries must not hide a source timestamp reversal");
    assert_eq!(error.code(), "ORNA-EVAL-VALUE");
}
