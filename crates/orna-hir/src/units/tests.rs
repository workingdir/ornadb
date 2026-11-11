//! Exact algebra of unit keys (units.md sections 3.1 to 3.5): rationals, exponent maps and keys.

use num_bigint::BigInt;

use super::{Exponents, KeyError, Rational, UnitKey};
use crate::DefId;

fn rat(num: i64, den: i64) -> Rational {
    Rational::new(BigInt::from(num), BigInt::from(den)).expect("a valid rational")
}

fn axis(index: u32) -> DefId {
    DefId::new(index)
}

/// The key of one axis raised to `exponent`, scaled by `scale`.
fn key(axis: DefId, exponent: i32, scale: Rational) -> UnitKey {
    UnitKey::base(axis)
        .pow(exponent)
        .expect("a bounded power")
        .scaled_by(&scale)
        .expect("a positive scale")
}

#[test]
fn rationals_reduce_and_carry_the_sign_in_the_numerator() {
    let value = rat(6, -4);
    assert_eq!(value.numerator(), &BigInt::from(-3));
    assert_eq!(value.denominator(), &BigInt::from(2));
    assert!(!value.is_positive());
}

#[test]
fn a_zero_denominator_is_refused() {
    assert_eq!(
        Rational::new(BigInt::from(1), BigInt::from(0)),
        Err(KeyError::ZeroDivision)
    );
}

#[test]
fn decimal_coefficients_are_exact() {
    let mile = Rational::parse_decimal("1609.344")
        .expect("a decimal")
        .expect("in budget");
    assert_eq!(mile, rat(201_168, 125));
    assert!(mile.is_finite_decimal());
    assert!(!rat(1, 3).is_finite_decimal());
    assert!(Rational::parse_decimal("1e3").is_none());
}

#[test]
fn multiplying_then_dividing_by_a_key_returns_the_key() {
    let metre = axis(0);
    let second = axis(1);
    for numerator in [1_i64, 2, 3, 5, 1000] {
        for denominator in [1_i64, 3, 7] {
            let value = key(metre, 2, rat(numerator, denominator));
            for exponent in -3..=3 {
                let other = key(second, exponent, rat(denominator, numerator));
                assert_eq!(
                    value.mul(&other).expect("mul").div(&other).expect("div"),
                    value
                );
            }
        }
    }
}

#[test]
fn the_dimensionless_key_is_the_identity_of_multiplication() {
    let value = key(axis(0), -2, rat(1609, 1000));
    assert_eq!(value.mul(&UnitKey::dimensionless()).expect("mul"), value);
    assert!(UnitKey::dimensionless().is_dimensionless());
}

#[test]
fn a_power_is_the_repeated_product() {
    let metre = UnitKey::base(axis(0));
    let squared = metre.mul(&metre).expect("mul");
    assert_eq!(metre.pow(2).expect("pow"), squared);
    let inverse = metre.pow(-1).expect("pow");
    assert_eq!(metre.mul(&inverse).expect("mul"), UnitKey::dimensionless());
    assert!(
        metre
            .mul(&inverse)
            .expect("mul")
            .exponents()
            .is_dimensionless()
    );
}

#[test]
fn exponents_beyond_the_bound_are_refused() {
    let metre = UnitKey::base(axis(0));
    assert!(metre.pow(32).is_ok());
    assert_eq!(metre.pow(33), Err(KeyError::Exponent));
    assert_eq!(metre.pow(-33), Err(KeyError::Exponent));
}

#[test]
fn a_rational_wider_than_the_budget_is_refused() {
    assert_eq!(Rational::from_int(2).pow(4096), Err(KeyError::Budget));
    assert!(Rational::from_int(2).pow(100).is_ok());
}

#[test]
fn a_non_positive_scale_is_refused() {
    assert_eq!(
        UnitKey::new(Exponents::default(), rat(0, 1)),
        Err(KeyError::NonPositiveScale)
    );
    assert_eq!(
        UnitKey::base(axis(0)).scaled_by(&rat(-1, 1)),
        Err(KeyError::NonPositiveScale)
    );
}

#[test]
fn ratios_of_one_dimension_are_exact() {
    let metre = UnitKey::base(axis(0));
    let kilometre = metre.scaled_by(&rat(1000, 1)).expect("scale");
    let mile = metre.scaled_by(&rat(1_609_344, 1000)).expect("scale");
    assert_eq!(kilometre.ratio_to(&metre).expect("ratio"), rat(1000, 1));
    assert_eq!(
        mile.ratio_to(&kilometre).expect("ratio"),
        rat(25_146, 15_625)
    );
    assert!(kilometre.same_dimension(&mile));
    assert!(!kilometre.same_dimension(&UnitKey::base(axis(1))));
}

#[test]
fn the_finer_key_has_the_smaller_scale() {
    let metre = UnitKey::base(axis(0));
    let kilometre = metre.scaled_by(&rat(1000, 1)).expect("scale");
    let mile = metre.scaled_by(&rat(1_609_344, 1000)).expect("scale");
    assert_eq!(UnitKey::finer(&kilometre, &mile), &kilometre);
    assert_eq!(UnitKey::finer(&mile, &kilometre), &kilometre);
    assert_eq!(UnitKey::finer(&metre, &metre), &metre);
}

#[test]
fn exponent_maps_drop_zero_entries_in_axis_order() {
    let metre = axis(0);
    let second = axis(1);
    let sum = Exponents::axis(second)
        .add(&Exponents::axis(metre))
        .expect("add");
    assert_eq!(
        sum.iter().collect::<Vec<_>>(),
        vec![(metre, 1), (second, 1)]
    );
    let cancelled = sum
        .add(&Exponents::axis(metre).scale(-1).expect("scale"))
        .expect("add");
    assert_eq!(cancelled.iter().collect::<Vec<_>>(), vec![(second, 1)]);
    assert_eq!(cancelled.get(metre), 0);
}
