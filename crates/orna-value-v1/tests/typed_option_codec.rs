use orna_value_v1::{decode_typed, encode_typed, Error, Raw, Value};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

#[test]
fn option_none_uses_the_canonical_ovb_tag_and_payload() {
    let value: Option<i64> = None;
    let encoded = encode_typed(&value).expect("typed Option encoding succeeds");

    assert_eq!(encoded, hex_bytes("d9ea6d8100"));
    assert_eq!(decode_typed::<Option<i64>>(&encoded), Ok(value));
}

#[test]
fn option_some_null_is_distinct_from_none() {
    let null = Value::new(Raw::Null).expect("bare null is a valid OVB value");
    let none: Option<Value> = None;
    let some_null = Some(null.clone());

    let none_bytes = encode_typed(&none).expect("None encoding succeeds");
    let some_null_bytes = encode_typed(&some_null).expect("Some(null) encoding succeeds");

    assert_eq!(none_bytes, hex_bytes("d9ea6d8100"));
    assert_eq!(some_null_bytes, hex_bytes("d9ea6d8201f6"));
    assert_ne!(none_bytes, some_null_bytes);
    assert_eq!(decode_typed::<Option<Value>>(&some_null_bytes), Ok(some_null));
}

#[test]
fn nested_options_preserve_some_some_and_some_none() {
    let some_some: Option<Option<i64>> = Some(Some(42));
    let some_none: Option<Option<i64>> = Some(None);

    let some_some_bytes = encode_typed(&some_some).expect("nested Some(Some(value)) encoding succeeds");
    let some_none_bytes = encode_typed(&some_none).expect("nested Some(None) encoding succeeds");

    assert_eq!(some_some_bytes, hex_bytes("d9ea6d8201d9ea6d8201182a"));
    assert_eq!(some_none_bytes, hex_bytes("d9ea6d8201d9ea6d8100"));
    assert_ne!(some_some_bytes, some_none_bytes);
    assert_eq!(decode_typed::<Option<Option<i64>>>(&some_some_bytes), Ok(some_some));
    assert_eq!(decode_typed::<Option<Option<i64>>>(&some_none_bytes), Ok(some_none));
}

#[test]
fn typed_scalar_option_payload_is_canonical_and_round_trips() {
    let value: Option<i64> = Some(42);
    let encoded = encode_typed(&value).expect("typed scalar Option encoding succeeds");

    assert_eq!(encoded, hex_bytes("d9ea6d8201182a"));
    assert_eq!(decode_typed::<Option<i64>>(&encoded), Ok(value));
}

#[test]
fn scalar_type_mismatch_reports_the_option_payload_path() {
    let encoded = hex_bytes("d9ea6d8201f5");
    let error = decode_typed::<Option<i64>>(&encoded).expect_err("Bool cannot decode as Int");

    assert_eq!(error.path(), &["Option".to_owned(), "Int".to_owned()]);
    assert_eq!(error.type_path(), "Option.Int");
    assert_eq!(error.error(), &Error::InvalidValue);
}

#[test]
fn nested_scalar_type_mismatch_reports_every_option_layer() {
    let encoded = hex_bytes("d9ea6d8201d9ea6d8201f5");
    let error = decode_typed::<Option<Option<i64>>>(&encoded)
        .expect_err("Bool cannot decode as the nested Int payload");

    assert_eq!(
        error.path(),
        &[
            "Option".to_owned(),
            "Option".to_owned(),
            "Int".to_owned(),
        ]
    );
    assert_eq!(error.type_path(), "Option.Option.Int");
    assert_eq!(error.error(), &Error::InvalidValue);
}

#[test]
fn malformed_option_payload_reports_the_failing_option_layer() {
    let encoded = hex_bytes("d9ea6d8101");
    let error = decode_typed::<Option<i64>>(&encoded)
        .expect_err("Some discriminator without a payload is malformed");

    assert_eq!(error.path(), &["Option".to_owned()]);
    assert_eq!(error.type_path(), "Option");
    assert!(matches!(error.error(), Error::InvalidTag | Error::InvalidValue));
}
