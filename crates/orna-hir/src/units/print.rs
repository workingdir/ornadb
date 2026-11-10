//! Printing of unit keys, dimensions and quantity types (units.md section 4, rules 1 to 3).
//!
//! Printing is total and deterministic. A key with a declared unit prints that unit's name; a key of scale 1 prints
//! its axes; any other key prints its scale over its axes. Axes and names are family-qualified (`Family.name`) when the
//! unit is a variant of a family.

use crate::ids::UnitKeyId;
use crate::units::{Exponents, Rational};
use crate::{DefId, DefKind, Program};

/// Prints the quantity type `Quantity<num, key>`, where `num` is the already printed numeric type.
#[must_use]
pub fn quantity_text(program: &Program, num: &str, key: UnitKeyId) -> String {
    format!("Quantity<{num}, {}>", key_text(program, key))
}

/// Prints the unit key `key` by rules 1 to 3.
#[must_use]
pub fn key_text(program: &Program, key: UnitKeyId) -> String {
    if let Some(unit) = program.units.declared_unit(key) {
        return unit_name(program, unit);
    }
    let unit_key = program.units.key(key);
    let axes = axes_text(program, unit_key.exponents());
    let scale = unit_key.scale();
    if *scale == Rational::one() {
        return axes;
    }
    let scale_text = if scale.is_integer() {
        scale.numerator().to_string()
    } else {
        format!("{}/{}", scale.numerator(), scale.denominator())
    };
    format!("({scale_text}*{axes})")
}

/// Prints the axes of an exponent map: positive exponents before the slash, negative ones after it, in ascending
/// canonical order. Several denominators are parenthesised, so `kilogram/(metre*second)` is unambiguous.
#[must_use]
pub fn axes_text(program: &Program, exponents: &Exponents) -> String {
    let mut numerator = Vec::new();
    let mut denominator = Vec::new();
    for (axis, exponent) in exponents.iter() {
        let name = unit_name(program, axis);
        let magnitude = exponent.unsigned_abs();
        let term = if magnitude == 1 {
            name
        } else {
            format!("{name}^{magnitude}")
        };
        if exponent > 0 {
            numerator.push(term);
        } else {
            denominator.push(term);
        }
    }
    let numerator = if numerator.is_empty() {
        "1".to_owned()
    } else {
        numerator.join("*")
    };
    match denominator.len() {
        0 => numerator,
        1 => format!("{numerator}/{}", denominator[0]),
        _ => format!("{numerator}/({})", denominator.join("*")),
    }
}

/// The name of a unit: `Family.name` for a variant of a family, otherwise the bare name.
#[must_use]
pub fn unit_name(program: &Program, unit: DefId) -> String {
    let Some(definition) = program.defs.get(unit) else {
        return "?".to_owned();
    };
    let family = match &definition.kind {
        DefKind::Unit(unit_def) => unit_def.family,
        _ => None,
    };
    match family.and_then(|owner| program.defs.get(owner)) {
        Some(owner) => format!("{}.{}", owner.name.as_str(), definition.name.as_str()),
        None => definition.name.as_str().to_owned(),
    }
}
