use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

#[test]
fn pinned_duration_formatters_preserve_exact_fractional_elapsed_values() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-time-duration-decimal-compact-6ev0.orna"),
            "1.25s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-clock-6ev0.orna"),
            "01:30:00",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-words-6ev0.orna"),
            "0.5 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-iso-6ev0.orna"),
            "PT0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-negative-6ev0.orna"),
            "-0.5s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-zero-6ev0.orna"),
            "PT0S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-large-clock-6ev0.orna"),
            "2501999792983:36:33",
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(text(expected))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn duration_formatter_rejects_subnanosecond_and_calendar_period_inputs() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-subnanosecond-6ev0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-calendar-period-6ev0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S021-TYPE"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-bad-zone-6ev0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}

#[test]
fn exact_decimal_duration_respects_seconds_digit_limit_not_nanosecond_intermediate() {
    let limits = Limits {
        max_integer_digits: 10,
        ..Limits::default()
    };
    let mut session = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna")),
        Ok(None)
    );
    let source = include_str!("fixtures/stdlib-time-duration-limited-digits-gy9ln.orna");
    let result = session.submit(source);
    assert_eq!(
        result,
        Ok(Some(text("277777:46:39"))),
        "{source}; diagnostic={}",
        result.as_ref().err().map_or("none", |error| error.code())
    );
}

#[test]
fn integer_duration_units_enforce_limit_after_seconds_scaling() {
    let limits = Limits {
        max_integer_digits: 5,
        ..Limits::default()
    };
    let mut session = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-hour-boundary-vfh0m.orna")),
        Ok(Some(text("27:00:00")))
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-hour-overflow-vfh0m.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );

    let limits = Limits {
        max_integer_digits: 4,
        ..Limits::default()
    };
    let mut session = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-minute-overflow-vfh0m.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn duration_clock_format_keeps_fractional_minute_and_sign_boundaries() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna")),
        Ok(None)
    );
    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-time-duration-before-minute-vfh0m.orna"),
            "00:00:59.999999999",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-after-minute-vfh0m.orna"),
            "00:01:00.000000001",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-negative-nanosecond-vfh0m.orna"),
            "-00:00:00.000000001",
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(text(expected))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}
