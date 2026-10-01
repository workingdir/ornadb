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
fn positive_factor_rescaled_tails_close_after_tenth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Ten doublings carry the 3 ns tail to 3,072 ns around 1,024
    // minutes; correcting by 3,072 ns closes both sides at 17h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-tenth-doubling-from-below-huqsy.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-tenth-doubling-from-above-huqsy.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "17h 4m",
                "17:04:00",
                "17 hours, 4 minutes",
                "PT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eleventh_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Eleven doublings carry the 3 ns tail to 6,144 ns around 2,048
    // minutes; correcting by 6,144 ns closes both sides at 1d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eleventh-doubling-from-below-ek7bo.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eleventh-doubling-from-above-ek7bo.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1d 10h 8m",
                "34:08:00",
                "1 day, 10 hours, 8 minutes",
                "P1DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twelfth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Twelve doublings carry the 3 ns tail to 12,288 ns around 4,096
    // minutes; correcting by 12,288 ns closes both sides at 2d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twelfth-doubling-from-below-mav32.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twelfth-doubling-from-above-mav32.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "2d 20h 16m",
                "68:16:00",
                "2 days, 20 hours, 16 minutes",
                "P2DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirteenth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Thirteen doublings carry the 3 ns tail to 24,576 ns around 8,192
    // minutes; correcting by 24,576 ns closes both sides at 5d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirteenth-doubling-from-below-2tfji.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirteenth-doubling-from-above-2tfji.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "5d 16h 32m",
                "136:32:00",
                "5 days, 16 hours, 32 minutes",
                "P5DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fourteenth_doubling() {
    // The reference is silent on closing the tail after another scaling
    // step. Fourteen doublings carry the 3 ns tail to 49,152 ns around
    // 16,384 minutes; correcting by 49,152 ns closes both sides at 11d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fourteenth-doubling-from-below-cz3dj.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fourteenth-doubling-from-above-cz3dj.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "11d 9h 4m",
                "273:04:00",
                "11 days, 9 hours, 4 minutes",
                "P11DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifteenth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 98,304 ns around 32,768 minutes, pinning 22d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifteenth-doubling-from-below-rsy9k.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifteenth-doubling-from-above-rsy9k.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "22d 18h 8m",
                "546:08:00",
                "22 days, 18 hours, 8 minutes",
                "P22DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixteenth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 196,608 ns around 65,536 minutes, pinning 45d 12h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixteenth-doubling-from-below-94n07.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixteenth-doubling-from-above-94n07.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "45d 12h 16m",
                "1092:16:00",
                "45 days, 12 hours, 16 minutes",
                "P45DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventeenth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 393,216 ns around 131,072 minutes, pinning 91d 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventeenth-doubling-from-below-yiis7.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventeenth-doubling-from-above-yiis7.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "91d 32m",
                "2184:32:00",
                "91 days, 32 minutes",
                "P91DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighteenth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 786,432 ns around 262,144 minutes, pinning 182d 1h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighteenth-doubling-from-below-5pwio.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighteenth-doubling-from-above-5pwio.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "182d 1h 4m",
                "4369:04:00",
                "182 days, 1 hour, 4 minutes",
                "P182DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_nineteenth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 1,572,864 ns around 524,288 minutes, pinning 364d 2h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-nineteenth-doubling-from-below-3kipa.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-nineteenth-doubling-from-above-3kipa.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "364d 2h 8m",
                "8738:08:00",
                "364 days, 2 hours, 8 minutes",
                "P364DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twentieth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 3,145,728 ns around 1,048,576 minutes, pinning 728d 4h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twentieth-doubling-from-below-5hesi.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twentieth-doubling-from-above-5hesi.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "728d 4h 16m",
                "17476:16:00",
                "728 days, 4 hours, 16 minutes",
                "P728DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_first_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 6,291,456 ns around 2,097,152 minutes, pinning 1456d 8h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-first-doubling-from-below-3zcm5.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-first-doubling-from-above-3zcm5.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1456d 8h 32m",
                "34952:32:00",
                "1456 days, 8 hours, 32 minutes",
                "P1456DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_second_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 12,582,912 ns around 4,194,304 minutes, pinning 2912d 17h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-second-doubling-from-below-lb4fd.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-second-doubling-from-above-lb4fd.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "2912d 17h 4m",
                "69905:04:00",
                "2912 days, 17 hours, 4 minutes",
                "P2912DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_third_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 25,165,824 ns around 8,388,608 minutes, pinning 5825d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-third-doubling-from-below-rin86.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-third-doubling-from-above-rin86.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "5825d 10h 8m",
                "139810:08:00",
                "5825 days, 10 hours, 8 minutes",
                "P5825DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_fourth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 50,331,648 ns around 16,777,216 minutes, pinning 11650d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-fourth-doubling-from-below-uk8xx.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-fourth-doubling-from-above-uk8xx.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "11650d 20h 16m",
                "279620:16:00",
                "11650 days, 20 hours, 16 minutes",
                "P11650DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_fifth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 100,663,296 ns around 33,554,432 minutes, pinning 23301d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-fifth-doubling-from-below-hmsm4.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-fifth-doubling-from-above-hmsm4.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "23301d 16h 32m",
                "559240:32:00",
                "23301 days, 16 hours, 32 minutes",
                "P23301DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_sixth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 201,326,592 ns around 67,108,864 minutes, pinning 46603d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-sixth-doubling-from-below-yki5c.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-sixth-doubling-from-above-yki5c.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "46603d 9h 4m",
                "1118481:04:00",
                "46603 days, 9 hours, 4 minutes",
                "P46603DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_seventh_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 402,653,184 ns around 134,217,728 minutes, pinning 93206d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-seventh-doubling-from-below-dafue.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-seventh-doubling-from-above-dafue.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "93206d 18h 8m",
                "2236962:08:00",
                "93206 days, 18 hours, 8 minutes",
                "P93206DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_eighth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 805,306,368 ns around 268,435,456 minutes, pinning 186413d 12h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-eighth-doubling-from-below-vds1m.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-eighth-doubling-from-above-vds1m.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "186413d 12h 16m",
                "4473924:16:00",
                "186413 days, 12 hours, 16 minutes",
                "P186413DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_twenty_ninth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 1,610,612,736 ns around 536,870,912 minutes,
    // pinning 372827d 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-ninth-doubling-from-below-jztpq.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-twenty-ninth-doubling-from-above-jztpq.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "372827d 32m",
                "8947848:32:00",
                "372827 days, 32 minutes",
                "P372827DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirtieth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 3,221,225,472 ns around 1,073,741,824 minutes,
    // pinning 745654d 1h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirtieth-doubling-from-below-vetoe.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirtieth-doubling-from-above-vetoe.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "745654d 1h 4m",
                "17895697:04:00",
                "745654 days, 1 hour, 4 minutes",
                "P745654DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_first_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 6,442,450,944 ns around 2,147,483,648 minutes,
    // pinning 1491308d 2h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-first-doubling-from-below-jacpl.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-first-doubling-from-above-jacpl.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1491308d 2h 8m",
                "35791394:08:00",
                "1491308 days, 2 hours, 8 minutes",
                "P1491308DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_second_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 12,884,901,888 ns around 4,294,967,296 minutes,
    // pinning 2982616d 4h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-second-doubling-from-below-ogf2o.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-second-doubling-from-above-ogf2o.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "2982616d 4h 16m",
                "71582788:16:00",
                "2982616 days, 4 hours, 16 minutes",
                "P2982616DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_third_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 25,769,803,776 ns around 8,589,934,592 minutes,
    // pinning 5965232d 8h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-third-doubling-from-below-vgnsi.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-third-doubling-from-above-vgnsi.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "5965232d 8h 32m",
                "143165576:32:00",
                "5965232 days, 8 hours, 32 minutes",
                "P5965232DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_fourth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 51,539,607,552 ns around 17,179,869,184 minutes,
    // pinning 11930464d 17h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-fourth-doubling-from-below-8six5.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-fourth-doubling-from-above-8six5.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "11930464d 17h 4m",
                "286331153:04:00",
                "11930464 days, 17 hours, 4 minutes",
                "P11930464DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_fifth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 103,079,215,104 ns around 34,359,738,368 minutes,
    // pinning 23860929d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-fifth-doubling-from-below-dc54e.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-fifth-doubling-from-above-dc54e.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "23860929d 10h 8m",
                "572662306:08:00",
                "23860929 days, 10 hours, 8 minutes",
                "P23860929DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_sixth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 206,158,430,208 ns around 68,719,476,736 minutes,
    // pinning 47721858d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-sixth-doubling-from-below-5sqnf.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-sixth-doubling-from-above-5sqnf.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "47721858d 20h 16m",
                "1145324612:16:00",
                "47721858 days, 20 hours, 16 minutes",
                "P47721858DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_seventh_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 412,316,860,416 ns around 137,438,953,472 minutes,
    // pinning 95443717d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-seventh-doubling-from-below-pohlo.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-seventh-doubling-from-above-pohlo.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "95443717d 16h 32m",
                "2290649224:32:00",
                "95443717 days, 16 hours, 32 minutes",
                "P95443717DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_eighth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 824,633,720,832 ns around 274,877,906,944 minutes,
    // pinning 190887435d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-eighth-doubling-from-below-b2t19.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-eighth-doubling-from-above-b2t19.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "190887435d 9h 4m",
                "4581298449:04:00",
                "190887435 days, 9 hours, 4 minutes",
                "P190887435DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_thirty_ninth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 1,649,267,441,664 ns around 549,755,813,888 minutes,
    // pinning 381774870d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-ninth-doubling-from-below-uxgoh.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-thirty-ninth-doubling-from-above-uxgoh.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "381774870d 18h 8m",
                "9162596898:08:00",
                "381774870 days, 18 hours, 8 minutes",
                "P381774870DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fortieth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 3,298,534,883,328 ns around 1,099,511,627,776 minutes,
    // pinning 763549741d 12h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fortieth-doubling-from-below-fbdf6.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fortieth-doubling-from-above-fbdf6.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "763549741d 12h 16m",
                "18325193796:16:00",
                "763549741 days, 12 hours, 16 minutes",
                "P763549741DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_first_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 6,597,069,766,656 ns around 2,199,023,255,552 minutes,
    // pinning 1527099483d 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-first-doubling-from-below-tde74.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-first-doubling-from-above-tde74.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1527099483d 32m",
                "36650387592:32:00",
                "1527099483 days, 32 minutes",
                "P1527099483DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_second_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 13,194,139,533,312 ns around 4,398,046,511,104 minutes,
    // pinning 3054198966d 1h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-second-doubling-from-below-jp6xf.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-second-doubling-from-above-jp6xf.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3054198966d 1h 4m",
                "73300775185:04:00",
                "3054198966 days, 1 hour, 4 minutes",
                "P3054198966DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_third_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 26,388,279,066,624 ns around 8,796,093,022,208 minutes,
    // pinning 6108397932d 2h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-third-doubling-from-below-14ekt.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-third-doubling-from-above-14ekt.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "6108397932d 2h 8m",
                "146601550370:08:00",
                "6108397932 days, 2 hours, 8 minutes",
                "P6108397932DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_fourth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 52,776,558,133,248 ns around 17,592,186,044,416 minutes,
    // pinning 12216795864d 4h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-fourth-doubling-from-below-ycs5d.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-fourth-doubling-from-above-ycs5d.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "12216795864d 4h 16m",
                "293203100740:16:00",
                "12216795864 days, 4 hours, 16 minutes",
                "P12216795864DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_fifth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 105,553,116,266,496 ns around 35,184,372,088,832 minutes,
    // pinning 24433591728d 8h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-fifth-doubling-from-below-7c0na.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-fifth-doubling-from-above-7c0na.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "24433591728d 8h 32m",
                "586406201480:32:00",
                "24433591728 days, 8 hours, 32 minutes",
                "P24433591728DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_sixth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 211,106,232,532,992 ns around 70,368,744,177,664 minutes,
    // pinning 48867183456d 17h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-sixth-doubling-from-below-mlchm.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-sixth-doubling-from-above-mlchm.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "48867183456d 17h 4m",
                "1172812402961:04:00",
                "48867183456 days, 17 hours, 4 minutes",
                "P48867183456DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_seventh_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 422,212,465,065,984 ns around 140,737,488,355,328 minutes,
    // pinning 97734366913d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-seventh-doubling-from-below-v60uf.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-seventh-doubling-from-above-v60uf.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "97734366913d 10h 8m",
                "2345624805922:08:00",
                "97734366913 days, 10 hours, 8 minutes",
                "P97734366913DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_eighth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 844,424,930,131,968 ns around 281,474,976,710,656 minutes,
    // pinning 195468733826d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-eighth-doubling-from-below-fmws0.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-eighth-doubling-from-above-fmws0.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "195468733826d 20h 16m",
                "4691249611844:16:00",
                "195468733826 days, 20 hours, 16 minutes",
                "P195468733826DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_forty_ninth_doubling() {
    // The reference is silent on extending positive-factor tail closure this
    // far. Continue the symmetric edge correction through one more scaling:
    // the 3 ns tail becomes 1,688,849,860,263,936 ns around 562,949,953,421,312 minutes,
    // pinning 390937467653d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-ninth-doubling-from-below-rf5yg.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-forty-ninth-doubling-from-above-rf5yg.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "390937467653d 16h 32m",
                "9382499223688:32:00",
                "390937467653 days, 16 hours, 32 minutes",
                "P390937467653DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fiftieth_doubling() {
    // The reference is silent on this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,377,699,720,527,872 ns around
    // 1,125,899,906,842,624 minutes, pinning 781874935307d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fiftieth-doubling-from-below-cp53z.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fiftieth-doubling-from-above-cp53z.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "781874935307d 9h 4m",
                "18764998447377:04:00",
                "781874935307 days, 9 hours, 4 minutes",
                "P781874935307DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_first_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 6,755,399,441,055,744 ns around
    // 2,251,799,813,685,248 minutes, pinning 1563749870614d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-first-doubling-from-below-g5d1u.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-first-doubling-from-above-g5d1u.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1563749870614d 18h 8m",
                "37529996894754:08:00",
                "1563749870614 days, 18 hours, 8 minutes",
                "P1563749870614DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_second_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 13,510,798,882,111,488 ns around
    // 4,503,599,627,370,496 minutes, pinning 3127499741229d 12h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-second-doubling-from-below-gxlme.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-second-doubling-from-above-gxlme.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3127499741229d 12h 16m",
                "75059993789508:16:00",
                "3127499741229 days, 12 hours, 16 minutes",
                "P3127499741229DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_third_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 27,021,597,764,222,976 ns around
    // 9,007,199,254,740,992 minutes, pinning 6254999482459d 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-third-doubling-from-below-n0iky.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-third-doubling-from-above-n0iky.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "6254999482459d 32m",
                "150119987579016:32:00",
                "6254999482459 days, 32 minutes",
                "P6254999482459DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_fourth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 54,043,195,528,445,952 ns around
    // 18,014,398,509,481,984 minutes, pinning 12509998964918d 1h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-fourth-doubling-from-below-csnst.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-fourth-doubling-from-above-csnst.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "12509998964918d 1h 4m",
                "300239975158033:04:00",
                "12509998964918 days, 1 hour, 4 minutes",
                "P12509998964918DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_fifth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 108,086,391,056,891,904 ns around
    // 36,028,797,018,963,968 minutes, pinning 25019997929836d 2h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-fifth-doubling-from-below-h2qin.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-fifth-doubling-from-above-h2qin.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "25019997929836d 2h 8m",
                "600479950316066:08:00",
                "25019997929836 days, 2 hours, 8 minutes",
                "P25019997929836DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_sixth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 216,172,782,113,783,808 ns around
    // 72,057,594,037,927,936 minutes, pinning 50039995859672d 4h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-sixth-doubling-from-below-ninf8.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-sixth-doubling-from-above-ninf8.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "50039995859672d 4h 16m",
                "1200959900632132:16:00",
                "50039995859672 days, 4 hours, 16 minutes",
                "P50039995859672DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_seventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 432,345,564,227,567,616 ns around
    // 144,115,188,075,855,872 minutes, pinning 100079991719344d 8h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-seventh-doubling-from-below-8x2y6.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-seventh-doubling-from-above-8x2y6.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "100079991719344d 8h 32m",
                "2401919801264264:32:00",
                "100079991719344 days, 8 hours, 32 minutes",
                "P100079991719344DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_eighth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 864,691,128,455,135,232 ns around
    // 288,230,376,151,711,744 minutes, pinning 200159983438688d 17h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-eighth-doubling-from-below-27i5c.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-eighth-doubling-from-above-27i5c.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "200159983438688d 17h 4m",
                "4803839602528529:04:00",
                "200159983438688 days, 17 hours, 4 minutes",
                "P200159983438688DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_fifty_ninth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 1,729,382,256,910,270,464 ns
    // around 576,460,752,303,423,488 minutes, pinning 400319966877377d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-ninth-doubling-from-below-0lzmd.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-fifty-ninth-doubling-from-above-0lzmd.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "400319966877377d 10h 8m",
                "9607679205057058:08:00",
                "400319966877377 days, 10 hours, 8 minutes",
                "P400319966877377DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixtieth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,458,764,513,820,540,928 ns
    // around 1,152,921,504,606,846,976 minutes, pinning 800639933754754d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixtieth-doubling-from-below-92vxf.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixtieth-doubling-from-above-92vxf.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "800639933754754d 20h 16m",
                "19215358410114116:16:00",
                "800639933754754 days, 20 hours, 16 minutes",
                "P800639933754754DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_first_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 6,917,529,027,641,081,856 ns
    // around 2,305,843,009,213,693,952 minutes, pinning 1601279867509509d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-first-doubling-from-below-hzxzd.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-first-doubling-from-above-hzxzd.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1601279867509509d 16h 32m",
                "38430716820228232:32:00",
                "1601279867509509 days, 16 hours, 32 minutes",
                "P1601279867509509DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_second_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 13,835,058,055,282,163,712 ns
    // around 4,611,686,018,427,387,904 minutes, pinning 3202559735019019d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-second-doubling-from-below-fm4ke.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-second-doubling-from-above-fm4ke.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3202559735019019d 9h 4m",
                "76861433640456465:04:00",
                "3202559735019019 days, 9 hours, 4 minutes",
                "P3202559735019019DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_third_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 27,670,116,110,564,327,424 ns
    // around 9,223,372,036,854,775,808 minutes, pinning 6405119470038038d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-third-doubling-from-below-l5vbz.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-third-doubling-from-above-l5vbz.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "6405119470038038d 18h 8m",
                "153722867280912930:08:00",
                "6405119470038038 days, 18 hours, 8 minutes",
                "P6405119470038038DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_fourth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 55,340,232,221,128,654,848 ns
    // around 18,446,744,073,709,551,616 minutes, pinning 12810238940076077d 12h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-fourth-doubling-from-below-y7dit.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-fourth-doubling-from-above-y7dit.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "12810238940076077d 12h 16m",
                "307445734561825860:16:00",
                "12810238940076077 days, 12 hours, 16 minutes",
                "P12810238940076077DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_fifth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 110,680,464,442,257,309,696 ns
    // around 36,893,488,147,419,103,232 minutes, pinning 25620477880152155d 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-fifth-doubling-from-below-0q58q.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-fifth-doubling-from-above-0q58q.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "25620477880152155d 32m",
                "614891469123651720:32:00",
                "25620477880152155 days, 32 minutes",
                "P25620477880152155DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_sixth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 221,360,928,884,514,619,392 ns
    // around 73,786,976,294,838,206,464 minutes, pinning 51240955760304310d 1h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-sixth-doubling-from-below-6in0r.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-sixth-doubling-from-above-6in0r.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "51240955760304310d 1h 4m",
                "1229782938247303441:04:00",
                "51240955760304310 days, 1 hour, 4 minutes",
                "P51240955760304310DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_seventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 442,721,857,769,029,238,784 ns
    // around 147,573,952,589,676,412,928 minutes, pinning 102481911520608620d 2h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-seventh-doubling-from-below-08bkb.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-seventh-doubling-from-above-08bkb.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "102481911520608620d 2h 8m",
                "2459565876494606882:08:00",
                "102481911520608620 days, 2 hours, 8 minutes",
                "P102481911520608620DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_eighth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 885,443,715,538,058,477,568 ns
    // around 295,147,905,179,352,825,856 minutes, pinning 204963823041217240d 4h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-eighth-doubling-from-below-59jn6.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-eighth-doubling-from-above-59jn6.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "204963823041217240d 4h 16m",
                "4919131752989213764:16:00",
                "204963823041217240 days, 4 hours, 16 minutes",
                "P204963823041217240DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_sixty_ninth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 1,770,887,431,076,116,955,136 ns
    // around 590,295,810,358,705,651,712 minutes, pinning 409927646082434480d 8h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-ninth-doubling-from-below-csbtw.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-sixty-ninth-doubling-from-above-csbtw.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "409927646082434480d 8h 32m",
                "9838263505978427528:32:00",
                "409927646082434480 days, 8 hours, 32 minutes",
                "P409927646082434480DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventieth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,541,774,862,152,233,910,272 ns
    // around 1,180,591,620,717,411,303,424 minutes, pinning 819855292164868960d 17h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventieth-doubling-from-below-llwf9.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventieth-doubling-from-above-llwf9.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "819855292164868960d 17h 4m",
                "19676527011956855057:04:00",
                "819855292164868960 days, 17 hours, 4 minutes",
                "P819855292164868960DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_first_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 7,083,549,724,304,467,820,544 ns
    // around 2,361,183,241,434,822,606,848 minutes, pinning 1639710584329737921d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-first-doubling-from-below-wsxl1.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-first-doubling-from-above-wsxl1.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1639710584329737921d 10h 8m",
                "39353054023913710114:08:00",
                "1639710584329737921 days, 10 hours, 8 minutes",
                "P1639710584329737921DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_second_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 14,167,099,448,608,935,641,088 ns
    // around 4,722,366,482,869,645,213,696 minutes, pinning 3279421168659475842d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-second-doubling-from-below-2s8vj.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-second-doubling-from-above-2s8vj.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3279421168659475842d 20h 16m",
                "78706108047827420228:16:00",
                "3279421168659475842 days, 20 hours, 16 minutes",
                "P3279421168659475842DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_third_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 28,334,198,897,217,871,282,176 ns
    // around 9,444,732,965,739,290,427,392 minutes, pinning 6558842337318951685d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-third-doubling-from-below-9kc8j.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-third-doubling-from-above-9kc8j.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "6558842337318951685d 16h 32m",
                "157412216095654840456:32:00",
                "6558842337318951685 days, 16 hours, 32 minutes",
                "P6558842337318951685DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_fourth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 56,668,397,794,435,742,564,352 ns
    // around 18,889,465,931,478,580,854,784 minutes, pinning 13117684674637903371d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-fourth-doubling-from-below-re46p.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-fourth-doubling-from-above-re46p.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "13117684674637903371d 9h 4m",
                "314824432191309680913:04:00",
                "13117684674637903371 days, 9 hours, 4 minutes",
                "P13117684674637903371DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_fifth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 113,336,795,588,871,485,128,704 ns
    // around 37,778,931,862,957,161,709,568 minutes, pinning 26235369349275806742d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-fifth-doubling-from-below-03vng.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-fifth-doubling-from-above-03vng.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "26235369349275806742d 18h 8m",
                "629648864382619361826:08:00",
                "26235369349275806742 days, 18 hours, 8 minutes",
                "P26235369349275806742DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_sixth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 226,673,591,177,742,970,257,408 ns
    // around 75,557,863,725,914,323,419,136 minutes, pinning 52470738698551613485d 12h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-sixth-doubling-from-below-613sk.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-sixth-doubling-from-above-613sk.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "52470738698551613485d 12h 16m",
                "1259297728765238723652:16:00",
                "52470738698551613485 days, 12 hours, 16 minutes",
                "P52470738698551613485DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_seventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 453,347,182,355,485,940,514,816 ns
    // around 151,115,727,451,828,646,838,272 minutes, pinning 104941477397103226971d 0h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-seventh-doubling-from-below-crtks.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-seventh-doubling-from-above-crtks.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "104941477397103226971d 32m",
                "2518595457530477447304:32:00",
                "104941477397103226971 days, 32 minutes",
                "P104941477397103226971DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_eighth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 906,694,364,710,971,881,029,632 ns
    // around 302,231,454,903,657,293,676,544 minutes, pinning 209882954794206453942d 1h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-eighth-doubling-from-below-02cp0.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-eighth-doubling-from-above-02cp0.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "209882954794206453942d 1h 4m",
                "5037190915060954894609:04:00",
                "209882954794206453942 days, 1 hour, 4 minutes",
                "P209882954794206453942DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_seventy_ninth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 1,813,388,729,421,943,762,059,264 ns
    // around 604,462,909,807,314,587,353,088 minutes, pinning 419765909588412907884d 2h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-ninth-doubling-from-below-ze5r4.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-seventy-ninth-doubling-from-above-ze5r4.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "419765909588412907884d 2h 8m",
                "10074381830121909789218:08:00",
                "419765909588412907884 days, 2 hours, 8 minutes",
                "P419765909588412907884DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eightieth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,626,777,458,843,887,524,118,528 ns
    // around 1,208,925,819,614,629,174,706,176 minutes, pinning 839531819176825815768d 4h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eightieth-doubling-from-below-s2sja.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eightieth-doubling-from-above-s2sja.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "839531819176825815768d 4h 16m",
                "20148763660243819578436:16:00",
                "839531819176825815768 days, 4 hours, 16 minutes",
                "P839531819176825815768DT4H16M",
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

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_first_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 7,253,554,917,687,775,048,237,056 ns
    // around 2,417,851,639,229,258,349,412,352 minutes, pinning 1679063638353651631536d 8h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-first-doubling-from-below-qrtts.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-first-doubling-from-above-qrtts.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1679063638353651631536d 8h 32m",
                "40297527320487639156872:32:00",
                "1679063638353651631536 days, 8 hours, 32 minutes",
                "P1679063638353651631536DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_third_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 29,014,219,670,751,100,192,948,224 ns
    // around 9,671,406,556,917,033,397,649,408 minutes, pinning 6716254553414606526145d 10h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-third-doubling-from-below-cgljp.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-third-doubling-from-above-cgljp.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "6716254553414606526145d 10h 8m",
                "161190109281950556627490:08:00",
                "6716254553414606526145 days, 10 hours, 8 minutes",
                "P6716254553414606526145DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_fourth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 58,028,439,341,502,200,385,896,448 ns
    // around 19,342,813,113,834,066,795,298,816 minutes, pinning 13432509106829213052290d 20h 16m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-fourth-doubling-from-below-c5kho.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-fourth-doubling-from-above-c5kho.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "13432509106829213052290d 20h 16m",
                "322380218563901113254980:16:00",
                "13432509106829213052290 days, 20 hours, 16 minutes",
                "P13432509106829213052290DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_fifth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 116,056,878,683,004,400,771,792,896 ns
    // around 38,685,626,227,668,133,590,597,632 minutes, pinning 26865018213658426104581d 16h 32m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-fifth-doubling-from-below-w2kd1.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-fifth-doubling-from-above-w2kd1.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "26865018213658426104581d 16h 32m",
                "644760437127802226509960:32:00",
                "26865018213658426104581 days, 16 hours, 32 minutes",
                "P26865018213658426104581DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_sixth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 232,113,757,366,008,801,543,585,792 ns
    // around 77,371,252,455,336,267,181,195,264 minutes, pinning 53730036427316852209163d 9h 4m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-sixth-doubling-from-below-7xzso.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-sixth-doubling-from-above-7xzso.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "53730036427316852209163d 9h 4m",
                "1289520874255604453019921:04:00",
                "53730036427316852209163 days, 9 hours, 4 minutes",
                "P53730036427316852209163DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_seventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 464,227,514,732,017,603,087,171,584 ns
    // around 154,742,504,910,672,534,362,390,528 minutes, pinning 107460072854633704418326d 18h 8m.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-seventh-doubling-from-below-hui4r.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-seventh-doubling-from-above-hui4r.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "107460072854633704418326d 18h 8m",
                "2579041748511208906039842:08:00",
                "107460072854633704418326 days, 18 hours, 8 minutes",
                "P107460072854633704418326DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_eighth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 928,455,029,464,035,206,174,343,168 ns
    // around 309,485,009,821,345,068,724,781,056 minutes, pinning 214920145709267408836653d 12h 16m.
    // The fixture stages its 60 doublings in 20-factor bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-eighth-doubling-from-below-lt1ug.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-eighth-doubling-from-above-lt1ug.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "214920145709267408836653d 12h 16m",
                "5158083497022417812079684:16:00",
                "214920145709267408836653 days, 12 hours, 16 minutes",
                "P214920145709267408836653DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_eighty_ninth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 1,856,910,058,928,070,412.348686336 seconds
    // around 618,970,019,642,690,137,449,562,112 minutes, pinning 429840291418534817673307d 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-ninth-doubling-from-below-79wlg.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-eighty-ninth-doubling-from-above-79wlg.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "429840291418534817673307d 32m",
                "10316166994044835624159368:32:00",
                "429840291418534817673307 days, 32 minutes",
                "P429840291418534817673307DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninetieth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,713,820,117,856,140,824.697372672 seconds
    // around 1,237,940,039,285,380,274,899,124,224 minutes, pinning 859680582837069635346614d 1h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninetieth-doubling-from-below-7nrg7.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninetieth-doubling-from-above-7nrg7.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "859680582837069635346614d 1h 4m",
                "20632333988089671248318737:04:00",
                "859680582837069635346614 days, 1 hour, 4 minutes",
                "P859680582837069635346614DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_first_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 7,427,640,235,712,281,649.394745344 seconds
    // around 2,475,880,078,570,760,549,798,248,448 minutes, pinning 1719361165674139270693228d 2h 8m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-first-doubling-from-below-t1u4p.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-first-doubling-from-above-t1u4p.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1719361165674139270693228d 2h 8m",
                "41264667976179342496637474:08:00",
                "1719361165674139270693228 days, 2 hours, 8 minutes",
                "P1719361165674139270693228DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_second_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 14,855,280,471,424,563,298.789490688 seconds
    // around 4,951,760,157,141,521,099,596,496,896 minutes, pinning 3438722331348278541386456d 4h 16m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-second-doubling-from-below-cgiq2.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-second-doubling-from-above-cgiq2.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3438722331348278541386456d 4h 16m",
                "82529335952358684993274948:16:00",
                "3438722331348278541386456 days, 4 hours, 16 minutes",
                "P3438722331348278541386456DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_third_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 29,710,560,942,849,126,597.578981376 seconds
    // around 9,903,520,314,283,042,199,192,993,792 minutes, pinning 6877444662696557082772912d 8h 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-third-doubling-from-below-sf8pj.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-third-doubling-from-above-sf8pj.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "6877444662696557082772912d 8h 32m",
                "165058671904717369986549896:32:00",
                "6877444662696557082772912 days, 8 hours, 32 minutes",
                "P6877444662696557082772912DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_fourth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 59,421,121,885,698,253,195.157962752 seconds
    // around 19,807,040,628,566,084,398,385,987,584 minutes, pinning 13754889325393114165545824d 17h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-fourth-doubling-from-below-m8wcl.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-fourth-doubling-from-above-m8wcl.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "13754889325393114165545824d 17h 4m",
                "330117343809434739973099793:04:00",
                "13754889325393114165545824 days, 17 hours, 4 minutes",
                "P13754889325393114165545824DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_fifth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 118,842,243,771,396,506,390.315925504 seconds
    // around 39,614,081,257,132,168,796,771,975,168 minutes, pinning 27509778650786228331091649d 10h 8m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-fifth-doubling-from-below-di886.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-fifth-doubling-from-above-di886.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "27509778650786228331091649d 10h 8m",
                "660234687618869479946199586:08:00",
                "27509778650786228331091649 days, 10 hours, 8 minutes",
                "P27509778650786228331091649DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_sixth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 237,684,487,542,793,012,780.631851008 seconds
    // around 79,228,162,514,264,337,593,543,950,336 minutes, pinning 55019557301572456662183298d 20h 16m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-sixth-doubling-from-below-t2h45.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-sixth-doubling-from-above-t2h45.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "55019557301572456662183298d 20h 16m",
                "1320469375237738959892399172:16:00",
                "55019557301572456662183298 days, 20 hours, 16 minutes",
                "P55019557301572456662183298DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_seventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 475,368,975,085,586,025,561.263702016 seconds
    // around 158,456,325,028,528,675,187,087,900,672 minutes, pinning 110039114603144913324366597d 16h 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-seventh-doubling-from-below-8ba4o.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-seventh-doubling-from-above-8ba4o.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "110039114603144913324366597d 16h 32m",
                "2640938750475477919784798344:32:00",
                "110039114603144913324366597 days, 16 hours, 32 minutes",
                "P110039114603144913324366597DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_eighth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 950,737,950,171,172,051,122.527404032 seconds
    // around 316,912,650,057,057,350,374,175,801,344 minutes, pinning 220078229206289826648733195d 9h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-eighth-doubling-from-below-jdf45.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-eighth-doubling-from-above-jdf45.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "220078229206289826648733195d 9h 4m",
                "5281877500950955839569596689:04:00",
                "220078229206289826648733195 days, 9 hours, 4 minutes",
                "P220078229206289826648733195DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_ninety_ninth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 1,901,475,900,342,344,102,245.054808064 seconds
    // around 633,825,300,114,114,700,748,351,602,688 minutes, pinning 440156458412579653297466390d 18h 8m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-ninth-doubling-from-below-0py3i.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-ninety-ninth-doubling-from-above-0py3i.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "440156458412579653297466390d 18h 8m",
                "10563755001901911679139193378:08:00",
                "440156458412579653297466390 days, 18 hours, 8 minutes",
                "P440156458412579653297466390DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_hundredth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,802,951,800,684,688,204,490.109616128 seconds
    // around 1,267,650,600,228,229,401,496,703,205,376 minutes, pinning 880312916825159306594932781d 12h 16m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-hundredth-doubling-from-below-pc7og.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-hundredth-doubling-from-above-pc7og.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "880312916825159306594932781d 12h 16m",
                "21127510003803823358278386756:16:00",
                "880312916825159306594932781 days, 12 hours, 16 minutes",
                "P880312916825159306594932781DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_first_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 7,605,903,601,369,376,408,980.219232256 seconds
    // around 2,535,301,200,456,458,802,993,406,410,752 minutes, pinning 1760625833650318613189865563d 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-first-doubling-from-below-jc2p0.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-first-doubling-from-above-jc2p0.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1760625833650318613189865563d 32m",
                "42255020007607646716556773512:32:00",
                "1760625833650318613189865563 days, 32 minutes",
                "P1760625833650318613189865563DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_second_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 15,211,807,202,738,752,817,960.438464512 seconds
    // around 5,070,602,400,912,917,605,986,812,821,504 minutes, pinning 3521251667300637226379731126d 1h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-second-doubling-from-below-xtx7l.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-second-doubling-from-above-xtx7l.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3521251667300637226379731126d 1h 4m",
                "84510040015215293433113547025:04:00",
                "3521251667300637226379731126 days, 1 hour, 4 minutes",
                "P3521251667300637226379731126DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_third_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 30,423,614,405,477,505,635,920.876929024 seconds
    // around 10,141,204,801,825,835,211,973,625,643,008 minutes, pinning 7042503334601274452759462252d 2h 8m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-third-doubling-from-below-bnp2g.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-third-doubling-from-above-bnp2g.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "7042503334601274452759462252d 2h 8m",
                "169020080030430586866227094050:08:00",
                "7042503334601274452759462252 days, 2 hours, 8 minutes",
                "P7042503334601274452759462252DT2H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_fourth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 60,847,228,810,955,011,271,841.753858048 seconds
    // around 20,282,409,603,651,670,423,947,251,286,016 minutes, pinning 14085006669202548905518924504d 4h 16m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-fourth-doubling-from-below-ewkqi.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-fourth-doubling-from-above-ewkqi.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "14085006669202548905518924504d 4h 16m",
                "338040160060861173732454188100:16:00",
                "14085006669202548905518924504 days, 4 hours, 16 minutes",
                "P14085006669202548905518924504DT4H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_fifth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 121,694,457,621,910,022,543,683.507716096 seconds
    // around 40,564,819,207,303,340,847,894,502,572,032 minutes, pinning 28170013338405097811037849008d 8h 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-fifth-doubling-from-below-2qfsi.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-fifth-doubling-from-above-2qfsi.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "28170013338405097811037849008d 8h 32m",
                "676080320121722347464908376200:32:00",
                "28170013338405097811037849008 days, 8 hours, 32 minutes",
                "P28170013338405097811037849008DT8H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_sixth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 243,388,915,243,820,045,087,367.015432192 seconds
    // around 81,129,638,414,606,681,695,789,005,144,064 minutes, pinning 56340026676810195622075698016d 17h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-sixth-doubling-from-below-3n7d3.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-sixth-doubling-from-above-3n7d3.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "56340026676810195622075698016d 17h 4m",
                "1352160640243444694929816752401:04:00",
                "56340026676810195622075698016 days, 17 hours, 4 minutes",
                "P56340026676810195622075698016DT17H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_seventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 486,777,830,487,640,090,174,734.030864384 seconds
    // around 162,259,276,829,213,363,391,578,010,288,128 minutes, pinning 112680053353620391244151396033d 10h 8m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-seventh-doubling-from-below-rfol1.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-seventh-doubling-from-above-rfol1.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "112680053353620391244151396033d 10h 8m",
                "2704321280486889389859633504802:08:00",
                "112680053353620391244151396033 days, 10 hours, 8 minutes",
                "P112680053353620391244151396033DT10H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_eighth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 973,555,660,975,280,180,349,468.061728768 seconds
    // around 324,518,553,658,426,726,783,156,020,576,256 minutes, pinning 225360106707240782488302792066d 20h 16m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-eighth-doubling-from-below-iyk89.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-eighth-doubling-from-above-iyk89.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "225360106707240782488302792066d 20h 16m",
                "5408642560973778779719267009604:16:00",
                "225360106707240782488302792066 days, 20 hours, 16 minutes",
                "P225360106707240782488302792066DT20H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_ninth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 1,947,111,321,950,560,360,698,936.123457536 seconds
    // around 649,037,107,316,853,453,566,312,041,152,512 minutes, pinning 450720213414481564976605584133d 16h 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-ninth-doubling-from-below-iilhr.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-ninth-doubling-from-above-iilhr.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "450720213414481564976605584133d 16h 32m",
                "10817285121947557559438534019208:32:00",
                "450720213414481564976605584133 days, 16 hours, 32 minutes",
                "P450720213414481564976605584133DT16H32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_tenth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 3,894,222,643,901,120,721,397,872.246915072 seconds
    // around 1,298,074,214,633,706,907,132,624,082,305,024 minutes, pinning 901440426828963129953211168267d 9h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-tenth-doubling-from-below-0scv0.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-tenth-doubling-from-above-0scv0.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "901440426828963129953211168267d 9h 4m",
                "21634570243895115118877068038417:04:00",
                "901440426828963129953211168267 days, 9 hours, 4 minutes",
                "P901440426828963129953211168267DT9H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_eleventh_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 7,788,445,287,802,241,442,795,744.493830144 seconds
    // around 2,596,148,429,267,413,814,265,248,164,610,048 minutes, pinning 1802880853657926259906422336534d 18h 8m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-eleventh-doubling-from-below-xjeto.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-eleventh-doubling-from-above-xjeto.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "1802880853657926259906422336534d 18h 8m",
                "43269140487790230237754136076834:08:00",
                "1802880853657926259906422336534 days, 18 hours, 8 minutes",
                "P1802880853657926259906422336534DT18H8M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_twelfth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 15,576,890,575,604,482,885,591,488.987660288 seconds
    // around 5,192,296,858,534,827,628,530,496,329,220,096 minutes, pinning 3605761707315852519812844673069d 12h 16m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-twelfth-doubling-from-below-otedl.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-twelfth-doubling-from-above-otedl.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "3605761707315852519812844673069d 12h 16m",
                "86538280975580460475508272153668:16:00",
                "3605761707315852519812844673069 days, 12 hours, 16 minutes",
                "P3605761707315852519812844673069DT12H16M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_thirteenth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 31,153,781,151,208,965,771,182,977.975320576 seconds
    // around 10,384,593,717,069,655,257,060,992,658,440,192 minutes, pinning 7211523414631705039625689346139d 32m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-thirteenth-doubling-from-below-oulj4.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-thirteenth-doubling-from-above-oulj4.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "7211523414631705039625689346139d 32m",
                "173076561951160920951016544307336:32:00",
                "7211523414631705039625689346139 days, 32 minutes",
                "P7211523414631705039625689346139DT32M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}

#[test]
fn positive_factor_rescaled_tails_close_after_one_hundred_fourteenth_doubling() {
    // The reference is silent at this next scale. Continue the symmetric edge
    // correction by doubling the prior tail to 62,307,562,302,417,931,542,365,955.950641152 seconds
    // around 20,769,187,434,139,310,514,121,985,316,880,384 minutes, pinning 14423046829263410079251378692278d 1h 4m.
    // The fixture stages its doublings in bounded bindings to stay under the parser's AST-depth bound.
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
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-fourteenth-doubling-from-below-a6vp7.orna"),
        include_str!("fixtures/stdlib-time-duration-factor-rescaled-close-one-hundred-fourteenth-doubling-from-above-a6vp7.orna"),
    ] {
        let result = session.submit(source);
        assert_eq!(
            result,
            Ok(Some(texts(&[
                "14423046829263410079251378692278d 1h 4m",
                "346153123902321841902033088614673:04:00",
                "14423046829263410079251378692278 days, 1 hour, 4 minutes",
                "P14423046829263410079251378692278DT1H4M",
            ]))),
            "{source}; diagnostic={}",
            result.as_ref().err().map_or("none", |error| error.code())
        );
    }
}
