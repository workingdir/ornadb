use orna_evaluator_v1::{Environment, Limits, evaluate_expression};
use orna_value_v1::{CANONICAL_NAN_BITS, Raw, Value};

fn evaluate(source: &str) -> Value {
    evaluate_expression(source, &Environment::new(), Limits::default())
        .unwrap_or_else(|error| panic!("{source}: unexpected {}", error.code()))
}

fn float_bits(source: &str) -> u64 {
    match evaluate(source).raw() {
        Raw::Float(bits) => *bits,
        other => panic!("{source}: expected Float, got {other:?}"),
    }
}

fn bool_result(source: &str) -> bool {
    match evaluate(source).raw() {
        Raw::Bool(value) => *value,
        other => panic!("{source}: expected Bool, got {other:?}"),
    }
}

// Specified: ORNA-NUM-003; Float follows IEEE-754 binary64, so 0/0 yields NaN and
// nonzero/0 yields a signed infinity rather than aborting the activation.
#[test]
fn zero_divided_by_zero_is_canonical_nan() {
    assert_eq!(float_bits("0.0f / 0.0f"), CANONICAL_NAN_BITS);
    assert_eq!(float_bits("0.0f / -0.0f"), CANONICAL_NAN_BITS);
}

#[test]
fn nonzero_divided_by_zero_is_signed_infinity() {
    assert_eq!(float_bits("1.0f / 0.0f"), f64::INFINITY.to_bits());
    assert_eq!(float_bits("-1.0f / 0.0f"), f64::NEG_INFINITY.to_bits());
    assert_eq!(float_bits("1.0f / -0.0f"), f64::NEG_INFINITY.to_bits());
}

#[test]
fn remainder_by_zero_is_canonical_nan() {
    assert_eq!(float_bits("5.0f % 0.0f"), CANONICAL_NAN_BITS);
}

// Specified: ORNA-NUM-003; overflow rounds to a signed infinity rather than failing.
#[test]
fn overflowing_arithmetic_yields_infinity() {
    assert_eq!(float_bits("1.0e308f * 10.0f"), f64::INFINITY.to_bits());
    assert_eq!(float_bits("1.0e308f + 1.0e308f"), f64::INFINITY.to_bits());
    assert_eq!(float_bits("-1.0e308f * 10.0f"), f64::NEG_INFINITY.to_bits());
}

// Specified: ORNA-FLOAT-001; NaN is unequal to itself and every ordered comparison with NaN is false.
#[test]
fn nan_produced_by_arithmetic_is_unequal_and_unordered() {
    assert!(!bool_result("(0.0f / 0.0f) == (0.0f / 0.0f)"));
    assert!(bool_result("(0.0f / 0.0f) != (0.0f / 0.0f)"));
    assert!(!bool_result("(0.0f / 0.0f) < 1.0f"));
    assert!(!bool_result("(0.0f / 0.0f) >= 1.0f"));
}

// Specified: ORNA-NUM-003; `^` is IEEE pow, not exp(b * ln a).
#[test]
fn power_is_exact_for_representable_results() {
    assert_eq!(float_bits("10.0f ^ 2.0f"), 100.0f64.to_bits());
    assert_eq!(float_bits("3.0f ^ 3.0f"), 27.0f64.to_bits());
    assert_eq!(float_bits("2.0f ^ 0.0f"), 1.0f64.to_bits());
    assert_eq!(float_bits("0.0f ^ 0.0f"), 1.0f64.to_bits());
}

#[test]
fn power_accepts_negative_base_with_integral_exponent() {
    assert_eq!(float_bits("(-2.0f) ^ 2.0f"), 4.0f64.to_bits());
    assert_eq!(float_bits("(-2.0f) ^ 3.0f"), (-8.0f64).to_bits());
    assert_eq!(float_bits("-2.0f ^ 3.0f"), (-8.0f64).to_bits());
}

#[test]
fn power_follows_ieee_special_cases() {
    assert_eq!(float_bits("0.0f ^ -1.0f"), f64::INFINITY.to_bits());
    assert_eq!(float_bits("(-8.0f) ^ (1.0f / 3.0f)"), CANONICAL_NAN_BITS);
}

// Specified: ORNA-FLOAT-001; signed zero is a distinct Float value that arithmetic preserves.
#[test]
fn negative_zero_is_preserved_by_arithmetic() {
    assert_eq!(float_bits("-0.0f"), (-0.0f64).to_bits());
    assert_eq!(float_bits("0.0f * -1.0f"), (-0.0f64).to_bits());
    assert_eq!(float_bits("1.0f / (1.0f / 0.0f * -1.0f)"), (-0.0f64).to_bits());
}

// Specified: ORNA-FLOAT-001; NaN is unordered against itself and infinities order by value.
#[test]
fn nan_is_unordered_against_itself_and_infinities_order() {
    assert!(!bool_result("(0.0f / 0.0f) <= (0.0f / 0.0f)"));
    assert!(!bool_result("(0.0f / 0.0f) > (0.0f / 0.0f)"));
    assert!(bool_result("(1.0f / 0.0f) == (2.0f / 0.0f)"));
    assert!(bool_result("(-1.0f / 0.0f) < (1.0f / 0.0f)"));
}

// Specified: ORNA-LIT-002; a Float numeral is correctly rounded to nearest, so an out-of-range
// numeral is a signed infinity and an underflowing numeral is a signed zero.
#[test]
fn out_of_range_float_literals_round_to_signed_infinity_or_zero() {
    assert_eq!(float_bits("1.0e400f"), f64::INFINITY.to_bits());
    assert_eq!(float_bits("-1.0e400f"), f64::NEG_INFINITY.to_bits());
    assert_eq!(float_bits("1.0e-400f"), 0.0f64.to_bits());
}

// Specified: ORNA-FLOAT-007; min prefers -0.0 and max prefers +0.0 among zeros, and any NaN
// operand yields the canonical NaN instead of an evaluation error.
#[test]
fn scalar_min_and_max_follow_float_extrema_rules() {
    assert_eq!(float_bits("std.math.min(0.0f, -0.0f)"), (-0.0f64).to_bits());
    assert_eq!(float_bits("std.math.min(-0.0f, 0.0f)"), (-0.0f64).to_bits());
    assert_eq!(float_bits("std.math.max(-0.0f, 0.0f)"), 0.0f64.to_bits());
    assert_eq!(float_bits("std.math.max(0.0f, -0.0f)"), 0.0f64.to_bits());
    assert_eq!(float_bits("std.math.min(0.0f / 0.0f, 1.0f)"), CANONICAL_NAN_BITS);
    assert_eq!(float_bits("std.math.max(1.0f, 0.0f / 0.0f)"), CANONICAL_NAN_BITS);
}
