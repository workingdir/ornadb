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
