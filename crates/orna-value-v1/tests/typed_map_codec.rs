use std::collections::BTreeMap;

use orna_value_v1::{
    decode, decode_typed, encode, encode_typed, DecodeError, Error, OvbCodec, Raw, Result, Value,
};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CollidingKey(u8);

impl OvbCodec for CollidingKey {
    fn type_label() -> &'static str {
        "CollidingKey"
    }

    fn encode_value(&self) -> Result<Value> {
        // Distinct Rust keys intentionally share one canonical key encoding so
        // the map codec's encoded-key uniqueness check is exercised.
        Ok(Value::int(0.into()))
    }

    fn decode_value(
        value: &Value,
        _path: &mut Vec<String>,
    ) -> std::result::Result<Self, DecodeError> {
        let decoded = decode_typed::<i64>(&value.encode().expect("validated value encodes"))?;
        Ok(Self(decoded as u8))
    }
}

#[test]
fn map_encoding_is_independent_of_insertion_order() {
    let mut first = BTreeMap::new();
    first.insert("beta".to_owned(), 2_i64);
    first.insert("alpha".to_owned(), 1_i64);
    first.insert("gamma".to_owned(), 3_i64);

    let mut second = BTreeMap::new();
    second.insert("gamma".to_owned(), 3_i64);
    second.insert("beta".to_owned(), 2_i64);
    second.insert("alpha".to_owned(), 1_i64);

    assert_eq!(
        encode_typed(&first).expect("first map encoding succeeds"),
        encode_typed(&second).expect("second map encoding succeeds")
    );
}

#[test]
fn map_keys_are_sorted_by_complete_encoded_key_bytes() {
    let mut value = BTreeMap::new();
    value.insert(-1_i64, "negative".to_owned());
    value.insert(0_i64, "zero".to_owned());
    value.insert(24_i64, "twenty-four".to_owned());
    value.insert(256_i64, "two-five-six".to_owned());

    // Numeric Ord places -1 before 0, but OVB-1 key bytes order 0, 24, 256,
    // then -1. The expected map makes the complete-byte ordering observable.
    assert_eq!(
        encode_typed(&value).expect("map encoding succeeds"),
        hex_bytes("a400647a65726f18186b7477656e74792d666f75721901006c74776f2d666976652d73697820686e65676174697665")
    );
}

#[test]
fn nested_maps_and_existing_option_tuple_codecs_round_trip() {
    let mut inner_one = BTreeMap::new();
    inner_one.insert(1_i64, Some(("one".to_owned(), true)));
    inner_one.insert(2_i64, None);

    let mut inner_two = BTreeMap::new();
    inner_two.insert(3_i64, Some(("three".to_owned(), false)));

    let mut value = BTreeMap::new();
    value.insert("first".to_owned(), inner_one);
    value.insert("second".to_owned(), inner_two);

    let encoded = encode_typed(&value).expect("nested map encoding succeeds");
    assert_eq!(
        decode_typed::<BTreeMap<String, BTreeMap<i64, Option<(String, bool)>>>>(&encoded),
        Ok(value)
    );
}

#[test]
fn duplicate_canonical_key_encodings_are_rejected_on_encode() {
    let mut value = BTreeMap::new();
    value.insert(CollidingKey(1), "first".to_owned());
    value.insert(CollidingKey(2), "second".to_owned());

    assert_eq!(
        encode_typed(&value).expect_err("duplicate encoded keys must fail"),
        Error::DuplicateOrUnorderedMapKey
    );
}

#[test]
fn malformed_duplicate_unordered_and_noncanonical_maps_are_rejected_at_map_path() {
    for (name, encoded, expected) in [
        (
            "duplicate",
            "a200010002",
            Error::DuplicateOrUnorderedMapKey,
        ),
        (
            "unordered",
            "a220010002",
            Error::DuplicateOrUnorderedMapKey,
        ),
        ("noncanonical", "a1180001", Error::NonCanonical),
    ] {
        let error = match decode_typed::<BTreeMap<i64, i64>>(&hex_bytes(encoded)) {
            Ok(_) => panic!("{name} map unexpectedly decoded"),
            Err(error) => error,
        };
        assert_eq!(error.path(), &["Map".to_owned()], "{name} path");
        assert_eq!(error.error(), &expected, "{name} error");
    }
}

#[test]
fn key_and_value_type_failures_keep_stable_map_index_paths() {
    let key_error = decode_typed::<BTreeMap<i64, i64>>(&hex_bytes("a1f501"))
        .expect_err("Bool key must not decode as Int");
    assert_eq!(
        key_error.path(),
        &["Map".to_owned(), "0".to_owned(), "Key".to_owned()]
    );
    assert_eq!(key_error.type_path(), "Map.0.Key");
    assert_eq!(key_error.error(), &Error::InvalidValue);

    let value_error = decode_typed::<BTreeMap<i64, i64>>(&hex_bytes("a100f5"))
        .expect_err("Bool value must not decode as Int");
    assert_eq!(
        value_error.path(),
        &["Map".to_owned(), "0".to_owned(), "Value".to_owned()]
    );
    assert_eq!(value_error.type_path(), "Map.0.Value");
    assert_eq!(value_error.error(), &Error::InvalidValue);
}

#[test]
fn map_codec_preserves_prior_generic_encode_decode_aliases() {
    let mut value = BTreeMap::new();
    value.insert("optional".to_owned(), Some(42_i64));
    value.insert("missing".to_owned(), None);

    let encoded = encode(&value).expect("generic encode alias succeeds");
    assert_eq!(
        decode::<BTreeMap<String, Option<i64>>>(&encoded),
        Ok(value)
    );
}

#[test]
fn map_codec_reuses_raw_value_round_trip_for_nested_supported_values() {
    let mut value = BTreeMap::new();
    value.insert("blob".to_owned(), Value::new(Raw::Bytes(vec![1, 2, 3])).unwrap());
    value.insert("null".to_owned(), Value::new(Raw::Null).unwrap());

    let encoded = encode_typed(&value).expect("Value map encoding succeeds");
    assert_eq!(
        decode_typed::<BTreeMap<String, Value>>(&encoded),
        Ok(value)
    );
}
