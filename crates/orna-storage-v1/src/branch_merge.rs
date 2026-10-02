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
use orna_foundation_v1::{OvbRaw, compare_primary_keys};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write as _},
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
                compare_primary_keys_with_encoding_tiebreak(left_key, right_key)
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

/// One incomplete paired depth wave and the replacement fragments to apply
/// during an atomic recovery transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeDepthWaveRecovery {
    pub order: u64,
    pub fragment_count: usize,
    pub fragments: BTreeMap<usize, Vec<(ObjectId, CanonicalValue)>>,
}

/// Replacement or completion data for one fragment in an already buffered
/// paired depth wave. Applying this record replaces that fragment if present
/// and preserves all other buffered fragments in the wave.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeDepthFragmentRecovery {
    pub order: u64,
    pub fragment: usize,
    pub fragment_count: usize,
    pub tombstones: Vec<(ObjectId, CanonicalValue)>,
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
    /// No incomplete buffered depth wave exists at this lineage position.
    NoIncompleteDepthWave { order: u64 },
    /// A batch wave-restart request must include at least one position.
    EmptyDepthWaveRestartBatch,
    /// A batch wave-restart request names one position more than once.
    DuplicateDepthWaveRestart { order: u64 },
    /// A batch retry-plan binding request must include at least one position.
    EmptyDepthFragmentRetryPlanBindingBatch,
    /// A batch retry-plan binding request names one position more than once.
    DuplicateDepthFragmentRetryPlanBinding { order: u64 },
    /// A fragment-recovery request must include at least one fragment.
    EmptyDepthFragmentRecoveryBatch,
    /// A fragment-recovery request names one position and index more than once.
    DuplicateDepthFragmentRecovery { order: u64, fragment: usize },
    /// Overlapping fragments attempted to record one table/key twice in a wave.
    DuplicateTombstone { order: u64 },
    /// Two concurrently buffered lineage positions record one table/key.
    ConcurrentDuplicateTombstone { first_order: u64, second_order: u64 },
    /// A whole paired plan and split depth fragments were both submitted for one position.
    ConflictingSubmission { order: u64 },
    /// A recovery append reused an accepted whole-plan position with a different paired result.
    AppendRetryMismatch { order: u64 },
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

type PairedRetryPlanSignature = (u64, [u8; 32], Vec<(ObjectId, CanonicalValue)>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct AppliedDepthFragmentRetryTransaction {
    bindings: Vec<PairedRetryPlanSignature>,
    recoveries: Vec<BranchMergeDepthFragmentRecovery>,
    appends: Vec<PairedRetryPlanSignature>,
}

impl AppliedDepthFragmentRetryTransaction {
    fn new(
        bindings: &[SequencedBranchMergePlan],
        recoveries: &[BranchMergeDepthFragmentRecovery],
        appends: &[SequencedBranchMergePlan],
    ) -> Self {
        let plan_signatures = |steps: &[SequencedBranchMergePlan]| {
            let mut signatures = steps
                .iter()
                .map(|step| {
                    (
                        step.order,
                        paired_plan_retry_identity(&step.plan),
                        step.ordered_row_tombstones.clone(),
                    )
                })
                .collect::<Vec<_>>();
            signatures.sort_unstable_by_key(|(order, _, _)| *order);
            signatures
        };
        let mut recoveries = recoveries.to_vec();
        recoveries.sort_unstable_by_key(|recovery| (recovery.order, recovery.fragment));
        Self {
            bindings: plan_signatures(bindings),
            recoveries,
            appends: plan_signatures(appends),
        }
    }
}

/// Append-only tombstone history for committed paired merge plans.
///
/// MERGE-1 is silent on tombstone accumulation across committed waves,
/// overlapping depth fragments, and cross-mode retries after release. This v1
/// policy retains the accepted mode for each released position and the first
/// mode rejected by a cross-position duplicate until a corrected delta is
/// buffered. It accepts paired plans at exact lineage positions and appends table/key-ordered
/// deletion events in lineage order, advancing through restore-only empty deltas.
/// Duplicate table/key events within one lineage position are rejected as
/// soon as the overlapping fragment arrives. Two concurrently buffered
/// positions cannot record the same logical key when every intervening
/// position is buffered and also records that key. A cross-position duplicate
/// reports both positions and buffers no tombstone data. If the rejected
/// submission passes local shape and within-wave checks but is rejected by a
/// cross-position duplicate, its mode remains as retry intent until a corrected
/// same-mode delta is buffered; an opposite-mode retry conflicts. A missing
/// position delays duplicate classification; an intervening position that omits the key
/// separates a later re-delete. Once an earlier position has been released,
/// a later position may record the key again as a separate event.
/// Submission positions are classified centrally. Exhaustion takes priority;
/// accepted and duplicate-retry positions keep their mode through release.
/// `submit` and fragment submission classify replays of accepted positions as
/// stale and cross-mode retries as conflicts. `append` writes at the first
/// unoccupied position after the contiguous buffered prefix, so whole paired
/// plans can queue behind an incomplete depth wave without crossing a gap. It
/// preserves its strict `OutOfOrder` result for same-mode or unoccupied
/// position mismatches but checks known cross-mode conflicts first. Failed
/// appends are atomic: a cross-position duplicate does not retain the
/// retry-mode reservation that `submit` and fragment submission keep.
/// An incomplete depth wave can be explicitly restarted after its split plan
/// is recomputed; restart clears only that wave's buffered pieces and keeps
/// its lineage position, submission mode, and later append queue.
/// Multiple wave restarts can be applied as one transaction: the batch is
/// normalized by lineage order, and any invalid member leaves every wave and
/// queued append unchanged.
/// A recovery batch may also include new whole-plan appends, which are applied
/// after the restarts and participate in the same all-or-nothing transaction.
/// Recovery can include replacement fragments as well; restarts, appends, and
/// fragment submissions commit together, with released events returned only
/// after the entire transaction succeeds. MERGE-1 does not specify retries for
/// these tombstone appends. This v1 policy treats a repeated same-order
/// whole-plan delta as an idempotent retry only when its schema, checkpoint
/// state, materialized table result, and tombstone set match the accepted
/// paired plan. Materialized row and tombstone partitions are normalized, so
/// equivalent plans with uneven depth boundaries still match. A changed
/// paired result at an accepted order returns `AppendRetryMismatch`. A
/// recovery append may also match a complete depth-fragment delta at that
/// same order, independent of fragment count or boundaries, provided the
/// batch does not replace fragments at that order. A caller with the paired
/// plan can bind its full identity to a complete fragment wave; unbound legacy
/// waves can compare only their tombstone projection because fragment
/// submissions carry no schema, checkpoint, or row state. Incomplete or
/// changed cross-mode deltas still return `ConflictingSubmission`; ordinary
/// submission APIs remain mode-strict. A successful combined bind/recovery
/// transaction records its normalized request, so replaying the same paired
/// identities and fragment repairs returns no events and cannot duplicate
/// committed tombstones.
/// Unrecorded stale order precedes
/// buffered and retry-mode checks, and mixed-mode conflicts precede
/// fragment-index or tombstone-content validation.
/// Concurrent completions may arrive out of order; future deltas wait until
/// every earlier paired position is present. Split waves wait until every
/// fragment arrives, then flatten in canonical table/key order atomically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTombstoneHistory {
    next_order: Option<u64>,
    events: Vec<BranchMergeTombstoneEvent>,
    pending_deltas: BTreeMap<u64, BufferedBranchMergeTombstoneDelta>,
    pending_plan_identities: BTreeMap<u64, [u8; 32]>,
    committed_modes: BTreeMap<u64, BranchMergeTombstoneSubmissionMode>,
    committed_plan_identities: BTreeMap<u64, [u8; 32]>,
    duplicate_retry_modes: BTreeMap<u64, BranchMergeTombstoneSubmissionMode>,
    applied_fragment_retry_transactions: Vec<AppliedDepthFragmentRetryTransaction>,
}

