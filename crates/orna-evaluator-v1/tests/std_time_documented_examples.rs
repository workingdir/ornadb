use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn text(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

/// The examples in the reference for `std.time.duration.*.format` applied to
/// one duration of 2 hours 14 minutes (8040 seconds). Each formatter is called
/// with the single-argument form the reference documents.
#[test]
fn duration_formatters_match_the_documented_examples() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default()).unwrap();
    for (source, expected) in [
        (
            "std.time.duration.compact.format(8040.seconds)",
            "2h 14m",
        ),
        (
            "std.time.duration.words.format(8040.seconds)",
            "2 hours, 14 minutes",
        ),
        (
            "std.time.duration.iso.format(8040.seconds)",
            "PT2H14M",
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
