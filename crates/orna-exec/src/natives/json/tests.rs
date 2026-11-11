//! Focused tests of the JSON number tokens and the strict parser.

use num_bigint::BigInt;

use super::number;
use super::parse::{Node, parse};
use crate::decimal::Decimal;
use crate::error::Stop;
use crate::machine::ExecLimits;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 1_000,
    max_depth: 8,
    max_collection_items: 2_048,
    max_string_bytes: 64,
    max_integer_digits: 40,
};

fn decimal(coefficient: i64, exponent: i64) -> Decimal {
    Decimal::new(BigInt::from(coefficient), BigInt::from(exponent)).expect("in-range decimal")
}

#[test]
fn decimal_tokens_write_the_exact_value_with_a_point() {
    let cases = [
        (15, -1, "1.5"),
        (1, 3, "1000.0"),
        (-5, -3, "-0.005"),
        (0, 0, "0.0"),
        (123, -1, "12.3"),
        (7, -4, "0.0007"),
    ];
    for (coefficient, exponent, expected) in cases {
        let token = number::decimal_token(&decimal(coefficient, exponent), &LIMITS).expect("token");
        assert_eq!(
            token, expected,
            "coefficient {coefficient} exponent {exponent}"
        );
    }
}

#[test]
fn decimal_token_round_trips_through_the_decimal_reader() {
    for token in ["1.5", "-0.005", "1000.0", "12.25"] {
        let value = number::decimal(token, &LIMITS).expect("decimal token");
        assert_eq!(
            number::decimal_token(&value, &LIMITS).expect("token"),
            token
        );
    }
}

#[test]
fn exponent_form_is_a_decimal_and_not_an_integer() {
    let value = number::decimal("1e3", &LIMITS).expect("exponent decimal");
    assert_eq!(
        number::decimal_token(&value, &LIMITS).expect("token"),
        "1000.0"
    );
    assert!(number::integer("1e3", &LIMITS).is_err());
    assert!(number::integer("1.0", &LIMITS).is_err());
    assert!(number::decimal("12", &LIMITS).is_err());
}

#[test]
fn integer_reads_signed_digits_and_bounds_them() {
    assert_eq!(
        number::integer("-12", &LIMITS).expect("integer"),
        BigInt::from(-12)
    );
    let too_long = "9".repeat(LIMITS.max_integer_digits + 1);
    assert!(matches!(
        number::integer(&too_long, &LIMITS),
        Err(Stop::Error("ORNA-EVAL-LIMIT"))
    ));
}

#[test]
fn float_rejects_overflow_to_infinity() {
    assert_eq!(number::float("2.5").expect("float"), 2.5);
    assert!(number::float("1e400").is_err());
}

#[test]
fn parser_accepts_a_nested_document_and_rejects_trailing_commas() {
    let node = parse(&LIMITS, r#"{"a":[1,"x",null],"b":true}"#).expect("document");
    let Node::Object(members) = node else {
        panic!("object expected");
    };
    assert_eq!(members.len(), 2);
    assert!(parse(&LIMITS, r#"{"a":1,}"#).is_err());
    assert!(parse(&LIMITS, "[1, 2] x").is_err());
}
