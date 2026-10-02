//! Bounded, side-effect-free three-way branch merge planning.
//!
//! Manifests and segment ranges are verified by the repository adapter. The
//! planner compares their digests before it asks the row source to decode a
//! segment, reuses unchanged physical segments, and only decodes ranges whose
//! three versions differ. No plan is returned on a conflict or budget stop,
//! so callers cannot partially apply this operation to the CWD.

use orna_evolution_v1::{
    CanonicalValue, CheckpointGeneration, CheckpointMergeConflict, KeyedRow, ObjectId, RowMergeConflict,
    RowMergeOperation, RowSnapshotMergeError, RowSnapshotState, Schema, SchemaMergeConflict,
    merge_checkpoint_generation, merge_keyed_row_states, merge_schema_bounded,
};
use orna_foundation_v1::compare_primary_keys;
use std::{
    cell::Cell,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

pub type CheckpointId = Vec<u8>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MergeSide {
    Base,
    Left,
    Right,
}

/// A half-open range whose bounds are canonical encodings of primary keys.
/// Boundary comparisons use the same logical order as compact row storage.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyRange {
    pub start: Option<Vec<u8>>,
    pub end: Option<Vec<u8>>,
}

impl KeyRange {
    pub fn all() -> Self {
        Self { start: None, end: None }
    }

    pub fn new(start: Option<Vec<u8>>, end: Option<Vec<u8>>) -> Option<Self> {
        for bound in [&start, &end].into_iter().flatten() {
            let value = CanonicalValue::decode(bound).ok()?;
            compare_primary_keys(&value, &value).ok()?;
        }
        if let (Some(start), Some(end)) = (&start, &end) {
            if compare_encoded_primary_keys(start, end)? != Ordering::Less {
                return None;
            }
        }
        Some(Self { start, end })
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        let Ok(key_value) = CanonicalValue::decode(key) else {
            return false;
        };
        if compare_primary_keys(&key_value, &key_value).is_err() {
            return false;
        }
        self.start
            .as_deref()
            .is_none_or(|start| {
                compare_key_to_encoded_primary_key(&key_value, start)
                    .is_some_and(|order| order != Ordering::Less)
            })
            && self
                .end
                .as_deref()
                .is_none_or(|end| compare_key_to_encoded_primary_key(&key_value, end) == Some(Ordering::Less))
    }
}

fn compare_encoded_primary_keys(left: &[u8], right: &[u8]) -> Option<Ordering> {
    let left = CanonicalValue::decode(left).ok()?;
    compare_key_to_encoded_primary_key(&left, right)
}

