use orna_value_v1::{decode_typed, encode_typed, Error, Raw, Value};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

#[test]
fn unit_tuple_uses_the_canonical_unit_tag_and_round_trips() {
    let encoded = encode_typed(&()).expect("typed unit encoding succeeds");

    assert_eq!(encoded, hex_bytes("d9ea6e80"));
    assert_eq!(decode_typed::<()>(&encoded), Ok(()));
    assert_eq!(Value::decode(&encoded).expect("unit bytes decode"), Value::unit());
}

#[test]
fn nonzero_tuple_preserves_component_order_in_canonical_bytes() {
    let value = (7_i64, String::from("ordered"), false);
    let encoded = encode_typed(&value).expect("typed tuple encoding succeeds");

    assert_eq!(encoded, hex_bytes("d9ea6f8307676f726465726564f4"));
    assert_eq!(decode_typed::<(i64, String, bool)>(&encoded), Ok(value));
}

#[test]
fn nested_tuples_round_trip_recursively_with_stable_bytes() {
    let value = (1_i64, (String::from("nested"), (true, -2_i64)));
    let encoded = encode_typed(&value).expect("nested tuple encoding succeeds");

    assert_eq!(
        encoded,
        hex_bytes("d9ea6f8201d9ea6f82666e6573746564d9ea6f82f521")
    );
    assert_eq!(
        decode_typed::<(i64, (String, (bool, i64)))>(&encoded),
        Ok(value)
    );
}

#[test]
fn nested_tuple_element_mismatch_reports_the_precise_type_path() {
    let encoded = hex_bytes("d9ea6f8201d9ea6f82666e6573746564d9ea6f820021");
    let error = decode_typed::<(i64, (String, (bool, i64)))>(&encoded)
        .expect_err("an Int cannot decode as the nested Bool element");

    assert_eq!(
        error.path(),
        &[
            "Tuple".to_owned(),
            "1".to_owned(),
            "Tuple".to_owned(),
            "1".to_owned(),
            "Tuple".to_owned(),
            "0".to_owned(),
            "Bool".to_owned(),
        ]
    );
    assert_eq!(error.type_path(), "Tuple.1.Tuple.1.Tuple.0.Bool");
    assert_eq!(error.error(), &Error::InvalidValue);
}

#[test]
fn tuple_composes_existing_option_and_scalar_codecs_without_aliasing_payloads() {
    let value = (Some(42_i64), Option::<i64>::None, String::from("scalar"));
    let encoded = encode_typed(&value).expect("tuple with Option and scalar encoding succeeds");

    assert_eq!(
        encoded,
        hex_bytes("d9ea6f83d9ea6d8201182ad9ea6d8100667363616c6172")
    );
    assert_eq!(
        decode_typed::<(Option<i64>, Option<i64>, String)>(&encoded),
        Ok(value)
    );
}

#[test]
fn raw_and_value_views_round_trip_the_same_tuple_bytes() {
    let encoded = hex_bytes("d9ea6f8201f5");
    let value = Value::decode(&encoded).expect("canonical tuple bytes decode as Value");
    let raw = decode_typed::<Raw>(&encoded).expect("canonical tuple bytes decode as Raw");

    assert_eq!(value.raw(), &raw);
    assert_eq!(encode_typed(&value).expect("Value re-encodes"), encoded);
    assert_eq!(encode_typed(&raw).expect("Raw re-encodes"), encoded);
}
