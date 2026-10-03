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

/// One committed redo frame containing the paired log checkpoint snapshots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFrame {
    pub left: BTreeMap<CheckpointId, CheckpointGeneration>,
    pub right: BTreeMap<CheckpointId, CheckpointGeneration>,
}

/// One run of adjacent redo frames with the same paired checkpoint state.
///
/// `None` means that side has no checkpoint entry. A present checkpoint whose
/// `position` is `None` remains a distinct `Some(CheckpointGeneration)` value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoRunSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
}

/// The compressed redo chain for one stable checkpoint identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoRunSnapshot>,
}

/// The ordered write-ahead log identities associated with one paired redo frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedWriteAheadIdentity {
    pub left_log: Vec<u8>,
    pub right_log: Vec<u8>,
}

/// One paired redo frame with its source write-ahead identities attached.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoIdentityFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub write_ahead_identity: BranchMergePairedWriteAheadIdentity,
}

/// One compressed checkpoint run retaining every paired write-ahead identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoIdentityRunSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    /// One pair per input order, in ascending order; repeated IDs are retained.
    pub write_ahead_identities: Vec<BranchMergePairedWriteAheadIdentity>,
}

/// One checkpoint identity's compressed redo history and paired log lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoIdentityChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoIdentityRunSnapshot>,
}

/// One committed redo order projected onto a sparse checkpoint stream.
///
/// An absent side remains `None`, while the frame's write-ahead identity is
/// retained even when both checkpoint sides omit this stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSparseSlotSnapshot {
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub write_ahead_identity: BranchMergePairedWriteAheadIdentity,
}

/// A checkpoint stream's sparse state across the supplied paired redo frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSparseStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoSparseSlotSnapshot>,
}

/// One sparse checkpoint occurrence tagged with its input fold and frame order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    /// Committed redo order inside the identified fold.
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub write_ahead_identity: BranchMergePairedWriteAheadIdentity,
}

/// A checkpoint stream's ordered occurrences across a chain of sparse folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoSparseChainSlotSnapshot>,
}

/// Opaque directional identities for one paired checkpoint redo fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedRedoFoldIdentity {
    pub left_fold: Vec<u8>,
    pub right_fold: Vec<u8>,
}

/// One sparse checkpoint redo fold with its paired fold identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSparseFold {
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub frames: BTreeMap<u64, BranchMergePairedCheckpointRedoFrame>,
}

/// One sparse checkpoint occurrence retaining its paired redo-fold identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A checkpoint stream's paired redo-fold identity across sparse folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSparseChainSlotSnapshot>,
}

/// One occurrence from a source sparse checkpoint chain after merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseMergeChainSlotSnapshot {
    /// Zero-based position of the source chain supplied to the merge.
    pub merge_ordinal: usize,
    /// Position of the sparse fold inside that source chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A checkpoint stream merged across independent sparse redo-fold chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseMergeChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSparseMergeChainSlotSnapshot>,
}

/// One checkpoint pin bound to the checkpoint stream it names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointPinIdentity {
    pub checkpoint_id: CheckpointId,
    pub generation: CheckpointGeneration,
}

/// One directional segment incarnation pinned by a checkpoint generation.
///
/// The segment ID is copied from that side of the paired segment identity;
/// it is never inferred from a sibling pin or selected by generation order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointSegmentPinIdentity {
    pub checkpoint_id: CheckpointId,
    pub generation: CheckpointGeneration,
    pub segment_id: Vec<u8>,
}

/// One sparse occurrence retaining checkpoint-bound pins and its source pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparsePinMergeChainSlotSnapshot {
    /// Zero-based position of the source chain supplied to the merge.
    pub merge_ordinal: usize,
    /// Position of the sparse fold inside that source chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// Kept even when one or both checkpoint sides have no pin.
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// Checkpoint streams merged across sparse chains with segment pins bound to
/// both their checkpoint IDs and directional segment incarnations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparsePinMergeChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots:
        Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparsePinMergeChainSlotSnapshot>,
}

/// One sparse occurrence retaining each directional checkpoint pin explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparsePinMergeChainSlotSnapshot {
    /// Zero-based position of the source chain supplied to the merge.
    pub merge_ordinal: usize,
    /// Position of the sparse fold inside that source chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A checkpoint stream merged across sparse chains without detaching pin IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparsePinMergeChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSparsePinMergeChainSlotSnapshot>,
}

/// One occurrence from a source sparse checkpoint chain after merge, retaining
/// both the enclosing fold pair and the exact segment pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSlotSnapshot {
    /// Zero-based position of the source chain supplied to the merge.
    pub merge_ordinal: usize,
    /// Position of the sparse fold inside that source chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream merged across independent sparse fold/segment chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSlotSnapshot>,
}

/// A compacted merged state run retaining source, fold, and segment lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionRunSnapshot {
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// One exact directional segment pair for every order in the run.
    pub segment_identities: Vec<BranchMergePairedWriteAheadSegmentIdentity>,
}

/// A merged checkpoint stream compacted without losing segment rebinding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionRunSnapshot>,
}

/// A malformed compacted merged segment run cannot be restored losslessly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainRestoreError {
    InvalidOrderRange {
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    SegmentIdentityCountMismatch {
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One restored checkpoint/segment occurrence retaining its restore batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainSlotSnapshot {
    /// Zero-based position of the independent compacted restore batch.
    pub restore_ordinal: usize,
    /// Original source-chain position preserved by the compacted merge.
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// Kept intact even when one or both checkpoint pins are absent.
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// Restored checkpoint streams across independent sparse restore batches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots:
        Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainSlotSnapshot>,
}

/// A malformed compacted source run rejected during a multi-chain restore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainError {
    InvalidOrderRange {
        restore_ordinal: usize,
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    SegmentIdentityCountMismatch {
        restore_ordinal: usize,
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One compacted run retaining its independent handoff and fold coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRunSnapshot {
    /// Zero-based position of the source compacted chain supplied to handoff.
    pub handoff_ordinal: usize,
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A checkpoint stream handed off across independently compacted fold chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRunSnapshot>,
}

/// A compacted handoff run with a descending range cannot be restored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRestoreError {
    InvalidOrderRange {
        handoff_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
}

/// One restored occurrence retaining both restore-batch and handoff lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffSlotSnapshot {
    /// Zero-based position of the compacted restore batch supplied by caller.
    pub restore_ordinal: usize,
    /// Source handoff ordinal retained by the compacted chain.
    pub handoff_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A checkpoint stream restored across multiple sparse handoff batches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffSlotSnapshot>,
}

/// One restored occurrence retaining stream and source compaction positions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffSlotSnapshot {
    /// Zero-based position of the independent restore batch.
    pub restore_ordinal: usize,
    /// Zero-based source stream position within that batch.
    pub stream_ordinal: usize,
    /// Zero-based compacted run position within the source stream.
    pub compaction_ordinal: usize,
    /// Source compacted chain position retained by the handoff run.
    pub handoff_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A restored checkpoint stream with complete compaction handoff lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffSlotSnapshot>,
}

/// A malformed compacted handoff run rejected with its complete source path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        stream_ordinal: usize,
        compaction_ordinal: usize,
        handoff_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
}

/// One restored occurrence with each present side bound to its checkpoint pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseRestorePinHandoffSlotSnapshot {
    pub restore_ordinal: usize,
    pub handoff_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A restored checkpoint stream with paired, checkpoint-bound side pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseRestorePinHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSparseRestorePinHandoffSlotSnapshot>,
}

/// A malformed run rejected while restoring one compacted handoff batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
}

/// One restored occurrence retaining fold, atomic WAL, and handoff lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalRestoreHandoffSlotSnapshot {
    /// Zero-based position of the independent restore batch.
    pub restore_ordinal: usize,
    /// Zero-based position of this compacted source chain in the batch.
    pub handoff_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A checkpoint stream restored from sparse WAL handoff batches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldWalRestoreHandoffSlotSnapshot>,
}

/// A malformed WAL handoff run cannot be expanded without changing history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldWalRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    LogSegmentIdentityCountMismatch {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One restored WAL rotation occurrence retaining merge and restore lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffSlotSnapshot {
    /// Zero-based position of the independent restore batch.
    pub restore_ordinal: usize,
    /// Zero-based position of this source handoff in the restore batch.
    pub handoff_ordinal: usize,
    /// Original merge position retained by the compacted WAL run.
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// Atomic paired WAL IDs and their segment incarnations for this order.
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A checkpoint stream restored from sparse WAL merge handoffs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffSlotSnapshot>,
}

/// One directional checkpoint pin bound to its WAL and segment incarnation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointWalSegmentPinIdentity {
    pub checkpoint_id: CheckpointId,
    pub generation: CheckpointGeneration,
    pub write_ahead_id: Vec<u8>,
    pub segment_id: Vec<u8>,
}

/// One restored WAL compaction occurrence with checkpoint-scoped side pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalMergeRestorePinHandoffSlotSnapshot {
    pub restore_ordinal: usize,
    pub handoff_ordinal: usize,
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointWalSegmentPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointWalSegmentPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// Retains the original atomic pair even when a checkpoint side is absent.
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A checkpoint stream restored with its complete paired WAL pin lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalMergeRestorePinHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldWalMergeRestorePinHandoffSlotSnapshot>,
}

/// A malformed sparse WAL merge run cannot be restored without losing lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    LogSegmentIdentityCountMismatch {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One restored checkpoint occurrence retaining fold and segment rotations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffSlotSnapshot {
    /// Zero-based position of the independent restore batch.
    pub restore_ordinal: usize,
    /// Zero-based position of the compacted checkpoint chain in the batch.
    pub handoff_ordinal: usize,
    /// Zero-based position of this checkpoint stream within its handoff.
    pub stream_ordinal: usize,
    /// Zero-based compacted run position within the source stream.
    pub compaction_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream restored across sparse rotation handoffs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots:
        Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffSlotSnapshot>,
}

/// A malformed rotation handoff run cannot be restored without losing lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        stream_ordinal: usize,
        compaction_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    SegmentIdentityCountMismatch {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        stream_ordinal: usize,
        compaction_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One restored occurrence retaining source stream and compaction ordinals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffSlotSnapshot {
    /// Zero-based position of the independent restore batch.
    pub restore_ordinal: usize,
    /// Zero-based position of the source handoff in the restore batch.
    pub handoff_ordinal: usize,
    /// Zero-based source stream position within the handoff.
    pub stream_ordinal: usize,
    /// Zero-based compacted run position within the source stream.
    pub compaction_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream restored with its sparse rotation source coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffSlotSnapshot>,
}

/// One restored compacted segment occurrence with checkpoint-bound side pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestorePinHandoffSlotSnapshot {
    pub restore_ordinal: usize,
    pub handoff_ordinal: usize,
    pub stream_ordinal: usize,
    pub compaction_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// Retained even if one of the directional checkpoint pins is absent.
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A compacted checkpoint stream restored with complete paired pin lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestorePinHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestorePinHandoffSlotSnapshot>,
}

/// A malformed compacted rotation run cannot be expanded losslessly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        stream_ordinal: usize,
        compaction_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    SegmentIdentityCountMismatch {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        stream_ordinal: usize,
        compaction_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One restored merge occurrence retaining all segment-rotation coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffSlotSnapshot {
    /// Zero-based position of the independent restore batch.
    pub restore_ordinal: usize,
    /// Zero-based position of this compacted handoff in the restore batch.
    pub handoff_ordinal: usize,
    /// Original source-chain position retained by compaction.
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream restored from sparse merged segment-rotation chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots:
        Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffSlotSnapshot>,
}

/// A malformed compacted merge run cannot be restored without losing lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffError {
    InvalidOrderRange {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    SegmentIdentityCountMismatch {
        restore_ordinal: usize,
        handoff_ordinal: usize,
        merge_ordinal: usize,
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// One restored segment occurrence with both pins bound to their stream ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestorePinHandoffSlotSnapshot {
    pub restore_ordinal: usize,
    pub handoff_ordinal: usize,
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub order: u64,
    pub left_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub right_pin: Option<BranchMergePairedCheckpointSegmentPinIdentity>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// Retained even when a directional checkpoint pin is absent.
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream restored with source-scoped, directional segment pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestorePinHandoffStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<
        BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestorePinHandoffSlotSnapshot,
    >,
}

/// One compacted checkpoint run retaining its fold identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
}

/// A compacted checkpoint stream retaining paired redo-fold identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionRunSnapshot>,
}

/// A sparse redo fold carrying both fold provenance and per-frame segment IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationFold {
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub frames: BTreeMap<u64, BranchMergePairedCheckpointRedoSegmentFrame>,
}

/// One checkpoint slot retaining fold provenance and its exact segment pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream's sparse states and both identity layers across folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseChainSlotSnapshot>,
}

/// One compacted state run retaining fold identity and every segment rotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// One exact directional segment pair for every order in the run.
    pub segment_identities: Vec<BranchMergePairedWriteAheadSegmentIdentity>,
}

/// A sparse checkpoint stream compacted without losing either identity layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRunSnapshot>,
}

/// A malformed compacted fold/segment run cannot be restored losslessly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreError {
    InvalidOrderRange {
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    SegmentIdentityCountMismatch {
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// A sparse redo fold carrying fold provenance and each frame's atomic
/// write-ahead log/segment identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldLogSegmentIdentityFold {
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub frames: BTreeMap<u64, BranchMergePairedCheckpointRedoLogSegmentFrame>,
}

/// One sparse checkpoint slot retaining fold provenance and its exact paired
/// write-ahead log/segment identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A checkpoint stream's state and atomic write-ahead rotation lineage across
/// sparse redo folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseChainSlotSnapshot>,
}

/// One compacted state run retaining every atomic log/segment rotation pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// One exact atomic write-ahead log/segment pair for every run order.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
}

/// A sparse checkpoint stream compacted without losing fold or write-ahead
/// rotation identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionRunSnapshot>,
}

/// One compacted WAL rotation run retaining its source merge and fold labels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalSparseMergeChainCompactionRunSnapshot {
    pub merge_ordinal: usize,
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    /// One exact paired WAL/segment incarnation for every order in the run.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
}

/// A compacted sparse WAL stream retaining merge, fold, and rotation lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldWalSparseMergeChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldWalSparseMergeChainCompactionRunSnapshot>,
}

/// A malformed compacted fold/log-segment run cannot be restored losslessly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergePairedCheckpointRedoFoldLogSegmentIdentityRestoreError {
    InvalidOrderRange {
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
    },
    LogSegmentIdentityCountMismatch {
        fold_ordinal: usize,
        first_order: u64,
        last_order: u64,
        expected: u128,
        actual: usize,
    },
}

/// A sparse fold pairing fold identity with per-order redo and compaction lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldChainCompactionIdentityFold {
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub frames: BTreeMap<u64, BranchMergePairedCheckpointRedoChainCompactionIdentityFrame>,
}

/// One checkpoint slot preserving fold, redo-chain, log/segment, and compaction identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
    pub compaction_identity: Option<BranchMergePairedCompactionIdentity>,
}

/// A checkpoint stream's sparse compaction chain with its enclosing fold identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseChainSlotSnapshot>,
}

/// One compacted state run preserving fold and per-order compaction lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_fold_identity: BranchMergePairedRedoFoldIdentity,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    /// One paired log/segment lineage for each order in the run.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
    /// One optional compaction observation for each order in the run.
    pub compaction_identities: Vec<Option<BranchMergePairedCompactionIdentity>>,
}

/// A compacted sparse stream retaining fold identity across compaction omissions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionRunSnapshot>,
}

/// Opaque directional identities for the left and right undo chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedUndoChainIdentity {
    pub left_chain: Vec<u8>,
    pub right_chain: Vec<u8>,
}

/// A paired checkpoint redo frame carrying its paired undo-chain identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub undo_chain_identity: BranchMergePairedUndoChainIdentity,
}

/// One sparse stream slot retaining its frame's paired undo-chain identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoSparseSlotSnapshot {
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub undo_chain_identity: BranchMergePairedUndoChainIdentity,
}

/// A sparse checkpoint stream fold with exact paired undo-chain provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoSparseStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoUndoSparseSlotSnapshot>,
}

/// One sparse undo-chain slot tagged with its containing fold and order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub undo_chain_identity: BranchMergePairedUndoChainIdentity,
}

/// A checkpoint stream's paired undo lineage across sparse redo folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoUndoSparseChainSlotSnapshot>,
}

/// One compacted checkpoint run retaining every paired undo-chain rotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    /// One exact directional undo-chain pair for every order in the run.
    pub undo_chain_identities: Vec<BranchMergePairedUndoChainIdentity>,
}

/// A compacted checkpoint stream retaining paired undo rotations per fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionRunSnapshot>,
}

/// A paired checkpoint redo frame carrying both undo-chain and compaction lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoCompactionIdentityFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub undo_chain_identity: BranchMergePairedUndoChainIdentity,
    pub compaction_identity: BranchMergePairedCompactionIdentity,
}

/// One sparse slot retaining the frame's paired undo and compaction identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoCompactionSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub undo_chain_identity: BranchMergePairedUndoChainIdentity,
    pub compaction_identity: BranchMergePairedCompactionIdentity,
}

/// A checkpoint stream's paired undo and compaction lineage across sparse folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoUndoCompactionSparseChainSlotSnapshot>,
}

/// One compacted checkpoint run retaining both paired identities at every order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    /// One exact directional undo-chain pair for every order in the run.
    pub undo_chain_identities: Vec<BranchMergePairedUndoChainIdentity>,
    /// One exact directional compaction pair for every order in the run.
    pub compaction_identities: Vec<BranchMergePairedCompactionIdentity>,
}

/// A compacted checkpoint stream retaining undo and compaction rotations per fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionRunSnapshot>,
}

/// Opaque active write-ahead segment identities for both redo sides.
///
/// A segment token identifies one segment incarnation. A rotated segment gets
/// a new token, including when a physical segment label is reused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedWriteAheadSegmentIdentity {
    pub left_segment: Vec<u8>,
    pub right_segment: Vec<u8>,
}

/// The paired write-ahead log IDs bound to their active segment incarnations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedLogSegmentIdentity {
    pub write_ahead_identity: BranchMergePairedWriteAheadIdentity,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A paired redo frame with its log IDs atomically bound to segment IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoLogSegmentFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// One checkpoint slot retaining its atomic log/segment lineage across folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoLogSegmentSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    /// Committed redo order inside the identified fold.
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A checkpoint stream's complete paired lineage across sparse fold chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoLogSegmentSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoLogSegmentSparseChainSlotSnapshot>,
}

/// A paired checkpoint redo frame carrying the active write-ahead segments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// One sparse checkpoint slot retaining the active segment pair at its order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentSparseSlotSnapshot {
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A sparse checkpoint stream fold with per-order segment rotation lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentSparseStreamSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoSegmentSparseSlotSnapshot>,
}

/// One segment-bearing stream slot tagged with its sparse fold and frame order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentSparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    /// Committed redo order inside the identified fold.
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub segment_identity: BranchMergePairedWriteAheadSegmentIdentity,
}

/// A checkpoint stream's ordered segment lineage across sparse redo folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentSparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoSegmentSparseChainSlotSnapshot>,
}

/// One compacted run of equal checkpoint state within a single sparse fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    /// One exact active left/right segment pair for every order in the run.
    pub segment_identities: Vec<BranchMergePairedWriteAheadSegmentIdentity>,
}

/// A sparse checkpoint stream compacted without losing fold or rotation identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionRunSnapshot>,
}

/// One compressed run of adjacent equal checkpoint state with every segment pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentCompactionRunSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    /// One active left/right segment pair for each input order in this run.
    pub segment_identities: Vec<BranchMergePairedWriteAheadSegmentIdentity>,
}

/// One sparse checkpoint stream's compacted redo and segment rotation history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoSegmentCompactionChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoSegmentCompactionRunSnapshot>,
}

/// One sparse checkpoint stream's compacted log-to-segment identity history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoLogSegmentCompactionChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoLogSegmentCompactionRunSnapshot>,
}

/// A compacted checkpoint state run retaining each order's paired log/segment identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoLogSegmentCompactionRunSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    /// One atomically bound left/right log and segment pair per input order.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
}

/// Opaque redo-chain identities for the two sides of a paired recovery fold.
///
/// Chain identities are opaque incarnation labels. Matching bytes identify
/// the same paired chain; callers should not infer ordering from their values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedRedoChainIdentity {
    pub left_chain: Vec<u8>,
    pub right_chain: Vec<u8>,
}

/// A paired checkpoint redo frame bound to both its redo chain and log segment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentityFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A compacted state run retaining its stable chain identity and per-order
/// paired log/segment lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentityRunSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    /// One atomically bound left/right log and segment pair per input order.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
}

/// One sparse checkpoint stream's compacted paired redo-chain history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentityCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoChainIdentityRunSnapshot>,
}

/// One sparse checkpoint slot retaining its paired redo chain and fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentitySparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
}

/// A checkpoint stream's paired redo-chain identity across sparse folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoChainIdentitySparseChainSlotSnapshot>,
}

/// One compacted run retaining fold, redo-chain, and per-order log lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    /// One atomically bound paired log/segment identity for each run order.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
}

/// A sparse checkpoint stream compacted without losing paired fold identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionRunSnapshot>,
}

/// Opaque compaction identities for the left and right redo histories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCompactionIdentity {
    pub left_compaction: Vec<u8>,
    pub right_compaction: Vec<u8>,
}

/// A paired redo-chain frame carrying the compaction pair that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoCompactionIdentityFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
    pub compaction_identity: BranchMergePairedCompactionIdentity,
}

/// A compacted checkpoint run retaining per-order compaction and log lineage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoCompactionIdentityRunSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    /// One paired compaction identity for each order in the run.
    pub compaction_identities: Vec<BranchMergePairedCompactionIdentity>,
    /// One paired log/segment identity for each order in the run.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
}

/// One sparse checkpoint stream's compacted redo and compaction identity view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoCompactionIdentitySnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoCompactionIdentityRunSnapshot>,
}

/// A paired redo-chain frame whose write-ahead compaction observation may be absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainCompactionIdentityFrame {
    pub checkpoints: BranchMergePairedCheckpointRedoFrame,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
    /// `None` records a frame without a corresponding compaction observation.
    pub compaction_identity: Option<BranchMergePairedCompactionIdentity>,
}

/// One sparse slot retaining redo-chain lineage and optional write-ahead compaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainCompactionIdentitySparseChainSlotSnapshot {
    /// Zero-based position of the sparse fold in the caller's chain.
    pub fold_ordinal: usize,
    pub order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    pub log_segment_identity: BranchMergePairedLogSegmentIdentity,
    pub compaction_identity: Option<BranchMergePairedCompactionIdentity>,
}

/// A sparse checkpoint stream retaining redo-chain folds and compaction omissions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainSnapshot {
    pub checkpoint_id: CheckpointId,
    pub slots: Vec<BranchMergePairedCheckpointRedoChainCompactionIdentitySparseChainSlotSnapshot>,
}

