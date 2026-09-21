use num_bigint::BigInt;
use orna_value_v1::{Decimal, Money, Raw, Value};

fn hex_bytes(input: &str) -> Vec<u8> {
    hex::decode(input).expect("valid test vector hex")
}

fn decimal_raw(coefficient: i64, exponent10: i64) -> Raw {
    Raw::Tag(
        60000,
        Box::new(Raw::Array(vec![
            Raw::Int(BigInt::from(coefficient)),
            Raw::Int(BigInt::from(exponent10)),
        ])),
    )
}

fn money_raw(coefficient: i64, exponent10: i64, currency: [u8; 16]) -> Raw {
    Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            decimal_raw(coefficient, exponent10),
            Raw::Tag(37, Box::new(Raw::Bytes(currency.into()))),
        ])),
    )
}

#[test]
fn money_tag_60007_canonical_vectors_round_trip() {
    let vectors = [
        (
            money_raw(0, 0, [0; 16]),
            concat!(
                "d9ea6782d9ea60820000d82550",
                "00000000000000000000000000000000",
            ),
        ),
        (
            money_raw(1234, -2, [0x01; 16]),
            "d9ea6782d9ea60821904d221d8255001010101010101010101010101010101",
        ),
        (
            money_raw(-98765, -3, [0xff; 16]),
            "d9ea6782d9ea60823a000181cc22d82550ffffffffffffffffffffffffffffffff",
        ),
    ];

    for (raw, expected_hex) in vectors {
        let value = Value::new(raw).expect("valid canonical Money value");
        let encoded = value.encode().expect("Money encoding succeeds");
        assert_eq!(encoded, hex_bytes(expected_hex));
        let money = Money::decode(&encoded).expect("typed Money decoding succeeds");
        assert_eq!(money.value(), &value);
        assert_eq!(money.encode().expect("typed Money encoding succeeds"), encoded);
        assert_eq!(Value::decode(&encoded).expect("Money decoding succeeds"), value);
    }
}

#[test]
fn money_round_trip_preserves_nested_decimal_representation() {
    let currency = [0x42; 16];
    let decimal = Value::decimal(BigInt::from(123_400_i64), BigInt::from(-4_i64))
        .expect("exact Decimal construction");
    assert_eq!(
        decimal.raw(),
        &decimal_raw(1234, -2),
        "Decimal is normalized before it becomes Money payload"
    );

    let money = Value::new(Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            decimal.raw().clone(),
            Raw::Tag(37, Box::new(Raw::Bytes(currency.into()))),
        ])),
    ))
    .expect("normalized Decimal is valid Money payload");
    let decoded = Value::decode(&money.encode().expect("Money encoding succeeds"))
        .expect("Money decoding succeeds");

    assert_eq!(decoded, money);
    assert_eq!(decoded.raw(), money.raw());
}

#[test]
fn typed_money_constructor_preserves_exact_decimal_and_currency() {
    let amount = Decimal::try_new(BigInt::from(12_340_i64), BigInt::from(-3_i64))
        .expect("exact Decimal construction");
    let money = Money::new(amount.clone(), [0x47; 16]).expect("exact Money construction");

    assert_eq!(money.amount(), &amount);
    assert_eq!(money.currency(), [0x47; 16]);
    let encoded = money.encode().expect("Money encoding succeeds");
    let decoded = Money::decode(&encoded).expect("Money decoding succeeds");
    assert_eq!(decoded.amount(), &amount);
    assert_eq!(decoded.currency(), [0x47; 16]);
    assert_eq!(decoded, money);
}

#[test]
fn money_witness_round_trip_preserves_currency_identity() {
    let currency = [0x7a; 16];
    let witness = Raw::Array(vec![
        Raw::Int(BigInt::from(8_i64)),
        Raw::Tag(37, Box::new(Raw::Bytes(currency.into()))),
    ]);
    let typed_money = Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            decimal_raw(25, -2),
            Raw::Tag(37, Box::new(Raw::Bytes(currency.into()))),
        ])),
    );
    let value = Value::new(Raw::Tag(
        60026,
        Box::new(Raw::Array(vec![witness, typed_money])),
    ))
    .expect("matching Money currency witness is valid");

    let encoded = value.encode().expect("witnessed Money encoding succeeds");
    assert_eq!(Value::decode(&encoded).expect("witnessed Money decoding succeeds"), value);
}

fn assert_money_decode_rejects(bytes: &[u8], expected: orna_value_v1::Error) {
    assert_eq!(Value::decode(bytes), Err(expected));
    assert_eq!(Money::decode(bytes), Err(expected));
}

#[test]
fn rejects_malformed_money_outer_and_nested_payloads() {
    let currency = "01010101010101010101010101010101";
    for (encoded, expected) in [
        ("d9ea6781", orna_value_v1::Error::Truncated),
        (
            &format!("d9ea678201d82550{currency}"),
            orna_value_v1::Error::InvalidTag,
        ),
        (
            &format!("d9ea6782d9ea608101d82550{currency}"),
            orna_value_v1::Error::InvalidTag,
        ),
        (
            "d9ea6782d9ea60820121d8254f010101010101010101010101010101",
            orna_value_v1::Error::InvalidValue,
        ),
    ] {
        assert_money_decode_rejects(&hex_bytes(encoded), expected);
    }
}

#[test]
fn rejects_noncanonical_money_decimal_and_tag_forms() {
    let decimal_alias =
        hex_bytes("d9ea6782d9ea608219303422d8255001010101010101010101010101010101");
    assert_money_decode_rejects(&decimal_alias, orna_value_v1::Error::NonCanonical);

    let wide_tag =
        hex_bytes("db000000000000ea6782d9ea60820121d8255001010101010101010101010101010101");
    assert_money_decode_rejects(&wide_tag, orna_value_v1::Error::NonCanonical);
}

#[test]
fn rejects_money_currency_witness_with_different_object_id() {
    let declared_currency = [0x07; 16];
    let payload_currency = [0x08; 16];
    let type_node = Raw::Array(vec![
        Raw::Int(BigInt::from(8_i64)),
        Raw::Tag(
            37,
            Box::new(Raw::Bytes(declared_currency.into())),
        ),
    ]);
    let witnessed_money = Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![
            decimal_raw(25, -2),
            Raw::Tag(37, Box::new(Raw::Bytes(payload_currency.into()))),
        ])),
    );
    let mismatched = Raw::Tag(
        60026,
        Box::new(Raw::Array(vec![type_node, witnessed_money])),
    );

    assert_eq!(Value::new(mismatched), Err(orna_value_v1::Error::InvalidValue));
}

#[test]
fn rejects_decimal_and_money_exponents_beyond_exact_limit() {
    for exponent in [1_000_001_i64, -1_000_001_i64] {
        assert_eq!(
            Decimal::try_new(BigInt::from(1_i64), BigInt::from(exponent)),
            Err(orna_value_v1::Error::DecimalLimit)
        );
    }

    let positive =
        hex_bytes("d9ea6782d9ea6082011a000f4241d8255001010101010101010101010101010101");
    assert_eq!(
        Money::decode(&positive),
        Err(orna_value_v1::Error::DecimalLimit)
    );

    let negative =
        hex_bytes("d9ea6782d9ea6082013a000f4240d8255001010101010101010101010101010101");
    assert_eq!(
        Money::decode(&negative),
        Err(orna_value_v1::Error::DecimalLimit)
    );
}
