//! The `core.money` rounding natives (hir-host.md B7): `quantize`, `allocate` and `format` of `Money<C>`. The currency's
//! `code` and `minor_digits` are read from the `impl Currency` of `targs[0]`; the locale of `format` comes from the host's
//! activation. Each call charges one step.

use std::rc::Rc;

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_hir::{DefKind, Program, Ty, TyKind, engine_money::is_currency_protocol};

use crate::decimal::{Decimal, MAX_EXPONENT};
use crate::decimal_round::RoundingMode;
use crate::error::Stop;
use crate::host::UNSUPPORTED;
use crate::machine::Machine;
use crate::natives::decimal_divide::rounding_mode;
use crate::natives::values::exactly;
use crate::value::Value;

/// The currency of a `Money<C>` call: its `code` and its `minor_digits`, read from the `impl Currency` of `C`.
struct Currency {
    code: String,
    minor_digits: usize,
}

/// Reads the `code` and `minor_digits` statics of the currency `targs[0]`, evaluating the impl's `static` members.
fn currency(machine: &mut Machine<'_>, targs: &[Ty]) -> Result<Currency, Stop> {
    let program = machine.program;
    let owner = match targs.first().and_then(|ty| program.tys.kind(*ty)) {
        Some(TyKind::Adt(owner, _)) => *owner,
        _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
    };
    let implementation = currency_impl(program, owner).ok_or(Stop::Error("ORNA-EVAL-TYPE"))?;
    let mut code = None;
    let mut minor_digits = None;
    for &member in implementation {
        match program.defs[member].name.as_str() {
            "code" => code = Some(machine.global(member)?),
            "minor_digits" => minor_digits = Some(machine.global(member)?),
            _ => {}
        }
    }
    let (Some(Value::Text(code)), Some(Value::Int(minor_digits))) = (code, minor_digits) else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let minor_digits = minor_digits
        .to_usize()
        .filter(|digits| *digits <= MAX_EXPONENT)
        .ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    Ok(Currency { code, minor_digits })
}

/// The member definitions of the `impl Currency` declared on `owner`, if it has one.
fn currency_impl(program: &Program, owner: orna_hir::DefId) -> Option<&[orna_hir::DefId]> {
    program
        .instances
        .get(&owner)?
        .iter()
        .find_map(|implementation| match &program.defs[*implementation].kind {
            DefKind::Impl(declared)
                if declared
                    .protocol
                    .is_some_and(|protocol| is_currency_protocol(program, protocol)) =>
            {
                Some(declared.members.as_slice())
            }
            _ => None,
        })
}

