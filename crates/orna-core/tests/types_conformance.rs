//! Focused core value checks for the frozen Orna 1.0 types contract.
//!
//! Evidence anchors: source/05-types.md lines 45, 65, 103-115. These checks
//! cover the finite/non-finite admission boundary, IEEE ordinary equality,
//! unordered NaN comparisons, bit preservation and checked result-row
//! transport. Type checking and aggregate execution are compiler/evaluator
//! responsibilities, not implemented by `orna-core`.

use orna_core::{
    types::{ResolvedType, StandardScalar},
    value::{ResultColumn, ResultRow, ResultRows, RuntimeFloat, RuntimeValue},
};

const NUMERIC_FIXTURE: &str = include_str!("fixtures/types_numeric_literal_context.orna");
const FLOAT_VECTORS: &str = include_str!("fixtures/float-vectors.json");

#[test]
fn checked_in_numeric_type_fixture_is_valid_source() {
    let parsed = orna_syntax_v1::parse_module(NUMERIC_FIXTURE);
    assert!(parsed.is_ok(), "fixture diagnostics: {:?}", parsed.diagnostics);
}

#[test]
fn binary64_float_constructor_accepts_nan_and_infinities() {
    for bits in [
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0001,
        0xfff0_0000_0000_0001,
    ] {
        let value = RuntimeFloat::new(f64::from_bits(bits))
            .expect("every IEEE-754 binary64 bit pattern is a Float value");
        assert_eq!(value.value().to_bits(), bits);
    }
}

#[test]
fn reference_total_order_vectors_survive_runtime_float_construction() {
    let vectors: serde_json::Value = serde_json::from_str(FLOAT_VECTORS).unwrap();
    let expected = vectors["ascending_total_order"].as_array().unwrap();
    let values = expected
        .iter()
        .map(|vector| {
            let bits = u64::from_str_radix(vector["bits"].as_str().unwrap(), 16).unwrap();
            RuntimeFloat::new(f64::from_bits(bits)).unwrap()
        })
        .collect::<Vec<_>>();

    assert_eq!(values.len(), expected.len());
    for pair in expected.windows(2) {
        let left = u64::from_str_radix(pair[0]["bits"].as_str().unwrap(), 16).unwrap();
        let right = u64::from_str_radix(pair[1]["bits"].as_str().unwrap(), 16).unwrap();
        assert_eq!(
            orna_value_v1::float_total_cmp(left, right),
            std::cmp::Ordering::Less
        );
    }
}

#[test]
fn reference_float_equality_vectors_match_runtime_float() {
    let vectors: serde_json::Value = serde_json::from_str(FLOAT_VECTORS).unwrap();
    for vector in vectors["ordinary_equality"].as_array().unwrap() {
        let bits = |side: &str| {
            u64::from_str_radix(vector[side].as_str().unwrap(), 16).unwrap()
        };
        let left = RuntimeFloat::new(f64::from_bits(bits("left"))).unwrap();
        let right = RuntimeFloat::new(f64::from_bits(bits("right"))).unwrap();
        assert_eq!(left == right, vector["equal"].as_bool().unwrap());
    }
}

#[test]
fn ordinary_float_equality_matches_ieee_zero_and_nan_rules() {
    let negative_zero = RuntimeFloat::new(f64::from_bits(0x8000_0000_0000_0000)).unwrap();
    let positive_zero = RuntimeFloat::new(0.0).unwrap();
    assert_eq!(negative_zero, positive_zero);

    let nan = RuntimeFloat::new(f64::from_bits(0x7ff8_0000_0000_0000)).unwrap();
    assert_ne!(nan, nan);
}

#[test]
fn ordered_float_comparisons_with_nan_are_all_false() {
    let nan = RuntimeFloat::new(f64::from_bits(0x7ff8_0000_0000_0000))
        .unwrap()
        .value();
    let number = RuntimeFloat::new(1.0).unwrap().value();

    assert!(!(nan < number));
    assert!(!(nan <= number));
    assert!(!(nan > number));
    assert!(!(nan >= number));
    assert!(!(number < nan));
    assert!(!(number <= nan));
    assert!(!(number > nan));
    assert!(!(number >= nan));
}

#[test]
fn result_rows_preserve_non_finite_float_values_and_signed_zero_bits() {
    let float_type = ResolvedType::scalar(StandardScalar::Float);
    let column = ResultColumn::new("value", float_type, false).unwrap();
    let values = [
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::from_bits(0x7ff8_0000_0000_0042),
        -0.0,
        0.0,
    ];
    let rows = ResultRows::new(
        [column],
        values.into_iter().map(|value| {
            ResultRow::new([RuntimeValue::Float(RuntimeFloat::new(value).unwrap())])
        }),
    )
    .expect("all binary64 values are valid values in a Float column");

    let bits = rows
        .rows()
        .iter()
        .map(|row| match &row.values()[0] {
            RuntimeValue::Float(value) => value.value().to_bits(),
            other => panic!("unexpected value: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        bits,
        [
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_0042,
            0x8000_0000_0000_0000,
            0x0000_0000_0000_0000,
        ]
    );
}
