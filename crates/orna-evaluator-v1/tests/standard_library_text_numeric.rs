use orna_evaluator_v1::{AdmittedReplSession, Environment, Limits, evaluate_expression};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_text_and_numeric_modules_expose_their_documented_behaviour() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for import in [
        include_str!("fixtures/stdlib-use-text-z09xc.orna"),
        include_str!("fixtures/stdlib-use-bits-z09xc.orna"),
        include_str!("fixtures/stdlib-use-math-z09xc.orna"),
        include_str!("fixtures/stdlib-use-stats-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None));
    }
    let actual = session
        .submit(include_str!("fixtures/stdlib-text-numeric-z09xc.orna"))
        .unwrap_or_else(|error| panic!("text/numeric fixture failed: {}", error.code()));
    assert_eq!(actual, Some(bool_value(true)));
}

#[test]
fn pinned_text_casing_and_decimal_edges_return_exact_values() {
    assert_eq!(unicode_case_mapping::UNICODE_VERSION, (16, 0, 0));
    assert_eq!(unicode_normalization::UNICODE_VERSION, (16, 0, 0));
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for import in [
        include_str!("fixtures/stdlib-use-text-z09xc.orna"),
        include_str!("fixtures/stdlib-use-math-z09xc.orna"),
    ] {
        assert_eq!(session.submit(import), Ok(None));
    }
    assert_eq!(
        session.submit(include_str!(
            "fixtures/stdlib-text-numeric-edges-etsj1.orna"
        )),
        Ok(Some(bool_value(true)))
    );

    let mut without_std = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        without_std
            .submit("1.decimal / 3.decimal")
            .unwrap_err()
            .code(),
        "InexactDivision"
    );
}

#[test]
fn pinned_statistics_exports_compute_all_aggregate_and_series_results() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stats-z09xc.orna")),
        Ok(None)
    );
    let actual = session
        .submit(include_str!(
            "fixtures/stdlib-stats-complete-behavior-yn4vz.orna"
        ))
        .unwrap_or_else(|error| panic!("statistics behavior fixture failed: {}", error.code()));
    assert_eq!(actual, Some(bool_value(true)));
}

#[test]
fn exact_core_numeric_types_remain_available_without_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-core-numeric-z09xc.orna")),
        Ok(Some(bool_value(true)))
    );
}

#[test]
fn decimal_exponent_underflow_returns_a_bounded_error() {
    let failure = evaluate_expression(
        include_str!("fixtures/stdlib-decimal-exponent-underflow-pyyhx.orna"),
        &Environment::new(),
        Limits::default(),
    )
    .unwrap_err();
    assert_eq!(failure.code(), "ORNA-EVAL-LIMIT");
}