/// The amount and currency operand of a unary money native.
fn money(value: Value) -> Result<Decimal, Stop> {
    match value {
        Value::Money { amount, .. } => Ok(amount),
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

/// The optional rounding argument: `null` or `None` is absent, `Some` of a `core.Rounding` member is the mode.
fn optional_rounding(value: &Value) -> Result<Option<RoundingMode>, Stop> {
    match value {
        Value::Null | Value::Option(None) => Ok(None),
        Value::Option(Some(inner)) => rounding_mode(inner).map(Some),
        other => rounding_mode(other).map(Some),
    }
}

/// `quantize(Money<C>, Rounding): Money<C>`: the amount rounded to the currency's minor digits under the rounding mode.
pub(super) fn quantize(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [amount, rounding] = exactly::<2>(arguments)?;
    let Value::Money { amount, currency } = amount else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let mode = rounding_mode(&rounding)?;
    let minor_digits = currency_digits(machine, targs)?;
    Ok(Value::Money {
        amount: amount.round_to_scale_with(minor_digits, mode)?,
        currency,
    })
}

/// `allocate(Money<C>, [Int], Rounding?): [Money<C>]`: the amount split by integer weights into the currency's minor
/// units, so the parts sum to the amount. The leftover units go to the largest fractional remainders, ties to the
/// lower input index (ORNA-MONEY-006). An amount finer than the minor digits needs a rounding mode.
pub(super) fn allocate(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [amount, weights, rounding] = exactly::<3>(arguments)?;
    let (amount, currency) = match amount {
        Value::Money { amount, currency } => (amount, currency),
        _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
    };
    let Value::List(weights) = weights else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let weights = weights
        .iter()
        .map(|weight| match weight {
            Value::Int(weight) => Ok(weight.clone()),
            _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
        })
        .collect::<Result<Vec<BigInt>, Stop>>()?;
    let mode = optional_rounding(&rounding)?;
    let minor_digits = currency_digits(machine, targs)?;
    let shares = allocate_units(&amount, &weights, minor_digits, mode)?;
    let parts = shares
        .into_iter()
        .map(|share| {
            Ok(Value::Money {
                amount: Decimal::new(share, -BigInt::from(minor_digits))?,
                currency,
            })
        })
        .collect::<Result<Vec<Value>, Stop>>()?;
    Ok(Value::List(Rc::from(parts)))
}

/// The integer minor-unit shares of `amount` under `weights`, which sum to the amount's units.
fn allocate_units(
    amount: &Decimal,
    weights: &[BigInt],
    scale: usize,
    mode: Option<RoundingMode>,
) -> Result<Vec<BigInt>, Stop> {
    if weights.is_empty() || weights.iter().any(Signed::is_negative) {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let total_weight = weights
        .iter()
        .fold(BigInt::zero(), |total, weight| total + weight);
    if total_weight.sign() != Sign::Plus {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let units = match (amount.scaled_units(scale)?, mode) {
        (Some(units), _) => units,
        (None, Some(mode)) => amount
            .round_to_scale_with(scale, mode)?
            .scaled_units(scale)?
            .ok_or(Stop::Error("ORNA-EVAL-VALUE"))?,
        (None, None) => return Err(Stop::Error("ORNA-EVAL-VALUE")),
    };
    let negative = units.sign() == Sign::Minus;
    let magnitude = units.abs();
    let mut shares = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    let mut assigned = BigInt::zero();
    for (index, weight) in weights.iter().enumerate() {
        let (share, remainder) = (&magnitude * weight).div_rem(&total_weight);
        assigned += &share;
        shares.push(share);
        remainders.push((index, remainder));
    }
    let leftover = (&magnitude - assigned)
        .to_usize()
        .filter(|count| *count <= weights.len())
        .ok_or(Stop::Error("ORNA-EVAL-LIMIT"))?;
    remainders.sort_by(|(left_index, left), (right_index, right)| {
        right.cmp(left).then(left_index.cmp(right_index))
    });
    for (index, _) in remainders.into_iter().take(leftover) {
        shares[index] += 1;
    }
    Ok(shares
        .into_iter()
        .map(|share| if negative { -share } else { share })
        .collect())
}

/// `format(Money<C>, Rounding): Str`: the amount rounded to the currency's minor digits, grouped and marked as the
/// host's locale profile says. The locale is the host's activation locale; a host with no activation stops UNSUPPORTED.
pub(super) fn format(
    machine: &mut Machine<'_>,
    targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    machine.tick(1)?;
    let [amount, rounding] = exactly::<2>(arguments)?;
    let amount = money(amount)?;
    let mode = rounding_mode(&rounding)?;
    let info = currency(machine, targs)?;
    let locale = machine
        .host
        .activation()
        .map(|activation| activation.locale.clone())
        .ok_or(Stop::Error(UNSUPPORTED))?;
    let text = format_money(
        &amount.round_to_scale_with(info.minor_digits, mode)?,
        &info,
        &locale,
    )?;
    Ok(Value::Text(text))
}

/// The minor digits of the currency `targs[0]`.
fn currency_digits(machine: &mut Machine<'_>, targs: &[Ty]) -> Result<usize, Stop> {
    Ok(currency(machine, targs)?.minor_digits)
}

/// The text of a rounded amount in `currency` under `locale`: the locale's grouping and decimal marks, and the
/// currency symbol or code.
fn format_money(amount: &Decimal, currency: &Currency, locale: &str) -> Result<String, Stop> {
    let code = &currency.code;
    if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    let (grouping, decimal_separator, suffix_symbol) = match locale {
        "en" | "en-US" | "en-GB" | "ja-JP" => (",", ".", false),
        "fr-FR" => ("\u{202f}", ",", true),
        "de-DE" => (".", ",", true),
        _ => return Err(Stop::Error("ORNA-EVAL-VALUE")),
    };
    let symbol = match code.as_str() {
        "GBP" => "£",
        "USD" => "$",
        "EUR" => "€",
        "JPY" => "¥",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        "CHF" => "CHF",
        _ => code.as_str(),
    };
    let minor_digits = currency.minor_digits;
    let units = amount
        .scaled_units(minor_digits)?
        .ok_or(Stop::Error("ORNA-EVAL-VALUE"))?;
    let negative = units.sign() == Sign::Minus;
    let digits = units.abs().to_str_radix(10);
    let (whole, fraction) = if minor_digits == 0 {
        (digits, String::new())
    } else if digits.len() <= minor_digits {
        (
            "0".to_owned(),
            format!("{}{}", "0".repeat(minor_digits - digits.len()), digits),
        )
    } else {
        let split = digits.len() - minor_digits;
        (digits[..split].to_owned(), digits[split..].to_owned())
    };
    let mut number = String::with_capacity(whole.len() + whole.len() / 3);
    let mut first_group = whole.len() % 3;
    if first_group == 0 {
        first_group = 3;
    }
    number.push_str(&whole[..first_group]);
    for start in (first_group..whole.len()).step_by(3) {
        number.push_str(grouping);
        number.push_str(&whole[start..start + 3]);
    }
    if minor_digits > 0 {
        number.push_str(decimal_separator);
        number.push_str(&fraction);
    }
    let sign = if negative { "-" } else { "" };
    Ok(if symbol == code {
        if suffix_symbol {
            format!("{sign}{number}\u{00a0}{code}")
        } else {
            format!("{sign}{code}\u{00a0}{number}")
        }
    } else if suffix_symbol {
        format!("{sign}{number}\u{00a0}{symbol}")
    } else {
        format!("{sign}{symbol}{number}")
    })
}

#[cfg(test)]
mod tests {
    use num_bigint::BigInt;

    use super::*;

    fn decimal(coefficient: i64, exponent: i64) -> Decimal {
        Decimal::new(BigInt::from(coefficient), BigInt::from(exponent)).expect("in range")
    }

    fn gbp() -> Currency {
        Currency {
            code: "GBP".to_owned(),
            minor_digits: 2,
        }
    }

    #[test]
    fn the_six_rounding_modes_round_a_tie_and_a_fraction_as_documented() {
        // 2.5 and -2.5 at scale 0, then 2.3 and -2.3.
        let cases: [(i64, [i64; 6]); 4] = [
            (25, [2, 3, 2, 3, 2, 3]),
            (-25, [-2, -3, -2, -3, -3, -2]),
            (23, [2, 2, 2, 3, 2, 3]),
            (-23, [-2, -2, -2, -3, -3, -2]),
        ];
        let modes = [
            RoundingMode::HalfEven,
            RoundingMode::HalfUp,
            RoundingMode::TowardZero,
            RoundingMode::AwayFromZero,
            RoundingMode::Floor,
            RoundingMode::Ceil,
        ];
        for (value, expected) in cases {
            for (mode, want) in modes.iter().zip(expected) {
                let rounded = decimal(value, -1)
                    .round_to_scale_with(0, *mode)
                    .expect("rounds");
                assert_eq!(rounded, decimal(want, 0), "{value}e-1 under {mode:?}");
            }
        }
    }

    #[test]
    fn allocate_of_one_hundred_pounds_over_three_equal_weights_sums_to_one_hundred() {
        let shares = allocate_units(
            &decimal(100, 0),
            &[BigInt::from(1), BigInt::from(1), BigInt::from(1)],
            2,
            None,
        )
        .expect("allocates");
        assert_eq!(
            shares,
            vec![BigInt::from(3334), BigInt::from(3333), BigInt::from(3333)]
        );
        assert_eq!(shares.iter().sum::<BigInt>(), BigInt::from(10_000));
    }

    #[test]
    fn allocate_gives_leftover_units_to_the_largest_remainders_then_the_lower_index() {
        // 0.10 split 1:2 is 3.33 and 6.67 minor units: the second share has the larger remainder.
        let shares = allocate_units(
            &decimal(10, -2),
            &[BigInt::from(1), BigInt::from(2)],
            2,
            None,
        )
        .expect("allocates");
        assert_eq!(shares, vec![BigInt::from(3), BigInt::from(7)]);
    }

    #[test]
    fn allocate_of_a_finer_amount_needs_a_rounding_mode() {
        let weights = [BigInt::from(1)];
        assert!(allocate_units(&decimal(1005, -3), &weights, 2, None).is_err());
        let shares = allocate_units(
            &decimal(1005, -3),
            &weights,
            2,
            Some(RoundingMode::HalfEven),
        )
        .expect("allocates");
        assert_eq!(shares, vec![BigInt::from(100)]);
    }

    #[test]
    fn allocate_rejects_negative_weights_and_zero_total() {
        assert!(
            allocate_units(
                &decimal(1, 0),
                &[BigInt::from(-1), BigInt::from(2)],
                2,
                None
            )
            .is_err()
        );
        assert!(allocate_units(&decimal(1, 0), &[BigInt::from(0)], 2, None).is_err());
    }

    #[test]
    fn format_groups_and_marks_by_locale() {
        let amount = decimal(12345, -1);
        assert_eq!(
            format_money(&amount, &gbp(), "en-GB").expect("formats"),
            "£1,234.50"
        );
        assert_eq!(
            format_money(&decimal(-12345, -1), &gbp(), "en-GB").expect("formats"),
            "-£1,234.50"
        );
        let eur = Currency {
            code: "EUR".to_owned(),
            minor_digits: 2,
        };
        assert_eq!(
            format_money(&amount, &eur, "de-DE").expect("formats"),
            "1.234,50\u{a0}€"
        );
        let xyz = Currency {
            code: "XYZ".to_owned(),
            minor_digits: 0,
        };
        assert_eq!(
            format_money(&decimal(7, 0), &xyz, "en").expect("formats"),
            "XYZ\u{a0}7"
        );
        assert!(format_money(&amount, &gbp(), "xx-XX").is_err());
    }
}