/// One compacted run retaining chain identity and optional lineage per order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionRunSnapshot {
    /// Zero-based position of the sparse fold containing this run.
    pub fold_ordinal: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub left: Option<CheckpointGeneration>,
    pub right: Option<CheckpointGeneration>,
    pub redo_chain_identity: BranchMergePairedRedoChainIdentity,
    /// One paired log/segment lineage for each order in the run.
    pub log_segment_identities: Vec<BranchMergePairedLogSegmentIdentity>,
    /// One optional compaction observation for each order in the run.
    pub compaction_identities: Vec<Option<BranchMergePairedCompactionIdentity>>,
}

/// A compacted sparse stream retaining redo-chain identity across omissions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionSnapshot {
    pub checkpoint_id: CheckpointId,
    pub runs: Vec<BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionRunSnapshot>,
}

/// Compresses adjacent paired redo frames only when both sides retain exactly
/// the same checkpoint state for an identity.
///
/// Frame keys are committed transaction orders. Runs merge only across
/// consecutive integer orders with equal full generations and positions on
/// both sides. Checkpoint IDs and position bytes remain opaque; missing map
/// entries are distinct from present checkpoints with `position: None`. A gap
/// or a change on either side starts a new run, so no intervening redo state
/// is inferred or discarded.
pub fn compress_paired_checkpoint_redo_chain(
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoFrame>,
) -> Vec<BranchMergePairedCheckpointRedoChainSnapshot> {
    let checkpoint_ids = frames
        .values()
        .flat_map(|frame| frame.left.keys().chain(frame.right.keys()).cloned())
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs = Vec::<BranchMergePairedCheckpointRedoRunSnapshot>::new();
            for (&order, frame) in frames {
                let left = frame.left.get(&checkpoint_id).cloned();
                let right = frame.right.get(&checkpoint_id).cloned();
                if let Some(last) = runs.last_mut()
                    && last.left == left
                    && last.right == right
                    && last.last_order.checked_add(1) == Some(order)
                {
                    last.last_order = order;
                } else {
                    runs.push(BranchMergePairedCheckpointRedoRunSnapshot {
                        first_order: order,
                        last_order: order,
                        left,
                        right,
                    });
                }
            }
            BranchMergePairedCheckpointRedoChainSnapshot { checkpoint_id, runs }
        })
        .collect()
}

/// Compresses paired checkpoint redo states while retaining each source log
/// identity in the order in which it was committed.
///
/// Checkpoint state uses the same conservative coalescing rule as
/// [`compress_paired_checkpoint_redo_chain`]. Different write-ahead identity
/// pairs do not prevent state compression: every original ordered pair is
/// copied into the compressed run, including repeated identities. This keeps
/// the exact left/right log provenance available after a chain is compacted.
pub fn compress_paired_checkpoint_redo_chain_preserving_write_ahead_identity(
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoIdentityFrame>,
) -> Vec<BranchMergePairedCheckpointRedoIdentityChainSnapshot> {
    let checkpoint_ids = frames
        .values()
        .flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        })
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs = Vec::<BranchMergePairedCheckpointRedoIdentityRunSnapshot>::new();
            for (&order, frame) in frames {
                let left = frame.checkpoints.left.get(&checkpoint_id).cloned();
                let right = frame.checkpoints.right.get(&checkpoint_id).cloned();
                if let Some(last) = runs.last_mut()
                    && last.left == left
                    && last.right == right
                    && last.last_order.checked_add(1) == Some(order)
                {
                    last.last_order = order;
                    last.write_ahead_identities
                        .push(frame.write_ahead_identity.clone());
                } else {
                    runs.push(BranchMergePairedCheckpointRedoIdentityRunSnapshot {
                        first_order: order,
                        last_order: order,
                        left,
                        right,
                        write_ahead_identities: vec![frame.write_ahead_identity.clone()],
                    });
                }
            }
            BranchMergePairedCheckpointRedoIdentityChainSnapshot { checkpoint_id, runs }
        })
        .collect()
}

/// Folds sparse paired redo frames into per-stream slots without losing stream
/// identity at omission boundaries.
///
/// `known_checkpoint_ids` lets a caller carry checkpoint identities forward
/// even when a stream is omitted from every frame in this fold. Identities
/// observed in a frame are also included, so an incomplete catalog never
/// drops checkpoint data. Output stream IDs and frame orders are sorted. Each
/// supplied frame contributes exactly one slot per stream; a missing side is
/// `None`, and no absent integer orders are invented. Paired write-ahead IDs
/// stay attached to every slot, including slots where both sides omit a
/// checkpoint. IDs, generations, positions, and log identities remain opaque.
pub fn fold_paired_checkpoint_redo_sparse_streams(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoIdentityFrame>,
) -> Vec<BranchMergePairedCheckpointRedoSparseStreamSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let slots = frames
                .iter()
                .map(|(&order, frame)| BranchMergePairedCheckpointRedoSparseSlotSnapshot {
                    order,
                    left: frame.checkpoints.left.get(&checkpoint_id).cloned(),
                    right: frame.checkpoints.right.get(&checkpoint_id).cloned(),
                    write_ahead_identity: frame.write_ahead_identity.clone(),
                })
                .collect();
            BranchMergePairedCheckpointRedoSparseStreamSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Chains sparse checkpoint folds without detaching paired write-ahead IDs.
///
/// The output checkpoint catalog is the union of the caller's IDs and all IDs
/// observed in every fold. Each supplied frame produces one slot per stream.
/// Slots retain the caller's fold order and the original order within that
/// fold, so equal redo order numbers in different folds remain distinct. The
/// paired write-ahead identity is copied to every slot, including total stream
/// omissions; repeated identity pairs and gaps are retained without
/// deduplication or synthesis.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_write_ahead_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoIdentityFrame>],
) -> Vec<BranchMergePairedCheckpointRedoSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame.checkpoints.left.get(stream_checkpoint_id).cloned(),
                            right: frame.checkpoints.right.get(stream_checkpoint_id).cloned(),
                            write_ahead_identity: frame.write_ahead_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Folds sparse checkpoint redo histories while retaining each fold's pair.
///
/// The checkpoint catalog unions known IDs with IDs observed in any fold.
/// Every supplied frame contributes one slot per output stream, even when
/// that stream is absent on both sides. Each slot carries the exact fold pair;
/// `(fold_ordinal, order)` remains the occurrence key when orders or fold IDs
/// repeat. No missing frame is synthesized.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_identity(
    known_checkpoint_ids: &[CheckpointId],
    folds: &[BranchMergePairedCheckpointRedoSparseFold],
) -> Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(folds.iter().flat_map(|fold| {
            fold.frames.values().flat_map(|frame| {
                frame.left.keys().chain(frame.right.keys()).cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = folds
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, fold)| {
                    fold.frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoFoldSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame.left.get(stream_checkpoint_id).cloned(),
                            right: frame.right.get(stream_checkpoint_id).cloned(),
                            redo_fold_identity: fold.redo_fold_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Merges already-folded sparse checkpoint chains without discarding lineage.
///
/// The checkpoint catalog unions known IDs with all source stream IDs. Each
/// source slot is copied once and tagged with its source-chain ordinal, so
/// equal local `(fold_ordinal, order)` coordinates remain distinguishable
/// across chains. Source order, fold identity, checkpoint values, omissions,
/// and sparse gaps are retained; no frames or states are synthesized.
pub fn merge_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_identity(
    known_checkpoint_ids: &[CheckpointId],
    merge_chains: &[Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot>],
) -> Vec<BranchMergePairedCheckpointRedoFoldSparseMergeChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(
            merge_chains
                .iter()
                .flat_map(|chain| chain.iter().map(|stream| stream.checkpoint_id.clone())),
        )
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut slots = merge_chains
                .iter()
                .enumerate()
                .flat_map(|(merge_ordinal, chain)| {
                    chain
                        .iter()
                        .filter(|stream| stream.checkpoint_id == checkpoint_id)
                        .flat_map(move |stream| {
                            stream.slots.iter().map(move |slot| {
                                BranchMergePairedCheckpointRedoFoldSparseMergeChainSlotSnapshot {
                                    merge_ordinal,
                                    fold_ordinal: slot.fold_ordinal,
                                    order: slot.order,
                                    left: slot.left.clone(),
                                    right: slot.right.clone(),
                                    redo_fold_identity: slot.redo_fold_identity.clone(),
                                }
                            })
                        })
                })
                .collect::<Vec<_>>();
            slots.sort_by_key(|slot| (slot.merge_ordinal, slot.fold_ordinal, slot.order));
            BranchMergePairedCheckpointRedoFoldSparseMergeChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Merges sparse checkpoint pin chains while binding each pin to its ID.
///
/// The catalog unions known IDs with IDs observed in any source chain. Every
/// occurrence keeps its source-chain ordinal, fold ordinal, order, and paired
/// redo-fold identity. Each present side becomes an explicit `(checkpoint_id,
/// generation)` pin; absent sides and sparse gaps stay absent. This prevents
/// a pin copied from one checkpoint stream from being relabeled by a sibling
/// stream during a three-way or wider merge.
pub fn merge_paired_checkpoint_redo_sparse_stream_chains_preserving_checkpoint_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    merge_chains: &[Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot>],
) -> Vec<BranchMergePairedCheckpointRedoFoldSparsePinMergeChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(
            merge_chains
                .iter()
                .flat_map(|chain| chain.iter().map(|stream| stream.checkpoint_id.clone())),
        )
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut slots = Vec::new();
            for (merge_ordinal, chain) in merge_chains.iter().enumerate() {
                for stream in chain
                    .iter()
                    .filter(|stream| stream.checkpoint_id == checkpoint_id)
                {
                    for slot in &stream.slots {
                        let pin = |generation: &Option<CheckpointGeneration>| {
                            generation.clone().map(|generation| {
                                BranchMergePairedCheckpointPinIdentity {
                                    checkpoint_id: checkpoint_id.clone(),
                                    generation,
                                }
                            })
                        };
                        slots.push(
                            BranchMergePairedCheckpointRedoFoldSparsePinMergeChainSlotSnapshot {
                                merge_ordinal,
                                fold_ordinal: slot.fold_ordinal,
                                order: slot.order,
                                left_pin: pin(&slot.left),
                                right_pin: pin(&slot.right),
                                redo_fold_identity: slot.redo_fold_identity.clone(),
                            },
                        );
                    }
                }
            }
            slots.sort_by_key(|slot| (slot.merge_ordinal, slot.fold_ordinal, slot.order));
            BranchMergePairedCheckpointRedoFoldSparsePinMergeChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Merges sparse checkpoint chains while retaining fold and segment identity.
///
/// The output catalog unions known IDs with every source stream ID. Each
/// source occurrence is copied once and tagged with its source-chain ordinal;
/// equal local `(fold_ordinal, order)` coordinates remain distinct. Fold
/// identity and the exact directional segment pair travel together through
/// omissions and rotations. The reference does not define this local chained
/// projection, so source-chain order is the stable provenance rule.
pub fn merge_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_segment_rotation_identity(
    known_checkpoint_ids: &[CheckpointId],
    merge_chains: &[Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot>],
) -> Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(
            merge_chains
                .iter()
                .flat_map(|chain| chain.iter().map(|stream| stream.checkpoint_id.clone())),
        )
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut slots = merge_chains
                .iter()
                .enumerate()
                .flat_map(|(merge_ordinal, chain)| {
                    chain
                        .iter()
                        .filter(|stream| stream.checkpoint_id == checkpoint_id)
                        .flat_map(move |stream| {
                            stream.slots.iter().map(move |slot| {
                                BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSlotSnapshot {
                                    merge_ordinal,
                                    fold_ordinal: slot.fold_ordinal,
                                    order: slot.order,
                                    left: slot.left.clone(),
                                    right: slot.right.clone(),
                                    redo_fold_identity: slot.redo_fold_identity.clone(),
                                    segment_identity: slot.segment_identity.clone(),
                                }
                            })
                        })
                })
                .collect::<Vec<_>>();
            slots.sort_by_key(|slot| (slot.merge_ordinal, slot.fold_ordinal, slot.order));
            BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Merges sparse segment-pin chains while binding every directional segment
/// to the checkpoint generation that pins it.
///
/// The output catalog unions known IDs with IDs observed in any source chain.
/// Source-chain, fold, and order coordinates remain distinct; no generation
/// comparison chooses a winner. A side's pin is present only when that source
/// occurrence has a checkpoint generation, and it carries the same stream ID
/// plus that side's exact segment ID. The paired segment identity is retained
/// even when one or both pins are omitted. This stable provenance rule defines
/// the local sparse projection where the reference is silent.
pub fn merge_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_segment_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    merge_chains: &[Vec<
        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot,
    >],
) -> Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparsePinMergeChainSnapshot> {
    merge_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_segment_rotation_identity(
        known_checkpoint_ids,
        merge_chains,
    )
    .into_iter()
    .map(|stream| {
        let checkpoint_id = stream.checkpoint_id;
        let slots = stream
            .slots
            .into_iter()
            .map(|slot| {
                let left_pin = slot.left.clone().map(|generation| {
                    BranchMergePairedCheckpointSegmentPinIdentity {
                        checkpoint_id: checkpoint_id.clone(),
                        generation,
                        segment_id: slot.segment_identity.left_segment.clone(),
                    }
                });
                let right_pin = slot.right.clone().map(|generation| {
                    BranchMergePairedCheckpointSegmentPinIdentity {
                        checkpoint_id: checkpoint_id.clone(),
                        generation,
                        segment_id: slot.segment_identity.right_segment.clone(),
                    }
                });
                BranchMergePairedCheckpointRedoFoldSegmentRotationSparsePinMergeChainSlotSnapshot {
                    merge_ordinal: slot.merge_ordinal,
                    fold_ordinal: slot.fold_ordinal,
                    order: slot.order,
                    left_pin,
                    right_pin,
                    redo_fold_identity: slot.redo_fold_identity,
                    segment_identity: slot.segment_identity,
                }
            })
            .collect();
        BranchMergePairedCheckpointRedoFoldSegmentRotationSparsePinMergeChainSnapshot {
            checkpoint_id,
            slots,
        }
    })
    .collect()
}

/// Compacts merged segment chains without erasing any directional rotation.
///
/// A run covers only adjacent orders from one source chain and one fold whose
/// checkpoint values and fold pair are equal. Every order's segment pair is
/// retained in sequence, so a rotation never disappears during compaction.
pub fn compress_paired_checkpoint_redo_sparse_merge_chains_preserving_fold_and_segment_rotation_identity(
    streams: &[BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.merge_ordinal == slot.merge_ordinal
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_fold_identity == slot.redo_fold_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.segment_identities.push(slot.segment_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionRunSnapshot {
                            merge_ordinal: slot.merge_ordinal,
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_fold_identity: slot.redo_fold_identity.clone(),
                            segment_identities: vec![slot.segment_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Restores each merged segment occurrence from compacted identity runs.
///
/// A run must provide exactly one directional segment pair for every order in
/// its range. Invalid ranges and missing or extra pairs are rejected rather
/// than inferred. Restored slots use source-chain, fold, and order order.
pub fn restore_paired_checkpoint_redo_sparse_merge_chains_preserving_fold_and_segment_rotation_identity(
    compacted: &[BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainRestoreError,
> {
    compacted
        .iter()
        .map(|stream| {
            let mut slots = Vec::new();
            for run in &stream.runs {
                if run.last_order < run.first_order {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainRestoreError::InvalidOrderRange {
                            merge_ordinal: run.merge_ordinal,
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                        },
                    );
                }
                let expected = u128::from(run.last_order) - u128::from(run.first_order) + 1;
                if expected != run.segment_identities.len() as u128 {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainRestoreError::SegmentIdentityCountMismatch {
                            merge_ordinal: run.merge_ordinal,
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                            expected,
                            actual: run.segment_identities.len(),
                        },
                    );
                }
                slots.extend(run.segment_identities.iter().enumerate().map(
                    |(offset, segment_identity)| {
                        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSlotSnapshot {
                            merge_ordinal: run.merge_ordinal,
                            fold_ordinal: run.fold_ordinal,
                            order: run.first_order + offset as u64,
                            left: run.left.clone(),
                            right: run.right.clone(),
                            redo_fold_identity: run.redo_fold_identity.clone(),
                            segment_identity: segment_identity.clone(),
                        }
                    },
                ));
            }
            slots.sort_by_key(|slot| (slot.merge_ordinal, slot.fold_ordinal, slot.order));
            Ok(BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                slots,
            })
        })
        .collect()
}

/// Restores independent sparse checkpoint chains without collapsing their
/// restore-batch, original merge-source, fold, or order identities.
///
/// Each outer input is one compacted restore batch. Its streams are expanded
/// with the existing lossless run validator, then projected into a catalog
/// formed from known and restored checkpoint IDs. Each present side regains a
/// pin containing that stream's ID, checkpoint generation, and exact
/// directional segment ID. Missing streams, sparse gaps, and omitted sides
/// remain absent; malformed ranges or segment counts report both restore and
/// source coordinates. The reference does not define identity across
/// independent restore batches, so restore ordinal is the stable extra
/// provenance scope.
pub fn restore_paired_checkpoint_redo_sparse_merge_chains_preserving_fold_and_segment_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_chains: &[Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainError,
> {
    let mut restored_chains = Vec::with_capacity(restore_chains.len());
    for (restore_ordinal, compacted) in restore_chains.iter().enumerate() {
        let restored =
            restore_paired_checkpoint_redo_sparse_merge_chains_preserving_fold_and_segment_rotation_identity(
                compacted,
            )
            .map_err(|error| match error {
                BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainRestoreError::InvalidOrderRange {
                    merge_ordinal,
                    fold_ordinal,
                    first_order,
                    last_order,
                } => BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainError::InvalidOrderRange {
                    restore_ordinal,
                    merge_ordinal,
                    fold_ordinal,
                    first_order,
                    last_order,
                },
                BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainRestoreError::SegmentIdentityCountMismatch {
                    merge_ordinal,
                    fold_ordinal,
                    first_order,
                    last_order,
                    expected,
                    actual,
                } => BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainError::SegmentIdentityCountMismatch {
                    restore_ordinal,
                    merge_ordinal,
                    fold_ordinal,
                    first_order,
                    last_order,
                    expected,
                    actual,
                },
            })?;
        restored_chains.push(restored);
    }

    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restored_chains.iter().flat_map(|batch| {
            batch.iter().map(|stream| stream.checkpoint_id.clone())
        }))
        .collect::<BTreeSet<_>>();

    Ok(checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut slots = Vec::new();
            for (restore_ordinal, batch) in restored_chains.iter().enumerate() {
                for stream in batch
                    .iter()
                    .filter(|stream| stream.checkpoint_id == checkpoint_id)
                {
                    for slot in &stream.slots {
                        let pin = |generation: &Option<CheckpointGeneration>,
                                   segment_id: &[u8]| {
                            generation.clone().map(|generation| {
                                BranchMergePairedCheckpointSegmentPinIdentity {
                                    checkpoint_id: checkpoint_id.clone(),
                                    generation,
                                    segment_id: segment_id.to_vec(),
                                }
                            })
                        };
                        slots.push(
                            BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainSlotSnapshot {
                                restore_ordinal,
                                merge_ordinal: slot.merge_ordinal,
                                fold_ordinal: slot.fold_ordinal,
                                order: slot.order,
                                left_pin: pin(
                                    &slot.left,
                                    &slot.segment_identity.left_segment,
                                ),
                                right_pin: pin(
                                    &slot.right,
                                    &slot.segment_identity.right_segment,
                                ),
                                redo_fold_identity: slot.redo_fold_identity.clone(),
                                segment_identity: slot.segment_identity.clone(),
                            },
                        );
                    }
                }
            }
            slots.sort_by_key(|slot| {
                (
                    slot.restore_ordinal,
                    slot.merge_ordinal,
                    slot.fold_ordinal,
                    slot.order,
                )
            });
            BranchMergePairedCheckpointRedoFoldSegmentRotationSparseRestoreChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect())
}

/// Hands off compacted sparse redo-fold chains while preserving their lineage.
///
/// Each compacted source chain is kept as an independent coordinate space.
/// Runs are copied verbatim and tagged with the source handoff ordinal, so
/// overlapping fold/order ranges cannot erase one another. Known and observed
/// checkpoint IDs are unioned; no runs or checkpoint values are synthesized.
pub fn merge_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_fold_identity(
    known_checkpoint_ids: &[CheckpointId],
    handoff_chains: &[Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionSnapshot>],
) -> Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(handoff_chains.iter().flat_map(|chain| {
            chain.iter().map(|stream| stream.checkpoint_id.clone())
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs = handoff_chains
                .iter()
                .enumerate()
                .flat_map(|(handoff_ordinal, chain)| {
                    chain
                        .iter()
                        .filter(|stream| stream.checkpoint_id == checkpoint_id)
                        .flat_map(move |stream| {
                            stream.runs.iter().map(move |run| {
                                BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRunSnapshot {
                                    handoff_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                    left: run.left.clone(),
                                    right: run.right.clone(),
                                    redo_fold_identity: run.redo_fold_identity.clone(),
                                }
                            })
                        })
                })
                .collect::<Vec<_>>();
            runs.sort_by_key(|run| {
                (run.handoff_ordinal, run.fold_ordinal, run.first_order)
            });
            BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot {
                checkpoint_id,
                runs,
            }
        })
        .collect()
}

/// Restores compacted handoff chains to their sparse per-order occurrences.
///
/// Each represented order gets its run's exact paired fold identity and
/// checkpoint values. Handoff and fold ordinals remain separate, and gaps
/// between runs remain absent. Descending ranges are rejected rather than
/// silently dropping a handoff's history.
pub fn restore_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_fold_identity(
    compacted: &[BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSparseMergeChainSnapshot>,
    BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRestoreError,
> {
    compacted
        .iter()
        .map(|stream| {
            let mut slots = Vec::new();
            for run in &stream.runs {
                if run.last_order < run.first_order {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRestoreError::InvalidOrderRange {
                            handoff_ordinal: run.handoff_ordinal,
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                        },
                    );
                }
                slots.extend((run.first_order..=run.last_order).map(|order| {
                    BranchMergePairedCheckpointRedoFoldSparseMergeChainSlotSnapshot {
                        merge_ordinal: run.handoff_ordinal,
                        fold_ordinal: run.fold_ordinal,
                        order,
                        left: run.left.clone(),
                        right: run.right.clone(),
                        redo_fold_identity: run.redo_fold_identity.clone(),
                    }
                }));
            }
            slots.sort_by_key(|slot| (slot.merge_ordinal, slot.fold_ordinal, slot.order));
            Ok(BranchMergePairedCheckpointRedoFoldSparseMergeChainSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                slots,
            })
        })
        .collect()
}

/// Restores multiple compacted handoff chains without colliding local labels.
///
/// Each input vector is one independent restore batch. Restored occurrences
/// retain both that batch's ordinal and the original handoff ordinal, followed
/// by their fold and order coordinates. The checkpoint catalog unions known
/// IDs with all restored streams; absent streams and sparse gaps remain absent.
pub fn restore_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_restore_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffError,
> {
    let mut restored_handoffs = Vec::with_capacity(restore_handoffs.len());
    for (restore_ordinal, compacted) in restore_handoffs.iter().enumerate() {
        let restored =
            restore_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_fold_identity(
                compacted,
            )
            .map_err(|error| match error {
                BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffRestoreError::InvalidOrderRange {
                    handoff_ordinal,
                    fold_ordinal,
                    first_order,
                    last_order,
                } => BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffError::InvalidOrderRange {
                    restore_ordinal,
                    handoff_ordinal,
                    fold_ordinal,
                    first_order,
                    last_order,
                },
            })?;
        restored_handoffs.push(restored);
    }

    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restored_handoffs.iter().flat_map(|batch| {
            batch.iter().map(|stream| stream.checkpoint_id.clone())
        }))
        .collect::<BTreeSet<_>>();

    Ok(checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut slots = restored_handoffs
                .iter()
                .enumerate()
                .flat_map(|(restore_ordinal, batch)| {
                    batch
                        .iter()
                        .filter(|stream| stream.checkpoint_id == checkpoint_id)
                        .flat_map(move |stream| {
                            stream.slots.iter().map(move |slot| {
                                BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffSlotSnapshot {
                                    restore_ordinal,
                                    handoff_ordinal: slot.merge_ordinal,
                                    fold_ordinal: slot.fold_ordinal,
                                    order: slot.order,
                                    left: slot.left.clone(),
                                    right: slot.right.clone(),
                                    redo_fold_identity: slot.redo_fold_identity.clone(),
                                }
                            })
                        })
                })
                .collect::<Vec<_>>();
            slots.sort_by_key(|slot| {
                (
                    slot.restore_ordinal,
                    slot.handoff_ordinal,
                    slot.fold_ordinal,
                    slot.order,
                )
            });
            BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffStreamSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect())
}

/// Restores compacted checkpoint handoffs without flattening source streams.
///
/// Each outer input is one independent restore batch. Within a batch, each
/// checkpoint stream and each compacted run retain their input ordinals; each
/// run also retains its original handoff and fold labels. Restored occurrences
/// therefore remain distinct even when checkpoint, handoff, fold, and order
/// labels overlap. The known and observed checkpoint IDs are unioned in sorted
/// order. Descending ranges are rejected with their full source path; sparse
/// gaps, absent streams, and omitted checkpoint sides remain absent. The
/// reference is silent on duplicate-stream/run ordering, so input order is
/// the stable local policy.
pub fn restore_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_source_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffError,
> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restore_handoffs.iter().flat_map(|batch| {
            batch.iter().map(|stream| stream.checkpoint_id.clone())
        }))
        .collect::<BTreeSet<_>>();

    let mut restored = Vec::with_capacity(checkpoint_ids.len());
    for checkpoint_id in checkpoint_ids {
        let mut slots = Vec::new();
        for (restore_ordinal, batch) in restore_handoffs.iter().enumerate() {
            for (stream_ordinal, stream) in batch.iter().enumerate() {
                if stream.checkpoint_id != checkpoint_id {
                    continue;
                }
                for (compaction_ordinal, run) in stream.runs.iter().enumerate() {
                    if run.last_order < run.first_order {
                        return Err(
                            BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffError::InvalidOrderRange {
                                restore_ordinal,
                                stream_ordinal,
                                compaction_ordinal,
                                handoff_ordinal: run.handoff_ordinal,
                                fold_ordinal: run.fold_ordinal,
                                first_order: run.first_order,
                                last_order: run.last_order,
                            },
                        );
                    }
                    slots.extend((run.first_order..=run.last_order).map(|order| {
                        BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffSlotSnapshot {
                            restore_ordinal,
                            stream_ordinal,
                            compaction_ordinal,
                            handoff_ordinal: run.handoff_ordinal,
                            fold_ordinal: run.fold_ordinal,
                            order,
                            left: run.left.clone(),
                            right: run.right.clone(),
                            redo_fold_identity: run.redo_fold_identity.clone(),
                        }
                    }));
                }
            }
        }
        slots.sort_by_key(|slot| {
            (
                slot.restore_ordinal,
                slot.stream_ordinal,
                slot.compaction_ordinal,
                slot.handoff_ordinal,
                slot.fold_ordinal,
                slot.order,
            )
        });
        restored.push(
            BranchMergePairedCheckpointRedoFoldSparseCompactionRestoreHandoffStreamSnapshot {
                checkpoint_id,
                slots,
            },
        );
    }
    Ok(restored)
}

/// Restores sparse compaction handoffs with paired checkpoint-pin identity.
///
/// Each present side keeps its exact generation and binds it to the enclosing
/// checkpoint stream. Restore, handoff, fold, and order coordinates and the
/// paired redo-fold identity remain independent. The reference is silent on
/// this checkpoint-pin projection, so omitted generations remain unpinned and
/// no pin or checkpoint state is synthesized.
pub fn restore_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_checkpoint_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<BranchMergePairedCheckpointRedoFoldSparseCompactionHandoffSnapshot>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSparseRestorePinHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSparseRestoreHandoffError,
> {
    restore_paired_checkpoint_redo_sparse_compaction_handoff_chains_preserving_restore_identity(
        known_checkpoint_ids,
        restore_handoffs,
    )
    .map(|streams| {
        streams
            .into_iter()
            .map(|stream| {
                let checkpoint_id = stream.checkpoint_id;
                let slots = stream
                    .slots
                    .into_iter()
                    .map(|slot| {
                        let pin = |generation: Option<CheckpointGeneration>| {
                            generation.map(|generation| {
                                BranchMergePairedCheckpointPinIdentity {
                                    checkpoint_id: checkpoint_id.clone(),
                                    generation,
                                }
                            })
                        };
                        BranchMergePairedCheckpointRedoFoldSparseRestorePinHandoffSlotSnapshot {
                            restore_ordinal: slot.restore_ordinal,
                            handoff_ordinal: slot.handoff_ordinal,
                            fold_ordinal: slot.fold_ordinal,
                            order: slot.order,
                            left_pin: pin(slot.left),
                            right_pin: pin(slot.right),
                            redo_fold_identity: slot.redo_fold_identity,
                        }
                    })
                    .collect();
                BranchMergePairedCheckpointRedoFoldSparseRestorePinHandoffStreamSnapshot {
                    checkpoint_id,
                    slots,
                }
            })
            .collect()
    })
}

/// Restores sparse redo-fold WAL handoffs without collapsing source identity.
///
/// Each outer input is one independent restore batch. Each inner input is one
/// WAL handoff containing its checkpoint streams, so its position supplies
/// the handoff ordinal while each run retains its fold ordinal. Every expanded
/// order consumes exactly one saved atomic log/segment pair. Checkpoint IDs
/// are unioned across known and observed streams; absent streams, omitted
/// sides, and gaps are not synthesized. The reference is silent on this
/// multi-batch WAL projection, so batch/source/fold/order is the stable
/// occurrence key.
pub fn restore_paired_checkpoint_redo_sparse_wal_handoff_chains_preserving_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldWalRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldWalRestoreHandoffError,
> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restore_handoffs.iter().flat_map(|batch| {
            batch.iter().flat_map(|handoff| {
                handoff.iter().map(|stream| stream.checkpoint_id.clone())
            })
        }))
        .collect::<BTreeSet<_>>();

    let mut restored = Vec::with_capacity(checkpoint_ids.len());
    for checkpoint_id in checkpoint_ids {
        let mut slots = Vec::new();
        for (restore_ordinal, batch) in restore_handoffs.iter().enumerate() {
            for (handoff_ordinal, handoff) in batch.iter().enumerate() {
                for stream in handoff
                    .iter()
                    .filter(|stream| stream.checkpoint_id == checkpoint_id)
                {
                    for run in &stream.runs {
                        if run.last_order < run.first_order {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldWalRestoreHandoffError::InvalidOrderRange {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                },
                            );
                        }
                        let expected =
                            u128::from(run.last_order) - u128::from(run.first_order) + 1;
                        if expected != run.log_segment_identities.len() as u128 {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldWalRestoreHandoffError::LogSegmentIdentityCountMismatch {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                    expected,
                                    actual: run.log_segment_identities.len(),
                                },
                            );
                        }
                        slots.extend(run.log_segment_identities.iter().enumerate().map(
                            |(offset, log_segment_identity)| {
                                BranchMergePairedCheckpointRedoFoldWalRestoreHandoffSlotSnapshot {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    order: run.first_order + offset as u64,
                                    left: run.left.clone(),
                                    right: run.right.clone(),
                                    redo_fold_identity: run.redo_fold_identity.clone(),
                                    log_segment_identity: log_segment_identity.clone(),
                                }
                            },
                        ));
                    }
                }
            }
        }
        slots.sort_by_key(|slot| {
            (
                slot.restore_ordinal,
                slot.handoff_ordinal,
                slot.fold_ordinal,
                slot.order,
            )
        });
        restored.push(BranchMergePairedCheckpointRedoFoldWalRestoreHandoffStreamSnapshot {
            checkpoint_id,
            slots,
        });
    }
    Ok(restored)
}

