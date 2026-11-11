//! The canonical bytes of the value kinds this stage adds, against the old evaluator's encoders (`raw_date`,
//! `raw_digest`, `raw_money`, `object_id_raw`), and the decode rules that read them back.

use num_bigint::BigInt;
use orna_value::Raw;

use crate::calendar::days_from_civil;
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::raw_value::from_raw;
use crate::value::Value;

fn decimal(coefficient: i64, exponent: i64) -> Decimal {
    Decimal::new(BigInt::from(coefficient), BigInt::from(exponent)).expect("a valid decimal")
}

fn uuid_raw(bytes: [u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(bytes.to_vec())))
}

fn digest_raw(bytes: [u8; 32]) -> Raw {
    Raw::Tag(
        60025,
        Box::new(Raw::Array(vec![
            Raw::Text("sha256".to_owned()),
            Raw::Bytes(bytes.to_vec()),
        ])),
    )
}

fn decimal_raw(coefficient: i64, exponent: i64) -> Raw {
    Raw::Tag(
        60000,
        Box::new(Raw::Array(vec![
            Raw::Int(coefficient.into()),
            Raw::Int(exponent.into()),
        ])),
    )
}

#[test]
fn date_encodes_as_its_text_and_reads_back() {
    let days = days_from_civil(2024, 2, 29);
    assert_eq!(
        Value::Date(days).to_raw(),
        Ok(Raw::Tag(
            60001,
            Box::new(Raw::Text("2024-02-29".to_owned()))
        ))
    );
    let raw = Raw::Tag(60001, Box::new(Raw::Text("2024-02-29".to_owned())));
    assert_eq!(from_raw(&raw).ok(), Some(Value::Date(days)));
}

#[test]
fn date_text_that_is_not_a_real_date_is_rejected() {
    for text in ["2023-02-29", "2026-13-01", "0000-01-01", "2026-1-01"] {
        let raw = Raw::Tag(60001, Box::new(Raw::Text(text.to_owned())));
        assert!(
            matches!(from_raw(&raw), Err(Stop::Error("ORNA-EVAL-VALUE"))),
            "{text}"
        );
    }
}

#[test]
fn uuid_encodes_as_its_object_id_and_reads_back() {
    let bytes = [0x55; 16];
    assert_eq!(Value::Uuid(bytes).to_raw(), Ok(uuid_raw(bytes)));
    assert_eq!(from_raw(&uuid_raw(bytes)).ok(), Some(Value::Uuid(bytes)));
}

#[test]
fn uuid_of_the_wrong_length_is_rejected() {
    let raw = Raw::Tag(37, Box::new(Raw::Bytes(vec![1; 15])));
    assert!(from_raw(&raw).is_err());
}

#[test]
fn digest_encodes_as_the_sha256_pair_and_reads_back() {
    let bytes = [7; 32];
    assert_eq!(Value::Digest(bytes).to_raw(), Ok(digest_raw(bytes)));
    assert_eq!(
        from_raw(&digest_raw(bytes)).ok(),
        Some(Value::Digest(bytes))
    );
}

#[test]
fn digest_with_another_algorithm_is_rejected() {
    let raw = Raw::Tag(
        60025,
        Box::new(Raw::Array(vec![
            Raw::Text("sha512".to_owned()),
            Raw::Bytes(vec![7; 32]),
        ])),
    );
    assert!(from_raw(&raw).is_err());
}

#[test]
fn money_encodes_as_the_old_pair_and_reads_back() {
    let currency = [0x55; 16];
    let value = Value::Money {
        amount: decimal(125, -2),
        currency,
    };
    let expected = Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![decimal_raw(125, -2), uuid_raw(currency)])),
    );
    assert_eq!(value.to_raw(), Ok(expected.clone()));
    assert_eq!(from_raw(&expected).ok(), Some(value));
}

#[test]
fn money_with_a_non_canonical_decimal_is_rejected() {
    let currency = [0x55; 16];
    let raw = Raw::Tag(
        60007,
        Box::new(Raw::Array(vec![decimal_raw(10, -1), uuid_raw(currency)])),
    );
    assert!(from_raw(&raw).is_err());
}

#[test]
fn period_has_no_canonical_form() {
    assert_eq!(
        Value::Period(BigInt::from(3)).to_raw(),
        Err("ORNA-EVAL-UNSUPPORTED")
    );
}
