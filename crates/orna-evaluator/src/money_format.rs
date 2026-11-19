use crate::*;

pub(crate) fn format_money_value(
    amount: &Value,
    currency_code: &str,
    minor_digits: usize,
    rounding: &str,
    locale: &str,
) -> Result<String, EvaluationError> {
    if currency_code.len() != 3
        || !currency_code
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_uppercase())
    {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let (grouping, decimal_separator, suffix_symbol) = match locale {
        "en" | "en-US" | "en-GB" | "ja-JP" => (",", ".", false),
        "fr-FR" => ("\u{202f}", ",", true),
        "de-DE" => (".", ",", true),
        _ => return Err(error("ORNA-EVAL-VALUE")),
    };
    let symbol = match currency_code {
        "GBP" => "£",
        "USD" => "$",
        "EUR" => "€",
        "JPY" => "¥",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        "CHF" => "CHF",
        _ => currency_code,
    };
    let original = money_amount(amount)?;
    let rounded = quantize_decimal(&original, minor_digits, rounding)?;
    let units =
        decimal_scaled_integer(&rounded, minor_digits)?.ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
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
    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    let mut first_group = whole.len() % 3;
    if first_group == 0 {
        first_group = 3;
    }
    grouped.push_str(&whole[..first_group]);
    for start in (first_group..whole.len()).step_by(3) {
        grouped.push_str(grouping);
        grouped.push_str(&whole[start..start + 3]);
    }
    let mut number = grouped;
    if minor_digits > 0 {
        number.push_str(decimal_separator);
        number.push_str(&fraction);
    }
    let sign = if negative { "-" } else { "" };
    if symbol == currency_code {
        if suffix_symbol {
            Ok(format!("{sign}{number}\u{00a0}{currency_code}"))
        } else {
            Ok(format!("{sign}{currency_code}\u{00a0}{number}"))
        }
    } else if suffix_symbol {
        Ok(format!("{sign}{number}\u{00a0}{symbol}"))
    } else {
        Ok(format!("{sign}{symbol}{number}"))
    }
}

pub(crate) fn quantize_decimal(
    value: &DecimalValue,
    scale: usize,
    rounding: &str,
) -> Result<DecimalValue, EvaluationError> {
    if scale > DEFAULT_INTEGER_DIGITS
        || !matches!(
            rounding,
            "half_even" | "half_up" | "toward_zero" | "away_from_zero" | "floor" | "ceil"
        )
    {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let shift = &value.exponent10 + BigInt::from(scale);
    if shift.sign() != Sign::Minus {
        let power = shift.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if power > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        return DecimalValue::new(
            &value.coefficient * BigInt::from(10u8).pow(power as u32),
            -BigInt::from(scale),
        );
    }
    let power = (-shift)
        .to_usize()
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    if power > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let divisor = BigInt::from(10u8).pow(power as u32);
    let (mut quotient, remainder) = value.coefficient.div_rem(&divisor);
    let negative = value.coefficient.sign() == Sign::Minus;
    if !remainder.is_zero() {
        let comparison = (remainder.abs() * 2u8).cmp(&divisor);
        let increment = match rounding {
            "toward_zero" => false,
            "away_from_zero" => true,
            "floor" => negative,
            "ceil" => !negative,
            "half_up" => comparison != std::cmp::Ordering::Less,
            "half_even" => {
                comparison == std::cmp::Ordering::Greater
                    || (comparison == std::cmp::Ordering::Equal
                        && (&quotient % 2u8).abs() == BigInt::from(1u8))
            }
            _ => return Err(error("ORNA-EVAL-VALUE")),
        };
        if increment {
            if negative {
                quotient -= 1;
            } else {
                quotient += 1;
            }
        }
    }
    DecimalValue::new(quotient, -BigInt::from(scale))
}

pub(crate) fn allocate_minor_units(
    amount: &Value,
    weights: &[BigInt],
    scale: usize,
    rounding: Option<&str>,
) -> Result<Vec<DecimalValue>, EvaluationError> {
    if weights.is_empty() || scale > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let total_weight = weights
        .iter()
        .fold(BigInt::zero(), |total, weight| total + weight);
    if total_weight <= BigInt::zero() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let original = money_amount(amount)?;
    let exact_units = decimal_scaled_integer(&original, scale)?;
    let amount_units = match (exact_units, rounding) {
        (Some(units), _) => units,
        (None, Some(rounding)) => {
            let rounded = quantize_decimal(&original, scale, rounding)?;
            decimal_scaled_integer(&rounded, scale)?.ok_or_else(|| error("ORNA-EVAL-VALUE"))?
        }
        (None, None) => return Err(error("ORNA-EVAL-VALUE")),
    };
    let negative = amount_units.sign() == Sign::Minus;
    let magnitude = amount_units.abs();
    let mut shares = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    let mut assigned = BigInt::zero();
    for (index, weight) in weights.iter().enumerate() {
        let numerator = &magnitude * weight;
        let (share, remainder) = numerator.div_rem(&total_weight);
        assigned += &share;
        shares.push(share);
        remainders.push((index, remainder));
    }
    let leftover = (&magnitude - assigned)
        .to_usize()
        .filter(|count| *count <= weights.len())
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    remainders.sort_by(|(left_index, left), (right_index, right)| {
        right.cmp(left).then(left_index.cmp(right_index))
    });
    for (index, _) in remainders.into_iter().take(leftover) {
        shares[index] += 1;
    }
    shares
        .into_iter()
        .map(|share| {
            let share = if negative { -share } else { share };
            DecimalValue::new(share, -BigInt::from(scale))
        })
        .collect()
}

fn decimal_scaled_integer(
    value: &DecimalValue,
    scale: usize,
) -> Result<Option<BigInt>, EvaluationError> {
    let shift = &value.exponent10 + BigInt::from(scale);
    if shift.sign() != Sign::Minus {
        let power = shift.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if power > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        return Ok(Some(
            &value.coefficient * BigInt::from(10u8).pow(power as u32),
        ));
    }
    let power = (-shift)
        .to_usize()
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    if power > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let divisor = BigInt::from(10u8).pow(power as u32);
    let (units, remainder) = value.coefficient.div_rem(&divisor);
    Ok(remainder.is_zero().then_some(units))
}
