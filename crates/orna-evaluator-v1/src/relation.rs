use std::collections::VecDeque;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::ToPrimitive;

use super::{
    timezone::{resolve_time_zone, Instant, LocalDateTime, LocalTimeResolution, TimeZone, TimeZoneError},
    Value,
};

/// The two boundary models admitted by `bucket_by`.
///
/// Elapsed periods are measured on the UTC timeline. Calendar periods are
/// measured in local civil days and therefore require an explicit zone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BucketPeriod {
    Elapsed { seconds: BigInt, nanosecond: u32 },
    CalendarDays { days: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BucketBySpec {
    pub(super) period: BucketPeriod,
    pub(super) zone: Option<String>,
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RelationBucketError {
    InvalidPeriod,
    MissingZone,
    TypeMismatch,
    OutOfOrder,
    BoundaryOverflow,
    AmbiguousBoundary,
    NonexistentBoundary,
    TimeZone(TimeZoneError),
}

/// One flushed bucket. Rows stay in their source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationBucket {
    pub(super) start: Instant,
    pub(super) end: Instant,
    pub(super) values: Vec<Value>,
}

/// Streaming state for a [`RelationStage::BucketBy`] stage.
///
/// Only the current bucket is retained. Consequently a relation must expose
/// its time values in nondecreasing order; retaining a map of every historical
/// bucket would silently turn this operation into whole-source materialisation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationBucketState {
    spec: BucketBySpec,
    zone: Option<TimeZone>,
    current: Option<RelationBucket>,
}

impl RelationBucketState {
    pub(super) fn try_new(spec: BucketBySpec) -> Result<Self, RelationBucketError> {
        match &spec.period {
            BucketPeriod::Elapsed {
                seconds,
                nanosecond,
            } if *nanosecond < 1_000_000_000
                && seconds * BigInt::from(1_000_000_000u32) + BigInt::from(*nanosecond)
                    > BigInt::from(0u8) => {}
            BucketPeriod::CalendarDays { days } if *days > 0 => {}
            _ => return Err(RelationBucketError::InvalidPeriod),
        }
        let zone = match (&spec.period, spec.zone.as_deref()) {
            (BucketPeriod::CalendarDays { .. }, Some(name)) => {
                Some(resolve_time_zone(name).map_err(RelationBucketError::TimeZone)?)
            }
            (BucketPeriod::CalendarDays { .. }, None) => {
                return Err(RelationBucketError::MissingZone)
            }
            (BucketPeriod::Elapsed { .. }, _) => None,
        };
        Ok(Self {
            spec,
            zone,
            current: None,
        })
    }

    pub(super) fn push(
        &mut self,
        value: Value,
    ) -> Result<Option<RelationBucket>, RelationBucketError> {
        let instant = value_instant(&value)?;
        let (start, end) = self.boundaries(instant)?;
        let Some(current) = self.current.as_mut() else {
            self.current = Some(RelationBucket {
                start,
                end,
                values: vec![value],
            });
            return Ok(None);
        };
        if start < current.start {
            return Err(RelationBucketError::OutOfOrder);
        }
        if start == current.start {
            current.values.push(value);
            return Ok(None);
        }
        let flushed = std::mem::replace(
            current,
            RelationBucket {
                start,
                end,
                values: vec![value],
            },
        );
        Ok(Some(flushed))
    }

    pub(super) fn finish(&mut self) -> Option<RelationBucket> {
        self.current.take()
    }


    fn boundaries(&self, instant: Instant) -> Result<(Instant, Instant), RelationBucketError> {
        match &self.spec.period {
            BucketPeriod::Elapsed {
                seconds,
                nanosecond,
            } => elapsed_boundaries(instant, seconds, *nanosecond),
            BucketPeriod::CalendarDays { days } => {
                let zone = self
                    .zone
                    .as_ref()
                    .ok_or(RelationBucketError::MissingZone)?;
                calendar_boundaries(instant, zone, *days)
            }
        }
    }
}