/// Restores paired redo folds across sparse WAL rotation merge handoffs.
///
/// Each outer input is an independent restore batch and each inner input is
/// one source handoff. Runs retain their original merge and fold labels;
/// every represented order consumes exactly one atomic WAL/segment identity.
/// The known and observed checkpoint catalog is unioned, but gaps, absent
/// streams, and missing checkpoint sides remain absent. The reference does
/// not define this nested projection, so caller order supplies the stable
/// restore and handoff ordinals.
pub fn restore_paired_checkpoint_redo_sparse_wal_merge_handoff_chains_preserving_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldWalSparseMergeChainCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffError,
> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restore_handoffs.iter().flat_map(|batch| {
            batch.iter().flat_map(|handoff| {
                handoff.iter().map(|stream| stream.checkpoint_id.clone())
            })
        }))
        .collect::<BTreeSet<_>>();

    let mut restored = Vec::with_capacity(checkpoint_ids.len());
    for checkpoint_id in checkpoint_ids {
        let mut slots = Vec::new();
        for (restore_ordinal, batch) in restore_handoffs.iter().enumerate() {
            for (handoff_ordinal, handoff) in batch.iter().enumerate() {
                for stream in handoff
                    .iter()
                    .filter(|stream| stream.checkpoint_id == checkpoint_id)
                {
                    for run in &stream.runs {
                        if run.last_order < run.first_order {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffError::InvalidOrderRange {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    merge_ordinal: run.merge_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                },
                            );
                        }
                        let expected =
                            u128::from(run.last_order) - u128::from(run.first_order) + 1;
                        if expected != run.log_segment_identities.len() as u128 {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffError::LogSegmentIdentityCountMismatch {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    merge_ordinal: run.merge_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                    expected,
                                    actual: run.log_segment_identities.len(),
                                },
                            );
                        }
                        slots.extend(run.log_segment_identities.iter().enumerate().map(
                            |(offset, log_segment_identity)| {
                                BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffSlotSnapshot {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    merge_ordinal: run.merge_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    order: run.first_order + offset as u64,
                                    left: run.left.clone(),
                                    right: run.right.clone(),
                                    redo_fold_identity: run.redo_fold_identity.clone(),
                                    log_segment_identity: log_segment_identity.clone(),
                                }
                            },
                        ));
                    }
                }
            }
        }
        slots.sort_by_key(|slot| {
            (
                slot.restore_ordinal,
                slot.handoff_ordinal,
                slot.merge_ordinal,
                slot.fold_ordinal,
                slot.order,
            )
        });
        restored.push(
            BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffStreamSnapshot {
                checkpoint_id,
                slots,
            },
        );
    }
    Ok(restored)
}

/// Restores sparse WAL compaction chains with checkpoint-bound directional pins.
///
/// Each present side's pin binds the enclosing checkpoint ID, exact generation,
/// exact WAL ID, and matching segment incarnation. Restore, handoff, merge,
/// fold, and order coordinates are inherited from the validated source chain;
/// the original atomic WAL/segment pair and redo-fold identity remain attached.
/// Missing sides stay unpinned rather than borrowing identity from the opposite
/// side or an adjacent occurrence. This pin projection is not defined by the
/// reference, so source coordinates and same-side bindings are used directly.
pub fn restore_paired_checkpoint_redo_sparse_wal_merge_handoff_chains_preserving_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldWalSparseMergeChainCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldWalMergeRestorePinHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldWalMergeRestoreHandoffError,
> {
    restore_paired_checkpoint_redo_sparse_wal_merge_handoff_chains_preserving_identity(
        known_checkpoint_ids,
        restore_handoffs,
    )
    .map(|streams| {
        streams
            .into_iter()
            .map(|stream| {
                let checkpoint_id = stream.checkpoint_id;
                let slots = stream
                    .slots
                    .into_iter()
                    .map(|slot| {
                        let pin = |generation: Option<CheckpointGeneration>,
                                   write_ahead_id: &[u8],
                                   segment_id: &[u8]| {
                            generation.map(|generation| {
                                BranchMergePairedCheckpointWalSegmentPinIdentity {
                                    checkpoint_id: checkpoint_id.clone(),
                                    generation,
                                    write_ahead_id: write_ahead_id.to_vec(),
                                    segment_id: segment_id.to_vec(),
                                }
                            })
                        };
                        let left_pin = pin(
                            slot.left,
                            &slot.log_segment_identity.write_ahead_identity.left_log,
                            &slot.log_segment_identity.segment_identity.left_segment,
                        );
                        let right_pin = pin(
                            slot.right,
                            &slot.log_segment_identity.write_ahead_identity.right_log,
                            &slot.log_segment_identity.segment_identity.right_segment,
                        );
                        BranchMergePairedCheckpointRedoFoldWalMergeRestorePinHandoffSlotSnapshot {
                            restore_ordinal: slot.restore_ordinal,
                            handoff_ordinal: slot.handoff_ordinal,
                            merge_ordinal: slot.merge_ordinal,
                            fold_ordinal: slot.fold_ordinal,
                            order: slot.order,
                            left_pin,
                            right_pin,
                            redo_fold_identity: slot.redo_fold_identity,
                            log_segment_identity: slot.log_segment_identity,
                        }
                    })
                    .collect();
                BranchMergePairedCheckpointRedoFoldWalMergeRestorePinHandoffStreamSnapshot {
                    checkpoint_id,
                    slots,
                }
            })
            .collect()
    })
}

/// Restores paired fold/segment identities across sparse checkpoint rotations.
///
/// Each outer input is an independent restore batch, and each inner input is
/// one compacted rotation handoff containing its checkpoint streams. The
/// batch, handoff, fold, and order coordinates remain independent even when
/// labels overlap. Repeated checkpoint streams within a handoff also retain
/// their original stream ordinal, even when their fold and order labels match.
/// Runs within each source stream retain their compaction ordinal as well, so
/// repeated fold and order labels from distinct runs remain distinct. Every
/// represented order consumes one exact directional segment pair; malformed
/// ranges or pair counts are rejected. The known and observed checkpoint
/// catalog is unioned, but gaps, omitted streams, and missing sides are not
/// filled in. The reference is silent on this chained restore projection, so
/// caller source order is the stable handoff, stream, and compaction rule.
pub fn restore_paired_checkpoint_redo_sparse_segment_rotation_handoff_chains_preserving_fold_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffError,
> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restore_handoffs.iter().flat_map(|batch| {
            batch.iter().flat_map(|handoff| {
                handoff.iter().map(|stream| stream.checkpoint_id.clone())
            })
        }))
        .collect::<BTreeSet<_>>();

    let mut restored = Vec::with_capacity(checkpoint_ids.len());
    for checkpoint_id in checkpoint_ids {
        let mut slots = Vec::new();
        for (restore_ordinal, batch) in restore_handoffs.iter().enumerate() {
            for (handoff_ordinal, handoff) in batch.iter().enumerate() {
                for (stream_ordinal, stream) in handoff
                    .iter()
                    .enumerate()
                    .filter(|(_, stream)| stream.checkpoint_id == checkpoint_id)
                {
                    for (compaction_ordinal, run) in stream.runs.iter().enumerate() {
                        if run.last_order < run.first_order {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffError::InvalidOrderRange {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    stream_ordinal,
                                    compaction_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                },
                            );
                        }
                        let expected =
                            u128::from(run.last_order) - u128::from(run.first_order) + 1;
                        if expected != run.segment_identities.len() as u128 {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffError::SegmentIdentityCountMismatch {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    stream_ordinal,
                                    compaction_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                    expected,
                                    actual: run.segment_identities.len(),
                                },
                            );
                        }
                        slots.extend(run.segment_identities.iter().enumerate().map(
                            |(offset, segment_identity)| {
                                BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffSlotSnapshot {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    stream_ordinal,
                                    compaction_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    order: run.first_order + offset as u64,
                                    left: run.left.clone(),
                                    right: run.right.clone(),
                                    redo_fold_identity: run.redo_fold_identity.clone(),
                                    segment_identity: segment_identity.clone(),
                                }
                            },
                        ));
                    }
                }
            }
        }
        slots.sort_by_key(|slot| {
            (
                slot.restore_ordinal,
                slot.handoff_ordinal,
                slot.stream_ordinal,
                slot.compaction_ordinal,
                slot.fold_ordinal,
                slot.order,
            )
        });
        restored.push(
            BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreHandoffStreamSnapshot {
                checkpoint_id,
                slots,
            },
        );
    }
    Ok(restored)
}

/// Restores sparse rotation compactions while retaining every source ordinal.
///
/// The nested inputs are ordered as restore batch, handoff, and checkpoint
/// stream. Each compacted run keeps its position within that stream. Expanded
/// slots therefore retain restore, handoff, stream, compaction, fold, and
/// order coordinates even when sparse labels overlap. Every represented order
/// consumes its exact directional segment pair. The reference does not define
/// this nested compaction projection, so source positions are stable ordinals;
/// checkpoint gaps, absent streams, and missing sides remain absent.
pub fn restore_paired_checkpoint_redo_sparse_segment_rotation_compaction_handoffs_preserving_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffError,
> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restore_handoffs.iter().flat_map(|batch| {
            batch.iter().flat_map(|handoff| {
                handoff.iter().map(|stream| stream.checkpoint_id.clone())
            })
        }))
        .collect::<BTreeSet<_>>();

    let mut restored = Vec::with_capacity(checkpoint_ids.len());
    for checkpoint_id in checkpoint_ids {
        let mut slots = Vec::new();
        for (restore_ordinal, batch) in restore_handoffs.iter().enumerate() {
            for (handoff_ordinal, handoff) in batch.iter().enumerate() {
                for (stream_ordinal, stream) in handoff.iter().enumerate() {
                    if stream.checkpoint_id != checkpoint_id {
                        continue;
                    }
                    for (compaction_ordinal, run) in stream.runs.iter().enumerate() {
                        if run.last_order < run.first_order {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffError::InvalidOrderRange {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    stream_ordinal,
                                    compaction_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                },
                            );
                        }
                        let expected =
                            u128::from(run.last_order) - u128::from(run.first_order) + 1;
                        if expected != run.segment_identities.len() as u128 {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffError::SegmentIdentityCountMismatch {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    stream_ordinal,
                                    compaction_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                    expected,
                                    actual: run.segment_identities.len(),
                                },
                            );
                        }
                        slots.extend(run.segment_identities.iter().enumerate().map(
                            |(offset, segment_identity)| {
                                BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffSlotSnapshot {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    stream_ordinal,
                                    compaction_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    order: run.first_order + offset as u64,
                                    left: run.left.clone(),
                                    right: run.right.clone(),
                                    redo_fold_identity: run.redo_fold_identity.clone(),
                                    segment_identity: segment_identity.clone(),
                                }
                            },
                        ));
                    }
                }
            }
        }
        slots.sort_by_key(|slot| {
            (
                slot.restore_ordinal,
                slot.handoff_ordinal,
                slot.stream_ordinal,
                slot.compaction_ordinal,
                slot.fold_ordinal,
                slot.order,
            )
        });
        restored.push(
            BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffStreamSnapshot {
                checkpoint_id,
                slots,
            },
        );
    }
    Ok(restored)
}

/// Restores sparse segment compactions with paired checkpoint-bound pin identity.
///
/// Each present side's pin binds the enclosing checkpoint ID, exact generation,
/// and exact directional segment ID. Restore, handoff, stream, compaction, fold,
/// and order coordinates remain distinct, as does the paired redo-fold
/// identity. The reference is silent on this pin projection, so missing sides
/// remain unpinned and are never filled from a sibling or adjacent order.
pub fn restore_paired_checkpoint_redo_sparse_segment_rotation_compaction_handoffs_preserving_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestorePinHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestoreHandoffError,
> {
    restore_paired_checkpoint_redo_sparse_segment_rotation_compaction_handoffs_preserving_identity(
        known_checkpoint_ids,
        restore_handoffs,
    )
    .map(|streams| {
        streams
            .into_iter()
            .map(|stream| {
                let checkpoint_id = stream.checkpoint_id;
                let slots = stream
                    .slots
                    .into_iter()
                    .map(|slot| {
                        let pin = |generation: Option<CheckpointGeneration>, segment_id: &[u8]| {
                            generation.map(|generation| {
                                BranchMergePairedCheckpointSegmentPinIdentity {
                                    checkpoint_id: checkpoint_id.clone(),
                                    generation,
                                    segment_id: segment_id.to_vec(),
                                }
                            })
                        };
                        let left_pin =
                            pin(slot.left, &slot.segment_identity.left_segment);
                        let right_pin =
                            pin(slot.right, &slot.segment_identity.right_segment);
                        BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestorePinHandoffSlotSnapshot {
                            restore_ordinal: slot.restore_ordinal,
                            handoff_ordinal: slot.handoff_ordinal,
                            stream_ordinal: slot.stream_ordinal,
                            compaction_ordinal: slot.compaction_ordinal,
                            fold_ordinal: slot.fold_ordinal,
                            order: slot.order,
                            left_pin,
                            right_pin,
                            redo_fold_identity: slot.redo_fold_identity,
                            segment_identity: slot.segment_identity,
                        }
                    })
                    .collect();
                BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRestorePinHandoffStreamSnapshot {
                    checkpoint_id,
                    slots,
                }
            })
            .collect()
    })
}

/// Restores merged sparse segment-rotation chains without flattening lineage.
///
/// Each outer input is an independent restore batch; each inner input is a
/// handoff containing checkpoint streams compacted from sparse merged chains.
/// Restored occurrences retain batch, handoff, original merge, fold, and order
/// coordinates. Every represented order consumes its exact directional
/// segment pair. Invalid ranges and missing or extra pairs return all source
/// coordinates. Known and observed streams are unioned, while gaps and absent
/// checkpoint sides remain absent. This chained projection is not defined by
/// the reference; input order supplies the stable handoff ordinal.
pub fn restore_paired_checkpoint_redo_sparse_segment_rotation_merge_handoffs_preserving_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffError,
> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(restore_handoffs.iter().flat_map(|batch| {
            batch.iter().flat_map(|handoff| {
                handoff.iter().map(|stream| stream.checkpoint_id.clone())
            })
        }))
        .collect::<BTreeSet<_>>();

    let mut restored = Vec::with_capacity(checkpoint_ids.len());
    for checkpoint_id in checkpoint_ids {
        let mut slots = Vec::new();
        for (restore_ordinal, batch) in restore_handoffs.iter().enumerate() {
            for (handoff_ordinal, handoff) in batch.iter().enumerate() {
                for stream in handoff
                    .iter()
                    .filter(|stream| stream.checkpoint_id == checkpoint_id)
                {
                    for run in &stream.runs {
                        if run.last_order < run.first_order {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffError::InvalidOrderRange {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    merge_ordinal: run.merge_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                },
                            );
                        }
                        let expected =
                            u128::from(run.last_order) - u128::from(run.first_order) + 1;
                        if expected != run.segment_identities.len() as u128 {
                            return Err(
                                BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffError::SegmentIdentityCountMismatch {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    merge_ordinal: run.merge_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    first_order: run.first_order,
                                    last_order: run.last_order,
                                    expected,
                                    actual: run.segment_identities.len(),
                                },
                            );
                        }
                        slots.extend(run.segment_identities.iter().enumerate().map(
                            |(offset, segment_identity)| {
                                BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffSlotSnapshot {
                                    restore_ordinal,
                                    handoff_ordinal,
                                    merge_ordinal: run.merge_ordinal,
                                    fold_ordinal: run.fold_ordinal,
                                    order: run.first_order + offset as u64,
                                    left: run.left.clone(),
                                    right: run.right.clone(),
                                    redo_fold_identity: run.redo_fold_identity.clone(),
                                    segment_identity: segment_identity.clone(),
                                }
                            },
                        ));
                    }
                }
            }
        }
        slots.sort_by_key(|slot| {
            (
                slot.restore_ordinal,
                slot.handoff_ordinal,
                slot.merge_ordinal,
                slot.fold_ordinal,
                slot.order,
            )
        });
        restored.push(
            BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffStreamSnapshot {
                checkpoint_id,
                slots,
            },
        );
    }
    Ok(restored)
}

