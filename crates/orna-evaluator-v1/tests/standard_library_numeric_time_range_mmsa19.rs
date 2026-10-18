use num_bigint::BigInt;
use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Value;

fn decimal(coefficient: i64, exponent10: i64) -> CanonicalValue {
    let value = Value::decimal(BigInt::from(coefficient), BigInt::from(exponent10))
        .expect("expected decimal is valid");
    CanonicalValue::new(value.raw().clone()).expect("expected decimal is canonical")
}

// Specified: ORNA-NUM-005; decimal division stays exact unless its rounding is explicitly selected.
#[test]
fn decimal_division_requires_exactness_or_explicit_rounding() {
    let mut session = AdmittedReplSession::new(Limits::default());

    assert_eq!(
        session.submit("1.decimal / 8.decimal"),
        Ok(Some(decimal(125, -3))),
        "finite decimal quotients remain exact"
    );
    assert_eq!(
        session
            .submit("1.decimal / 3.decimal")
            .unwrap_err()
            .code(),
        "InexactDivision",
        "non-terminating decimal quotients fail without an explicit precision"
    );
    assert_eq!(
        session.submit("1.decimal.divide(3.decimal, scale: 6, rounding: half_even)"),
        Ok(Some(decimal(333_333, -6))),
        "explicit half-even precision rounds only the selected quotient"
    );
    assert_eq!(
        session.submit("1.decimal.divide(value: 3.decimal, scale: 3, rounding: half_even)"),
        Ok(Some(decimal(333, -3))),
        "named divisor follows the Decimal.divide parameter contract"
    );
    assert_eq!(
        session.submit("1.decimal.divide(8.decimal, scale: 2, rounding: half_even)"),
        Ok(Some(decimal(12, -2))),
        "half-even rounds a midpoint to the even final digit"
    );
}

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(orna_value_v1::Raw::Bool(value)).expect("boolean is canonical")
}

fn standard_session() -> AdmittedReplSession {
    AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("reference standard profile is admitted")
}

// Specified: ORNA-FLOAT-004; left-to-right binary64 arithmetic must not be reassociated.
#[test]
fn float_arithmetic_preserves_source_evaluation_order() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let source = "((10000000000000000.0f + (-10000000000000000.0f)) + 1.0f) == 1.0f";
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}

// Specified: ORNA-TIME-001; telemetry rate points use absolute Instant timestamps.
#[test]
fn telemetry_rate_uses_instant_timestamps() {
    let mut session = standard_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-stats-z09xc.orna")),
        Ok(None)
    );
    let source = "stats.rate([(2024-01-01T00:00:00Z, 1), (2024-01-01T00:00:02Z, 5)]) == first([2])";
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}

// Specified: ORNA-TIME-002; calendar-day buckets require an explicit time zone.
#[test]
fn instant_calendar_buckets_require_and_use_the_selected_zone() {
    let mut session = standard_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-query-2213.orna")),
        Ok(None)
    );
    assert!(session
        .submit("query_ops.bucket_by([2024-03-31T00:30:00Z], 1.day)")
        .is_err());

    let source = "query_ops.bucket_by([2024-03-31T00:30:00Z, 2024-03-31T23:30:00Z], 1.day, \"Europe/London\") == [[2024-03-31T00:30:00Z], [2024-03-31T23:30:00Z]]";
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}

// Specified: ORNA-RANGE-007; overlapping ranges remain unequal and compare by their bounds.
#[test]
fn overlapping_ranges_are_not_equal_and_keep_lexicographic_order() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let source = "(1..=5) != (4..=8) && (1..=5) < (1..=6)";
    assert_eq!(session.submit(source), Ok(Some(bool_value(true))), "{source}");
}
