//! Focused tests of the canonical Orna token forms: decimals, floats, UUIDs and string escapes.

use num_bigint::BigInt;

use super::encode::{decimal_text, escape, float_text, uuid_text};
use crate::decimal::Decimal;

fn decimal(coefficient: i64, exponent: i64) -> Decimal {
    Decimal::new(BigInt::from(coefficient), BigInt::from(exponent)).expect("in-range decimal")
}

#[test]
fn decimals_are_normalised_coefficient_exponent_and_suffix() {
    assert_eq!(decimal_text(&decimal(15, -1)), "15e-1.decimal");
    assert_eq!(decimal_text(&decimal(-5, -3)), "-5e-3.decimal");
    assert_eq!(decimal_text(&decimal(0, 0)), "0e0.decimal");
}

#[test]
fn floats_use_seventeen_significant_digits_and_the_f_suffix() {
    assert_eq!(float_text(1.0).expect("finite"), "1.0000000000000000e0f");
    assert_eq!(float_text(0.1).expect("finite"), "1.0000000000000001e-1f");
    assert_eq!(float_text(-0.0).expect("finite"), "-0.0000000000000000e0f");
}

#[test]
fn nonfinite_floats_use_their_named_forms() {
    assert_eq!(float_text(f64::NAN).expect("nan"), "Float.nan");
    assert_eq!(float_text(f64::INFINITY).expect("inf"), "Float.infinity");
    assert_eq!(
        float_text(f64::NEG_INFINITY).expect("-inf"),
        "-Float.infinity"
    );
}

#[test]
fn uuids_are_lowercase_eight_four_four_four_twelve() {
    let bytes: [u8; 16] = core::array::from_fn(|index| index as u8);
    assert_eq!(uuid_text(&bytes), "00010203-0405-0607-0809-0a0b0c0d0e0f");
}

#[test]
fn strings_escape_quote_backslash_named_controls_and_braces() {
    let mut out = String::new();
    escape("a\"b\\c\n\r\t{", &mut out);
    assert_eq!(out, "\"a\\\"b\\\\c\\n\\r\\t\\u{7b}\"");
}

#[test]
fn other_control_scalars_use_minimal_lowercase_hex_escapes() {
    let mut out = String::new();
    escape("\u{1}\u{8}\u{c}\u{1b}\u{7f}", &mut out);
    assert_eq!(out, "\"\\u{1}\\u{8}\\u{c}\\u{1b}\\u{7f}\"");
}

#[test]
fn ordinary_unicode_scalars_stay_as_utf8() {
    let mut out = String::new();
    escape("héllo", &mut out);
    assert_eq!(out, "\"héllo\"");
}
