use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

#[test]
fn pinned_duration_formatters_apply_explicit_context_and_elapsed_units() {
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
            include_str!("fixtures/stdlib-time-duration-compact-b1e0.orna"),
            "1h 1m 1s",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-clock-b1e0.orna"),
            "01:01:01",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-words-b1e0.orna"),
            "1 hour, 1 minute, 1 second",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-iso-b1e0.orna"),
            "PT1H1M1S",
        ),
        (
            include_str!("fixtures/stdlib-time-duration-day-b1e0.orna"),
            "P1D",
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
            .submit(include_str!("fixtures/stdlib-time-duration-bad-locale-b1e0.orna"))
            .unwrap_err()
            .code(),
        "ORNA-EVAL-VALUE"
    );
}
