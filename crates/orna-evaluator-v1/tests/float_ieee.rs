use orna_evaluator_v1::{AdmittedReplSession, Environment, Limits, evaluate_expression};
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

fn repl_value(source: &str) -> Value {
    AdmittedReplSession::new(Limits::default())
        .submit(source)
        .unwrap_or_else(|error| panic!("{source}: unexpected {error:?}"))
        .unwrap_or_else(|| panic!("{source}: produced no value"))
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

// Audit W01-01: the canonical text forms `Float.nan`, `Float.infinity` and
// `-Float.infinity` (ORNA-FORMAT, 29-formats.md) must also resolve as source values.
#[test]
fn float_nan_and_infinity_constants_resolve_in_source() {
    assert_eq!(
        repl_value("Float.nan"),
        Value::float_bits(CANONICAL_NAN_BITS)
    );
    assert_eq!(
        repl_value("Float.infinity"),
        Value::float_bits(f64::INFINITY.to_bits())
    );
    assert_eq!(
        repl_value("-Float.infinity"),
        Value::float_bits(f64::NEG_INFINITY.to_bits())
    );
}

#[test]
fn float_nan_constant_is_unequal_to_itself() {
    assert_eq!(repl_value("Float.nan == Float.nan"), Value::new(Raw::Bool(false)).unwrap());
}
