//! The `core.time` zone natives: the offset of an instant in a zone, and local-time resolution with its ambiguity and
//! gap policies.
//!
//! Ported from the old evaluator's `time` family. Each call charges one step, as the old evaluator does, and reads
//! zones from the immutable edition in `crate::timezone`.

use num_bigint::BigInt;
use orna_hir::Ty;

use super::values::exactly;
use crate::error::Stop;
use crate::machine::Machine;
use crate::timezone::{Instant, LocalDateTime, LocalTimeResolution, TimeZone, resolve_time_zone};
use crate::value::Value;

/// `offset_at(Instant, Str): Int`, the zone's offset from UTC in seconds at `instant`.
pub(super) fn offset_at(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [instant, zone] = exactly::<2>(arguments)?;
    let (
        Value::Instant {
            unix_seconds,
            nanosecond,
        },
        Value::Text(zone),
    ) = (instant, zone)
    else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    machine.tick(1)?;
    let zone = resolve_time_zone(&zone).map_err(|_| invalid())?;
    let instant = Instant::new(unix_seconds, nanosecond).map_err(|_| invalid())?;
    let offset = zone.at(instant).map_err(|_| invalid())?.offset_seconds;
    Ok(Value::Int(BigInt::from(offset)))
}

/// `resolve_local(Str, Str, Str, Str?): Instant`. The ambiguity policy is `reject`, `earlier` or `later`; the gap
/// policy is `reject`, `shift_forward` or `shift_backward`, with `null` meaning `reject`.
pub(super) fn resolve_local(
    machine: &mut Machine<'_>,
    _targs: &[Ty],
    arguments: Vec<Value>,
) -> Result<Value, Stop> {
    let [local, zone, ambiguous, gap] = exactly::<4>(arguments)?;
    let (Value::Text(local), Value::Text(zone), Value::Text(ambiguous)) = (local, zone, ambiguous)
    else {
        return Err(Stop::Error("ORNA-EVAL-TYPE"));
    };
    let gap = optional_text(gap)?;
    resolve_local_time(machine, &local, &zone, &ambiguous, gap.as_deref())
}

/// The text of a flat or boxed optional text value; `None` for an absent value.
fn optional_text(value: Value) -> Result<Option<String>, Stop> {
    match value {
        Value::Null | Value::Option(None) => Ok(None),
        Value::Text(text) => Ok(Some(text)),
        Value::Option(Some(inner)) => match inner.as_ref() {
            Value::Text(text) => Ok(Some(text.clone())),
            _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
        },
        _ => Err(Stop::Error("ORNA-EVAL-TYPE")),
    }
}

fn resolve_local_time(
    machine: &mut Machine<'_>,
    local_text: &str,
    zone_name: &str,
    ambiguous: &str,
    gap: Option<&str>,
) -> Result<Value, Stop> {
    if !matches!(ambiguous, "reject" | "earlier" | "later")
        || !matches!(
            gap,
            None | Some("reject" | "shift_forward" | "shift_backward")
        )
    {
        return Err(invalid());
    }
    machine.tick(1)?;
    let local = parse_local_datetime(local_text).ok_or_else(invalid)?;
    let zone = resolve_time_zone(zone_name).map_err(|_| invalid())?;
    let instant = match zone.resolve_local(local).map_err(|_| invalid())? {
        LocalTimeResolution::Unique { instant, .. } => instant,
        LocalTimeResolution::Ambiguous { earlier, .. } if ambiguous == "earlier" => earlier,
        LocalTimeResolution::Ambiguous { later, .. } if ambiguous == "later" => later,
        LocalTimeResolution::Ambiguous { .. } => return Err(invalid()),
        LocalTimeResolution::Nonexistent { before, after } => {
            shift_across_gap(&zone, local, before, after, gap)?
        }
    };
    Ok(Value::Instant {
        unix_seconds: instant.unix_seconds,
        nanosecond: instant.nanosecond,
    })
}

/// Resolves a local time that falls in a gap by shifting it across the transition with the offset the policy names.
fn shift_across_gap(
    zone: &TimeZone,
    local: LocalDateTime,
    before: Instant,
    after: Instant,
    gap: Option<&str>,
) -> Result<Instant, Stop> {
    let forward = match gap {
        Some("shift_forward") => true,
        Some("shift_backward") => false,
        _ => return Err(invalid()),
    };
    let offset_before = zone.at(before).map_err(|_| invalid())?.offset_seconds;
    let offset_after = zone.at(after).map_err(|_| invalid())?.offset_seconds;
    let utc = resolve_time_zone("UTC").map_err(|_| invalid())?;
    let LocalTimeResolution::Unique {
        instant: local_as_utc,
        ..
    } = utc.resolve_local(local).map_err(|_| invalid())?
    else {
        return Err(invalid());
    };
    let offset = if forward { offset_before } else { offset_after };
    let shifted = Instant::new(
        local_as_utc
            .unix_seconds
            .checked_sub(i64::from(offset))
            .ok_or_else(invalid)?,
        local.nanosecond,
    )
    .map_err(|_| invalid())?;
    // Validate the shifted instant against the selected zone's supported range and transition table before returning it.
    zone.at(shifted).map_err(|_| invalid())?;
    Ok(shifted)
}

/// Parses the narrow wire spelling of a local civil time: ISO `YYYY-MM-DDTHH:MM:SS` with an optional decimal fraction
/// of at most nine digits. It carries no offset and no host locale.
fn parse_local_datetime(text: &str) -> Option<LocalDateTime> {
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 9
                || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            (whole, Some(fraction))
        }
        None => (text, None),
    };
    if whole.len() != 19
        || whole.as_bytes()[4] != b'-'
        || whole.as_bytes()[7] != b'-'
        || whole.as_bytes()[10] != b'T'
        || whole.as_bytes()[13] != b':'
        || whole.as_bytes()[16] != b':'
        || !whole
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    {
        return None;
    }
    let parse = |range: std::ops::Range<usize>| whole[range].parse::<u32>().ok();
    let year = i32::try_from(parse(0..4)?).ok()?;
    let month = u8::try_from(parse(5..7)?).ok()?;
    let day = u8::try_from(parse(8..10)?).ok()?;
    let hour = u8::try_from(parse(11..13)?).ok()?;
    let minute = u8::try_from(parse(14..16)?).ok()?;
    let second = u8::try_from(parse(17..19)?).ok()?;
    let nanosecond = fraction.map_or(Some(0), |fraction| {
        format!("{fraction:0<9}").parse::<u32>().ok()
    })?;
    LocalDateTime::new(year, month, day, hour, minute, second, nanosecond).ok()
}

/// The error of a time value the zone edition or the local-time rules refuse.
fn invalid() -> Stop {
    Stop::Error("ORNA-EVAL-VALUE")
}
