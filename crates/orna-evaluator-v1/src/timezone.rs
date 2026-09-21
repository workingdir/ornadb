//! Deterministic, dependency-free time-zone resolution for the evaluator.
//!
//! The embedded dataset is intentionally small and immutable.  It contains the
//! canonical `UTC` zone and the current European rules for `Europe/London`:
//! standard time is UTC, and daylight time is UTC+01:00 from 01:00 UTC on the
//! last Sunday in March through 01:00 UTC on the last Sunday in October.  The
//! calendar implementation accepts the proleptic Gregorian years 0001..=9999;
//! this is the supported range of this pinned profile.

use std::fmt;

/// Identity of the immutable time-zone rules used by this module.
///
/// This is an implementation-pinned profile, rather than the host's installed
/// IANA database.  Callers must persist this value with data whose result
/// depends on time-zone rules.
pub const TIMEZONE_DATASET_VERSION: &str = "orna-iana-2024a";

const NANOS_PER_SECOND: u32 = 1_000_000_000;
const SECONDS_PER_DAY: i64 = 86_400;
const MIN_YEAR: i32 = 1;
const MAX_YEAR: i32 = 9_999;

/// An absolute point on the UTC timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Instant {
    pub unix_seconds: i64,
    pub nanosecond: u32,
}

impl Instant {
    /// Construct a floor-normalised instant.
    pub fn new(unix_seconds: i64, nanosecond: u32) -> Result<Self, TimeZoneError> {
        if nanosecond >= NANOS_PER_SECOND {
            return Err(TimeZoneError::InvalidInstant);
        }
        Ok(Self {
            unix_seconds,
            nanosecond,
        })
    }
}

/// Civil date and time without a zone or offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub nanosecond: u32,
}

impl LocalDateTime {
    /// Construct a validated proleptic-Gregorian local date and time.
    pub fn new(
        year: i32,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        nanosecond: u32,
    ) -> Result<Self, TimeZoneError> {
        let value = Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanosecond,
        };
        validate_local(value)?;
        Ok(value)
    }
}

/// A local date/time together with its resolved offset and canonical zone name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZonedLocalDateTime {
    pub local: LocalDateTime,
    pub offset_seconds: i32,
    pub zone: String,
}

/// The result of resolving a local civil date/time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalTimeResolution {
    /// Exactly one instant has this local representation.
    Unique {
        instant: Instant,
        offset_seconds: i32,
    },
    /// The clock repeated; `earlier` is the daylight-time occurrence and
    /// `later` is the standard-time occurrence.
    Ambiguous { earlier: Instant, later: Instant },
    /// The clock jumped forward.  `before` and `after` are the instants
    /// immediately bracketing the transition (nanosecond precision).
    Nonexistent { before: Instant, after: Instant },
}

/// Errors produced by validation or deterministic zone lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeZoneError {
    UnknownZone,
    InvalidInstant,
    InvalidLocalDateTime,
    AmbiguousLocalTime,
    NonexistentLocalTime,
    UnsupportedDateRange,
}

impl fmt::Display for TimeZoneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnknownZone => "unknown time zone",
            Self::InvalidInstant => "invalid instant",
            Self::InvalidLocalDateTime => "invalid local date-time",
            Self::AmbiguousLocalTime => "ambiguous local time",
            Self::NonexistentLocalTime => "nonexistent local time",
            Self::UnsupportedDateRange => "time-zone date is outside the supported range",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TimeZoneError {}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ZoneKind {
    Utc,
    London,
}

/// A canonical, immutable time zone from the pinned profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeZone {
    name: String,
    kind: ZoneKind,
}