/// Restores sparse segment chains and binds each present side to its checkpoint pin.
///
/// Restore, handoff, merge, fold, and order coordinates retain the underlying
/// restoration lineage. A directional pin is emitted only when that side has
/// a checkpoint generation; it carries the enclosing checkpoint ID and the
/// exact segment ID from the same side. The original pair remains available
/// when a side is omitted. The reference is silent on this pin projection, so
/// absent generations stay unpinned instead of receiving synthetic pins.
pub fn restore_paired_checkpoint_redo_sparse_segment_rotation_merge_handoffs_preserving_pin_identity(
    known_checkpoint_ids: &[CheckpointId],
    restore_handoffs: &[Vec<Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseMergeChainCompactionSnapshot>>],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestorePinHandoffStreamSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestoreHandoffError,
> {
    restore_paired_checkpoint_redo_sparse_segment_rotation_merge_handoffs_preserving_identity(
        known_checkpoint_ids,
        restore_handoffs,
    )
    .map(|streams| {
        streams
            .into_iter()
            .map(|stream| {
                let checkpoint_id = stream.checkpoint_id;
                let slots = stream
                    .slots
                    .into_iter()
                    .map(|slot| {
                        let left_pin = slot.left.clone().map(|generation| {
                            BranchMergePairedCheckpointSegmentPinIdentity {
                                checkpoint_id: checkpoint_id.clone(),
                                generation,
                                segment_id: slot.segment_identity.left_segment.clone(),
                            }
                        });
                        let right_pin = slot.right.clone().map(|generation| {
                            BranchMergePairedCheckpointSegmentPinIdentity {
                                checkpoint_id: checkpoint_id.clone(),
                                generation,
                                segment_id: slot.segment_identity.right_segment.clone(),
                            }
                        });
                        BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestorePinHandoffSlotSnapshot {
                            restore_ordinal: slot.restore_ordinal,
                            handoff_ordinal: slot.handoff_ordinal,
                            merge_ordinal: slot.merge_ordinal,
                            fold_ordinal: slot.fold_ordinal,
                            order: slot.order,
                            left_pin,
                            right_pin,
                            redo_fold_identity: slot.redo_fold_identity,
                            segment_identity: slot.segment_identity,
                        }
                    })
                    .collect();
                BranchMergePairedCheckpointRedoFoldSegmentRotationMergeRestorePinHandoffStreamSnapshot {
                    checkpoint_id,
                    slots,
                }
            })
            .collect()
    })
}

/// Compacts equal checkpoint state without losing paired fold identity.
///
/// A run joins only adjacent orders in one fold with identical left and right
/// checkpoint generations and the same directional fold-identity pair. Fold
/// boundaries and sparse gaps split runs even when an identity is reused.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_identity(
    streams: &[BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_fold_identity == slot.redo_fold_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_fold_identity: slot.redo_fold_identity.clone(),
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Folds redo-chain snapshots across sparse folds without losing paired identity.
///
/// The checkpoint catalog unions caller-known IDs with IDs observed in every
/// fold. Each frame contributes its two-sided checkpoint state, stable paired
/// redo-chain identity, and atomic log/segment identity to every stream. Fold
/// ordinal and in-fold order distinguish overlapping orders; gaps and
/// omissions remain explicit.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_chain_and_log_segment_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoChainIdentityFrame>],
) -> Vec<BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoChainIdentitySparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame
                                .checkpoints
                                .left
                                .get(stream_checkpoint_id)
                                .cloned(),
                            right: frame
                                .checkpoints
                                .right
                                .get(stream_checkpoint_id)
                                .cloned(),
                            redo_chain_identity: frame.redo_chain_identity.clone(),
                            log_segment_identity: frame.log_segment_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts sparse checkpoint folds while retaining chain and log lineage.
///
/// Runs join only across consecutive in-fold orders with equal two-sided
/// checkpoint state and the same paired redo-chain identity. Changes in the
/// atomic log/segment identity do not split a run: every exact pair is retained
/// in input order. Fold boundaries, state changes, chain changes, and sparse
/// gaps start new runs. This uses the v1 opaque redo-chain equality policy.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_chain_and_log_segment_identity(
    streams: &[BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_chain_identity == slot.redo_chain_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.log_segment_identities
                        .push(slot.log_segment_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_chain_identity: slot.redo_chain_identity.clone(),
                            log_segment_identities: vec![slot.log_segment_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoChainIdentitySparseStreamChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Restores sparse checkpoint streams from compacted fold-identity runs.
///
/// Every integer order covered by a compacted run represents an observed input
/// frame and expands to one slot with the run's exact checkpoint values and
/// paired fold identity. Orders between runs remain absent, and fold ordinals
/// remain attached so overlapping orders in different restores stay distinct.
pub fn restore_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_identity(
    compacted: &[BranchMergePairedCheckpointRedoFoldSparseStreamChainCompactionSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot> {
    compacted
        .iter()
        .map(|stream| {
            let mut slots = stream
                .runs
                .iter()
                .flat_map(|run| {
                    (run.first_order..=run.last_order).map(move |order| {
                        BranchMergePairedCheckpointRedoFoldSparseChainSlotSnapshot {
                            fold_ordinal: run.fold_ordinal,
                            order,
                            left: run.left.clone(),
                            right: run.right.clone(),
                            redo_fold_identity: run.redo_fold_identity.clone(),
                        }
                    })
                })
                .collect::<Vec<_>>();
            slots.sort_by_key(|slot| (slot.fold_ordinal, slot.order));
            BranchMergePairedCheckpointRedoFoldSparseStreamChainSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                slots,
            }
        })
        .collect()
}

/// Folds redo-chain identity with sparse write-ahead compaction observations.
///
/// Known and observed checkpoint IDs are projected across every fold. Every
/// frame contributes its paired redo-chain and log/segment identities to each
/// stream. The compaction pair is optional per frame, so a missing observation
/// remains explicit without dropping the frame's other lineage.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_chain_and_compaction_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoChainCompactionIdentityFrame>],
) -> Vec<BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoChainCompactionIdentitySparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame
                                .checkpoints
                                .left
                                .get(stream_checkpoint_id)
                                .cloned(),
                            right: frame
                                .checkpoints
                                .right
                                .get(stream_checkpoint_id)
                                .cloned(),
                            redo_chain_identity: frame.redo_chain_identity.clone(),
                            log_segment_identity: frame.log_segment_identity.clone(),
                            compaction_identity: frame.compaction_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts equal checkpoint state and redo-chain identity across each fold.
///
/// Runs join only adjacent in-fold orders with equal two-sided checkpoint
/// state and the same paired redo-chain identity. Missing or changed compaction
/// observations do not break that run: each order retains its exact optional
/// compaction pair alongside the atomic log/segment lineage. Fold boundaries,
/// state changes, chain changes, and sparse gaps split runs.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_chain_and_compaction_identity(
    streams: &[BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_chain_identity == slot.redo_chain_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.log_segment_identities
                        .push(slot.log_segment_identity.clone());
                    last.compaction_identities
                        .push(slot.compaction_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_chain_identity: slot.redo_chain_identity.clone(),
                            log_segment_identities: vec![slot.log_segment_identity.clone()],
                            compaction_identities: vec![slot.compaction_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoChainCompactionIdentitySparseStreamChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Folds sparse compaction chains while retaining each enclosing redo-fold pair.
///
/// Known and observed checkpoint IDs are projected across all folds. Every
/// frame contributes its checkpoint state, redo-chain identity, atomic
/// log/segment pair, and optional compaction pair to each stream. The
/// enclosing fold identity is repeated on every slot, including omissions.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_chain_compaction_identity(
    known_checkpoint_ids: &[CheckpointId],
    folds: &[BranchMergePairedCheckpointRedoFoldChainCompactionIdentityFold],
) -> Vec<BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(folds.iter().flat_map(|fold| {
            fold.frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = folds
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, fold)| {
                    fold.frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame
                                .checkpoints
                                .left
                                .get(stream_checkpoint_id)
                                .cloned(),
                            right: frame
                                .checkpoints
                                .right
                                .get(stream_checkpoint_id)
                                .cloned(),
                            redo_fold_identity: fold.redo_fold_identity.clone(),
                            redo_chain_identity: frame.redo_chain_identity.clone(),
                            log_segment_identity: frame.log_segment_identity.clone(),
                            compaction_identity: frame.compaction_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts equal states while preserving fold and per-order compaction lineage.
///
/// Adjacent orders join only in one fold when both checkpoint states, redo-fold
/// pair, and redo-chain pair match. A missing compaction observation does not
/// split a run: the exact optional pair and atomic log/segment pair are retained
/// at every order. Gaps, fold boundaries, state changes, or chain changes split
/// runs, and no absent order is synthesized.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_chain_compaction_identity(
    streams: &[BranchMergePairedCheckpointRedoFoldChainCompactionIdentitySparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_fold_identity == slot.redo_fold_identity
                    && last.redo_chain_identity == slot.redo_chain_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.log_segment_identities
                        .push(slot.log_segment_identity.clone());
                    last.compaction_identities
                        .push(slot.compaction_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_fold_identity: slot.redo_fold_identity.clone(),
                            redo_chain_identity: slot.redo_chain_identity.clone(),
                            log_segment_identities: vec![slot.log_segment_identity.clone()],
                            compaction_identities: vec![slot.compaction_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoFoldChainCompactionIdentityCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Folds sparse paired redo frames while retaining each order's undo-chain pair.
///
/// Known checkpoint IDs and IDs observed in the frames are both projected.
/// Every supplied frame contributes a slot per output stream, so a checkpoint
/// omitted by both logs retains the frame's directional undo identity rather
/// than erasing its lineage. Repeated undo pairs remain repeated at their
/// original orders, and missing integer orders are not synthesized. All IDs,
/// generations, positions, and undo-chain identities are opaque bytes/values.
pub fn fold_paired_checkpoint_redo_sparse_streams_preserving_undo_chain_identity(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoUndoFrame>,
) -> Vec<BranchMergePairedCheckpointRedoUndoSparseStreamSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let slots = frames
                .iter()
                .map(|(&order, frame)| {
                    BranchMergePairedCheckpointRedoUndoSparseSlotSnapshot {
                        order,
                        left: frame.checkpoints.left.get(&checkpoint_id).cloned(),
                        right: frame.checkpoints.right.get(&checkpoint_id).cloned(),
                        undo_chain_identity: frame.undo_chain_identity.clone(),
                    }
                })
                .collect();
            BranchMergePairedCheckpointRedoUndoSparseStreamSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Folds paired redo history while retaining undo-chain identity per fold.
///
/// The checkpoint catalog unions caller-known IDs with IDs observed in any
/// fold. Each frame contributes an exact left/right checkpoint state and its
/// directional undo-chain identity to every stream, including omissions.
/// Fold ordinal plus in-fold order distinguishes repeated orders across folds;
/// gaps and repeated undo pairs remain exactly as supplied.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_undo_chain_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoUndoFrame>],
) -> Vec<BranchMergePairedCheckpointRedoUndoSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoUndoSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame
                                .checkpoints
                                .left
                                .get(stream_checkpoint_id)
                                .cloned(),
                            right: frame
                                .checkpoints
                                .right
                                .get(stream_checkpoint_id)
                                .cloned(),
                            undo_chain_identity: frame.undo_chain_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoUndoSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts equal checkpoint state within each fold, preserving undo rotations.
///
/// Runs join only for adjacent in-fold orders with equal left and right
/// checkpoint values. Every order contributes its original paired undo-chain
/// identity, including repeated identities and rotations. A fold boundary,
/// state change, or sparse order gap starts a new run; no undo identity is used
/// to split or merge checkpoint state.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_undo_chain_identity(
    streams: &[BranchMergePairedCheckpointRedoUndoSparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.undo_chain_identities
                        .push(slot.undo_chain_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            undo_chain_identities: vec![slot.undo_chain_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoUndoSparseStreamChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Folds paired undo and compaction lineage over sparse checkpoint histories.
///
/// Known checkpoint IDs are unioned with IDs observed in any fold. Every
/// supplied frame contributes its exact directional undo-chain and compaction
/// pairs to each projected stream, including omissions. Fold ordinal plus
/// in-fold order keeps repeated orders distinct across rotation boundaries;
/// gaps are not synthesized.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_undo_and_compaction_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoUndoCompactionIdentityFrame>],
) -> Vec<BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoUndoCompactionSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame
                                .checkpoints
                                .left
                                .get(stream_checkpoint_id)
                                .cloned(),
                            right: frame
                                .checkpoints
                                .right
                                .get(stream_checkpoint_id)
                                .cloned(),
                            undo_chain_identity: frame.undo_chain_identity.clone(),
                            compaction_identity: frame.compaction_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts equal checkpoint state within each fold, retaining both lineages.
///
/// Adjacent in-fold orders share a run when the complete left and right
/// checkpoint generations are equal. Undo or compaction rotations do not split
/// that checkpoint run: each identity pair is retained at its exact order in
/// parallel vectors. A fold boundary, checkpoint state change, or sparse gap
/// starts a new run, so compaction cannot collapse distinct rotation histories.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_undo_and_compaction_identity(
    streams: &[BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.undo_chain_identities
                        .push(slot.undo_chain_identity.clone());
                    last.compaction_identities
                        .push(slot.compaction_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            undo_chain_identities: vec![slot.undo_chain_identity.clone()],
                            compaction_identities: vec![slot.compaction_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoUndoCompactionSparseStreamChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Folds sparse paired redo frames while preserving each side's active segment.
///
/// Known checkpoint IDs and IDs observed in the frames are both projected.
/// Each supplied frame contributes one slot per output stream and retains the
/// frame's exact left/right segment tokens even when both checkpoint sides
/// omit that stream. Segment rotation is represented by a distinct token per
/// segment incarnation; tokens remain directional and opaque. Repeated token
/// pairs are retained at every order, and missing integer orders are not
/// synthesized.
pub fn fold_paired_checkpoint_redo_sparse_streams_preserving_segment_rotation_identity(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoSegmentFrame>,
) -> Vec<BranchMergePairedCheckpointRedoSegmentSparseStreamSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let slots = frames
                .iter()
                .map(|(&order, frame)| {
                    BranchMergePairedCheckpointRedoSegmentSparseSlotSnapshot {
                        order,
                        left: frame.checkpoints.left.get(&checkpoint_id).cloned(),
                        right: frame.checkpoints.right.get(&checkpoint_id).cloned(),
                        segment_identity: frame.segment_identity.clone(),
                    }
                })
                .collect();
            BranchMergePairedCheckpointRedoSegmentSparseStreamSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Chains sparse redo folds while retaining each paired segment incarnation.
///
/// The output catalog unions caller-known checkpoint IDs with IDs observed in
/// every fold. Each supplied frame contributes one slot per output stream and
/// keeps its exact two-sided checkpoint state and segment identity. Fold
/// ordinal plus original in-fold order disambiguates equal order numbers from
/// separate folds; order gaps and repeated segment pairs remain unmodified.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_segment_rotation_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoSegmentFrame>],
) -> Vec<BranchMergePairedCheckpointRedoSegmentSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoSegmentSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame.checkpoints.left.get(stream_checkpoint_id).cloned(),
                            right: frame.checkpoints.right.get(stream_checkpoint_id).cloned(),
                            segment_identity: frame.segment_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoSegmentSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts equal checkpoint states within sparse folds, preserving rotations.
///
/// Runs never cross fold ordinals, even when the final order in one fold and
/// the first order in the next are numerically adjacent. Within a fold, only
/// consecutive orders with equal left and right checkpoint values join. Every
/// input segment pair is retained in order, so compaction cannot erase a
/// rotation or conflate a repeated identity with a missing slot. Sparse gaps,
/// state changes, and empty catalog streams remain explicit.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_segment_rotation_identity(
    streams: &[BranchMergePairedCheckpointRedoSegmentSparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.segment_identities.push(slot.segment_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            segment_identities: vec![slot.segment_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoSegmentSparseStreamChainCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Chains sparse redo folds while retaining fold provenance and segment rotations.
///
/// The stream catalog includes known checkpoint IDs and IDs observed in any
/// frame. Every frame contributes one slot per stream, even for omissions.
/// Slots retain the enclosing fold's exact directional identity and the
/// frame's exact directional segment pair. Fold ordinal plus in-fold order
/// distinguishes repeated orders; no gaps are synthesized.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_segment_rotation_identity(
    known_checkpoint_ids: &[CheckpointId],
    folds: &[BranchMergePairedCheckpointRedoFoldSegmentRotationFold],
) -> Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(folds.iter().flat_map(|fold| {
            fold.frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = folds
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, fold)| {
                    fold.frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame
                                .checkpoints
                                .left
                                .get(stream_checkpoint_id)
                                .cloned(),
                            right: frame
                                .checkpoints
                                .right
                                .get(stream_checkpoint_id)
                                .cloned(),
                            redo_fold_identity: fold.redo_fold_identity.clone(),
                            segment_identity: frame.segment_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts equal checkpoint states without erasing either identity layer.
///
/// Consecutive orders join only within one fold when both checkpoint values
/// and the redo-fold identity match. Segment rotations do not split a state
/// run: every per-order pair is retained in `segment_identities`. Fold
/// boundaries, sparse gaps, and state changes always start a new run.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_segment_rotation_identity(
    streams: &[BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_fold_identity == slot.redo_fold_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.segment_identities.push(slot.segment_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_fold_identity: slot.redo_fold_identity.clone(),
                            segment_identities: vec![slot.segment_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Restores sparse checkpoint slots from compacted fold/segment runs.
///
/// Every represented order expands with its exact checkpoint state, enclosing
/// redo-fold identity, and directional segment pair. Sparse gaps and fold
/// boundaries remain distinct. Runs with an invalid range or anything other
/// than one saved segment pair per represented order are rejected rather than
/// synthesizing or losing rotation history.
pub fn restore_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_segment_rotation_identity(
    compacted: &[BranchMergePairedCheckpointRedoFoldSegmentRotationCompactionSnapshot],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot>,
    BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreError,
> {
    compacted
        .iter()
        .map(|stream| {
            let mut slots = Vec::new();
            for run in &stream.runs {
                if run.last_order < run.first_order {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreError::InvalidOrderRange {
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                        },
                    );
                }
                let expected = u128::from(run.last_order) - u128::from(run.first_order) + 1;
                if expected != run.segment_identities.len() as u128 {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldSegmentRotationRestoreError::SegmentIdentityCountMismatch {
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                            expected,
                            actual: run.segment_identities.len(),
                        },
                    );
                }
                slots.extend(run.segment_identities.iter().enumerate().map(
                    |(offset, segment_identity)| {
                        BranchMergePairedCheckpointRedoFoldSegmentRotationSparseChainSlotSnapshot {
                            fold_ordinal: run.fold_ordinal,
                            order: run.first_order + offset as u64,
                            left: run.left.clone(),
                            right: run.right.clone(),
                            redo_fold_identity: run.redo_fold_identity.clone(),
                            segment_identity: segment_identity.clone(),
                        }
                    },
                ));
            }
            slots.sort_by_key(|slot| (slot.fold_ordinal, slot.order));
            Ok(BranchMergePairedCheckpointRedoFoldSegmentRotationSparseStreamChainSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                slots,
            })
        })
        .collect()
}

/// Chains sparse redo folds while keeping fold identity distinct from the
/// frame's atomically bound write-ahead logs and active segment incarnations.
///
/// Known and observed checkpoint IDs are projected into every supplied
/// frame. Each slot retains the exact fold identity and log/segment pair;
/// `(fold_ordinal, order)` distinguishes reused fold labels and overlapping
/// frame orders. Missing checkpoint sides and absent orders remain missing.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_log_segment_identity(
    known_checkpoint_ids: &[CheckpointId],
    folds: &[BranchMergePairedCheckpointRedoFoldLogSegmentIdentityFold],
) -> Vec<BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(folds.iter().flat_map(|fold| {
            fold.frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = folds
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, fold)| {
                    fold.frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame.checkpoints.left.get(stream_checkpoint_id).cloned(),
                            right: frame.checkpoints.right.get(stream_checkpoint_id).cloned(),
                            redo_fold_identity: fold.redo_fold_identity.clone(),
                            log_segment_identity: frame.log_segment_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

/// Compacts adjacent equal checkpoint states without erasing either fold or
/// write-ahead rotation identity.
///
/// A run stays within one fold and joins only consecutive orders with equal
/// two-sided checkpoint state and redo-fold identity. Changes to either log
/// or segment identity do not split a run; every exact atomic pair remains in
/// input order. Gaps, fold boundaries, and state changes split runs.
pub fn compress_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_log_segment_identity(
    streams: &[BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseStreamChainSnapshot],
) -> Vec<BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionSnapshot> {
    streams
        .iter()
        .map(|stream| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionRunSnapshot,
            >::new();
            for slot in &stream.slots {
                if let Some(last) = runs.last_mut()
                    && last.fold_ordinal == slot.fold_ordinal
                    && last.left == slot.left
                    && last.right == slot.right
                    && last.redo_fold_identity == slot.redo_fold_identity
                    && last.last_order.checked_add(1) == Some(slot.order)
                {
                    last.last_order = slot.order;
                    last.log_segment_identities
                        .push(slot.log_segment_identity.clone());
                } else {
                    runs.push(
                        BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionRunSnapshot {
                            fold_ordinal: slot.fold_ordinal,
                            first_order: slot.order,
                            last_order: slot.order,
                            left: slot.left.clone(),
                            right: slot.right.clone(),
                            redo_fold_identity: slot.redo_fold_identity.clone(),
                            log_segment_identities: vec![slot.log_segment_identity.clone()],
                        },
                    );
                }
            }
            BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionSnapshot {
                checkpoint_id: stream.checkpoint_id.clone(),
                runs,
            }
        })
        .collect()
}

/// Re-expands compacted fold/log-segment runs into sparse checkpoint slots.
///
/// Each run order consumes exactly one saved atomic log/segment pair. Invalid
/// ranges and truncated or overlong identity vectors are rejected instead of
/// silently synthesizing, dropping, or widening a rotation history.
pub fn restore_paired_checkpoint_redo_sparse_stream_chains_preserving_fold_and_log_segment_identity(
    compacted: &[BranchMergePairedCheckpointRedoFoldLogSegmentIdentityCompactionSnapshot],
) -> Result<
    Vec<BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseStreamChainSnapshot>,
    BranchMergePairedCheckpointRedoFoldLogSegmentIdentityRestoreError,
> {
    compacted
        .iter()
        .map(|stream| {
            let mut slots = Vec::new();
            for run in &stream.runs {
                if run.last_order < run.first_order {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldLogSegmentIdentityRestoreError::InvalidOrderRange {
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                        },
                    );
                }
                let expected = u128::from(run.last_order) - u128::from(run.first_order) + 1;
                if expected != run.log_segment_identities.len() as u128 {
                    return Err(
                        BranchMergePairedCheckpointRedoFoldLogSegmentIdentityRestoreError::LogSegmentIdentityCountMismatch {
                            fold_ordinal: run.fold_ordinal,
                            first_order: run.first_order,
                            last_order: run.last_order,
                            expected,
                            actual: run.log_segment_identities.len(),
                        },
                    );
                }
                slots.extend(run.log_segment_identities.iter().enumerate().map(
                    |(offset, log_segment_identity)| {
                        BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseChainSlotSnapshot {
                            fold_ordinal: run.fold_ordinal,
                            order: run.first_order + offset as u64,
                            left: run.left.clone(),
                            right: run.right.clone(),
                            redo_fold_identity: run.redo_fold_identity.clone(),
                            log_segment_identity: log_segment_identity.clone(),
                        }
                    },
                ));
            }
            slots.sort_by_key(|slot| (slot.fold_ordinal, slot.order));
            Ok(
                BranchMergePairedCheckpointRedoFoldLogSegmentIdentitySparseStreamChainSnapshot {
                    checkpoint_id: stream.checkpoint_id.clone(),
                    slots,
                },
            )
        })
        .collect()
}

/// Compresses adjacent equal sparse checkpoint states while retaining each
/// order's paired segment identity through rotation boundaries.
///
/// Known checkpoint IDs and IDs observed in the frames are both retained. A
/// run joins only consecutive orders with equal left and right checkpoint
/// values; missing entries are `None` and differ from a present positionless
/// checkpoint. Segment rotation does not block state compaction because every
/// segment pair is copied into the run in input order. An order gap or a
/// checkpoint state change starts a new run, so no redo state is omitted.
/// Chains sparse checkpoint folds with each frame's atomic log/segment pair.
///
/// The output checkpoint catalog unions caller-known IDs with IDs observed in
/// every fold. Each supplied frame contributes one slot per stream, retaining
/// its exact paired checkpoint values and bound write-ahead/segment identity.
/// Fold ordinal plus in-fold order distinguishes overlapping orders across
/// folds. Missing streams, gaps, and repeated identity pairs are preserved as
/// supplied; no checkpoint value or lineage is inferred or deduplicated.
pub fn fold_paired_checkpoint_redo_sparse_stream_chains_preserving_log_segment_identity(
    known_checkpoint_ids: &[CheckpointId],
    fold_frames: &[BTreeMap<u64, BranchMergePairedCheckpointRedoLogSegmentFrame>],
) -> Vec<BranchMergePairedCheckpointRedoLogSegmentSparseStreamChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(fold_frames.iter().flat_map(|frames| {
            frames.values().flat_map(|frame| {
                frame
                    .checkpoints
                    .left
                    .keys()
                    .chain(frame.checkpoints.right.keys())
                    .cloned()
            })
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let stream_checkpoint_id = &checkpoint_id;
            let slots = fold_frames
                .iter()
                .enumerate()
                .flat_map(|(fold_ordinal, frames)| {
                    frames.iter().map(move |(&order, frame)| {
                        BranchMergePairedCheckpointRedoLogSegmentSparseChainSlotSnapshot {
                            fold_ordinal,
                            order,
                            left: frame.checkpoints.left.get(stream_checkpoint_id).cloned(),
                            right: frame.checkpoints.right.get(stream_checkpoint_id).cloned(),
                            log_segment_identity: frame.log_segment_identity.clone(),
                        }
                    })
                })
                .collect();
            BranchMergePairedCheckpointRedoLogSegmentSparseStreamChainSnapshot {
                checkpoint_id,
                slots,
            }
        })
        .collect()
}

pub fn compress_paired_checkpoint_redo_sparse_chains_preserving_segment_rotation_identity(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoSegmentFrame>,
) -> Vec<BranchMergePairedCheckpointRedoSegmentCompactionChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs =
                Vec::<BranchMergePairedCheckpointRedoSegmentCompactionRunSnapshot>::new();
            for (&order, frame) in frames {
                let left = frame.checkpoints.left.get(&checkpoint_id).cloned();
                let right = frame.checkpoints.right.get(&checkpoint_id).cloned();
                if let Some(last) = runs.last_mut()
                    && last.left == left
                    && last.right == right
                    && last.last_order.checked_add(1) == Some(order)
                {
                    last.last_order = order;
                    last.segment_identities.push(frame.segment_identity.clone());
                } else {
                    runs.push(BranchMergePairedCheckpointRedoSegmentCompactionRunSnapshot {
                        first_order: order,
                        last_order: order,
                        left,
                        right,
                        segment_identities: vec![frame.segment_identity.clone()],
                    });
                }
            }
            BranchMergePairedCheckpointRedoSegmentCompactionChainSnapshot {
                checkpoint_id,
                runs,
            }
        })
        .collect()
}

/// Compacts sparse checkpoint state while preserving each order's paired
/// write-ahead log and segment identities as one lineage record.
///
/// Checkpoint IDs come from both the caller's known catalog and frame entries.
/// Runs join only across consecutive orders with identical full state on both
/// sides. Every input order contributes its original ordered log/segment pair
/// to the run, including repeated identities and rotations. Keeping the pair
/// in one value prevents a compacted log sequence from becoming misaligned
/// with its segment rotation sequence. State changes and order gaps split runs.
pub fn compress_paired_checkpoint_redo_sparse_chains_preserving_log_segment_identity(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoLogSegmentFrame>,
) -> Vec<BranchMergePairedCheckpointRedoLogSegmentCompactionChainSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs = Vec::<
                BranchMergePairedCheckpointRedoLogSegmentCompactionRunSnapshot,
            >::new();
            for (&order, frame) in frames {
                let left = frame.checkpoints.left.get(&checkpoint_id).cloned();
                let right = frame.checkpoints.right.get(&checkpoint_id).cloned();
                if let Some(last) = runs.last_mut()
                    && last.left == left
                    && last.right == right
                    && last.last_order.checked_add(1) == Some(order)
                {
                    last.last_order = order;
                    last.log_segment_identities
                        .push(frame.log_segment_identity.clone());
                } else {
                    runs.push(BranchMergePairedCheckpointRedoLogSegmentCompactionRunSnapshot {
                        first_order: order,
                        last_order: order,
                        left,
                        right,
                        log_segment_identities: vec![frame.log_segment_identity.clone()],
                    });
                }
            }
            BranchMergePairedCheckpointRedoLogSegmentCompactionChainSnapshot {
                checkpoint_id,
                runs,
            }
        })
        .collect()
}

/// Compacts paired sparse redo state without losing chain incarnation.
///
/// Checkpoint IDs from the known catalog and observed frames are both kept.
/// Runs join only across consecutive orders when both checkpoint states and
/// the paired redo-chain identity are equal. Each accepted frame contributes
/// its complete log/segment identity pair in input order; segment rotation
/// therefore does not split a stable chain, while a chain change or order gap
/// does. Missing checkpoint entries remain distinct from present positionless
/// generations. Redo-chain identity is a v1 grouping token because the wire
/// reference does not specify chain-label comparison or compaction behavior.
pub fn compress_paired_checkpoint_redo_sparse_chains_preserving_chain_and_log_segment_identity(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoChainIdentityFrame>,
) -> Vec<BranchMergePairedCheckpointRedoChainIdentityCompactionSnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs = Vec::<BranchMergePairedCheckpointRedoChainIdentityRunSnapshot>::new();
            for (&order, frame) in frames {
                let left = frame.checkpoints.left.get(&checkpoint_id).cloned();
                let right = frame.checkpoints.right.get(&checkpoint_id).cloned();
                if let Some(last) = runs.last_mut()
                    && last.left == left
                    && last.right == right
                    && last.redo_chain_identity == frame.redo_chain_identity
                    && last.last_order.checked_add(1) == Some(order)
                {
                    last.last_order = order;
                    last.log_segment_identities
                        .push(frame.log_segment_identity.clone());
                } else {
                    runs.push(BranchMergePairedCheckpointRedoChainIdentityRunSnapshot {
                        first_order: order,
                        last_order: order,
                        left,
                        right,
                        redo_chain_identity: frame.redo_chain_identity.clone(),
                        log_segment_identities: vec![frame.log_segment_identity.clone()],
                    });
                }
            }
            BranchMergePairedCheckpointRedoChainIdentityCompactionSnapshot {
                checkpoint_id,
                runs,
            }
        })
        .collect()
}

/// Compacts sparse redo chains while retaining each paired compaction ID.
///
/// Checkpoint IDs from both the caller's catalog and the input frames are
/// emitted. A run joins only consecutive orders with identical two-sided
/// checkpoint state and redo-chain identity. Compaction IDs do not alter the
/// run boundary: each order's opaque left/right compaction pair is preserved
/// in order, alongside the paired log/segment lineage. Thus compaction changes
/// survive chain rotations without being mistaken for redo state changes.
/// Missing checkpoint entries remain distinct from present positionless
/// generations; order gaps and redo-chain changes split runs.
pub fn compress_paired_checkpoint_redo_sparse_chains_preserving_compaction_identity(
    known_checkpoint_ids: &[CheckpointId],
    frames: &BTreeMap<u64, BranchMergePairedCheckpointRedoCompactionIdentityFrame>,
) -> Vec<BranchMergePairedCheckpointRedoCompactionIdentitySnapshot> {
    let checkpoint_ids = known_checkpoint_ids
        .iter()
        .cloned()
        .chain(frames.values().flat_map(|frame| {
            frame
                .checkpoints
                .left
                .keys()
                .chain(frame.checkpoints.right.keys())
                .cloned()
        }))
        .collect::<BTreeSet<_>>();

    checkpoint_ids
        .into_iter()
        .map(|checkpoint_id| {
            let mut runs = Vec::<BranchMergePairedCheckpointRedoCompactionIdentityRunSnapshot>::new();
            for (&order, frame) in frames {
                let left = frame.checkpoints.left.get(&checkpoint_id).cloned();
                let right = frame.checkpoints.right.get(&checkpoint_id).cloned();
                if let Some(last) = runs.last_mut()
                    && last.left == left
                    && last.right == right
                    && last.redo_chain_identity == frame.redo_chain_identity
                    && last.last_order.checked_add(1) == Some(order)
                {
                    last.last_order = order;
                    last.compaction_identities
                        .push(frame.compaction_identity.clone());
                    last.log_segment_identities
                        .push(frame.log_segment_identity.clone());
                } else {
                    runs.push(BranchMergePairedCheckpointRedoCompactionIdentityRunSnapshot {
                        first_order: order,
                        last_order: order,
                        left,
                        right,
                        redo_chain_identity: frame.redo_chain_identity.clone(),
                        compaction_identities: vec![frame.compaction_identity.clone()],
                        log_segment_identities: vec![frame.log_segment_identity.clone()],
                    });
                }
            }
            BranchMergePairedCheckpointRedoCompactionIdentitySnapshot {
                checkpoint_id,
                runs,
            }
        })
        .collect()
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

/// A table's complete depth layout within one paired tombstone wave.
///
/// Peer tables may have different depth counts. Each table supplies every
/// fragment index in `0..fragment_count`; an empty fragment is still an
/// identity-bearing depth position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTableDepthFragments {
    pub fragment_count: usize,
    pub fragments: BTreeMap<usize, Vec<CanonicalValue>>,
}

/// The complete depth labels released for one stable table branch.
/// Empty labels remain present even when their fragments contain no keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTableDepthLadderEvent {
    pub order: u64,
    pub table: ObjectId,
    pub depth_labels: Vec<usize>,
}

/// One complete paired tombstone wave whose peer tables have independent
/// depth-fragment labels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTabularDepthWave {
    pub order: u64,
    pub tables: BTreeMap<ObjectId, BranchMergeTableDepthFragments>,
}

/// One stable table column's complete depth layout in a restore wave.
///
/// Each fragment carries canonical `(row_key, value)` cells. Columns keep
/// independent depth counts, and an empty fragment remains an identity-bearing
/// position in that column's restore ladder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnDepthFragments {
    pub fragment_count: usize,
    pub fragments: BTreeMap<usize, Vec<(CanonicalValue, CanonicalValue)>>,
}

/// One paired restore wave with independent depth ladders for each stable
/// `(table, column)` identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTabularColumnDepthWave {
    pub order: u64,
    pub columns: BTreeMap<(ObjectId, ObjectId), BranchMergeColumnDepthFragments>,
}

/// One paired restore wave contributed by multiple stable parent branches.
/// Every parent carries its own table-column depth ladders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeMultiParentTabularColumnDepthWave {
    pub order: u64,
    pub parents:
        BTreeMap<ObjectId, BTreeMap<(ObjectId, ObjectId), BranchMergeColumnDepthFragments>>,
}

/// A canonical column cell released from a complete restore ladder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnDepthEvent {
    pub order: u64,
    pub table: ObjectId,
    pub column: ObjectId,
    pub fragment: usize,
    pub key: CanonicalValue,
    pub value: CanonicalValue,
}

/// The complete depth labels released for one stable table-column branch.
/// Empty labels remain present even when their fragments carried no cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnDepthLadderEvent {
    pub order: u64,
    pub table: ObjectId,
    pub column: ObjectId,
    pub depth_labels: Vec<usize>,
}

/// One labeled position in a stable table-column restore ladder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnDepthFragmentSnapshot {
    pub label: usize,
    pub cells: Vec<(CanonicalValue, CanonicalValue)>,
}

/// A stable table-column ladder with each label bound to its fragment cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnDepthLadderSnapshot {
    pub table: ObjectId,
    pub column: ObjectId,
    pub fragments: Vec<BranchMergeColumnDepthFragmentSnapshot>,
}

/// A released paired column restore wave with complete per-column ladders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeTabularColumnRestoreWaveSnapshot {
    pub order: u64,
    pub columns: Vec<BranchMergeColumnDepthLadderSnapshot>,
}

/// One stable column's independent depth labels at one released wave order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnDepthLadderWaveSnapshot {
    pub order: u64,
    pub fragments: Vec<BranchMergeColumnDepthFragmentSnapshot>,
}

/// A stable table-column identity folded across its released restore waves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreLadderFoldSnapshot {
    pub table: ObjectId,
    pub column: ObjectId,
    pub waves: Vec<BranchMergeColumnDepthLadderWaveSnapshot>,
}