fn compare_key_to_encoded_primary_key(key: &CanonicalValue, encoded: &[u8]) -> Option<Ordering> {
    let boundary = CanonicalValue::decode(encoded).ok()?;
    compare_primary_keys(key, &boundary).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowSegmentManifest {
    /// Opaque immutable segment locator understood by the adapter.
    pub locator: Vec<u8>,
    pub range: KeyRange,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableManifest {
    pub digest: [u8; 32],
    pub segments: Vec<RowSegmentManifest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreeWaySnapshot {
    pub schema: Schema,
    pub tables: BTreeMap<ObjectId, TableManifest>,
    /// Keys are stable stream identities; generations and positions remain
    /// opaque values owned by the consumer implementation.
    pub checkpoints: BTreeMap<CheckpointId, CheckpointGeneration>,
}

/// The decoder is called only for a range whose three manifest digests differ
/// (or when segment layouts cannot be aligned). Implementations should decode
/// incrementally and stop immediately when the visitor returns `false`.
///
/// A successful visit must be complete for the requested snapshot and range:
/// an omitted key means the row is known absent in that snapshot. Missing,
/// pruned, or unhydrated segment data must return an error instead, because
/// treating unavailable history as an empty range would manufacture deletes.
/// A failed visit aborts the plan; after the adapter recovers the data, a
/// retry starts from the manifests and canonical key order again. No partial
/// tombstones or conflicts survive the failed attempt, so concurrent retries
/// over the same recovered snapshots have the same result.
pub trait BranchRowSource {
    fn visit_rows(
        &mut self,
        side: MergeSide,
        table: ObjectId,
        segment: Option<&RowSegmentManifest>,
        range: &KeyRange,
        visitor: &mut dyn FnMut(KeyedRow) -> bool,
    ) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BranchMergeBudget {
    /// Per-invocation cap; concurrent merge plans do not share row counters.
    pub max_rows_examined: usize,
    /// Per-invocation cap on materialized conflict details; clean tombstones
    /// do not count against it, and concurrent plans do not share the count.
    pub max_conflicts: usize,
}

impl Default for BranchMergeBudget {
    fn default() -> Self {
        Self { max_rows_examined: 1_000_000, max_conflicts: 1_000 }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergeConflict {
    Schema(SchemaMergeConflict),
    Table { table: ObjectId, reason: &'static str },
    Row { range: KeyRange, conflict: RowMergeConflict },
    /// Projects to the user-facing `sys.CheckpointConflict` diagnostic.
    CheckpointConflict { id: CheckpointId, conflict: CheckpointMergeConflict },
}

impl BranchMergeConflict {
    pub const fn diagnostic_code(&self) -> Option<&'static str> {
        match self {
            Self::CheckpointConflict { .. } => Some("sys.CheckpointConflict"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BranchMergeReport {
    pub rows_examined: usize,
    /// Exact while under budget; at least this many when the budget stops the
    /// merge. Conflict details are separately capped by `max_conflicts`.
    pub conflicts_lower_bound: usize,
    pub affected_tables: BTreeSet<ObjectId>,
    pub affected_ranges: BTreeSet<(ObjectId, KeyRange)>,
    pub affected_checkpoints: BTreeSet<CheckpointId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergeError {
    Conflicts { conflicts: Vec<BranchMergeConflict>, report: BranchMergeReport },
    BudgetExceeded { report: BranchMergeReport },
    InvalidManifest { table: ObjectId },
    RowRead { message: String },
    InvalidRow { table: ObjectId },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergedSegment {
    /// Reuse an immutable segment without decompressing or comparing rows.
    Reuse { from: MergeSide, manifest: RowSegmentManifest },
    /// Materialized logical rows for a range where all three sides changed.
    /// `tombstones` are canonical keys present in the common base but absent
    /// from the merged live rows. They are this plan's versioned deletion
    /// delta, not the accumulated tombstone history, and do not authorize
    /// removing older Git snapshots. Both rows and tombstones are ordered by
    /// canonical primary key. A key that spells a path prefix is still only
    /// that one key; deleting it does not implicitly tombstone deeper keys.
    Rows {
        range: KeyRange,
        rows: Vec<KeyedRow>,
        tombstones: Vec<CanonicalValue>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergedTable {
    pub id: ObjectId,
    pub whole_table_reuse: Option<(MergeSide, TableManifest)>,
    /// Materialized ranges retain canonical primary-key order. Collecting
    /// tombstones by segment order therefore preserves table-wide key order
    /// even when a depth-shaped key boundary splits the ranges.
    pub segments: Vec<MergedSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePlan {
    pub schema: Schema,
    pub tables: BTreeMap<ObjectId, MergedTable>,
    pub checkpoints: BTreeMap<CheckpointId, CheckpointGeneration>,
    pub report: BranchMergeReport,
}

impl BranchMergePlan {
    /// Flattens this plan's row tombstones in table and canonical primary-key
    /// order, independent of its split layout. Concatenate results from
    /// [`BranchMergePlanSequencer`] in the released order to retain commit
    /// lineage across plans whose depth splits differ.
    pub fn ordered_row_tombstones(&self) -> Vec<(ObjectId, CanonicalValue)> {
        let mut ordered = Vec::new();
        for (table, merged) in &self.tables {
            for segment in &merged.segments {
                if let MergedSegment::Rows { tombstones, .. } = segment {
                    ordered.extend(tombstones.iter().cloned().map(|key| (*table, key)));
                }
            }
        }
        ordered.sort_by(|(left_table, left_key), (right_table, right_key)| {
            left_table.cmp(right_table).then_with(|| {
                compare_primary_keys(left_key, right_key).unwrap_or(Ordering::Equal)
            })
        });
        ordered
    }
}

/// One complete paired plan released at its selected commit position, with
/// that plan's exact row tombstones flattened in table and canonical key
/// order, independent of the plan's split layout.
///
/// Adapters should persist `plan` as one paired step and append
/// `ordered_row_tombstones` at `order`. This keeps each plan's depth order
/// attached to its lineage position when concurrent waves finish out of
/// order or use different split boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequencedBranchMergePlan {
    pub order: u64,
    pub plan: BranchMergePlan,
    pub ordered_row_tombstones: Vec<(ObjectId, CanonicalValue)>,
}

/// One exact-key tombstone event retained in committed paired history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTombstoneEvent {
    pub order: u64,
    pub table: ObjectId,
    pub key: CanonicalValue,
}

/// Error returned when a sequenced plan cannot extend a tombstone history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BranchMergeTombstoneHistoryError {
    /// The supplied step is not the next lineage position.
    OutOfOrder { expected: u64, actual: u64 },
    /// This position is already buffered or has already been released.
    DuplicateOrStale { order: u64 },
    /// A depth fragment index is outside its declared fragment count.
    InvalidFragment { fragment: usize, fragment_count: usize },
    /// A depth fragment repeats a fragment already buffered for this position.
    DuplicateFragment { order: u64, fragment: usize },
    /// Fragments for one position disagree about how many pieces it contains.
    FragmentCountMismatch { order: u64, expected: usize, actual: usize },
    /// Overlapping fragments attempted to record one table/key twice in a wave.
    DuplicateTombstone { order: u64 },
    /// Two concurrently buffered lineage positions record one table/key.
    ConcurrentDuplicateTombstone { first_order: u64, second_order: u64 },
    /// A whole paired plan and split depth fragments were both submitted for one position.
    ConflictingSubmission { order: u64 },
    /// The history already consumed the final representable lineage position.
    OrderExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BufferedBranchMergeTombstoneDelta {
    WholePlan(Vec<(ObjectId, CanonicalValue)>),
    DepthFragments {
        fragment_count: usize,
        fragments: BTreeMap<usize, Vec<(ObjectId, CanonicalValue)>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BranchMergeTombstoneSubmissionMode {
    WholePlan,
    DepthFragments,
}

/// Append-only tombstone history for committed paired merge plans.
///
/// MERGE-1 is silent on tombstone accumulation across committed waves,
/// overlapping depth fragments, and cross-mode retries after release. This v1
/// policy retains the accepted mode for each released position so cross-mode
/// conflicts stay distinguishable from same-mode stale retries. It accepts
/// paired plans at exact lineage positions and appends table/key-ordered
/// deletion events in lineage order, advancing through restore-only empty deltas.
/// Duplicate table/key events within one lineage position are rejected as
/// soon as the overlapping fragment arrives. Two concurrently buffered
/// positions cannot record the same logical key when every intervening
/// position is buffered and also records that key. A duplicate reports both
/// positions and leaves the attempted submission unchanged. A missing
/// position delays that decision; an intervening position that omits the key
/// separates a later re-delete. Once an earlier position has been released,
/// a later position may record the key again as a separate event.
/// Submission positions are classified centrally. Exhaustion takes priority;
/// otherwise an already accepted position keeps its mode classification after
/// release, so same-mode retries are stale and cross-mode retries conflict.
/// Unrecorded stale order precedes pending-mode checks, and mixed-mode
/// conflicts precede fragment-index or tombstone-content validation.
/// Concurrent completions may arrive out of order; future deltas wait until
/// every earlier paired position is present. Split waves wait until every
/// fragment arrives, then flatten in canonical table/key order atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTombstoneHistory {
    next_order: Option<u64>,
    events: Vec<BranchMergeTombstoneEvent>,
    pending_deltas: BTreeMap<u64, BufferedBranchMergeTombstoneDelta>,
    committed_modes: BTreeMap<u64, BranchMergeTombstoneSubmissionMode>,
}

impl BranchMergeTombstoneHistory {
    /// Starts history after the caller's already-committed prefix.
    pub fn new(first_order: u64) -> Self {
        Self {
            next_order: Some(first_order),
            events: Vec::new(),
            pending_deltas: BTreeMap::new(),
            committed_modes: BTreeMap::new(),
        }
    }

    /// Appends one plan emitted by [`BranchMergePlanSequencer`] at its next
    /// lineage position. Empty deltas still consume their paired plan order.
    pub fn append(
        &mut self,
        step: &SequencedBranchMergePlan,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let Some(expected) = self.next_order else {
            return Err(BranchMergeTombstoneHistoryError::OrderExhausted);
        };
        if step.order != expected {
            return Err(BranchMergeTombstoneHistoryError::OutOfOrder {
                expected,
                actual: step.order,
            });
        }

        self.submit(step).map(|_| ())
    }

    /// Submits a completed paired plan, buffering future lineage positions
    /// until the missing prefix arrives. Returns only the new contiguous
    /// tombstone events released by this submission. Repeated or stale
    /// positions are rejected without changing buffered or committed history.
    pub fn submit(
        &mut self,
        step: &SequencedBranchMergePlan,
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        self.classify_submission_position(
            step.order,
            BranchMergeTombstoneSubmissionMode::WholePlan,
        )?;
        if has_duplicate_tombstones_in_wave(&step.ordered_row_tombstones) {
            return Err(BranchMergeTombstoneHistoryError::DuplicateTombstone {
                order: step.order,
            });
        }
        if let Some(other_order) =
            self.pending_duplicate_order(step.order, &step.ordered_row_tombstones)
        {
            return Err(concurrent_duplicate_error(step.order, other_order));
        }

        self.pending_deltas.insert(
            step.order,
            BufferedBranchMergeTombstoneDelta::WholePlan(step.ordered_row_tombstones.clone()),
        );
        Ok(self.release_contiguous())
    }

    /// Submits one depth-local piece of a paired merge. A lineage position is
    /// released only after all `fragment_count` pieces arrive; their tombstones
    /// are then normalized by table and canonical key, regardless of fragment
    /// completion order. An empty fragment still counts toward completeness.
    pub fn submit_depth_merge_fragment(
        &mut self,
        order: u64,
        fragment: usize,
        fragment_count: usize,
        tombstones: &[(ObjectId, CanonicalValue)],
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        self.classify_submission_position(
            order,
            BranchMergeTombstoneSubmissionMode::DepthFragments,
        )?;
        if fragment_count == 0 || fragment >= fragment_count {
            return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                fragment,
                fragment_count,
            });
        }

        match self.pending_deltas.get(&order) {
            Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)) => unreachable!(
                "whole-plan mode conflicts are classified before fragment validation"
            ),
            Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count: expected_count,
                fragments,
            }) => {
                if *expected_count != fragment_count {
                    return Err(BranchMergeTombstoneHistoryError::FragmentCountMismatch {
                        order,
                        expected: *expected_count,
                        actual: fragment_count,
                    });
                }
                if fragments.contains_key(&fragment) {
                    return Err(BranchMergeTombstoneHistoryError::DuplicateFragment {
                        order,
                        fragment,
                    });
                }
            }
            None => {}
        }

        let mut combined = match self.pending_deltas.get(&order) {
            Some(BufferedBranchMergeTombstoneDelta::DepthFragments { fragments, .. }) => {
                fragments.values().flatten().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        combined.extend_from_slice(tombstones);
        if has_duplicate_tombstones_in_wave(&combined) {
            return Err(BranchMergeTombstoneHistoryError::DuplicateTombstone { order });
        }
        if let Some(other_order) = self.pending_duplicate_order(order, tombstones) {
            return Err(concurrent_duplicate_error(order, other_order));
        }

        let buffered = self.pending_deltas.entry(order).or_insert_with(|| {
            BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count,
                fragments: BTreeMap::new(),
            }
        });
        let BufferedBranchMergeTombstoneDelta::DepthFragments { fragments, .. } = buffered else {
            unreachable!("whole-plan submissions are rejected before inserting a fragment")
        };
        fragments.insert(fragment, tombstones.to_vec());
        Ok(self.release_contiguous())
    }

    fn release_contiguous(&mut self) -> Vec<BranchMergeTombstoneEvent> {
        let first_new_event = self.events.len();
        while let Some(order) = self.next_order {
            let ready = match self.pending_deltas.get(&order) {
                Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)) => true,
                Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                    fragment_count,
                    fragments,
                }) => fragments.len() == *fragment_count,
                None => false,
            };
            if !ready {
                break;
            }
            let delta = self.pending_deltas.remove(&order).expect("ready delta is buffered");
            let (mut tombstones, mode) = match delta {
                BufferedBranchMergeTombstoneDelta::WholePlan(tombstones) => (
                    tombstones,
                    BranchMergeTombstoneSubmissionMode::WholePlan,
                ),
                BufferedBranchMergeTombstoneDelta::DepthFragments { fragments, .. } => {
                    (
                        fragments.into_values().flatten().collect(),
                        BranchMergeTombstoneSubmissionMode::DepthFragments,
                    )
                }
            };
            self.committed_modes.insert(order, mode);
            tombstones.sort_by(|(left_table, left_key), (right_table, right_key)| {
                left_table.cmp(right_table).then_with(|| {
                    compare_primary_keys(left_key, right_key).unwrap_or(Ordering::Equal)
                })
            });
            self.events.extend(tombstones.into_iter().map(|(table, key)| {
                BranchMergeTombstoneEvent { order, table, key }
            }));
            self.next_order = order.checked_add(1);
        }

        self.events[first_new_event..].to_vec()
    }

    fn classify_submission_position(
        &self,
        order: u64,
        incoming_mode: BranchMergeTombstoneSubmissionMode,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let Some(expected) = self.next_order else {
            return Err(BranchMergeTombstoneHistoryError::OrderExhausted);
        };
        if let Some(committed_mode) = self.committed_modes.get(&order) {
            return if *committed_mode == incoming_mode {
                Err(BranchMergeTombstoneHistoryError::DuplicateOrStale { order })
            } else {
                Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order })
            };
        }
        if order < expected {
            return Err(BranchMergeTombstoneHistoryError::DuplicateOrStale { order });
        }

        match (self.pending_deltas.get(&order), incoming_mode) {
            (
                Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)),
                BranchMergeTombstoneSubmissionMode::WholePlan,
            ) => Err(BranchMergeTombstoneHistoryError::DuplicateOrStale { order }),
            (
                Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)),
                BranchMergeTombstoneSubmissionMode::DepthFragments,
            )
            | (
                Some(BufferedBranchMergeTombstoneDelta::DepthFragments { .. }),
                BranchMergeTombstoneSubmissionMode::WholePlan,
            ) => Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order }),
            (
                Some(BufferedBranchMergeTombstoneDelta::DepthFragments { .. }),
                BranchMergeTombstoneSubmissionMode::DepthFragments,
            )
            | (None, _) => Ok(()),
        }
    }

    fn pending_duplicate_order(
        &self,
        order: u64,
        tombstones: &[(ObjectId, CanonicalValue)],
    ) -> Option<u64> {
        self.pending_deltas.iter().find_map(|(pending_order, delta)| {
            if *pending_order == order {
                return None;
            }
            let first_order = order.min(*pending_order);
            let last_order = order.max(*pending_order);
            tombstones
                .iter()
                .any(|(table, key)| {
                    delta.contains_tombstone(*table, key)
                        && {
                            let between = self
                                .pending_deltas
                                .range((first_order + 1)..last_order)
                                .collect::<Vec<_>>();
                            let expected_between = last_order - first_order - 1;
                            u64::try_from(between.len()).ok() == Some(expected_between)
                                && between.iter().all(|(_, middle_delta)| {
                                    middle_delta.contains_tombstone(*table, key)
                                })
                        }
                })
                .then_some(*pending_order)
        })
    }

    /// Returns the next lineage position required by this history.
    pub fn next_order(&self) -> Option<u64> {
        self.next_order
    }

    /// Returns exact deletion events in their append order.
    pub fn events(&self) -> &[BranchMergeTombstoneEvent] {
        &self.events
    }
}

