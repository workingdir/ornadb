use orna_value_v1::{Error, RangeValue, decode_typed, encode_typed};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

#[test]
fn bounded_range_uses_canonical_option_endpoints_and_upper_inclusivity() {
    let value = RangeValue::new(Some(1_i64), Some(5_i64), true);
    let encoded = encode_typed(&value).expect("typed Range encoding succeeds");

    assert_eq!(encoded, hex_bytes("d9ea7383d9ea6d820101d9ea6d820105f5"));
    assert_eq!(decode_typed::<RangeValue<i64>>(&encoded), Ok(value));
}

#[test]
fn unbounded_endpoints_round_trip_without_losing_upper_bound_kind() {
    for value in [
        RangeValue::<i64>::new(None, Some(9), false),
        RangeValue::<i64>::new(Some(9), None, false),
        RangeValue::<i64>::new(None, None, false),
    ] {
        let encoded = encode_typed(&value).expect("typed Range encoding succeeds");
        let decoded =
            decode_typed::<RangeValue<i64>>(&encoded).expect("typed Range decode succeeds");
        assert_eq!(decoded, value);
    }

    let lower_unbounded = RangeValue::<i64>::new(None, Some(9), false);
    let encoded = encode_typed(&lower_unbounded).expect("typed Range encoding succeeds");
    assert_eq!(encoded, hex_bytes("d9ea7383d9ea6d8100d9ea6d820109f4"));
    let decoded = decode_typed::<RangeValue<i64>>(&encoded).expect("typed Range decode succeeds");
    assert_eq!(decoded.lower(), None);
    assert_eq!(decoded.upper(), Some(&9));
    assert!(!decoded.upper_inclusive());
}

#[test]
fn empty_range_is_a_valid_typed_value() {
    let value = RangeValue::new(Some(4_i64), Some(4_i64), false);
    let encoded = encode_typed(&value).expect("empty Range encoding succeeds");

    assert_eq!(decode_typed::<RangeValue<i64>>(&encoded), Ok(value));
}

#[test]
fn range_decode_rejects_endpoint_type_mismatch_with_precise_path() {
    let encoded = hex_bytes("d9ea7383d9ea6d820101d9ea6d8201f5f4");
    let error = decode_typed::<RangeValue<i64>>(&encoded)
        .expect_err("Bool cannot be used as the Int upper endpoint");

    assert_eq!(
        error.path(),
        &["Range".to_owned(), "upper".to_owned(), "Int".to_owned()]
    );
    assert_eq!(error.error(), &Error::InvalidValue);
}

#[test]
fn malformed_range_shape_and_noncanonical_endpoints_are_rejected() {
    // The range tag requires two canonical Option values and an upper Bool.
    let missing_upper = hex_bytes("d9ea7382d9ea6d8100d9ea6d8100");
    assert_eq!(
        decode_typed::<RangeValue<i64>>(&missing_upper)
            .expect_err("a missing upper-inclusive flag is malformed")
            .error(),
        &Error::InvalidTag
    );

    // Integer 1 is encoded with a non-shortest head inside the lower Option.
    let noncanonical = hex_bytes("d9ea7383d9ea6d82180101d9ea6d8100f4");
    assert_eq!(
        decode_typed::<RangeValue<i64>>(&noncanonical)
            .expect_err("a noncanonical endpoint must fail before typed decoding")
            .error(),
        &Error::NonCanonical
    );
}