impl BranchMergeTombstoneHistory {
    /// Starts history after the caller's already-committed prefix.
    pub fn new(first_order: u64) -> Self {
        Self {
            next_order: Some(first_order),
            events: Vec::new(),
            pending_deltas: BTreeMap::new(),
            pending_plan_identities: BTreeMap::new(),
            committed_modes: BTreeMap::new(),
            committed_plan_identities: BTreeMap::new(),
            duplicate_retry_modes: BTreeMap::new(),
            applied_fragment_retry_transactions: Vec::new(),
        }
    }

    /// Appends one plan emitted by [`BranchMergePlanSequencer`] after the
    /// contiguous positions already buffered or released. Whole plans may
    /// queue behind incomplete depth waves, but cannot skip an unbuffered
    /// position. Empty deltas still consume their paired plan order. A
    /// rejected append leaves all history, including retry-mode state,
    /// unchanged.
    pub fn append(
        &mut self,
        step: &SequencedBranchMergePlan,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let Some(mut expected) = self.next_order else {
            return Err(BranchMergeTombstoneHistoryError::OrderExhausted);
        };
        self.classify_submission_mode_conflict(
            step.order,
            BranchMergeTombstoneSubmissionMode::WholePlan,
        )?;
        while self.pending_deltas.contains_key(&expected) {
            let Some(next) = expected.checked_add(1) else {
                return Err(BranchMergeTombstoneHistoryError::OrderExhausted);
            };
            expected = next;
        }
        if step.order != expected {
            return Err(BranchMergeTombstoneHistoryError::OutOfOrder {
                expected,
                actual: step.order,
            });
        }

        let mut candidate = self.clone();
        candidate.submit(step)?;
        *self = candidate;
        Ok(())
    }

