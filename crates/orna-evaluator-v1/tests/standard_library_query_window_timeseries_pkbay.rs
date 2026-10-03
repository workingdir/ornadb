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
        .unwrap_or_else(|error| panic!("reference standard profile failed with {}", error.code()));
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
fn sparse_statistic_windows_keep_their_source_time_order() {
    let actual = session()
        .submit(include_str!(
            "fixtures/stdlib-query-time-order-sparse-windows-d05h9.orna"
        ))
        .unwrap_or_else(|error| panic!("sparse ordered window values failed with {}", error.code()));
    assert_eq!(
        actual,
        Some(canonical(Raw::Array(vec![
            Raw::Bool(true),
            Raw::Bool(true),
            Raw::Bool(true),
        ])))
    );
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
fn query_windows_keep_sparse_step_boundaries_and_omit_the_short_tail() {
    let mut session = session();
    let rate = session
        .submit(include_str!("fixtures/stdlib-query-window-rate-deep-sparse-hb3lu.orna"))
        .unwrap_or_else(|error| panic!("deep sparse window rate failed with {}", error.code()));
    assert_eq!(rate, Some(optional_ints(&[2, 2])));

    let derivative = session
        .submit(include_str!("fixtures/stdlib-query-window-derivative-deep-sparse-hb3lu.orna"))
        .unwrap_or_else(|error| panic!("deep sparse window derivative failed with {}", error.code()));
    assert_eq!(derivative, Some(canonical(Raw::Bool(true))));

    let integral = session
        .submit(include_str!("fixtures/stdlib-query-window-integrate-deep-sparse-hb3lu.orna"))
        .unwrap_or_else(|error| panic!("deep sparse window integration failed with {}", error.code()));
    assert_eq!(integral, Some(optional_ints(&[1, 40])));
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
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}

#[test]
fn time_series_rejects_reversal_between_disjoint_windows() {
    let error = session()
        .submit(include_str!(
            "fixtures/stdlib-query-window-time-boundary-reversal-pkbay.orna"
        ))
        .expect_err("window boundaries must not hide a source timestamp reversal");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}

#[test]
fn time_series_checks_ordering_inside_a_multi_point_sparse_gap() {
    let error = session()
        .submit(include_str!(
            "fixtures/stdlib-query-window-time-gap-reversal-neuwd.orna"
        ))
        .expect_err("ordering inside skipped positions must be validated");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}

#[test]
fn time_series_statistics_preserve_elapsed_time_after_a_deeper_sparse_gap() {
    let mut session = session();
    let rate = session
        .submit(include_str!("fixtures/stdlib-query-window-rate-deep-gap-neuwd.orna"))
        .unwrap_or_else(|error| panic!("deep-gap window rate failed with {}", error.code()));
    assert_eq!(rate, Some(optional_ints(&[2, 2])));

    let derivative = session
        .submit(include_str!("fixtures/stdlib-query-window-derivative-deep-gap-neuwd.orna"))
        .unwrap_or_else(|error| panic!("deep-gap derivative failed with {}", error.code()));
    assert_eq!(derivative, Some(canonical(Raw::Bool(true))));

    let integral = session
        .submit(include_str!("fixtures/stdlib-query-window-integrate-deep-gap-neuwd.orna"))
        .unwrap_or_else(|error| panic!("deep-gap integration failed with {}", error.code()));
    assert_eq!(integral, Some(optional_ints(&[1, 51])));
}

#[test]
fn time_series_window_rejects_a_singleton_source_even_without_complete_windows() {
    let error = session()
        .submit(include_str!(
            "fixtures/stdlib-query-window-time-singleton-pkbay.orna"
        ))
        .expect_err("Section 9 time-series statistics require at least two source points");
    assert_eq!(error.code(), "ORNA-EVAL-ERROR");
}

#[test]
fn time_series_window_validates_bounds_before_sparse_source_cardinality() {
    let mut session = session();
    let invalid_size = session
        .submit(include_str!("fixtures/stdlib-query-window-time-invalid-size-hb3lu.orna"))
        .expect_err("nonpositive window size must fail before source-series validation");
    assert_eq!(invalid_size.code(), "ORNA-EVAL-ERROR");

    let invalid_step = session
        .submit(include_str!("fixtures/stdlib-query-window-time-invalid-step-hb3lu.orna"))
        .expect_err("nonpositive window step must fail before source-series validation");
    assert_eq!(invalid_step.code(), "ORNA-EVAL-ERROR");

    let singleton_window = session
        .submit(include_str!("fixtures/stdlib-query-window-time-singleton-window-hb3lu.orna"))
        .expect_err("each time-series statistic window must contain two points");
    assert_eq!(singleton_window.code(), "ORNA-EVAL-ERROR");
}
