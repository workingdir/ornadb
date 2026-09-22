use num_bigint::BigInt;
use orna_value_v1::{decode_typed, encode_typed, Decimal, Error};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

fn assert_decimal_decode_error(bytes: &[u8], expected: Error) {
    let error = match decode_typed::<Decimal>(bytes) {
        Ok(value) => panic!("Decimal decoding unexpectedly succeeded: {value:?}"),
        Err(error) => error,
    };

    assert_eq!(error.path(), &["Decimal".to_owned()]);
    assert_eq!(error.type_path(), "Decimal");
    assert_eq!(error.error(), &expected);
}

#[test]
fn decimal_codec_emits_the_canonical_tag_60000_golden_bytes() {
    let value = Decimal::try_new(BigInt::from(1234), BigInt::from(-2))
        .expect("finite Decimal construction succeeds");
    let expected = hex_bytes("d9ea60821904d221");

    assert_eq!(encode_typed(&value).expect("typed Decimal encoding succeeds"), expected);
    assert_eq!(decode_typed::<Decimal>(&expected), Ok(value));
}

#[test]
fn decimal_zero_uses_one_canonical_coefficient_and_exponent() {
    let value = Decimal::try_new(BigInt::from(0), BigInt::from(99))
        .expect("zero normalizes within the Decimal range");
    let expected = Decimal::try_new(BigInt::from(0), BigInt::from(0)).unwrap();
    let bytes = hex_bytes("d9ea60820000");

    assert_eq!(value, expected);
    assert_eq!(encode_typed(&value).expect("typed zero encoding succeeds"), bytes);
    assert_eq!(decode_typed::<Decimal>(&bytes), Ok(expected));
}

#[test]
fn decimal_trailing_zeroes_normalize_before_encoding_and_round_trip() {
    let unnormalized = Decimal::try_new(BigInt::from(123_400), BigInt::from(-4))
        .expect("finite Decimal construction succeeds");
    let normalized = Decimal::try_new(BigInt::from(1234), BigInt::from(-2))
        .expect("finite Decimal construction succeeds");
    let expected = hex_bytes("d9ea60821904d221");

    assert_eq!(unnormalized, normalized);
    assert_eq!(encode_typed(&unnormalized).expect("typed Decimal encoding succeeds"), expected);
    assert_eq!(decode_typed::<Decimal>(&expected), Ok(normalized));
}

#[test]
fn decimal_codec_rejects_malformed_and_noncanonical_payloads() {
    let cases = [
        ("d9ea61820000", Error::InvalidValue),
        ("d9ea608100", Error::InvalidTag),
        ("d9ea6082f500", Error::InvalidValue),
        ("d9ea60821904d22100", Error::TrailingBytes),
        ("d9ea60820021", Error::NonCanonical),
        ("d9ea6082180000", Error::NonCanonical),
    ];

    for (encoded, expected) in cases {
        assert_decimal_decode_error(&hex_bytes(encoded), expected);
    }
}

#[test]
fn decimal_codec_preserves_resource_limit_failures() {
    let over_limit = BigInt::from(Decimal::MAX_ABS_EXPONENT) + BigInt::from(1_u8);
    assert_eq!(
        Decimal::try_new(BigInt::from(1), over_limit.clone()),
        Err(Error::DecimalLimit)
    );

    let positive = hex_bytes("d9ea6082011a000f4241");
    let negative = hex_bytes("d9ea6082013a000f4240");
    assert_decimal_decode_error(&positive, Error::DecimalLimit);
    assert_decimal_decode_error(&negative, Error::DecimalLimit);
}