fn value_instant(value: &Value) -> Result<Instant, RelationBucketError> {
    let value = match value {
        Value::Record(fields) => fields
            .get("time")
            .ok_or(RelationBucketError::TypeMismatch)?,
        value => value,
    };
    let Value::Instant {
        unix_seconds,
        nanosecond,
    } = value
    else {
        return Err(RelationBucketError::TypeMismatch);
    };
    Instant::new(*unix_seconds, *nanosecond).map_err(RelationBucketError::TimeZone)
}

fn elapsed_boundaries(
    instant: Instant,
    seconds: &BigInt,
    nanosecond: u32,
) -> Result<(Instant, Instant), RelationBucketError> {
    if nanosecond >= 1_000_000_000 {
        return Err(RelationBucketError::InvalidPeriod);
    }
    let width = seconds * BigInt::from(1_000_000_000u32) + BigInt::from(nanosecond);
    if width <= BigInt::from(0u8) {
        return Err(RelationBucketError::InvalidPeriod);
    }
    let instant_nanos =
        BigInt::from(instant.unix_seconds) * BigInt::from(1_000_000_000u32)
            + BigInt::from(instant.nanosecond);
    let start_nanos = instant_nanos.div_floor(&width) * &width;
    let end_nanos = &start_nanos + width;
    Ok((
        instant_from_nanos(start_nanos)?,
        instant_from_nanos(end_nanos)?,
    ))
}

fn instant_from_nanos(value: BigInt) -> Result<Instant, RelationBucketError> {
    let (seconds, nanosecond) = value.div_rem(&BigInt::from(1_000_000_000u32));
    let (seconds, nanosecond) = if nanosecond.sign() == num_bigint::Sign::Minus {
        (
            seconds - BigInt::from(1u8),
            nanosecond + BigInt::from(1_000_000_000u32),
        )
    } else {
        (seconds, nanosecond)
    };
    Instant::new(
        seconds
            .to_i64()
            .ok_or(RelationBucketError::BoundaryOverflow)?,
        nanosecond
            .to_u32()
            .ok_or(RelationBucketError::BoundaryOverflow)?,
    )
    .map_err(RelationBucketError::TimeZone)
}

fn calendar_boundaries(
    instant: Instant,
    zone: &TimeZone,
    days: u32,
) -> Result<(Instant, Instant), RelationBucketError> {
    let local = zone
        .at(instant)
        .map_err(RelationBucketError::TimeZone)?
        .local;
    let start_local = LocalDateTime::new(local.year, local.month, local.day, 0, 0, 0, 0)
        .map_err(RelationBucketError::TimeZone)?;
    let end_local = add_days(start_local, days)?;
    Ok((
        resolve_boundary(zone, start_local)?,
        resolve_boundary(zone, end_local)?,
    ))
}

fn resolve_boundary(zone: &TimeZone, local: LocalDateTime) -> Result<Instant, RelationBucketError> {
    match zone
        .resolve_local(local)
        .map_err(RelationBucketError::TimeZone)?
    {
        LocalTimeResolution::Unique { instant, .. } => Ok(instant),
        LocalTimeResolution::Ambiguous { .. } => Err(RelationBucketError::AmbiguousBoundary),
        LocalTimeResolution::Nonexistent { .. } => Err(RelationBucketError::NonexistentBoundary),
    }
}

