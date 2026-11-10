//! Literal payloads (design section 2.4).
//!
//! A literal lowers to its canonical scalar payload, never to a runtime value:
//! integers are arbitrary-width, decimals are a coefficient and a base-ten
//! exponent, floats are their bit pattern, and calendar literals are days and
//! seconds since the epoch. A spelling without a canonical payload returns
//! `None`, and lowering reports one diagnostic instead of guessing.

use num_bigint::BigInt;
use orna_syntax::LiteralKind;

use crate::Lit;

/// The canonical payload of one literal spelling.
pub(crate) fn lower_literal(text: &str, kind: LiteralKind) -> Option<Lit> {
    match kind {
        LiteralKind::Integer => parse_int(text).map(Lit::Int),
        LiteralKind::Decimal => parse_decimal(text),
        LiteralKind::Float => text
            .parse::<f64>()
            .ok()
            .map(|value| Lit::Float(value.to_bits())),
        LiteralKind::String => orna_syntax::unescape_string_literal(text)
            .ok()
            .map(|value| Lit::Str(value.into_boxed_str())),
        LiteralKind::Boolean => match text {
            "true" => Some(Lit::Bool(true)),
            "false" => Some(Lit::Bool(false)),
            _ => None,
        },
        LiteralKind::Null => Some(Lit::None),
        LiteralKind::Date => parse_date(text).map(Lit::Date),
        LiteralKind::Instant => parse_instant(text).map(|(seconds, nanoseconds)| Lit::Instant {
            seconds,
            nanoseconds,
        }),
    }
}

/// An integer spelling, with `_` separators removed.
fn parse_int(text: &str) -> Option<BigInt> {
    let digits = text.replace('_', "");
    let (negative, digits) = match digits.strip_prefix('-') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, digits),
    };
    let value = BigInt::parse_bytes(digits.as_bytes(), 10)?;
    Some(if negative { -value } else { value })
}

/// A decimal spelling as a coefficient and a base-ten exponent.
fn parse_decimal(text: &str) -> Option<Lit> {
    let cleaned = text.replace('_', "");
    let (negative, unsigned) = match cleaned.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, cleaned.as_str()),
    };
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let exponent = unsigned[index + 1..].parse::<i64>().ok()?;
            (&unsigned[..index], exponent)
        }
        None => (unsigned, 0),
    };
    let (whole, fraction) = match mantissa.find('.') {
        Some(index) => (&mantissa[..index], &mantissa[index + 1..]),
        None => (mantissa, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    let digits = if digits.is_empty() { "0" } else { &digits };
    let coefficient = BigInt::parse_bytes(digits.as_bytes(), 10)?;
    let coefficient = if negative { -coefficient } else { coefficient };
    let exponent = BigInt::from(exponent - i64::try_from(fraction.len()).ok()?);
    Some(Lit::Decimal {
        coefficient,
        exponent,
    })
}

/// An ISO calendar date as days since 1970-01-01.
fn parse_date(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = text[0..4].parse::<i64>().ok()?;
    let month = text[5..7].parse::<u32>().ok()?;
    let day = text[8..10].parse::<u32>().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// An ISO instant as seconds and nanoseconds since the epoch, normalised to UTC.
fn parse_instant(text: &str) -> Option<(i64, u32)> {
    let (date, rest) = text.split_once('T')?;
    let days = parse_date(date)?;
    let (time, offset) = split_offset(rest)?;
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, fraction),
        None => (time, ""),
    };
    let mut parts = clock.split(':');
    let hour = parts.next()?.parse::<i64>().ok()?;
    let minute = parts.next()?.parse::<i64>().ok()?;
    let second = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut nanos = 0_u32;
    let mut scale = 100_000_000_u32;
    for digit in fraction.chars() {
        nanos += digit.to_digit(10)? * scale;
        scale /= 10;
    }
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?
        .checked_sub(offset)?;
    Some((seconds, nanos))
}

/// Splits an instant's time tail into its clock and its UTC offset in seconds.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(clock) = rest.strip_suffix('Z') {
        return Some((clock, 0));
    }
    if rest.len() < 6 {
        return None;
    }
    let split = rest.len() - 6;
    let (clock, offset) = rest.split_at(split);
    let sign = match offset.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if offset.as_bytes()[3] != b':' {
        return None;
    }
    let hours = offset[1..3].parse::<i64>().ok()?;
    let minutes = offset[4..6].parse::<i64>().ok()?;
    Some((clock, sign * (hours * 3_600 + minutes * 60)))
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::{days_from_civil, lower_literal};
    use crate::Lit;
    use orna_syntax::LiteralKind;

    #[test]
    fn integers_and_decimals_keep_exact_payloads() {
        assert_eq!(
            lower_literal("1_000", LiteralKind::Integer),
            Some(Lit::Int(1000.into()))
        );
        assert_eq!(
            lower_literal("1.25", LiteralKind::Decimal),
            Some(Lit::Decimal {
                coefficient: 125.into(),
                exponent: (-2).into()
            })
        );
    }

    #[test]
    fn calendar_dates_and_instants_lower_to_seconds() {
        assert_eq!(
            lower_literal("1970-01-01", LiteralKind::Date),
            Some(Lit::Date(0))
        );
        assert_eq!(days_from_civil(2024, 2, 29), 19782);
        assert_eq!(
            lower_literal("1970-01-01T00:00:01Z", LiteralKind::Instant),
            Some(Lit::Instant {
                seconds: 1,
                nanoseconds: 0
            })
        );
        assert_eq!(
            lower_literal("1970-01-01T01:00:00+01:00", LiteralKind::Instant),
            Some(Lit::Instant {
                seconds: 0,
                nanoseconds: 0
            })
        );
    }
}
