use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

fn texts(values: &[&str]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values
            .iter()
            .map(|value| Raw::Text((*value).to_owned()))
            .collect(),
    ))
    .unwrap()
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
        (
            include_str!("fixtures/stdlib-time-duration-chain-compact-y40kw.orna"),
            "0.000000001s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-chain-clock-y40kw.orna"),
            "00:00:00.000000001",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-chain-words-y40kw.orna"),
            "0.000000001 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-chain-iso-y40kw.orna"),
            "PT0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-minute-compact-mncjs.orna"),
            "1m 0.00012s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-minute-clock-mncjs.orna"),
            "00:01:00.00012",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-minute-words-mncjs.orna"),
            "1 minute, 0.00012 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-minute-iso-mncjs.orna"),
            "PT1M0.00012S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-minute-compact-kzs8s.orna"),
            "59.999999999s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-minute-clock-kzs8s.orna"),
            "00:00:59.999999999",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-minute-words-kzs8s.orna"),
            "59.999999999 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-minute-iso-kzs8s.orna"),
            "PT59.999999999S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-minute-compact-kzs8s.orna"),
            "1m 0.000000001s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-minute-clock-kzs8s.orna"),
            "00:01:00.000000001",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-minute-words-kzs8s.orna"),
            "1 minute, 0.000000001 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-minute-iso-kzs8s.orna"),
            "PT1M0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-hour-compact-u9jg9.orna"),
            "59m 59.999999999s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-hour-clock-u9jg9.orna"),
            "00:59:59.999999999",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-hour-words-u9jg9.orna"),
            "59 minutes, 59.999999999 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-before-hour-iso-u9jg9.orna"),
            "PT59M59.999999999S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-hour-compact-u9jg9.orna"),
            "1h 0.000000001s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-hour-clock-u9jg9.orna"),
            "01:00:00.000000001",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-hour-words-u9jg9.orna"),
            "1 hour, 0.000000001 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scaled-after-hour-iso-u9jg9.orna"),
            "PT1H0.000000001S",
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
fn elapsed_duration_scaling_keeps_nanosecond_precision_and_sign() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna")),
        Ok(None)
    );

    for (source, expected) in [
        (
            include_str!("fixtures/stdlib-time-duration-scale-multiply-mbiom.orna"),
            "PT0.75S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scale-commuted-mbiom.orna"),
            "-PT1.5S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scale-divide-mbiom.orna"),
            "PT0.5S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-scale-negative-divide-mbiom.orna"),
            "-PT0.5S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-multiply-c57w6.orna"),
            "PT0.125S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-commuted-c57w6.orna"),
            "-PT0.75S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-divide-c57w6.orna"),
            "PT2S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-negative-divide-kmy4x.orna"),
            "-PT0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-nanosecond-divide-kmy4x.orna"),
            "PT0.000000002S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-negative-product-kmy4x.orna"),
            "PT1S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-zero-product-kmy4x.orna"),
            "PT0S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-nanosecond-product-28a35.orna"),
            "PT0.000000001S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-reciprocal-28a35.orna"),
            "PT1S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-decimal-scale-negative-reciprocal-28a35.orna"),
            "PT0.000000002S",
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
            .submit(include_str!("fixtures/stdlib-time-duration-scale-inexact-mbiom.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-scale-zero-mbiom.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-decimal-scale-inexact-c57w6.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-decimal-scale-subnanosecond-c57w6.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-decimal-scale-zero-c57w6.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-DIVIDE-BY-ZERO"
    );

    let limits = Limits {
        max_integer_digits: 4,
        ..Limits::default()
    };
    let mut bounded = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        bounded.submit(include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        bounded
            .submit(include_str!("fixtures/stdlib-time-duration-scale-overflow-mbiom.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );
    assert_eq!(
        bounded
            .submit(include_str!("fixtures/stdlib-time-duration-decimal-scale-overflow-c57w6.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-LIMIT"
    );
}

#[test]
fn decimal_duration_scaling_chains_preserve_exact_tails_across_instants() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-scale-chain-7agp9.orna")),
        Ok(Some(text("PT0.000000001S")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-scale-multistep-chain-opffs.orna")),
        Ok(Some(text("PT0.000000001S")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-scale-cancel-negative-chain-opffs.orna")),
        Ok(Some(text("PT0S")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-scale-instant-boundary-7agp9.orna")),
        Ok(Some(boolean(true)))
    );
    assert_eq!(
        session
            .submit(include_str!("fixtures/stdlib-time-duration-scale-inexact-chain-7agp9.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );

    let limits = Limits {
        max_integer_digits: 4,
        ..Limits::default()
    };
    let mut bounded = AdmittedReplSession::with_reference_standard(limits).unwrap();
    assert_eq!(
        bounded.submit(include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna")),
        Ok(None)
    );
    assert_eq!(
        bounded
            .submit(include_str!("fixtures/stdlib-time-duration-scale-overflow-chain-opffs.orna"))
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
            include_str!("fixtures/stdlib-time-duration-clock-negative-scaled-before-minute-efsv6.orna"),
            "-00:00:59.999999999",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-scaled-after-minute-efsv6.orna"),
            "-00:01:00.000000001",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-tail-before-minute-iqqdy.orna"),
            "-00:00:59.99988",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-tail-after-minute-iqqdy.orna"),
            "-00:01:00.00012",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-factor-before-minute-1lj0e.orna"),
            "-00:00:59.99988",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-factor-after-minute-1lj0e.orna"),
            "-00:01:00.00012",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-fine-factor-before-minute-9cfl2.orna"),
            "-00:00:59.99999997",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-fine-factor-after-minute-9cfl2.orna"),
            "-00:01:00.00000003",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-fine-factor-chain-before-minute-x6lyg.orna"),
            "-00:00:59.99999997",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-fine-factor-chain-after-minute-x6lyg.orna"),
            "-00:01:00.00000003",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-factor-closure-before-minute-za8t2.orna"),
            "-00:00:59.999999994",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-negative-factor-closure-after-minute-za8t2.orna"),
            "-00:01:00.000000006",
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
fn duration_formatters_keep_positive_factor_products_on_the_correct_side_of_a_minute() {
    // Orna-1.0.0 shows minute formatting but leaves fractional-factor edge
    // precision unspecified. Pin exact nanoseconds and decompose at 60 seconds.
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
            include_str!("fixtures/stdlib-time-duration-compact-factor-before-minute-96nep.orna"),
            "59.999999994s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-compact-factor-after-minute-96nep.orna"),
            "1m 0.000000006s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-factor-before-minute-96nep.orna"),
            "00:00:59.999999994",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-factor-after-minute-96nep.orna"),
            "00:01:00.000000006",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-words-factor-before-minute-96nep.orna"),
            "59.999999994 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-words-factor-after-minute-96nep.orna"),
            "1 minute, 0.000000006 seconds",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-iso-factor-before-minute-96nep.orna"),
            "PT59.999999994S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-iso-factor-after-minute-96nep.orna"),
            "PT1M0.000000006S",
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
fn positive_factor_tails_keep_the_minute_edge_after_duration_arithmetic() {
    // The reference is silent on scaled-tail addition and subtraction. Keep
    // exact nanoseconds through arithmetic: five ns leaves a 1 ns tail, while
    // six ns closes the factor-produced tail exactly at one minute.
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
            include_str!("fixtures/stdlib-time-duration-factor-tail-before-minute-p9kew.orna"),
            texts(&[
                "59.999999999s",
                "00:00:59.999999999",
                "59.999999999 seconds",
                "PT59.999999999S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-tail-at-minute-from-below-p9kew.orna"),
            texts(&["1m", "00:01:00", "1 minute", "PT1M"]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-tail-after-minute-p9kew.orna"),
            texts(&[
                "1m 0.000000001s",
                "00:01:00.000000001",
                "1 minute, 0.000000001 seconds",
                "PT1M0.000000001S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-tail-at-minute-from-above-p9kew.orna"),
            texts(&["1m", "00:01:00", "1 minute", "PT1M"]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_tail_chains_keep_the_minute_side_after_rescaling() {
    // The reference is silent on rescaling a factor-derived near-minute tail.
    // Pin exact nanoseconds: doubling a 1 ns edge tail leaves 2 ns around 2m.
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
            include_str!("fixtures/stdlib-time-duration-factor-tail-chain-before-minute-rfzcx.orna"),
            texts(&[
                "1m 59.999999998s",
                "00:01:59.999999998",
                "1 minute, 59.999999998 seconds",
                "PT1M59.999999998S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-tail-chain-after-minute-rfzcx.orna"),
            texts(&[
                "2m 0.000000002s",
                "00:02:00.000000002",
                "2 minutes, 0.000000002 seconds",
                "PT2M0.000000002S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_values_around_two_keep_the_minute_boundary_tails() {
    // The reference does not specify fractional factors that scale a
    // subminute duration across one minute. Pin the exact 3 ns edge tails.
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
            include_str!("fixtures/stdlib-time-duration-factor-near-two-before-minute-2p80b.orna"),
            texts(&[
                "59.999999997s",
                "00:00:59.999999997",
                "59.999999997 seconds",
                "PT59.999999997S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-near-two-after-minute-2p80b.orna"),
            texts(&[
                "1m 0.000000003s",
                "00:01:00.000000003",
                "1 minute, 0.000000003 seconds",
                "PT1M0.000000003S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_values_around_three_keep_the_minute_remainder_tails() {
    // The reference leaves fractional-factor remainders unspecified. Pin
    // exact nanoseconds for 20 seconds scaled by factors just below/above 3.
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
            include_str!("fixtures/stdlib-time-duration-factor-near-three-before-minute-b2kid.orna"),
            texts(&[
                "59.999999998s",
                "00:00:59.999999998",
                "59.999999998 seconds",
                "PT59.999999998S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-near-three-after-minute-b2kid.orna"),
            texts(&[
                "1m 0.000000002s",
                "00:01:00.000000002",
                "1 minute, 0.000000002 seconds",
                "PT1M0.000000002S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_values_around_six_keep_single_nanosecond_minute_remainders() {
    // The reference leaves fractional-factor remainders unspecified. Pin the
    // final one-nanosecond tails for 10 seconds scaled just below/above 6.
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
            include_str!("fixtures/stdlib-time-duration-factor-near-six-before-minute-3894o.orna"),
            texts(&[
                "59.999999999s",
                "00:00:59.999999999",
                "59.999999999 seconds",
                "PT59.999999999S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-near-six-after-minute-3894o.orna"),
            texts(&[
                "1m 0.000000001s",
                "00:01:00.000000001",
                "1 minute, 0.000000001 seconds",
                "PT1M0.000000001S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_subnanosecond_remainders_at_minute_edges_are_rejected() {
    // The reference is silent on fractional-factor products below Duration's
    // nanosecond quantum. Keep the evaluator's exactness rule at both sides of
    // the minute: 59.9999999995s and 60.0000000005s are not rounded.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna")),
        Ok(None)
    );

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-half-ns-before-minute-f6bmg.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-half-ns-after-minute-f6bmg.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result.unwrap_err().code(),
            "ORNA-EVAL-VALUE",
            "{source}"
        );
    }
}

#[test]
fn positive_factor_subnanosecond_tails_compose_to_exact_minute_edges() {
    // The reference is silent on composing factors before duration scaling.
    // Each near-two factor alone leaves a half-nanosecond on five seconds;
    // multiplying it by six first yields exact 3 ns tails around one minute.
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
            include_str!("fixtures/stdlib-time-duration-factor-composed-half-ns-before-minute-f7dqb.orna"),
            texts(&[
                "59.999999997s",
                "00:00:59.999999997",
                "59.999999997 seconds",
                "PT59.999999997S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-composed-half-ns-after-minute-f7dqb.orna"),
            texts(&[
                "1m 0.000000003s",
                "00:01:00.000000003",
                "1 minute, 0.000000003 seconds",
                "PT1M0.000000003S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_subnanosecond_tails_rescale_through_duration_edges() {
    // The reference is silent on sequential factor rescaling. Scaling five
    // seconds by six first makes 30 seconds, so a near-two factor then yields
    // exact 3 ns tails instead of the half-nanosecond product on five seconds.
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
            include_str!("fixtures/stdlib-time-duration-factor-rescaled-half-ns-before-minute-0okws.orna"),
            texts(&[
                "59.999999997s",
                "00:00:59.999999997",
                "59.999999997 seconds",
                "PT59.999999997S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-rescaled-half-ns-after-minute-0okws.orna"),
            texts(&[
                "1m 0.000000003s",
                "00:01:00.000000003",
                "1 minute, 0.000000003 seconds",
                "PT1M0.000000003S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_subnanosecond_tails_rescale_across_the_second_minute_edge() {
    // The reference is silent on rescaling subnanosecond-derived minute tails.
    // Carry the exact 3 ns tail through one more doubling to pin the 6 ns edge
    // immediately below and above two minutes.
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
            include_str!("fixtures/stdlib-time-duration-factor-rescaled-second-minute-before-mtdzz.orna"),
            texts(&[
                "1m 59.999999994s",
                "00:01:59.999999994",
                "1 minute, 59.999999994 seconds",
                "PT1M59.999999994S",
            ]),
        ),
        (
            include_str!("fixtures/stdlib-time-duration-factor-rescaled-second-minute-after-mtdzz.orna"),
            texts(&[
                "2m 0.000000006s",
                "00:02:00.000000006",
                "2 minutes, 0.000000006 seconds",
                "PT2M0.000000006S",
            ]),
        ),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(expected)),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_subnanosecond_rescaled_tails_close_at_two_minutes() {
    // The reference is silent on closing a rescaled subnanosecond-derived
    // minute tail. Pin the exact 6 ns correction from either side of 2m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-from-below-f55yc.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-from-above-f55yc.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&["2m", "00:02:00", "2 minutes", "PT2M"]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_second_doubling() {
    // The reference is silent on closing a factor tail after repeated
    // rescaling. Two doublings carry the 3 ns tail to 12 ns around four
    // minutes; an exact 12 ns correction closes both sides at 4m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-second-doubling-from-below-3ohyv.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-second-doubling-from-above-3ohyv.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&["4m", "00:04:00", "4 minutes", "PT4M"]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_third_doubling() {
    // The reference is silent on closing subnanosecond-derived tails after
    // another rescaling step. Three doublings carry the 3 ns tail to 24 ns
    // around eight minutes; correcting by 24 ns closes both sides at 8m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-third-doubling-from-below-9ymm6.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-third-doubling-from-above-9ymm6.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&["8m", "00:08:00", "8 minutes", "PT8M"]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fourth_doubling() {
    // The reference is silent on closing the tail after one further scaling
    // step. Four doublings carry the 3 ns tail to 48 ns around sixteen
    // minutes; correcting by 48 ns closes both sides at 16m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fourth-doubling-from-below-ghs9x.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fourth-doubling-from-above-ghs9x.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&["16m", "00:16:00", "16 minutes", "PT16M"]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Five doublings carry the 3 ns tail to 96 ns around thirty-two
    // minutes; correcting by 96 ns closes both sides at 32m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifth-doubling-from-below-9tosd.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifth-doubling-from-above-9tosd.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&["32m", "00:32:00", "32 minutes", "PT32M"]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Six doublings carry the 3 ns tail to 192 ns around sixty-four
    // minutes; correcting by 192 ns closes both sides at 64m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixth-doubling-from-below-sx8m7.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixth-doubling-from-above-sx8m7.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1h 4m",
                "01:04:00",
                "1 hour, 4 minutes",
                "PT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventh_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Seven doublings carry the 3 ns tail to 384 ns around two hours;
    // correcting by 384 ns closes both sides at 128m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventh-doubling-from-below-81ori.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventh-doubling-from-above-81ori.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "2h 8m",
                "02:08:00",
                "2 hours, 8 minutes",
                "PT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Eight doublings carry the 3 ns tail to 768 ns around 256 minutes;
    // correcting by 768 ns closes both sides at 4h 16m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighth-doubling-from-below-g7ako.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighth-doubling-from-above-g7ako.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "4h 16m",
                "04:16:00",
                "4 hours, 16 minutes",
                "PT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Nine doublings carry the 3 ns tail to 1,536 ns around 512 minutes;
    // correcting by 1,536 ns closes both sides at 8h 32m.
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for source in [
        include_str!("fixtures/stdlib-time-duration-use-compact-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-clock-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-words-b1e0.orna"),
        include_str!("fixtures/stdlib-time-duration-use-iso-b1e0.orna"),
    ] {
        assert_eq!(session.submit(source), Ok(None), "{source}");
    }

    for source in [
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninth-doubling-from-below-ty93h.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninth-doubling-from-above-ty93h.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "8h 32m",
                "08:32:00",
                "8 hours, 32 minutes",
                "PT8H32M",
            ]))),
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
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-scaled-before-24-hours-2j4s2.orna")),
        Ok(Some(text("23:59:59.999999999")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-scaled-after-24-hours-2j4s2.orna")),
        Ok(Some(text("24:00:00.000000001")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-negative-scaled-before-24-hours-c2zdh.orna")),
        Ok(Some(text("-23:59:59.999999999")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-negative-scaled-after-24-hours-c2zdh.orna")),
        Ok(Some(text("-24:00:00.000000001")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-scaled-before-24h-tail-auorn.orna")),
        Ok(Some(text("23:59:59.99988")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-scaled-after-24h-tail-auorn.orna")),
        Ok(Some(text("24:00:00.00012")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-scaled-negative-before-24h-tail-8g41m.orna")),
        Ok(Some(text("-23:59:59.99988")))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-time-duration-clock-scaled-negative-after-24h-tail-8g41m.orna")),
        Ok(Some(text("-24:00:00.00012")))
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