impl BufferedBranchMergeTombstoneDelta {
    fn contains_tombstone(&self, table: ObjectId, key: &CanonicalValue) -> bool {
        let contains = |candidate_table: &ObjectId, candidate_key: &CanonicalValue| {
            *candidate_table == table && same_primary_key(candidate_key, key)
        };
        match self {
            Self::WholePlan(tombstones) => {
                tombstones.iter().any(|(candidate_table, candidate_key)| {
                    contains(candidate_table, candidate_key)
                })
            }
            Self::DepthFragments { fragments, .. } => fragments
                .values()
                .flatten()
                .any(|(candidate_table, candidate_key)| contains(candidate_table, candidate_key)),
        }
    }
}

fn concurrent_duplicate_error(
    order: u64,
    other_order: u64,
) -> BranchMergeTombstoneHistoryError {
    BranchMergeTombstoneHistoryError::ConcurrentDuplicateTombstone {
        first_order: order.min(other_order),
        second_order: order.max(other_order),
    }
}

fn same_primary_key(left: &CanonicalValue, right: &CanonicalValue) -> bool {
    left == right || matches!(compare_primary_keys(left, right), Ok(Ordering::Equal))
}

fn has_duplicate_tombstones_in_wave(tombstones: &[(ObjectId, CanonicalValue)]) -> bool {
    let mut ordered = tombstones.to_vec();
    ordered.sort_by(|(left_table, left_key), (right_table, right_key)| {
        left_table.cmp(right_table).then_with(|| {
            compare_primary_keys(left_key, right_key).unwrap_or(Ordering::Equal)
        })
    });
    ordered
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0 && same_primary_key(&pair[0].1, &pair[1].1))
}

