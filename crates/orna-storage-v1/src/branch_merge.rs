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
    /// Per-invocation cap; concurrent merge plans do not share conflict counts.
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
    /// from the merged live rows. They describe this snapshot's versioned
    /// deletions; they do not authorize removing older Git snapshots. Both
    /// rows and tombstones are ordered by canonical primary key. A key that
    /// spells a path prefix is still only that one key; deleting it does not
    /// implicitly tombstone deeper keys.
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

/// Computes a complete semantic merge plan or returns bounded conflict facts.
/// It never writes files, advances refs, or mutates input snapshots.
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
