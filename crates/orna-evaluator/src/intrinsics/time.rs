//! Standard-library dispatch for the time family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn time(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            (
                "offset_at",
                [
                    Value::Instant {
                        unix_seconds,
                        nanosecond,
                    },
                    Value::String(zone),
                ],
            ) => {
                self.step()?;
                let zone = resolve_time_zone(zone).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                let instant = Instant::new(*unix_seconds, *nanosecond)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                let offset = zone
                    .at(instant)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                    .offset_seconds;
                Ok(Value::Int(BigInt::from(offset)))
            }
            (
                "resolve_local",
                [
                    Value::String(local),
                    Value::String(zone),
                    Value::String(ambiguous),
                ],
            ) => self.resolve_local_time(local, zone, ambiguous, None),
            (
                "resolve_local",
                [
                    Value::String(local),
                    Value::String(zone),
                    Value::String(ambiguous),
                    gap,
                ],
            ) => {
                let gap = match gap {
                    Value::Null | Value::Option(None) => None,
                    Value::String(gap) => Some(gap.as_str()),
                    Value::Option(Some(inner)) => match inner.as_ref() {
                        Value::String(gap) => Some(gap.as_str()),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    },
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                };
                self.resolve_local_time(local, zone, ambiguous, gap)
            }
            (
                "duration.compact.format"
                | "duration.clock.format"
                | "duration.words.format"
                | "duration.iso.format",
                [
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                ],
            ) => {
                self.step()?;
                format_duration_context(None)?;
                let format = duration_format(name)?;
                self.string(format_duration(format, seconds, *nanosecond))
                    .map(Value::String)
            }
            (
                "duration.compact.format"
                | "duration.clock.format"
                | "duration.words.format"
                | "duration.iso.format",
                [
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                    context,
                ],
            ) => {
                self.step()?;
                format_duration_context(Some(context))?;
                let format = duration_format(name)?;
                self.string(format_duration(format, seconds, *nanosecond))
                    .map(Value::String)
            }
            ("offset_at" | "resolve_local", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn resolve_local_time(
        &mut self,
        local_text: &str,
        zone_name: &str,
        ambiguous: &str,
        gap_adjustment: Option<&str>,
    ) -> Result<Value, EvaluationError> {
        if !matches!(ambiguous, "reject" | "earlier" | "later")
            || !matches!(
                gap_adjustment,
                None | Some("reject" | "shift_forward" | "shift_backward")
            )
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.step()?;
        let local = parse_local_datetime(local_text).ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let zone = resolve_time_zone(zone_name).map_err(|_| error("ORNA-EVAL-VALUE"))?;
        let instant = match zone
            .resolve_local(local)
            .map_err(|_| error("ORNA-EVAL-VALUE"))?
        {
            LocalTimeResolution::Unique { instant, .. } => instant,
            LocalTimeResolution::Ambiguous { earlier, .. } if ambiguous == "earlier" => earlier,
            LocalTimeResolution::Ambiguous { later, .. } if ambiguous == "later" => later,
            LocalTimeResolution::Ambiguous { .. } => return Err(error("ORNA-EVAL-VALUE")),
            LocalTimeResolution::Nonexistent { before, after } => {
                let adjustment = match gap_adjustment {
                    Some("shift_forward") => "shift_forward",
                    Some("shift_backward") => "shift_backward",
                    None | Some("reject") => return Err(error("ORNA-EVAL-VALUE")),
                    _ => return Err(error("ORNA-EVAL-VALUE")),
                };
                let offset_before = zone
                    .at(before)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                    .offset_seconds;
                let offset_after = zone
                    .at(after)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                    .offset_seconds;
                let utc = resolve_time_zone("UTC").map_err(|_| error("ORNA-EVAL-VALUE"))?;
                let LocalTimeResolution::Unique {
                    instant: local_as_utc,
                    ..
                } = utc
                    .resolve_local(local)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                let offset = if adjustment == "shift_forward" {
                    offset_before
                } else {
                    offset_after
                };
                let shifted = Instant::new(
                    local_as_utc
                        .unix_seconds
                        .checked_sub(i64::from(offset))
                        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?,
                    local.nanosecond,
                )
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                // Validate the shifted instant against the selected zone's
                // supported range and transition table before returning it.
                zone.at(shifted).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                shifted
            }
        };
        Ok(Value::Instant {
            unix_seconds: instant.unix_seconds,
            nanosecond: instant.nanosecond,
        })
    }
}