/// Buffers selected successful plans and releases them in paired lineage order,
/// independent of the order in which concurrent workers finish.
///
/// Callers assign each commit position before dispatching its merge work and
/// submit only the selected successful plan for that position. A failed or
/// conflicted attempt does not consume a position; its retry reuses that
/// position. A later plan may finish first, but it is held until every earlier
/// position is submitted. Each returned plan remains one atomic paired step,
/// so adapters can persist its tables together and append its table-local
/// deltas without replaying or re-sorting earlier history. Use
/// [`Self::submit_with_tombstone_deltas`] when consumers need each plan's
/// lineage position and depth-ordered row delta attached to the same paired
/// step. The sequencer does not perform durable commits; adapters must enact
/// returned plans in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePlanSequencer {
    next_order: Option<u64>,
    pending: BTreeMap<u64, BranchMergePlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BranchMergePlanSequenceError {
    /// The order has already been submitted or released.
    DuplicateOrStale { order: u64 },
    /// All representable commit positions have been released.
    OrderExhausted,
}

impl BranchMergePlanSequencer {
    /// Starts a sequence at the next commit position following the caller's
    /// already-persisted prefix.
    pub fn new(first_order: u64) -> Self {
        Self { next_order: Some(first_order), pending: BTreeMap::new() }
    }

    /// Submits one selected successful plan. Returns the newly contiguous
    /// lineage prefix, which must be enacted in the returned order.
    #[must_use = "released plans must be enacted in lineage order"]
    pub fn submit(
        &mut self,
        order: u64,
        plan: &BranchMergePlan,
    ) -> Result<Vec<BranchMergePlan>, BranchMergePlanSequenceError> {
        self.submit_selected(order, plan)
            .map(|ready| ready.into_iter().map(|(_, plan)| plan).collect())
    }

    /// Submits one selected successful plan and returns each newly contiguous
    /// plan with its commit position and depth-ordered tombstones attached.
    /// Tombstone order within each paired step follows table and canonical key
    /// order, independent of validated split ranges; the returned steps
    /// preserve commit lineage across calls, regardless of worker completion
    /// or depth layout.
    #[must_use = "released paired plans and tombstone deltas must be enacted in lineage order"]
    pub fn submit_with_tombstone_deltas(
        &mut self,
        order: u64,
        plan: &BranchMergePlan,
    ) -> Result<Vec<SequencedBranchMergePlan>, BranchMergePlanSequenceError> {
        self.submit_selected(order, plan).map(|ready| {
            ready
                .into_iter()
                .map(|(order, plan)| SequencedBranchMergePlan {
                    order,
                    ordered_row_tombstones: plan.ordered_row_tombstones(),
                    plan,
                })
                .collect()
        })
    }

