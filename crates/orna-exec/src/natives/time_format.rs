//! The `core.time` duration presentations: the compact, clock, words and ISO spellings of an elapsed span, in English.
//!
//! Ported from the old evaluator's `format_duration*` family. The context record is validated against the same zone
//! edition as local-time lookup, and every rendered string is bounded by the string limit.

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{ToPrimitive, Zero};
use orna_hir::{Name, Ty};

use super::text::bounded;
use super::values::exactly;
use crate::error::Stop;
use crate::machine::Machine;
use crate::timezone::resolve_time_zone;
use crate::value::Value;

/// `duration_compact_format(Duration, context): Str`, such as `2d 3h 4m 5.5s`.
pub(super) fn duration_compact_format(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    format_duration(machine, arguments, DurationFormat::Compact)
}

/// `duration_clock_format(Duration, context): Str`, such as `26:04:05.5`.
pub(super) fn duration_clock_format(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    format_duration(machine, arguments, DurationFormat::Clock)
}

/// `duration_words_format(Duration, context): Str`, such as `2 days, 3 hours, 4 minutes, 5.5 seconds`.
pub(super) fn duration_words_format(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    format_duration(machine, arguments, DurationFormat::Words)
}

/// `duration_iso_format(Duration, context): Str`, such as `P2DT3H4M5.5S`.
pub(super) fn duration_iso_format(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    format_duration(machine, arguments, DurationFormat::Iso)
}

/// A duration presentation spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurationFormat {
    Compact,
    Clock,
    Words,
    Iso,
}

/// Charges one step, validates the context record, and renders the span in `format`.
fn format_duration(
    machine: &mut Machine<'_>,
    arguments: Vec<Value>,
    format: DurationFormat,
) -> Result<Value, Stop> {
    let [duration, context] = exactly::<2>(arguments)?;
    let Value::Duration {
        seconds,
        nanosecond,
    } = duration
    else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    machine.tick(1)?;
    check_context(&context)?;
    let parts = DurationParts::new(&seconds, nanosecond);
    let text = match format {
        DurationFormat::Compact => compact(&parts),
        DurationFormat::Clock => clock(&parts),
        DurationFormat::Words => words(&parts),
        DurationFormat::Iso => iso(&parts),
    };
    bounded(machine, text)
}

/// Validates the context record: the English locales this profile implements, and a known time zone.
fn check_context(context: &Value) -> Result<(), Stop> {
    let Value::Record(fields) = context else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let locale = text_field(fields, "locale")?;
    let zone = text_field(fields, "time_zone")?;
    if !matches!(locale, "en" | "en-US" | "en-GB") {
        return Err(Stop::Error("ORNA-EVAL-VALUE"));
    }
    resolve_time_zone(zone).map_err(|_| Stop::Error("ORNA-EVAL-VALUE"))?;
    Ok(())
}

/// The text field `key` of a record, or `ORNA-EVAL-TYPE` when it is missing or not text.
fn text_field<'a>(fields: &'a [(Name, Value)], key: &str) -> Result<&'a str, Stop> {
    fields
        .iter()
        .find_map(|(name, value)| match value {
            Value::Text(text) if name.as_str() == key => Some(text.as_str()),
            _ => None,
        })
        .ok_or(Stop::Error("ORNA-EVAL-TYPE"))
}

/// The day-to-nanosecond components a duration spelling renders.
///
/// `fractional_seconds` joins the whole seconds to the nanosecond remainder and trims trailing zeros; the renderers
/// reuse it wherever they emit a seconds field with its fraction.
struct DurationParts {
    sign: &'static str,
    days: BigInt,
    hours: u32,
    minutes: u32,
    whole_seconds: u32,
    fraction: u32,
    fractional_seconds: String,
}