/// One released column-restore order in a dense ladder timeline.
///
/// `None` means the stable column was omitted from that restore wave. A
/// present ladder contains all local labels, so an empty fragment remains a
/// labeled entry with an empty `cells` vector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreLadderWaveSlotSnapshot {
    pub order: u64,
    pub fragments: Option<Vec<BranchMergeColumnDepthFragmentSnapshot>>,
}

/// A stable table-column ladder with explicit presence across restore waves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreLadderTimelineSnapshot {
    pub table: ObjectId,
    pub column: ObjectId,
    pub waves: Vec<BranchMergeColumnRestoreLadderWaveSlotSnapshot>,
}

/// A maximal contiguous run of committed paired column-restore waves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub ladders: Vec<BranchMergeColumnRestoreLadderTimelineSnapshot>,
}

/// One stable column's dense depth-label timeline inside a restore storm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormLadderSnapshot {
    pub first_order: u64,
    pub last_order: u64,
    pub waves: Vec<BranchMergeColumnRestoreLadderWaveSlotSnapshot>,
}

/// A stable table-column identity folded across all committed restore storms.
///
/// Every storm remains a separate group. A column absent for an entire storm
/// still receives a storm entry with `None` wave slots, so later depth labels
/// cannot be mistaken for a continuation of an earlier group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormFoldSnapshot {
    pub table: ObjectId,
    pub column: ObjectId,
    pub storms: Vec<BranchMergeColumnRestoreStormLadderSnapshot>,
}

/// One stable column's depth labels at a specific wave in a restore storm.
///
/// `storm_index` is zero-based in committed lineage order. The stable
/// `(table, column)` identity, storm range, and wave order travel with the
/// labels so sibling ladders can be paired without interpreting cell values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormDepthLabelWaveSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub order: u64,
    pub table: ObjectId,
    pub column: ObjectId,
    pub depth_labels: Vec<usize>,
}

/// Snapshot paths bound to one depth label in a paired column restore storm.
///
/// `snapshot_paths` contains the canonical row keys from that labeled
/// fragment. An empty fragment remains represented with an empty path list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormDepthPathSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub order: u64,
    pub table: ObjectId,
    pub column: ObjectId,
    pub label: usize,
    pub snapshot_paths: Vec<CanonicalValue>,
}

/// One fragment's path identity in a dense storm-depth path timeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormDepthPathFragmentSnapshot {
    pub label: usize,
    pub snapshot_paths: Vec<CanonicalValue>,
}

/// A stable column's path-bearing depth slot at one restore wave in a storm.
///
/// `fragments: None` means the column was omitted from this wave. A present
/// labeled empty fragment is `Some` with an empty `snapshot_paths` list, so
/// omission cannot shift or inherit a neighboring depth identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormDepthPathWaveSlotSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub order: u64,
    pub table: ObjectId,
    pub column: ObjectId,
    pub fragments: Option<Vec<BranchMergeColumnRestoreStormDepthPathFragmentSnapshot>>,
}

/// One restore-wave slot for a canonical snapshot path within a column fold.
///
/// `depth_labels: None` means the column was omitted from this wave. A
/// present column with no occurrence of this path uses `Some(Vec::new())`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormSnapshotPathWaveSlotSnapshot {
    pub order: u64,
    pub depth_labels: Option<Vec<usize>>,
}

/// One canonical snapshot path folded across a stable column's restore storm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreStormSnapshotPathFoldSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub table: ObjectId,
    pub column: ObjectId,
    pub snapshot_path: CanonicalValue,
    pub waves: Vec<BranchMergeColumnRestoreStormSnapshotPathWaveSlotSnapshot>,
}

/// One restore storm in a chained canonical snapshot-path fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreSnapshotPathStormSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub waves: Vec<BranchMergeColumnRestoreStormSnapshotPathWaveSlotSnapshot>,
}

/// One canonical snapshot path folded across all restore storms for a column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreSnapshotPathChainFoldSnapshot {
    pub table: ObjectId,
    pub column: ObjectId,
    pub snapshot_path: CanonicalValue,
    pub storms: Vec<BranchMergeColumnRestoreSnapshotPathStormSnapshot>,
}

/// One stable column's contribution to a table-wide paired snapshot-path fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedSnapshotPathColumnFoldSnapshot {
    pub column: ObjectId,
    pub storms: Vec<BranchMergeColumnRestoreSnapshotPathStormSnapshot>,
}

/// One canonical snapshot path aligned across every stable column in a table.
///
/// Each column keeps its own storm and depth identities. An omitted column
/// wave remains `None`; a present column where this path is absent remains
/// `Some(Vec::new())`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedSnapshotPathChainFoldSnapshot {
    pub table: ObjectId,
    pub snapshot_path: CanonicalValue,
    pub columns: Vec<BranchMergeColumnRestorePairedSnapshotPathColumnFoldSnapshot>,
}

/// One restore-wave slot for both sides of a strict text-path extension.
///
/// `None` means the column was omitted. `Some(Vec::new())` means the column
/// was present but that exact path did not occur in any depth fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionWaveSlotSnapshot {
    pub order: u64,
    pub prefix_depth_labels: Option<Vec<usize>>,
    pub extension_depth_labels: Option<Vec<usize>>,
}

/// A path-prefix/extension pair folded across one restore storm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionStormSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub waves: Vec<BranchMergeColumnRestorePairedPathExtensionWaveSlotSnapshot>,
}

/// One stable column's path-prefix/extension history across restore storms.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionColumnFoldSnapshot {
    pub column: ObjectId,
    pub storms: Vec<BranchMergeColumnRestorePairedPathExtensionStormSnapshot>,
}

/// One strict path extension paired with its prefix across table columns.
///
/// Only canonical text keys with a slash-delimited prefix relationship form
/// an extension pair. Keys remain opaque everywhere else in storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionFoldSnapshot {
    pub table: ObjectId,
    pub prefix_path: CanonicalValue,
    pub extension_path: CanonicalValue,
    pub columns: Vec<BranchMergeColumnRestorePairedPathExtensionColumnFoldSnapshot>,
}

/// The occurrence state of one canonical snapshot path in a restore wave.
///
/// Column omission and path omission are separate states: `ColumnOmitted`
/// means the stable column has no wave slot, while `PathOmitted` means the
/// column is present but this path has no depth label in that wave.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BranchMergeSnapshotPathOccurrence {
    ColumnOmitted,
    PathOmitted,
    DepthLabels(Vec<usize>),
}

/// One typed occurrence slot for a canonical snapshot path in a restore wave.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreSnapshotPathOccurrenceWaveSnapshot {
    pub order: u64,
    pub occurrence: BranchMergeSnapshotPathOccurrence,
}

/// One storm of typed occurrence slots for a snapshot path and stable column.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestoreSnapshotPathOccurrenceStormSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub waves: Vec<BranchMergeColumnRestoreSnapshotPathOccurrenceWaveSnapshot>,
}

/// One stable column's typed occurrence history for a paired path fold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedSnapshotPathOccurrenceColumnFoldSnapshot {
    pub column: ObjectId,
    pub storms: Vec<BranchMergeColumnRestoreSnapshotPathOccurrenceStormSnapshot>,
}

/// A canonical snapshot path with typed omission states across paired columns.
///
/// The path identity remains attached to the fold even when every column
/// omits it in a wave. Each stable column retains its independent storm and
/// depth labels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedSnapshotPathOccurrenceFoldSnapshot {
    pub table: ObjectId,
    pub snapshot_path: CanonicalValue,
    pub columns: Vec<BranchMergeColumnRestorePairedSnapshotPathOccurrenceColumnFoldSnapshot>,
}

/// One wave's typed depth identities for a path prefix and its extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionOccurrenceWaveSnapshot {
    pub order: u64,
    pub prefix_occurrence: BranchMergeSnapshotPathOccurrence,
    pub extension_occurrence: BranchMergeSnapshotPathOccurrence,
}

/// A strict prefix/extension pair's typed wave identities in one restore storm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionOccurrenceStormSnapshot {
    pub storm_index: usize,
    pub first_order: u64,
    pub last_order: u64,
    pub waves: Vec<BranchMergeColumnRestorePairedPathExtensionOccurrenceWaveSnapshot>,
}

/// One stable column's typed prefix/extension history across restore storms.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionOccurrenceColumnFoldSnapshot {
    pub column: ObjectId,
    pub storms: Vec<BranchMergeColumnRestorePairedPathExtensionOccurrenceStormSnapshot>,
}

/// A strict path extension and prefix with typed omission states across columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeColumnRestorePairedPathExtensionOccurrenceFoldSnapshot {
    pub table: ObjectId,
    pub prefix_path: CanonicalValue,
    pub extension_path: CanonicalValue,
    pub columns: Vec<BranchMergeColumnRestorePairedPathExtensionOccurrenceColumnFoldSnapshot>,
}

/// A canonical column cell released with its source parent identity intact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeParentColumnDepthEvent {
    pub order: u64,
    pub parent: ObjectId,
    pub table: ObjectId,
    pub column: ObjectId,
    pub fragment: usize,
    pub key: CanonicalValue,
    pub value: CanonicalValue,
}