    fn submit_selected(
        &mut self,
        order: u64,
        plan: &BranchMergePlan,
    ) -> Result<Vec<(u64, BranchMergePlan)>, BranchMergePlanSequenceError> {
        let Some(next_order) = self.next_order else {
            return Err(BranchMergePlanSequenceError::OrderExhausted);
        };
        if order < next_order || self.pending.contains_key(&order) {
            return Err(BranchMergePlanSequenceError::DuplicateOrStale { order });
        }

        self.pending.insert(order, plan.clone());
        let mut ready = Vec::new();
        while let Some(next_order) = self.next_order {
            let Some(plan) = self.pending.remove(&next_order) else {
                break;
            };
            ready.push((next_order, plan));
            self.next_order = next_order.checked_add(1);
        }
        Ok(ready)
    }

    /// Returns the next lineage position that must arrive before any later
    /// buffered plans can be released.
    pub fn next_order(&self) -> Option<u64> {
        self.next_order
    }
}

/// Computes a complete semantic merge plan or returns bounded conflict facts.
/// It never writes files, advances refs, or mutates input snapshots.
///
/// MERGE-1 leaves ordering across tables, split ranges, and recovery retries
/// open. This v1 policy walks table IDs in ascending order, aligned manifest
/// ranges in their validated order, and exact row keys in canonical
/// primary-key order. It therefore gives paired depth merges the same
/// tombstone and conflict order regardless of adapter visitation order or
/// concurrent scheduling.
///
/// A path-shaped tombstone chain remains a sequence of exact-key decisions:
/// chain depth extends the ordered walk but never gives a prefix deletion
/// precedence over a descendant row. If a chain read fails partway through,
/// the incomplete prefix is not observable; recovery retries produce the full
/// canonical sequence from the first table and range.
///
/// Paired chain storms run with per-invocation budgets and result buffers.
/// Concurrent retries therefore preserve each table's complete tombstone
/// sequence without sharing partial work or budget state. Storm breadth across
/// several prefix depths does not change table-local order or delete
/// precedence, even when a later table's scan fails after earlier tables.
///
/// MERGE-1 does not specify how tombstone output accumulates across separately
/// committed merge plans. This v1 policy treats each plan as a delta from its
/// own common base: after a completed plan is committed, its live rows and
/// manifest form the next base, and a later plan reports only exact keys newly
/// deleted relative to that base. Earlier tombstones remain part of committed
/// history but are not replayed as new deletions; deleting a descendant in a
/// later wave remains an independent exact-key decision. A failed wave is
/// retried from the same committed base without carrying forward partial facts.
/// A later wave may restore a key that an earlier wave deleted: that wave
/// emits the row as an upsert and does not replay the old tombstone. If a
/// committed later wave deletes the restored key again, its delta contains a
/// new tombstone event for that exact key. Consumers that retain a history
/// append only the selected committed plan's delta, in commit order; competing
/// retries from one base are alternatives and must not append duplicate events.
/// A key absent from the current base is reconciled as an ordinary creation,
/// even when an older committed delta tombstoned it. Equal explicit-row
/// restorations converge; divergent restorations conflict by exact key in the
/// normal table/key order. A conflicted wave has no appendable tombstone delta,
/// and retrying from the same committed base does not give either restoration
/// precedence because of older history. Swapping left and right branch
/// orientation likewise preserves the conflict key identities and their
/// table/key order.
/// If a storm attempt reads one table's conflict prefix and then fails while
/// loading a later paired table, that prefix is discarded with the attempt.
/// Recovered retries rebuild the full branch-symmetric conflict set from the
/// same committed base; failed work cannot duplicate or bias a later delta.
/// The same restart rule applies to successful restoration waves: if a later
/// paired-table read fails after an earlier table found restored rows, no
/// partial upsert plan is returned. Concurrent retries from the unchanged
/// post-delete base independently rebuild the complete restored row set and
/// emit no old tombstones; commit one successful retry as the wave's delta.
/// Across chained merge waves, each restoration retry starts from the latest
/// committed live rows. Earlier wave deltas remain historical facts, while a
/// later retry emits only its own exact-key tombstones and restored rows; a
/// failed attempt cannot replay prior deletes or discard earlier restores.
/// Paired tables may carry chains with different depths and split layouts.
/// A retry still rebuilds each table's restored exact keys in that table's
/// canonical order; failure in one chain discards the whole paired candidate,
/// and the recovered plan preserves both table-local results and old history.
/// A storm on the deeper chain does not change that contract: after its prior
/// delete wave is committed, a retry may restore storm keys while tombstoning
/// still-live keys in either paired table. The candidate contains only those
/// new table-local deletes, and a later-chain read failure returns no part of
/// the deeper table's or its peer's restore plan.
/// A subsequent wave may restore keys deleted by that storm wave while
/// deleting storm or chain keys restored in it. Retries start from that latest
/// paired base, emit only the new exact-key deletions, and append one selected
/// plan so re-deletes become new history events without replaying old ones.
/// Concurrent retries may target different committed waves at once. MERGE-1
/// does not specify how those plans compose, so this v1 policy keeps each
/// result relative to its own paired base and branch snapshots; one wave's
/// restore or tombstone delta never becomes another wave's implicit input.
/// Worker completion order is not history order: callers sequence selected
/// successful deltas by their committed base lineage and append each only
/// after that wave commits. `BranchMergePlanSequencer` buffers a later
/// successful plan until its earlier lineage positions arrive, then returns
/// complete paired plans in the order adapters must enact them. A failed or
/// conflicted attempt leaves its position available for a retry.
/// Across several successors, each append preserves the entire committed
/// prefix; a wave-local plan never replaces or truncates its ancestors' events.
/// A paired plan is one lineage step across its tables: commit both table
/// results from the same paired base, while each table contributes only its
/// own tombstone delta. A table with no new tombstones contributes no event,
/// and its existing history remains an unchanged prefix.
/// Within one table's delta keys stay in canonical depth order, but separate
/// commits append by lineage: a later shallow ancestor tombstone follows
/// earlier descendant storm events instead of being sorted ahead of them.
/// If successive plans use different split boundaries, each plan first
/// flattens its keys in table and canonical primary-key order, independent of
/// that plan's ranges; depth changes never reorder or repartition the already-
/// committed prefix.
/// A conflicted candidate is not a paired commit step and has no appendable
/// delta; concurrent retries from its base are alternatives, and at most one
/// successful plan advances that paired lineage position.
/// MERGE-1 is silent on isolation between concurrent retry invocations. This
/// v1 policy keeps row buffers, budgets, and candidate plans invocation-local:
/// a failure after one paired table has materialized aborts only that attempt,
/// while peer retries from the same committed base can still return complete
/// independent plans. Callers append only one successful plan per committed
/// wave.
///
/// A read failure at any table or range aborts the whole invocation. Facts
/// gathered from earlier tables or depth ranges remain private; after source
/// recovery, a fresh retry starts from the first table and range. Clean
/// tombstones do not consume conflict budget. If the conflict budget is
/// exceeded, the result contains no partial conflict list or candidate plan;
/// its lower bound includes the first conflict beyond the configured limit,
/// and its affected ranges identify the bounded scan.
pub fn merge_three_way_snapshots<R: BranchRowSource>(
    base: &ThreeWaySnapshot,
    left: &ThreeWaySnapshot,
    right: &ThreeWaySnapshot,
    rows: &mut R,
    budget: BranchMergeBudget,
) -> Result<BranchMergePlan, BranchMergeError> {
    let mut report = BranchMergeReport::default();
    let mut conflicts = Vec::new();
    // Resolution has a stable phase boundary: schema must be settled before
    // row data is read; after that, row conflicts are collected before
    // checkpoint conflicts. A schema conflict ends planning even if it leaves
    // budget unused, because lower phases cannot be resolved against a
    // candidate schema. MERGE-1 requires an isolated complete result but
    // leaves diagnostic ordering open, so this order is the deterministic
    // policy used here.
    let schema = match merge_schema_bounded(&base.schema, &left.schema, &right.schema, budget.max_conflicts) {
        Ok(schema) => schema,
        Err(schema_failure) => {
            for conflict in &schema_failure.conflicts {
                let affected = conflict.affected_tables();
                if affected.is_empty() {
                    report.affected_tables.extend(base.schema.tables.iter().map(|table| table.id));
                    report.affected_tables.extend(left.schema.tables.iter().map(|table| table.id));
                    report.affected_tables.extend(right.schema.tables.iter().map(|table| table.id));
                } else {
                    report.affected_tables.extend(affected);
                }
            }
            if schema_failure.conflicts_lower_bound > schema_failure.conflicts.len() {
                // The budget can truncate away the only detail naming a
                // conflicted table (including a zero-detail budget). When
                // that happens, report every candidate table conservatively;
                // the reference requires affected-table evidence but does
                // not define a summary shape for omitted schema details.
                report.affected_tables.extend(base.schema.tables.iter().map(|table| table.id));
                report.affected_tables.extend(left.schema.tables.iter().map(|table| table.id));
                report.affected_tables.extend(right.schema.tables.iter().map(|table| table.id));
            }
            if schema_failure.conflicts_lower_bound > budget.max_conflicts {
                report.conflicts_lower_bound = schema_failure.conflicts_lower_bound;
                return Err(BranchMergeError::BudgetExceeded { report });
            }
            report.conflicts_lower_bound = schema_failure.conflicts_lower_bound;
            conflicts.extend(schema_failure.conflicts.into_iter().map(BranchMergeConflict::Schema));
            return Err(BranchMergeError::Conflicts { conflicts, report });
        }
    };

    // The ascending union is part of the recovery ordering policy: a paired
    // depth walk cannot reorder tombstones or conflicts by table visitation.
    let table_ids: BTreeSet<_> = base.tables.keys().chain(left.tables.keys()).chain(right.tables.keys()).copied().collect();
    let mut merged_tables = BTreeMap::new();
    for table in table_ids {
        let b = base.tables.get(&table);
        let l = left.tables.get(&table);
        let r = right.tables.get(&table);
        match merge_table(table, b, l, r, rows, budget, &mut conflicts, &mut report)? {
            Some(table_plan) => { merged_tables.insert(table, table_plan); }
            None => {}
        }
    }

    // If the bounded row phase stops early, do not mix checkpoint impacts
    // into a report whose row materialization was cut short.
    let checkpoints = merge_checkpoint_phase(
        base,
        left,
        right,
        budget,
        &mut conflicts,
        &mut report,
    )?;

    if !conflicts.is_empty() {
        // Intermediate table candidates are never observable when any later
        // row or checkpoint conflict remains unresolved.
        return Err(BranchMergeError::Conflicts { conflicts, report });
    }
    Ok(BranchMergePlan { schema, tables: merged_tables, checkpoints, report })
}

