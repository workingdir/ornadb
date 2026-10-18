use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn session() -> AdmittedReplSession {
    AdmittedReplSession::with_reference_standard(Limits::default())
        .expect("production reference standard modules are admitted")
}

fn submit(session: &mut AdmittedReplSession, source: &str) -> CanonicalValue {
    session
        .submit(source)
        .unwrap_or_else(|error| panic!("{source}: {}", error.code()))
        .expect("codec expression returns a value")
}

fn canonical(raw: Raw) -> CanonicalValue {
    CanonicalValue::new(raw).expect("expected canonical test value")
}

/// ORNA-CODEC-001: the admitted evaluator exposes typed `std.encoding.orna.encode` and `decode(..., as: T)` operations.
#[test]
fn orna_codec_round_trips_a_typed_integer_through_the_language_entry_point() {
    let mut repl = session();
    assert_eq!(
        submit(
            &mut repl,
            "std.encoding.orna.decode(std.encoding.orna.encode(42), as: Int)",
        ),
        canonical(Raw::Int(42.into())),
    );
}

/// ORNA-CODEC-003: canonical Orna encoding is deterministic, unambiguous, full-precision, and round-trippable.
#[test]
fn orna_codec_is_deterministic_unambiguous_and_preserves_large_integer_precision() {
    let large_integer = "123456789012345678901234567890123456789";
    let encode_large = format!("std.encoding.orna.encode({large_integer})");
    let mut repl = session();

    let first_encoding = submit(&mut repl, &encode_large);
    let second_encoding = submit(&mut repl, &encode_large);
    let canonical_text = canonical(Raw::Text(format!("{large_integer}\n")));
    assert_eq!(first_encoding, canonical_text);
    assert_eq!(second_encoding, canonical_text);

    let encode_as_string = format!("std.encoding.orna.encode(\"{large_integer}\")");
    let encoded_string = submit(&mut repl, &encode_as_string);
    assert_eq!(
        encoded_string,
        canonical(Raw::Text(format!("\"{large_integer}\"\n"))),
    );
    assert_ne!(first_encoding, encoded_string);

    let round_trip = format!(
        "std.encoding.orna.decode(std.encoding.orna.encode({large_integer}), as: Int)"
    );
    assert_eq!(
        submit(&mut repl, &round_trip),
        canonical(Raw::Int(large_integer.parse().expect("large integer parses"))),
    );
}
