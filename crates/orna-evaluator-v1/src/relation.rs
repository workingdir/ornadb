use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

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

/// Ordered filter chunks shared when a cascade fans out through a union.
#[derive(Debug)]
pub(super) struct FilterBatch {
    node: FilterBatchNode,
    flattened: OnceLock<Vec<Arc<Vec<Value>>>>,
}

#[derive(Debug)]
enum FilterBatchNode {
    Values(Arc<Vec<Value>>),
    Then(Arc<FilterBatch>, Arc<FilterBatch>),
}

type FilterBatchJoinCache = HashMap<(*const FilterBatch, *const FilterBatch), Arc<FilterBatch>>;

impl FilterBatch {
    fn from_values(values: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            node: FilterBatchNode::Values(Arc::new(values)),
            flattened: OnceLock::new(),
        })
    }

    fn followed_by(previous: &Arc<Self>, next: &Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            node: FilterBatchNode::Then(Arc::clone(previous), Arc::clone(next)),
            flattened: OnceLock::new(),
        })
    }

    fn shared_followed_by(
        previous: &Arc<Self>,
        next: &Arc<Self>,
        joins: &mut FilterBatchJoinCache,
    ) -> Arc<Self> {
        let key = (Arc::as_ptr(previous), Arc::as_ptr(next));
        if let Some(batch) = joins.get(&key) {
            return Arc::clone(batch);
        }
        let batch = Self::followed_by(previous, next);
        joins.insert(key, Arc::clone(&batch));
        batch
    }

    fn prefixed_by(values: Vec<Value>, next: &Arc<Self>) -> Arc<Self> {
        Self::followed_by(&Self::from_values(values), next)
    }

    pub(super) fn chunks(&self) -> &[Arc<Vec<Value>>] {
        self.flattened.get_or_init(|| {
            let mut pending = vec![self];
            let mut chunks = Vec::new();
            while let Some(batch) = pending.pop() {
                match &batch.node {
                    FilterBatchNode::Values(values) => chunks.push(Arc::clone(values)),
                    FilterBatchNode::Then(previous, next) => {
                        pending.push(next);
                        pending.push(previous);
                    }
                }
            }
            chunks
        })
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &Value> {
        self.chunks().iter().flat_map(|chunk| chunk.iter())
    }
}