/// Resolves storage checkpoint state after row ranges have been planned.
///
/// A missing map entry is a checkpoint deletion; a present generation with
/// `position: None` is a valid reset. The bytewise ordered ID union keeps
/// strict-prefix, extension and deeply nested identities independent while
/// giving checkpoint conflicts a stable order in the shared detail budget.
fn merge_checkpoint_phase(
    base: &ThreeWaySnapshot,
    left: &ThreeWaySnapshot,
    right: &ThreeWaySnapshot,
    budget: BranchMergeBudget,
    conflicts: &mut Vec<BranchMergeConflict>,
    report: &mut BranchMergeReport,
) -> Result<BTreeMap<CheckpointId, CheckpointGeneration>, BranchMergeError> {
    let checkpoint_ids: BTreeSet<_> = base.checkpoints.keys().chain(left.checkpoints.keys()).chain(right.checkpoints.keys()).cloned().collect();
    let mut checkpoints = BTreeMap::new();
    for id in checkpoint_ids {
        match merge_checkpoint_generation(
            base.checkpoints.get(&id),
            left.checkpoints.get(&id),
            right.checkpoints.get(&id),
        ) {
            Ok(Some(checkpoint)) => { checkpoints.insert(id, checkpoint); }
            Ok(None) => {}
            Err(conflict) => {
                report.affected_checkpoints.insert(id.clone());
                if record_conflict(
                    BranchMergeConflict::CheckpointConflict { id, conflict },
                    None,
                    None,
                    conflicts,
                    report,
                    budget,
                ) {
                    return Err(BranchMergeError::BudgetExceeded { report: report.clone() });
                }
            }
        }
    }
    Ok(checkpoints)
}

