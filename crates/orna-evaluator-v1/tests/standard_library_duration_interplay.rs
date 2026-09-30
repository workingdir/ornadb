use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
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

#[test]
fn clock_output_bound_keeps_elapsed_hours_and_fractional_tail() {
    let limits = Limits {
        max_string_bytes: 129,
        max_integer_digits: 125,
        ..Limits::default()
    };
    let mut session = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-24-hours-h4ei4.orna")),
        Ok(Some(text("24:00:00")))
    );
    let source = include_str!("fixtures/stdlib-time-duration-clock-max-hours-h4ei4.orna");
    let result = session.submit(source);
    let expected = format!("1{}:00:00.000000001", "0".repeat(112));
    assert_eq!(
        result,
        Ok(Some(text(&expected))),
        "{source}; diagnostic={}",
        result.as_ref().err().map_or("none", |error| error.code())
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-clock-calendar-period-h4ei4.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S021-TYPE"
    );

    let limits = Limits {
        max_string_bytes: 128,
        max_integer_digits: 125,
        ..Limits::default()
    };
    let mut bounded = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        bounded.submit(include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        bounded
            .submit(include_str!("fixtures/stdlib-time-duration-clock-max-hours-h4ei4.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn core_elapsed_instant_arithmetic_works_without_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    for source in [
        include_str!("fixtures/stdlib-time-instant-add-duration-y2w0.orna"),
        include_str!("fixtures/stdlib-time-duration-add-instant-y2w0.orna"),
        include_str!("fixtures/stdlib-time-instant-subtract-duration-y2w0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(Some(boolean(true))), "{source}");
    }
}

#[test]
fn pinned_std_formats_normalized_elapsed_arithmetic() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-elapsed-duration-use-iso-y2w0.orna")),
        Ok(None)
    );
    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-time-elapsed-duration-sum-y2w0.orna"),
            "PT1S",
        ),
        (
            include_str!("fixtures/stdlib-time-elapsed-duration-difference-y2w0.orna"),
            "-PT0.25S",
        ),
        (
            include_str!("fixtures/stdlib-time-elapsed-duration-negation-y2w0.orna"),
            "-PT0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-instant-difference-y2w0.orna"),
            "PT0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-instant-negative-difference-y2w0.orna"),
            "-PT0.5S",
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
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-instant-overflow-y2w0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-instant-calendar-period-y2w0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-S021-TYPE"
    );
}

#[test]
fn elapsed_duration_arithmetic_enforces_result_digit_limit() {
    let limits = Limits {
        max_integer_digits: 2,
        ..Limits::default()
    };
    let mut session = AdmittedReplSession::new(limits);
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-sum-digit-limit-y2w0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );

    let limits = Limits {
        max_integer_digits: 11,
        ..Limits::default()
    };
    let mut session = AdmittedReplSession::new(limits);
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-instant-difference-digit-limit-y2w0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );
}