/// The complete depth labels released for one parent-local table-column branch.
/// Empty labels remain present even when their fragments carried no cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeParentColumnDepthLadderEvent {
    pub order: u64,
    pub parent: ObjectId,
    pub table: ObjectId,
    pub column: ObjectId,
    pub depth_labels: Vec<usize>,
}

/// The complete parent-local ladder roster released for one multi-parent
/// restore wave. All entries share `order` and are published as one atomic
/// lineage step, so consumers do not need to infer a wave boundary from the
/// flattened parent-column event stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeMultiParentColumnDepthLadderWaveEvent {
    pub order: u64,
    pub ladders: Vec<BranchMergeParentColumnDepthLadderEvent>,
}

/// One labeled depth in a parent-local column ladder, including empty depths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeParentColumnDepthFragmentSnapshot {
    pub label: usize,
    pub cells: Vec<(CanonicalValue, CanonicalValue)>,
}

/// A parent/table/column ladder with each depth label bound to its cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeParentColumnDepthLadderSnapshot {
    pub parent: ObjectId,
    pub table: ObjectId,
    pub column: ObjectId,
    pub fragments: Vec<BranchMergeParentColumnDepthFragmentSnapshot>,
}

/// A released multi-parent restore wave with complete labeled cell ladders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMergeMultiParentColumnRestoreWaveSnapshot {
    pub order: u64,
    pub ladders: Vec<BranchMergeParentColumnDepthLadderSnapshot>,
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
    /// A tabular depth wave does not name any peer tables.
    EmptyTabularDepthWave { order: u64 },
    /// A tabular column depth wave does not name any stable table columns.
    EmptyTabularColumnDepthWave { order: u64 },
    /// A multi-parent restore wave must carry at least two distinct parents.
    InsufficientTabularDepthParents { order: u64, actual: usize },
    /// One parent in a multi-parent wave has no table-column branches.
    EmptyParentColumnDepthWave { order: u64, parent: ObjectId },
    /// A tabular depth wave omits or repeats a table-local fragment index.
    IncompleteTabularDepthFragments { order: u64, table: ObjectId },
    /// A column branch omits or repeats one of its local depth positions.
    IncompleteTabularColumnDepthFragments {
        order: u64,
        table: ObjectId,
        column: ObjectId,
    },
    /// A parent column branch omits or repeats one local depth position.
    IncompleteParentColumnDepthFragments {
        order: u64,
        parent: ObjectId,
        table: ObjectId,
        column: ObjectId,
    },
    /// A committed peer table was restored with a different depth count.
    TabularFragmentCountMismatch {
        order: u64,
        table: ObjectId,
        expected: usize,
        actual: usize,
    },
    /// A committed column branch was restored with a different depth count.
    TabularColumnFragmentCountMismatch {
        order: u64,
        table: ObjectId,
        column: ObjectId,
        expected: usize,
        actual: usize,
    },
    /// A committed parent column was restored with a different depth count.
    ParentColumnFragmentCountMismatch {
        order: u64,
        parent: ObjectId,
        table: ObjectId,
        column: ObjectId,
        expected: usize,
        actual: usize,
    },
    /// One column fragment contains more than one value for a logical row.
    DuplicateColumnDepthRow {
        order: u64,
        table: ObjectId,
        column: ObjectId,
        fragment: usize,
    },
    /// A parent column fragment repeats one logical row key.
    DuplicateParentColumnDepthRow {
        order: u64,
        parent: ObjectId,
        table: ObjectId,
        column: ObjectId,
        fragment: usize,
    },
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
    TabularDepthWave(BranchMergeTabularDepthWave),
    TabularColumnDepthWave(BranchMergeTabularColumnDepthWave),
    MultiParentTabularColumnDepthWave(BranchMergeMultiParentTabularColumnDepthWave),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BranchMergeTombstoneSubmissionMode {
    WholePlan,
    DepthFragments,
    TabularDepthWave,
    TabularColumnDepthWave,
    MultiParentTabularColumnDepthWave,
}

type TabularFragmentRetryIdentity = BTreeMap<ObjectId, (usize, BTreeMap<usize, [u8; 32]>)>;
type TabularColumnFragmentRetryIdentity =
    BTreeMap<(ObjectId, ObjectId), (usize, BTreeMap<usize, [u8; 32]>)>;
type MultiParentColumnFragmentRetryIdentity = BTreeMap<
    ObjectId,
    BTreeMap<(ObjectId, ObjectId), (usize, BTreeMap<usize, [u8; 32]>)>,
>;

type PairedRetryPlanSignature = (u64, [u8; 32], [u8; 32]);
type DepthFragmentRetrySignature = (u64, usize, usize, [u8; 32]);