fn merge_table<R: BranchRowSource>(
    table: ObjectId,
    base: Option<&TableManifest>,
    left: Option<&TableManifest>,
    right: Option<&TableManifest>,
    rows: &mut R,
    budget: BranchMergeBudget,
    conflicts: &mut Vec<BranchMergeConflict>,
    report: &mut BranchMergeReport,
) -> Result<Option<MergedTable>, BranchMergeError> {
    for manifest in [base, left, right].into_iter().flatten() {
        if !valid_manifest(manifest) {
            return Err(BranchMergeError::InvalidManifest { table });
        }
    }
    let reuse = |from, manifest: &TableManifest| MergedTable {
        id: table,
        whole_table_reuse: Some((from, manifest.clone())),
        segments: Vec::new(),
    };
    match (base, left, right) {
        (None, None, None) => Ok(None),
        (None, Some(l), None) => Ok(Some(reuse(MergeSide::Left, l))),
        (None, None, Some(r)) => Ok(Some(reuse(MergeSide::Right, r))),
        (None, Some(l), Some(r)) if l.digest == r.digest => Ok(Some(reuse(MergeSide::Left, l))),
        (None, Some(_), Some(_)) => {
            if record_conflict(BranchMergeConflict::Table { table, reason: "same table identity added differently" }, Some(table), None, conflicts, report, budget) {
                return Err(BranchMergeError::BudgetExceeded { report: report.clone() });
            }
            Ok(None)
        }
        (Some(_), None, None) => Ok(None),
        (Some(b), None, Some(r)) if b.digest == r.digest => Ok(None),
        (Some(b), Some(l), None) if b.digest == l.digest => Ok(None),
        (Some(_), None, Some(_)) | (Some(_), Some(_), None) => {
            if record_conflict(BranchMergeConflict::Table { table, reason: "table deleted on one side and edited on the other" }, Some(table), None, conflicts, report, budget) {
                return Err(BranchMergeError::BudgetExceeded { report: report.clone() });
            }
            Ok(None)
        }
        (Some(_b), Some(l), Some(r)) if l.digest == r.digest => Ok(Some(reuse(MergeSide::Left, l))),
        (Some(b), Some(l), Some(r)) if l.digest == b.digest => Ok(Some(reuse(MergeSide::Right, r))),
        (Some(b), Some(l), Some(r)) if r.digest == b.digest => Ok(Some(reuse(MergeSide::Left, l))),
        (Some(b), Some(l), Some(r)) => {
            // Matching segment layouts let unchanged subtrees be reused by
            // digest. Different layouts fall back to one table-wide key scan;
            // merging arbitrary overlapping segment boundaries needs an
            // adapter-provided range index, which v1 does not require.
            let aligned = aligned_layouts(b, l, r);
            let mut segments = Vec::new();
            if aligned {
                // `valid_manifest` checks boundaries by canonical primary-key
                // order. Preserve that segment order while consuming the
                // shared conflict budget: clean tombstones in an earlier
                // depth range are entries in the walk, not conflict slots,
                // even when encoded boundary bytes sort differently.
                for index in 0..b.segments.len() {
                    let (bs, ls, rs) = (&b.segments[index], &l.segments[index], &r.segments[index]);
                    if ls.digest == rs.digest {
                        segments.push(MergedSegment::Reuse { from: MergeSide::Left, manifest: ls.clone() });
                    } else if ls.digest == bs.digest {
                        segments.push(MergedSegment::Reuse { from: MergeSide::Right, manifest: rs.clone() });
                    } else if rs.digest == bs.digest {
                        segments.push(MergedSegment::Reuse { from: MergeSide::Left, manifest: ls.clone() });
                    } else {
                        let range = bs.range.clone();
                        let base_rows = read_segment(rows, MergeSide::Base, table, Some(bs), &range, budget, report)?;
                        let left_rows = read_segment(rows, MergeSide::Left, table, Some(ls), &range, budget, report)?;
                        let right_rows = read_segment(rows, MergeSide::Right, table, Some(rs), &range, budget, report)?;
                        let merged = merge_range_rows(table, range.clone(), base_rows, left_rows, right_rows, budget, conflicts, report)?;
                        segments.push(MergedSegment::Rows {
                            range,
                            rows: merged.rows,
                            tombstones: merged.tombstones,
                        });
                    }
                }
            } else {
                // With different segment layouts there is no common split
                // walk to define output order. Read one complete logical
                // range, then let `merge_range_rows` sort exact keys by the
                // canonical primary-key comparator. This makes tombstones
                // and conflict details independent of source visitation and
                // of other concurrently planned merges.
                let range = KeyRange::all();
                let base_rows = read_segment(rows, MergeSide::Base, table, None, &range, budget, report)?;
                let left_rows = read_segment(rows, MergeSide::Left, table, None, &range, budget, report)?;
                let right_rows = read_segment(rows, MergeSide::Right, table, None, &range, budget, report)?;
                let merged = merge_range_rows(table, range.clone(), base_rows, left_rows, right_rows, budget, conflicts, report)?;
                segments.push(MergedSegment::Rows {
                    range,
                    rows: merged.rows,
                    tombstones: merged.tombstones,
                });
            }
            Ok(Some(MergedTable { id: table, whole_table_reuse: None, segments }))
        }
    }
}

fn valid_manifest(manifest: &TableManifest) -> bool {
    if manifest.segments.is_empty() {
        return true;
    }
    let mut previous_end: Option<&[u8]> = None;
    for (index, segment) in manifest.segments.iter().enumerate() {
        if KeyRange::new(segment.range.start.clone(), segment.range.end.clone()).is_none() {
            return false;
        }
        if index == 0 && segment.range.start.is_some() {
            return false;
        }
        if previous_end != segment.range.start.as_deref() {
            return false;
        }
        previous_end = segment.range.end.as_deref();
    }
    previous_end.is_none()
}