fn add_days(mut local: LocalDateTime, days: u32) -> Result<LocalDateTime, RelationBucketError> {
    for _ in 0..days {
        if local.day < days_in_month(local.year, local.month) {
            local.day += 1;
        } else if local.month < 12 {
            local.month += 1;
            local.day = 1;
        } else if local.year < 9_999 {
            local.year += 1;
            local.month = 1;
            local.day = 1;
        } else {
            return Err(RelationBucketError::BoundaryOverflow);
        }
    }
    LocalDateTime::new(
        local.year,
        local.month,
        local.day,
        local.hour,
        local.minute,
        local.second,
        local.nanosecond,
    )
    .map_err(RelationBucketError::TimeZone)
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationPlan {
    pub(super) source: String,
    pub(super) source_union: Option<(Box<RelationPlan>, Box<RelationPlan>)>,
    pub(super) stages: Vec<RelationStage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RelationStage {
    Filter(Value),
    Map(Value),
    /// Lazily expands each upstream value through a transform returning a
    /// finite `List`, preserving source and inner order.
    FlatMap(Value),
    SortBy(Value),
    BucketBy(BucketBySpec),
    Distinct,
    /// Adjacent overlapping row pairs, preserving upstream order.
    ///
    /// Evaluation keeps the previous post-prefix row across source pages and
    /// stage applications; the stage index is absolute through `stage_offset`
    /// when buffered suffixes are evaluated.
    Pairs,
    /// Complete positional windows over the post-prefix source.
    ///
    /// The evaluator owns one [`RelationWindowState`] per stage so that a
    /// window can span source pages, buffered sort suffixes, and values
    /// produced by an upstream flat map.
    Window(usize, usize),
    Drop(usize),
    Take(usize),
}

/// Incremental state for a [`RelationStage::Window`] stage.
///
/// `pending` contains the values beginning at the next candidate window
/// position. Once a complete window is emitted, values before the next
/// candidate are removed for overlap, or incoming values are skipped for a
/// gap. No finalisation method exists intentionally: an incomplete trailing
/// window is never emitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationWindowState {
    size: usize,
    step: usize,
    pending: VecDeque<Value>,
    skipped: usize,
}

impl RelationWindowState {
    /// Builds state only for the valid positive size and step domain.
    pub(super) fn try_new(size: usize, step: usize) -> Option<Self> {
        (size > 0 && step > 0).then(|| Self {
            size,
            step,
            pending: VecDeque::new(),
            skipped: 0,
        })
    }

    /// Feeds one ordered value and returns a complete window when available.
    ///
    /// The returned list owns its values, allowing the caller to pass it down
    /// the remaining relation stages while this state retains overlap values.
    pub(super) fn push(&mut self, value: Value) -> Option<Vec<Value>> {
        if self.skipped > 0 {
            self.skipped -= 1;
            return None;
        }

        self.pending.push_back(value);
        if self.pending.len() < self.size {
            return None;
        }

        let window = self.pending.iter().take(self.size).cloned().collect();
        if self.step < self.size {
            for _ in 0..self.step {
                self.pending.pop_front();
            }
        } else {
            self.pending.clear();
            self.skipped = self.step - self.size;
        }
        Some(window)
    }
}
/// Incremental state for a relation `last` observation.
///
/// The terminal retains only the most recently emitted value while the
/// relation source is scanned. This keeps unsorted relation traversal lazy and
/// bounded by one retained row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationLastState {
    last: Option<Value>,
}

impl RelationLastState {
    /// Creates an empty terminal state.
    pub(super) fn new() -> Self {
        Self { last: None }
    }
    /// Retains the latest emitted relation value.
    pub(super) fn push(&mut self, value: Value) {
        self.last = Some(value);
    }

    /// Consumes the state and returns the final emitted value, if any.
    pub(super) fn finish(self) -> Option<Value> {
        self.last
    }
}

impl RelationPlan {
    pub(super) fn new(source: String) -> Self {
        Self {
            source,
            source_union: None,
            stages: Vec::new(),
        }
    }

    pub(super) fn union(left: Self, right: Self) -> Self {
        Self {
            source: String::new(),
            source_union: Some((Box::new(left), Box::new(right))),
            stages: Vec::new(),
        }
    }

    pub(super) fn with_stage(mut self, stage: RelationStage) -> Self {
        self.stages.push(stage);
        self
    }
}