impl DurationParts {
    fn new(seconds: &BigInt, nanosecond: u32) -> Self {
        const NANOS_PER_SECOND: u64 = 1_000_000_000;
        const NANOS_PER_MINUTE: u64 = 60 * NANOS_PER_SECOND;
        const NANOS_PER_HOUR: u64 = 60 * NANOS_PER_MINUTE;
        const NANOS_PER_DAY: u64 = 24 * NANOS_PER_HOUR;

        let mut total = seconds * BigInt::from(NANOS_PER_SECOND) + BigInt::from(nanosecond);
        let negative = total.sign() == Sign::Minus;
        if negative {
            total = -total;
        }
        let (days, remainder) = total.div_rem(&BigInt::from(NANOS_PER_DAY));
        let (hours, remainder) = remainder.div_rem(&BigInt::from(NANOS_PER_HOUR));
        let (minutes, remainder) = remainder.div_rem(&BigInt::from(NANOS_PER_MINUTE));
        let (whole_seconds, fraction) = remainder.div_rem(&BigInt::from(NANOS_PER_SECOND));
        let hours = hours.to_u32().expect("hour remainder is below one day");
        let minutes = minutes
            .to_u32()
            .expect("minute remainder is below one hour");
        let whole_seconds = whole_seconds
            .to_u32()
            .expect("second remainder is below one minute");
        let fraction = fraction.to_u32().expect("fraction is below one second");
        let fractional_seconds = if fraction == 0 {
            whole_seconds.to_string()
        } else {
            format!("{whole_seconds}.{fraction:09}")
                .trim_end_matches('0')
                .to_owned()
        };
        Self {
            sign: if negative { "-" } else { "" },
            days,
            hours,
            minutes,
            whole_seconds,
            fraction,
            fractional_seconds,
        }
    }
}

fn compact(parts: &DurationParts) -> String {
    let DurationParts {
        sign,
        days,
        hours,
        minutes,
        whole_seconds,
        fraction,
        fractional_seconds,
    } = parts;
    let mut components = Vec::new();
    if !days.is_zero() {
        components.push(format!("{days}d"));
    }
    if *hours > 0 {
        components.push(format!("{hours}h"));
    }
    if *minutes > 0 {
        components.push(format!("{minutes}m"));
    }
    if *whole_seconds > 0 || *fraction > 0 || components.is_empty() {
        components.push(format!("{fractional_seconds}s"));
    }
    format!("{sign}{}", components.join(" "))
}

fn clock(parts: &DurationParts) -> String {
    let DurationParts {
        sign,
        days,
        hours,
        minutes,
        whole_seconds,
        fraction,
        ..
    } = parts;
    // This is elapsed time, not a wall clock: keep total hours beyond 23, then the caller bounds the output.
    let total_hours = (days * BigInt::from(24u8) + BigInt::from(*hours)).to_str_radix(10);
    let total_hours = if total_hours.len() < 2 {
        format!("{total_hours:0>2}")
    } else {
        total_hours
    };
    // Pad the integral seconds field before its decimal tail; padding the combined fractional text leaves
    // single-digit seconds short.
    let clock_seconds = if *fraction == 0 {
        format!("{whole_seconds:02}")
    } else {
        format!("{whole_seconds:02}.{fraction:09}")
            .trim_end_matches('0')
            .to_owned()
    };
    format!("{sign}{total_hours}:{minutes:02}:{clock_seconds}")
}

fn words(parts: &DurationParts) -> String {
    let DurationParts {
        sign,
        days,
        hours,
        minutes,
        whole_seconds,
        fraction,
        fractional_seconds,
    } = parts;
    let mut components = Vec::new();
    if !days.is_zero() {
        let plural = if days == &BigInt::from(1u8) { "" } else { "s" };
        components.push(format!("{days} day{plural}"));
    }
    if *hours > 0 {
        let plural = if *hours == 1 { "" } else { "s" };
        components.push(format!("{hours} hour{plural}"));
    }
    if *minutes > 0 {
        let plural = if *minutes == 1 { "" } else { "s" };
        components.push(format!("{minutes} minute{plural}"));
    }
    if *whole_seconds > 0 || *fraction > 0 || components.is_empty() {
        let singular = *whole_seconds == 1 && *fraction == 0;
        let plural = if singular { "" } else { "s" };
        components.push(format!("{fractional_seconds} second{plural}"));
    }
    format!("{sign}{}", components.join(", "))
}

fn iso(parts: &DurationParts) -> String {
    let DurationParts {
        sign,
        days,
        hours,
        minutes,
        whole_seconds,
        fraction,
        fractional_seconds,
    } = parts;
    let mut output = String::from(*sign);
    output.push('P');
    if !days.is_zero() {
        output.push_str(&days.to_str_radix(10));
        output.push('D');
    }
    if *hours > 0 || *minutes > 0 || *whole_seconds > 0 || *fraction > 0 || days.is_zero() {
        output.push('T');
        if *hours > 0 {
            output.push_str(&hours.to_string());
            output.push('H');
        }
        if *minutes > 0 {
            output.push_str(&minutes.to_string());
            output.push('M');
        }
        if *whole_seconds > 0 || *fraction > 0 || (days.is_zero() && *hours == 0 && *minutes == 0) {
            output.push_str(fractional_seconds);
            output.push('S');
        }
    }
    output
}
