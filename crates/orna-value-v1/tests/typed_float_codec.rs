use orna_value_v1::{decode_typed, encode_typed, Error, CANONICAL_NAN_BITS};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

fn assert_float_decode_error(bytes: &[u8], expected: Error) {
    let error = decode_typed::<f64>(bytes).expect_err("Float decoding unexpectedly succeeded");

    assert_eq!(error.path(), &["Float".to_owned()]);
    assert_eq!(error.type_path(), "Float");
    assert_eq!(error.error(), &expected);
}

#[test]
fn float_codec_emits_canonical_binary64_bytes() {
    let value = 1.5_f64;
    let expected = hex_bytes("fb3ff8000000000000");

    assert_eq!(encode_typed(&value).expect("typed Float encoding succeeds"), expected);
    assert_eq!(decode_typed::<f64>(&expected), Ok(value));
}

#[test]
fn float_codec_preserves_signed_zero_in_canonical_bytes_and_bits() {
    let positive = encode_typed(&0.0_f64).expect("positive zero encoding succeeds");
    let negative = encode_typed(&(-0.0_f64)).expect("negative zero encoding succeeds");

    assert_eq!(positive, hex_bytes("fb0000000000000000"));
    assert_eq!(negative, hex_bytes("fb8000000000000000"));
    assert_ne!(positive, negative);
    assert_eq!(decode_typed::<f64>(&positive).unwrap().to_bits(), 0x0000_0000_0000_0000);
    assert_eq!(decode_typed::<f64>(&negative).unwrap().to_bits(), 0x8000_0000_0000_0000);
}

#[test]
fn float_codec_canonicalizes_nan_payloads_without_losing_nan_semantics() {
    let noncanonical_nan = f64::from_bits(0x7ff0_0000_0000_0001);
    let expected = hex_bytes("fb7ff8000000000000");

    assert_eq!(encode_typed(&noncanonical_nan).expect("NaN encoding succeeds"), expected);
    let decoded = decode_typed::<f64>(&expected).expect("canonical NaN decoding succeeds");
    assert!(decoded.is_nan());
    assert_eq!(decoded.to_bits(), CANONICAL_NAN_BITS);
}

#[test]
fn float_codec_preserves_signed_infinities() {
    let positive = encode_typed(&f64::INFINITY).expect("positive infinity encoding succeeds");
    let negative = encode_typed(&f64::NEG_INFINITY).expect("negative infinity encoding succeeds");

    assert_eq!(positive, hex_bytes("fb7ff0000000000000"));
    assert_eq!(negative, hex_bytes("fbfff0000000000000"));
    assert_eq!(decode_typed::<f64>(&positive), Ok(f64::INFINITY));
    assert_eq!(decode_typed::<f64>(&negative), Ok(f64::NEG_INFINITY));
}

#[test]
fn float_codec_round_trips_finite_values_at_full_binary64_precision() {
    for value in [
        f64::from_bits(0x3fd5_5555_5555_5555),
        f64::from_bits(0x0010_0000_0000_0000),
        f64::from_bits(0x7fef_ffff_ffff_ffff),
        f64::from_bits(0xc087_4380_0000_0000),
    ] {
        let encoded = encode_typed(&value).expect("finite Float encoding succeeds");
        let decoded = decode_typed::<f64>(&encoded).expect("finite Float decoding succeeds");
        assert_eq!(decoded.to_bits(), value.to_bits());
    }
}

#[test]
fn float_codec_rejects_malformed_and_noncanonical_bytes_with_float_path() {
    let cases = [
        ("fb3ff80000000000", Error::Truncated),
        ("f93e00", Error::Unsupported),
        ("fb7ff8000000000001", Error::NonCanonical),
        ("fb3ff800000000000000", Error::TrailingBytes),
    ];

    for (encoded, expected) in cases {
        assert_float_decode_error(&hex_bytes(encoded), expected);
    }
}

#[test]
fn float_codec_reports_float_path_for_canonical_non_float_values() {
    assert_float_decode_error(&hex_bytes("01"), Error::InvalidValue);
}