impl TimeZone {
    /// Return the canonical IANA name of this zone.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Convert an absolute instant into this zone's local representation.
    pub fn at(&self, instant: Instant) -> Result<ZonedLocalDateTime, TimeZoneError> {
        validate_instant(instant)?;
        let (utc_year, _, _, _, _, _) = civil_from_seconds(instant.unix_seconds)?;
        let offset = self.offset_at_utc(instant.unix_seconds, utc_year)?;
        let local_seconds = instant
            .unix_seconds
            .checked_add(i64::from(offset))
            .ok_or(TimeZoneError::InvalidInstant)?;
        let (year, month, day, hour, minute, second) = civil_from_seconds(local_seconds)?;
        let local = LocalDateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanosecond: instant.nanosecond,
        };
        // The conversion above is internal, but retain one validation boundary
        // so malformed public struct literals cannot escape through this API.
        validate_local(local)?;
        Ok(ZonedLocalDateTime {
            local,
            offset_seconds: offset,
            zone: self.name.clone(),
        })
    }

    /// Resolve a local civil date/time without silently selecting an offset.
    pub fn resolve_local(
        &self,
        local: LocalDateTime,
    ) -> Result<LocalTimeResolution, TimeZoneError> {
        validate_local(local)?;
        let local_seconds = local_seconds(local)?;
        match self.kind {
            ZoneKind::Utc => Ok(LocalTimeResolution::Unique {
                instant: Instant::new(local_seconds, local.nanosecond)?,
                offset_seconds: 0,
            }),
            ZoneKind::London => {
                let spring_utc = transition_seconds(local.year, 3, 1)?;
                let fall_utc = transition_seconds(local.year, 10, 1)?;
                let spring_start = spring_utc;
                let spring_end = spring_utc
                    .checked_add(3_600)
                    .ok_or(TimeZoneError::InvalidLocalDateTime)?;
                let fall_start = fall_utc
                    .checked_add(3_600)
                    .ok_or(TimeZoneError::InvalidLocalDateTime)?;
                let fall_end = fall_utc;

                if (spring_start..spring_end).contains(&local_seconds) {
                    return Ok(LocalTimeResolution::Nonexistent {
                        before: instant_before(spring_utc)?,
                        after: Instant::new(spring_utc, 0)?,
                    });
                }
                if (fall_end..fall_start).contains(&local_seconds) {
                    let earlier = Instant::new(
                        local_seconds
                            .checked_sub(3_600)
                            .ok_or(TimeZoneError::InvalidLocalDateTime)?,
                        local.nanosecond,
                    )?;
                    let later = Instant::new(local_seconds, local.nanosecond)?;
                    return Ok(LocalTimeResolution::Ambiguous { earlier, later });
                }

                let offset = if (spring_end..fall_start).contains(&local_seconds) {
                    3_600
                } else {
                    0
                };
                let unix_seconds = local_seconds
                    .checked_sub(i64::from(offset))
                    .ok_or(TimeZoneError::InvalidLocalDateTime)?;
                Ok(LocalTimeResolution::Unique {
                    instant: Instant::new(unix_seconds, local.nanosecond)?,
                    offset_seconds: offset,
                })
            }
        }
    }

    fn offset_at_utc(&self, unix_seconds: i64, utc_year: i32) -> Result<i32, TimeZoneError> {
        match self.kind {
            ZoneKind::Utc => Ok(0),
            ZoneKind::London => {
                let spring = transition_seconds(utc_year, 3, 1)?;
                let fall = transition_seconds(utc_year, 10, 1)?;
                Ok(if (spring..fall).contains(&unix_seconds) {
                    3_600
                } else {
                    0
                })
            }
        }
    }
}

/// Resolve one of the canonical names in the pinned profile.
pub fn resolve_time_zone(name: &str) -> Result<TimeZone, TimeZoneError> {
    match name {
        "UTC" => Ok(TimeZone {
            name: String::from("UTC"),
            kind: ZoneKind::Utc,
        }),
        "Europe/London" => Ok(TimeZone {
            name: String::from("Europe/London"),
            kind: ZoneKind::London,
        }),
        _ => Err(TimeZoneError::UnknownZone),
    }
}