#[derive(Clone, Debug, Eq, PartialEq)]
struct AppliedDepthFragmentRetryTransaction {
    bindings: Vec<PairedRetryPlanSignature>,
    // Preserve the depth label and canonical delta identity while allowing
    // set-equivalent tombstones in a fragment to arrive in another order.
    recoveries: Vec<DepthFragmentRetrySignature>,
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
                        tombstone_delta_retry_identity(&step.ordered_row_tombstones),
                    )
                })
                .collect::<Vec<_>>();
            signatures.sort_unstable_by_key(|(order, _, _)| *order);
            signatures
        };
        let mut recoveries = recoveries
            .iter()
            .map(|recovery| {
                (
                    recovery.order,
                    recovery.fragment,
                    recovery.fragment_count,
                    tombstone_delta_retry_identity(&recovery.tombstones),
                )
            })
            .collect::<Vec<_>>();
        recoveries.sort_unstable();
        Self {
            bindings: plan_signatures(bindings),
            recoveries,
            appends: plan_signatures(appends),
        }
    }

    fn touches_order(&self, order: u64) -> bool {
        self.bindings
            .iter()
            .chain(&self.appends)
            .any(|(transaction_order, _, _)| *transaction_order == order)
            || self
                .recoveries
                .iter()
                .any(|recovery| recovery.0 == order)
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
    table_ladder_events: Vec<BranchMergeTableDepthLadderEvent>,
    pending_deltas: BTreeMap<u64, BufferedBranchMergeTombstoneDelta>,
    pending_plan_identities: BTreeMap<u64, [u8; 32]>,
    committed_modes: BTreeMap<u64, BranchMergeTombstoneSubmissionMode>,
    committed_fragment_counts: BTreeMap<u64, usize>,
    committed_fragment_retry_identities: BTreeMap<u64, BTreeMap<usize, [u8; 32]>>,
    committed_tabular_fragment_retry_identities: BTreeMap<u64, TabularFragmentRetryIdentity>,
    committed_tabular_column_fragment_retry_identities:
        BTreeMap<u64, TabularColumnFragmentRetryIdentity>,
    column_events: Vec<BranchMergeColumnDepthEvent>,
    column_ladder_events: Vec<BranchMergeColumnDepthLadderEvent>,
    committed_multi_parent_column_retry_identities:
        BTreeMap<u64, MultiParentColumnFragmentRetryIdentity>,
    parent_column_events: Vec<BranchMergeParentColumnDepthEvent>,
    parent_column_ladder_events: Vec<BranchMergeParentColumnDepthLadderEvent>,
    parent_column_ladder_waves: Vec<BranchMergeMultiParentColumnDepthLadderWaveEvent>,
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
            table_ladder_events: Vec::new(),
            pending_deltas: BTreeMap::new(),
            pending_plan_identities: BTreeMap::new(),
            committed_modes: BTreeMap::new(),
            committed_fragment_counts: BTreeMap::new(),
            committed_fragment_retry_identities: BTreeMap::new(),
            committed_tabular_fragment_retry_identities: BTreeMap::new(),
            committed_tabular_column_fragment_retry_identities: BTreeMap::new(),
            column_events: Vec::new(),
            column_ladder_events: Vec::new(),
            committed_multi_parent_column_retry_identities: BTreeMap::new(),
            parent_column_events: Vec::new(),
            parent_column_ladder_events: Vec::new(),
            parent_column_ladder_waves: Vec::new(),
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
    /// For an existing wave, a changed fragment count is reported before an
    /// index that is invalid under the caller's changed count, preserving the
    /// wave's authoritative depth label in diagnostics. After a wave commits,
    /// its count, valid index range, and per-fragment tombstone identity remain
    /// authoritative for stale retries too. Changed labels report
    /// `FragmentCountMismatch` or `InvalidFragment`, and changed fragment data
    /// reports `ConflictingSubmission`, before the generic stale-position
    /// error. MERGE-1 is silent on validating retry bodies after a fold; this
    /// v1 policy retains a fingerprint for each fragment's tombstone delta.
    pub fn submit_depth_merge_fragment(
        &mut self,
        order: u64,
        fragment: usize,
        fragment_count: usize,
        tombstones: &[(ObjectId, CanonicalValue)],
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        self.classify_submission_mode_conflict(
            order,
            BranchMergeTombstoneSubmissionMode::DepthFragments,
        )?;
        if let Some(expected_count) = self.committed_fragment_counts.get(&order).copied()
            && expected_count != fragment_count
        {
            return Err(BranchMergeTombstoneHistoryError::FragmentCountMismatch {
                order,
                expected: expected_count,
                actual: fragment_count,
            });
        }
        if let Some(expected_count) = self.committed_fragment_counts.get(&order).copied()
            && fragment >= expected_count
        {
            return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                fragment,
                fragment_count: expected_count,
            });
        }
        self.validate_committed_fragment_retry_identity(order, fragment, tombstones)?;
        self.classify_submission_position(
            order,
            BranchMergeTombstoneSubmissionMode::DepthFragments,
        )?;
        if fragment_count == 0 {
            return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                fragment,
                fragment_count,
            });
        }

        match self.pending_deltas.get(&order) {
            Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)) => unreachable!(
                "whole-plan mode conflicts are classified before fragment validation"
            ),
            Some(BufferedBranchMergeTombstoneDelta::TabularDepthWave(_)) => unreachable!(
                "tabular-wave mode conflicts are classified before fragment validation"
            ),
            Some(BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(_)) => unreachable!(
                "column-wave mode conflicts are classified before fragment validation"
            ),
            Some(BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(_)) => {
                unreachable!("multi-parent mode conflicts are classified before fragment validation")
            }
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
                if fragment >= fragment_count {
                    return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                        fragment,
                        fragment_count,
                    });
                }
                if fragments.contains_key(&fragment) {
                    return Err(BranchMergeTombstoneHistoryError::DuplicateFragment {
                        order,
                        fragment,
                    });
                }
            }
            None if fragment >= fragment_count => {
                return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                    fragment,
                    fragment_count,
                });
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

    /// Submits a complete paired wave with a table-local depth layout.
    ///
    /// A peer table may have a different fragment count from the other tables
    /// in the same paired restore. Every table must provide all indices in
    /// its declared count; empty fragments retain their depth identity. The
    /// wave is queued atomically behind earlier lineage positions and commits
    /// tombstones in table/key order. After release, a retry must preserve the
    /// peer table set, each table's fragment count, and each fragment's
    /// tombstone identity. [`Self::table_ladder_events`] publishes each
    /// table's complete label set, including empty positions, when the wave
    /// releases. MERGE-1 is silent on peer-table depth labels; this v1 policy
    /// preserves them independently across restore folds.
    pub fn submit_tabular_depth_wave(
        &mut self,
        wave: &BranchMergeTabularDepthWave,
    ) -> Result<Vec<BranchMergeTombstoneEvent>, BranchMergeTombstoneHistoryError> {
        let order = wave.order;
        self.classify_submission_mode_conflict(
            order,
            BranchMergeTombstoneSubmissionMode::TabularDepthWave,
        )?;

        if let Some(expected) = self.committed_tabular_fragment_retry_identities.get(&order) {
            if wave.tables.len() != expected.len() || wave.tables.keys().ne(expected.keys()) {
                return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
            }
            for (table, fragments) in &wave.tables {
                let (expected_count, _) = expected
                    .get(table)
                    .expect("the peer table set was checked above");
                if *expected_count != fragments.fragment_count {
                    return Err(
                        BranchMergeTombstoneHistoryError::TabularFragmentCountMismatch {
                            order,
                            table: *table,
                            expected: *expected_count,
                            actual: fragments.fragment_count,
                        },
                    );
                }
            }
        }

        let tombstones = tabular_depth_wave_tombstones(wave)?;
        if has_duplicate_tombstones_in_wave(&tombstones) {
            return Err(BranchMergeTombstoneHistoryError::DuplicateTombstone { order });
        }
        if let Some(expected) = self.committed_tabular_fragment_retry_identities.get(&order)
            && *expected != tabular_depth_wave_retry_identity(wave)
        {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }

        self.classify_submission_position(
            order,
            BranchMergeTombstoneSubmissionMode::TabularDepthWave,
        )?;
        if let Some(other_order) = self.pending_duplicate_order(order, &tombstones) {
            self.reserve_duplicate_retry_mode(
                order,
                BranchMergeTombstoneSubmissionMode::TabularDepthWave,
            );
            return Err(concurrent_duplicate_error(order, other_order));
        }

        self.duplicate_retry_modes.remove(&order);
        self.pending_deltas.insert(
            order,
            BufferedBranchMergeTombstoneDelta::TabularDepthWave(wave.clone()),
        );
        Ok(self.release_contiguous())
    }

    /// Returns complete depth labels for released stable table branches.
    ///
    /// A ladder event includes labels whose fragment had no keys. It is
    /// recorded only after its paired lineage prefix is contiguous and
    /// released.
    pub fn table_ladder_events(&self) -> &[BranchMergeTableDepthLadderEvent] {
        &self.table_ladder_events
    }

    /// Submits a paired restore wave whose depth labels belong to stable
    /// table-column branches. Each `(table, column)` supplies every local
    /// fragment index, while sibling columns may use different fragment
    /// counts. Complete future waves wait behind the missing paired prefix;
    /// release records their cells in table, column, depth, and primary-key
    /// order. Committed retries retain each column's count and per-fragment
    /// cell identity, so one branch cannot borrow another branch's labels.
    /// On release, [`Self::column_ladder_events`] also exposes every label,
    /// including positions whose fragments contain no cells.
    ///
    /// MERGE-1 specifies stable column identities and three-way row merge
    /// behavior, but is silent on restore-ladder labels across uneven column
    /// branches. This v1 policy gives each stable `(table, column)` pair its
    /// own complete depth range and keeps empty depths identity-bearing.
    pub fn submit_tabular_column_depth_wave(
        &mut self,
        wave: &BranchMergeTabularColumnDepthWave,
    ) -> Result<Vec<BranchMergeColumnDepthEvent>, BranchMergeTombstoneHistoryError> {
        let order = wave.order;
        self.classify_submission_mode_conflict(
            order,
            BranchMergeTombstoneSubmissionMode::TabularColumnDepthWave,
        )?;

        if let Some(expected) = self
            .committed_tabular_column_fragment_retry_identities
            .get(&order)
        {
            if wave.columns.len() != expected.len() || wave.columns.keys().ne(expected.keys()) {
                return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
            }
            for ((table, column), fragments) in &wave.columns {
                let (expected_count, _) = expected
                    .get(&(*table, *column))
                    .expect("the stable table-column set was checked above");
                if *expected_count != fragments.fragment_count {
                    return Err(
                        BranchMergeTombstoneHistoryError::TabularColumnFragmentCountMismatch {
                            order,
                            table: *table,
                            column: *column,
                            expected: *expected_count,
                            actual: fragments.fragment_count,
                        },
                    );
                }
            }
        }

        tabular_column_depth_wave_events(wave)?;
        let incoming_identity = tabular_column_depth_wave_retry_identity(wave);
        if let Some(expected) = self
            .committed_tabular_column_fragment_retry_identities
            .get(&order)
            && *expected != incoming_identity
        {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }
        if let Some(BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(existing)) =
            self.pending_deltas.get(&order)
        {
            if tabular_column_depth_wave_retry_identity(existing) != incoming_identity {
                return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
            }
            return Err(BranchMergeTombstoneHistoryError::DuplicateOrStale { order });
        }

        self.classify_submission_position(
            order,
            BranchMergeTombstoneSubmissionMode::TabularColumnDepthWave,
        )?;
        let first_new_event = self.column_events.len();
        self.pending_deltas.insert(
            order,
            BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(wave.clone()),
        );
        self.release_contiguous();
        Ok(self.column_events[first_new_event..].to_vec())
    }

    /// Returns the ordered column cells released by complete restore ladders.
    pub fn column_events(&self) -> &[BranchMergeColumnDepthEvent] {
        &self.column_events
    }

    /// Returns complete depth labels for released stable table-column branches.
    ///
    /// Unlike cell events, each ladder event includes labels whose fragment
    /// had no cells. A ladder is recorded only when its paired lineage prefix
    /// is contiguous and released.
    pub fn column_ladder_events(&self) -> &[BranchMergeColumnDepthLadderEvent] {
        &self.column_ladder_events
    }

    /// Returns committed column restore waves with every depth label bound to
    /// that fragment's cells, including empty fragments.
    ///
    /// Columns retain independent `(table, column)` ladders. Only waves whose
    /// paired lineage prefix has been released are materialized; the reference
    /// is silent about this storage view, so v1 keeps the labels independent
    /// and presents the complete wave as one snapshot.
    pub fn column_restore_waves(&self) -> Vec<BranchMergeTabularColumnRestoreWaveSnapshot> {
        let mut cells_by_depth = BTreeMap::new();
        for ladder in &self.column_ladder_events {
            for label in &ladder.depth_labels {
                cells_by_depth.insert(
                    (ladder.order, ladder.table, ladder.column, *label),
                    Vec::new(),
                );
            }
        }
        for event in &self.column_events {
            let cells = cells_by_depth
                .get_mut(&(event.order, event.table, event.column, event.fragment))
                .expect("released column cells have a retained ladder label");
            cells.push((event.key.clone(), event.value.clone()));
        }

        let mut waves = BTreeMap::new();
        for ladder in &self.column_ladder_events {
            let fragments = ladder
                .depth_labels
                .iter()
                .map(|label| {
                    let cells = cells_by_depth
                        .remove(&(ladder.order, ladder.table, ladder.column, *label))
                        .expect("released ladder labels have a cell bucket");
                    BranchMergeColumnDepthFragmentSnapshot {
                        label: *label,
                        cells,
                    }
                })
                .collect();
            waves
                .entry(ladder.order)
                .or_insert_with(Vec::new)
                .push(BranchMergeColumnDepthLadderSnapshot {
                    table: ladder.table,
                    column: ladder.column,
                    fragments,
                });
        }

        waves
            .into_iter()
            .map(|(order, columns)| BranchMergeTabularColumnRestoreWaveSnapshot {
                order,
                columns,
            })
            .collect()
    }

    /// Returns each stable table-column ladder folded across released waves.
    ///
    /// A fold keeps each wave's order and local labels intact: labels restart
    /// at zero for every wave and are never combined with labels from another
    /// order or sibling column. The reference is silent about this cross-wave
    /// view, so v1 folds by stable `(table, column)` identity only.
    pub fn column_restore_ladder_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestoreLadderFoldSnapshot> {
        let mut folds = BTreeMap::new();
        for wave in self.column_restore_waves() {
            for column in wave.columns {
                folds
                    .entry((column.table, column.column))
                    .or_insert_with(Vec::new)
                    .push(BranchMergeColumnDepthLadderWaveSnapshot {
                        order: wave.order,
                        fragments: column.fragments,
                    });
            }
        }
        folds
            .into_iter()
            .map(|((table, column), waves)| BranchMergeColumnRestoreLadderFoldSnapshot {
                table,
                column,
                waves,
            })
            .collect()
    }

    /// Returns dense timelines for stable columns present in released restore
    /// waves. Each timeline has one slot for every column-restore order; a
    /// missing column is `None`, while an empty labeled depth is present with
    /// no cells. The reference is silent about omitted-column slots, so v1
    /// distinguishes absence from an empty restore fragment.
    pub fn column_restore_ladder_timelines(
        &self,
    ) -> Vec<BranchMergeColumnRestoreLadderTimelineSnapshot> {
        let restore_waves = self.column_restore_waves();
        let orders = restore_waves
            .iter()
            .map(|wave| wave.order)
            .collect::<Vec<_>>();
        let mut present_ladders = BTreeMap::new();
        for wave in restore_waves {
            for column in wave.columns {
                present_ladders
                    .entry((column.table, column.column))
                    .or_insert_with(BTreeMap::new)
                    .insert(wave.order, column.fragments);
            }
        }

        present_ladders
            .into_iter()
            .map(|((table, column), mut present)| {
                let waves = orders
                    .iter()
                    .map(|order| BranchMergeColumnRestoreLadderWaveSlotSnapshot {
                        order: *order,
                        fragments: present.remove(order),
                    })
                    .collect();
                BranchMergeColumnRestoreLadderTimelineSnapshot {
                    table,
                    column,
                    waves,
                }
            })
            .collect()
    }

    /// Returns column-restore storms as maximal contiguous runs of paired
    /// column-wave orders. A different committed submission mode closes the
    /// current storm. Each result carries dense per-column timelines for its
    /// own order range. MERGE-1 is silent about storm boundaries; v1 uses the
    /// contiguous restore-mode run as the boundary policy.
    pub fn column_restore_storms(&self) -> Vec<BranchMergeColumnRestoreStormSnapshot> {
        let timelines = self.column_restore_ladder_timelines();
        let mut ranges = Vec::new();
        let mut first_order: Option<u64> = None;
        let mut last_order: Option<u64> = None;

        for (order, mode) in &self.committed_modes {
            if *mode == BranchMergeTombstoneSubmissionMode::TabularColumnDepthWave {
                let contiguous = last_order
                    .map(|last| last.checked_add(1) == Some(*order))
                    .unwrap_or(true);
                if first_order.is_some() && !contiguous {
                    ranges.push((
                        first_order.take().expect("storm has a first order"),
                        last_order.take().expect("storm has a last order"),
                    ));
                }
                if first_order.is_none() {
                    first_order = Some(*order);
                }
                last_order = Some(*order);
            } else if first_order.is_some() {
                ranges.push((
                    first_order.take().expect("storm has a first order"),
                    last_order.take().expect("storm has a last order"),
                ));
            }
        }
        if let (Some(first), Some(last)) = (first_order, last_order) {
            ranges.push((first, last));
        }

        ranges
            .into_iter()
            .map(|(first_order, last_order)| {
                let ladders = timelines
                    .iter()
                    .filter_map(|timeline| {
                        let waves = timeline
                            .waves
                            .iter()
                            .filter(|wave| wave.order >= first_order && wave.order <= last_order)
                            .cloned()
                            .collect::<Vec<_>>();
                        waves
                            .iter()
                            .any(|wave| wave.fragments.is_some())
                            .then(|| BranchMergeColumnRestoreLadderTimelineSnapshot {
                                table: timeline.table,
                                column: timeline.column,
                                waves,
                            })
                    })
                    .collect();
                BranchMergeColumnRestoreStormSnapshot {
                    first_order,
                    last_order,
                    ladders,
                }
            })
            .collect()
    }

    /// Folds each stable table-column pair across committed restore storms.
    ///
    /// Unlike `column_restore_ladder_timelines`, this view retains storm
    /// boundaries as explicit groups. Each storm has dense wave slots, and a
    /// column absent for the whole group is represented by `None` in every
    /// slot. MERGE-1 is silent about cross-storm label continuity; v1 keeps
    /// every storm's labels local and never carries a depth index across a
    /// non-column submission boundary.
    pub fn column_restore_storm_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestoreStormFoldSnapshot> {
        let storms = self.column_restore_storms();
        let mut identities = BTreeMap::new();
        for storm in &storms {
            for ladder in &storm.ladders {
                identities.insert((ladder.table, ladder.column), ());
            }
        }

        identities
            .into_keys()
            .map(|(table, column)| {
                let storm_folds = storms
                    .iter()
                    .map(|storm| {
                        let waves = storm
                            .ladders
                            .iter()
                            .find(|ladder| ladder.table == table && ladder.column == column)
                            .map(|ladder| ladder.waves.clone())
                            .unwrap_or_else(|| {
                                storm
                                    .ladders
                                    .first()
                                    .expect("a committed restore storm contains a column")
                                    .waves
                                    .iter()
                                    .map(|wave| BranchMergeColumnRestoreLadderWaveSlotSnapshot {
                                        order: wave.order,
                                        fragments: None,
                                    })
                                    .collect()
                            });
                        BranchMergeColumnRestoreStormLadderSnapshot {
                            first_order: storm.first_order,
                            last_order: storm.last_order,
                            waves,
                        }
                    })
                    .collect();
                BranchMergeColumnRestoreStormFoldSnapshot {
                    table,
                    column,
                    storms: storm_folds,
                }
            })
            .collect()
    }

    /// Returns the explicit paired depth-label roster for every present
    /// column ladder in every committed restore storm.
    ///
    /// Each result binds local labels to its stable `(table, column)` pair,
    /// storm index and order range, and restore-wave order. Empty fragments
    /// contribute their labels; omitted columns contribute no label record.
    /// This v1 projection does not renumber or align labels across waves.
    pub fn column_restore_storm_depth_labels(
        &self,
    ) -> Vec<BranchMergeColumnRestoreStormDepthLabelWaveSnapshot> {
        let mut labels = Vec::new();
        for (storm_index, storm) in self.column_restore_storms().into_iter().enumerate() {
            for ladder in storm.ladders {
                for wave in ladder.waves {
                    if let Some(fragments) = wave.fragments {
                        labels.push(BranchMergeColumnRestoreStormDepthLabelWaveSnapshot {
                            storm_index,
                            first_order: storm.first_order,
                            last_order: storm.last_order,
                            order: wave.order,
                            table: ladder.table,
                            column: ladder.column,
                            depth_labels: fragments
                                .into_iter()
                                .map(|fragment| fragment.label)
                                .collect(),
                        });
                    }
                }
            }
        }
        labels
    }

    /// Returns each stable column's canonical snapshot paths under its local
    /// depth label, keeping storm and restore-wave identity attached.
    ///
    /// Every present labeled fragment yields one record, including an empty
    /// path list for an empty fragment. Omitted columns yield no records. This
    /// v1 view treats the canonical row key as snapshot-path identity and
    /// never carries that identity across labels, waves, columns, or storms.
    pub fn column_restore_storm_depth_paths(
        &self,
    ) -> Vec<BranchMergeColumnRestoreStormDepthPathSnapshot> {
        let mut paths = Vec::new();
        for (storm_index, storm) in self.column_restore_storms().into_iter().enumerate() {
            for ladder in storm.ladders {
                for wave in ladder.waves {
                    if let Some(fragments) = wave.fragments {
                        for fragment in fragments {
                            paths.push(BranchMergeColumnRestoreStormDepthPathSnapshot {
                                storm_index,
                                first_order: storm.first_order,
                                last_order: storm.last_order,
                                order: wave.order,
                                table: ladder.table,
                                column: ladder.column,
                                label: fragment.label,
                                snapshot_paths: fragment
                                    .cells
                                    .into_iter()
                                    .map(|(path, _value)| path)
                                    .collect(),
                            });
                        }
                    }
                }
            }
        }
        paths
    }

    /// Returns dense depth-path slots for each stable column across all
    /// committed restore storms.
    ///
    /// Every storm and wave slot remains paired with its stable `(table,
    /// column)` identity. An omitted column is `None`; a present fragment
    /// keeps its local label and canonical row-key paths, including an empty
    /// path list. Labels restart for each wave and never carry across an
    /// omission or storm boundary. MERGE-1 is silent about this extension;
    /// v1 derives slots from the committed column folds without filling an
    /// omitted slot from a sibling or earlier wave.
    pub fn column_restore_storm_depth_path_slots(
        &self,
    ) -> Vec<BranchMergeColumnRestoreStormDepthPathWaveSlotSnapshot> {
        let mut slots = Vec::new();
        for fold in self.column_restore_storm_folds() {
            for (storm_index, storm) in fold.storms.into_iter().enumerate() {
                for wave in storm.waves {
                    slots.push(BranchMergeColumnRestoreStormDepthPathWaveSlotSnapshot {
                        storm_index,
                        first_order: storm.first_order,
                        last_order: storm.last_order,
                        order: wave.order,
                        table: fold.table,
                        column: fold.column,
                        fragments: wave.fragments.map(|fragments| {
                            fragments
                                .into_iter()
                                .map(|fragment| {
                                    BranchMergeColumnRestoreStormDepthPathFragmentSnapshot {
                                        label: fragment.label,
                                        snapshot_paths: fragment
                                            .cells
                                            .into_iter()
                                            .map(|(path, _value)| path)
                                            .collect(),
                                    }
                                })
                                .collect()
                        }),
                    });
                }
            }
        }
        slots
    }

    /// Folds each canonical snapshot path through every depth wave of its
    /// stable table-column pair in each restore storm.
    ///
    /// Paths are discovered from the committed cells, then each result gets a
    /// dense wave timeline. `None` marks an omitted column; `Some(Vec::new())`
    /// marks a present column where that exact path was omitted. A path found
    /// in multiple depths lists every local label, so a sibling path cannot
    /// adopt its identity. Storm boundaries remain independent. MERGE-1 is
    /// silent about this inverse view; v1 keeps one fold per stable column and
    /// canonical path within each storm. Paths follow their first occurrence
    /// while scanning restore order, depth label, then retained cell order.
    pub fn column_restore_storm_snapshot_path_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestoreStormSnapshotPathFoldSnapshot> {
        let mut folds = Vec::new();
        for column_fold in self.column_restore_storm_folds() {
            for (storm_index, storm) in column_fold.storms.into_iter().enumerate() {
                let mut snapshot_paths = Vec::new();
                for wave in &storm.waves {
                    if let Some(fragments) = &wave.fragments {
                        for (path, _) in fragments
                            .iter()
                            .flat_map(|fragment| fragment.cells.iter())
                        {
                            if !snapshot_paths.contains(path) {
                                snapshot_paths.push(path.clone());
                            }
                        }
                    }
                }
                for snapshot_path in snapshot_paths {
                    let waves = storm
                        .waves
                        .iter()
                        .map(|wave| {
                            let depth_labels = wave.fragments.as_ref().map(|fragments| {
                                fragments
                                    .iter()
                                    .filter_map(|fragment| {
                                        fragment
                                            .cells
                                            .iter()
                                            .any(|(path, _)| path == &snapshot_path)
                                            .then_some(fragment.label)
                                    })
                                    .collect()
                            });
                            BranchMergeColumnRestoreStormSnapshotPathWaveSlotSnapshot {
                                order: wave.order,
                                depth_labels,
                            }
                        })
                        .collect();
                    folds.push(BranchMergeColumnRestoreStormSnapshotPathFoldSnapshot {
                        storm_index,
                        first_order: storm.first_order,
                        last_order: storm.last_order,
                        table: column_fold.table,
                        column: column_fold.column,
                        snapshot_path,
                        waves,
                    });
                }
            }
        }
        folds
    }

    /// Folds each stable canonical snapshot path through the complete chain
    /// of restore storms for its table-column pair.
    ///
    /// Unlike per-storm path folds, this view keeps one path identity across
    /// every storm while retaining explicit storm ranges. Every storm has
    /// dense wave slots: an omitted column remains `None`, while a present
    /// column where the path is absent remains `Some(Vec::new())`. Depth
    /// labels stay local to their wave and do not flow through omitted slots.
    /// MERGE-1 is silent about this chained fold, so v1 groups by stable
    /// `(table, column, canonical row key)` identity and keeps every storm's
    /// local depth roster unchanged.
    pub fn column_restore_snapshot_path_chain_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestoreSnapshotPathChainFoldSnapshot> {
        let mut folds = Vec::new();
        for column_fold in self.column_restore_storm_folds() {
            let mut snapshot_paths = Vec::new();
            for storm in &column_fold.storms {
                for wave in &storm.waves {
                    if let Some(fragments) = &wave.fragments {
                        for (path, _) in fragments
                            .iter()
                            .flat_map(|fragment| fragment.cells.iter())
                        {
                            if !snapshot_paths.contains(path) {
                                snapshot_paths.push(path.clone());
                            }
                        }
                    }
                }
            }

            for snapshot_path in snapshot_paths {
                let storms = column_fold
                    .storms
                    .iter()
                    .enumerate()
                    .map(|(storm_index, storm)| {
                        let waves = storm
                            .waves
                            .iter()
                            .map(|wave| {
                                let depth_labels = wave.fragments.as_ref().map(|fragments| {
                                    fragments
                                        .iter()
                                        .filter_map(|fragment| {
                                            fragment
                                                .cells
                                                .iter()
                                                .any(|(path, _)| path == &snapshot_path)
                                                .then_some(fragment.label)
                                        })
                                        .collect()
                                });
                                BranchMergeColumnRestoreStormSnapshotPathWaveSlotSnapshot {
                                    order: wave.order,
                                    depth_labels,
                                }
                            })
                            .collect();
                        BranchMergeColumnRestoreSnapshotPathStormSnapshot {
                            storm_index,
                            first_order: storm.first_order,
                            last_order: storm.last_order,
                            waves,
                        }
                    })
                    .collect();
                folds.push(BranchMergeColumnRestoreSnapshotPathChainFoldSnapshot {
                    table: column_fold.table,
                    column: column_fold.column,
                    snapshot_path,
                    storms,
                });
            }
        }
        folds
    }

    /// Pairs each canonical snapshot path across all stable columns in its
    /// table while preserving each column's complete restore-chain timeline.
    ///
    /// A path observed in any column is included for every stable column in
    /// that table. Each column retains dense storm and wave slots: `None`
    /// means that column was omitted, while `Some(Vec::new())` means the
    /// column was present but that path was absent. Repeated occurrences keep
    /// their own local depth labels, and labels never migrate between sibling
    /// columns, storms, or waves. MERGE-1 is silent about this paired inverse
    /// view; v1 aligns only the canonical row-key identity and keeps all
    /// column-local depth facts unchanged.
    pub fn column_restore_paired_snapshot_path_chain_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestorePairedSnapshotPathChainFoldSnapshot> {
        let column_folds = self.column_restore_storm_folds();
        let mut identities = Vec::<(ObjectId, CanonicalValue)>::new();

        for column_fold in &column_folds {
            for storm in &column_fold.storms {
                for wave in &storm.waves {
                    if let Some(fragments) = &wave.fragments {
                        for (snapshot_path, _) in fragments
                            .iter()
                            .flat_map(|fragment| fragment.cells.iter())
                        {
                            let identity = (column_fold.table, snapshot_path.clone());
                            if !identities.contains(&identity) {
                                identities.push(identity);
                            }
                        }
                    }
                }
            }
        }

        identities
            .into_iter()
            .map(|(table, snapshot_path)| {
                let columns = column_folds
                    .iter()
                    .filter(|column_fold| column_fold.table == table)
                    .map(|column_fold| {
                        let storms = column_fold
                            .storms
                            .iter()
                            .enumerate()
                            .map(|(storm_index, storm)| {
                                let waves = storm
                                    .waves
                                    .iter()
                                    .map(|wave| {
                                        let depth_labels =
                                            wave.fragments.as_ref().map(|fragments| {
                                                fragments
                                                    .iter()
                                                    .filter_map(|fragment| {
                                                        fragment
                                                            .cells
                                                            .iter()
                                                            .any(|(path, _)| {
                                                                path == &snapshot_path
                                                            })
                                                            .then_some(fragment.label)
                                                    })
                                                    .collect()
                                            });
                                        BranchMergeColumnRestoreStormSnapshotPathWaveSlotSnapshot {
                                            order: wave.order,
                                            depth_labels,
                                        }
                                    })
                                    .collect();
                                BranchMergeColumnRestoreSnapshotPathStormSnapshot {
                                    storm_index,
                                    first_order: storm.first_order,
                                    last_order: storm.last_order,
                                    waves,
                                }
                            })
                            .collect();
                        BranchMergeColumnRestorePairedSnapshotPathColumnFoldSnapshot {
                            column: column_fold.column,
                            storms,
                        }
                    })
                    .collect();
                BranchMergeColumnRestorePairedSnapshotPathChainFoldSnapshot {
                    table,
                    snapshot_path,
                    columns,
                }
            })
            .collect()
    }

    /// Pairs every strict slash-delimited text-key extension with its prefix
    /// across all stable columns, storms, and restore waves.
    ///
    /// Both paths retain independent local depth-label lists in every wave.
    /// A missing column stays `None` on both sides; when the column is present,
    /// an absent prefix or extension is `Some(Vec::new())`. Only a canonical
    /// text key beginning with the exact prefix plus `/` is considered an
    /// extension, so `rooted` is not an extension of `root`. MERGE-1 specifies
    /// merge identity but is silent about this paired extension projection;
    /// v1 applies the relation only to this read-only view and never rewrites
    /// row keys or aligns sibling columns' depth labels.
    pub fn column_restore_paired_path_extension_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestorePairedPathExtensionFoldSnapshot> {
        let path_folds = self.column_restore_paired_snapshot_path_chain_folds();
        let mut extension_folds = Vec::new();

        for prefix_fold in &path_folds {
            for extension_fold in &path_folds {
                if prefix_fold.table != extension_fold.table {
                    continue;
                }
                let is_extension = match (
                    prefix_fold.snapshot_path.raw(),
                    extension_fold.snapshot_path.raw(),
                ) {
                    (OvbRaw::Text(prefix), OvbRaw::Text(extension)) => extension
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('/') && suffix.len() > 1),
                    _ => false,
                };
                if !is_extension {
                    continue;
                }

                let columns = prefix_fold
                    .columns
                    .iter()
                    .map(|prefix_column| {
                        let extension_column = extension_fold
                            .columns
                            .iter()
                            .find(|column| column.column == prefix_column.column)
                            .expect("paired path folds share a table's stable column roster");
                        debug_assert_eq!(
                            prefix_column.storms.len(),
                            extension_column.storms.len()
                        );
                        let storms = prefix_column
                            .storms
                            .iter()
                            .zip(&extension_column.storms)
                            .map(|(prefix_storm, extension_storm)| {
                                debug_assert_eq!(
                                    prefix_storm.storm_index,
                                    extension_storm.storm_index
                                );
                                debug_assert_eq!(
                                    prefix_storm.first_order,
                                    extension_storm.first_order
                                );
                                debug_assert_eq!(
                                    prefix_storm.last_order,
                                    extension_storm.last_order
                                );
                                debug_assert_eq!(
                                    prefix_storm.waves.len(),
                                    extension_storm.waves.len()
                                );
                                let waves = prefix_storm
                                    .waves
                                    .iter()
                                    .zip(&extension_storm.waves)
                                    .map(|(prefix_wave, extension_wave)| {
                                        debug_assert_eq!(prefix_wave.order, extension_wave.order);
                                        BranchMergeColumnRestorePairedPathExtensionWaveSlotSnapshot {
                                            order: prefix_wave.order,
                                            prefix_depth_labels: prefix_wave.depth_labels.clone(),
                                            extension_depth_labels: extension_wave
                                                .depth_labels
                                                .clone(),
                                        }
                                    })
                                    .collect();
                                BranchMergeColumnRestorePairedPathExtensionStormSnapshot {
                                    storm_index: prefix_storm.storm_index,
                                    first_order: prefix_storm.first_order,
                                    last_order: prefix_storm.last_order,
                                    waves,
                                }
                            })
                            .collect();
                        BranchMergeColumnRestorePairedPathExtensionColumnFoldSnapshot {
                            column: prefix_column.column,
                            storms,
                        }
                    })
                    .collect();
                extension_folds.push(BranchMergeColumnRestorePairedPathExtensionFoldSnapshot {
                    table: prefix_fold.table,
                    prefix_path: prefix_fold.snapshot_path.clone(),
                    extension_path: extension_fold.snapshot_path.clone(),
                    columns,
                });
            }
        }

        extension_folds
    }

    /// Folds each canonical snapshot path across paired stable columns with
    /// an explicit state for every restore-wave slot.
    ///
    /// The path key remains on the fold when waves omit the column or the
    /// path. Those cases become `ColumnOmitted` and `PathOmitted`, while an
    /// observed path carries only its own local depth labels. MERGE-1 is
    /// silent about this typed omission view; v1 derives it from the paired
    /// path history without changing row identity or depth assignment.
    pub fn column_restore_paired_snapshot_path_occurrence_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestorePairedSnapshotPathOccurrenceFoldSnapshot> {
        self.column_restore_paired_snapshot_path_chain_folds()
            .into_iter()
            .map(|fold| {
                let columns = fold
                    .columns
                    .into_iter()
                    .map(|column| {
                        let storms = column
                            .storms
                            .into_iter()
                            .map(|storm| {
                                let waves = storm
                                    .waves
                                    .into_iter()
                                    .map(|wave| {
                                        let occurrence = match wave.depth_labels {
                                            None => BranchMergeSnapshotPathOccurrence::ColumnOmitted,
                                            Some(labels) if labels.is_empty() => BranchMergeSnapshotPathOccurrence::PathOmitted,
                                            Some(labels) => BranchMergeSnapshotPathOccurrence::DepthLabels(labels),
                                        };
                                        BranchMergeColumnRestoreSnapshotPathOccurrenceWaveSnapshot {
                                            order: wave.order,
                                            occurrence,
                                        }
                                    })
                                    .collect();
                                BranchMergeColumnRestoreSnapshotPathOccurrenceStormSnapshot {
                                    storm_index: storm.storm_index,
                                    first_order: storm.first_order,
                                    last_order: storm.last_order,
                                    waves,
                                }
                            })
                            .collect();
                        BranchMergeColumnRestorePairedSnapshotPathOccurrenceColumnFoldSnapshot {
                            column: column.column,
                            storms,
                        }
                    })
                    .collect();
                BranchMergeColumnRestorePairedSnapshotPathOccurrenceFoldSnapshot {
                    table: fold.table,
                    snapshot_path: fold.snapshot_path,
                    columns,
                }
            })
            .collect()
    }

    /// Folds each strict text-path extension pair with typed occurrence states
    /// for both paths in every paired column restore wave.
    ///
    /// Prefix and extension keep independent local labels. For each side,
    /// `ColumnOmitted` identifies an absent column, `PathOmitted` identifies a
    /// present column without that key, and `DepthLabels` names every fragment
    /// containing the key. MERGE-1 is silent about this combined paired view;
    /// v1 reuses the slash-boundary extension relation of
    /// [`Self::column_restore_paired_path_extension_folds`] and does not alter
    /// canonical keys or align sibling depth labels.
    pub fn column_restore_paired_path_extension_occurrence_folds(
        &self,
    ) -> Vec<BranchMergeColumnRestorePairedPathExtensionOccurrenceFoldSnapshot> {
        self.column_restore_paired_path_extension_folds()
            .into_iter()
            .map(|fold| {
                let columns = fold
                    .columns
                    .into_iter()
                    .map(|column| {
                        let storms = column
                            .storms
                            .into_iter()
                            .map(|storm| {
                                let waves = storm
                                    .waves
                                    .into_iter()
                                    .map(|wave| {
                                        let occurrence = |labels: Option<Vec<usize>>| match labels {
                                            None => BranchMergeSnapshotPathOccurrence::ColumnOmitted,
                                            Some(labels) if labels.is_empty() => BranchMergeSnapshotPathOccurrence::PathOmitted,
                                            Some(labels) => BranchMergeSnapshotPathOccurrence::DepthLabels(labels),
                                        };
                                        BranchMergeColumnRestorePairedPathExtensionOccurrenceWaveSnapshot {
                                            order: wave.order,
                                            prefix_occurrence: occurrence(wave.prefix_depth_labels),
                                            extension_occurrence: occurrence(wave.extension_depth_labels),
                                        }
                                    })
                                    .collect();
                                BranchMergeColumnRestorePairedPathExtensionOccurrenceStormSnapshot {
                                    storm_index: storm.storm_index,
                                    first_order: storm.first_order,
                                    last_order: storm.last_order,
                                    waves,
                                }
                            })
                            .collect();
                        BranchMergeColumnRestorePairedPathExtensionOccurrenceColumnFoldSnapshot {
                            column: column.column,
                            storms,
                        }
                    })
                    .collect();
                BranchMergeColumnRestorePairedPathExtensionOccurrenceFoldSnapshot {
                    table: fold.table,
                    prefix_path: fold.prefix_path,
                    extension_path: fold.extension_path,
                    columns,
                }
            })
            .collect()
    }

    /// Submits a complete paired restore wave from at least two distinct
    /// parent branches. Each parent and stable table-column pair retains its
    /// own fragment count and cell retry identity. Parent provenance remains
    /// attached to released cells, even when parents restore the same row and
    /// column with equal or different values. Complete future waves wait for
    /// their lineage prefix, then release in parent, table, column, depth, and
    /// primary-key order. [`Self::parent_column_ladder_events`] exposes each
    /// parent's complete table-column labels, including empty positions, once
    /// that wave releases.
    ///
    /// The reference defines stable column identities and three-way merge
    /// behavior, but is silent about depth labels across multi-parent restore
    /// ladders. This v1 policy treats each parent as an independent source and
    /// never aligns its fragment indexes with a sibling parent.
    pub fn submit_multi_parent_tabular_column_depth_wave(
        &mut self,
        wave: &BranchMergeMultiParentTabularColumnDepthWave,
    ) -> Result<Vec<BranchMergeParentColumnDepthEvent>, BranchMergeTombstoneHistoryError> {
        let order = wave.order;
        self.classify_submission_mode_conflict(
            order,
            BranchMergeTombstoneSubmissionMode::MultiParentTabularColumnDepthWave,
        )?;

        if let Some(expected) = self
            .committed_multi_parent_column_retry_identities
            .get(&order)
        {
            if wave.parents.len() != expected.len() || wave.parents.keys().ne(expected.keys()) {
                return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
            }
            for (parent, columns) in &wave.parents {
                let expected_columns = expected
                    .get(parent)
                    .expect("the parent identity set was checked above");
                if columns.len() != expected_columns.len()
                    || columns.keys().ne(expected_columns.keys())
                {
                    return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
                }
                for ((table, column), depth) in columns {
                    let (expected_count, _) = expected_columns
                        .get(&(*table, *column))
                        .expect("the parent-local column set was checked above");
                    if *expected_count != depth.fragment_count {
                        return Err(
                            BranchMergeTombstoneHistoryError::ParentColumnFragmentCountMismatch {
                                order,
                                parent: *parent,
                                table: *table,
                                column: *column,
                                expected: *expected_count,
                                actual: depth.fragment_count,
                            },
                        );
                    }
                }
            }
        }

        parent_column_depth_wave_events(wave)?;
        let incoming_identity = parent_column_depth_wave_retry_identity(wave);
        if let Some(expected) = self
            .committed_multi_parent_column_retry_identities
            .get(&order)
            && *expected != incoming_identity
        {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }
        if let Some(BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(existing)) =
            self.pending_deltas.get(&order)
        {
            if parent_column_depth_wave_retry_identity(existing) != incoming_identity {
                return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
            }
            return Err(BranchMergeTombstoneHistoryError::DuplicateOrStale { order });
        }

        self.classify_submission_position(
            order,
            BranchMergeTombstoneSubmissionMode::MultiParentTabularColumnDepthWave,
        )?;
        let first_new_event = self.parent_column_events.len();
        self.pending_deltas.insert(
            order,
            BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(wave.clone()),
        );
        self.release_contiguous();
        Ok(self.parent_column_events[first_new_event..].to_vec())
    }

    /// Returns ordered cells released with their source parent identities.
    pub fn parent_column_events(&self) -> &[BranchMergeParentColumnDepthEvent] {
        &self.parent_column_events
    }

    /// Returns complete depth labels for released parent-local column branches.
    ///
    /// A ladder event includes labels whose fragment had no cells. It is
    /// recorded only after its paired lineage prefix is contiguous and
    /// released.
    pub fn parent_column_ladder_events(&self) -> &[BranchMergeParentColumnDepthLadderEvent] {
        &self.parent_column_ladder_events
    }

    /// Returns complete parent-local ladder rosters grouped by released wave.
    ///
    /// Each entry appears only after its paired lineage prefix is contiguous.
    /// This v1 policy treats parents as independent sources and publishes all
    /// of a wave's table-column ladders together, without aligning their depth
    /// labels across parents.
    pub fn parent_column_ladder_waves(
        &self,
    ) -> &[BranchMergeMultiParentColumnDepthLadderWaveEvent] {
        &self.parent_column_ladder_waves
    }

    /// Returns committed multi-parent column waves with every depth label
    /// bound to that fragment's cells.
    ///
    /// Empty fragment positions are returned with no cells. Ladders retain
    /// their parent/table/column identity and do not align depths across
    /// parents. This is a materialized snapshot over the released event
    /// history; pending waves are not visible.
    pub fn parent_column_restore_waves(
        &self,
    ) -> Vec<BranchMergeMultiParentColumnRestoreWaveSnapshot> {
        let mut cells_by_depth = BTreeMap::new();
        for wave in &self.parent_column_ladder_waves {
            for ladder in &wave.ladders {
                for label in &ladder.depth_labels {
                    cells_by_depth.insert(
                        (wave.order, ladder.parent, ladder.table, ladder.column, *label),
                        Vec::new(),
                    );
                }
            }
        }
        for event in &self.parent_column_events {
            let cells = cells_by_depth
                .get_mut(&(
                    event.order,
                    event.parent,
                    event.table,
                    event.column,
                    event.fragment,
                ))
                .expect("released parent column cells have a retained ladder label");
            cells.push((event.key.clone(), event.value.clone()));
        }

        self.parent_column_ladder_waves
            .iter()
            .map(|wave| {
                let ladders = wave
                    .ladders
                    .iter()
                    .map(|ladder| {
                        let fragments = ladder
                            .depth_labels
                            .iter()
                            .map(|label| {
                                let cells = cells_by_depth
                                    .remove(&(
                                        wave.order,
                                        ladder.parent,
                                        ladder.table,
                                        ladder.column,
                                        *label,
                                    ))
                                    .expect("released ladder labels have a cell bucket");
                                BranchMergeParentColumnDepthFragmentSnapshot {
                                    label: *label,
                                    cells,
                                }
                            })
                            .collect();
                        BranchMergeParentColumnDepthLadderSnapshot {
                            parent: ladder.parent,
                            table: ladder.table,
                            column: ladder.column,
                            fragments,
                        }
                    })
                    .collect();
                BranchMergeMultiParentColumnRestoreWaveSnapshot {
                    order: wave.order,
                    ladders,
                }
            })
            .collect()
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
    /// Recovery receipt identity includes each fragment's order, index, count,
    /// and canonical tombstone delta. Tombstone ordering inside one fragment
    /// does not change the receipt identity because released deltas are
    /// normalized by table and key. Paired append projections use the same
    /// order-insensitive delta identity alongside the full paired-plan body.
    /// Before changing paired identities, the batch checks fragment-count
    /// labels against every buffered wave in lineage order. This lets a stale
    /// label in an earlier wave remain visible even if a later binding would
    /// also conflict; all paired changes still commit atomically afterward.
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
        self.validate_depth_fragment_labels(recoveries)?;

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
    /// Combined retry receipts that referenced this wave are invalidated, so
    /// replay restores fragments cleared here.
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
            }
            _ => return Err(BranchMergeTombstoneHistoryError::NoIncompleteDepthWave { order }),
        }
        // Restart erased recovery fragments, so a prior combined-operation
        // receipt touching this wave must not turn its replay into a no-op.
        // Replaying the transaction rechecks its paired plans and restores the
        // selected fragments while leaving unrelated cascade identities bound.
        self.applied_fragment_retry_transactions
            .retain(|transaction| !transaction.touches_order(order));
        Ok(())
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
    /// A committed depth wave keeps its fragment-count retry label, so a later
    /// recovery with a changed count reports `FragmentCountMismatch` rather
    /// than losing the label when an uneven cascade folds. MERGE-1 is silent
    /// on retaining labels after folds; this v1 policy preserves each wave's
    /// declared count for deterministic stale-retry diagnostics.
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
        self.validate_depth_fragment_labels(&recoveries)?;

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
        if recovery.fragment_count == 0 {
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
                if recovery.fragment >= recovery.fragment_count {
                    return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                        fragment: recovery.fragment,
                        fragment_count: recovery.fragment_count,
                    });
                }
                fragments
            }
            Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)) => unreachable!(
                "whole-plan mode conflicts are classified before fragment recovery"
            ),
            Some(BufferedBranchMergeTombstoneDelta::TabularDepthWave(_)) => unreachable!(
                "tabular-wave mode conflicts are classified before fragment recovery"
            ),
            Some(BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(_)) => unreachable!(
                "column-wave mode conflicts are classified before fragment recovery"
            ),
            Some(BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(_)) => {
                unreachable!("multi-parent mode conflicts are classified before fragment recovery")
            }
            None => {
                if recovery.fragment >= recovery.fragment_count {
                    return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                        fragment: recovery.fragment,
                        fragment_count: recovery.fragment_count,
                    });
                }
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

    fn validate_depth_fragment_labels(
        &self,
        recoveries: &[BranchMergeDepthFragmentRecovery],
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let mut recoveries = recoveries.iter().collect::<Vec<_>>();
        recoveries.sort_unstable_by_key(|recovery| (recovery.order, recovery.fragment));
        if recoveries.windows(2).any(|pair| {
            pair[0].order == pair[1].order && pair[0].fragment == pair[1].fragment
        }) {
            // The public recovery API reports duplicate labels after paired
            // bindings. Leave that established structural-error precedence
            // intact instead of preflighting a partial label view.
            return Ok(());
        }

        for recovery in recoveries {
            let expected_count = match self.pending_deltas.get(&recovery.order) {
                Some(BufferedBranchMergeTombstoneDelta::DepthFragments {
                    fragment_count,
                    ..
                }) => Some(*fragment_count),
                Some(BufferedBranchMergeTombstoneDelta::WholePlan(_)) => None,
                Some(BufferedBranchMergeTombstoneDelta::TabularDepthWave(_)) => None,
                Some(BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(_)) => None,
                Some(BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(_)) => None,
                None => self.committed_fragment_counts.get(&recovery.order).copied(),
            };
            let Some(expected_count) = expected_count else {
                continue;
            };

            if recovery.fragment_count == 0 {
                return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                    fragment: recovery.fragment,
                    fragment_count: recovery.fragment_count,
                });
            }
            if expected_count != recovery.fragment_count {
                return Err(BranchMergeTombstoneHistoryError::FragmentCountMismatch {
                    order: recovery.order,
                    expected: expected_count,
                    actual: recovery.fragment_count,
                });
            }
            if recovery.fragment >= recovery.fragment_count {
                return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                    fragment: recovery.fragment,
                    fragment_count: recovery.fragment_count,
                });
            }
            self.validate_committed_fragment_retry_identity(
                recovery.order,
                recovery.fragment,
                &recovery.tombstones,
            )?;
        }
        Ok(())
    }

    fn validate_committed_fragment_retry_identity(
        &self,
        order: u64,
        fragment: usize,
        tombstones: &[(ObjectId, CanonicalValue)],
    ) -> Result<(), BranchMergeTombstoneHistoryError> {
        let Some(expected_identity) = self
            .committed_fragment_retry_identities
            .get(&order)
            .and_then(|fragments| fragments.get(&fragment))
        else {
            return Ok(());
        };
        if *expected_identity != tombstone_delta_retry_identity(tombstones) {
            return Err(BranchMergeTombstoneHistoryError::ConflictingSubmission { order });
        }
        Ok(())
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
                Some(BufferedBranchMergeTombstoneDelta::TabularDepthWave(_)) => true,
                Some(BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(_)) => true,
                Some(BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(_)) => true,
                None => false,
            };
            if !ready {
                break;
            }
            let delta = self.pending_deltas.remove(&order).expect("ready delta is buffered");
            let mode = delta.submission_mode();
            if let BufferedBranchMergeTombstoneDelta::DepthFragments {
                fragment_count,
                fragments,
            } = &delta
            {
                // The fold drops buffered fragments, but retry diagnostics
                // still need the wave's label after its events commit.
                self.committed_fragment_counts
                    .insert(order, *fragment_count);
                self.committed_fragment_retry_identities.insert(
                    order,
                    fragments
                        .iter()
                        .map(|(fragment, tombstones)| {
                            (*fragment, tombstone_delta_retry_identity(tombstones))
                        })
                        .collect(),
                );
            }
            if let BufferedBranchMergeTombstoneDelta::TabularDepthWave(wave) = &delta {
                self.committed_tabular_fragment_retry_identities
                    .insert(order, tabular_depth_wave_retry_identity(wave));
            }
            if let BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(wave) = &delta {
                self.committed_tabular_column_fragment_retry_identities
                    .insert(order, tabular_column_depth_wave_retry_identity(wave));
            }
            if let BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(wave) = &delta {
                self.committed_multi_parent_column_retry_identities
                    .insert(order, parent_column_depth_wave_retry_identity(wave));
            }
            self.duplicate_retry_modes.remove(&order);
            if let Some(identity) = self.pending_plan_identities.remove(&order) {
                self.committed_plan_identities.insert(order, identity);
            } else if mode == BranchMergeTombstoneSubmissionMode::WholePlan {
                unreachable!("whole-plan identity is buffered with its tombstone delta");
            }
            let mut column_events = Vec::new();
            let mut column_ladder_events = Vec::new();
            let mut parent_column_events = Vec::new();
            let mut parent_column_ladder_events = Vec::new();
            let mut parent_column_ladder_waves = Vec::new();
            let mut table_ladder_events = Vec::new();
            let mut tombstones = match delta {
                BufferedBranchMergeTombstoneDelta::WholePlan(tombstones) => tombstones,
                BufferedBranchMergeTombstoneDelta::DepthFragments { fragments, .. } => {
                    fragments.into_values().flatten().collect()
                }
                BufferedBranchMergeTombstoneDelta::TabularDepthWave(wave) => {
                    table_ladder_events = tabular_depth_wave_ladder_events(&wave);
                    tabular_depth_wave_tombstones(&wave)
                        .expect("buffered tabular depth waves were validated before insertion")
                }
                BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(wave) => {
                    column_events = tabular_column_depth_wave_events(&wave)
                        .expect("buffered column waves were validated before insertion");
                    column_ladder_events = tabular_column_depth_ladder_events(&wave);
                    Vec::new()
                }
                BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(wave) => {
                    parent_column_events = parent_column_depth_wave_events(&wave)
                        .expect("buffered multi-parent waves were validated before insertion");
                    parent_column_ladder_events =
                        parent_column_depth_wave_ladder_events(&wave);
                    parent_column_ladder_waves.push(
                        BranchMergeMultiParentColumnDepthLadderWaveEvent {
                            order: wave.order,
                            ladders: parent_column_ladder_events.clone(),
                        },
                    );
                    Vec::new()
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
            self.table_ladder_events.extend(table_ladder_events);
            self.column_events.extend(column_events);
            self.column_ladder_events.extend(column_ladder_events);
            self.parent_column_events.extend(parent_column_events);
            self.parent_column_ladder_events
                .extend(parent_column_ladder_events);
            self.parent_column_ladder_waves
                .extend(parent_column_ladder_waves);
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
            | (
                Some(BufferedBranchMergeTombstoneDelta::TabularDepthWave(_)),
                BranchMergeTombstoneSubmissionMode::TabularDepthWave,
            )
            | (
                Some(BufferedBranchMergeTombstoneDelta::TabularColumnDepthWave(_)),
                BranchMergeTombstoneSubmissionMode::TabularColumnDepthWave,
            )
            | (
                Some(BufferedBranchMergeTombstoneDelta::MultiParentTabularColumnDepthWave(_)),
                BranchMergeTombstoneSubmissionMode::MultiParentTabularColumnDepthWave,
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
            Self::TabularDepthWave(_) => BranchMergeTombstoneSubmissionMode::TabularDepthWave,
            Self::TabularColumnDepthWave(_) => {
                BranchMergeTombstoneSubmissionMode::TabularColumnDepthWave
            }
            Self::MultiParentTabularColumnDepthWave(_) => {
                BranchMergeTombstoneSubmissionMode::MultiParentTabularColumnDepthWave
            }
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
            Self::TabularDepthWave(wave) => wave.tables.iter().any(|(table_id, table)| {
                table
                    .fragments
                    .values()
                    .flatten()
                    .any(|candidate_key| contains(table_id, candidate_key))
            }),
            Self::TabularColumnDepthWave(_) => false,
            Self::MultiParentTabularColumnDepthWave(_) => false,
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

/// Fingerprints a canonical tombstone delta without retaining its values in
/// retry history. Ordering is normalized because release sorts by table/key.
fn tombstone_delta_retry_identity(tombstones: &[(ObjectId, CanonicalValue)]) -> [u8; 32] {
    let mut encoded = tombstones
        .iter()
        .map(|(table, key)| {
            (
                table.bytes(),
                key.encode()
                    .expect("validated canonical primary keys remain encodable"),
            )
        })
        .collect::<Vec<_>>();
    encoded.sort_unstable();

    let mut hash = Sha256::new();
    hash.update(b"orna-storage-tombstone-delta-retry-v1");
    update_retry_identity_count(&mut hash, encoded.len());
    for (table, key) in encoded {
        hash.update(table);
        update_retry_identity_count(&mut hash, key.len());
        hash.update(key);
    }
    hash.finalize().into()
}

fn tabular_depth_wave_tombstones(
    wave: &BranchMergeTabularDepthWave,
) -> Result<Vec<(ObjectId, CanonicalValue)>, BranchMergeTombstoneHistoryError> {
    if wave.tables.is_empty() {
        return Err(BranchMergeTombstoneHistoryError::EmptyTabularDepthWave {
            order: wave.order,
        });
    }

    let mut tombstones = Vec::new();
    for (table, depth) in &wave.tables {
        if depth.fragment_count == 0 {
            return Err(BranchMergeTombstoneHistoryError::InvalidFragment {
                fragment: 0,
                fragment_count: 0,
            });
        }
        if depth.fragments.len() != depth.fragment_count
            || depth
                .fragments
                .keys()
                .enumerate()
                .any(|(expected, actual)| *actual != expected)
        {
            return Err(
                BranchMergeTombstoneHistoryError::IncompleteTabularDepthFragments {
                    order: wave.order,
                    table: *table,
                },
            );
        }
        for keys in depth.fragments.values() {
            tombstones.extend(keys.iter().cloned().map(|key| (*table, key)));
        }
    }
    Ok(tombstones)
}

fn tabular_depth_wave_retry_identity(
    wave: &BranchMergeTabularDepthWave,
) -> TabularFragmentRetryIdentity {
    wave.tables
        .iter()
        .map(|(table, depth)| {
            let fragments = depth
                .fragments
                .iter()
                .map(|(fragment, keys)| {
                    let tombstones = keys
                        .iter()
                        .cloned()
                        .map(|key| (*table, key))
                        .collect::<Vec<_>>();
                    (*fragment, tombstone_delta_retry_identity(&tombstones))
                })
                .collect();
            (*table, (depth.fragment_count, fragments))
        })
        .collect()
}

fn tabular_depth_wave_ladder_events(
    wave: &BranchMergeTabularDepthWave,
) -> Vec<BranchMergeTableDepthLadderEvent> {
    wave.tables
        .iter()
        .map(|(table, depth)| BranchMergeTableDepthLadderEvent {
            order: wave.order,
            table: *table,
            depth_labels: depth.fragments.keys().copied().collect(),
        })
        .collect()
}

fn tabular_column_depth_wave_events(
    wave: &BranchMergeTabularColumnDepthWave,
) -> Result<Vec<BranchMergeColumnDepthEvent>, BranchMergeTombstoneHistoryError> {
    if wave.columns.is_empty() {
        return Err(BranchMergeTombstoneHistoryError::EmptyTabularColumnDepthWave {
            order: wave.order,
        });
    }

    let mut events = Vec::new();
    for ((table, column), depth) in &wave.columns {
        if depth.fragment_count == 0
            || depth.fragments.len() != depth.fragment_count
            || depth
                .fragments
                .keys()
                .enumerate()
                .any(|(expected, actual)| *actual != expected)
        {
            return Err(
                BranchMergeTombstoneHistoryError::IncompleteTabularColumnDepthFragments {
                    order: wave.order,
                    table: *table,
                    column: *column,
                },
            );
        }

        for (fragment, cells) in &depth.fragments {
            for (index, (key, value)) in cells.iter().enumerate() {
                if cells[..index]
                    .iter()
                    .any(|(prior, _)| same_primary_key(prior, key))
                {
                    return Err(BranchMergeTombstoneHistoryError::DuplicateColumnDepthRow {
                        order: wave.order,
                        table: *table,
                        column: *column,
                        fragment: *fragment,
                    });
                }
                events.push(BranchMergeColumnDepthEvent {
                    order: wave.order,
                    table: *table,
                    column: *column,
                    fragment: *fragment,
                    key: key.clone(),
                    value: value.clone(),
                });
            }
        }
    }
    events.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| left.fragment.cmp(&right.fragment))
            .then_with(|| {
                compare_primary_keys_with_encoding_tiebreak(&left.key, &right.key)
            })
    });
    Ok(events)
}