    /// Submits a completed paired plan, buffering future lineage positions
    /// until the missing prefix arrives. Returns only the new contiguous
    /// tombstone events released by this submission. Repeated or stale
    /// positions are rejected without changing buffered or committed tombstones.
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
            self.reserve_duplicate_retry_mode(
                step.order,
                BranchMergeTombstoneSubmissionMode::WholePlan,
            );
            return Err(concurrent_duplicate_error(step.order, other_order));
        }

        self.duplicate_retry_modes.remove(&step.order);
        self.pending_plan_identities
            .insert(step.order, paired_plan_retry_identity(&step.plan));
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
            self.reserve_duplicate_retry_mode(
                order,
                BranchMergeTombstoneSubmissionMode::DepthFragments,
            );
            return Err(concurrent_duplicate_error(order, other_order));
        }

        self.duplicate_retry_modes.remove(&order);
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

    /// Associates a complete paired result with an already-complete depth
    /// fragment wave at the same order. This preserves the schema, checkpoint,
    /// row, and tombstone identity that fragments cannot carry by themselves,
    /// so an equivalent recovery append can be deduplicated across uneven
    /// fragment boundaries without accepting a different paired result.
    ///
    /// The wave may still be pending behind an earlier lineage gap or may
    /// already be committed. Incomplete, missing, conflicting, or tombstone-
    /// mismatched waves return [`BranchMergeTombstoneHistoryError::ConflictingSubmission`]
    /// without changing history. Rebinding the same identity is idempotent.
    /// MERGE-1 does not specify this cross-mode recovery case; this v1 policy
    /// uses the full paired retry identity when a caller supplies it and keeps
    /// tombstone-projection matching for legacy unbound fragment waves.
    pub fn bind_depth_fragment_retry_plan(
        &mut self,
        step: &SequencedBranchMergePlan,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let order = step.order;
        let planned_tombstones = step.plan.ordered_row_tombstones();
        if has_duplicate_tombstones_in_wave(&planned_tombstones)
            || !same_tombstone_encoding_delta(&planned_tombstones, &step.ordered_row_tombstones)
        {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }

        let existing = match self.pending_deltas.get(&order) {
            Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count,
                fragments,
            }) if fragments.len() == *fragment_count => {
                Some(fragments.values().flatten().cloned().collect::<Vec<_>>())
            }
            _ if self.committed_modes.get(&order)
                == Some(&BranchMergeTombstoneSubmissionMode::DepthFragments) => Some(
                    self.events
                        .iter()
                        .filter(|event| event.order == order)
                        .map(|event| (event.table, event.key.clone()))
                        .collect::<Vec<_>>(),
                ),
            _ => None,
        };
        let Some(existing) = existing else {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        };
        if !same_tombstone_encoding_delta(&existing, &step.ordered_row_tombstones) {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }

        let identity = paired_plan_retry_identity(&step.plan);
        let identities = if self.committed_modes.get(&order)
            == Some(&BranchMergeTombstoneSubmissionMode::DepthFragments)
        {
            &mut self.committed_plan_identities
        } else {
            &mut self.pending_plan_identities
        };
        if identities.get(&order).is_some_and(|previous| *previous != identity) {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }
        identities.insert(order, identity);
        Ok(())
    }

    /// Atomically binds paired retry identities to several complete depth
    /// fragment waves. Positions are validated in ascending lineage order so
    /// the reported error is independent of caller order. Every position must
    /// be unique and refer to a complete pending or committed fragment wave;
    /// if any binding fails, none of the identities are retained. An empty
    /// batch and repeated positions are rejected.
    ///
    /// MERGE-1 is silent on binding multiple paired plans to one recovery
    /// chain. This v1 policy treats the batch as one all-or-nothing identity
    /// update while leaving the single-wave binding API available to callers
    /// that recover positions independently.
    pub fn bind_depth_fragment_retry_plans(
        &mut self,
        steps: &[SequencedBranchMergePlan],
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        if steps.is_empty() {
            return Err(
                BranchMergeTombstoneHistoryError::EmptyDepthFragmentRetryPlanBindingBatch,
            );
        }

        let mut steps = steps.to_vec();
        steps.sort_unstable_by_key(|step| step.order);
        for pair in steps.windows(2) {
            if pair[0].order == pair[1].order {
                return Err(
                    BranchMergeTombstoneHistoryError::DuplicateDepthFragmentRetryPlanBinding {
                        order: pair[0].order,
                    },
                );
            }
        }

        let mut candidate = self.clone();
        for step in &steps {
            candidate.bind_depth_fragment_retry_plan(step)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Rebinds the full paired identity for one complete, still-pending depth
    /// fragment wave. The replacement plan must retain the exact tombstone
    /// encoding already submitted by the fragments. Released waves cannot be
    /// rebound because their committed event history is immutable.
    ///
    /// MERGE-1 is silent on revised paired plans during a blocked cascade.
    /// This v1 policy permits identity changes only before release and only
    /// when the tombstone projection is unchanged. Successful rebinding also
    /// invalidates replay receipts that refer to the previous identity.
    pub fn rebind_depth_fragment_retry_plan(
        &mut self,
        step: &SequencedBranchMergePlan,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let order = step.order;
        let planned_tombstones = step.plan.ordered_row_tombstones();
        if has_duplicate_tombstones_in_wave(&planned_tombstones)
            || !same_tombstone_encoding_delta(&planned_tombstones, &step.ordered_row_tombstones)
        {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }

        let existing = match self.pending_deltas.get(&order) {
            Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count,
                fragments,
            }) if fragments.len() == *fragment_count => {
                Some(fragments.values().flatten().cloned().collect::<Vec<_>>())
            }
            _ => None,
        };
        let Some(existing) = existing else {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        };
        if !same_tombstone_encoding_delta(&existing, &step.ordered_row_tombstones) {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }

        let Some(previous) = self.pending_plan_identities.get(&order).copied() else {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        };
        let identity = paired_plan_retry_identity(&step.plan);
        if previous != identity {
            self.pending_plan_identities.insert(order, identity);
            self.applied_fragment_retry_transactions.retain(|transaction| {
                !transaction
                    .bindings
                    .iter()
                    .chain(&transaction.appends)
                    .any(|(transaction_order, _, _)| *transaction_order == order)
            });
        }
        Ok(())
    }

    /// Atomically rebinds several complete pending depth waves through one
    /// paired cascade. Positions are checked in lineage order, must be
    /// unique, and must preserve each wave's tombstone projection. If any
    /// position is missing, incomplete, released, or mismatched, no identity
    /// or replay receipt changes. Empty and duplicate batches are rejected.
    pub fn rebind_depth_fragment_retry_plans(
        &mut self,
        steps: &[SequencedBranchMergePlan],
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        if steps.is_empty() {
            return Err(
                BranchMergeTombstoneHistoryError::EmptyDepthFragmentRetryPlanBindingBatch,
            );
        }

        let mut steps = steps.to_vec();
        steps.sort_unstable_by_key(|step| step.order);
        for pair in steps.windows(2) {
            if pair[0].order == pair[1].order {
                return Err(
                    BranchMergeTombstoneHistoryError::DuplicateDepthFragmentRetryPlanBinding {
                        order: pair[0].order,
                    },
                );
            }
        }

        let mut candidate = self.clone();
        for step in &steps {
            candidate.rebind_depth_fragment_retry_plan(step)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Atomically binds paired retry identities, applies fragment recoveries,
    /// and validates same-position paired retry appends as one transaction.
    /// Bindings run first so recovered appends are checked against full paired
    /// identity rather than only their tombstone projection. If any binding,
    /// recovery, or append conflicts, the original history is left unchanged,
    /// including any fragment events that a partial recovery could release.
    ///
    /// MERGE-1 is silent on composing retry identity binding with fragment
    /// recovery. This v1 policy makes the combined operation all-or-nothing;
    /// callers that need separate transactions can use the constituent APIs.
    /// A successful equivalent request can be retried: it returns no new
    /// events and leaves the already-committed result unchanged.
    pub fn bind_depth_fragment_retry_plans_with_recovery(
        &mut self,
        bindings: &[SequencedBranchMergePlan],
        recoveries: &[BranchMergeDepthFragmentRecovery],
        appends: &[SequencedBranchMergePlan],
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        let transaction =
            AppliedDepthFragmentRetryTransaction::new(bindings, recoveries, appends);
        if self
            .applied_fragment_retry_transactions
            .contains(&transaction)
        {
            return Ok(Vec::new());
        }

        let mut candidate = self.clone();
        candidate.bind_depth_fragment_retry_plans(bindings)?;
        let released = candidate.recover_depth_merge_fragments_with_appends(recoveries, appends)?;
        candidate
            .applied_fragment_retry_transactions
            .push(transaction);
        *self = candidate;
        Ok(released)
    }

    /// Restarts an incomplete depth wave after its split plan has been
    /// recomputed. Previously buffered pieces for this position are discarded,
    /// and the new fragment count replaces the old one. Its paired lineage
    /// position and depth-fragment submission mode remain reserved, while
    /// later buffered plans stay queued. No tombstone events are emitted.
    ///
    /// Returns [`BranchMergeTombstoneHistoryError::NoIncompleteDepthWave`] if
    /// this position has no incomplete buffered depth wave to recover.
    pub fn restart_depth_merge_wave(
        &mut self,
        order: u64,
        fragment_count: usize,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        self.classify_submission_position(
            order,
            BranchMergeTombstoneSubmissionMode::DepthFragments,
        )?;
        if fragment_count == 0 {
            return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                fragment: 0,
                fragment_count,
            });
        }

        match self.pending_deltas.get_mut(&order) {
            Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count: previous_count,
                fragments,
            }) if fragments.len() < *previous_count => {
                *previous_count = fragment_count;
                fragments.clear();
                Ok(())
            }
            _ => Err(BranchMergeTombstoneHistoryError::NoIncompleteDepthWave { order }),
        }
    }

    /// Atomically restarts several incomplete depth waves as one recovery
    /// transaction. Positions are processed in ascending lineage order so the
    /// reported error is stable regardless of the caller's input order.
    /// Every position must be unique and refer to an incomplete depth wave;
    /// each replacement count must be nonzero. If validation of any member
    /// fails, the original waves and all later queued appends remain intact.
    /// An empty batch is rejected rather than treated as a successful recovery.
    pub fn restart_depth_merge_waves(
        &mut self,
        restarts: &[(u64, usize)],
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        if restarts.is_empty() {
            return Err(BranchMergeTombstoneHistoryError::EmptyDepthWaveRestartBatch);
        }

        let mut restarts = restarts.to_vec();
        restarts.sort_unstable_by_key(|(order, _)| *order);
        for pair in restarts.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(BranchMergeTombstoneHistoryError::DuplicateDepthWaveRestart {
                    order: pair[0].0,
                });
            }
        }

        let mut candidate = self.clone();
        for (order, fragment_count) in restarts {
            candidate.restart_depth_merge_wave(order, fragment_count)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Atomically restarts incomplete depth waves and adds whole-plan appends
    /// to the resulting queue. Appends are processed in ascending lineage
    /// order after wave validation. If any restart or append fails, neither
    /// the wave replacements nor any earlier append in this batch is retained.
    /// Existing queued appends remain in place throughout the transaction.
    pub fn restart_depth_merge_waves_with_appends(
        &mut self,
        restarts: &[(u64, usize)],
        appends: &[SequencedBranchMergePlan],
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let mut candidate = self.clone();
        candidate.restart_depth_merge_waves(restarts)?;

        let mut appends = appends.to_vec();
        appends.sort_unstable_by_key(|step| step.order);
        for step in &appends {
            candidate.append(step)?;
        }

        *self = candidate;
        Ok(())
    }

    /// Atomically recovers multiple incomplete depth waves with replacement
    /// fragments and adds whole-plan appends to the same lineage queue.
    /// Wave restarts and appends are normalized by lineage order; fragments
    /// within each wave are applied by fragment index. Any invalid restart,
    /// append, or replacement fragment leaves the original history unchanged,
    /// including events that a partial attempt would otherwise release.
    pub fn recover_depth_merge_waves_with_appends(
        &mut self,
        recoveries: &[BranchMergeDepthWaveRecovery],
        appends: &[SequencedBranchMergePlan],
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        let restarts = recoveries
            .iter()
            .map(|recovery| (recovery.order, recovery.fragment_count))
            .collect::<Vec<_>>();
        let mut candidate = self.clone();
        candidate.restart_depth_merge_waves(&restarts)?;

        let mut appends = appends.to_vec();
        appends.sort_unstable_by_key(|step| step.order);
        for step in &appends {
            candidate.append(step)?;
        }

        let mut recoveries = recoveries.to_vec();
        recoveries.sort_unstable_by_key(|recovery| recovery.order);
        let mut released = Vec::new();
        for recovery in &recoveries {
            for (fragment, tombstones) in &recovery.fragments {
                released.extend(candidate.submit_depth_merge_fragment(
                    recovery.order,
                    *fragment,
                    recovery.fragment_count,
                    tombstones,
                )?);
            }
        }

        *self = candidate;
        Ok(released)
    }

    /// Atomically replaces or completes selected fragments in buffered depth
    /// waves and adds whole-plan appends to the same queue. Unlike a wave
    /// restart, unmentioned fragments remain buffered. The supplied fragment
    /// count must match each wave's current count; a fragment index replaces
    /// its buffered value or fills a missing slot. Appends are queued first,
    /// then fragment updates run in lineage and index order. Any failure leaves
    /// the original wave contents, queue, and released events unchanged. An
    /// exact same-order retry of an already queued or committed paired plan is
    /// skipped when its semantic result matches, including across different
    /// row-fragment boundaries. A retry of a complete depth-fragment delta is
    /// also skipped when its tombstone projection matches and that order is
    /// not being replaced in this batch. Incomplete or changed waves remain
    /// conflicts. A changed paired result at that order fails with
    /// [`BranchMergeTombstoneHistoryError::AppendRetryMismatch`]. Standalone
    /// [`Self::append`] remains strict and rejects reused positions. When a
    /// retry plan carries tombstones, its exact projection must match the
    /// separately supplied delta even for unbound fragment waves. Legacy
    /// projection-only plans with no embedded tombstones remain supported.
    pub fn recover_depth_merge_fragments_with_appends(
        &mut self,
        recoveries: &[BranchMergeDepthFragmentRecovery],
        appends: &[SequencedBranchMergePlan],
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        if recoveries.is_empty() {
            return Err(BranchMergeTombstoneHistoryError::EmptyDepthFragmentRecoveryBatch);
        }

        let mut recoveries = recoveries.to_vec();
        recoveries.sort_unstable_by_key(|recovery| (recovery.order, recovery.fragment));
        for pair in recoveries.windows(2) {
            if pair[0].order == pair[1].order && pair[0].fragment == pair[1].fragment {
                return Err(BranchMergeTombstoneHistoryError::DuplicateDepthFragmentRecovery {
                    order: pair[0].order,
                    fragment: pair[0].fragment,
                });
            }
        }

        let mut candidate = self.clone();
        let mut appends = appends.to_vec();
        appends.sort_unstable_by_key(|step| step.order);
        for step in &appends {
            let replaces_same_order = recoveries.iter().any(|recovery| recovery.order == step.order);
            candidate.append_recovery_plan(step, replaces_same_order)?;
        }

        let mut released = Vec::new();
        for recovery in &recoveries {
            released.extend(candidate.recover_buffered_depth_fragment(recovery)?);
        }

        *self = candidate;
        Ok(released)
    }

    fn append_recovery_plan(
        &mut self,
        step: &SequencedBranchMergePlan,
        replaces_same_order: bool,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        if self.next_order.is_none() {
            return Err(BranchMergeTombstoneHistoryError::OrderExhausted);
        }

        let existing_mode = self.committed_modes.get(&step.order).copied()
            .or_else(|| {
                self.pending_deltas
                    .get(&step.order)
                    .map(BufferedBranchMergeTombstoneDelta::submission_mode)
            })
            .or_else(|| self.duplicate_retry_modes.get(&step.order).copied());
        if existing_mode == Some(BranchMergeTombstoneSubmissionMode::DepthFragments) {
            let plan_tombstones = step.plan.ordered_row_tombstones();
            let identity = self
                .pending_plan_identities
                .get(&step.order)
                .or_else(|| self.committed_plan_identities.get(&step.order));
            let projection_only_legacy_plan = identity.is_none() && plan_tombstones.is_empty();
            let paired_projection_matches = projection_only_legacy_plan
                || (!has_duplicate_tombstones_in_wave(&plan_tombstones)
                    && same_tombstone_encoding_delta(
                        &plan_tombstones,
                        &step.ordered_row_tombstones,
                    ));
            if !replaces_same_order
                && !has_duplicate_tombstones_in_wave(&step.ordered_row_tombstones)
                && paired_projection_matches
            {
                let existing = match self.pending_deltas.get(&step.order) {
                    Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                        fragment_count,
                        fragments,
                    }) if fragments.len() == *fragment_count => Some(
                        fragments.values().flatten().cloned().collect::<Vec<_>>(),
                    ),
                    _ if self.committed_modes.get(&step.order)
                        == Some(&BranchMergeTombstoneSubmissionMode::DepthFragments) => Some(
                            self.events.iter()
                                .filter(|event| event.order == step.order)
                                .map(|event| (event.table, event.key.clone()))
                                .collect::<Vec<_>>(),
                        ),
                    _ => None,
                };
                let tombstones_match = existing.as_ref().is_some_and(|existing| {
                    if identity.is_some() {
                        same_tombstone_encoding_delta(existing, &step.ordered_row_tombstones)
                    } else {
                        same_tombstone_delta(existing, &step.ordered_row_tombstones)
                    }
                });
                if tombstones_match
                    && identity.is_none_or(|identity| {
                        *identity == paired_plan_retry_identity(&step.plan)
                    })
                {
                    return Ok(());
                }
            }
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission {
                order: step.order,
            });
        }
        self.classify_submission_mode_conflict(
            step.order,
            BranchMergeTombstoneSubmissionMode::WholePlan,
        )?;
        if has_duplicate_tombstones_in_wave(&step.ordered_row_tombstones) {
            return Err(BranchMergeTombstoneHistoryError::DuplicateTombstone {
                order: step.order,
            });
        }

        if let Some(BufferedBranchMergeTombstoneDelta::WholePlan(existing)) =
            self.pending_deltas.get(&step.order)
        {
            let same_plan = self
                .pending_plan_identities
                .get(&step.order)
                .is_some_and(|identity| *identity == paired_plan_retry_identity(&step.plan));
            return if same_plan && same_tombstone_delta(existing, &step.ordered_row_tombstones) {
                Ok(())
            } else {
                Err(BranchMergeTombstoneHistoryError::AppendRetryMismatch {
                    order: step.order,
                })
            };
        }

        if self.committed_modes.get(&step.order)
            == Some(&BranchMergeTombstoneSubmissionMode::WholePlan)
        {
            let same_plan = self
                .committed_plan_identities
                .get(&step.order)
                .is_some_and(|identity| *identity == paired_plan_retry_identity(&step.plan));
            let committed = self.events.iter()
                .filter(|event| event.order == step.order)
                .map(|event| (event.table, event.key.clone()))
                .collect::<Vec<_>>();
            return if same_plan && same_tombstone_delta(&committed, &step.ordered_row_tombstones) {
                Ok(())
            } else {
                Err(BranchMergeTombstoneHistoryError::AppendRetryMismatch {
                    order: step.order,
                })
            };
        }

        self.append(step)
    }

    fn recover_buffered_depth_fragment(
        &mut self,
        recovery: &BranchMergeDepthFragmentRecovery,
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        self.classify_submission_position(
            recovery.order,
            BranchMergeTombstoneSubmissionMode::DepthFragments,
        )?;
        if recovery.fragment_count == 0 || recovery.fragment >= recovery.fragment_count {
            return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                fragment: recovery.fragment,
                fragment_count: recovery.fragment_count,
            });
        }

        let fragments = match self.pending_deltas.get(&recovery.order) {
            Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count,
                fragments,
            }) => {
                if *fragment_count != recovery.fragment_count {
                    return Err(BranchMergeTombstoneHistoryError::FragmentCountMismatch {
                        order: recovery.order,
                        expected: *fragment_count,
                        actual: recovery.fragment_count,
                    });
                }
                fragments
            }
            Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)) => unreachable!(
                "whole-plan mode conflicts are classified before fragment recovery"
            ),
            None => {
                return Err(BranchMergeTombstoneHistoryError::NoIncompleteDepthWave {
                    order: recovery.order,
                });
            }
        };

        let mut combined = fragments
            .iter()
            .filter(|(fragment, _)| **fragment != recovery.fragment)
            .flat_map(|(_, tombstones)| tombstones.iter().cloned())
            .collect::<Vec<_>>();
        combined.extend_from_slice(&recovery.tombstones);
        if has_duplicate_tombstones_in_wave(&combined) {
            return Err(BranchMergeTombstoneHistoryError::DuplicateTombstone {
                order: recovery.order,
            });
        }
        if self.pending_plan_identities.contains_key(&recovery.order) {
            let accepted = fragments.values().flatten().cloned().collect::<Vec<_>>();
            if !same_tombstone_encoding_delta(&accepted, &combined) {
                return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission {
                    order: recovery.order,
                });
            }
        }
        if let Some(other_order) = self.pending_duplicate_order(recovery.order, &combined) {
            return Err(concurrent_duplicate_error(recovery.order, other_order));
        }

        self.duplicate_retry_modes.remove(&recovery.order);
        let Some(BufferedBranchMergeTombstoneDelta::DepthFragments { fragments, .. }) =
            self.pending_deltas.get_mut(&recovery.order)
        else {
            unreachable!("the recovered depth wave remains buffered after validation")
        };
        fragments.insert(recovery.fragment, recovery.tombstones.clone());
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
            let mode = delta.submission_mode();
            self.duplicate_retry_modes.remove(&order);
            if let Some(identity) = self.pending_plan_identities.remove(&order) {
                self.committed_plan_identities.insert(order, identity);
            } else if mode == BranchMergeTombstoneSubmissionMode::WholePlan {
                unreachable!("whole-plan identity is buffered with its tombstone delta");
            }
            let mut tombstones = match delta {
                BufferedBranchMergeTombstoneDelta::WholePlan(tombstones) => tombstones,
                BufferedBranchMergeTombstoneDelta::DepthFragments { fragments, .. } => {
                    fragments.into_values().flatten().collect()
                }
            };
            self.committed_modes.insert(order, mode);
            tombstones.sort_by(|(left_table, left_key), (right_table, right_key)| {
                left_table.cmp(right_table).then_with(|| {
                    compare_primary_keys_with_encoding_tiebreak(left_key, right_key)
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
        self.classify_submission_mode_conflict(order, incoming_mode)?;
        if let Some(committed_mode) = self.committed_modes.get(&order) {
            debug_assert_eq!(*committed_mode, incoming_mode);
            return Err(BranchMergeTombstoneHistoryError::DuplicateOrStale { order });
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
                Some(BufferedBranchMergeTombstoneDelta::DepthFragments { .. }),
                BranchMergeTombstoneSubmissionMode::DepthFragments,
            )
            | (None, _) => Ok(()),
            (Some(_), _) => unreachable!(
                "mixed submission modes are rejected by the shared priority classifier"
            ),
        }
    }

    fn classify_submission_mode_conflict(
        &self,
        order: u64,
        incoming_mode: BranchMergeTombstoneSubmissionMode,
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let existing_mode = self
            .committed_modes
            .get(&order)
            .copied()
            .or_else(|| {
                self.pending_deltas
                    .get(&order)
                    .map(BufferedBranchMergeTombstoneDelta::submission_mode)
            })
            .or_else(|| self.duplicate_retry_modes.get(&order).copied());
        if existing_mode.is_some_and(|mode| mode != incoming_mode) {
            Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order })
        } else {
            Ok(())
        }
    }

    fn reserve_duplicate_retry_mode(
        &mut self,
        order: u64,
        mode: BranchMergeTombstoneSubmissionMode,
    ) {
        if !self.pending_deltas.contains_key(&order) && !self.committed_modes.contains_key(&order) {
            self.duplicate_retry_modes.entry(order).or_insert(mode);
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
    fn submission_mode(&self) -> BranchMergeTombstoneSubmissionMode {
        match self {
            Self::WholePlan(_) => BranchMergeTombstoneSubmissionMode::WholePlan,
            Self::DepthFragments { .. } => BranchMergeTombstoneSubmissionMode::DepthFragments,
        }
    }

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

/// Makes partial primary-key comparison deterministic for storage ordering.
///
/// The primary-key comparator deliberately rejects tuples with different
/// arities. Paired retries and fragment releases still need a total order so
/// segment layout cannot erase that depth distinction; v1 orders differing
/// top-level arities numerically, then uses canonical bytes when logical
/// comparison is equal or undefined within one arity.
fn compare_primary_keys_with_encoding_tiebreak(
    left: &CanonicalValue,
    right: &CanonicalValue,
) -> Ordering {
    let left_arity = primary_key_tuple_arity(left);
    let right_arity = primary_key_tuple_arity(right);
    if let (Some(left_arity), Some(right_arity)) = (left_arity, right_arity)
        && left_arity != right_arity
    {
        return left_arity.cmp(&right_arity);
    }

    match compare_primary_keys(left, right) {
        Ok(Ordering::Less) => Ordering::Less,
        Ok(Ordering::Greater) => Ordering::Greater,
        Ok(Ordering::Equal) | Err(_) => {
            let left = left
                .encode()
                .expect("validated canonical primary keys remain encodable");
            let right = right
                .encode()
                .expect("validated canonical primary keys remain encodable");
            left.cmp(&right)
        }
    }
}

fn primary_key_tuple_arity(value: &CanonicalValue) -> Option<usize> {
    match value.raw() {
        OvbRaw::Tag(60015, payload) => match payload.as_ref() {
            OvbRaw::Array(components) => Some(components.len()),
            _ => None,
        },
        OvbRaw::Array(components) => Some(components.len()),
        _ => Some(1),
    }
}

fn has_duplicate_tombstones_in_wave(tombstones: &[(ObjectId, CanonicalValue)]) -> bool {
    let mut ordered = tombstones.to_vec();
    ordered.sort_by(|(left_table, left_key), (right_table, right_key)| {
        left_table.cmp(right_table).then_with(|| {
            compare_primary_keys_with_encoding_tiebreak(left_key, right_key)
        })
    });
    ordered
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0 && same_primary_key(&pair[0].1, &pair[1].1))
}

fn same_tombstone_delta(
    left: &[(ObjectId, CanonicalValue)],
    right: &[(ObjectId, CanonicalValue)],
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(table, key)| {
            right.iter().any(|(other_table, other_key)| {
                table == other_table && same_primary_key(key, other_key)
            })
        })
}