fn validate_instant(instant: Instant) -> Result<(), TimeZoneError> {
    if instant.nanosecond >= NANOS_PER_SECOND {
        return Err(TimeZoneError::InvalidInstant);
    }
    // Instants are representable only where their UTC civil date is in the
    // profile's documented range.  This also keeps offset conversion checked.
    let (year, _, _, _, _, _) = civil_from_seconds(instant.unix_seconds)?;
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return Err(TimeZoneError::UnsupportedDateRange);
    }
    Ok(())
}

fn validate_local(local: LocalDateTime) -> Result<(), TimeZoneError> {
    if !(MIN_YEAR..=MAX_YEAR).contains(&local.year)
        || !(1..=12).contains(&local.month)
        || local.hour >= 24
        || local.minute >= 60
        || local.second >= 60
        || local.nanosecond >= NANOS_PER_SECOND
    {
        return Err(TimeZoneError::InvalidLocalDateTime);
    }
    let days = days_in_month(local.year, local.month);
    if local.day == 0 || local.day > days {
        return Err(TimeZoneError::InvalidLocalDateTime);
    }
    Ok(())
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn local_seconds(local: LocalDateTime) -> Result<i64, TimeZoneError> {
    let days = days_from_civil(local.year, local.month, local.day);
    let day_seconds = i64::from(local.hour) * 3_600
        + i64::from(local.minute) * 60
        + i64::from(local.second);
    days.checked_mul(SECONDS_PER_DAY)
        .and_then(|seconds| seconds.checked_add(day_seconds))
        .ok_or(TimeZoneError::InvalidLocalDateTime)
}

fn transition_seconds(year: i32, month: u8, _day: u8) -> Result<i64, TimeZoneError> {
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return Err(TimeZoneError::UnsupportedDateRange);
    }
    let next_month = if month == 12 { 1 } else { month + 1 };
    let next_year = if month == 12 { year + 1 } else { year };
    let first_next = days_from_civil(next_year, next_month, 1);
    let last_day = first_next - 1;
    // 1970-01-01 was Thursday.  Sunday is weekday zero.
    let weekday = (last_day + 4).rem_euclid(7);
    let last_sunday = last_day - weekday;
    last_sunday
        .checked_mul(SECONDS_PER_DAY)
        .and_then(|seconds| seconds.checked_add(3_600))
        .ok_or(TimeZoneError::UnsupportedDateRange)
}

fn instant_before(seconds: i64) -> Result<Instant, TimeZoneError> {
    if seconds == i64::MIN {
        return Err(TimeZoneError::InvalidInstant);
    }
    Instant::new(seconds - 1, NANOS_PER_SECOND - 1)
}

/// Convert a UTC unix-second count to civil fields.  The returned year is
/// checked against the profile range by callers that expose it publicly.
fn civil_from_seconds(seconds: i64) -> Result<(i32, u8, u8, u8, u8, u8), TimeZoneError> {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let day_seconds = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days)?;
    if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return Err(TimeZoneError::UnsupportedDateRange);
    }
    Ok((
        year,
        month,
        day,
        (day_seconds / 3_600) as u8,
        ((day_seconds % 3_600) / 60) as u8,
        (day_seconds % 60) as u8,
    ))
}

// Howard Hinnant's proleptic-Gregorian civil calendar conversion, expressed
// with Euclidean division so dates before 1970 remain deterministic.
fn civil_from_days(days: i64) -> Result<(i32, u8, u8), TimeZoneError> {
    let z = days
        .checked_add(719_468)
        .ok_or(TimeZoneError::InvalidInstant)?;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_part = (5 * doy + 2) / 153;
    let day = doy - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Ok((year as i32, month as u8, day as u8))

}

fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 {
        year / 400
    } else {
        (year - 399) / 400
    };
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5
        + i64::from(day)
        - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