fn tabular_column_depth_wave_retry_identity(
    wave: &BranchMergeTabularColumnDepthWave,
) -> TabularColumnFragmentRetryIdentity {
    wave.columns
        .iter()
        .map(|(identity, depth)| {
            let fragments = depth
                .fragments
                .iter()
                .map(|(fragment, cells)| (*fragment, column_depth_fragment_retry_identity(cells)))
                .collect();
            (*identity, (depth.fragment_count, fragments))
        })
        .collect()
}

fn tabular_column_depth_ladder_events(
    wave: &BranchMergeTabularColumnDepthWave,
) -> Vec<BranchMergeColumnDepthLadderEvent> {
    wave.columns
        .iter()
        .map(|((table, column), depth)| BranchMergeColumnDepthLadderEvent {
            order: wave.order,
            table: *table,
            column: *column,
            depth_labels: depth.fragments.keys().copied().collect(),
        })
        .collect()
}

fn column_depth_fragment_retry_identity(
    cells: &[(CanonicalValue, CanonicalValue)],
) -> [u8; 32] {
    let mut encoded = cells
        .iter()
        .map(|(key, value)| {
            (
                key.encode()
                    .expect("validated canonical primary keys remain encodable"),
                value
                    .encode()
                    .expect("canonical column values remain encodable"),
            )
        })
        .collect::<Vec<_>>();
    encoded.sort_unstable();

    let mut hash = Sha256::new();
    hash.update(b"orna-storage-column-depth-fragment-retry-v1");
    update_retry_identity_count(&mut hash, encoded.len());
    for (key, value) in encoded {
        update_retry_identity_count(&mut hash, key.len());
        hash.update(key);
        update_retry_identity_count(&mut hash, value.len());
        hash.update(value);
    }
    hash.finalize().into()
}

fn parent_column_depth_wave_events(
    wave: &BranchMergeMultiParentTabularColumnDepthWave,
) -> Result<Vec<BranchMergeParentColumnDepthEvent>, BranchMergeTombstoneHistoryError> {
    if wave.parents.len() < 2 {
        return Err(
            BranchMergeTombstoneHistoryError::InsufficientTabularDepthParents {
                order: wave.order,
                actual: wave.parents.len(),
            },
        );
    }

    let mut events = Vec::new();
    for (parent, columns) in &wave.parents {
        if columns.is_empty() {
            return Err(BranchMergeTombstoneHistoryError::EmptyParentColumnDepthWave {
                order: wave.order,
                parent: *parent,
            });
        }
        for ((table, column), depth) in columns {
            if depth.fragment_count == 0
                || depth.fragments.len() != depth.fragment_count
                || depth
                    .fragments
                    .keys()
                    .enumerate()
                    .any(|(expected, actual)| *actual != expected)
            {
                return Err(
                    BranchMergeTombstoneHistoryError::IncompleteParentColumnDepthFragments {
                        order: wave.order,
                        parent: *parent,
                        table: *table,
                        column: *column,
                    },
                );
            }

            for (fragment, cells) in &depth.fragments {
                for (index, (key, value)) in cells.iter().enumerate() {
                    if cells[..index]
                        .iter()
                        .any(|(prior, _)| same_primary_key(prior, key))
                    {
                        return Err(
                            BranchMergeTombstoneHistoryError::DuplicateParentColumnDepthRow {
                                order: wave.order,
                                parent: *parent,
                                table: *table,
                                column: *column,
                                fragment: *fragment,
                            },
                        );
                    }
                    events.push(BranchMergeParentColumnDepthEvent {
                        order: wave.order,
                        parent: *parent,
                        table: *table,
                        column: *column,
                        fragment: *fragment,
                        key: key.clone(),
                        value: value.clone(),
                    });
                }
            }
        }
    }
    events.sort_by(|left, right| {
        left.parent
            .cmp(&right.parent)
            .then_with(|| left.table.cmp(&right.table))
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| left.fragment.cmp(&right.fragment))
            .then_with(|| compare_primary_keys_with_encoding_tiebreak(&left.key, &right.key))
    });
    Ok(events)
}

fn parent_column_depth_wave_ladder_events(
    wave: &BranchMergeMultiParentTabularColumnDepthWave,
) -> Vec<BranchMergeParentColumnDepthLadderEvent> {
    wave.parents
        .iter()
        .flat_map(|(parent, columns)| {
            columns
                .iter()
                .map(move |((table, column), depth)| {
                    BranchMergeParentColumnDepthLadderEvent {
                        order: wave.order,
                        parent: *parent,
                        table: *table,
                        column: *column,
                        depth_labels: depth.fragments.keys().copied().collect(),
                    }
                })
        })
        .collect()
}

fn parent_column_depth_wave_retry_identity(
    wave: &BranchMergeMultiParentTabularColumnDepthWave,
) -> MultiParentColumnFragmentRetryIdentity {
    wave.parents
        .iter()
        .map(|(parent, columns)| {
            let identities = columns
                .iter()
                .map(|(identity, depth)| {
                    let fragments = depth
                        .fragments
                        .iter()
                        .map(|(fragment, cells)| {
                            (*fragment, column_depth_fragment_retry_identity(cells))
                        })
                        .collect();
                    (*identity, (depth.fragment_count, fragments))
                })
                .collect();
            (*parent, identities)
        })
        .collect()
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