#[derive(Clone, Debug)]
pub(super) enum RelationStage {
    /// Adjacent filters, evaluated in order and short-circuited per input row.
    Filter(Vec<Value>),
    /// A merged batch whose predicate chunks can be shared across union leaves.
    SharedFilter(Arc<FilterBatch>),
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

impl RelationStage {
    fn filter_values_equal(left: &Self, right: &Self) -> Option<bool> {
        match (left, right) {
            (Self::Filter(left), Self::Filter(right)) => Some(left == right),
            (Self::Filter(left), Self::SharedFilter(right)) => {
                Some(left.iter().eq(right.values()))
            }
            (Self::SharedFilter(left), Self::Filter(right)) => {
                Some(left.values().eq(right.iter()))
            }
            (Self::SharedFilter(left), Self::SharedFilter(right)) => {
                Some(Arc::ptr_eq(left, right) || left.values().eq(right.values()))
            }
            _ => None,
        }
    }
}

impl PartialEq for RelationStage {
    fn eq(&self, other: &Self) -> bool {
        if let Some(equal) = Self::filter_values_equal(self, other) {
            return equal;
        }
        match (self, other) {
            (Self::Map(left), Self::Map(right))
            | (Self::FlatMap(left), Self::FlatMap(right))
            | (Self::SortBy(left), Self::SortBy(right)) => left == right,
            (Self::BucketBy(left), Self::BucketBy(right)) => left == right,
            (Self::Distinct, Self::Distinct) | (Self::Pairs, Self::Pairs) => true,
            (Self::Window(left_size, left_step), Self::Window(right_size, right_step)) => {
                left_size == right_size && left_step == right_step
            }
            (Self::Drop(left), Self::Drop(right)) | (Self::Take(left), Self::Take(right)) => {
                left == right
            }
            _ => false,
        }
    }
}

impl Eq for RelationStage {}

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
        // A nested union is a composition boundary for either operand's
        // pending cascade. Flush each completed operand before making it a
        // child, so its filters compile through its own union storm once.
        let left = left.flush_filter_cascade();
        let right = right.flush_filter_cascade();
        Self {
            source: String::new(),
            source_union: Some((Box::new(left), Box::new(right))),
            stages: Vec::new(),
        }
    }

    pub(super) fn with_stage(mut self, stage: RelationStage) -> Self {
        match stage {
            RelationStage::Drop(0) => {
                // A zero-row drop is an identity. Remove it so a following
                // filter cascade can still reach union leaves, while demand-
                // changing stages such as Take remain pushdown barriers.
                return self;
            }
            RelationStage::Filter(mut predicates) => {
                match self.stages.pop() {
                    Some(RelationStage::Filter(mut previous)) => {
                        previous.append(&mut predicates);
                        self.stages.push(RelationStage::Filter(previous));
                    }
                    Some(RelationStage::SharedFilter(previous)) => {
                        let next = FilterBatch::from_values(predicates);
                        self.stages.push(RelationStage::SharedFilter(
                            FilterBatch::followed_by(&previous, &next),
                        ));
                    }
                    Some(previous) => {
                        self.stages.push(previous);
                        self.stages.push(RelationStage::Filter(predicates));
                    }
                    None => self.stages.push(RelationStage::Filter(predicates)),
                }
            }
            stage => {
                self = self.flush_filter_cascade();
                self.stages.push(stage);
            }
        }
        self
    }

    /// Pushes a pending root filter cascade into union leaves as one batch.
    /// Adjacent filters remain together while a pipeline is assembled; a
    /// demand-changing stage or terminal observer flushes the complete batch.
    pub(super) fn flush_filter_cascade(mut self) -> Self {
        if self.source_union.is_none() || self.stages.len() != 1 {
            return self;
        }
        let predicates = match self.stages.pop().expect("checked filter stage") {
            RelationStage::Filter(predicates) => FilterBatch::from_values(predicates),
            RelationStage::SharedFilter(predicates) => predicates,
            stage => {
                self.stages.push(stage);
                return self;
            }
        };
        let (left, right) = self.source_union.take().expect("checked union source");
        // Keep composed roots shared when the same operand batch fans out to
        // several leaves under this cascade.
        let mut shared_joins = FilterBatchJoinCache::new();
        self.source_union = Some((
            Box::new(left.push_filter_cascade(Arc::clone(&predicates), &mut shared_joins)),
            Box::new(right.push_filter_cascade(predicates, &mut shared_joins)),
        ));
        self
    }

    fn push_filter_cascade(
        mut self,
        predicates: Arc<FilterBatch>,
        shared_joins: &mut FilterBatchJoinCache,
    ) -> Self {
        if let Some((left, right)) = self.source_union.take() {
            if self.stages.is_empty() {
                self.source_union = Some((
                    Box::new(left.push_filter_cascade(
                        Arc::clone(&predicates),
                        shared_joins,
                    )),
                    Box::new(right.push_filter_cascade(predicates, shared_joins)),
                ));
                return self;
            }
            if self.stages.len() == 1
                && matches!(
                    self.stages.first(),
                    Some(RelationStage::Filter(_) | RelationStage::SharedFilter(_))
                )
            {
                let previous = self.stages.pop().expect("checked filter stage");
                let predicates = match previous {
                    RelationStage::Filter(previous) => {
                        FilterBatch::prefixed_by(previous, &predicates)
                    }
                    RelationStage::SharedFilter(previous) => {
                        FilterBatch::shared_followed_by(&previous, &predicates, shared_joins)
                    }
                    _ => unreachable!("checked filter stage"),
                };
                self.source_union = Some((
                    Box::new(left.push_filter_cascade(
                        Arc::clone(&predicates),
                        shared_joins,
                    )),
                    Box::new(right.push_filter_cascade(predicates, shared_joins)),
                ));
                return self;
            }
            self.source_union = Some((left, right));
        }
        match self.stages.pop() {
            Some(RelationStage::Filter(previous)) => {
                self.stages.push(RelationStage::SharedFilter(
                    FilterBatch::prefixed_by(previous, &predicates),
                ));
            }
            Some(RelationStage::SharedFilter(previous)) => {
                self.stages.push(RelationStage::SharedFilter(
                    FilterBatch::shared_followed_by(&previous, &predicates, shared_joins),
                ));
            }
            Some(previous) => {
                self.stages.push(previous);
                self.stages
                    .push(RelationStage::SharedFilter(predicates));
            }
            None => self
                .stages
                .push(RelationStage::SharedFilter(predicates)),
        }
        self
    }
}