fn same_tombstone_encoding_delta(
    left: &[(ObjectId, CanonicalValue)],
    right: &[(ObjectId, CanonicalValue)],
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(table, key)| {
            right
                .iter()
                .any(|(other_table, other_key)| table == other_table && key == other_key)
        })
}

struct RetryIdentityFormatter<'a>(&'a mut Sha256);

impl fmt::Write for RetryIdentityFormatter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0.update(value.as_bytes());
        Ok(())
    }
}

fn update_retry_identity_debug(hash: &mut Sha256, value: &impl fmt::Debug) {
    hash.update([0xf0]);
    {
        let mut formatter = RetryIdentityFormatter(hash);
        write!(&mut formatter, "{value:?}").expect("writing to the retry identity hash cannot fail");
    }
    hash.update([0]);
}

fn update_retry_identity_count(hash: &mut Sha256, count: usize) {
    hash.update(u64::try_from(count).unwrap_or(u64::MAX).to_be_bytes());
}

fn update_retry_identity_value(hash: &mut Sha256, value: &CanonicalValue) {
    let encoded = value
        .encode()
        .expect("validated canonical values remain encodable");
    update_retry_identity_count(hash, encoded.len());
    hash.update(encoded);
}

/// Fingerprints the logical paired plan output without retaining its row
/// payloads in the history. Row and tombstone segment boundaries are omitted;
/// schema/checkpoint state and reused immutable segments remain part of the
/// retry identity.
fn paired_plan_retry_identity(plan: &BranchMergePlan) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"orna-storage-paired-plan-retry-v1");
    update_retry_identity_debug(&mut hash, &plan.schema);
    update_retry_identity_debug(&mut hash, &plan.checkpoints);
    update_retry_identity_count(&mut hash, plan.tables.len());

    for (table_id, table) in &plan.tables {
        hash.update(table_id.bytes());
        hash.update(table.id.bytes());
        update_retry_identity_debug(&mut hash, &table.whole_table_reuse);

        let reused_segments = table
            .segments
            .iter()
            .filter_map(|segment| match segment {
                MergedSegment::Reuse { from, manifest } => Some((from, manifest)),
                MergedSegment::Rows { .. } => None,
            })
            .collect::<Vec<_>>();
        update_retry_identity_count(&mut hash, reused_segments.len());
        for reuse in reused_segments {
            update_retry_identity_debug(&mut hash, &reuse);
        }

        let mut rows = Vec::<&KeyedRow>::new();
        let mut tombstones = Vec::<(ObjectId, &CanonicalValue)>::new();
        for segment in &table.segments {
            if let MergedSegment::Rows {
                rows: segment_rows,
                tombstones: segment_tombstones,
                ..
            } = segment
            {
                rows.extend(segment_rows.iter());
                tombstones.extend(segment_tombstones.iter().map(|key| (table.id, key)));
            }
        }
        rows.sort_by(|left, right| {
            left.table.cmp(&right.table).then_with(|| {
                compare_primary_keys_with_encoding_tiebreak(&left.key, &right.key)
            })
        });
        update_retry_identity_count(&mut hash, rows.len());
        for row in rows {
            hash.update(row.table.bytes());
            hash.update([match row.key_kind {
                orna_evolution_v1::RowKeyKind::Explicit => 0,
                orna_evolution_v1::RowKeyKind::Automatic => 1,
            }]);
            update_retry_identity_value(&mut hash, &row.key);
            update_retry_identity_count(&mut hash, row.fields.len());
            for (field, value) in &row.fields {
                hash.update(field.bytes());
                update_retry_identity_value(&mut hash, value);
            }
        }

        tombstones.sort_by(|(left_table, left_key), (right_table, right_key)| {
            left_table.cmp(right_table).then_with(|| {
                compare_primary_keys_with_encoding_tiebreak(left_key, right_key)
            })
        });
        update_retry_identity_count(&mut hash, tombstones.len());
        for (table, key) in tombstones {
            hash.update(table.bytes());
            update_retry_identity_value(&mut hash, key);
        }
    }

    hash.finalize().into()
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