fn aligned_layouts(base: &TableManifest, left: &TableManifest, right: &TableManifest) -> bool {
    let same = |a: &TableManifest, b: &TableManifest| {
        a.segments.iter().map(|s| &s.range).eq(b.segments.iter().map(|s| &s.range))
    };
    same(base, left) && same(base, right)
}

fn read_segment<R: BranchRowSource>(
    source: &mut R,
    side: MergeSide,
    table: ObjectId,
    segment: Option<&RowSegmentManifest>,
    range: &KeyRange,
    budget: BranchMergeBudget,
    report: &mut BranchMergeReport,
) -> Result<BTreeMap<Vec<u8>, KeyedRow>, BranchMergeError> {
    report.affected_tables.insert(table);
    report.affected_ranges.insert((table, range.clone()));
    let mut decoded = BTreeMap::new();
    let mut budget_hit = false;
    let mut invalid = false;
    source.visit_rows(side, table, segment, range, &mut |row| {
        if report.rows_examined >= budget.max_rows_examined {
            report.rows_examined = report.rows_examined.saturating_add(1);
            budget_hit = true;
            return false;
        }
        report.rows_examined += 1;
        if row.table != table {
            invalid = true;
            return false;
        }
        let Ok(key) = row.key.encode() else {
            invalid = true;
            return false;
        };
        if !range.contains(&key) || decoded.insert(key, row).is_some() {
            invalid = true;
            return false;
        }
        true
    }).map_err(|message| BranchMergeError::RowRead { message })?;
    if budget_hit {
        return Err(BranchMergeError::BudgetExceeded { report: report.clone() });
    }
    if invalid {
        return Err(BranchMergeError::InvalidRow { table });
    }
    Ok(decoded)
}

struct MergedRangeRows {
    rows: Vec<KeyedRow>,
    tombstones: Vec<CanonicalValue>,
}

fn merge_range_rows(
    table: ObjectId,
    range: KeyRange,
    base: BTreeMap<Vec<u8>, KeyedRow>,
    left: BTreeMap<Vec<u8>, KeyedRow>,
    right: BTreeMap<Vec<u8>, KeyedRow>,
    budget: BranchMergeBudget,
    conflicts: &mut Vec<BranchMergeConflict>,
    report: &mut BranchMergeReport,
) -> Result<MergedRangeRows, BranchMergeError> {
    let encoded_keys: BTreeSet<_> = base
        .keys()
        .chain(left.keys())
        .chain(right.keys())
        .cloned()
        .collect();
    let mut keys = Vec::with_capacity(encoded_keys.len());
    for encoded in encoded_keys {
        let key = CanonicalValue::decode(&encoded)
            .map_err(|_| BranchMergeError::InvalidRow { table })?;
        keys.push((encoded, key));
    }
    // Encoded byte order is not always the table's logical primary-key order:
    // a short text key can sort before a lexically earlier deep path because
    // its CBOR length prefix is smaller. Depth is not a delete precedence
    // rule; merge each exact key independently and traverse in the same order
    // used by compact storage.
    let invalid_key_order = Cell::new(false);
    keys.sort_by(|(left_bytes, left_key), (right_bytes, right_key)| {
        match compare_primary_keys(left_key, right_key) {
            Ok(Ordering::Equal) => left_bytes.cmp(right_bytes),
            Ok(ordering) => ordering,
            Err(_) => {
                invalid_key_order.set(true);
                left_bytes.cmp(right_bytes)
            }
        }
    });
    if invalid_key_order.get() {
        return Err(BranchMergeError::InvalidRow { table });
    }
    let mut merged = Vec::new();
    let mut tombstones = Vec::new();
    for (key, _) in keys {
        let base_state = base
            .get(&key)
            .map(RowSnapshotState::Present)
            .unwrap_or(RowSnapshotState::Absent);
        let left_state = left
            .get(&key)
            .map(RowSnapshotState::Present)
            .unwrap_or(RowSnapshotState::Absent);
        let right_state = right
            .get(&key)
            .map(RowSnapshotState::Present)
            .unwrap_or(RowSnapshotState::Absent);
        match merge_keyed_row_states(base_state, left_state, right_state) {
            Ok(RowMergeOperation::Upsert(row)) => merged.push(row),
            Ok(RowMergeOperation::Tombstone { key, .. }) => tombstones.push(key),
            Ok(RowMergeOperation::Absent) => {}
            Err(RowSnapshotMergeError::PrunedInput { .. }) => {
                // `read_segment` must fail before producing an incomplete
                // map. Keep this guard fail-closed if a future caller adds
                // an unavailable row state to the decoded merge input.
                return Err(BranchMergeError::RowRead {
                    message: "pruned row state reached the semantic merge".into(),
                });
            }
            Err(RowSnapshotMergeError::Conflict(conflict)) => {
                if record_conflict(
                    BranchMergeConflict::Row { range: range.clone(), conflict },
                    Some(table),
                    Some((table, range.clone())),
                    conflicts,
                    report,
                    budget,
                ) {
                    return Err(BranchMergeError::BudgetExceeded { report: report.clone() });
                }
            }
        }
    }
    Ok(MergedRangeRows { rows: merged, tombstones })
}

fn record_conflict(
    conflict: BranchMergeConflict,
    table: Option<ObjectId>,
    range: Option<(ObjectId, KeyRange)>,
    conflicts: &mut Vec<BranchMergeConflict>,
    report: &mut BranchMergeReport,
    budget: BranchMergeBudget,
) -> bool {
    // The same materialization budget spans row and checkpoint phases. The
    // first conflict over it stops resolution and returns only its bounded
    // location summary, never an incomplete plan or an unbounded detail list.
    report.conflicts_lower_bound = report.conflicts_lower_bound.saturating_add(1);
    if let Some(table) = table { report.affected_tables.insert(table); }
    if let Some(range) = range { report.affected_ranges.insert(range); }
    if conflicts.len() < budget.max_conflicts { conflicts.push(conflict); }
    report.conflicts_lower_bound > budget.max_conflicts
}
