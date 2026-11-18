use crate::*;

pub(crate) fn format_duration_context(context: Option<&Value>) -> Result<(), EvaluationError> {
    let (locale, zone) = match context {
        None => ("en", "UTC"),
        Some(Value::Record(fields)) => {
            let Some(Value::String(locale)) = fields.get("locale") else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let Some(Value::String(zone)) = fields.get("time_zone") else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            (locale.as_str(), zone.as_str())
        }
        Some(_) => return Err(error("ORNA-EVAL-TYPE")),
    };
    // This first pinned profile implements English labels and validates the
    // context zone against the same immutable edition as local-time lookup.
    if !matches!(locale, "en" | "en-US" | "en-GB") {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    resolve_time_zone(zone).map_err(|_| error("ORNA-EVAL-VALUE"))?;
    Ok(())
}

pub(crate) fn elapsed_total_nanoseconds(seconds: &BigInt, nanosecond: u32) -> BigInt {
    seconds * BigInt::from(1_000_000_000u32) + BigInt::from(nanosecond)
}

/// A duration presentation spelling. The standard library admits exactly
/// these four formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DurationFormat {
    Compact,
    Clock,
    Words,
    Iso,
}

impl DurationFormat {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "duration.compact.format" => Self::Compact,
            "duration.clock.format" => Self::Clock,
            "duration.words.format" => Self::Words,
            "duration.iso.format" => Self::Iso,
            _ => return None,
        })
    }
}

/// The admitted presentation spelling `name`.
pub(crate) fn duration_format(name: &str) -> Result<DurationFormat, EvaluationError> {
    DurationFormat::from_name(name).ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))
}

/// The day-to-nanosecond components one duration spelling renders.
///
/// `fractional_seconds` joins the whole seconds to the nanosecond remainder and
/// trims trailing zeros; the renderers reuse it wherever they emit a seconds
/// field with its fraction.
struct DurationParts {
    sign: &'static str,
    days: BigInt,
    hours: u32,
    minutes: u32,
    whole_seconds: u32,
    fraction: u32,
    fractional_seconds: String,
}

fn duration_parts(seconds: &BigInt, nanosecond: u32) -> DurationParts {
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
    DurationParts {
        sign: if negative { "-" } else { "" },
        days,
        hours,
        minutes,
        whole_seconds,
        fraction,
        fractional_seconds,
    }
}

pub(crate) fn format_duration(format: DurationFormat, seconds: &BigInt, nanosecond: u32) -> String {
    let parts = duration_parts(seconds, nanosecond);
    match format {
        DurationFormat::Compact => format_duration_compact(&parts),
        DurationFormat::Clock => format_duration_clock(&parts),
        DurationFormat::Words => format_duration_words(&parts),
        DurationFormat::Iso => format_duration_iso(&parts),
    }
}

fn format_duration_compact(parts: &DurationParts) -> String {
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

fn format_duration_clock(parts: &DurationParts) -> String {
    let DurationParts {
        sign,
        days,
        hours,
        minutes,
        whole_seconds,
        fraction,
        ..
    } = parts;
    // This is elapsed time, not a wall clock: retain total hours beyond 23,
    // then enforce the output byte bound at the caller.
    let total_hours = days * BigInt::from(24u8) + BigInt::from(*hours);
    let total_hours = total_hours.to_str_radix(10);
    let total_hours = if total_hours.len() < 2 {
        format!("{total_hours:0>2}")
    } else {
        total_hours
    };
    // Pad the integral seconds field before its decimal tail; padding the
    // combined fractional text leaves single-digit seconds short.
    let clock_seconds = if *fraction == 0 {
        format!("{whole_seconds:02}")
    } else {
        format!("{whole_seconds:02}.{fraction:09}")
            .trim_end_matches('0')
            .to_owned()
    };
    format!("{sign}{total_hours}:{minutes:02}:{clock_seconds}")
}

fn format_duration_words(parts: &DurationParts) -> String {
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
        components.push(format!(
            "{days} day{}",
            if days == &BigInt::from(1u8) { "" } else { "s" }
        ));
    }
    if *hours > 0 {
        components.push(format!(
            "{hours} hour{}",
            if *hours == 1 { "" } else { "s" }
        ));
    }
    if *minutes > 0 {
        components.push(format!(
            "{minutes} minute{}",
            if *minutes == 1 { "" } else { "s" }
        ));
    }
    if *whole_seconds > 0 || *fraction > 0 || components.is_empty() {
        let singular = *whole_seconds == 1 && *fraction == 0;
        components.push(format!(
            "{fractional_seconds} second{}",
            if singular { "" } else { "s" }
        ));
    }
    format!("{sign}{}", components.join(", "))
}

fn format_duration_iso(parts: &DurationParts) -> String {
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
