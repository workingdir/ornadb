//! Structured planner and dependency-graph values for the portable `sys` API.
//!
//! The runtime/catalogue adapter owns object identity and query resolution. This
//! module only accepts already typed, snapshot-pinned inputs and turns them
//! into deterministic, bounded `sys.Plan` and `sys.Dependency` projections.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{Serialize, ser::SerializeStruct};
use sha2::{Digest, Sha256};

use super::{Diagnostic, ObjectRef, PlanRef};

pub const MAX_DEPENDENCY_OBJECTS: usize = 65_536;
pub const MAX_DEPENDENCY_EDGES: usize = 1_000_000;
pub const MAX_PLAN_NODES: usize = 16_384;
pub const MAX_PLAN_EXPRESSIONS: usize = 8_192;
pub const MAX_REFERENCE_BYTES: usize = 4_096;
const MAX_DISJUNCT_STORM_DEPTH: usize = 64;

macro_rules! descriptive_reference {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
        pub struct $name(String);

        impl $name {
            pub fn descriptive(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

descriptive_reference!(SnapshotRef);
descriptive_reference!(FunctionRef);
descriptive_reference!(FileRef);
descriptive_reference!(DefinitionRef);
descriptive_reference!(ExpressionRef);
descriptive_reference!(DependencyRef);
descriptive_reference!(PlanNodeRef);

/// Dependency kinds use the exact portable `sys.DependencyKind` spellings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Import,
    TypeReference,
    Call,
    TableRead,
    TableWrite,
    Assertion,
    Implementation,
    PageEntry,
    RendererRequirement,
    ExtensionImport,
    StorageProjection,
}

impl DependencyKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::TypeReference => "type_reference",
            Self::Call => "call",
            Self::TableRead => "table_read",
            Self::TableWrite => "table_write",
            Self::Assertion => "assertion",
            Self::Implementation => "implementation",
            Self::PageEntry => "page_entry",
            Self::RendererRequirement => "renderer_requirement",
            Self::ExtensionImport => "extension_import",
            Self::StorageProjection => "storage_projection",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyConfidence {
    Exact,
    Conservative,
    Possible,
}

impl DependencyConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Conservative => "conservative",
            Self::Possible => "possible",
        }
    }
}

/// Canonical UTF-8 byte range and one-based source coordinates retained with
/// an edge. The shape matches `sys.SourceSpan`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct SourceSpan {
    pub file: FileRef,
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// An edge supplied by the snapshot's catalogue adapter before the graph
/// assigns its stable `sys.DependencyRef`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyInput {
    from: ObjectRef,
    to: ObjectRef,
    kind: DependencyKind,
    definition: Option<DefinitionRef>,
    span: Option<SourceSpan>,
    confidence: DependencyConfidence,
    conditional: bool,
}

impl DependencyInput {
    pub fn new(
        from: ObjectRef,
        to: ObjectRef,
        kind: DependencyKind,
        definition: Option<DefinitionRef>,
        span: Option<SourceSpan>,
        confidence: DependencyConfidence,
        conditional: bool,
    ) -> Self {
        Self {
            from,
            to,
            kind,
            definition,
            span,
            confidence,
            conditional,
        }
    }
}

/// One immutable `sys.Dependency` row with a graph-assigned, snapshot-stable
/// reference. Endpoints remain typed object references throughout traversal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Dependency {
    reference: DependencyRef,
    from: ObjectRef,
    to: ObjectRef,
    kind: DependencyKind,
    definition: Option<DefinitionRef>,
    span: Option<SourceSpan>,
    confidence: DependencyConfidence,
    conditional: bool,
}

impl Dependency {
    pub fn reference(&self) -> &DependencyRef {
        &self.reference
    }

    pub fn from(&self) -> &ObjectRef {
        &self.from
    }

    pub fn to(&self) -> &ObjectRef {
        &self.to
    }

    pub const fn kind(&self) -> DependencyKind {
        self.kind
    }

    pub fn definition(&self) -> Option<&DefinitionRef> {
        self.definition.as_ref()
    }

    pub fn span(&self) -> Option<&SourceSpan> {
        self.span.as_ref()
    }

    pub const fn confidence(&self) -> DependencyConfidence {
        self.confidence
    }

    pub const fn is_conditional(&self) -> bool {
        self.conditional
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct DependencyKey {
    from: ObjectRef,
    to: ObjectRef,
    kind: DependencyKind,
    span: Option<SourceSpan>,
}

impl From<&Dependency> for DependencyKey {
    fn from(edge: &Dependency) -> Self {
        Self {
            from: edge.from.clone(),
            to: edge.to.clone(),
            kind: edge.kind,
            span: edge.span.clone(),
        }
    }
}

impl From<&DependencyInput> for DependencyKey {
    fn from(edge: &DependencyInput) -> Self {
        Self {
            from: edge.from.clone(),
            to: edge.to.clone(),
            kind: edge.kind,
            span: edge.span.clone(),
        }
    }
}

/// An immutable graph for one pinned snapshot. Construction validates every
/// typed endpoint before `sys.dependencies` or `sys.dependents` can traverse it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyGraph {
    snapshot: SnapshotRef,
    objects: BTreeSet<ObjectRef>,
    edges: Vec<Dependency>,
    outgoing: BTreeMap<ObjectRef, Vec<usize>>,
    incoming: BTreeMap<ObjectRef, Vec<usize>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyGraphError {
    InvalidSnapshot,
    InvalidObject,
    InvalidSpan,
    UnknownEndpoint,
    TooManyObjects,
    TooManyEdges,
    ConflictingDuplicate,
    UnknownObject,
}

impl DependencyGraphError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidSnapshot => "sys.dependencies.invalid_snapshot",
            Self::InvalidObject => "sys.dependencies.invalid_object",
            Self::InvalidSpan => "sys.dependencies.invalid_span",
            Self::UnknownEndpoint => "sys.dependencies.unknown_endpoint",
            Self::TooManyObjects | Self::TooManyEdges => "sys.dependencies.limit",
            Self::ConflictingDuplicate => "sys.dependencies.conflicting_duplicate",
            Self::UnknownObject => "sys.dependencies.object_unavailable",
        }
    }
}

impl fmt::Display for DependencyGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for DependencyGraphError {}

impl DependencyGraph {
    pub fn new(
        snapshot: SnapshotRef,
        objects: impl IntoIterator<Item = ObjectRef>,
        edges: impl IntoIterator<Item = DependencyInput>,
    ) -> Result<Self, DependencyGraphError> {
        if invalid_reference(snapshot.as_str()) {
            return Err(DependencyGraphError::InvalidSnapshot);
        }

        let objects = objects.into_iter().collect::<BTreeSet<_>>();
        if objects.len() > MAX_DEPENDENCY_OBJECTS {
            return Err(DependencyGraphError::TooManyObjects);
        }
        if objects.iter().any(|object| invalid_reference(object.as_str())) {
            return Err(DependencyGraphError::InvalidObject);
        }

        let mut keyed = BTreeMap::<DependencyKey, DependencyInput>::new();
        for edge in edges {
            if keyed.len() >= MAX_DEPENDENCY_EDGES {
                return Err(DependencyGraphError::TooManyEdges);
            }
            if !objects.contains(&edge.from) || !objects.contains(&edge.to) {
                return Err(DependencyGraphError::UnknownEndpoint);
            }
            if edge
                .span
                .as_ref()
                .is_some_and(|span| {
                    invalid_reference(span.file.as_str())
                        || span.start_byte > span.end_byte
                        || span.start_line == 0
                        || span.start_column == 0
                        || span.end_line == 0
                        || span.end_column == 0
                        || (span.start_line, span.start_column) > (span.end_line, span.end_column)
                })
            {
                return Err(DependencyGraphError::InvalidSpan);
            }
            if edge
                .definition
                .as_ref()
                .is_some_and(|definition| invalid_reference(definition.as_str()))
            {
                return Err(DependencyGraphError::InvalidObject);
            }

            let key = DependencyKey::from(&edge);
            if let Some(existing) = keyed.get(&key) {
                if existing.definition != edge.definition
                    || existing.confidence != edge.confidence
                    || existing.conditional != edge.conditional
                {
                    return Err(DependencyGraphError::ConflictingDuplicate);
                }
            } else {
                keyed.insert(key, edge);
            }
        }

        let mut edges = Vec::with_capacity(keyed.len());
        for (key, edge) in keyed {
            edges.push(Dependency {
                reference: DependencyRef::descriptive(dependency_reference(&snapshot, &key)),
                from: edge.from,
                to: edge.to,
                kind: edge.kind,
                definition: edge.definition,
                span: edge.span,
                confidence: edge.confidence,
                conditional: edge.conditional,
            });
        }
        let mut outgoing = BTreeMap::<ObjectRef, Vec<usize>>::new();
        let mut incoming = BTreeMap::<ObjectRef, Vec<usize>>::new();
        for (index, edge) in edges.iter().enumerate() {
            outgoing.entry(edge.from.clone()).or_default().push(index);
            incoming.entry(edge.to.clone()).or_default().push(index);
        }

        Ok(Self {
            snapshot,
            objects,
            edges,
            outgoing,
            incoming,
        })
    }

    pub fn snapshot(&self) -> &SnapshotRef {
        &self.snapshot
    }

    pub fn objects(&self) -> &BTreeSet<ObjectRef> {
        &self.objects
    }

    pub fn dependencies(
        &self,
        object: &ObjectRef,
        transitive: bool,
        kinds: Option<&BTreeSet<DependencyKind>>,
    ) -> Result<Vec<Dependency>, DependencyGraphError> {
        self.traverse(object, transitive, kinds, false)
    }

    pub fn dependents(
        &self,
        object: &ObjectRef,
        transitive: bool,
        kinds: Option<&BTreeSet<DependencyKind>>,
    ) -> Result<Vec<Dependency>, DependencyGraphError> {
        self.traverse(object, transitive, kinds, true)
    }

    fn traverse(
        &self,
        object: &ObjectRef,
        transitive: bool,
        kinds: Option<&BTreeSet<DependencyKind>>,
        reverse: bool,
    ) -> Result<Vec<Dependency>, DependencyGraphError> {
        if !self.objects.contains(object) {
            return Err(DependencyGraphError::UnknownObject);
        }

        let mut result = Vec::new();
        let mut seen_edges = BTreeSet::new();
        let mut seen_objects = BTreeSet::from([object.clone()]);
        let mut frontier = BTreeSet::from([object.clone()]);
        loop {
            let mut next = BTreeSet::new();
            for current in &frontier {
                let adjacent = if reverse {
                    self.incoming.get(current)
                } else {
                    self.outgoing.get(current)
                };
                let Some(adjacent) = adjacent else {
                    continue;
                };
                for index in adjacent {
                    let edge = &self.edges[*index];
                    if kinds.is_some_and(|kinds| !kinds.contains(&edge.kind)) {
                        continue;
                    }
                    let key = DependencyKey::from(edge);
                    if seen_edges.insert(key) {
                        result.push(edge.clone());
                    }
                    if transitive {
                        let target = if reverse { &edge.from } else { &edge.to };
                        if seen_objects.insert(target.clone()) {
                            next.insert(target.clone());
                        }
                    }
                }
            }
            if !transitive || next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(result)
    }
}

fn invalid_reference(value: &str) -> bool {
    value.trim().is_empty()
        || value.len() > MAX_REFERENCE_BYTES
        || value.chars().any(char::is_control)
}

fn dependency_reference(snapshot: &SnapshotRef, key: &DependencyKey) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.dependency.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, key.from.as_str().as_bytes());
    hash_part(&mut hash, key.to.as_str().as_bytes());
    hash_part(&mut hash, key.kind.as_str().as_bytes());
    match &key.span {
        Some(span) => {
            hash.update([1]);
            hash_part(&mut hash, span.file.as_str().as_bytes());
            hash.update(span.start_byte.to_be_bytes());
            hash.update(span.end_byte.to_be_bytes());
            hash.update(span.start_line.to_be_bytes());
            hash.update(span.start_column.to_be_bytes());
            hash.update(span.end_line.to_be_bytes());
            hash.update(span.end_column.to_be_bytes());
        }
        None => hash.update([0]),
    }
    format!("dependency:{}", hex(&hash.finalize()))
}

fn hash_part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// The operator vocabulary is kept in lock-step with `sys.PlanNodeKind`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeKind {
    Scan,
    IndexLookup,
    Filter,
    Project,
    Join,
    Aggregate,
    Sort,
    Limit,
    Materialize,
    Invoke,
    AssertionValidate,
    CheckpointUpdate,
    External,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSortDirection {
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNullOrder {
    First,
    Last,
}

impl PlanSortDirection {
    const fn as_ref_str(self) -> &'static str {
        match self {
            Self::Ascending => "ascending",
            Self::Descending => "descending",
        }
    }
}

impl PlanNullOrder {
    const fn as_ref_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Last => "last",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanOrdering {
    pub expression: ExpressionRef,
    pub direction: PlanSortDirection,
    pub null_order: PlanNullOrder,
}

/// A resolved bound on a window frame. Numeric bounds are row offsets; the
/// planner uses explicit bounds rather than inferring frames after sparse
/// input reordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "rows")]
pub enum PlanWindowFrameBound {
    UnboundedPreceding,
    Preceding(u64),
    CurrentRow,
    Following(u64),
    UnboundedFollowing,
}

impl PlanWindowFrameBound {
    fn ordinal(self) -> i128 {
        match self {
            Self::UnboundedPreceding => i128::MIN,
            Self::Preceding(rows) => -i128::from(rows),
            Self::CurrentRow => 0,
            Self::Following(rows) => i128::from(rows),
            Self::UnboundedFollowing => i128::MAX,
        }
    }

    fn as_detail(self) -> String {
        match self {
            Self::UnboundedPreceding => "unbounded_preceding".to_owned(),
            Self::Preceding(rows) => format!("preceding:{rows}"),
            Self::CurrentRow => "current_row".to_owned(),
            Self::Following(rows) => format!("following:{rows}"),
            Self::UnboundedFollowing => "unbounded_following".to_owned(),
        }
    }
}

/// A resolved, snapshot-pinned logical query input. Expressions are opaque
/// typed references, so plan details never echo literal query values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlanDescription {
    pub snapshot: SnapshotRef,
    pub source: ObjectRef,
    /// Snapshot statistics for `source`. If `mutable_branch` is set, counts
    /// must include the branch's current uncommitted overlay at `generation`.
    pub source_statistics: Option<QuerySourceStatistics>,
    /// Additional inputs joined after `source`. The explain adapter may
    /// reorder these physical join inputs using complete scan-work estimates;
    /// unknown-cost inputs and equal-cost inputs keep declaration order.
    pub joins: Vec<QueryJoinDescription>,
    pub predicate: Option<ExpressionRef>,
    pub projections: Vec<ExpressionRef>,
    pub distinct: bool,
    pub ordering: Vec<PlanOrdering>,
    pub limit: Option<u64>,
    /// Ordered table-write stages applied after the query operators.
    pub mutations: Vec<QueryMutationDescription>,
    /// Optional destination for an explicit result materialization. Explain
    /// estimates the write work but never performs this operation.
    pub materialize_into: Option<ObjectRef>,
}

/// One nested disjunct storm stage whose branches each have their own limit
/// cascade and conjunct chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisjunctStormDescription {
    /// Resolved predicate reference for this expansion stage.
    pub predicate: ExpressionRef,
    /// Number of independent OR branches expanded at this stage.
    pub disjunct_count: u64,
    /// Ordered limits applied independently inside every branch.
    pub nested_branch_limits: Vec<u64>,
    /// Ordered AND terms evaluated inside each branch after its limits.
    pub conjunct_count_per_disjunct: u64,
}

/// One branch in a storm whose branch-local limit chain differs from its
/// siblings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisjunctStormBranchDescription {
    /// Ordered limits applied to this branch before its conjuncts.
    pub nested_limits: Vec<u64>,
    /// Ordered AND terms evaluated in this branch after its limits.
    pub conjunct_count: u64,
    /// Nested cascades rebound after one-based positions in `nested_limits`.
    /// List entries must be ordered by position; entries at one position run
    /// in declaration order. Each cascade consumes current branch rows, and
    /// its output feeds the next limit in the chain. A rebind at a later
    /// position starts from the branch output after all earlier limits and
    /// rebinds; it never restarts from the enclosing storm input.
    pub limit_rebinds: Vec<DisjunctStormLimitRebindDescription>,
    /// Nested disjunct storms evaluated against this branch's filtered output.
    pub nested_storms: Vec<DisjunctStormCascadeDescription>,
}

/// A group of cascades rebound at a specific point in a branch-local limit chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisjunctStormLimitRebindDescription {
    /// One-based position in the enclosing branch's `nested_limits` chain.
    pub after_limit: usize,
    /// Cascades to run in declaration order after this limit. The first uses
    /// the branch rows and bytes after the limit; each later cascade uses the
    /// previous cascade's bounded output, which ultimately feeds the next
    /// limit in the enclosing branch.
    pub storms: Vec<DisjunctStormCascadeDescription>,
}

/// A disjunct storm stage with independently described branch pipelines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisjunctStormCascadeDescription {
    /// Resolved predicate reference for this expansion stage.
    pub predicate: ExpressionRef,
    /// Branch pipelines are combined in declaration order.
    pub branches: Vec<DisjunctStormBranchDescription>,
}

/// Statistics supplied by the snapshot/catalogue adapter, never measured by
/// the explain path itself. Missing values remain missing in the plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuerySourceStatistics {
    pub estimated_rows: Option<u64>,
    pub estimated_bytes: Option<u64>,
    /// The generation identifies a mutable branch view; its counts above are
    /// overlay-inclusive, so repeated edits cannot reuse a stale plan identity.
    pub mutable_branch: Option<MutableBranchSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutableBranchSnapshot {
    pub name: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryJoinDescription {
    pub source: ObjectRef,
    pub statistics: Option<QuerySourceStatistics>,
    pub predicate: Option<ExpressionRef>,
}

/// Resolver-supplied identity for the logical pair folded by a join. It is
/// associated by exact right-source and predicate identity, so physical cost
/// reordering cannot shift a pair label onto a neighboring join. The explain
/// adapter also derives an anchor-fold identity for the pair, so each join
/// remains scoped to its accumulated left-side chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryJoinPairIdentityDescription {
    pub identity: ObjectRef,
    pub left_source: ObjectRef,
    pub right_source: ObjectRef,
    pub predicate: Option<ExpressionRef>,
}

/// One checkpoint observed while compacting paired log segments for a
/// mutable branch-local join input. A missing side records a sparse slot;
/// when a segment is present, its compaction identity must also be present.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedCheckpointSegmentCompactionStepDescription {
    pub checkpoint_identity: ObjectRef,
    pub left_segment_identity: Option<ObjectRef>,
    pub right_segment_identity: Option<ObjectRef>,
    pub left_compaction_identity: Option<ObjectRef>,
    pub right_compaction_identity: Option<ObjectRef>,
}

/// Resolver-supplied paired checkpoint/segment lineage for one exact join
/// pair and mutable branch generation. Steps remain in compaction order.
/// ORNA defines checkpoint and segment identity preservation in storage, but
/// does not define how query-plan cost restoration should annotate that
/// lineage. The explain adapter treats these references as opaque and folds
/// them into plan metadata without changing cardinality or cost estimates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedCheckpointSegmentCompactionChainDescription {
    pub join_pair_identity: ObjectRef,
    pub branch: MutableBranchSnapshot,
    pub steps: Vec<QueryPairedCheckpointSegmentCompactionStepDescription>,
}

/// One paired segment incarnation observed within a checkpoint rotation
/// stream. The source stream ordinal is retained even when fold and order
/// coordinates repeat in another stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedSegmentRotationDescription {
    pub checkpoint_identity: ObjectRef,
    pub source_stream_ordinal: u64,
    pub fold_ordinal: u64,
    pub order: u64,
    pub left_segment_identity: ObjectRef,
    pub right_segment_identity: ObjectRef,
}

/// Ordered segment rotation lineage for one exact join pair and mutable
/// branch generation. Each rotation retains its complete source stream/fold/
/// order tuple and paired segment incarnations. ORNA specifies this provenance
/// at the storage layer but leaves query-cost fold identity open; the explain
/// adapter preserves caller order and hashes each full occurrence tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedSegmentRotationChainDescription {
    pub join_pair_identity: ObjectRef,
    pub branch: MutableBranchSnapshot,
    pub rotations: Vec<QueryPairedSegmentRotationDescription>,
}

/// One paired stream-compaction occurrence with every source coordinate.
/// Optional checkpoint sides remain optional; the paired segment identities
/// are retained even when both checkpoint sides are absent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedStreamCompactionOccurrenceDescription {
    pub checkpoint_identity: ObjectRef,
    pub restore_ordinal: u64,
    pub handoff_ordinal: u64,
    pub source_stream_ordinal: u64,
    pub compaction_ordinal: u64,
    pub fold_ordinal: u64,
    pub order: u64,
    pub left_checkpoint_identity: Option<ObjectRef>,
    pub right_checkpoint_identity: Option<ObjectRef>,
    pub redo_fold_identity: ObjectRef,
    pub left_segment_identity: ObjectRef,
    pub right_segment_identity: ObjectRef,
}

/// Ordered sparse restore/stream compaction occurrences attached to one exact
/// join pair and mutable branch generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedStreamCompactionChainDescription {
    pub join_pair_identity: ObjectRef,
    pub branch: MutableBranchSnapshot,
    pub occurrences: Vec<QueryPairedStreamCompactionOccurrenceDescription>,
}

/// Opaque checkpoint state copied from one side of a spill restore. An
/// absent position differs from a present empty position and remains so.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryCheckpointGenerationDescription {
    pub generation: u64,
    pub position: Option<Vec<u8>>,
}

/// One restored checkpoint spill occurrence with its complete source path.
/// A caller supplies expanded order slots, retaining source order and gaps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedCheckpointSpillRestoreOccurrenceDescription {
    pub checkpoint_identity: ObjectRef,
    pub restore_ordinal: u64,
    pub spill_ordinal: u64,
    pub spill_identity: ObjectRef,
    pub source_stream_ordinal: u64,
    pub source_stream_identity: ObjectRef,
    pub compaction_ordinal: u64,
    pub handoff_ordinal: u64,
    pub fold_ordinal: u64,
    pub order: u64,
    pub left_checkpoint_generation: Option<QueryCheckpointGenerationDescription>,
    pub right_checkpoint_generation: Option<QueryCheckpointGenerationDescription>,
    pub redo_fold_identity: ObjectRef,
}

/// Ordered checkpoint-spill restore occurrences for one exact join pair and
/// mutable branch generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedCheckpointSpillRestoreChainDescription {
    pub join_pair_identity: ObjectRef,
    pub branch: MutableBranchSnapshot,
    pub occurrences: Vec<QueryPairedCheckpointSpillRestoreOccurrenceDescription>,
}

/// One paired stream rotation with its complete checkpoint/source coordinate.
/// Missing sides remain missing, while occurrence order and identities remain
/// distinct even when a stream repeats the same checkpoint or rotation label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedStreamRotationOccurrenceDescription {
    pub checkpoint_identity: ObjectRef,
    pub rotation_identity: ObjectRef,
    pub source_stream_ordinal: u64,
    pub source_stream_identity: ObjectRef,
    pub rotation_ordinal: u64,
    pub fold_ordinal: u64,
    pub order: u64,
    pub left_stream_identity: Option<ObjectRef>,
    pub right_stream_identity: Option<ObjectRef>,
}

/// Ordered sparse stream-rotation occurrences for one exact join pair and
/// mutable branch generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedStreamRotationChainDescription {
    pub join_pair_identity: ObjectRef,
    pub branch: MutableBranchSnapshot,
    pub occurrences: Vec<QueryPairedStreamRotationOccurrenceDescription>,
}

/// One paired WAL rotation occurrence retaining its full restore and merge
/// coordinate, checkpoint-side values, directional redo folds, paired log
/// IDs, and active segment incarnations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedWalRotationOccurrenceDescription {
    pub checkpoint_identity: ObjectRef,
    pub restore_ordinal: u64,
    pub handoff_ordinal: u64,
    pub stream_ordinal: u64,
    pub compaction_ordinal: u64,
    pub merge_ordinal: u64,
    pub fold_ordinal: u64,
    pub order: u64,
    pub left_checkpoint_generation: Option<QueryCheckpointGenerationDescription>,
    pub right_checkpoint_generation: Option<QueryCheckpointGenerationDescription>,
    pub left_redo_fold_identity: ObjectRef,
    pub right_redo_fold_identity: ObjectRef,
    pub left_write_ahead_identity: ObjectRef,
    pub right_write_ahead_identity: ObjectRef,
    pub left_segment_identity: ObjectRef,
    pub right_segment_identity: ObjectRef,
}

/// Ordered paired WAL rotation occurrences attached to one exact join pair
/// and mutable branch generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPairedWalRotationChainDescription {
    pub join_pair_identity: ObjectRef,
    pub branch: MutableBranchSnapshot,
    pub occurrences: Vec<QueryPairedWalRotationOccurrenceDescription>,
}

/// A resolver-approved input limit attached to one exact logical join pair.
/// Multiple entries for the same pair form an ordered limit chain in slice
/// order. Explain applies the chain after source access and before the join;
/// it preserves the supplied limit and does not infer pushdown eligibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryLimitPushdownDescription {
    pub identity: ObjectRef,
    pub join_pair_identity: ObjectRef,
    pub source: ObjectRef,
    pub limit: u64,
}

/// A correlated subquery that the resolver has already approved for
/// decorrelation into a predicate join. The planner keeps the subquery and
/// correlation identities on both the join and its input while cost ordering
/// moves known inputs across sparse unknown-cost entries. Its decorrelated
/// anchor-fold identity additionally binds that input to the accumulated
/// anchor side of the join, so separate sparse lateral anchors cannot share a
/// fold identity. ORNA does not define decorrelation eligibility, a
/// subquery-specific cost model, or this explain-only identity encoding; the
/// adapter uses supplied pinned statistics and the ordinary predicate-join
/// estimate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryDecorrelatedSubqueryDescription {
    pub identity: ObjectRef,
    pub source: ObjectRef,
    pub correlation_predicate: ExpressionRef,
    pub statistics: Option<QuerySourceStatistics>,
}

/// A partial index that may accelerate a join predicate when both the input
/// table and the predicate reference match exactly. Sparse predicate cascades
/// therefore cannot shift an index candidate onto a neighboring join.
/// The lookup reuses the matching join input's pinned statistics because this
/// descriptor carries identity, not an independent index cardinality.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPartialIndexDescription {
    pub table: ObjectRef,
    pub index: ObjectRef,
    pub partial_predicate: ExpressionRef,
}

/// A resolved window aggregate eligible at its exact source boundary.
/// Aggregate and frame identities remain attached across sparse input
/// reordering. The planner also binds each right-side frame chain to its
/// accumulated sparse anchor fold. ORNA-PLAN does not prescribe a
/// window-pushdown cost or this explain identity encoding, so this adapter
/// charges one work unit per known input row and preserves cardinality.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryWindowAggregatePushdownDescription {
    pub identity: ObjectRef,
    pub source: ObjectRef,
    pub aggregate: ExpressionRef,
    pub frame_identity: ExpressionRef,
    pub frame_start: PlanWindowFrameBound,
    pub frame_end: PlanWindowFrameBound,
}

/// Resolver-supplied working-set and memory budget for one exact window
/// aggregate on one exact join pair. The explain adapter computes bytes above
/// budget and models each 4 KiB spill block as one write plus one read work
/// unit. A missing working-set estimate remains unknown; explain never
/// executes or infers a spill. ORNA does not prescribe a spill threshold or
/// cost model, so these are planner-local estimates carried on the existing
/// window aggregate node, not runtime spill instructions. Explain also folds
/// each aggregate identity's spill costs independently across the sparse
/// paired chain; pairs without spill descriptors carry those local folds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryWindowSpillDescription {
    pub identity: ObjectRef,
    pub join_pair_identity: ObjectRef,
    pub source: ObjectRef,
    pub window_aggregate_identity: ObjectRef,
    pub estimated_working_set_bytes: Option<u64>,
    pub memory_budget_bytes: u64,
}

/// A known table mutation at the tail of a query plan. Counts are estimates
/// from the pinned adapter state; inserts/deletes adjust the table estimate,
/// while updates/rekeys preserve its cardinality.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryMutationDescription {
    pub table: ObjectRef,
    pub kind: QueryMutationKind,
    pub estimated_affected_rows: Option<u64>,
    pub estimated_write_bytes: Option<u64>,
    /// Optional overlay-inclusive count before this mutation. If omitted, the
    /// planner uses the source/join estimate or a preceding mutation's result.
    pub estimated_table_rows_before: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryMutationKind {
    Insert,
    Update,
    Delete,
    Rekey,
}

impl QueryMutationKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Rekey => "rekey",
        }
    }
}

/// The input to `sys.explain(FunctionRef)`. The catalogue adapter supplies
/// direct, statically known dependencies for the selected function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionPlanDescription {
    pub snapshot: SnapshotRef,
    pub function: FunctionRef,
    pub dependencies: Vec<DependencyInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PlanDetail {
    Text(String),
    Integer(u64),
    Boolean(bool),
    Expressions(Vec<ExpressionRef>),
    Ordering(Vec<PlanOrdering>),
    /// Routes are ordered by nesting depth, typed input path, then typed
    /// output path.
    ByteCapHandoffRoutes(Vec<PlanByteCapHandoffRoute>),
}

/// One typed source-to-destination byte-cap handoff reported by a planner
/// storm rebind. Unknown byte estimates remain `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanByteCapHandoffRoute {
    /// One-based nesting depth of the storm scope containing the rebind.
    pub depth: usize,
    /// Typed path to the branch scope supplying the byte estimate.
    pub input_path: Vec<PlanByteCapScopeSegment>,
    /// Typed path to the nested cascade receiving the byte estimate.
    pub output_path: Vec<PlanByteCapScopeSegment>,
    /// Human-readable label derived from the complete typed input ancestry.
    pub input_scope: String,
    /// Human-readable label derived from the complete typed output ancestry.
    pub output_scope: String,
    /// Known input byte estimate; `None` means the estimate is unknown.
    pub input_bytes: Option<u64>,
    /// Known capped output byte estimate; `None` means the estimate is unknown.
    pub output_bytes: Option<u64>,
}

impl PlanByteCapHandoffRoute {
    /// Creates a handoff route whose display labels derive from its typed paths.
    pub fn from_typed_paths(
        depth: usize,
        input_path: Vec<PlanByteCapScopeSegment>,
        output_path: Vec<PlanByteCapScopeSegment>,
        input_bytes: Option<u64>,
        output_bytes: Option<u64>,
    ) -> Self {
        Self {
            depth,
            input_scope: byte_cap_scope_path_label(&input_path),
            output_scope: byte_cap_scope_path_label(&output_path),
            input_path,
            output_path,
            input_bytes,
            output_bytes,
        }
    }

    /// Returns the canonical input label derived from `input_path`.
    ///
    /// The typed path is authoritative even if the public `input_scope` field
    /// was changed after this route was constructed.
    pub fn input_scope_label(&self) -> String {
        byte_cap_scope_path_label(&self.input_path)
    }

    /// Returns the canonical output label derived from `output_path`.
    ///
    /// The typed path is authoritative even if the public `output_scope`
    /// field was changed after this route was constructed.
    pub fn output_scope_label(&self) -> String {
        byte_cap_scope_path_label(&self.output_path)
    }

    /// Returns both canonical labels from this route's typed paths.
    ///
    /// Keeping the input and output labels together is useful when presenting
    /// a route across post-storm ancestry, where the source path may include
    /// outputs from preceding stages.
    pub fn scope_labels(&self) -> (String, String) {
        (
            byte_cap_scope_path_label(&self.input_path),
            byte_cap_scope_path_label(&self.output_path),
        )
    }

    /// Returns the canonical labels as an explicitly named input/output pair.
    fn named_scope_labels(&self) -> PlanByteCapScopeLabels {
        let (input, output) = self.scope_labels();
        PlanByteCapScopeLabels { input, output }
    }

    /// Returns the canonical input-to-output scope label for this route.
    pub fn paired_scope_label(&self) -> String {
        let (input_scope, output_scope) = self.scope_labels();
        format!("{input_scope}=>{output_scope}")
    }
}

/// Serialized canonical byte-cap handoff labels with explicit input/output names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PlanByteCapScopeLabels {
    /// Label derived from the route's typed input ancestry.
    pub input: String,
    /// Label derived from the route's typed output ancestry.
    pub output: String,
}

impl Serialize for PlanByteCapHandoffRoute {
    // The typed paths are authoritative; derive serialized labels instead of
    // trusting duplicated strings that may be stale on a manually built route.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut route = serializer.serialize_struct("PlanByteCapHandoffRoute", 9)?;
        route.serialize_field("depth", &self.depth)?;
        route.serialize_field("input_path", &self.input_path)?;
        route.serialize_field("output_path", &self.output_path)?;
        let (input_scope, output_scope) = self.scope_labels();
        route.serialize_field("input_scope", &input_scope)?;
        route.serialize_field("output_scope", &output_scope)?;
        route.serialize_field("paired_scope_label", &self.paired_scope_label())?;
        route.serialize_field("scope_labels", &self.named_scope_labels())?;
        route.serialize_field("input_bytes", &self.input_bytes)?;
        route.serialize_field("output_bytes", &self.output_bytes)?;
        route.end()
    }
}

/// One typed step in a nested storm byte-cap handoff route. A route path
/// preserves every preceding handoff output in its ancestry, including when a
/// later limit or cascade consumes that bounded result.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanByteCapScopeSegment {
    /// A one-based top-level storm stage.
    StormStage { index: usize },
    /// The bounded output of a one-based top-level storm stage.
    StormStageOutput { index: usize },
    /// A one-based branch within a storm.
    Branch { index: usize },
    /// A one-based nested limit position within a branch.
    Limit { position: usize },
    /// The bounded output of a one-based branch after its limits, rebinds,
    /// and conjuncts, before that branch's nested storms.
    BranchOutput { index: usize },
    /// A one-based rebind declaration position within a branch.
    Rebind { position: usize },
    /// A one-based cascade position within a rebind.
    Cascade { index: usize },
    /// The bounded output of a one-based cascade at a rebind position.
    RebindCascadeOutput { position: usize, index: usize },
    /// A one-based nested storm position within a branch.
    NestedStorm { index: usize },
    /// The bounded output of a one-based nested storm within a branch.
    NestedStormOutput { index: usize },
}

impl PlanByteCapScopeSegment {
    /// Returns the canonical display component for this typed scope segment.
    ///
    /// ORNA-PLAN specifies typed planner ancestry but does not prescribe its
    /// text spelling. The local stable mapping is `StormStage` to `stormN`,
    /// `StormStageOutput` to `storm_stage_outputN`, `Branch` to `branchN`,
    /// `Limit` to `limitN`, `BranchOutput` to `branch_outputN`, `Rebind` to
    /// `rebindN`, `Cascade` to `cascadeN`, `RebindCascadeOutput` to
    /// `rebind_outputP_C`, `NestedStorm` to `nestedN`, and
    /// `NestedStormOutput` to `nested_outputN`. `N` is a one-based index or
    /// position; `P_C` is the rebind position and cascade index. The typed
    /// segment remains authoritative when a display stem is abbreviated.
    pub fn scope_component_label(&self) -> String {
        match self {
            Self::StormStage { index } => format!("storm{index}"),
            Self::StormStageOutput { index } => format!("storm_stage_output{index}"),
            Self::Branch { index } => format!("branch{index}"),
            Self::Limit { position } => format!("limit{position}"),
            Self::BranchOutput { index } => format!("branch_output{index}"),
            Self::Rebind { position } => format!("rebind{position}"),
            Self::Cascade { index } => format!("cascade{index}"),
            Self::RebindCascadeOutput { position, index } => {
                format!("rebind_output{position}_{index}")
            }
            Self::NestedStorm { index } => format!("nested{index}"),
            Self::NestedStormOutput { index } => format!("nested_output{index}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlanNode {
    reference: PlanNodeRef,
    plan: PlanRef,
    parent: Option<PlanNodeRef>,
    position: u32,
    kind: PlanNodeKind,
    inputs: Vec<PlanNodeRef>,
    object: Option<ObjectRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predicate: Option<ExpressionRef>,
    details: BTreeMap<String, PlanDetail>,
}

impl PlanNode {
    pub fn reference(&self) -> &PlanNodeRef {
        &self.reference
    }

    pub fn plan(&self) -> &PlanRef {
        &self.plan
    }

    pub fn parent(&self) -> Option<&PlanNodeRef> {
        self.parent.as_ref()
    }

    pub const fn position(&self) -> u32 {
        self.position
    }

    pub const fn kind(&self) -> PlanNodeKind {
        self.kind
    }

    pub fn inputs(&self) -> &[PlanNodeRef] {
        &self.inputs
    }

    pub fn object(&self) -> Option<&ObjectRef> {
        self.object.as_ref()
    }

    pub const fn actual_rows(&self) -> Option<u64> {
        self.actual_rows
    }

    pub const fn estimated_rows(&self) -> Option<u64> {
        self.estimated_rows
    }

    pub const fn actual_bytes(&self) -> Option<u64> {
        self.actual_bytes
    }

    pub const fn estimated_bytes(&self) -> Option<u64> {
        self.estimated_bytes
    }

    /// Returns the structured details carried by this operator in `sys.PlanNode`.
    pub fn details(&self) -> &BTreeMap<String, PlanDetail> {
        &self.details
    }

    /// Returns this operator's estimated work when all inputs for that local
    /// estimate are known. `Some(0)` is a known zero estimate; `None` means
    /// required inputs were unavailable or exact arithmetic overflowed. The
    /// latter case is identified by `details["estimated_work_overflow"]`.
    pub fn estimated_work(&self) -> Option<u64> {
        match self.details.get("estimated_work") {
            Some(PlanDetail::Integer(work)) => Some(*work),
            _ => None,
        }
    }
}

/// The canonical single row for `sys.Plan` plus its related plan-node rows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExplainedPlan {
    plan: Plan,
    nodes: Vec<PlanNode>,
}

/// One structured explain-plan row, matching the portable `sys.Plan` shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Plan {
    reference: PlanRef,
    id: String,
    snapshot: SnapshotRef,
    root: PlanNodeRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost: Option<String>,
    actual_available: bool,
    warnings: Vec<Diagnostic>,
}

impl ExplainedPlan {
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn nodes(&self) -> &[PlanNode] {
        &self.nodes
    }

    pub fn root(&self) -> &PlanNode {
        &self.nodes[0]
    }
}

impl Plan {
    pub fn reference(&self) -> &PlanRef {
        &self.reference
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn snapshot(&self) -> &SnapshotRef {
        &self.snapshot
    }

    pub fn root(&self) -> &PlanNodeRef {
        &self.root
    }

    pub fn estimated_cost(&self) -> Option<&str> {
        self.estimated_cost.as_deref()
    }

    pub const fn actual_available(&self) -> bool {
        self.actual_available
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplainError {
    InvalidSnapshot,
    InvalidObject,
    InvalidExpression,
    TooManyNodes,
    TooManyExpressions,
    InvalidDependency,
    InvalidPlanTree,
}

impl ExplainError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidSnapshot => "sys.explain.invalid_snapshot",
            Self::InvalidObject => "sys.explain.invalid_object",
            Self::InvalidExpression => "sys.explain.invalid_expression",
            Self::TooManyNodes => "sys.explain.plan_limit",
            Self::TooManyExpressions => "sys.explain.expression_limit",
            Self::InvalidDependency => "sys.explain.invalid_dependency",
            Self::InvalidPlanTree => "sys.explain.invalid_plan_tree",
        }
    }
}

impl fmt::Display for ExplainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for ExplainError {}

/// Plans a resolved query without changing how it is evaluated. A scan remains
/// the reference fallback; materialization is shown only when explicitly
/// requested. Statistics are estimates supplied by the snapshot adapter, and
/// actual fields remain absent because explain does not execute the query.
pub fn explain_query(query: &QueryPlanDescription) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_limit_chain(query, &[])
}

/// Explains a query while folding resolver-approved correlated subqueries into
/// predicate joins. Subquery identity remains attached to its source input and
/// resulting join if cost ordering moves it across known or unknown-cost
/// boundaries. Unknown-cost inputs remain after fully estimated inputs in
/// declaration order.
pub fn explain_query_with_decorrelated_subqueries(
    query: &QueryPlanDescription,
    subqueries: &[QueryDecorrelatedSubqueryDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        subqueries,
        &[],
        &[],
    )
}

/// Explains a query with stable identities for resolver-approved logical
/// join pairs. Pair metadata is looked up using the pair's exact right source
/// and optional predicate, then copied to both the source access node and its
/// join fold after physical cost ordering.
pub fn explain_query_with_join_pair_identities(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        pairs,
    )
}

/// Explains sparse joins with resolver-approved input limit pushdowns.
///
/// Each limit stage is matched to its exact logical pair identity and right
/// source, applied before that join, and included in an anchor-scoped fold
/// identity. The resolver supplies stages in their semantic order; this
/// adapter preserves that order while reordering known-cost join inputs.
pub fn explain_query_with_join_pair_identities_and_limit_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    limit_pushdowns: &[QueryLimitPushdownDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_limit_pushdowns(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        pairs,
        limit_pushdowns,
        &[],
        &[],
    )
}

/// Explains paired input limits composed with resolver-approved window
/// aggregate chains. Limits are applied before their exact-source window
/// stages; the combined identity binds both chains to the sparse join anchor.
/// The cumulative fold carries through joins where either chain is absent.
/// ORNA leaves this explain-only digest encoding unspecified.
pub fn explain_query_with_join_pair_identities_and_limit_window_aggregate_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    limit_pushdowns: &[QueryLimitPushdownDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_limit_pushdowns(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        aggregates,
        pairs,
        limit_pushdowns,
        &[],
        &[],
    )
}

/// Explains paired input limits composed with exact window aggregate and spill
/// chains. Spill estimates remain bound to their resolver-approved pair and
/// aggregate, while the sparse planner carries all three identities through
/// joins where one or more chains are absent. The adapter reports estimated
/// spill work; it does not perform or infer runtime compaction.
/// A cumulative fold additionally binds nested compaction history to paired
/// limit/aggregate restoration events. This explain-only identity policy is
/// adapter-defined because ORNA leaves digest composition unspecified.
pub fn explain_query_with_join_pair_identities_limit_window_aggregate_and_spill_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    limit_pushdowns: &[QueryLimitPushdownDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
    spills: &[QueryWindowSpillDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_limit_pushdowns(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        aggregates,
        pairs,
        limit_pushdowns,
        spills,
        &[],
    )
}

/// Explains a sparse join cascade while retaining logical join-pair,
/// window-pushdown, and paired aggregate-pushdown identities through physical
/// cost reordering. Window aggregates remain attached to their exact source
/// and ordered chain; each join fold includes that source chain in its
/// right-input identity and sparse anchor fold.
///
/// ORNA specifies structured plan rows but leaves window-chain identity
/// encoding open. This adapter uses a domain-separated digest of the source
/// reference and the resolver-ordered aggregate/frame tuples.
pub fn explain_query_with_join_pair_identities_and_window_aggregate_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        aggregates,
        pairs,
    )
}

/// Explains sparse joins with resolver-approved window aggregates and working
/// set spill estimates. Each spill is bound to one exact join pair, source,
/// and window aggregate. The adapter calculates estimated bytes beyond the
/// supplied memory budget, folds aggregate-chain spill byte/block/I/O totals
/// into an anchor-scoped identity, and preserves unknown estimates. It also
/// rolls those pair folds across the planned sparse join sequence, carrying
/// the latest identity through joins without spill stages. ORNA does not
/// prescribe this explain-only estimate or either fold encoding.
pub fn explain_query_with_join_pair_identities_window_aggregate_and_spill_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    aggregates: &[QueryWindowAggregatePushdownDescription],
    spills: &[QueryWindowSpillDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_limit_pushdowns(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        aggregates,
        pairs,
        &[],
        spills,
        &[],
    )
}

/// Explains window aggregates pushed to their resolved input sources.
///
/// Pushdown is attached by exact source identity, while each aggregate and
/// complete frame identity remain on the resulting node. A missing estimate
/// cannot shift a frame onto a neighboring input. Bounds are resolver supplied;
/// this adapter preserves them instead of inferring SQL frame semantics. The
/// caller supplies only aggregates already proven movable to that source
/// boundary; this explain adapter does not prove window algebra equivalence.
pub fn explain_query_with_window_aggregate_pushdowns(
    query: &QueryPlanDescription,
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        aggregates,
        &[],
    )
}

/// Explains a query while choosing exact-identity partial indexes for join
/// inputs. A candidate is selected only when both its table and partial
/// predicate equal the join input's table and predicate. For duplicate exact
/// matches, the lexicographically smallest index reference wins. Unmatched or
/// predicate-free joins retain their table scan.
pub fn explain_query_with_partial_indexes(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_predicate_pressure_and_branch_limits_and_storms_with_partial_indexes(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
    )
}

/// Explains exact partial-index selection together with resolver-supplied
/// logical join-pair identities. The pair and its selected predicate/index
/// identity move together through physical cost ordering and the fold chain.
pub fn explain_query_with_partial_indexes_and_join_pair_identities(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
    )
}

/// Explains paired index routes while retaining checkpoint segment compaction
/// lineage inside the branch-local cost restoration fold. Every supplied
/// chain must name one exact join pair and the mutable branch generation on
/// that input. Sparse inputs carry the prior fold; paired checkpoint steps
/// advance it in planned cost order. Compaction metadata is explain-only and
/// does not modify estimates or execute storage operations.
pub fn explain_query_with_partial_indexes_and_paired_checkpoint_segment_compaction_chains(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_limit_pushdowns(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
        &[],
        &[],
        checkpoint_chains,
    )
}

/// Explains paired cost-restoration folds with resolver-approved window
/// aggregate chains. The fold retains each exact source window identity across
/// restore pairs and sparse joins. ORNA leaves this explain-only identity
/// composition open; it does not change cardinality or execution.
pub fn explain_query_with_paired_cost_restoration_and_window_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
    stream_rotation_chains: &[QueryPairedStreamRotationChainDescription],
    wal_rotation_chains: &[QueryPairedWalRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_wal_rotation_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        window_aggregates,
        pairs,
        &[],
        &[],
        checkpoint_chains,
        segment_rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
        stream_rotation_chains,
        wal_rotation_chains,
    )
}

/// Explains paired cost-restoration folds together with exact window spills.
/// Each spill pair is joined to the window identity and restore chains on the
/// same input edge, then retained by the cumulative sparse fold. ORNA leaves
/// this explain-only identity composition open; spill estimates and plan
/// cardinalities keep their existing behavior.
pub fn explain_query_with_paired_cost_restoration_and_window_spill_pushdowns(
    query: &QueryPlanDescription,
    pairs: &[QueryJoinPairIdentityDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
    stream_rotation_chains: &[QueryPairedStreamRotationChainDescription],
    wal_rotation_chains: &[QueryPairedWalRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_wal_rotation_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        window_aggregates,
        pairs,
        &[],
        window_spills,
        checkpoint_chains,
        segment_rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
        stream_rotation_chains,
        wal_rotation_chains,
    )
}

/// Extends the branch-local checkpoint-compaction fold with paired segment
/// rotation chains. Rotation entries remain in caller order and retain their
/// source stream, fold, and order coordinates through sparse join routes.
/// Each chain must resolve to the exact paired input and mutable branch
/// generation. The lineage affects explain identities only, not estimates.
pub fn explain_query_with_partial_indexes_and_paired_checkpoint_compaction_and_segment_rotation_chains(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    rotation_chains: &[QueryPairedSegmentRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_segment_rotation_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
        &[],
        &[],
        checkpoint_chains,
        rotation_chains,
    )
}

/// Extends paired checkpoint and segment-rotation ancestry with ordered
/// stream-compaction occurrences. The fold preserves each restore, handoff,
/// stream, compaction, fold, and order coordinate, including absent checkpoint
/// sides. It is explain-only: estimates and execution remain unchanged.
pub fn explain_query_with_partial_indexes_and_paired_checkpoint_rotation_and_stream_compaction_chains(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_stream_compaction_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
        &[],
        &[],
        checkpoint_chains,
        rotation_chains,
        stream_compaction_chains,
    )
}

/// Adds sparse paired checkpoint-spill restores after checkpoint, rotation,
/// and stream-compaction ancestry. Spill, stream, compaction, handoff, fold,
/// order, optional generations, positions, and redo-fold identities are
/// preserved as explain metadata. The reference does not define planner fold
/// labels, so input order is the stable local ancestry rule.
pub fn explain_query_with_partial_indexes_and_paired_checkpoint_rotation_stream_and_spill_restore_chains(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_checkpoint_spill_restore_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
        &[],
        &[],
        checkpoint_chains,
        rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
    )
}

/// Extends branch-local checkpoint, stream-compaction, and spill-restoration
/// ancestry with paired stream rotations. Each rotation retains its source
/// stream, checkpoint, rotation/fold/order coordinates, and optional sides.
/// The reference does not specify this combined planner fold, so caller order
/// is retained as deterministic provenance. Explain identities change; cost
/// estimates do not.
pub fn explain_query_with_partial_indexes_and_paired_checkpoint_stream_rotation_chains(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
    stream_rotation_chains: &[QueryPairedStreamRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_stream_rotation_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
        &[],
        &[],
        checkpoint_chains,
        rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
        stream_rotation_chains,
    )
}

/// Adds nested paired WAL-rotation restoration after the checkpoint, stream,
/// spill, and stream-rotation ancestry. Each occurrence preserves its caller
/// coordinates and atomic directional redo/WAL/segment identities. The
/// reference is silent on the combined explain fold; caller order is retained
/// as stable provenance. This metadata does not alter estimates.
pub fn explain_query_with_partial_indexes_and_paired_checkpoint_wal_rotation_chains(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    pairs: &[QueryJoinPairIdentityDescription],
    checkpoint_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
    stream_rotation_chains: &[QueryPairedStreamRotationChainDescription],
    wal_rotation_chains: &[QueryPairedWalRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_wal_rotation_chains(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        &[],
        &[],
        pairs,
        &[],
        &[],
        checkpoint_chains,
        rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
        stream_rotation_chains,
        wal_rotation_chains,
    )
}

/// Explains exact partial-index pushdowns and resolver-approved correlated
/// subqueries in one sparse cost cascade. Predicate and subquery identities
/// remain paired to their exact right input as physical ordering moves known
/// inputs ahead of sparse unknown-cost inputs.
pub fn explain_query_with_partial_indexes_and_decorrelated_subqueries(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    subqueries: &[QueryDecorrelatedSubqueryDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        subqueries,
        &[],
        &[],
    )
}

/// Explains partial-index routes and resolver-approved decorrelated inputs
/// together with their logical join-pair identities. A shared restoration
/// chain records both event kinds in planned cost order, carrying through
/// joins with no route or decorrelation descriptor so sparse inputs cannot
/// detach a later outcome from its ancestry.
pub fn explain_query_with_partial_indexes_and_decorrelated_subqueries_and_join_pair_identities(
    query: &QueryPlanDescription,
    indexes: &[QueryPartialIndexDescription],
    subqueries: &[QueryDecorrelatedSubqueryDescription],
    pairs: &[QueryJoinPairIdentityDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        &[],
        &[],
        &[],
        indexes,
        subqueries,
        &[],
        pairs,
    )
}

/// Explains a query followed by additional ordered limit stages.
///
/// The optional `query.limit` is applied first, followed by each value in
/// `additional_limits`. Every stage retains its own input-row work estimate;
/// reducing later output cardinality cannot refund work from earlier stages.
pub fn explain_query_with_limit_chain(
    query: &QueryPlanDescription,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_disjunct_limit_chain(query, 1, additional_limits)
}

/// Explains a predicate expanded into disjunctive branches followed by an
/// ordered limit chain.
///
/// ORNA-PLAN leaves disjunct selectivity and cost estimates unspecified; this
/// adapter uses the following deterministic fallback in the absence of
/// histograms. Each disjunct is estimated to match half of the input rows.
/// The planner assumes independent branch selectivity, so the OR output
/// estimate is `input * (1 - 0.5^disjunct_count)` rounded up and capped by the
/// input. Each expanded branch examines the full input; later limits retain
/// that work and cannot refund it. A zero branch count is invalid.
pub fn explain_query_with_disjunct_limit_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        None,
        None,
        &[],
        &[],
        None,
        additional_limits,
    )
}

/// Explains a disjunction followed by a left-to-right chain of conjunctive
/// predicates and ordered limit stages.
///
/// ORNA-PLAN leaves predicate selectivity and cost estimates unspecified. In
/// the absence of histograms, each OR arm and each subsequent AND conjunct is
/// estimated at 50% selectivity. The planner charges every expanded OR arm
/// for the full input, then charges each AND conjunct for the rows surviving
/// earlier conjuncts. AND work follows the reference's left-to-right short-
/// circuit order. Limits retain their input-row work and cannot refund this
/// predicate work. `conjunct_count` must be nonzero and describes the AND
/// terms evaluated after the expanded disjunction.
pub fn explain_query_with_disjunct_conjunct_limit_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        Some(conjunct_count),
        None,
        &[],
        &[],
        None,
        additional_limits,
    )
}

/// Explains a disjunction whose expanded arms each contain a short-circuiting
/// chain of conjuncts, followed by ordered limit stages.
///
/// ORNA-PLAN does not prescribe selectivity or expanded-filter cost
/// aggregation. Without histograms, each conjunct is estimated to match half
/// of its input, arm selectivity is the product of those conjunct estimates,
/// and arms are treated as independent. Every expanded arm is charged against
/// the full source input; within an arm, each later conjunct is charged only
/// against rows surviving its predecessors. Integer match estimates round up
/// at each arm. `conjunct_count_per_disjunct` and `disjunct_count` must be
/// nonzero, and their product must fit the plan expression bound.
pub fn explain_query_with_conjunct_disjunct_limit_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0 || conjunct_count_per_disjunct == 0 {
        return Err(ExplainError::InvalidExpression);
    }
    if disjunct_count.saturating_mul(conjunct_count_per_disjunct) > MAX_PLAN_EXPRESSIONS as u64 {
        return Err(ExplainError::TooManyExpressions);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        None,
        Some(conjunct_count_per_disjunct),
        &[],
        &[],
        None,
        additional_limits,
    )
}

/// Explains disjunct expansion whose individual branches each run a nested
/// limit chain followed by a left-to-right conjunct chain, then applies the
/// query's outer limit and any additional limit stages.
///
/// Each branch starts from the same source/join input. Its nested limits run
/// in order before its conjuncts; limit work is charged once per disjunct.
/// The disjunction then combines the branch estimates. ORNA-PLAN leaves this
/// estimate composition unspecified, so the adapter uses the deterministic
/// 50%-per-conjunct, independent-branch fallback and rounds each branch's
/// surviving rows upward. The query's `limit` and `additional_limits` run
/// after the disjunction and retain all branch work. `disjunct_count`,
/// and `conjunct_count_per_disjunct` must be positive; `nested_branch_limits`
/// must be nonempty and the query must have a predicate.
pub fn explain_query_with_disjunct_branch_limit_conjunct_cascade(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    nested_branch_limits: &[u64],
    conjunct_count_per_disjunct: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0
        || nested_branch_limits.is_empty()
        || conjunct_count_per_disjunct == 0
        || query.predicate.is_none()
    {
        return Err(ExplainError::InvalidExpression);
    }
    if disjunct_count.saturating_mul(conjunct_count_per_disjunct)
        > MAX_PLAN_EXPRESSIONS as u64
    {
        return Err(ExplainError::TooManyExpressions);
    }
    explain_query_with_predicate_pressure_and_branch_limits(
        query,
        disjunct_count,
        None,
        Some(conjunct_count_per_disjunct),
        &[],
        nested_branch_limits,
        &[],
        None,
        additional_limits,
    )
}

/// Explains a sequence of nested disjunct storms. Every stage starts from the
/// previous stage's estimated output, expands its own disjuncts, applies an
/// ordered limit cascade to each branch, evaluates that branch's conjunct
/// chain, and combines the surviving branch estimates before feeding the
/// next storm. The query's outer limit and `additional_limits` run after all
/// storms.
///
/// ORNA-PLAN leaves estimate aggregation unspecified for this composition.
/// The adapter uses independent branches with 50% selectivity per conjunct,
/// rounds branch matches upward, and charges every branch limit for each
/// disjunct. It retains work and known overflow from earlier storms through
/// later stages and zero limits. The query-level predicate must be absent;
/// each `DisjunctStormDescription` supplies the predicate for its stage.
pub fn explain_query_with_disjunct_storm_chain(
    query: &QueryPlanDescription,
    storms: &[DisjunctStormDescription],
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if storms.is_empty() || query.predicate.is_some() {
        return Err(ExplainError::InvalidExpression);
    }
    if storms.iter().any(|storm| {
        storm.disjunct_count == 0
            || storm.nested_branch_limits.is_empty()
            || storm.conjunct_count_per_disjunct == 0
    }) {
        return Err(ExplainError::InvalidExpression);
    }
    explain_query_with_predicate_pressure_and_branch_limits_and_storms(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        additional_limits,
        storms,
        &[],
    )
}

/// Explains ordered disjunct-storm stages whose branches may use different
/// nested limit chains and conjunct counts.
///
/// ORNA-PLAN does not specify estimates for branch-local limit chains inside
/// nested disjunctions or where nested cascades rebind within those chains.
/// This adapter applies each branch's limits and 50%-per-conjunct fallback
/// independently from the same stage input, combines branch matches in
/// declaration order without counting more rows or bytes than that input, and
/// feeds the bounded result to the next storm stage. Every cascade result is
/// capped to its immediate input at every nesting depth, so a nested rebind
/// cannot expand beyond its ancestor branch's bounded estimate. The immediate
/// cap at a rebind is the current branch's bounded rows and bytes. An explicit
/// rebind runs just after its one-based limit position and feeds its capped
/// output to the following limit. Rebinds at the same position run in
/// declaration order, each consuming the previous rebind's bounded result.
/// Row and byte estimates are capped independently; an unknown dimension stays
/// unknown while a known dimension continues to use its immediate input cap.
/// Unknown row-based work does not erase a known byte estimate.
/// A rebind nested inside a cascade uses that nested branch's own post-limit
/// estimate as its cap, so nested work cannot borrow a wider ancestor cap.
/// The known byte cap therefore follows the immediate branch input at every
/// rebind depth, even when every row-based work estimate is unknown.
/// Plan details report the deepest cascade level that contains a rebind so the
/// nested cap boundary is visible alongside the recursive branch shape.
/// Since
/// `sys.PlanNodeKind` has no union node, each storm is one aggregate filter
/// node whose details retain the exact branch chains and rebind points; this
/// avoids presenting sibling limits as a false serial pipeline. The
/// query-level predicate must be absent because every stage supplies its own
/// predicate.
pub fn explain_query_with_disjunct_storm_branch_limit_chains(
    query: &QueryPlanDescription,
    storms: &[DisjunctStormCascadeDescription],
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if storms.is_empty() || query.predicate.is_some() {
        return Err(ExplainError::InvalidExpression);
    }
    explain_query_with_predicate_pressure_and_branch_limits_and_storms(
        query,
        1,
        None,
        None,
        &[],
        &[],
        &[],
        None,
        additional_limits,
        &[],
        storms,
    )
}

/// Explains nested input limits, one expanded disjunctive filter, and then
/// the query's outer limit followed by any additional limits.
///
/// The input limits model an already nested relation and run after source and
/// join work but before the predicate. Each limit is charged for its immediate
/// input rows; disjunct expansion then charges each arm for the bounded rows
/// that remain. ORNA-PLAN leaves cost aggregation unspecified, so this keeps
/// the same documented independent 50%-per-arm fallback as
/// [`explain_query_with_disjunct_limit_chain`].
pub fn explain_query_with_input_limit_disjunct_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    nested_input_limits: &[u64],
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0 || query.predicate.is_none() {
        return Err(ExplainError::InvalidExpression);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        None,
        None,
        nested_input_limits,
        &[],
        None,
        additional_limits,
    )
}

/// Explains nested input limits, expanded disjuncts with a conjunct chain in
/// each arm, and then the query's outer limit followed by any additional
/// limits.
///
/// Each nested input limit runs after source and join work but before the
/// predicate, and is charged for its immediate input rows. Every disjunct arm
/// then starts from the capped rows and evaluates its conjuncts
/// left-to-right, charging later conjuncts only for rows surviving earlier
/// ones. ORNA-PLAN leaves estimate aggregation unspecified, so the documented
/// independent 50%-per-conjunct fallback is applied within each arm; each arm
/// is charged against the full capped input.
pub fn explain_query_with_input_limit_conjunct_disjunct_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
    nested_input_limits: &[u64],
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0 || conjunct_count_per_disjunct == 0 || query.predicate.is_none() {
        return Err(ExplainError::InvalidExpression);
    }
    if disjunct_count.saturating_mul(conjunct_count_per_disjunct)
        > MAX_PLAN_EXPRESSIONS as u64
    {
        return Err(ExplainError::TooManyExpressions);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        None,
        Some(conjunct_count_per_disjunct),
        nested_input_limits,
        &[],
        None,
        additional_limits,
    )
}

/// Explains nested input limits, disjunct expansion, and then a separate
/// left-to-right conjunct chain followed by the query's outer limit and any
/// additional limits.
///
/// The input limits run once after source and join work and before the
/// disjunctive predicate. Disjunct expansion charges each arm for the capped
/// input; the conjunct chain then charges each term for the rows surviving
/// its predecessors. ORNA-PLAN leaves these estimates unspecified, so the
/// same deterministic 50%-per-arm and 50%-per-conjunct fallback is used.
/// `query.predicate` describes the disjunction and `conjunct_predicate`
/// describes the later AND chain. At least one nested limit, one disjunct,
/// and one conjunct are required.
pub fn explain_query_with_input_limit_disjunct_conjunct_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    nested_input_limits: &[u64],
    conjunct_predicate: ExpressionRef,
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0
        || nested_input_limits.is_empty()
        || conjunct_count == 0
        || query.predicate.is_none()
    {
        return Err(ExplainError::InvalidExpression);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        Some(conjunct_count),
        None,
        nested_input_limits,
        &[],
        Some(&conjunct_predicate),
        additional_limits,
    )
}

/// Explains nested input limits, disjunct expansion, a nested limit chain,
/// and then a left-to-right conjunct chain followed by the query's outer
/// limit and any additional limits.
///
/// Input limits run after source and join work, before disjunct expansion.
/// Each expanded disjunct is charged against the capped input; the nested
/// disjunct limits run after expansion and charge their immediate input. The
/// conjunct chain runs after those limits and charges each term for rows
/// surviving earlier terms. ORNA-PLAN leaves these estimate choices
/// unspecified, so this adapter uses independent 50% selectivity per OR arm
/// and left-to-right 50% selectivity per conjunct. Earlier work remains
/// charged when later limits reduce the estimated rows. At least one input
/// limit, one disjunct, one post-expansion limit, and one conjunct are
/// required. `query.predicate` describes the disjunction and
/// `conjunct_predicate` describes the later AND chain.
pub fn explain_query_with_input_disjunct_limit_conjunct_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    nested_input_limits: &[u64],
    nested_disjunct_limits: &[u64],
    conjunct_predicate: ExpressionRef,
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0
        || nested_input_limits.is_empty()
        || nested_disjunct_limits.is_empty()
        || conjunct_count == 0
        || query.predicate.is_none()
    {
        return Err(ExplainError::InvalidExpression);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        Some(conjunct_count),
        None,
        nested_input_limits,
        nested_disjunct_limits,
        Some(&conjunct_predicate),
        additional_limits,
    )
}

/// Explains nested input limits, expanded disjuncts whose arms each contain a
/// conjunct chain, a nested disjunct limit cascade, and then a separate
/// conjunct chain followed by the query's outer limit and any additional
/// limits.
///
/// Input limits run after source and join work and before disjunct expansion.
/// Each disjunct arm evaluates its conjuncts left-to-right against the capped
/// input. The nested disjunct limits then charge their immediate input before
/// the separate final conjunct chain runs. ORNA-PLAN leaves selectivity and
/// estimate aggregation unspecified, so this adapter uses independent 50%
/// selectivity per arm and per conjunct, rounds branch row estimates upward,
/// and keeps prior work charged after later caps. At least one input limit,
/// one disjunct, one per-arm conjunct, one post-disjunct limit, and one final
/// conjunct are required. `query.predicate` describes the disjunction with
/// its per-arm conjuncts; `conjunct_predicate` describes the later AND chain.
pub fn explain_query_with_input_limit_conjunct_disjunct_limit_conjunct_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
    nested_input_limits: &[u64],
    nested_disjunct_limits: &[u64],
    conjunct_predicate: ExpressionRef,
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0
        || conjunct_count_per_disjunct == 0
        || nested_input_limits.is_empty()
        || nested_disjunct_limits.is_empty()
        || conjunct_count == 0
        || query.predicate.is_none()
    {
        return Err(ExplainError::InvalidExpression);
    }
    if disjunct_count.saturating_mul(conjunct_count_per_disjunct)
        > MAX_PLAN_EXPRESSIONS as u64
    {
        return Err(ExplainError::TooManyExpressions);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        Some(conjunct_count),
        Some(conjunct_count_per_disjunct),
        nested_input_limits,
        nested_disjunct_limits,
        Some(&conjunct_predicate),
        additional_limits,
    )
}

/// Explains an expanded disjunction, a nested limit chain, and then a
/// left-to-right conjunct chain followed by the query's outer limit and any
/// additional limits.
///
/// Each disjunct is estimated at 50% selectivity and charged against the full
/// input. The nested limits run after that expansion, retain its work, and
/// charge each immediate input. The subsequent conjuncts use the same
/// deterministic 50%-per-term fallback, with each term charged only for rows
/// surviving its predecessors. `query.predicate` describes the disjunctive
/// predicate, and `conjunct_predicate` describes the post-limit conjunct
/// chain. ORNA-PLAN does not specify these estimate aggregation choices. At
/// least one nested limit, one disjunct, and one conjunct are required.
pub fn explain_query_with_disjunct_limit_conjunct_chain(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    nested_limits: &[u64],
    conjunct_predicate: ExpressionRef,
    conjunct_count: u64,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    if disjunct_count == 0
        || nested_limits.is_empty()
        || conjunct_count == 0
        || query.predicate.is_none()
    {
        return Err(ExplainError::InvalidExpression);
    }
    explain_query_with_predicate_pressure(
        query,
        disjunct_count,
        Some(conjunct_count),
        None,
        &[],
        nested_limits,
        Some(&conjunct_predicate),
        additional_limits,
    )
}

fn explain_query_with_predicate_pressure(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_predicate_pressure_and_branch_limits(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        &[],
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
    )
}

fn explain_query_with_predicate_pressure_and_branch_limits(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_predicate_pressure_and_branch_limits_and_storms(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        &[],
        &[],
    )
}

fn disjunct_storm_cascade_shape_counts<'a>(
    storms: &'a [DisjunctStormCascadeDescription],
) -> Result<(usize, usize, Vec<&'a ExpressionRef>), ExplainError> {
    let mut pending = storms.iter().map(|storm| (storm, 1usize)).collect::<Vec<_>>();
    let mut operators = 0usize;
    let mut expressions = 0usize;
    let mut predicates = Vec::new();
    while let Some((storm, depth)) = pending.pop() {
        if depth > MAX_DISJUNCT_STORM_DEPTH {
            return Err(ExplainError::TooManyNodes);
        }
        if storm.branches.is_empty() {
            return Err(ExplainError::InvalidExpression);
        }
        operators = operators.saturating_add(1);
        predicates.push(&storm.predicate);
        for branch in &storm.branches {
            if branch.nested_limits.is_empty() || branch.conjunct_count == 0 {
                return Err(ExplainError::InvalidExpression);
            }
            operators = operators
                .saturating_add(branch.nested_limits.len())
                .saturating_add(branch.limit_rebinds.len());
            expressions = expressions.saturating_add(
                usize::try_from(branch.conjunct_count).unwrap_or(usize::MAX),
            );
            let mut previous_rebind_position = 0;
            for rebind in &branch.limit_rebinds {
                if rebind.after_limit == 0
                    || rebind.after_limit > branch.nested_limits.len()
                    || rebind.after_limit < previous_rebind_position
                    || rebind.storms.is_empty()
                {
                    return Err(ExplainError::InvalidExpression);
                }
                previous_rebind_position = rebind.after_limit;
            }
            pending.extend(
                branch
                    .limit_rebinds
                    .iter()
                    .flat_map(|rebind| rebind.storms.iter())
                    .map(|nested| (nested, depth.saturating_add(1))),
            );
            pending.extend(
                branch
                    .nested_storms
                    .iter()
                    .map(|nested| (nested, depth.saturating_add(1))),
            );
            if operators > MAX_PLAN_NODES {
                return Err(ExplainError::TooManyNodes);
            }
            if expressions > MAX_PLAN_EXPRESSIONS {
                return Err(ExplainError::TooManyExpressions);
            }
        }
    }
    Ok((operators, expressions, predicates))
}

fn explain_query_with_predicate_pressure_and_branch_limits_and_storms(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_with_predicate_pressure_and_branch_limits_and_storms_with_partial_indexes(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        &[],
    )
}

fn explain_query_with_predicate_pressure_and_branch_limits_and_storms_with_partial_indexes(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_subqueries(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        &[],
        &[],
        &[],
    )
}

fn explain_query_core_with_subqueries(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_limit_pushdowns(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        decorrelated_subqueries,
        window_aggregates,
        join_pair_identities,
        &[],
        &[],
        &[],
    )
}

fn explain_query_core_with_limit_pushdowns(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
    input_limit_pushdowns: &[QueryLimitPushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_compaction_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_segment_rotation_chains(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        decorrelated_subqueries,
        window_aggregates,
        join_pair_identities,
        input_limit_pushdowns,
        window_spills,
        checkpoint_compaction_chains,
        &[],
    )
}

fn explain_query_core_with_segment_rotation_chains(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
    input_limit_pushdowns: &[QueryLimitPushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_compaction_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_stream_compaction_chains(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        decorrelated_subqueries,
        window_aggregates,
        join_pair_identities,
        input_limit_pushdowns,
        window_spills,
        checkpoint_compaction_chains,
        segment_rotation_chains,
        &[],
    )
}

fn explain_query_core_with_stream_compaction_chains(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
    input_limit_pushdowns: &[QueryLimitPushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_compaction_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_checkpoint_spill_restore_chains(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        decorrelated_subqueries,
        window_aggregates,
        join_pair_identities,
        input_limit_pushdowns,
        window_spills,
        checkpoint_compaction_chains,
        segment_rotation_chains,
        stream_compaction_chains,
        &[],
    )
}

fn explain_query_core_with_checkpoint_spill_restore_chains(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
    input_limit_pushdowns: &[QueryLimitPushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_compaction_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_stream_rotation_chains(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        decorrelated_subqueries,
        window_aggregates,
        join_pair_identities,
        input_limit_pushdowns,
        window_spills,
        checkpoint_compaction_chains,
        segment_rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
        &[],
    )
}

fn explain_query_core_with_stream_rotation_chains(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
    input_limit_pushdowns: &[QueryLimitPushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_compaction_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
    stream_rotation_chains: &[QueryPairedStreamRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    explain_query_core_with_wal_rotation_chains(
        query,
        disjunct_count,
        conjunct_count,
        conjunct_count_per_disjunct,
        nested_input_limits,
        nested_branch_limits,
        limits_between_disjunct_and_conjunct,
        post_expansion_conjunct,
        additional_limits,
        disjunct_storms,
        disjunct_storm_cascades,
        partial_indexes,
        decorrelated_subqueries,
        window_aggregates,
        join_pair_identities,
        input_limit_pushdowns,
        window_spills,
        checkpoint_compaction_chains,
        segment_rotation_chains,
        stream_compaction_chains,
        spill_restore_chains,
        stream_rotation_chains,
        &[],
    )
}

fn explain_query_core_with_wal_rotation_chains(
    query: &QueryPlanDescription,
    disjunct_count: u64,
    conjunct_count: Option<u64>,
    conjunct_count_per_disjunct: Option<u64>,
    nested_input_limits: &[u64],
    nested_branch_limits: &[u64],
    limits_between_disjunct_and_conjunct: &[u64],
    post_expansion_conjunct: Option<&ExpressionRef>,
    additional_limits: &[u64],
    disjunct_storms: &[DisjunctStormDescription],
    disjunct_storm_cascades: &[DisjunctStormCascadeDescription],
    partial_indexes: &[QueryPartialIndexDescription],
    decorrelated_subqueries: &[QueryDecorrelatedSubqueryDescription],
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    join_pair_identities: &[QueryJoinPairIdentityDescription],
    input_limit_pushdowns: &[QueryLimitPushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    checkpoint_compaction_chains: &[QueryPairedCheckpointSegmentCompactionChainDescription],
    segment_rotation_chains: &[QueryPairedSegmentRotationChainDescription],
    stream_compaction_chains: &[QueryPairedStreamCompactionChainDescription],
    spill_restore_chains: &[QueryPairedCheckpointSpillRestoreChainDescription],
    stream_rotation_chains: &[QueryPairedStreamRotationChainDescription],
    wal_rotation_chains: &[QueryPairedWalRotationChainDescription],
) -> Result<ExplainedPlan, ExplainError> {
    if query
        .joins
        .len()
        .saturating_add(decorrelated_subqueries.len())
        .saturating_add(window_aggregates.len())
        .saturating_add(join_pair_identities.len())
        .saturating_add(input_limit_pushdowns.len())
        .saturating_add(window_spills.len())
        .saturating_add(checkpoint_compaction_chains.len())
        .saturating_add(segment_rotation_chains.len())
        .saturating_add(stream_compaction_chains.len())
        .saturating_add(spill_restore_chains.len())
        .saturating_add(stream_rotation_chains.len())
        .saturating_add(wal_rotation_chains.len())
        > MAX_PLAN_NODES
    {
        return Err(ExplainError::TooManyNodes);
    }
    if decorrelated_subqueries
        .iter()
        .any(|subquery| invalid_reference(subquery.identity.as_str()))
    {
        return Err(ExplainError::InvalidObject);
    }
    let declared_join_count = query.joins.len();
    let mut expanded_query = query.clone();
    expanded_query
        .joins
        .extend(
            decorrelated_subqueries
                .iter()
                .map(|subquery| QueryJoinDescription {
                    source: subquery.source.clone(),
                    statistics: subquery.statistics.clone(),
                    predicate: Some(subquery.correlation_predicate.clone()),
                }),
        );
    let query = &expanded_query;

    let mut window_identities = BTreeSet::new();
    for aggregate in window_aggregates {
        if invalid_reference(aggregate.identity.as_str())
            || invalid_reference(aggregate.source.as_str())
        {
            return Err(ExplainError::InvalidObject);
        }
        if invalid_reference(aggregate.aggregate.as_str())
            || invalid_reference(aggregate.frame_identity.as_str())
            || aggregate.frame_start.ordinal() > aggregate.frame_end.ordinal()
        {
            return Err(ExplainError::InvalidExpression);
        }
        if !window_identities.insert(aggregate.identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let matching_sources = usize::from(query.source == aggregate.source)
            + query.joins.iter().filter(|join| join.source == aggregate.source).count();
        if matching_sources != 1 {
            return Err(ExplainError::InvalidObject);
        }
    }

    let mut input_limit_identities_seen = BTreeSet::new();
    for limit_pushdown in input_limit_pushdowns {
        if invalid_reference(limit_pushdown.identity.as_str())
            || invalid_reference(limit_pushdown.join_pair_identity.as_str())
            || invalid_reference(limit_pushdown.source.as_str())
        {
            return Err(ExplainError::InvalidObject);
        }
        if !input_limit_identities_seen.insert(limit_pushdown.identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
    }

    let mut join_pair_identities_seen = BTreeSet::new();
    let mut join_pair_targets_seen = BTreeSet::new();
    for pair in join_pair_identities {
        if invalid_reference(pair.identity.as_str())
            || invalid_reference(pair.left_source.as_str())
            || invalid_reference(pair.right_source.as_str())
        {
            return Err(ExplainError::InvalidObject);
        }
        if pair
            .predicate
            .as_ref()
            .is_some_and(|predicate| invalid_reference(predicate.as_str()))
        {
            return Err(ExplainError::InvalidExpression);
        }
        if pair.left_source == pair.right_source
            || !join_pair_identities_seen.insert(pair.identity.as_str())
        {
            return Err(ExplainError::InvalidObject);
        }
        let left_source_count = usize::from(query.source == pair.left_source)
            + query
                .joins
                .iter()
                .filter(|join| join.source == pair.left_source)
                .count();
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| {
                join.source == pair.right_source && join.predicate == pair.predicate
            })
            .count();
        if left_source_count != 1 || matching_joins != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let target = (
            pair.right_source.as_str().to_owned(),
            pair.predicate
                .as_ref()
                .map(|predicate| predicate.as_str().to_owned()),
        );
        if !join_pair_targets_seen.insert(target) {
            return Err(ExplainError::InvalidObject);
        }
    }
    let mut checkpoint_chain_pairs_seen = BTreeSet::new();
    let mut checkpoint_step_count = 0usize;
    for chain in checkpoint_compaction_chains {
        if invalid_reference(chain.join_pair_identity.as_str())
            || invalid_reference(&chain.branch.name)
            || chain.steps.is_empty()
        {
            return Err(ExplainError::InvalidObject);
        }
        if !checkpoint_chain_pairs_seen.insert(chain.join_pair_identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let matching_pairs = join_pair_identities
            .iter()
            .filter(|pair| pair.identity == chain.join_pair_identity)
            .count();
        if matching_pairs != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let pair = join_pair_identities
            .iter()
            .find(|pair| pair.identity == chain.join_pair_identity)
            .expect("one paired identity was counted");
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| {
                join.source == pair.right_source && join.predicate == pair.predicate
            })
            .collect::<Vec<_>>();
        if matching_joins.len() != 1
            || matching_joins[0]
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.mutable_branch.as_ref())
                != Some(&chain.branch)
        {
            return Err(ExplainError::InvalidObject);
        }
        checkpoint_step_count = checkpoint_step_count.saturating_add(chain.steps.len());
        if checkpoint_step_count > MAX_PLAN_NODES {
            return Err(ExplainError::TooManyNodes);
        }
        for step in &chain.steps {
            if invalid_reference(step.checkpoint_identity.as_str())
                || step
                    .left_segment_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
                || step
                    .right_segment_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
                || step
                    .left_compaction_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
                || step
                    .right_compaction_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
                || step.left_segment_identity.is_some()
                    != step.left_compaction_identity.is_some()
                || step.right_segment_identity.is_some()
                    != step.right_compaction_identity.is_some()
                || (step.left_segment_identity.is_none()
                    && step.right_segment_identity.is_none())
            {
                return Err(ExplainError::InvalidObject);
            }
        }
    }
    let mut segment_rotation_pairs_seen = BTreeSet::new();
    let mut segment_rotation_count = 0usize;
    for chain in segment_rotation_chains {
        if invalid_reference(chain.join_pair_identity.as_str())
            || invalid_reference(&chain.branch.name)
            || chain.rotations.is_empty()
        {
            return Err(ExplainError::InvalidObject);
        }
        if !segment_rotation_pairs_seen.insert(chain.join_pair_identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let matching_pairs = join_pair_identities
            .iter()
            .filter(|pair| pair.identity == chain.join_pair_identity)
            .count();
        if matching_pairs != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let pair = join_pair_identities
            .iter()
            .find(|pair| pair.identity == chain.join_pair_identity)
            .expect("one paired identity was counted");
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| {
                join.source == pair.right_source && join.predicate == pair.predicate
            })
            .collect::<Vec<_>>();
        if matching_joins.len() != 1
            || matching_joins[0]
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.mutable_branch.as_ref())
                != Some(&chain.branch)
        {
            return Err(ExplainError::InvalidObject);
        }
        segment_rotation_count = segment_rotation_count.saturating_add(chain.rotations.len());
        if checkpoint_step_count.saturating_add(segment_rotation_count) > MAX_PLAN_NODES {
            return Err(ExplainError::TooManyNodes);
        }
        for rotation in &chain.rotations {
            if invalid_reference(rotation.checkpoint_identity.as_str())
                || invalid_reference(rotation.left_segment_identity.as_str())
                || invalid_reference(rotation.right_segment_identity.as_str())
            {
                return Err(ExplainError::InvalidObject);
            }
        }
    }
    let mut stream_compaction_pairs_seen = BTreeSet::new();
    let mut stream_compaction_occurrence_count = 0usize;
    for chain in stream_compaction_chains {
        if invalid_reference(chain.join_pair_identity.as_str())
            || invalid_reference(&chain.branch.name)
            || chain.occurrences.is_empty()
        {
            return Err(ExplainError::InvalidObject);
        }
        if !stream_compaction_pairs_seen.insert(chain.join_pair_identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let pair = join_pair_identities
            .iter()
            .filter(|pair| pair.identity == chain.join_pair_identity)
            .collect::<Vec<_>>();
        if pair.len() != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| {
                join.source == pair[0].right_source && join.predicate == pair[0].predicate
            })
            .collect::<Vec<_>>();
        if matching_joins.len() != 1
            || matching_joins[0]
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.mutable_branch.as_ref())
                != Some(&chain.branch)
        {
            return Err(ExplainError::InvalidObject);
        }
        stream_compaction_occurrence_count =
            stream_compaction_occurrence_count.saturating_add(chain.occurrences.len());
        if checkpoint_step_count
            .saturating_add(segment_rotation_count)
            .saturating_add(stream_compaction_occurrence_count)
            > MAX_PLAN_NODES
        {
            return Err(ExplainError::TooManyNodes);
        }
        for occurrence in &chain.occurrences {
            if invalid_reference(occurrence.checkpoint_identity.as_str())
                || invalid_reference(occurrence.redo_fold_identity.as_str())
                || invalid_reference(occurrence.left_segment_identity.as_str())
                || invalid_reference(occurrence.right_segment_identity.as_str())
                || occurrence
                    .left_checkpoint_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
                || occurrence
                    .right_checkpoint_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
            {
                return Err(ExplainError::InvalidObject);
            }
        }
    }
    let mut spill_restore_pairs_seen = BTreeSet::new();
    let mut spill_restore_occurrence_count = 0usize;
    for chain in spill_restore_chains {
        if invalid_reference(chain.join_pair_identity.as_str())
            || invalid_reference(&chain.branch.name)
            || chain.occurrences.is_empty()
        {
            return Err(ExplainError::InvalidObject);
        }
        if !spill_restore_pairs_seen.insert(chain.join_pair_identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let pairs = join_pair_identities
            .iter()
            .filter(|pair| pair.identity == chain.join_pair_identity)
            .collect::<Vec<_>>();
        if pairs.len() != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| join.source == pairs[0].right_source && join.predicate == pairs[0].predicate)
            .collect::<Vec<_>>();
        if matching_joins.len() != 1
            || matching_joins[0]
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.mutable_branch.as_ref())
                != Some(&chain.branch)
        {
            return Err(ExplainError::InvalidObject);
        }
        spill_restore_occurrence_count =
            spill_restore_occurrence_count.saturating_add(chain.occurrences.len());
        if checkpoint_step_count
            .saturating_add(segment_rotation_count)
            .saturating_add(stream_compaction_occurrence_count)
            .saturating_add(spill_restore_occurrence_count)
            > MAX_PLAN_NODES
        {
            return Err(ExplainError::TooManyNodes);
        }
        for occurrence in &chain.occurrences {
            if invalid_reference(occurrence.checkpoint_identity.as_str())
                || invalid_reference(occurrence.spill_identity.as_str())
                || invalid_reference(occurrence.source_stream_identity.as_str())
                || invalid_reference(occurrence.redo_fold_identity.as_str())
            {
                return Err(ExplainError::InvalidObject);
            }
        }
    }
    let mut stream_rotation_pairs_seen = BTreeSet::new();
    let mut stream_rotation_occurrence_count = 0usize;
    for chain in stream_rotation_chains {
        if invalid_reference(chain.join_pair_identity.as_str())
            || invalid_reference(&chain.branch.name)
            || chain.occurrences.is_empty()
        {
            return Err(ExplainError::InvalidObject);
        }
        if !stream_rotation_pairs_seen.insert(chain.join_pair_identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let pairs = join_pair_identities
            .iter()
            .filter(|pair| pair.identity == chain.join_pair_identity)
            .collect::<Vec<_>>();
        if pairs.len() != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| join.source == pairs[0].right_source && join.predicate == pairs[0].predicate)
            .collect::<Vec<_>>();
        if matching_joins.len() != 1
            || matching_joins[0]
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.mutable_branch.as_ref())
                != Some(&chain.branch)
        {
            return Err(ExplainError::InvalidObject);
        }
        stream_rotation_occurrence_count =
            stream_rotation_occurrence_count.saturating_add(chain.occurrences.len());
        if checkpoint_step_count
            .saturating_add(segment_rotation_count)
            .saturating_add(stream_compaction_occurrence_count)
            .saturating_add(spill_restore_occurrence_count)
            .saturating_add(stream_rotation_occurrence_count)
            > MAX_PLAN_NODES
        {
            return Err(ExplainError::TooManyNodes);
        }
        for occurrence in &chain.occurrences {
            if invalid_reference(occurrence.checkpoint_identity.as_str())
                || invalid_reference(occurrence.rotation_identity.as_str())
                || invalid_reference(occurrence.source_stream_identity.as_str())
                || occurrence
                    .left_stream_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
                || occurrence
                    .right_stream_identity
                    .as_ref()
                    .is_some_and(|identity| invalid_reference(identity.as_str()))
            {
                return Err(ExplainError::InvalidObject);
            }
        }
    }
    let mut wal_rotation_pairs_seen = BTreeSet::new();
    let mut wal_rotation_occurrence_count = 0usize;
    for chain in wal_rotation_chains {
        if invalid_reference(chain.join_pair_identity.as_str())
            || invalid_reference(&chain.branch.name)
            || chain.occurrences.is_empty()
        {
            return Err(ExplainError::InvalidObject);
        }
        if !wal_rotation_pairs_seen.insert(chain.join_pair_identity.as_str()) {
            return Err(ExplainError::InvalidObject);
        }
        let pairs = join_pair_identities
            .iter()
            .filter(|pair| pair.identity == chain.join_pair_identity)
            .collect::<Vec<_>>();
        if pairs.len() != 1 {
            return Err(ExplainError::InvalidObject);
        }
        let matching_joins = query
            .joins
            .iter()
            .filter(|join| join.source == pairs[0].right_source && join.predicate == pairs[0].predicate)
            .collect::<Vec<_>>();
        if matching_joins.len() != 1
            || matching_joins[0]
                .statistics
                .as_ref()
                .and_then(|statistics| statistics.mutable_branch.as_ref())
                != Some(&chain.branch)
        {
            return Err(ExplainError::InvalidObject);
        }
        wal_rotation_occurrence_count =
            wal_rotation_occurrence_count.saturating_add(chain.occurrences.len());
        if checkpoint_step_count
            .saturating_add(segment_rotation_count)
            .saturating_add(stream_compaction_occurrence_count)
            .saturating_add(spill_restore_occurrence_count)
            .saturating_add(stream_rotation_occurrence_count)
            .saturating_add(wal_rotation_occurrence_count)
            > MAX_PLAN_NODES
        {
            return Err(ExplainError::TooManyNodes);
        }
        for occurrence in &chain.occurrences {
            if invalid_reference(occurrence.checkpoint_identity.as_str())
                || invalid_reference(occurrence.left_redo_fold_identity.as_str())
                || invalid_reference(occurrence.right_redo_fold_identity.as_str())
                || invalid_reference(occurrence.left_write_ahead_identity.as_str())
                || invalid_reference(occurrence.right_write_ahead_identity.as_str())
                || invalid_reference(occurrence.left_segment_identity.as_str())
                || invalid_reference(occurrence.right_segment_identity.as_str())
            {
                return Err(ExplainError::InvalidObject);
            }
        }
    }
    for limit_pushdown in input_limit_pushdowns {
        let matching_pairs = join_pair_identities
            .iter()
            .filter(|pair| {
                pair.identity == limit_pushdown.join_pair_identity
                    && pair.right_source == limit_pushdown.source
            })
            .count();
        if matching_pairs != 1 {
            return Err(ExplainError::InvalidObject);
        }
    }
    let mut window_spill_identities_seen = BTreeSet::new();
    let mut window_spill_targets_seen = BTreeSet::new();
    for spill in window_spills {
        if invalid_reference(spill.identity.as_str())
            || invalid_reference(spill.join_pair_identity.as_str())
            || invalid_reference(spill.source.as_str())
            || invalid_reference(spill.window_aggregate_identity.as_str())
        {
            return Err(ExplainError::InvalidObject);
        }
        if !window_spill_identities_seen.insert(spill.identity.as_str())
            || !window_spill_targets_seen.insert((
                spill.join_pair_identity.as_str(),
                spill.window_aggregate_identity.as_str(),
            ))
        {
            return Err(ExplainError::InvalidObject);
        }
        let matching_pairs = join_pair_identities
            .iter()
            .filter(|pair| {
                pair.identity == spill.join_pair_identity && pair.right_source == spill.source
            })
            .count();
        let matching_aggregates = window_aggregates
            .iter()
            .filter(|aggregate| {
                aggregate.identity == spill.window_aggregate_identity
                    && aggregate.source == spill.source
            })
            .count();
        if matching_pairs != 1 || matching_aggregates != 1 {
            return Err(ExplainError::InvalidObject);
        }
    }

    let (storm_cascade_operators, storm_cascade_expressions, storm_cascade_predicates) =
        disjunct_storm_cascade_shape_counts(disjunct_storm_cascades)?;
    if disjunct_count == 0
        || conjunct_count == Some(0)
        || conjunct_count_per_disjunct == Some(0)
        || (!nested_branch_limits.is_empty() && conjunct_count_per_disjunct.is_none())
        || ((disjunct_count > 1
            || conjunct_count.is_some()
            || conjunct_count_per_disjunct.is_some())
            && query.predicate.is_none())
        || (!disjunct_storm_cascades.is_empty() && query.predicate.is_some())
        || (!limits_between_disjunct_and_conjunct.is_empty()
            && post_expansion_conjunct.is_none())
        || (post_expansion_conjunct.is_some() && conjunct_count.is_none())
        || (post_expansion_conjunct.is_none()
            && conjunct_count.is_some()
            && conjunct_count_per_disjunct.is_some())
    {
        return Err(ExplainError::InvalidExpression);
    }
    if invalid_reference(query.snapshot.as_str()) {
        return Err(ExplainError::InvalidSnapshot);
    }
    if invalid_reference(query.source.as_str()) {
        return Err(ExplainError::InvalidObject);
    }
    if query
        .joins
        .iter()
        .any(|join| invalid_reference(join.source.as_str()))
        || query
            .materialize_into
            .as_ref()
            .is_some_and(|target| invalid_reference(target.as_str()))
        || query
            .mutations
            .iter()
            .any(|mutation| invalid_reference(mutation.table.as_str()))
        || query
            .source_statistics
            .iter()
            .chain(query.joins.iter().filter_map(|join| join.statistics.as_ref()))
            .filter_map(|statistics| statistics.mutable_branch.as_ref())
            .any(|branch| invalid_reference(&branch.name))
    {
        return Err(ExplainError::InvalidObject);
    }
    if partial_indexes.len() > MAX_PLAN_NODES {
        return Err(ExplainError::TooManyNodes);
    }
    if partial_indexes
        .iter()
        .any(|candidate| {
            invalid_reference(candidate.table.as_str())
                || invalid_reference(candidate.index.as_str())
        })
    {
        return Err(ExplainError::InvalidObject);
    }
    let operator_bound = 1usize
        .saturating_add(query.joins.len().saturating_mul(2))
        .saturating_add(usize::from(query.predicate.is_some()))
        .saturating_add(disjunct_storms.len())
        .saturating_add(usize::from(!query.projections.is_empty()))
        .saturating_add(usize::from(query.distinct))
        .saturating_add(usize::from(!query.ordering.is_empty()))
        .saturating_add(usize::from(query.limit.is_some()))
        .saturating_add(nested_input_limits.len())
        .saturating_add(nested_branch_limits.len())
        .saturating_add(disjunct_storms.iter().fold(0usize, |total, storm| {
            total.saturating_add(storm.nested_branch_limits.len())
        }))
        .saturating_add(storm_cascade_operators)
        .saturating_add(limits_between_disjunct_and_conjunct.len())
        .saturating_add(usize::from(post_expansion_conjunct.is_some()))
        .saturating_add(additional_limits.len())
        .saturating_add(query.mutations.len())
        .saturating_add(window_aggregates.len())
        .saturating_add(input_limit_pushdowns.len())
        .saturating_add(usize::from(query.materialize_into.is_some()));
    if operator_bound > MAX_PLAN_NODES {
        return Err(ExplainError::TooManyNodes);
    }
    if query
        .predicate
        .iter()
        .chain(query.projections.iter())
        .chain(query.ordering.iter().map(|ordering| &ordering.expression))
        .chain(query.joins.iter().filter_map(|join| join.predicate.as_ref()))
        .chain(
            partial_indexes
                .iter()
                .map(|candidate| &candidate.partial_predicate),
        )
        .chain(post_expansion_conjunct.iter().copied())
        .chain(disjunct_storms.iter().map(|storm| &storm.predicate))
        .chain(storm_cascade_predicates.iter().copied())
        .chain(window_aggregates.iter().flat_map(|aggregate| {
            [&aggregate.aggregate, &aggregate.frame_identity]
        }))
        .chain(join_pair_identities.iter().filter_map(|pair| pair.predicate.as_ref()))
        .any(|expression| invalid_reference(expression.as_str()))
    {
        return Err(ExplainError::InvalidExpression);
    }
    if query
        .projections
        .len()
        .saturating_add(query.ordering.len())
        .saturating_add(window_aggregates.len().saturating_mul(2))
        .saturating_add(
            join_pair_identities
                .iter()
                .filter(|pair| pair.predicate.is_some())
                .count(),
        )
        .saturating_add(usize::from(query.predicate.is_some()))
        .saturating_add(usize::from(post_expansion_conjunct.is_some()))
        .saturating_add(query.joins.iter().filter(|join| join.predicate.is_some()).count())
        .saturating_add(disjunct_storms.iter().fold(0usize, |total, storm| {
            let stage = usize::try_from(
                storm
                    .disjunct_count
                    .saturating_mul(storm.conjunct_count_per_disjunct),
            )
            .unwrap_or(usize::MAX);
            total.saturating_add(stage)
        }))
        .saturating_add(storm_cascade_expressions)
        > MAX_PLAN_EXPRESSIONS
    {
        return Err(ExplainError::TooManyExpressions);
    }

    let mut operators = Vec::<Operator>::new();
    let mut current = push_scan(
        &mut operators,
        query.source.clone(),
        query.source_statistics.as_ref(),
    );
    let mut current_cardinality = source_cardinality(query.source_statistics.as_ref());
    current = push_window_aggregates(
        &mut operators,
        current,
        &query.source,
        current_cardinality,
        window_aggregates,
    );
    let mut join_cost_fold_identity = query_join_cost_fold_seed(
        &query.snapshot,
        &query.source,
        query.source_statistics.as_ref(),
        window_aggregates,
    );
    let mut decorrelated_pin_chain_identity: Option<String> = None;
    let mut paired_index_anchor_chain_identity: Option<String> = None;
    let mut paired_index_cost_restoration_chain_identity: Option<String> = None;
    let mut paired_index_cost_compaction_chain: Option<(String, u64, u64, u64)> = None;
    let mut paired_checkpoint_cost_restoration_fold: Option<
        QueryPairedCheckpointCostRestorationFold,
    > = None;
    let mut paired_segment_rotation_cost_restoration_fold: Option<
        QueryPairedSegmentRotationCostRestorationFold,
    > = None;
    let mut paired_stream_compaction_cost_restoration_fold: Option<
        QueryPairedStreamCompactionCostRestorationFold,
    > = None;
    let mut paired_checkpoint_spill_cost_restoration_fold: Option<
        QueryPairedCheckpointSpillCostRestorationFold,
    > = None;
    let mut paired_stream_rotation_cost_restoration_fold: Option<
        QueryPairedStreamRotationCostRestorationFold,
    > = None;
    let mut paired_wal_rotation_cost_restoration_fold: Option<
        QueryPairedWalRotationCostRestorationFold,
    > = None;
    let tracks_window_cost_restoration = !window_aggregates.is_empty()
        && (!checkpoint_compaction_chains.is_empty()
            || !segment_rotation_chains.is_empty()
            || !stream_compaction_chains.is_empty()
            || !spill_restore_chains.is_empty()
            || !stream_rotation_chains.is_empty()
            || !wal_rotation_chains.is_empty());
    let anchor_window_identity =
        query_window_pushdown_chain_identity(&query.source, window_aggregates);
    let mut paired_window_cost_restoration_fold = tracks_window_cost_restoration
        .then(|| {
            anchor_window_identity.as_deref().map(|identity| {
                query_paired_window_cost_restoration_seed(
                    &query.snapshot,
                    &join_cost_fold_identity,
                    &query.source,
                    identity,
                )
            })
        })
        .flatten();
    let mut paired_decorrelation_cost_restoration_chain_identity: Option<String> = None;
    let mut paired_window_spill_cascade_fold: Option<QueryWindowSpillCascadeFold> = None;
    let mut paired_window_spill_rule_cost_fold: Option<QueryPairedWindowSpillRuleCostFold> =
        None;
    let mut paired_aggregate_anchor_cascade_fold: Option<QueryPairedAggregateAnchorCascadeFold> =
        None;
    let mut paired_scoped_window_compaction_fold: Option<
        QueryPairedScopedWindowCompactionFold,
    > = None;
    let mut paired_limit_aggregate_restoration_fold:
        Option<QueryPairedLimitAggregateRestorationFold> = None;
    let mut paired_scoped_window_limit_restoration_fold: Option<
        QueryPairedScopedWindowLimitRestorationFold,
    > = None;
    let mut latest_paired_limit_scope_identity: Option<String> = None;
    let mut latest_paired_limit_bound_rows: Option<u64> = None;
    let mut paired_scoped_limit_window_restoration_fold: Option<
        QueryPairedScopedLimitWindowRestorationFold,
    > = None;
    let mut paired_bounded_limit_window_spill_fold: Option<
        QueryPairedBoundedLimitWindowSpillFold,
    > = None;
    let mut paired_limit_aggregate_spill_restoration_fold: Option<
        QueryPairedLimitAggregateSpillRestorationFold,
    > = None;
    let mut paired_aggregate_spill_restoration_fold: Option<
        QueryPairedAggregateSpillRestorationFold,
    > = None;
    let mut paired_window_compaction_spill_fold: Option<
        QueryPairedWindowCompactionSpillFold,
    > = None;
    let mut paired_limit_window_compaction_fold: Option<
        QueryPairedLimitWindowCompactionFold,
    > = None;
    let mut paired_join_limit_anchor_cascade_fold: Option<QueryJoinLimitAnchorCascadeFold> = None;
    let mut paired_limit_window_cascade_fold: Option<QueryLimitWindowCascadeFold> = None;
    add_join_cost_fold_seed_details(
        &mut operators[current].details,
        &join_cost_fold_identity,
    );
    if let Some(window_identity) = query_window_pushdown_chain_identity(
        &query.source,
        window_aggregates,
    ) {
        add_window_pushdown_chain_details(&mut operators[current].details, &window_identity);
    }
    for (planned_position, (declared_position, join)) in
        planned_query_join_order(&query.joins).into_iter().enumerate()
    {
        let decorrelated_subquery = declared_position
            .checked_sub(declared_join_count)
            .and_then(|offset| decorrelated_subqueries.get(offset));
        let selected_partial_index = partial_index_for_join(join, partial_indexes);
        let join_pair_identity = join_pair_identity_for_join(join, join_pair_identities);
        let checkpoint_compaction_chain = join_pair_identity.and_then(|pair| {
            checkpoint_compaction_chains
                .iter()
                .find(|chain| chain.join_pair_identity == pair.identity)
        });
        let segment_rotation_chain = join_pair_identity.and_then(|pair| {
            segment_rotation_chains
                .iter()
                .find(|chain| chain.join_pair_identity == pair.identity)
        });
        let stream_compaction_chain = join_pair_identity.and_then(|pair| {
            stream_compaction_chains
                .iter()
                .find(|chain| chain.join_pair_identity == pair.identity)
        });
        let spill_restore_chain = join_pair_identity.and_then(|pair| {
            spill_restore_chains
                .iter()
                .find(|chain| chain.join_pair_identity == pair.identity)
        });
        let stream_rotation_chain = join_pair_identity.and_then(|pair| {
            stream_rotation_chains
                .iter()
                .find(|chain| chain.join_pair_identity == pair.identity)
        });
        let wal_rotation_chain = join_pair_identity.and_then(|pair| {
            wal_rotation_chains
                .iter()
                .find(|chain| chain.join_pair_identity == pair.identity)
        });
        let paired_index_selection_id = join_pair_identity
            .map(|pair| paired_index_selection_identity(pair, selected_partial_index));
        let decorrelated_index_omission_identity = decorrelated_subquery
            .filter(|_| selected_partial_index.is_none())
            .map(decorrelated_index_selection_omission_identity);
        let decorrelated_index_fold_id = decorrelated_subquery.map(|subquery| {
            decorrelated_index_selection_fold_identity(
                subquery,
                selected_partial_index,
                decorrelated_index_omission_identity.as_deref(),
            )
        });
        let right_access = if let Some(candidate) = selected_partial_index {
            push_index_lookup(&mut operators, candidate, join.statistics.as_ref())
        } else {
            push_scan(&mut operators, join.source.clone(), join.statistics.as_ref())
        };
        let right_limit_operator_start = operators.len();
        let (right_after_limits, right_cardinality, right_limit_chain_identity) =
            push_query_limit_pushdowns(
                &mut operators,
                right_access,
                &join.source,
                source_cardinality(join.statistics.as_ref()),
                join_pair_identity,
                input_limit_pushdowns,
            );
        let right_limit_operator_end = operators.len();
        if let Some(identity) = right_limit_chain_identity.as_deref() {
            let mut limit_nodes = BTreeSet::from([right_access]);
            limit_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            for index in limit_nodes {
                add_query_limit_pushdown_chain_details(&mut operators[index].details, identity);
            }
        }
        let right_window_operator_start = operators.len();
        let right = push_window_aggregates(
            &mut operators,
            right_after_limits,
            &join.source,
            right_cardinality,
            window_aggregates,
        );
        let right_window_identity =
            query_window_pushdown_chain_identity(&join.source, window_aggregates);
        let right_window_spill_chain_identity = join_pair_identity.and_then(|pair| {
            query_window_spill_chain_identity(
                pair,
                &join.source,
                window_aggregates,
                window_spills,
            )
        });
        if let Some(spill_chain_identity) = right_window_spill_chain_identity.as_deref() {
            let right_window_operator_end = operators.len();
            apply_query_window_spills(
                &mut operators,
                right_window_operator_start..right_window_operator_end,
                join_pair_identity.expect("spill descriptors require a resolved join pair"),
                spill_chain_identity,
                &join.source,
                window_spills,
            );
            let mut spill_nodes = BTreeSet::from([right_access]);
            spill_nodes.extend(right_window_operator_start..right_window_operator_end);
            for index in spill_nodes {
                add_window_spill_chain_details(
                    &mut operators[index].details,
                    spill_chain_identity,
                );
            }
        }
        if let Some(window_identity) = right_window_identity.as_deref() {
            for index in BTreeSet::from([right_access, right]) {
                add_window_pushdown_chain_details(&mut operators[index].details, window_identity);
            }
        }
        if let Some(subquery) = decorrelated_subquery {
            for index in BTreeSet::from([right_access, right]) {
                add_decorrelated_subquery_details(&mut operators[index].details, subquery);
            }
        }
        if let Some(identity) = decorrelated_index_omission_identity.as_deref() {
            for index in BTreeSet::from([right_access, right]) {
                add_decorrelated_index_selection_omission_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let Some(pair_identity) = join_pair_identity {
            let mut pair_nodes = BTreeSet::from([right_access, right]);
            pair_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            for index in pair_nodes {
                add_join_pair_identity_details(&mut operators[index].details, pair_identity);
            }
        }
        if let Some(identity) = paired_index_selection_id.as_deref() {
            let mut index_nodes = BTreeSet::from([right_access, right]);
            index_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            for index in index_nodes {
                add_paired_index_selection_details(&mut operators[index].details, identity);
            }
        }
        let paired_predicate_pushdown_identity = join_pair_identity
            .zip(selected_partial_index)
            .map(|(pair, index)| paired_predicate_pushdown_identity(pair, index));
        if let Some(identity) = paired_predicate_pushdown_identity.as_deref() {
            let mut predicate_nodes = BTreeSet::from([right_access, right]);
            predicate_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            for index in predicate_nodes {
                add_paired_predicate_pushdown_details(&mut operators[index].details, identity);
            }
        }
        let cardinality = join_cardinality(
            current_cardinality,
            right_cardinality,
            join.predicate.is_some(),
        );
        // This adapter has no selectivity histogram: predicate joins use a
        // documented 10% selectivity heuristic, while predicate-free joins
        // retain the cross-product estimate. Neither choice changes execution.
        let work = current_cardinality
            .rows
            .zip(right_cardinality.rows)
            .and_then(|(left, right)| left.checked_add(right));
        let work_overflow = current_cardinality
            .rows
            .zip(right_cardinality.rows)
            .is_some_and(|(left, right)| left.checked_add(right).is_none());
        let left_fold_identity = join_cost_fold_identity.clone();
        let decorrelated_predicate_identity = decorrelated_subquery
            .zip(selected_partial_index)
            .map(|(subquery, index)| decorrelated_predicate_pushdown_identity(subquery, index));
        let right_fold_identity = query_join_cost_input_identity(
            &query.snapshot,
            join,
            selected_partial_index,
            window_aggregates,
            decorrelated_subquery,
            decorrelated_predicate_identity.as_deref(),
            decorrelated_index_omission_identity.as_deref(),
            right_limit_chain_identity.as_deref(),
            right_window_spill_chain_identity.as_deref(),
        );
        let paired_index_refold_id = join_pair_identity
            .zip(paired_index_selection_id.as_deref())
            .map(|(pair, selection_identity)| {
                query_paired_index_refold_identity(
                    &left_fold_identity,
                    &right_fold_identity,
                    pair,
                    selection_identity,
                    paired_predicate_pushdown_identity.as_deref(),
                    cardinality,
                    work,
                    work_overflow,
                )
            });
        let paired_index_anchor_chain_advance = join_pair_identity
            .zip(selected_partial_index)
            .zip(paired_index_refold_id.as_deref())
            .zip(paired_index_selection_id.as_deref())
            .map(|(((pair, _index), refold_identity), selection_identity)| {
                let parent_chain_identity = paired_index_anchor_chain_identity
                    .clone()
                    .unwrap_or_else(|| {
                        query_paired_index_anchor_chain_seed_identity(&left_fold_identity)
                    });
                let identity = query_paired_index_anchor_chain_identity(
                    &parent_chain_identity,
                    &left_fold_identity,
                    pair,
                    selection_identity,
                    refold_identity,
                );
                (
                    parent_chain_identity,
                    identity,
                    refold_identity.to_owned(),
                    selection_identity.to_owned(),
                )
            });
        let next_paired_index_anchor_chain_identity = paired_index_anchor_chain_advance
            .as_ref()
            .map(|(_, identity, _, _)| identity.clone())
            .or_else(|| paired_index_anchor_chain_identity.clone());
        let paired_index_anchor_chain_parent_identity = paired_index_anchor_chain_advance
            .as_ref()
            .map(|(parent_identity, _, _, _)| parent_identity.as_str())
            .or(paired_index_anchor_chain_identity.as_deref());
        let paired_index_anchor_chain_transition = if paired_index_anchor_chain_advance.is_some() {
            Some("append_exact_index_refold")
        } else if next_paired_index_anchor_chain_identity.is_some() {
            Some("carry_through_sparse_cost_fold")
        } else {
            None
        };
        let paired_index_anchor_chain_step_refold_identity = paired_index_anchor_chain_advance
            .as_ref()
            .map(|(_, _, identity, _)| identity.as_str());
        let paired_index_anchor_chain_step_selection_identity = paired_index_anchor_chain_advance
            .as_ref()
            .map(|(_, _, _, identity)| identity.as_str());
        // Every logical pair contributes its exact index-or-scan outcome to a
        // separate restoration chain. This keeps a no-index pair observable
        // between indexed pairs, and lets the optimized fold be mapped back to
        // declared positions without losing sparse pair slots.
        let paired_index_cost_restoration_advance = join_pair_identity
            .zip(paired_index_selection_id.as_deref())
            .zip(paired_index_refold_id.as_deref())
            .map(|((pair, selection_identity), refold_identity)| {
                let parent_chain_identity = paired_index_cost_restoration_chain_identity
                    .clone()
                    .unwrap_or_else(|| {
                        query_paired_index_cost_restoration_seed_identity(&left_fold_identity)
                    });
                let identity = query_paired_index_cost_restoration_chain_identity(
                    &parent_chain_identity,
                    &left_fold_identity,
                    pair,
                    selection_identity,
                    refold_identity,
                );
                (
                    parent_chain_identity,
                    identity,
                    pair.identity.as_str(),
                    refold_identity,
                    selection_identity,
                    selected_partial_index.is_some(),
                )
            });
        let next_paired_index_cost_restoration_chain_identity =
            paired_index_cost_restoration_advance
                .as_ref()
                .map(|(_, identity, _, _, _, _)| identity.clone())
                .or_else(|| paired_index_cost_restoration_chain_identity.clone());
        let paired_index_cost_restoration_parent_identity =
            paired_index_cost_restoration_advance
                .as_ref()
                .map(|(parent_identity, _, _, _, _, _)| parent_identity.as_str())
                .or(paired_index_cost_restoration_chain_identity.as_deref());
        let paired_index_cost_restoration_transition =
            if paired_index_cost_restoration_advance.is_some() {
                Some(if selected_partial_index.is_some() {
                    "append_exact_index_outcome"
                } else {
                    "append_scan_outcome"
                })
            } else if next_paired_index_cost_restoration_chain_identity.is_some() {
                Some("carry_through_unpaired_cost_fold")
            } else {
                None
            };
        // ORNA does not define compact index-route explain metadata. Compact
        // pair outcomes to a bounded digest and counts; a sparse unpaired
        // fold carries that summary without consuming a route slot.
        let paired_index_cost_compaction_advance = join_pair_identity
            .zip(paired_index_selection_id.as_deref())
            .zip(paired_index_refold_id.as_deref())
            .map(|((pair, selection_identity), refold_identity)| {
                let (
                    parent_chain_identity,
                    previous_pair_count,
                    previous_index_count,
                    previous_scan_count,
                ) = paired_index_cost_compaction_chain.as_ref().map_or_else(
                        || {
                            (
                                query_paired_index_cost_compaction_seed_identity(
                                    &left_fold_identity,
                                ),
                                0,
                                0,
                                0,
                            )
                        },
                        |(identity, pair_count, index_count, scan_count)| {
                            (
                                identity.clone(),
                                *pair_count,
                                *index_count,
                                *scan_count,
                            )
                        },
                    );
                let has_index = selected_partial_index.is_some();
                let identity = query_paired_index_cost_compaction_chain_identity(
                    &parent_chain_identity,
                    &left_fold_identity,
                    pair,
                    selection_identity,
                    refold_identity,
                    has_index,
                );
                (
                    parent_chain_identity,
                    identity,
                    previous_pair_count.saturating_add(1),
                    previous_index_count.saturating_add(u64::from(has_index)),
                    previous_scan_count.saturating_add(u64::from(!has_index)),
                )
            });
        let next_paired_index_cost_compaction_chain = paired_index_cost_compaction_advance
            .as_ref()
            .map(|(_, identity, pair_count, index_count, scan_count)| {
                (identity.clone(), *pair_count, *index_count, *scan_count)
            })
            .or_else(|| paired_index_cost_compaction_chain.clone());
        let paired_index_cost_compaction_parent_identity =
            paired_index_cost_compaction_advance
                .as_ref()
                .map(|(parent_identity, _, _, _, _)| parent_identity.as_str())
                .or_else(|| {
                    paired_index_cost_compaction_chain
                        .as_ref()
                        .map(|(identity, _, _, _)| identity.as_str())
                });
        let paired_index_cost_compaction_transition =
            if paired_index_cost_compaction_advance.is_some() {
                Some(if selected_partial_index.is_some() {
                    "append_exact_index_outcome"
                } else {
                    "append_scan_outcome"
                })
            } else if next_paired_index_cost_compaction_chain.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_index_cost_compaction_counts = paired_index_cost_compaction_advance
            .as_ref()
            .map(|(_, _, pair_count, index_count, scan_count)| {
                (*pair_count, *index_count, *scan_count)
            })
            .or_else(|| {
                next_paired_index_cost_compaction_chain
                    .as_ref()
                    .map(|(_, pair_count, index_count, scan_count)| {
                        (*pair_count, *index_count, *scan_count)
                    })
            });
        let decorrelated_anchor_fold_id = decorrelated_subquery.map(|subquery| {
            query_decorrelated_anchor_fold_identity(
                &left_fold_identity,
                &right_fold_identity,
                subquery,
                decorrelated_predicate_identity.as_deref(),
                decorrelated_index_fold_id.as_deref(),
            )
        });
        let decorrelated_omission_refold_id = decorrelated_subquery
            .zip(decorrelated_index_omission_identity.as_deref())
            .zip(decorrelated_index_fold_id.as_deref())
            .map(|((subquery, omission_identity), selection_identity)| {
                query_decorrelated_omission_refold_identity(
                    &left_fold_identity,
                    &right_fold_identity,
                    subquery,
                    omission_identity,
                    selection_identity,
                    cardinality,
                    work,
                    work_overflow,
                )
            });
        let decorrelated_pin_chain_advance = decorrelated_subquery
            .zip(decorrelated_index_fold_id.as_deref())
            .zip(decorrelated_anchor_fold_id.as_deref())
            .map(|((subquery, selection_identity), anchor_fold_identity)| {
                let parent_pin_identity = decorrelated_pin_chain_identity
                    .clone()
                    .unwrap_or_else(|| {
                        query_decorrelated_pin_chain_seed_identity(
                            &query.snapshot,
                            &left_fold_identity,
                        )
                    });
                let identity = query_decorrelated_pin_chain_identity(
                    &parent_pin_identity,
                    &query.snapshot,
                    &left_fold_identity,
                    &right_fold_identity,
                    subquery,
                    selection_identity,
                    anchor_fold_identity,
                    decorrelated_omission_refold_id.as_deref(),
                    cardinality,
                    work,
                    work_overflow,
                );
                (
                    parent_pin_identity,
                    identity,
                    subquery.identity.as_str().to_owned(),
                    selection_identity.to_owned(),
                    anchor_fold_identity.to_owned(),
                    decorrelated_omission_refold_id.clone(),
                )
            });
        let next_decorrelated_pin_chain_identity = decorrelated_pin_chain_advance
            .as_ref()
            .map(|(_, identity, _, _, _, _)| identity.clone())
            .or_else(|| decorrelated_pin_chain_identity.clone());
        let decorrelated_pin_chain_parent_identity = decorrelated_pin_chain_advance
            .as_ref()
            .map(|(parent_identity, _, _, _, _, _)| parent_identity.as_str())
            .or(decorrelated_pin_chain_identity.as_deref());
        let decorrelated_pin_chain_transition = if decorrelated_pin_chain_advance.is_some() {
            Some("append_resolved_subquery_pin")
        } else if next_decorrelated_pin_chain_identity.is_some() {
            Some("carry_through_sparse_cost_fold")
        } else {
            None
        };
        let decorrelated_pin_chain_step_subquery_identity = decorrelated_pin_chain_advance
            .as_ref()
            .map(|(_, _, identity, _, _, _)| identity.as_str());
        let decorrelated_pin_chain_step_selection_identity = decorrelated_pin_chain_advance
            .as_ref()
            .map(|(_, _, _, identity, _, _)| identity.as_str());
        let decorrelated_pin_chain_step_anchor_fold_identity = decorrelated_pin_chain_advance
            .as_ref()
            .map(|(_, _, _, _, identity, _)| identity.as_str());
        let decorrelated_pin_chain_step_omission_refold_identity = decorrelated_pin_chain_advance
            .as_ref()
            .and_then(|(_, _, _, _, _, identity)| identity.as_deref());
        let paired_decorrelation_cost_restoration_advance =
            if paired_index_cost_restoration_advance.is_some()
                || decorrelated_pin_chain_advance.is_some()
            {
                let parent_identity = paired_decorrelation_cost_restoration_chain_identity
                    .clone()
                    .unwrap_or_else(|| {
                        query_paired_decorrelation_cost_restoration_seed_identity(
                            &left_fold_identity,
                        )
                    });
                let identity = query_paired_decorrelation_cost_restoration_chain_identity(
                    &parent_identity,
                    &left_fold_identity,
                    join_pair_identity,
                    paired_index_selection_id.as_deref(),
                    paired_index_refold_id.as_deref(),
                    decorrelated_subquery,
                    next_decorrelated_pin_chain_identity.as_deref(),
                    decorrelated_anchor_fold_id.as_deref(),
                    decorrelated_omission_refold_id.as_deref(),
                );
                Some((parent_identity, identity))
            } else {
                None
            };
        let next_paired_decorrelation_cost_restoration_chain_identity =
            paired_decorrelation_cost_restoration_advance
                .as_ref()
                .map(|(_, identity)| identity.clone())
                .or_else(|| paired_decorrelation_cost_restoration_chain_identity.clone());
        let paired_decorrelation_cost_restoration_parent_identity =
            paired_decorrelation_cost_restoration_advance
                .as_ref()
                .map(|(parent_identity, _)| parent_identity.as_str())
                .or(paired_decorrelation_cost_restoration_chain_identity.as_deref());
        let paired_decorrelation_cost_restoration_transition =
            match (
                paired_index_cost_restoration_advance.is_some(),
                decorrelated_pin_chain_advance.is_some(),
            ) {
                (true, true) => Some("append_paired_route_and_decorrelation"),
                (true, false) => Some("append_paired_route"),
                (false, true) => Some("append_decorrelation"),
                (false, false)
                    if next_paired_decorrelation_cost_restoration_chain_identity.is_some() =>
                {
                    Some("carry_through_sparse_cost_fold")
                }
                (false, false) => None,
            };
        let window_anchor_fold_id = right_window_identity.as_deref().map(|chain_identity| {
            query_window_anchor_fold_identity(
                &left_fold_identity,
                &right_fold_identity,
                chain_identity,
            )
        });
        let join_pair_anchor_fold_id = join_pair_identity.map(|pair| {
            query_join_pair_anchor_fold_identity(
                &left_fold_identity,
                &right_fold_identity,
                pair,
            )
        });
        let paired_limit_pushdown_anchor_fold_id = join_pair_identity
            .zip(right_limit_chain_identity.as_deref())
            .map(|(pair, chain_identity)| {
                query_paired_limit_pushdown_anchor_fold_identity(
                    &left_fold_identity,
                    &right_fold_identity,
                    pair,
                    chain_identity,
                )
            });
        if let Some(identity) = paired_limit_pushdown_anchor_fold_id.as_deref() {
            latest_paired_limit_scope_identity = Some(identity.to_owned());
            latest_paired_limit_bound_rows = join_pair_identity.and_then(|pair| {
                query_limit_pushdown_bound_rows(pair, &join.source, input_limit_pushdowns)
            });
        }
        let paired_join_limit_anchor_fold_id = join_pair_identity
            .zip(join_pair_anchor_fold_id.as_deref())
            .zip(paired_limit_pushdown_anchor_fold_id.as_deref())
            .map(|((pair, join_anchor_identity), limit_anchor_identity)| {
                query_paired_join_limit_anchor_fold_identity(
                    pair,
                    join_anchor_identity,
                    limit_anchor_identity,
                )
            });
        if let Some(identity) = paired_join_limit_anchor_fold_id.as_deref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_join_limit_anchor_fold_details(
                    &mut operators[index].details,
                    identity,
                );
            }
            let limit_stage_count =
                u64::try_from(right_limit_operator_end - right_limit_operator_start)
                    .unwrap_or(u64::MAX);
            paired_join_limit_anchor_cascade_fold =
                Some(query_paired_join_limit_anchor_cascade_fold(
                    paired_join_limit_anchor_cascade_fold.as_ref(),
                    identity,
                    limit_stage_count,
                ));
        }
        if let Some(cascade_fold) = paired_join_limit_anchor_cascade_fold.as_ref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_join_limit_anchor_cascade_fold_details(
                    &mut operators[index].details,
                    cascade_fold,
                );
            }
        }
        let paired_limit_window_anchor_fold_id = join_pair_identity
            .zip(paired_limit_pushdown_anchor_fold_id.as_deref())
            .zip(right_window_identity.as_deref())
            .map(|((pair, limit_anchor_fold_identity), window_chain_identity)| {
                query_paired_limit_window_anchor_fold_identity(
                    pair,
                    limit_anchor_fold_identity,
                    window_chain_identity,
                )
            });
        if let Some(identity) = paired_limit_window_anchor_fold_id.as_deref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_window_anchor_fold_details(
                    &mut operators[index].details,
                    identity,
                );
            }
            let limit_stage_count =
                u64::try_from(right_limit_operator_end - right_limit_operator_start)
                    .unwrap_or(u64::MAX);
            paired_limit_window_cascade_fold = Some(query_paired_limit_window_cascade_fold(
                paired_limit_window_cascade_fold.as_ref(),
                identity,
                limit_stage_count,
                right_cardinality,
            ));
        }
        if let Some(cascade_fold) = paired_limit_window_cascade_fold.as_ref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_window_cascade_fold_details(
                    &mut operators[index].details,
                    cascade_fold,
                );
            }
        }
        if let Some(identity) = paired_limit_pushdown_anchor_fold_id.as_deref() {
            let mut limit_nodes = BTreeSet::from([right_access, right]);
            limit_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            limit_nodes.extend(right_window_operator_start..operators.len());
            for index in limit_nodes {
                add_paired_limit_pushdown_anchor_fold_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        let paired_aggregate_pushdown_anchor_fold_id = join_pair_identity
            .zip(right_window_identity.as_deref())
            .map(|(pair, chain_identity)| {
                query_paired_aggregate_pushdown_anchor_fold_identity(
                    &left_fold_identity,
                    &right_fold_identity,
                    pair,
                    chain_identity,
                )
            });
        if let Some(identity) = paired_aggregate_pushdown_anchor_fold_id.as_deref() {
            let mut aggregate_nodes = BTreeSet::from([right_access, right]);
            aggregate_nodes.extend(right_window_operator_start..operators.len());
            for index in aggregate_nodes {
                add_paired_aggregate_cascade_pair_details(&mut operators[index].details, identity);
            }
            let aggregate_stage_count =
                u64::try_from(operators.len() - right_window_operator_start).unwrap_or(u64::MAX);
            paired_aggregate_anchor_cascade_fold =
                Some(query_paired_aggregate_anchor_cascade_fold(
                    paired_aggregate_anchor_cascade_fold.as_ref(),
                    identity,
                    aggregate_stage_count,
                ));
        }
        if let Some(cascade_fold) = paired_aggregate_anchor_cascade_fold.as_ref() {
            let mut aggregate_nodes = BTreeSet::from([right_access, right]);
            aggregate_nodes.extend(right_window_operator_start..operators.len());
            for index in aggregate_nodes {
                add_paired_aggregate_cascade_fold_details(
                    &mut operators[index].details,
                    cascade_fold,
                );
            }
        }
        let paired_scoped_window_compaction_pair_identity = join_pair_identity
            .zip(window_anchor_fold_id.as_deref())
            .zip(paired_aggregate_anchor_cascade_fold.as_ref())
            .map(|((pair, window_scope_identity), aggregate_fold)| {
                query_paired_scoped_window_compaction_pair_identity(
                    pair,
                    window_scope_identity,
                    &aggregate_fold.identity,
                )
            });
        if let (Some(pair_identity), Some(window_scope_identity), Some(aggregate_fold)) = (
            paired_scoped_window_compaction_pair_identity.as_deref(),
            window_anchor_fold_id.as_deref(),
            paired_aggregate_anchor_cascade_fold.as_ref(),
        ) {
            let window_stage_count =
                u64::try_from(operators.len() - right_window_operator_start).unwrap_or(u64::MAX);
            paired_scoped_window_compaction_fold =
                Some(query_paired_scoped_window_compaction_fold(
                    paired_scoped_window_compaction_fold.as_ref(),
                    pair_identity,
                    window_scope_identity,
                    window_stage_count,
                    aggregate_fold,
                ));
        }
        if let Some(identity) = paired_scoped_window_compaction_pair_identity.as_deref() {
            let mut scope_nodes = BTreeSet::from([right_access, right]);
            scope_nodes.extend(right_window_operator_start..operators.len());
            for index in scope_nodes {
                add_paired_scoped_window_compaction_pair_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let Some(fold) = paired_scoped_window_compaction_fold.as_ref() {
            let mut scope_nodes = BTreeSet::from([right_access, right]);
            scope_nodes.extend(right_window_operator_start..operators.len());
            for index in scope_nodes {
                add_paired_scoped_window_compaction_fold_details(
                    &mut operators[index].details,
                    fold,
                    paired_scoped_window_compaction_pair_identity.is_some(),
                );
            }
        }
        let paired_limit_aggregate_restoration_pair_identity =
            query_paired_limit_aggregate_restoration_pair_identity(
                paired_limit_pushdown_anchor_fold_id.as_deref(),
                paired_aggregate_pushdown_anchor_fold_id.as_deref(),
            );
        if let Some(pair_identity) =
            paired_limit_aggregate_restoration_pair_identity.as_deref()
        {
            paired_limit_aggregate_restoration_fold = Some(
                query_paired_limit_aggregate_restoration_fold(
                    paired_limit_aggregate_restoration_fold.as_ref(),
                    pair_identity,
                    paired_join_limit_anchor_cascade_fold.as_ref(),
                    paired_aggregate_anchor_cascade_fold.as_ref(),
                ),
            );
        }
        if let Some(identity) = paired_limit_aggregate_restoration_pair_identity.as_deref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_aggregate_restoration_pair_details(
                    &mut operators[index].details,
                    identity,
                    paired_limit_pushdown_anchor_fold_id.as_deref(),
                    paired_aggregate_pushdown_anchor_fold_id.as_deref(),
                );
            }
        }
        if let Some(fold) = paired_limit_aggregate_restoration_fold.as_ref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_aggregate_restoration_fold_details(
                    &mut operators[index].details,
                    fold,
                );
            }
        }
        let paired_scoped_window_limit_restoration_pair_identity =
            paired_scoped_window_compaction_fold
                .as_ref()
                .zip(paired_limit_aggregate_restoration_fold.as_ref())
                .map(|(window_fold, limit_fold)| {
                    query_paired_scoped_window_limit_restoration_pair_identity(
                        window_fold, limit_fold,
                    )
                });
        let paired_scoped_window_limit_restoration_transition =
            paired_scoped_window_compaction_fold
                .as_ref()
                .zip(paired_limit_aggregate_restoration_fold.as_ref())
                .map(|(window_fold, limit_fold)| {
                    query_paired_scoped_window_limit_restoration_transition(
                        paired_scoped_window_limit_restoration_fold.as_ref(),
                        window_fold,
                        limit_fold,
                    )
                });
        if let (Some(pair_identity), Some(window_fold), Some(limit_fold)) = (
            paired_scoped_window_limit_restoration_pair_identity.as_deref(),
            paired_scoped_window_compaction_fold.as_ref(),
            paired_limit_aggregate_restoration_fold.as_ref(),
        ) {
            paired_scoped_window_limit_restoration_fold =
                Some(query_paired_scoped_window_limit_restoration_fold(
                    paired_scoped_window_limit_restoration_fold.as_ref(),
                    pair_identity,
                    window_fold,
                    limit_fold,
                ));
        }
        if let (Some(fold), Some(transition)) = (
            paired_scoped_window_limit_restoration_fold.as_ref(),
            paired_scoped_window_limit_restoration_transition,
        ) {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_scoped_window_limit_restoration_fold_details(
                    &mut operators[index].details,
                    fold,
                    transition,
                );
            }
        }
        let paired_window_spill_anchor_fold_id = join_pair_identity
            .zip(right_window_identity.as_deref())
            .zip(right_window_spill_chain_identity.as_deref())
            .map(|((pair, window_chain_identity), spill_chain_identity)| {
                query_paired_window_spill_anchor_fold_identity(
                    &left_fold_identity,
                    &right_fold_identity,
                    pair,
                    window_chain_identity,
                    spill_chain_identity,
                )
            });
        let paired_aggregate_spill_fold = join_pair_identity
            .zip(right_window_identity.as_deref())
            .zip(right_window_spill_chain_identity.as_deref())
            .and_then(|((pair, aggregate_chain_identity), spill_chain_identity)| {
                query_aggregate_spill_totals(
                    pair,
                    &join.source,
                    window_aggregates,
                    window_spills,
                )
                .map(|totals| {
                    let identity = query_paired_aggregate_spill_anchor_fold_identity(
                        &left_fold_identity,
                        &right_fold_identity,
                        pair,
                        aggregate_chain_identity,
                        spill_chain_identity,
                        totals,
                    );
                    (identity, totals)
                })
            });
        if let Some((pair_spill_fold_identity, totals)) = paired_aggregate_spill_fold.as_ref() {
            paired_window_spill_cascade_fold = Some(
                query_paired_window_spill_cascade_fold(
                    paired_window_spill_cascade_fold.as_ref(),
                    pair_spill_fold_identity,
                    *totals,
                ),
            );
            if let Some(pair) = join_pair_identity {
                let cascade = paired_window_spill_cascade_fold
                    .as_ref()
                    .expect("a spill pair advanced the cumulative cascade");
                paired_window_spill_rule_cost_fold = Some(
                    query_paired_window_spill_rule_cost_fold(
                        paired_window_spill_rule_cost_fold.as_ref(),
                        pair,
                        pair_spill_fold_identity,
                        window_aggregates,
                        window_spills,
                        cascade,
                    ),
                );
            }
        }
        let paired_bounded_limit_window_spill_pair_identity = latest_paired_limit_scope_identity
            .as_deref()
            .zip(latest_paired_limit_bound_rows)
            .zip(paired_join_limit_anchor_cascade_fold.as_ref())
            .zip(paired_window_spill_cascade_fold.as_ref())
            .map(
                |(((limit_identity, bounded_rows), limit_fold), spill_fold)| {
                    query_paired_bounded_limit_window_spill_pair_identity(
                        limit_identity,
                        bounded_rows,
                        limit_fold,
                        spill_fold,
                    )
                },
            );
        let paired_bounded_limit_window_spill_transition = latest_paired_limit_scope_identity
            .as_deref()
            .zip(latest_paired_limit_bound_rows)
            .zip(paired_join_limit_anchor_cascade_fold.as_ref())
            .zip(paired_window_spill_cascade_fold.as_ref())
            .map(
                |(((limit_identity, bounded_rows), limit_fold), spill_fold)| {
                    query_paired_bounded_limit_window_spill_transition(
                        paired_bounded_limit_window_spill_fold.as_ref(),
                        limit_identity,
                        bounded_rows,
                        limit_fold,
                        spill_fold,
                    )
                },
            );
        if let (
            Some(pair_identity),
            Some(limit_identity),
            Some(bounded_rows),
            Some(limit_fold),
            Some(spill_fold),
        ) = (
            paired_bounded_limit_window_spill_pair_identity.as_deref(),
            latest_paired_limit_scope_identity.as_deref(),
            latest_paired_limit_bound_rows,
            paired_join_limit_anchor_cascade_fold.as_ref(),
            paired_window_spill_cascade_fold.as_ref(),
        ) {
            paired_bounded_limit_window_spill_fold =
                Some(query_paired_bounded_limit_window_spill_fold(
                    paired_bounded_limit_window_spill_fold.as_ref(),
                    pair_identity,
                    limit_identity,
                    bounded_rows,
                    limit_fold,
                    spill_fold,
                ));
        }
        let paired_window_compaction_spill_pair_identity = join_pair_identity
            .zip(paired_aggregate_pushdown_anchor_fold_id.as_deref())
            .zip(right_window_identity.as_deref())
            .map(|((pair, window_anchor_fold_identity), window_chain_identity)| {
                query_paired_window_compaction_spill_pair_identity(
                    pair,
                    window_anchor_fold_identity,
                    window_chain_identity,
                    right_window_spill_chain_identity.as_deref(),
                )
            });
        if let (Some(pair_identity), Some(window_fold)) = (
            paired_window_compaction_spill_pair_identity.as_deref(),
            paired_aggregate_anchor_cascade_fold.as_ref(),
        ) {
            paired_window_compaction_spill_fold = Some(
                query_paired_window_compaction_spill_fold(
                    paired_window_compaction_spill_fold.as_ref(),
                    pair_identity,
                    window_fold,
                    paired_window_spill_cascade_fold.as_ref(),
                ),
            );
        }
        let paired_scoped_limit_window_restoration_pair_identity =
            latest_paired_limit_scope_identity
                .as_deref()
                .zip(paired_join_limit_anchor_cascade_fold.as_ref())
                .zip(paired_window_compaction_spill_fold.as_ref())
                .map(|((limit_scope_identity, limit_fold), window_fold)| {
                    query_paired_scoped_limit_window_restoration_pair_identity(
                        limit_scope_identity,
                        limit_fold,
                        window_fold,
                    )
                });
        let paired_scoped_limit_window_restoration_transition =
            latest_paired_limit_scope_identity
                .as_deref()
                .zip(paired_join_limit_anchor_cascade_fold.as_ref())
                .zip(paired_window_compaction_spill_fold.as_ref())
                .map(|((limit_scope_identity, limit_fold), window_fold)| {
                    query_paired_scoped_limit_window_restoration_transition(
                        paired_scoped_limit_window_restoration_fold.as_ref(),
                        limit_scope_identity,
                        limit_fold,
                        window_fold,
                    )
                });
        if let (
            Some(pair_identity),
            Some(limit_scope_identity),
            Some(limit_fold),
            Some(window_fold),
        ) = (
            paired_scoped_limit_window_restoration_pair_identity.as_deref(),
            latest_paired_limit_scope_identity.as_deref(),
            paired_join_limit_anchor_cascade_fold.as_ref(),
            paired_window_compaction_spill_fold.as_ref(),
        ) {
            paired_scoped_limit_window_restoration_fold =
                Some(query_paired_scoped_limit_window_restoration_fold(
                    paired_scoped_limit_window_restoration_fold.as_ref(),
                    pair_identity,
                    limit_scope_identity,
                    limit_fold,
                    window_fold,
                ));
        }
        if let (Some(fold), Some(transition)) = (
            paired_scoped_limit_window_restoration_fold.as_ref(),
            paired_scoped_limit_window_restoration_transition,
        ) {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_scoped_limit_window_restoration_fold_details(
                    &mut operators[index].details,
                    fold,
                    transition,
                );
            }
        }
        let paired_limit_window_compaction_pair_identity =
            query_paired_limit_window_compaction_pair_identity(
                paired_limit_aggregate_restoration_pair_identity.as_deref(),
                paired_window_compaction_spill_pair_identity.as_deref(),
            );
        if let Some(pair_identity) = paired_limit_window_compaction_pair_identity.as_deref() {
            paired_limit_window_compaction_fold = Some(
                query_paired_limit_window_compaction_fold(
                    paired_limit_window_compaction_fold.as_ref(),
                    pair_identity,
                    paired_limit_aggregate_restoration_fold.as_ref(),
                    paired_window_compaction_spill_fold.as_ref(),
                ),
            );
        }
        let paired_aggregate_spill_restoration_pair_identity =
            query_paired_aggregate_spill_restoration_pair_identity(
                paired_aggregate_pushdown_anchor_fold_id.as_deref(),
                paired_aggregate_spill_fold
                    .as_ref()
                    .map(|(identity, _)| identity.as_str()),
            );
        if let (Some(pair_identity), Some(aggregate_fold)) = (
            paired_aggregate_spill_restoration_pair_identity.as_deref(),
            paired_aggregate_anchor_cascade_fold.as_ref(),
        ) {
            paired_aggregate_spill_restoration_fold = Some(
                query_paired_aggregate_spill_restoration_fold(
                    paired_aggregate_spill_restoration_fold.as_ref(),
                    pair_identity,
                    aggregate_fold,
                    paired_window_spill_cascade_fold.as_ref(),
                ),
            );
        }
        let paired_limit_aggregate_spill_restoration_pair_identity =
            paired_join_limit_anchor_cascade_fold
                .as_ref()
                .zip(paired_aggregate_spill_restoration_fold.as_ref())
                .map(|(limit_fold, aggregate_fold)| {
                    query_paired_limit_aggregate_spill_restoration_pair_identity(
                        limit_fold,
                        aggregate_fold,
                    )
                });
        let paired_limit_aggregate_spill_restoration_transition =
            paired_join_limit_anchor_cascade_fold
                .as_ref()
                .zip(paired_aggregate_spill_restoration_fold.as_ref())
                .map(|(limit_fold, aggregate_fold)| {
                    query_paired_limit_aggregate_spill_restoration_transition(
                        paired_limit_aggregate_spill_restoration_fold.as_ref(),
                        limit_fold,
                        aggregate_fold,
                    )
                });
        if let (Some(pair_identity), Some(limit_fold), Some(aggregate_fold)) = (
            paired_limit_aggregate_spill_restoration_pair_identity.as_deref(),
            paired_join_limit_anchor_cascade_fold.as_ref(),
            paired_aggregate_spill_restoration_fold.as_ref(),
        ) {
            paired_limit_aggregate_spill_restoration_fold =
                Some(query_paired_limit_aggregate_spill_restoration_fold(
                    paired_limit_aggregate_spill_restoration_fold.as_ref(),
                    pair_identity,
                    limit_fold,
                    aggregate_fold,
                ));
        }
        if let (Some(fold), Some(transition)) = (
            paired_limit_aggregate_spill_restoration_fold.as_ref(),
            paired_limit_aggregate_spill_restoration_transition,
        ) {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_aggregate_spill_restoration_fold_details(
                    &mut operators[index].details,
                    fold,
                    transition,
                );
            }
        }
        if let Some(identity) = paired_window_spill_anchor_fold_id.as_deref() {
            let mut spill_nodes = BTreeSet::from([right_access, right]);
            spill_nodes.extend(right_window_operator_start..operators.len());
            for index in spill_nodes {
                add_paired_window_spill_anchor_fold_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let Some((identity, totals)) = paired_aggregate_spill_fold.as_ref() {
            let mut spill_nodes = BTreeSet::from([right_access, right]);
            spill_nodes.extend(right_window_operator_start..operators.len());
            for index in spill_nodes {
                add_paired_aggregate_spill_fold_details(
                    &mut operators[index].details,
                    identity,
                    totals,
                );
            }
        }
        if let Some(cascade_fold) = paired_window_spill_cascade_fold.as_ref() {
            let mut spill_nodes = BTreeSet::from([right_access, right]);
            spill_nodes.extend(right_window_operator_start..operators.len());
            for index in spill_nodes {
                add_paired_window_spill_cascade_fold_details(
                    &mut operators[index].details,
                    cascade_fold,
                );
            }
        }
        if let Some(rule_cost_fold) = paired_window_spill_rule_cost_fold.as_ref() {
            let mut spill_nodes = BTreeSet::from([right_access, right]);
            spill_nodes.extend(right_window_operator_start..operators.len());
            for index in spill_nodes {
                add_paired_window_spill_rule_cost_fold_details(
                    &mut operators[index].details,
                    rule_cost_fold,
                    paired_aggregate_spill_fold.is_some(),
                );
            }
        }
        if let (Some(fold), Some(transition)) = (
            paired_bounded_limit_window_spill_fold.as_ref(),
            paired_bounded_limit_window_spill_transition,
        ) {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_bounded_limit_window_spill_fold_details(
                    &mut operators[index].details,
                    fold,
                    transition,
                );
            }
        }
        if let Some(identity) = paired_aggregate_spill_restoration_pair_identity.as_deref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_aggregate_spill_restoration_pair_details(
                    &mut operators[index].details,
                    identity,
                    paired_aggregate_pushdown_anchor_fold_id.as_deref(),
                    paired_aggregate_spill_fold
                        .as_ref()
                        .map(|(identity, _)| identity.as_str()),
                );
            }
        }
        if let Some(fold) = paired_aggregate_spill_restoration_fold.as_ref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_aggregate_spill_restoration_fold_details(
                    &mut operators[index].details,
                    fold,
                );
            }
        }
        if let Some(identity) = paired_window_compaction_spill_pair_identity.as_deref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_window_compaction_spill_pair_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let Some(fold) = paired_window_compaction_spill_fold.as_ref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_window_compaction_spill_fold_details(
                    &mut operators[index].details,
                    fold,
                    paired_window_compaction_spill_pair_identity.is_some(),
                );
            }
        }
        if let Some(identity) = paired_limit_window_compaction_pair_identity.as_deref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_window_compaction_pair_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let Some(fold) = paired_limit_window_compaction_fold.as_ref() {
            let mut fold_nodes = BTreeSet::from([right_access, right]);
            fold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            fold_nodes.extend(right_window_operator_start..operators.len());
            for index in fold_nodes {
                add_paired_limit_window_compaction_fold_details(
                    &mut operators[index].details,
                    fold,
                    paired_limit_window_compaction_pair_identity.is_some(),
                );
            }
        }
        if let Some(identity) = paired_aggregate_pushdown_anchor_fold_id.as_deref() {
            let mut aggregate_nodes = BTreeSet::from([right_access, right]);
            aggregate_nodes.extend(right_window_operator_start..operators.len());
            for index in aggregate_nodes {
                add_paired_aggregate_pushdown_anchor_fold_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let Some(identity) = join_pair_anchor_fold_id.as_deref() {
            let mut pair_nodes = BTreeSet::from([right_access, right]);
            pair_nodes.extend(right_window_operator_start..operators.len());
            for index in pair_nodes {
                add_join_pair_anchor_fold_details(&mut operators[index].details, identity);
            }
        }
        if let Some(identity) = window_anchor_fold_id.as_deref() {
            let mut chain_nodes = BTreeSet::from([right_access, right]);
            chain_nodes.extend(right_window_operator_start..operators.len());
            for index in chain_nodes {
                add_window_anchor_fold_details(&mut operators[index].details, identity);
            }
        }
        if let Some(identity) = decorrelated_predicate_identity.as_deref() {
            for index in BTreeSet::from([right_access, right]) {
                add_decorrelated_predicate_pushdown_details(
                    &mut operators[index].details,
                    identity,
                );
            }
        }
        if let (Some(identity), Some(index_selection_identity)) = (
            decorrelated_anchor_fold_id.as_deref(),
            decorrelated_index_fold_id.as_deref(),
        ) {
            for index in BTreeSet::from([right_access, right]) {
                add_decorrelated_anchor_fold_details(
                    &mut operators[index].details,
                    identity,
                    &left_fold_identity,
                    &right_fold_identity,
                    index_selection_identity,
                );
            }
        }
        if let (
            Some(identity),
            Some(subquery),
            Some(omission_identity),
            Some(selection_identity),
        ) = (
            decorrelated_omission_refold_id.as_deref(),
            decorrelated_subquery,
            decorrelated_index_omission_identity.as_deref(),
            decorrelated_index_fold_id.as_deref(),
        ) {
            for index in BTreeSet::from([right_access, right]) {
                add_decorrelated_omission_refold_details(
                    &mut operators[index].details,
                    identity,
                    &left_fold_identity,
                    &right_fold_identity,
                    subquery,
                    omission_identity,
                    selection_identity,
                );
            }
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_decorrelated_pin_chain_identity.as_deref(),
            decorrelated_pin_chain_parent_identity,
            decorrelated_pin_chain_transition,
        ) {
            for index in BTreeSet::from([right_access, right]) {
                add_decorrelated_pin_chain_details(
                    &mut operators[index].details,
                    identity,
                    parent_identity,
                    &query.snapshot,
                    &left_fold_identity,
                    transition,
                    decorrelated_pin_chain_step_subquery_identity,
                    decorrelated_pin_chain_step_selection_identity,
                    decorrelated_pin_chain_step_anchor_fold_identity,
                    decorrelated_pin_chain_step_omission_refold_identity,
                );
            }
        }
        if let (Some(identity), Some(pair), Some(selection_identity)) = (
            paired_index_refold_id.as_deref(),
            join_pair_identity,
            paired_index_selection_id.as_deref(),
        ) {
            let mut refold_nodes = BTreeSet::from([right_access, right]);
            refold_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            for index in refold_nodes {
                add_paired_index_refold_details(
                    &mut operators[index].details,
                    identity,
                    &left_fold_identity,
                    &right_fold_identity,
                    pair,
                    selection_identity,
                );
            }
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_paired_index_anchor_chain_identity.as_deref(),
            paired_index_anchor_chain_parent_identity,
            paired_index_anchor_chain_transition,
        ) {
            let pair_identity = join_pair_identity.map(|pair| pair.identity.as_str());
            for index in BTreeSet::from([right_access, right]) {
                add_paired_index_anchor_chain_details(
                    &mut operators[index].details,
                    identity,
                    parent_identity,
                    &left_fold_identity,
                    transition,
                    pair_identity,
                    paired_index_anchor_chain_step_refold_identity,
                    paired_index_anchor_chain_step_selection_identity,
                );
            }
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_paired_index_cost_restoration_chain_identity.as_deref(),
            paired_index_cost_restoration_parent_identity,
            paired_index_cost_restoration_transition,
        ) {
            let step = paired_index_cost_restoration_advance.as_ref();
            let mut restoration_nodes = BTreeSet::from([right_access, right]);
            restoration_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            restoration_nodes.extend(right_window_operator_start..operators.len());
            for index in restoration_nodes {
                add_paired_index_cost_restoration_details(
                    &mut operators[index].details,
                    identity,
                    parent_identity,
                    &left_fold_identity,
                    declared_position,
                    planned_position,
                    transition,
                    step.map(|(_, _, pair, _, _, _)| *pair),
                    step.map(|(_, _, _, refold, _, _)| *refold),
                    step.map(|(_, _, _, _, selection, _)| *selection),
                    step.map(|(_, _, _, _, _, has_index)| *has_index),
                );
            }
        }
        if let (
            Some((identity, _, _, _)),
            Some(parent_identity),
            Some(transition),
            Some((pair_count, index_count, scan_count)),
        ) = (
            next_paired_index_cost_compaction_chain.as_ref(),
            paired_index_cost_compaction_parent_identity,
            paired_index_cost_compaction_transition,
            paired_index_cost_compaction_counts,
        ) {
            let mut compaction_nodes = BTreeSet::from([right_access, right]);
            compaction_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            compaction_nodes.extend(right_window_operator_start..operators.len());
            for index in compaction_nodes {
                add_paired_index_cost_compaction_details(
                    &mut operators[index].details,
                    identity,
                    parent_identity,
                    &left_fold_identity,
                    declared_position,
                    planned_position,
                    transition,
                    join_pair_identity.map(|pair| pair.identity.as_str()),
                    paired_index_selection_id.as_deref(),
                    paired_index_refold_id.as_deref(),
                    join_pair_identity.map(|_| {
                        if selected_partial_index.is_some() {
                            "exact_index"
                        } else {
                            "scan"
                        }
                    }),
                    pair_count,
                    index_count,
                    scan_count,
                );
            }
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_paired_decorrelation_cost_restoration_chain_identity.as_deref(),
            paired_decorrelation_cost_restoration_parent_identity,
            paired_decorrelation_cost_restoration_transition,
        ) {
            let mut restoration_nodes = BTreeSet::from([right_access, right]);
            restoration_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            restoration_nodes.extend(right_window_operator_start..operators.len());
            for index in restoration_nodes {
                add_paired_decorrelation_cost_restoration_details(
                    &mut operators[index].details,
                    identity,
                    parent_identity,
                    &left_fold_identity,
                    declared_position,
                    planned_position,
                    transition,
                    join_pair_identity,
                    paired_index_refold_id.as_deref(),
                    paired_index_selection_id.as_deref(),
                    join_pair_identity.map(|_| selected_partial_index.is_some()),
                    decorrelated_subquery,
                    next_decorrelated_pin_chain_identity.as_deref(),
                    decorrelated_anchor_fold_id.as_deref(),
                    decorrelated_omission_refold_id.as_deref(),
                );
            }
        }
        let paired_window_spill_rule_cost_pair_identity = join_pair_identity
            .zip(paired_window_spill_rule_cost_fold.as_ref())
            .map(|(pair, fold)| {
                query_paired_window_spill_rule_cost_pair_identity(
                    &left_fold_identity,
                    &right_fold_identity,
                    pair,
                    fold,
                )
            });
        let paired_window_cost_restoration_chain_identities = [
            (
                "checkpoint_compaction",
                checkpoint_compaction_chain
                    .map(query_paired_checkpoint_segment_compaction_chain_identity),
            ),
            (
                "segment_rotation",
                segment_rotation_chain.map(query_paired_segment_rotation_chain_identity),
            ),
            (
                "stream_compaction",
                stream_compaction_chain.map(query_paired_stream_compaction_chain_identity),
            ),
            (
                "checkpoint_spill_restore",
                spill_restore_chain.map(query_paired_checkpoint_spill_restore_chain_identity),
            ),
            (
                "stream_rotation",
                stream_rotation_chain.map(query_paired_stream_rotation_chain_identity),
            ),
            (
                "wal_rotation",
                wal_rotation_chain.map(query_paired_wal_rotation_chain_identity),
            ),
        ];
        let paired_window_cost_restoration_pair_identity = tracks_window_cost_restoration
            .then(|| {
                query_paired_window_cost_restoration_pair_identity(
                    paired_window_cost_restoration_fold.as_ref(),
                    &left_fold_identity,
                    &right_fold_identity,
                    join_pair_identity,
                    &join.source,
                    join.predicate.as_ref(),
                    right_window_identity.as_deref(),
                    paired_aggregate_spill_fold
                        .as_ref()
                        .map(|(identity, _)| identity.as_str()),
                    &paired_window_cost_restoration_chain_identities,
                )
            })
            .flatten();
        let next_join_cost_fold_identity = query_join_cost_fold(
            &left_fold_identity,
            &right_fold_identity,
            join_pair_identity,
            paired_predicate_pushdown_identity.as_deref(),
            paired_index_refold_id.as_deref(),
            decorrelated_predicate_identity.as_deref(),
            decorrelated_anchor_fold_id.as_deref(),
            decorrelated_omission_refold_id.as_deref(),
            window_anchor_fold_id.as_deref(),
            join_pair_anchor_fold_id.as_deref(),
            paired_limit_pushdown_anchor_fold_id.as_deref(),
            paired_aggregate_pushdown_anchor_fold_id.as_deref(),
            paired_window_spill_anchor_fold_id.as_deref(),
            paired_aggregate_spill_fold
                .as_ref()
                .map(|(identity, _)| identity.as_str()),
            paired_aggregate_anchor_cascade_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_scoped_window_compaction_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_window_spill_cascade_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_window_spill_rule_cost_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_window_spill_rule_cost_pair_identity.as_deref(),
            paired_window_cost_restoration_pair_identity.as_deref(),
            paired_aggregate_spill_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_limit_aggregate_spill_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_window_compaction_spill_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_limit_aggregate_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_scoped_window_limit_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_scoped_limit_window_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_bounded_limit_window_spill_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_limit_window_compaction_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_limit_window_cascade_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_join_limit_anchor_cascade_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            cardinality,
            work,
            work_overflow,
            paired_checkpoint_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_segment_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_stream_compaction_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_checkpoint_spill_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_stream_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
            paired_wal_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str()),
        );
        let paired_checkpoint_cost_restoration_advance =
            checkpoint_compaction_chain.map(|chain| {
                let parent_identity = paired_checkpoint_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.clone())
                    .unwrap_or_else(|| {
                        query_paired_checkpoint_cost_restoration_seed_identity(
                            &query.snapshot,
                            &left_fold_identity,
                        )
                    });
                let chain_identity =
                    query_paired_checkpoint_segment_compaction_chain_identity(chain);
                let fold = query_paired_checkpoint_cost_restoration_fold(
                    paired_checkpoint_cost_restoration_fold.as_ref(),
                    &parent_identity,
                    &next_join_cost_fold_identity,
                    next_paired_index_cost_compaction_chain
                        .as_ref()
                        .map(|(identity, _, _, _)| identity.as_str()),
                    chain,
                    &chain_identity,
                );
                (parent_identity, chain_identity, fold)
            });
        let next_paired_checkpoint_cost_restoration_fold =
            paired_checkpoint_cost_restoration_advance
                .as_ref()
                .map(|(_, _, fold)| fold.clone())
                .or_else(|| paired_checkpoint_cost_restoration_fold.clone());
        let paired_checkpoint_cost_restoration_parent_identity =
            paired_checkpoint_cost_restoration_advance
                .as_ref()
                .map(|(parent, _, _)| parent.as_str())
                .or_else(|| {
                    paired_checkpoint_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.as_str())
                });
        let paired_checkpoint_cost_restoration_transition =
            if paired_checkpoint_cost_restoration_advance.is_some() {
                Some("append_branch_checkpoint_segment_chain")
            } else if next_paired_checkpoint_cost_restoration_fold.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_segment_rotation_cost_restoration_advance = segment_rotation_chain.map(|chain| {
            let parent_identity = paired_segment_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.clone())
                .or_else(|| {
                    next_paired_checkpoint_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .unwrap_or_else(|| {
                    query_paired_segment_rotation_cost_restoration_seed_identity(
                        &query.snapshot,
                        &left_fold_identity,
                    )
                });
            let chain_identity = query_paired_segment_rotation_chain_identity(chain);
            let fold = query_paired_segment_rotation_cost_restoration_fold(
                paired_segment_rotation_cost_restoration_fold.as_ref(),
                &parent_identity,
                &next_join_cost_fold_identity,
                next_paired_checkpoint_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
                chain,
                &chain_identity,
            );
            (parent_identity, chain_identity, fold)
        });
        let next_paired_segment_rotation_cost_restoration_fold =
            paired_segment_rotation_cost_restoration_advance
                .as_ref()
                .map(|(_, _, fold)| fold.clone())
                .or_else(|| paired_segment_rotation_cost_restoration_fold.clone());
        let paired_segment_rotation_cost_restoration_parent_identity =
            paired_segment_rotation_cost_restoration_advance
                .as_ref()
                .map(|(parent, _, _)| parent.as_str())
                .or_else(|| {
                    paired_segment_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.as_str())
                });
        let paired_segment_rotation_cost_restoration_transition =
            if paired_segment_rotation_cost_restoration_advance.is_some() {
                Some("append_paired_segment_rotation_chain")
            } else if next_paired_segment_rotation_cost_restoration_fold.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_stream_compaction_cost_restoration_advance = stream_compaction_chain.map(|chain| {
            let checkpoint_fold_identity = next_paired_checkpoint_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let rotation_fold_identity = next_paired_segment_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let parent_identity = paired_stream_compaction_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.clone())
                .or_else(|| {
                    next_paired_segment_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_checkpoint_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .unwrap_or_else(|| {
                    query_paired_stream_compaction_cost_restoration_seed_identity(
                        &query.snapshot,
                        &left_fold_identity,
                    )
                });
            let chain_identity = query_paired_stream_compaction_chain_identity(chain);
            let fold = query_paired_stream_compaction_cost_restoration_fold(
                paired_stream_compaction_cost_restoration_fold.as_ref(),
                &parent_identity,
                &next_join_cost_fold_identity,
                checkpoint_fold_identity,
                rotation_fold_identity,
                chain,
                &chain_identity,
            );
            (parent_identity, chain_identity, fold)
        });
        let next_paired_stream_compaction_cost_restoration_fold =
            paired_stream_compaction_cost_restoration_advance
                .as_ref()
                .map(|(_, _, fold)| fold.clone())
                .or_else(|| paired_stream_compaction_cost_restoration_fold.clone());
        let paired_stream_compaction_cost_restoration_parent_identity =
            paired_stream_compaction_cost_restoration_advance
                .as_ref()
                .map(|(parent, _, _)| parent.as_str())
                .or_else(|| {
                    paired_stream_compaction_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.as_str())
                });
        let paired_stream_compaction_cost_restoration_transition =
            if paired_stream_compaction_cost_restoration_advance.is_some() {
                Some("append_paired_stream_compaction_chain")
            } else if next_paired_stream_compaction_cost_restoration_fold.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_checkpoint_spill_cost_restoration_advance = spill_restore_chain.map(|chain| {
            let stream_fold_identity = next_paired_stream_compaction_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let parent_identity = paired_checkpoint_spill_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.clone())
                .or_else(|| {
                    next_paired_stream_compaction_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_segment_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_checkpoint_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .unwrap_or_else(|| {
                    query_paired_checkpoint_spill_cost_restoration_seed_identity(
                        &query.snapshot,
                        &left_fold_identity,
                    )
                });
            let chain_identity = query_paired_checkpoint_spill_restore_chain_identity(chain);
            let fold = query_paired_checkpoint_spill_cost_restoration_fold(
                paired_checkpoint_spill_cost_restoration_fold.as_ref(),
                &parent_identity,
                &next_join_cost_fold_identity,
                stream_fold_identity,
                chain,
                &chain_identity,
            );
            (parent_identity, chain_identity, fold)
        });
        let next_paired_checkpoint_spill_cost_restoration_fold =
            paired_checkpoint_spill_cost_restoration_advance
                .as_ref()
                .map(|(_, _, fold)| fold.clone())
                .or_else(|| paired_checkpoint_spill_cost_restoration_fold.clone());
        let paired_checkpoint_spill_cost_restoration_parent_identity =
            paired_checkpoint_spill_cost_restoration_advance
                .as_ref()
                .map(|(parent, _, _)| parent.as_str())
                .or_else(|| {
                    paired_checkpoint_spill_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.as_str())
                });
        let paired_checkpoint_spill_cost_restoration_transition =
            if paired_checkpoint_spill_cost_restoration_advance.is_some() {
                Some("append_paired_checkpoint_spill_restore_chain")
            } else if next_paired_checkpoint_spill_cost_restoration_fold.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_stream_rotation_cost_restoration_advance =
            stream_rotation_chain.map(|chain| {
                let spill_fold_identity = next_paired_checkpoint_spill_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str());
                let parent_identity = paired_stream_rotation_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.clone())
                    .or_else(|| {
                        next_paired_checkpoint_spill_cost_restoration_fold
                            .as_ref()
                            .map(|fold| fold.identity.clone())
                    })
                    .or_else(|| {
                        next_paired_stream_compaction_cost_restoration_fold
                            .as_ref()
                            .map(|fold| fold.identity.clone())
                    })
                    .or_else(|| {
                        next_paired_segment_rotation_cost_restoration_fold
                            .as_ref()
                            .map(|fold| fold.identity.clone())
                    })
                    .or_else(|| {
                        next_paired_checkpoint_cost_restoration_fold
                            .as_ref()
                            .map(|fold| fold.identity.clone())
                    })
                    .unwrap_or_else(|| {
                        query_paired_stream_rotation_cost_restoration_seed_identity(
                            &query.snapshot,
                            &left_fold_identity,
                        )
                    });
                let chain_identity = query_paired_stream_rotation_chain_identity(chain);
                let fold = query_paired_stream_rotation_cost_restoration_fold(
                    paired_stream_rotation_cost_restoration_fold.as_ref(),
                    &parent_identity,
                    &next_join_cost_fold_identity,
                    spill_fold_identity,
                    chain,
                    &chain_identity,
                );
                (parent_identity, chain_identity, fold)
            });
        let next_paired_stream_rotation_cost_restoration_fold =
            paired_stream_rotation_cost_restoration_advance
                .as_ref()
                .map(|(_, _, fold)| fold.clone())
                .or_else(|| paired_stream_rotation_cost_restoration_fold.clone());
        let paired_stream_rotation_cost_restoration_parent_identity =
            paired_stream_rotation_cost_restoration_advance
                .as_ref()
                .map(|(parent, _, _)| parent.as_str())
                .or_else(|| {
                    paired_stream_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.as_str())
                });
        let paired_stream_rotation_cost_restoration_transition =
            if paired_stream_rotation_cost_restoration_advance.is_some() {
                Some("append_paired_stream_rotation_chain")
            } else if next_paired_stream_rotation_cost_restoration_fold.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_wal_rotation_cost_restoration_advance = wal_rotation_chain.map(|chain| {
            let stream_rotation_fold_identity = next_paired_stream_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let parent_identity = paired_wal_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.clone())
                .or_else(|| {
                    next_paired_stream_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_checkpoint_spill_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_stream_compaction_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_segment_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .or_else(|| {
                    next_paired_checkpoint_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.clone())
                })
                .unwrap_or_else(|| {
                    query_paired_wal_rotation_cost_restoration_seed_identity(
                        &query.snapshot,
                        &left_fold_identity,
                    )
                });
            let chain_identity = query_paired_wal_rotation_chain_identity(chain);
            let fold = query_paired_wal_rotation_cost_restoration_fold(
                paired_wal_rotation_cost_restoration_fold.as_ref(),
                &parent_identity,
                &next_join_cost_fold_identity,
                stream_rotation_fold_identity,
                chain,
                &chain_identity,
            );
            (parent_identity, chain_identity, fold)
        });
        let next_paired_wal_rotation_cost_restoration_fold =
            paired_wal_rotation_cost_restoration_advance
                .as_ref()
                .map(|(_, _, fold)| fold.clone())
                .or_else(|| paired_wal_rotation_cost_restoration_fold.clone());
        let paired_wal_rotation_cost_restoration_parent_identity =
            paired_wal_rotation_cost_restoration_advance
                .as_ref()
                .map(|(parent, _, _)| parent.as_str())
                .or_else(|| {
                    paired_wal_rotation_cost_restoration_fold
                        .as_ref()
                        .map(|fold| fold.identity.as_str())
                });
        let paired_wal_rotation_cost_restoration_transition =
            if paired_wal_rotation_cost_restoration_advance.is_some() {
                Some("append_paired_wal_rotation_chain")
            } else if next_paired_wal_rotation_cost_restoration_fold.is_some() {
                Some("carry_through_sparse_cost_fold")
            } else {
                None
            };
        let paired_window_cost_restoration_fold_identities = [
            (
                "checkpoint_cost_restoration",
                next_paired_checkpoint_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
            ),
            (
                "segment_rotation_cost_restoration",
                next_paired_segment_rotation_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
            ),
            (
                "stream_compaction_cost_restoration",
                next_paired_stream_compaction_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
            ),
            (
                "checkpoint_spill_cost_restoration",
                next_paired_checkpoint_spill_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
            ),
            (
                "stream_rotation_cost_restoration",
                next_paired_stream_rotation_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
            ),
            (
                "wal_rotation_cost_restoration",
                next_paired_wal_rotation_cost_restoration_fold
                    .as_ref()
                    .map(|fold| fold.identity.as_str()),
            ),
        ];
        let paired_window_cost_restoration_transition =
            paired_window_cost_restoration_pair_identity.as_ref().map(|_| {
                let has_restore_chain = paired_window_cost_restoration_chain_identities
                    .iter()
                    .any(|(_, identity)| identity.is_some());
                if right_window_identity.is_some() && has_restore_chain {
                    "append_window_and_cost_restore_pair"
                } else if right_window_identity.is_some() {
                    "append_window_pair"
                } else if has_restore_chain {
                    "append_cost_restore_pair"
                } else {
                    "carry_across_sparse_pair"
                }
            });
        let next_paired_window_cost_restoration_fold = paired_window_cost_restoration_pair_identity
            .as_deref()
            .map(|pair_identity| {
                query_paired_window_cost_restoration_fold(
                    paired_window_cost_restoration_fold.as_ref(),
                    pair_identity,
                    join_pair_identity.map_or(join.source.as_str(), |pair| pair.identity.as_str()),
                    right_window_identity.as_deref(),
                    paired_aggregate_spill_fold
                        .as_ref()
                        .map(|(identity, _)| identity.as_str()),
                    &paired_window_cost_restoration_fold_identities,
                )
            });
        for index in BTreeSet::from([right_access, right]) {
            operators[index].details.insert(
                "paired_join_cost_fold_identity".to_owned(),
                PlanDetail::Text(next_join_cost_fold_identity.clone()),
            );
            if let Some(identity) = paired_window_spill_rule_cost_pair_identity.as_deref() {
                add_paired_window_spill_rule_cost_pair_identity_details(
                    &mut operators[index].details,
                    identity,
                );
            }
            if let (Some(fold), Some(pair_identity), Some(transition)) = (
                next_paired_window_cost_restoration_fold.as_ref(),
                paired_window_cost_restoration_pair_identity.as_deref(),
                paired_window_cost_restoration_transition,
            ) {
                add_paired_window_cost_restoration_fold_details(
                    &mut operators[index].details,
                    fold,
                    pair_identity,
                    paired_aggregate_spill_fold
                        .as_ref()
                        .map(|(identity, _)| identity.as_str()),
                    transition,
                );
            }
        }
        let mut details = BTreeMap::from([(
            "strategy".to_owned(),
            PlanDetail::Text("hash".to_owned()),
        )]);
        details.insert(
            "declared_input_position".to_owned(),
            PlanDetail::Integer(
                u64::try_from(declared_position + 1).map_err(|_| ExplainError::TooManyNodes)?,
            ),
        );
        details.insert(
            "planned_input_position".to_owned(),
            PlanDetail::Integer(
                u64::try_from(planned_position + 1).map_err(|_| ExplainError::TooManyNodes)?,
            ),
        );
        details.insert(
            "join_order_policy".to_owned(),
            PlanDetail::Text("known_scan_work_then_declared".to_owned()),
        );
        if work_overflow {
            record_work_overflow(&mut details);
        }
        if join.predicate.is_some() {
            details.insert(
                "selectivity_assumption".to_owned(),
                PlanDetail::Text("0.1_no_histogram".to_owned()),
            );
        } else {
            details.insert(
                "join_type".to_owned(),
                PlanDetail::Text("cross".to_owned()),
            );
        }
        if let Some(subquery) = decorrelated_subquery {
            add_decorrelated_subquery_details(&mut details, subquery);
        }
        if let Some(identity) = decorrelated_predicate_identity.as_deref() {
            add_decorrelated_predicate_pushdown_details(&mut details, identity);
        }
        if let Some(identity) = decorrelated_index_omission_identity.as_deref() {
            add_decorrelated_index_selection_omission_details(&mut details, identity);
        }
        if let (Some(identity), Some(index_selection_identity)) = (
            decorrelated_anchor_fold_id.as_deref(),
            decorrelated_index_fold_id.as_deref(),
        ) {
            add_decorrelated_anchor_fold_details(
                &mut details,
                identity,
                &left_fold_identity,
                &right_fold_identity,
                index_selection_identity,
            );
        }
        if let (
            Some(identity),
            Some(subquery),
            Some(omission_identity),
            Some(selection_identity),
        ) = (
            decorrelated_omission_refold_id.as_deref(),
            decorrelated_subquery,
            decorrelated_index_omission_identity.as_deref(),
            decorrelated_index_fold_id.as_deref(),
        ) {
            add_decorrelated_omission_refold_details(
                &mut details,
                identity,
                &left_fold_identity,
                &right_fold_identity,
                subquery,
                omission_identity,
                selection_identity,
            );
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_decorrelated_pin_chain_identity.as_deref(),
            decorrelated_pin_chain_parent_identity,
            decorrelated_pin_chain_transition,
        ) {
            add_decorrelated_pin_chain_details(
                &mut details,
                identity,
                parent_identity,
                &query.snapshot,
                &left_fold_identity,
                transition,
                decorrelated_pin_chain_step_subquery_identity,
                decorrelated_pin_chain_step_selection_identity,
                decorrelated_pin_chain_step_anchor_fold_identity,
                decorrelated_pin_chain_step_omission_refold_identity,
            );
        }
        if let Some(identity) = window_anchor_fold_id.as_deref() {
            add_window_anchor_fold_details(&mut details, identity);
        }
        if let Some(identity) = join_pair_anchor_fold_id.as_deref() {
            add_join_pair_anchor_fold_details(&mut details, identity);
        }
        if let Some(identity) = paired_aggregate_pushdown_anchor_fold_id.as_deref() {
            add_paired_aggregate_pushdown_anchor_fold_details(&mut details, identity);
        }
        if let Some(cascade_fold) = paired_aggregate_anchor_cascade_fold.as_ref() {
            add_paired_aggregate_cascade_fold_details(&mut details, cascade_fold);
        }
        if let Some(identity) = paired_scoped_window_compaction_pair_identity.as_deref() {
            add_paired_scoped_window_compaction_pair_details(&mut details, identity);
        }
        if let Some(fold) = paired_scoped_window_compaction_fold.as_ref() {
            add_paired_scoped_window_compaction_fold_details(
                &mut details,
                fold,
                paired_scoped_window_compaction_pair_identity.is_some(),
            );
        }
        if let Some(identity) = paired_window_spill_anchor_fold_id.as_deref() {
            add_paired_window_spill_anchor_fold_details(&mut details, identity);
        }
        if let Some((identity, totals)) = paired_aggregate_spill_fold.as_ref() {
            add_paired_aggregate_spill_fold_details(&mut details, identity, totals);
        }
        if let Some(cascade_fold) = paired_window_spill_cascade_fold.as_ref() {
            add_paired_window_spill_cascade_fold_details(&mut details, cascade_fold);
        }
        if let Some(rule_cost_fold) = paired_window_spill_rule_cost_fold.as_ref() {
            add_paired_window_spill_rule_cost_fold_details(
                &mut details,
                rule_cost_fold,
                paired_aggregate_spill_fold.is_some(),
            );
        }
        if let (Some(fold), Some(transition)) = (
            paired_bounded_limit_window_spill_fold.as_ref(),
            paired_bounded_limit_window_spill_transition,
        ) {
            add_paired_bounded_limit_window_spill_fold_details(
                &mut details,
                fold,
                transition,
            );
        }
        if let Some(identity) = paired_aggregate_spill_restoration_pair_identity.as_deref() {
            add_paired_aggregate_spill_restoration_pair_details(
                &mut details,
                identity,
                paired_aggregate_pushdown_anchor_fold_id.as_deref(),
                paired_aggregate_spill_fold
                    .as_ref()
                    .map(|(identity, _)| identity.as_str()),
            );
        }
        if let Some(fold) = paired_aggregate_spill_restoration_fold.as_ref() {
            add_paired_aggregate_spill_restoration_fold_details(&mut details, fold);
        }
        if let (Some(fold), Some(transition)) = (
            paired_limit_aggregate_spill_restoration_fold.as_ref(),
            paired_limit_aggregate_spill_restoration_transition,
        ) {
            add_paired_limit_aggregate_spill_restoration_fold_details(
                &mut details,
                fold,
                transition,
            );
        }
        if let Some(identity) = paired_window_compaction_spill_pair_identity.as_deref() {
            add_paired_window_compaction_spill_pair_details(&mut details, identity);
        }
        if let Some(fold) = paired_window_compaction_spill_fold.as_ref() {
            add_paired_window_compaction_spill_fold_details(
                &mut details,
                fold,
                paired_window_compaction_spill_pair_identity.is_some(),
            );
        }
        if let Some(identity) = paired_limit_aggregate_restoration_pair_identity.as_deref() {
            add_paired_limit_aggregate_restoration_pair_details(
                &mut details,
                identity,
                paired_limit_pushdown_anchor_fold_id.as_deref(),
                paired_aggregate_pushdown_anchor_fold_id.as_deref(),
            );
        }
        if let Some(fold) = paired_limit_aggregate_restoration_fold.as_ref() {
            add_paired_limit_aggregate_restoration_fold_details(&mut details, fold);
        }
        if let (Some(fold), Some(transition)) = (
            paired_scoped_window_limit_restoration_fold.as_ref(),
            paired_scoped_window_limit_restoration_transition,
        ) {
            add_paired_scoped_window_limit_restoration_fold_details(
                &mut details,
                fold,
                transition,
            );
        }
        if let (Some(fold), Some(transition)) = (
            paired_scoped_limit_window_restoration_fold.as_ref(),
            paired_scoped_limit_window_restoration_transition,
        ) {
            add_paired_scoped_limit_window_restoration_fold_details(
                &mut details,
                fold,
                transition,
            );
        }
        if let Some(identity) = paired_limit_window_compaction_pair_identity.as_deref() {
            add_paired_limit_window_compaction_pair_details(&mut details, identity);
        }
        if let Some(fold) = paired_limit_window_compaction_fold.as_ref() {
            add_paired_limit_window_compaction_fold_details(
                &mut details,
                fold,
                paired_limit_window_compaction_pair_identity.is_some(),
            );
        }
        if let Some(identity) = paired_limit_pushdown_anchor_fold_id.as_deref() {
            add_paired_limit_pushdown_anchor_fold_details(&mut details, identity);
        }
        if let Some(identity) = paired_join_limit_anchor_fold_id.as_deref() {
            add_paired_join_limit_anchor_fold_details(&mut details, identity);
        }
        if let Some(cascade_fold) = paired_join_limit_anchor_cascade_fold.as_ref() {
            add_paired_join_limit_anchor_cascade_fold_details(&mut details, cascade_fold);
        }
        if let Some(identity) = paired_limit_window_anchor_fold_id.as_deref() {
            add_paired_limit_window_anchor_fold_details(&mut details, identity);
        }
        if let Some(cascade_fold) = paired_limit_window_cascade_fold.as_ref() {
            add_paired_limit_window_cascade_fold_details(&mut details, cascade_fold);
        }
        if let Some(identity) = right_limit_chain_identity.as_deref() {
            add_query_limit_pushdown_chain_details(&mut details, identity);
        }
        if let Some(pair_identity) = join_pair_identity {
            add_join_pair_identity_details(&mut details, pair_identity);
        }
        if let Some(identity) = paired_predicate_pushdown_identity.as_deref() {
            add_paired_predicate_pushdown_details(&mut details, identity);
        }
        if let Some(identity) = paired_index_selection_id.as_deref() {
            add_paired_index_selection_details(&mut details, identity);
        }
        if let (Some(identity), Some(pair), Some(selection_identity)) = (
            paired_index_refold_id.as_deref(),
            join_pair_identity,
            paired_index_selection_id.as_deref(),
        ) {
            add_paired_index_refold_details(
                &mut details,
                identity,
                &left_fold_identity,
                &right_fold_identity,
                pair,
                selection_identity,
            );
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_paired_index_anchor_chain_identity.as_deref(),
            paired_index_anchor_chain_parent_identity,
            paired_index_anchor_chain_transition,
        ) {
            let pair_identity = join_pair_identity.map(|pair| pair.identity.as_str());
            add_paired_index_anchor_chain_details(
                &mut details,
                identity,
                parent_identity,
                &left_fold_identity,
                transition,
                pair_identity,
                paired_index_anchor_chain_step_refold_identity,
                paired_index_anchor_chain_step_selection_identity,
            );
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_paired_index_cost_restoration_chain_identity.as_deref(),
            paired_index_cost_restoration_parent_identity,
            paired_index_cost_restoration_transition,
        ) {
            let step = paired_index_cost_restoration_advance.as_ref();
            add_paired_index_cost_restoration_details(
                &mut details,
                identity,
                parent_identity,
                &left_fold_identity,
                declared_position,
                planned_position,
                transition,
                step.map(|(_, _, pair, _, _, _)| *pair),
                step.map(|(_, _, _, refold, _, _)| *refold),
                step.map(|(_, _, _, _, selection, _)| *selection),
                step.map(|(_, _, _, _, _, has_index)| *has_index),
            );
        }
        if let (
            Some((identity, _, _, _)),
            Some(parent_identity),
            Some(transition),
            Some((pair_count, index_count, scan_count)),
        ) = (
            next_paired_index_cost_compaction_chain.as_ref(),
            paired_index_cost_compaction_parent_identity,
            paired_index_cost_compaction_transition,
            paired_index_cost_compaction_counts,
        ) {
            add_paired_index_cost_compaction_details(
                &mut details,
                identity,
                parent_identity,
                &left_fold_identity,
                declared_position,
                planned_position,
                transition,
                join_pair_identity.map(|pair| pair.identity.as_str()),
                paired_index_selection_id.as_deref(),
                paired_index_refold_id.as_deref(),
                join_pair_identity.map(|_| {
                    if selected_partial_index.is_some() {
                        "exact_index"
                    } else {
                        "scan"
                    }
                }),
                pair_count,
                index_count,
                scan_count,
            );
        }
        if let (Some(identity), Some(parent_identity), Some(transition)) = (
            next_paired_decorrelation_cost_restoration_chain_identity.as_deref(),
            paired_decorrelation_cost_restoration_parent_identity,
            paired_decorrelation_cost_restoration_transition,
        ) {
            add_paired_decorrelation_cost_restoration_details(
                &mut details,
                identity,
                parent_identity,
                &left_fold_identity,
                declared_position,
                planned_position,
                transition,
                join_pair_identity,
                paired_index_refold_id.as_deref(),
                paired_index_selection_id.as_deref(),
                join_pair_identity.map(|_| selected_partial_index.is_some()),
                decorrelated_subquery,
                next_decorrelated_pin_chain_identity.as_deref(),
                decorrelated_anchor_fold_id.as_deref(),
                decorrelated_omission_refold_id.as_deref(),
            );
        }
        if let Some(window_identity) = right_window_identity.as_deref() {
            add_window_pushdown_chain_details(&mut details, window_identity);
        }
        if let Some(spill_chain_identity) = right_window_spill_chain_identity.as_deref() {
            add_window_spill_chain_details(&mut details, spill_chain_identity);
        }
        if let Some(candidate) = selected_partial_index {
            add_partial_index_pair_identity_details(&mut details, candidate);
            add_partial_index_selection_tie_break_details(&mut details);
        }
        add_join_cost_fold_details(
            &mut details,
            &left_fold_identity,
            &right_fold_identity,
            &next_join_cost_fold_identity,
        );
        if let Some(identity) = paired_window_spill_rule_cost_pair_identity.as_deref() {
            add_paired_window_spill_rule_cost_pair_identity_details(&mut details, identity);
        }
        if let (Some(fold), Some(pair_identity), Some(transition)) = (
            next_paired_window_cost_restoration_fold.as_ref(),
            paired_window_cost_restoration_pair_identity.as_deref(),
            paired_window_cost_restoration_transition,
        ) {
            add_paired_window_cost_restoration_fold_details(
                &mut details,
                fold,
                pair_identity,
                paired_aggregate_spill_fold
                    .as_ref()
                    .map(|(identity, _)| identity.as_str()),
                transition,
            );
        }
        if let (
            Some(fold),
            Some(parent_identity),
            Some(transition),
        ) = (
            next_paired_checkpoint_cost_restoration_fold.as_ref(),
            paired_checkpoint_cost_restoration_parent_identity,
            paired_checkpoint_cost_restoration_transition,
        ) {
            let chain = paired_checkpoint_cost_restoration_advance
                .as_ref()
                .map(|(_, chain_identity, _)| chain_identity.as_str());
            let pair = checkpoint_compaction_chain
                .map(|chain| chain.join_pair_identity.as_str());
            let mut checkpoint_nodes = BTreeSet::from([right_access, right]);
            checkpoint_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            checkpoint_nodes.extend(right_window_operator_start..operators.len());
            for index in checkpoint_nodes {
                add_paired_checkpoint_cost_restoration_details(
                    &mut operators[index].details,
                    fold,
                    parent_identity,
                    &next_join_cost_fold_identity,
                    transition,
                    pair,
                    chain,
                    checkpoint_compaction_chain,
                );
            }
            add_paired_checkpoint_cost_restoration_details(
                &mut details,
                fold,
                parent_identity,
                &next_join_cost_fold_identity,
                transition,
                pair,
                chain,
                checkpoint_compaction_chain,
            );
        }
        if let (
            Some(fold),
            Some(parent_identity),
            Some(transition),
        ) = (
            next_paired_segment_rotation_cost_restoration_fold.as_ref(),
            paired_segment_rotation_cost_restoration_parent_identity,
            paired_segment_rotation_cost_restoration_transition,
        ) {
            let chain_identity = paired_segment_rotation_cost_restoration_advance
                .as_ref()
                .map(|(_, chain_identity, _)| chain_identity.as_str());
            let pair_identity = segment_rotation_chain
                .map(|chain| chain.join_pair_identity.as_str());
            let checkpoint_fold_identity = next_paired_checkpoint_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let mut rotation_nodes = BTreeSet::from([right_access, right]);
            rotation_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            rotation_nodes.extend(right_window_operator_start..operators.len());
            for index in rotation_nodes {
                add_paired_segment_rotation_cost_restoration_details(
                    &mut operators[index].details,
                    fold,
                    parent_identity,
                    &next_join_cost_fold_identity,
                    transition,
                    pair_identity,
                    chain_identity,
                    checkpoint_fold_identity,
                    segment_rotation_chain,
                );
            }
            add_paired_segment_rotation_cost_restoration_details(
                &mut details,
                fold,
                parent_identity,
                &next_join_cost_fold_identity,
                transition,
                pair_identity,
                chain_identity,
                checkpoint_fold_identity,
                segment_rotation_chain,
            );
        }
        if let (
            Some(fold),
            Some(parent_identity),
            Some(transition),
        ) = (
            next_paired_stream_compaction_cost_restoration_fold.as_ref(),
            paired_stream_compaction_cost_restoration_parent_identity,
            paired_stream_compaction_cost_restoration_transition,
        ) {
            let chain_identity = paired_stream_compaction_cost_restoration_advance
                .as_ref()
                .map(|(_, chain_identity, _)| chain_identity.as_str());
            let pair_identity = stream_compaction_chain
                .map(|chain| chain.join_pair_identity.as_str());
            let checkpoint_fold_identity = next_paired_checkpoint_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let rotation_fold_identity = next_paired_segment_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let mut stream_nodes = BTreeSet::from([right_access, right]);
            stream_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            stream_nodes.extend(right_window_operator_start..operators.len());
            for index in stream_nodes {
                add_paired_stream_compaction_cost_restoration_details(
                    &mut operators[index].details,
                    fold,
                    parent_identity,
                    &next_join_cost_fold_identity,
                    transition,
                    pair_identity,
                    chain_identity,
                    checkpoint_fold_identity,
                    rotation_fold_identity,
                    stream_compaction_chain,
                );
            }
            add_paired_stream_compaction_cost_restoration_details(
                &mut details,
                fold,
                parent_identity,
                &next_join_cost_fold_identity,
                transition,
                pair_identity,
                chain_identity,
                checkpoint_fold_identity,
                rotation_fold_identity,
                stream_compaction_chain,
            );
        }
        if let (
            Some(fold),
            Some(parent_identity),
            Some(transition),
        ) = (
            next_paired_checkpoint_spill_cost_restoration_fold.as_ref(),
            paired_checkpoint_spill_cost_restoration_parent_identity,
            paired_checkpoint_spill_cost_restoration_transition,
        ) {
            let chain_identity = paired_checkpoint_spill_cost_restoration_advance
                .as_ref()
                .map(|(_, chain_identity, _)| chain_identity.as_str());
            let pair_identity = spill_restore_chain.map(|chain| chain.join_pair_identity.as_str());
            let stream_fold_identity = next_paired_stream_compaction_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let mut spill_nodes = BTreeSet::from([right_access, right]);
            spill_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            spill_nodes.extend(right_window_operator_start..operators.len());
            for index in spill_nodes {
                add_paired_checkpoint_spill_cost_restoration_details(
                    &mut operators[index].details,
                    fold,
                    parent_identity,
                    &next_join_cost_fold_identity,
                    transition,
                    pair_identity,
                    chain_identity,
                    stream_fold_identity,
                    spill_restore_chain,
                );
            }
            add_paired_checkpoint_spill_cost_restoration_details(
                &mut details,
                fold,
                parent_identity,
                &next_join_cost_fold_identity,
                transition,
                pair_identity,
                chain_identity,
                stream_fold_identity,
                spill_restore_chain,
            );
        }
        if let (
            Some(fold),
            Some(parent_identity),
            Some(transition),
        ) = (
            next_paired_stream_rotation_cost_restoration_fold.as_ref(),
            paired_stream_rotation_cost_restoration_parent_identity,
            paired_stream_rotation_cost_restoration_transition,
        ) {
            let chain_identity = paired_stream_rotation_cost_restoration_advance
                .as_ref()
                .map(|(_, chain_identity, _)| chain_identity.as_str());
            let pair_identity = stream_rotation_chain
                .map(|chain| chain.join_pair_identity.as_str());
            let spill_fold_identity = next_paired_checkpoint_spill_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let mut rotation_nodes = BTreeSet::from([right_access, right]);
            rotation_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            rotation_nodes.extend(right_window_operator_start..operators.len());
            for index in rotation_nodes {
                add_paired_stream_rotation_cost_restoration_details(
                    &mut operators[index].details,
                    fold,
                    parent_identity,
                    &next_join_cost_fold_identity,
                    transition,
                    pair_identity,
                    chain_identity,
                    spill_fold_identity,
                    stream_rotation_chain,
                );
            }
            add_paired_stream_rotation_cost_restoration_details(
                &mut details,
                fold,
                parent_identity,
                &next_join_cost_fold_identity,
                transition,
                pair_identity,
                chain_identity,
                spill_fold_identity,
                stream_rotation_chain,
            );
        }
        if let (
            Some(fold),
            Some(parent_identity),
            Some(transition),
        ) = (
            next_paired_wal_rotation_cost_restoration_fold.as_ref(),
            paired_wal_rotation_cost_restoration_parent_identity,
            paired_wal_rotation_cost_restoration_transition,
        ) {
            let chain_identity = paired_wal_rotation_cost_restoration_advance
                .as_ref()
                .map(|(_, chain_identity, _)| chain_identity.as_str());
            let pair_identity = wal_rotation_chain
                .map(|chain| chain.join_pair_identity.as_str());
            let stream_rotation_fold_identity = next_paired_stream_rotation_cost_restoration_fold
                .as_ref()
                .map(|fold| fold.identity.as_str());
            let mut wal_nodes = BTreeSet::from([right_access, right]);
            wal_nodes.extend(right_limit_operator_start..right_limit_operator_end);
            wal_nodes.extend(right_window_operator_start..operators.len());
            for index in wal_nodes {
                add_paired_wal_rotation_cost_restoration_details(
                    &mut operators[index].details,
                    fold,
                    parent_identity,
                    &next_join_cost_fold_identity,
                    transition,
                    pair_identity,
                    chain_identity,
                    stream_rotation_fold_identity,
                    wal_rotation_chain,
                );
            }
            add_paired_wal_rotation_cost_restoration_details(
                &mut details,
                fold,
                parent_identity,
                &next_join_cost_fold_identity,
                transition,
                pair_identity,
                chain_identity,
                stream_rotation_fold_identity,
                wal_rotation_chain,
            );
        }
        let prior = current;
        current = operators.len();
        operators.push(Operator::new(
            PlanNodeKind::Join,
            None,
            join.predicate.clone(),
            details,
            vec![prior, right],
            cardinality,
            work,
        ));
        current_cardinality = cardinality;
        join_cost_fold_identity = next_join_cost_fold_identity;
        decorrelated_pin_chain_identity = next_decorrelated_pin_chain_identity;
        paired_index_anchor_chain_identity = next_paired_index_anchor_chain_identity;
        paired_index_cost_restoration_chain_identity =
            next_paired_index_cost_restoration_chain_identity;
        paired_index_cost_compaction_chain = next_paired_index_cost_compaction_chain;
        paired_checkpoint_cost_restoration_fold =
            next_paired_checkpoint_cost_restoration_fold;
        paired_segment_rotation_cost_restoration_fold =
            next_paired_segment_rotation_cost_restoration_fold;
        paired_stream_compaction_cost_restoration_fold =
            next_paired_stream_compaction_cost_restoration_fold;
        paired_checkpoint_spill_cost_restoration_fold =
            next_paired_checkpoint_spill_cost_restoration_fold;
        paired_stream_rotation_cost_restoration_fold =
            next_paired_stream_rotation_cost_restoration_fold;
        paired_wal_rotation_cost_restoration_fold =
            next_paired_wal_rotation_cost_restoration_fold;
        paired_window_cost_restoration_fold = next_paired_window_cost_restoration_fold;
        paired_decorrelation_cost_restoration_chain_identity =
            next_paired_decorrelation_cost_restoration_chain_identity;
    }
    for limit in nested_input_limits {
        let cardinality = limit_cardinality(current_cardinality, *limit);
        let work = current_cardinality.rows;
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Limit,
            None,
            BTreeMap::from([("limit".to_owned(), PlanDetail::Integer(*limit))]),
            cardinality,
            work,
        );
        current_cardinality = cardinality;
    }
    for limit in nested_branch_limits {
        let cardinality = limit_cardinality(current_cardinality, *limit);
        let work = current_cardinality
            .rows
            .and_then(|rows| rows.checked_mul(disjunct_count));
        let mut details = BTreeMap::from([
            ("limit".to_owned(), PlanDetail::Integer(*limit)),
            (
                "limit_scope".to_owned(),
                PlanDetail::Text("per_disjunct_branch".to_owned()),
            ),
            (
                "disjunct_count".to_owned(),
                PlanDetail::Integer(disjunct_count),
            ),
        ]);
        if current_cardinality.rows.is_some() && work.is_none() {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Limit,
            None,
            details,
            cardinality,
            work,
        );
        current_cardinality = cardinality;
    }
    if let Some(predicate) = &query.predicate {
        // Expression references are opaque to this planner. The selected API
        // shape supplies any branch/conjunct counts; the default count of one
        // preserves the historical single-filter fallback.
        let (cardinality, work, mut details) = if post_expansion_conjunct.is_some()
            && conjunct_count_per_disjunct.is_none()
        {
            (
                disjunction_cardinality(current_cardinality, disjunct_count),
                current_cardinality
                    .rows
                    .and_then(|rows| rows.checked_mul(disjunct_count)),
                BTreeMap::from([
                    (
                        "selectivity_assumption".to_owned(),
                        PlanDetail::Text("0.5_per_disjunct_independent_or".to_owned()),
                    ),
                    (
                        "disjunct_count".to_owned(),
                        PlanDetail::Integer(disjunct_count),
                    ),
                    (
                        "expansion_work".to_owned(),
                        PlanDetail::Text("full_input_per_disjunct".to_owned()),
                    ),
                ]),
            )
        } else if let Some(conjunct_count) = conjunct_count_per_disjunct {
            let (cardinality, work) = conjunctive_disjunction_cardinality_and_work(
                current_cardinality,
                disjunct_count,
                conjunct_count,
            );
            (
                cardinality,
                work,
                BTreeMap::from([
                    (
                        "selectivity_assumption".to_owned(),
                        PlanDetail::Text(
                            "0.5_per_left_to_right_conjunct_independent_disjuncts".to_owned(),
                        ),
                    ),
                    (
                        "disjunct_count".to_owned(),
                        PlanDetail::Integer(disjunct_count),
                    ),
                    (
                        "conjunct_count_per_disjunct".to_owned(),
                        PlanDetail::Integer(conjunct_count),
                    ),
                    (
                        "conjunct_order".to_owned(),
                        PlanDetail::Text("left_to_right_short_circuit".to_owned()),
                    ),
                    (
                        "expansion_work".to_owned(),
                        PlanDetail::Text("full_input_per_disjunct".to_owned()),
                    ),
                ]),
            )
        } else if let Some(conjunct_count) = conjunct_count {
            let (cardinality, work) = disjunction_conjunct_cardinality_and_work(
                current_cardinality,
                disjunct_count,
                conjunct_count,
            );
            (
                cardinality,
                work,
                BTreeMap::from([
                    (
                        "selectivity_assumption".to_owned(),
                        PlanDetail::Text(
                            "0.5_per_disjunct_independent_or_then_0.5_per_conjunct_and".to_owned(),
                        ),
                    ),
                    (
                        "disjunct_count".to_owned(),
                        PlanDetail::Integer(disjunct_count),
                    ),
                    (
                        "conjunct_count".to_owned(),
                        PlanDetail::Integer(conjunct_count),
                    ),
                    (
                        "conjunct_order".to_owned(),
                        PlanDetail::Text("left_to_right_short_circuit".to_owned()),
                    ),
                ]),
            )
        } else if disjunct_count == 1 {
            (
                scale_cardinality(current_cardinality, 1, 2),
                current_cardinality.rows,
                BTreeMap::from([(
                    "selectivity_assumption".to_owned(),
                    PlanDetail::Text("0.5_no_histogram".to_owned()),
                )]),
            )
        } else {
            (
                disjunction_cardinality(current_cardinality, disjunct_count),
                current_cardinality
                    .rows
                    .and_then(|rows| rows.checked_mul(disjunct_count)),
                BTreeMap::from([
                    (
                        "selectivity_assumption".to_owned(),
                        PlanDetail::Text("0.5_per_disjunct_independent_or".to_owned()),
                    ),
                    (
                        "disjunct_count".to_owned(),
                        PlanDetail::Integer(disjunct_count),
                    ),
                ]),
            )
        };
        if current_cardinality.rows.is_some() && work.is_none() {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Filter,
            Some(predicate.clone()),
            details,
            cardinality,
            work,
        );
        current_cardinality = cardinality;
        if post_expansion_conjunct.is_some() {
            for limit in limits_between_disjunct_and_conjunct {
                let cardinality = limit_cardinality(current_cardinality, *limit);
                let work = current_cardinality.rows;
                current = push_unary(
                    &mut operators,
                    current,
                    PlanNodeKind::Limit,
                    None,
                    BTreeMap::from([("limit".to_owned(), PlanDetail::Integer(*limit))]),
                    cardinality,
                    work,
                );
                current_cardinality = cardinality;
            }

            let conjunct_count = conjunct_count.expect("nested disjunct limits require conjuncts");
            let (cardinality, work) = conjunctive_disjunction_cardinality_and_work(
                current_cardinality,
                1,
                conjunct_count,
            );
            let mut details = BTreeMap::from([
                (
                    "selectivity_assumption".to_owned(),
                    PlanDetail::Text("0.5_per_left_to_right_conjunct".to_owned()),
                ),
                (
                    "conjunct_count".to_owned(),
                    PlanDetail::Integer(conjunct_count),
                ),
                (
                    "conjunct_order".to_owned(),
                    PlanDetail::Text("left_to_right_short_circuit".to_owned()),
                ),
            ]);
            if current_cardinality.rows.is_some() && work.is_none() {
                record_work_overflow(&mut details);
            }
            current = push_unary(
                &mut operators,
                current,
                PlanNodeKind::Filter,
                Some(
                    post_expansion_conjunct
                        .expect("nested disjunct limits require a conjunct predicate")
                        .clone(),
                ),
                details,
                cardinality,
                work,
            );
            current_cardinality = cardinality;
        }
    }
    for (storm_index, storm) in disjunct_storms.iter().enumerate() {
        let storm_index = u64::try_from(storm_index + 1).map_err(|_| ExplainError::TooManyNodes)?;
        for limit in &storm.nested_branch_limits {
            let cardinality = limit_cardinality(current_cardinality, *limit);
            let work = current_cardinality
                .rows
                .and_then(|rows| rows.checked_mul(storm.disjunct_count));
            let mut details = BTreeMap::from([
                ("limit".to_owned(), PlanDetail::Integer(*limit)),
                (
                    "limit_scope".to_owned(),
                    PlanDetail::Text("per_disjunct_branch".to_owned()),
                ),
                (
                    "disjunct_count".to_owned(),
                    PlanDetail::Integer(storm.disjunct_count),
                ),
                (
                    "disjunct_storm".to_owned(),
                    PlanDetail::Integer(storm_index),
                ),
            ]);
            if current_cardinality.rows.is_some() && work.is_none() {
                record_work_overflow(&mut details);
            }
            current = push_unary(
                &mut operators,
                current,
                PlanNodeKind::Limit,
                None,
                details,
                cardinality,
                work,
            );
            current_cardinality = cardinality;
        }
        let (cardinality, work) = conjunctive_disjunction_cardinality_and_work(
            current_cardinality,
            storm.disjunct_count,
            storm.conjunct_count_per_disjunct,
        );
        let mut details = BTreeMap::from([
            (
                "selectivity_assumption".to_owned(),
                PlanDetail::Text(
                    "0.5_per_branch_conjunct_then_independent_disjuncts".to_owned(),
                ),
            ),
            (
                "disjunct_count".to_owned(),
                PlanDetail::Integer(storm.disjunct_count),
            ),
            (
                "conjunct_count_per_disjunct".to_owned(),
                PlanDetail::Integer(storm.conjunct_count_per_disjunct),
            ),
            (
                "conjunct_order".to_owned(),
                PlanDetail::Text("left_to_right_short_circuit".to_owned()),
            ),
            (
                "expansion_work".to_owned(),
                PlanDetail::Text("full_input_per_disjunct_after_branch_limits".to_owned()),
            ),
            (
                "disjunct_storm".to_owned(),
                PlanDetail::Integer(storm_index),
            ),
        ]);
        if current_cardinality.rows.is_some() && work.is_none() {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Filter,
            Some(storm.predicate.clone()),
            details,
            cardinality,
            work,
        );
        current_cardinality = cardinality;
    }
    let mut prior_storm_stage_path = Vec::new();
    for (storm_index, storm) in disjunct_storm_cascades.iter().enumerate() {
        let storm_stage_index = storm_index + 1;
        let storm_index =
            u64::try_from(storm_stage_index).map_err(|_| ExplainError::TooManyNodes)?;
        let branch_count =
            u64::try_from(storm.branches.len()).map_err(|_| ExplainError::TooManyNodes)?;
        let mut byte_cap_handoff_estimates_by_depth = BTreeMap::new();
        let mut storm_scope_path = prior_storm_stage_path.clone();
        storm_scope_path.push(PlanByteCapScopeSegment::StormStage {
            index: storm_stage_index,
        });
        let (cardinality, work, overflowed) = disjunct_storm_cascade_cardinality_and_work(
            current_cardinality,
            storm,
            1,
            &storm_scope_path,
            &mut byte_cap_handoff_estimates_by_depth,
        );
        stabilize_rebind_byte_cap_handoff_routes(&mut byte_cap_handoff_estimates_by_depth);
        let branch_limits = storm
            .branches
            .iter()
            .enumerate()
            .map(|(index, branch)| {
                format!(
                    "{}:[{}]",
                    index + 1,
                    branch
                        .nested_limits
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(";");
        let conjunct_counts = storm
            .branches
            .iter()
            .map(|branch| branch.conjunct_count.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let nested_cascade_shapes = storm
            .branches
            .iter()
            .enumerate()
            .filter(|(_, branch)| !branch.nested_storms.is_empty())
            .map(|(index, branch)| {
                format!(
                    "{}:{}",
                    index + 1,
                    branch
                        .nested_storms
                        .iter()
                        .map(disjunct_storm_shape_text)
                        .collect::<Vec<_>>()
                        .join(">")
                )
            })
            .collect::<Vec<_>>()
            .join(";");
        let nested_cascade_predicates = storm
            .branches
            .iter()
            .flat_map(|branch| disjunct_storm_predicates(&branch.nested_storms))
            .collect::<Vec<_>>();
        let has_nested_cascades = storm
            .branches
            .iter()
            .any(|branch| !branch.nested_storms.is_empty());
        let limit_chain_rebind_shapes = storm
            .branches
            .iter()
            .enumerate()
            .flat_map(|(branch_index, branch)| {
                branch.limit_rebinds.iter().map(move |rebind| {
                    format!(
                        "{}@{}:{}",
                        branch_index + 1,
                        rebind.after_limit,
                        rebind
                            .storms
                            .iter()
                            .map(disjunct_storm_shape_text)
                            .collect::<Vec<_>>()
                            .join(">")
                    )
                })
            })
            .collect::<Vec<_>>()
            .join(";");
        let limit_chain_rebind_byte_cap_handoffs_by_depth =
            disjunct_storm_rebind_cascade_counts_by_depth(storm);
        let limit_chain_rebind_byte_cap_depths = limit_chain_rebind_byte_cap_handoffs_by_depth
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let limit_chain_rebind_max_nested_depth = limit_chain_rebind_byte_cap_depths
            .last()
            .copied()
            .unwrap_or_default();
        let limit_chain_rebind_byte_cap_depths_text = limit_chain_rebind_byte_cap_depths
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let limit_chain_rebind_byte_cap_handoffs_by_depth_text =
            limit_chain_rebind_byte_cap_handoffs_by_depth
                .iter()
                .map(|(depth, count)| format!("{depth}:{count}"))
                .collect::<Vec<_>>()
                .join(",");
        let limit_chain_rebind_byte_cap_handoff_estimates_by_depth_text =
            rebind_byte_cap_handoff_estimates_by_depth_text(
                &byte_cap_handoff_estimates_by_depth,
            );
        let limit_chain_rebind_byte_cap_handoff_route_records =
            rebind_byte_cap_handoff_route_records(&byte_cap_handoff_estimates_by_depth);
        // Derive both scoped handoff summaries from the serialized typed routes
        // so their stage-output labels cannot diverge from route ancestry.
        let limit_chain_rebind_byte_cap_handoff_scopes_by_depth_text =
            rebind_byte_cap_handoff_scopes_by_depth_text(
                &limit_chain_rebind_byte_cap_handoff_route_records,
            );
        let limit_chain_rebind_byte_cap_handoff_routes_by_depth_text =
            rebind_byte_cap_handoff_routes_by_depth_text(
                &limit_chain_rebind_byte_cap_handoff_route_records,
            );
        let has_limit_chain_rebind_byte_cap_handoff_routes =
            !limit_chain_rebind_byte_cap_handoff_route_records.is_empty();
        let limit_chain_rebind_predicates = storm
            .branches
            .iter()
            .flat_map(|branch| {
                branch
                    .limit_rebinds
                    .iter()
                    .flat_map(|rebind| disjunct_storm_predicates(&rebind.storms))
            })
            .collect::<Vec<_>>();
        let mut details = BTreeMap::from([
            (
                "selectivity_assumption".to_owned(),
                PlanDetail::Text("0.5_per_branch_conjunct_then_ordered_independent_or".to_owned()),
            ),
            ("disjunct_count".to_owned(), PlanDetail::Integer(branch_count)),
            (
                "branch_limit_chains".to_owned(),
                PlanDetail::Text(branch_limits),
            ),
            (
                "branch_conjunct_counts".to_owned(),
                PlanDetail::Text(conjunct_counts),
            ),
            (
                "nested_cascade_shapes".to_owned(),
                PlanDetail::Text(nested_cascade_shapes),
            ),
            (
                "nested_cascade_predicates".to_owned(),
                PlanDetail::Expressions(nested_cascade_predicates),
            ),
            (
                "limit_chain_rebind_shapes".to_owned(),
                PlanDetail::Text(limit_chain_rebind_shapes),
            ),
            (
                "limit_chain_rebind_max_nested_depth".to_owned(),
                PlanDetail::Integer(
                    u64::try_from(limit_chain_rebind_max_nested_depth).unwrap_or(u64::MAX),
                ),
            ),
            (
                "limit_chain_rebind_byte_cap_depths".to_owned(),
                PlanDetail::Text(limit_chain_rebind_byte_cap_depths_text),
            ),
            (
                "limit_chain_rebind_byte_cap_handoffs_by_depth".to_owned(),
                PlanDetail::Text(limit_chain_rebind_byte_cap_handoffs_by_depth_text),
            ),
            (
                "limit_chain_rebind_byte_cap_handoff_estimates_by_depth".to_owned(),
                PlanDetail::Text(limit_chain_rebind_byte_cap_handoff_estimates_by_depth_text),
            ),
            (
                "limit_chain_rebind_byte_cap_handoff_scopes_by_depth".to_owned(),
                PlanDetail::Text(limit_chain_rebind_byte_cap_handoff_scopes_by_depth_text),
            ),
            (
                "limit_chain_rebind_byte_cap_handoff_routes_by_depth".to_owned(),
                PlanDetail::Text(limit_chain_rebind_byte_cap_handoff_routes_by_depth_text),
            ),
            (
                "limit_chain_rebind_byte_cap_handoff_route_records".to_owned(),
                PlanDetail::ByteCapHandoffRoutes(
                    limit_chain_rebind_byte_cap_handoff_route_records,
                ),
            ),
            (
                "limit_chain_rebind_predicates".to_owned(),
                PlanDetail::Expressions(limit_chain_rebind_predicates),
            ),
            (
                "storm_stage_input_scope".to_owned(),
                PlanDetail::Text(
                    "query_input_then_previous_stage_bounded_rows_and_bytes".to_owned(),
                ),
            ),
            (
                "limit_chain_rebind_stage_input_scope".to_owned(),
                PlanDetail::Text(
                    "post_limit_branch_input_then_previous_rebind_stage_bounded_rows_and_bytes"
                        .to_owned(),
                ),
            ),
            (
                "limit_chain_rebind_stage_order".to_owned(),
                PlanDetail::Text(
                    "declaration_order_each_rebind_output_capped_before_next_rebind".to_owned(),
                ),
            ),
            (
                "limit_chain_rebind_dimension_scope".to_owned(),
                PlanDetail::Text(
                    "rows_and_bytes_capped_independently_unknown_dimensions_remain_unknown"
                        .to_owned(),
                ),
            ),
            (
                "limit_chain_rebind_work_scope".to_owned(),
                PlanDetail::Text(
                    "unknown_row_work_does_not_erase_known_byte_estimates".to_owned(),
                ),
            ),
            (
                "nested_limit_chain_rebind_byte_cap_scope".to_owned(),
                PlanDetail::Text(
                    "immediate_post_limit_branch_bytes_at_every_rebind_nesting_depth".to_owned(),
                ),
            ),
            (
                "limit_chain_rebind_position_input_scope".to_owned(),
                PlanDetail::Text(
                    "current_branch_rows_and_bytes_after_prior_limits_and_rebind_cascades"
                        .to_owned(),
                ),
            ),
            (
                "branch_local_storm_cap_scope".to_owned(),
                PlanDetail::Text(
                    "each_cascade_output_capped_to_immediate_input_rows_and_bytes_at_every_nesting_depth"
                        .to_owned(),
                ),
            ),
            (
                "limit_chain_rebind_cap_scope".to_owned(),
                PlanDetail::Text(
                    "post_limit_branch_rows_and_bytes_at_every_rebind_nesting_depth".to_owned(),
                ),
            ),
            (
                "expansion_work".to_owned(),
                PlanDetail::Text("sum_of_branch_limit_and_conjunct_inputs".to_owned()),
            ),
            (
                "disjunct_storm".to_owned(),
                PlanDetail::Integer(storm_index),
            ),
        ]);
        if has_nested_cascades {
            details.insert(
                "nested_storm_input_scope".to_owned(),
                PlanDetail::Text(
                    "bounded_branch_output_after_limits_rebinds_and_conjuncts_then_prior_nested_storm_outputs"
                        .to_owned(),
                ),
            );
        }
        if has_limit_chain_rebind_byte_cap_handoff_routes {
            details.insert(
                "limit_chain_rebind_byte_cap_handoff_route_order".to_owned(),
                PlanDetail::Text(
                    "ascending_depth_then_typed_input_path_then_typed_output_path".to_owned(),
                ),
            );
        }
        if overflowed {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Filter,
            Some(storm.predicate.clone()),
            details,
            cardinality,
            work,
        );
        current_cardinality = cardinality;
        prior_storm_stage_path = storm_scope_path;
        prior_storm_stage_path.push(PlanByteCapScopeSegment::StormStageOutput {
            index: storm_stage_index,
        });
    }
    if !query.projections.is_empty() {
        let cardinality = current_cardinality;
        let work = current_cardinality
            .rows
            .and_then(|rows| rows.checked_mul(query.projections.len() as u64));
        let mut details = BTreeMap::from([(
            "expressions".to_owned(),
            PlanDetail::Expressions(query.projections.clone()),
        )]);
        if current_cardinality.rows.is_some() && work.is_none() {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Project,
            None,
            details,
            cardinality,
            work,
        );
    }
    if query.distinct {
        // The reference names no standalone DISTINCT plan kind. Aggregate is
        // the existing 1.0 logical operator for duplicate elimination.
        let cardinality = scale_cardinality(current_cardinality, 1, 2);
        let work = current_cardinality.rows.and_then(|rows| rows.checked_mul(2));
        let mut details = BTreeMap::from([
            (
                "operation".to_owned(),
                PlanDetail::Text("distinct".to_owned()),
            ),
            (
                "selectivity_assumption".to_owned(),
                PlanDetail::Text("0.5_no_histogram".to_owned()),
            ),
        ]);
        if current_cardinality.rows.is_some() && work.is_none() {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Aggregate,
            None,
            details,
            cardinality,
            work,
        );
        current_cardinality = cardinality;
    }
    if !query.ordering.is_empty() {
        let cardinality = current_cardinality;
        let work = current_cardinality.rows.and_then(sort_work);
        let mut details = BTreeMap::from([(
            "keys".to_owned(),
            PlanDetail::Ordering(query.ordering.clone()),
        )]);
        if current_cardinality.rows.is_some() && work.is_none() {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Sort,
            None,
            details,
            cardinality,
            work,
        );
    }
    for limit in query.limit.iter().chain(additional_limits.iter()) {
        let cardinality = limit_cardinality(current_cardinality, *limit);
        let work = current_cardinality.rows;
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Limit,
            None,
            BTreeMap::from([("limit".to_owned(), PlanDetail::Integer(*limit))]),
            cardinality,
            work,
        );
        current_cardinality = cardinality;
    }
    let mut table_rows = BTreeMap::<ObjectRef, Option<u64>>::new();
    table_rows.insert(
        query.source.clone(),
        query.source_statistics.as_ref().and_then(|stats| stats.estimated_rows),
    );
    for join in &query.joins {
        table_rows.insert(
            join.source.clone(),
            join.statistics.as_ref().and_then(|stats| stats.estimated_rows),
        );
    }
    for mutation in &query.mutations {
        let before = mutation
            .estimated_table_rows_before
            .or_else(|| table_rows.get(&mutation.table).copied().flatten());
        let after = mutated_table_rows(mutation.kind, before, mutation.estimated_affected_rows);
        let mut details = BTreeMap::from([
            (
                "mutation".to_owned(),
                PlanDetail::Text(mutation.kind.as_str().to_owned()),
            ),
            (
                "strategy".to_owned(),
                PlanDetail::Text("apply_table_mutation".to_owned()),
            ),
        ]);
        if let Some(before) = before {
            details.insert(
                "table_rows_before".to_owned(),
                PlanDetail::Integer(before),
            );
        }
        if let Some(after) = after {
            details.insert(
                "table_rows_after".to_owned(),
                PlanDetail::Integer(after),
            );
        }
        if let Some(affected) = mutation.estimated_affected_rows {
            details.insert("affected_rows".to_owned(), PlanDetail::Integer(affected));
        }
        if let Some(bytes) = mutation.estimated_write_bytes {
            details.insert("write_bytes".to_owned(), PlanDetail::Integer(bytes));
        }
        let work = mutation_work(
            mutation.estimated_affected_rows,
            mutation.estimated_write_bytes,
        );
        if mutation
            .estimated_affected_rows
            .zip(mutation.estimated_write_bytes)
            .is_some_and(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)).is_none())
        {
            record_work_overflow(&mut details);
        }
        // `sys.PlanNodeKind` has no mutation variant in 1.0. Preserve the
        // portable vocabulary and identify the table-write operation in details.
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Invoke,
            None,
            details,
            Cardinality {
                rows: mutation.estimated_affected_rows,
                bytes: mutation.estimated_write_bytes,
            },
            work,
        );
        current_cardinality = Cardinality {
            rows: mutation.estimated_affected_rows,
            bytes: mutation.estimated_write_bytes,
        };
        operators[current].object = Some(mutation.table.clone());
        table_rows.insert(mutation.table.clone(), after);
    }
    if let Some(target) = &query.materialize_into {
        let work = materialize_work(current_cardinality);
        let mut details = BTreeMap::from([
            (
                "mode".to_owned(),
                PlanDetail::Text("write_through".to_owned()),
            ),
            (
                "fallback".to_owned(),
                PlanDetail::Text("evaluate_query".to_owned()),
            ),
        ]);
        if current_cardinality
            .rows
            .zip(current_cardinality.bytes)
            .is_some_and(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)).is_none())
        {
            record_work_overflow(&mut details);
        }
        current = push_unary(
            &mut operators,
            current,
            PlanNodeKind::Materialize,
            None,
            details,
            current_cardinality,
            work,
        );
        operators[current].object = Some(target.clone());
    }
    if let Some((identity, pair_count, index_count, scan_count)) =
        paired_index_cost_compaction_chain.as_ref()
    {
        add_paired_index_cost_compaction_output_details(
            &mut operators[current].details,
            identity,
            *pair_count,
            *index_count,
            *scan_count,
        );
    }
    if let Some(fold) = paired_checkpoint_cost_restoration_fold.as_ref() {
        add_paired_checkpoint_cost_restoration_output_details(
            &mut operators[current].details,
            fold,
        );
    }
    if let Some(fold) = paired_segment_rotation_cost_restoration_fold.as_ref() {
        add_paired_segment_rotation_cost_restoration_output_details(
            &mut operators[current].details,
            fold,
        );
    }
    if let Some(fold) = paired_stream_compaction_cost_restoration_fold.as_ref() {
        add_paired_stream_compaction_cost_restoration_output_details(
            &mut operators[current].details,
            fold,
        );
    }
    if let Some(fold) = paired_checkpoint_spill_cost_restoration_fold.as_ref() {
        add_paired_checkpoint_spill_cost_restoration_output_details(
            &mut operators[current].details,
            fold,
        );
    }
    if let Some(fold) = paired_stream_rotation_cost_restoration_fold.as_ref() {
        add_paired_stream_rotation_cost_restoration_output_details(
            &mut operators[current].details,
            fold,
        );
    }
    if let Some(fold) = paired_wal_rotation_cost_restoration_fold.as_ref() {
        add_paired_wal_rotation_cost_restoration_output_details(
            &mut operators[current].details,
            fold,
        );
    }
    build_plan(query.snapshot.clone(), operators, current)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Cardinality {
    rows: Option<u64>,
    bytes: Option<u64>,
}

struct RebindByteCapHandoffEstimate {
    input_path: Vec<PlanByteCapScopeSegment>,
    output_path: Vec<PlanByteCapScopeSegment>,
    input_bytes: Option<u64>,
    output_bytes: Option<u64>,
}

type RebindByteCapHandoffEstimatesByDepth =
    BTreeMap<usize, Vec<RebindByteCapHandoffEstimate>>;

fn source_cardinality(statistics: Option<&QuerySourceStatistics>) -> Cardinality {
    statistics.map_or_else(Cardinality::default, |statistics| Cardinality {
        rows: statistics.estimated_rows,
        bytes: statistics.estimated_bytes,
    })
}

/// Selects a deterministic physical order for the right-hand join inputs.
/// ORNA permits internal reordering when results and observable order are
/// preserved, but does not prescribe a cost heuristic. This explain-only
/// adapter ranks fully estimated scans by rows plus 4-KiB byte blocks, then
/// keeps tied and sparse inputs in declaration order. The executor remains
/// responsible for restoring any declared result ordering.
fn planned_query_join_order(joins: &[QueryJoinDescription]) -> Vec<(usize, &QueryJoinDescription)> {
    let mut ordered = joins.iter().enumerate().collect::<Vec<_>>();
    ordered.sort_by_key(|(declared_position, join)| {
        let work = join
            .statistics
            .as_ref()
            .and_then(|statistics| statistics.estimated_rows.zip(statistics.estimated_bytes))
            .map(|(rows, bytes)| u128::from(rows) + u128::from(ceil_div(bytes, 4096)));
        (work.is_none(), work.unwrap_or_default(), *declared_position)
    });
    ordered
}

fn add_decorrelated_subquery_details(
    details: &mut BTreeMap<String, PlanDetail>,
    subquery: &QueryDecorrelatedSubqueryDescription,
) {
    details.insert(
        "subquery_identity".to_owned(),
        PlanDetail::Text(subquery.identity.as_str().to_owned()),
    );
    details.insert(
        "correlation_predicate_identity".to_owned(),
        PlanDetail::Text(subquery.correlation_predicate.as_str().to_owned()),
    );
    details.insert(
        "subquery_decorrelation".to_owned(),
        PlanDetail::Text("resolver_approved_predicate_join".to_owned()),
    );
}

fn join_pair_identity_for_join<'a>(
    join: &QueryJoinDescription,
    pairs: &'a [QueryJoinPairIdentityDescription],
) -> Option<&'a QueryJoinPairIdentityDescription> {
    pairs.iter().find(|pair| {
        pair.right_source == join.source && pair.predicate == join.predicate
    })
}

fn add_join_pair_identity_details(
    details: &mut BTreeMap<String, PlanDetail>,
    pair: &QueryJoinPairIdentityDescription,
) {
    details.insert(
        "join_pair_identity".to_owned(),
        PlanDetail::Text(pair.identity.as_str().to_owned()),
    );
    details.insert(
        "logical_left_source_identity".to_owned(),
        PlanDetail::Text(pair.left_source.as_str().to_owned()),
    );
    details.insert(
        "logical_right_source_identity".to_owned(),
        PlanDetail::Text(pair.right_source.as_str().to_owned()),
    );
    details.insert(
        "join_pair_identity_resolution".to_owned(),
        PlanDetail::Text("exact_right_source_and_predicate".to_owned()),
    );
    if let Some(predicate) = &pair.predicate {
        details.insert(
            "logical_predicate_identity".to_owned(),
            PlanDetail::Text(predicate.as_str().to_owned()),
        );
    }
}

fn push_query_limit_pushdowns(
    operators: &mut Vec<Operator>,
    mut input: usize,
    source: &ObjectRef,
    mut cardinality: Cardinality,
    pair: Option<&QueryJoinPairIdentityDescription>,
    limit_pushdowns: &[QueryLimitPushdownDescription],
) -> (usize, Cardinality, Option<String>) {
    let Some(pair) = pair else {
        return (input, cardinality, None);
    };
    let Some(chain_identity) = query_limit_pushdown_chain_identity(pair, source, limit_pushdowns)
    else {
        return (input, cardinality, None);
    };

    for (position, pushdown) in limit_pushdowns
        .iter()
        .filter(|pushdown| {
            pushdown.join_pair_identity == pair.identity && pushdown.source == *source
        })
        .enumerate()
    {
        let next_cardinality = limit_cardinality(cardinality, pushdown.limit);
        let mut details = BTreeMap::from([
            (
                "limit".to_owned(),
                PlanDetail::Integer(pushdown.limit),
            ),
            (
                "limit_pushdown_identity".to_owned(),
                PlanDetail::Text(pushdown.identity.as_str().to_owned()),
            ),
            (
                "limit_pushdown_pair_identity".to_owned(),
                PlanDetail::Text(pair.identity.as_str().to_owned()),
            ),
            (
                "limit_pushdown_source_identity".to_owned(),
                PlanDetail::Text(source.as_str().to_owned()),
            ),
            (
                "limit_pushdown_chain_position".to_owned(),
                PlanDetail::Integer(u64::try_from(position + 1).unwrap_or(u64::MAX)),
            ),
        ]);
        add_query_limit_pushdown_chain_details(&mut details, &chain_identity);
        input = push_unary(
            operators,
            input,
            PlanNodeKind::Limit,
            None,
            details,
            next_cardinality,
            cardinality.rows,
        );
        cardinality = next_cardinality;
    }
    (input, cardinality, Some(chain_identity))
}

fn query_limit_pushdown_chain_identity(
    pair: &QueryJoinPairIdentityDescription,
    source: &ObjectRef,
    limit_pushdowns: &[QueryLimitPushdownDescription],
) -> Option<String> {
    let matching = limit_pushdowns
        .iter()
        .filter(|pushdown| {
            pushdown.join_pair_identity == pair.identity && pushdown.source == *source
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-limit-pushdown-chain.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, source.as_str().as_bytes());
    hash.update((matching.len() as u64).to_be_bytes());
    for pushdown in matching {
        hash_part(&mut hash, pushdown.identity.as_str().as_bytes());
        hash.update(pushdown.limit.to_be_bytes());
    }
    Some(format!("limit-pushdown-chain:{}", hex(&hash.finalize())))
}

fn query_limit_pushdown_bound_rows(
    pair: &QueryJoinPairIdentityDescription,
    source: &ObjectRef,
    limit_pushdowns: &[QueryLimitPushdownDescription],
) -> Option<u64> {
    limit_pushdowns
        .iter()
        .filter(|pushdown| {
            pushdown.join_pair_identity == pair.identity && pushdown.source == *source
        })
        .map(|pushdown| pushdown.limit)
        .min()
}

fn add_query_limit_pushdown_chain_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "limit_pushdown_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "limit_pushdown_chain_policy".to_owned(),
        PlanDetail::Text("resolver_ordered_per_exact_join_pair".to_owned()),
    );
}

fn push_window_aggregates(
    operators: &mut Vec<Operator>,
    mut input: usize,
    source: &ObjectRef,
    cardinality: Cardinality,
    aggregates: &[QueryWindowAggregatePushdownDescription],
) -> usize {
    let matching = aggregates
        .iter()
        .filter(|aggregate| aggregate.source == *source)
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return input;
    }
    let chain_identity = query_window_pushdown_chain_identity(source, aggregates)
        .expect("a nonempty matching window chain has an identity");
    let chain_length = u64::try_from(matching.len()).unwrap_or(u64::MAX);
    for (position, aggregate) in matching.into_iter().enumerate() {
        let details = BTreeMap::from([
            (
                "operation".to_owned(),
                PlanDetail::Text("window_aggregate".to_owned()),
            ),
            (
                "window_aggregate_identity".to_owned(),
                PlanDetail::Text(aggregate.identity.as_str().to_owned()),
            ),
            (
                "window_source_identity".to_owned(),
                PlanDetail::Text(aggregate.source.as_str().to_owned()),
            ),
            (
                "window_frame_identity".to_owned(),
                PlanDetail::Text(aggregate.frame_identity.as_str().to_owned()),
            ),
            (
                "window_frame_start".to_owned(),
                PlanDetail::Text(aggregate.frame_start.as_detail()),
            ),
            (
                "window_frame_end".to_owned(),
                PlanDetail::Text(aggregate.frame_end.as_detail()),
            ),
            (
                "window_pushdown_chain_identity".to_owned(),
                PlanDetail::Text(chain_identity.clone()),
            ),
            (
                "window_pushdown_chain_position".to_owned(),
                PlanDetail::Integer(u64::try_from(position + 1).unwrap_or(u64::MAX)),
            ),
            (
                "window_pushdown_chain_length".to_owned(),
                PlanDetail::Integer(chain_length),
            ),
            (
                "window_pushdown_identity_policy".to_owned(),
                PlanDetail::Text("source_ordered_sha256_v1".to_owned()),
            ),
            (
                "pushdown_policy".to_owned(),
                PlanDetail::Text("exact_source_identity_before_join".to_owned()),
            ),
            (
                "estimated_work_scope".to_owned(),
                PlanDetail::Text("one_unit_per_input_row".to_owned()),
            ),
        ]);
        let index = operators.len();
        operators.push(Operator::new(
            PlanNodeKind::Aggregate,
            Some(aggregate.identity.clone()),
            Some(aggregate.aggregate.clone()),
            details,
            vec![input],
            cardinality,
            cardinality.rows,
        ));
        input = index;
    }
    input
}

fn partial_index_for_join<'a>(
    join: &QueryJoinDescription,
    candidates: &'a [QueryPartialIndexDescription],
) -> Option<&'a QueryPartialIndexDescription> {
    let predicate = join.predicate.as_ref()?;
    // The reference names explain node kinds but is silent on partial-index
    // matching and tie-breaking. This adapter requires an exact table/predicate
    // match, then selects the lexicographically smallest index identity so the
    // explain-only choice is independent of catalog order. A missing exact
    // match stays a scan rather than inferring predicate implication.
    candidates
        .iter()
        .filter(|candidate| {
            candidate.table == join.source && candidate.partial_predicate == *predicate
        })
        .min_by(|left, right| left.index.as_str().cmp(right.index.as_str()))
}

fn push_index_lookup(
    operators: &mut Vec<Operator>,
    candidate: &QueryPartialIndexDescription,
    statistics: Option<&QuerySourceStatistics>,
) -> usize {
    let cardinality = source_cardinality(statistics);
    let mut details = BTreeMap::from([
        (
            "table".to_owned(),
            PlanDetail::Text(candidate.table.as_str().to_owned()),
        ),
        (
            "partial_predicate_identity".to_owned(),
            PlanDetail::Text(candidate.partial_predicate.as_str().to_owned()),
        ),
        (
            "index_selection".to_owned(),
            PlanDetail::Text("exact_table_and_predicate_identity".to_owned()),
        ),
    ]);
    add_partial_index_pair_identity_details(&mut details, candidate);
    add_partial_index_selection_tie_break_details(&mut details);
    if let Some(branch) = statistics.and_then(|stats| stats.mutable_branch.as_ref()) {
        details.insert(
            "mutable_branch".to_owned(),
            PlanDetail::Text(branch.name.clone()),
        );
        details.insert(
            "branch_generation".to_owned(),
            PlanDetail::Integer(branch.generation),
        );
        details.insert(
            "statistics_scope".to_owned(),
            PlanDetail::Text("overlay_inclusive".to_owned()),
        );
    }
    let work = cardinality
        .rows
        .zip(cardinality.bytes)
        .and_then(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)));
    if cardinality
        .rows
        .zip(cardinality.bytes)
        .is_some_and(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)).is_none())
    {
        record_work_overflow(&mut details);
    }
    let index = operators.len();
    operators.push(Operator::new(
        PlanNodeKind::IndexLookup,
        Some(candidate.index.clone()),
        Some(candidate.partial_predicate.clone()),
        details,
        Vec::new(),
        cardinality,
        work,
    ));
    index
}

/// Returns the stable identity of the exact logical predicate pushdown and
/// selected index pair. ORNA specifies the `index_lookup` node kind but does
/// not prescribe its identity encoding, so the explain adapter hashes the
/// table/index/predicate tuple. Cost reordering may move its join fold, but
/// cannot change this value while that tuple stays fixed.
fn partial_index_pair_identity(candidate: &QueryPartialIndexDescription) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.partial-index-pair.v1\0");
    hash_part(&mut hash, candidate.table.as_str().as_bytes());
    hash_part(&mut hash, candidate.index.as_str().as_bytes());
    hash_part(&mut hash, candidate.partial_predicate.as_str().as_bytes());
    format!("index-pair:{}", hex(&hash.finalize()))
}

/// Binds a logical join pair to the exact partial index selected for its
/// right source and predicate. ORNA leaves this combined planner identity
/// encoding open, so use a domain-separated deterministic digest.
fn paired_predicate_pushdown_identity(
    pair: &QueryJoinPairIdentityDescription,
    candidate: &QueryPartialIndexDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.join-predicate-pushdown-pair.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(
        &mut hash,
        partial_index_pair_identity(candidate).as_bytes(),
    );
    format!("join-predicate-pair:{}", hex(&hash.finalize()))
}

/// Records the selected exact index tuple, or a typed no-match outcome, for a
/// logical pair. ORNA leaves explain-only index identity encoding open; this
/// value keeps sparse pair slots from disappearing during later cost refolds.
fn paired_index_selection_identity(
    pair: &QueryJoinPairIdentityDescription,
    candidate: Option<&QueryPartialIndexDescription>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.paired-index-selection.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    if let Some(candidate) = candidate {
        hash.update([1]);
        hash_part(
            &mut hash,
            partial_index_pair_identity(candidate).as_bytes(),
        );
    } else {
        hash.update([0]);
    }
    format!("paired-index-selection:{}", hex(&hash.finalize()))
}

fn add_paired_index_selection_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_index_selection_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_index_selection_policy".to_owned(),
        PlanDetail::Text("exact_pair_index_or_no_match".to_owned()),
    );
}

fn add_paired_index_refold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    selection_identity: &str,
) {
    details.insert(
        "paired_index_refold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_index_refold_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_index_refold_input_identity".to_owned(),
        PlanDetail::Text(input_identity.to_owned()),
    );
    details.insert(
        "paired_index_refold_pair_identity".to_owned(),
        PlanDetail::Text(pair.identity.as_str().to_owned()),
    );
    details.insert(
        "paired_index_refold_selection_identity".to_owned(),
        PlanDetail::Text(selection_identity.to_owned()),
    );
    details.insert(
        "paired_index_refold_pairing".to_owned(),
        PlanDetail::Text("sparse_parent_fold_pair_and_exact_index_outcome".to_owned()),
    );
}

fn add_paired_index_anchor_chain_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    step_refold_identity: Option<&str>,
    step_selection_identity: Option<&str>,
) {
    details.insert(
        "paired_index_anchor_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_index_anchor_chain_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_index_anchor_chain_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "paired_index_anchor_chain_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    if let Some(pair_identity) = pair_identity {
        details.insert(
            "paired_index_anchor_chain_pair_identity".to_owned(),
            PlanDetail::Text(pair_identity.to_owned()),
        );
    }
    if let Some(refold_identity) = step_refold_identity {
        details.insert(
            "paired_index_anchor_chain_step_refold_identity".to_owned(),
            PlanDetail::Text(refold_identity.to_owned()),
        );
    }
    if let Some(selection_identity) = step_selection_identity {
        details.insert(
            "paired_index_anchor_chain_step_selection_identity".to_owned(),
            PlanDetail::Text(selection_identity.to_owned()),
        );
    }
}

fn add_paired_index_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    parent_cost_identity: &str,
    declared_position: usize,
    planned_position: usize,
    transition: &str,
    pair_identity: Option<&str>,
    step_refold_identity: Option<&str>,
    step_selection_identity: Option<&str>,
    has_index: Option<bool>,
) {
    details.insert(
        "paired_index_cost_restoration_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_restoration_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_restoration_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_index_cost_restoration_declared_input_position".to_owned(),
        PlanDetail::Integer(u64::try_from(declared_position + 1).unwrap_or(u64::MAX)),
    );
    details.insert(
        "paired_index_cost_restoration_planned_input_position".to_owned(),
        PlanDetail::Integer(u64::try_from(planned_position + 1).unwrap_or(u64::MAX)),
    );
    if let Some(pair_identity) = pair_identity {
        details.insert(
            "paired_index_cost_restoration_pair_identity".to_owned(),
            PlanDetail::Text(pair_identity.to_owned()),
        );
    }
    if let Some(identity) = step_refold_identity {
        details.insert(
            "paired_index_cost_restoration_step_refold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = step_selection_identity {
        details.insert(
            "paired_index_cost_restoration_step_selection_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(has_index) = has_index {
        details.insert(
            "paired_index_cost_restoration_outcome".to_owned(),
            PlanDetail::Text(if has_index { "exact_index" } else { "scan" }.to_owned()),
        );
    }
}

fn add_paired_index_cost_compaction_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    parent_cost_identity: &str,
    declared_position: usize,
    planned_position: usize,
    transition: &str,
    pair_identity: Option<&str>,
    selection_identity: Option<&str>,
    refold_identity: Option<&str>,
    route_outcome: Option<&str>,
    pair_count: u64,
    index_count: u64,
    scan_count: u64,
) {
    details.insert(
        "paired_index_cost_compaction_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_compaction_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_compaction_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_compaction_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_index_cost_compaction_declared_input_position".to_owned(),
        PlanDetail::Integer(u64::try_from(declared_position + 1).unwrap_or(u64::MAX)),
    );
    details.insert(
        "paired_index_cost_compaction_planned_input_position".to_owned(),
        PlanDetail::Integer(u64::try_from(planned_position + 1).unwrap_or(u64::MAX)),
    );
    details.insert(
        "paired_index_cost_compaction_pair_count".to_owned(),
        PlanDetail::Integer(pair_count),
    );
    details.insert(
        "paired_index_cost_compaction_exact_index_count".to_owned(),
        PlanDetail::Integer(index_count),
    );
    details.insert(
        "paired_index_cost_compaction_scan_count".to_owned(),
        PlanDetail::Integer(scan_count),
    );
    details.insert(
        "paired_index_cost_compaction_policy".to_owned(),
        PlanDetail::Text(
            "bounded_pair_route_digest_with_sparse_fold_carry".to_owned(),
        ),
    );
    if let Some(pair_identity) = pair_identity {
        details.insert(
            "paired_index_cost_compaction_pair_identity".to_owned(),
            PlanDetail::Text(pair_identity.to_owned()),
        );
    }
    if let Some(selection_identity) = selection_identity {
        details.insert(
            "paired_index_cost_compaction_step_selection_identity".to_owned(),
            PlanDetail::Text(selection_identity.to_owned()),
        );
    }
    if let Some(refold_identity) = refold_identity {
        details.insert(
            "paired_index_cost_compaction_step_refold_identity".to_owned(),
            PlanDetail::Text(refold_identity.to_owned()),
        );
    }
    if let Some(route_outcome) = route_outcome {
        details.insert(
            "paired_index_cost_compaction_route_outcome".to_owned(),
            PlanDetail::Text(route_outcome.to_owned()),
        );
    }
}

fn add_paired_index_cost_compaction_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    pair_count: u64,
    index_count: u64,
    scan_count: u64,
) {
    details.insert(
        "paired_index_cost_compaction_output_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_index_cost_compaction_output_pair_count".to_owned(),
        PlanDetail::Integer(pair_count),
    );
    details.insert(
        "paired_index_cost_compaction_output_exact_index_count".to_owned(),
        PlanDetail::Integer(index_count),
    );
    details.insert(
        "paired_index_cost_compaction_output_scan_count".to_owned(),
        PlanDetail::Integer(scan_count),
    );
    details.insert(
        "paired_index_cost_compaction_output_policy".to_owned(),
        PlanDetail::Text("retain_route_digest_and_counts_on_plan_output".to_owned()),
    );
}

fn add_paired_decorrelation_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    parent_cost_identity: &str,
    declared_position: usize,
    planned_position: usize,
    transition: &str,
    pair: Option<&QueryJoinPairIdentityDescription>,
    refold_identity: Option<&str>,
    selection_identity: Option<&str>,
    has_index: Option<bool>,
    subquery: Option<&QueryDecorrelatedSubqueryDescription>,
    pin_chain_identity: Option<&str>,
    anchor_fold_identity: Option<&str>,
    omission_refold_identity: Option<&str>,
) {
    details.insert(
        "paired_decorrelation_cost_restoration_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_decorrelation_cost_restoration_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_decorrelation_cost_restoration_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "paired_decorrelation_cost_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_decorrelation_cost_restoration_declared_input_position".to_owned(),
        PlanDetail::Integer(u64::try_from(declared_position + 1).unwrap_or(u64::MAX)),
    );
    details.insert(
        "paired_decorrelation_cost_restoration_planned_input_position".to_owned(),
        PlanDetail::Integer(u64::try_from(planned_position + 1).unwrap_or(u64::MAX)),
    );
    details.insert(
        "paired_decorrelation_cost_restoration_pairing".to_owned(),
        PlanDetail::Text("paired_routes_and_decorrelated_pins_across_sparse_cost_folds".to_owned()),
    );
    if let Some(pair) = pair {
        details.insert(
            "paired_decorrelation_cost_restoration_pair_identity".to_owned(),
            PlanDetail::Text(pair.identity.as_str().to_owned()),
        );
    }
    if let Some(identity) = refold_identity {
        details.insert(
            "paired_decorrelation_cost_restoration_step_refold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = selection_identity {
        details.insert(
            "paired_decorrelation_cost_restoration_step_selection_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(has_index) = has_index {
        details.insert(
            "paired_decorrelation_cost_restoration_route_outcome".to_owned(),
            PlanDetail::Text(if has_index { "exact_index" } else { "scan" }.to_owned()),
        );
    }
    if let Some(subquery) = subquery {
        details.insert(
            "paired_decorrelation_cost_restoration_subquery_identity".to_owned(),
            PlanDetail::Text(subquery.identity.as_str().to_owned()),
        );
    }
    if let Some(identity) = pin_chain_identity {
        details.insert(
            "paired_decorrelation_cost_restoration_pin_chain_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = anchor_fold_identity {
        details.insert(
            "paired_decorrelation_cost_restoration_anchor_fold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = omission_refold_identity {
        details.insert(
            "paired_decorrelation_cost_restoration_omission_refold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
}

fn add_paired_predicate_pushdown_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_predicate_pushdown_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_predicate_pushdown_policy".to_owned(),
        PlanDetail::Text("logical_join_and_exact_table_index_predicate".to_owned()),
    );
}

/// Combines the resolver-approved subquery/correlation tuple with the exact
/// selected partial-index tuple. The reference leaves this combined identity
/// encoding open, so use a domain-separated deterministic digest.
fn decorrelated_predicate_pushdown_identity(
    subquery: &QueryDecorrelatedSubqueryDescription,
    candidate: &QueryPartialIndexDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.decorrelated-predicate-pushdown.v1\0");
    hash_part(&mut hash, subquery.identity.as_str().as_bytes());
    hash_part(&mut hash, subquery.source.as_str().as_bytes());
    hash_part(
        &mut hash,
        subquery.correlation_predicate.as_str().as_bytes(),
    );
    hash_part(
        &mut hash,
        partial_index_pair_identity(candidate).as_bytes(),
    );
    format!("decorrelated-pushdown:{}", hex(&hash.finalize()))
}

/// Gives an unindexed resolver-approved subquery a stable identity. The
/// reference does not define an identity for this explain-only scan case, so
/// bind the exact omitted source/predicate tuple to the subquery and preserve
/// that omission in later sparse cost folds.
fn decorrelated_index_selection_omission_identity(
    subquery: &QueryDecorrelatedSubqueryDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.decorrelated-index-omission.v1\0");
    hash_part(&mut hash, subquery.identity.as_str().as_bytes());
    hash_part(&mut hash, subquery.source.as_str().as_bytes());
    hash_part(
        &mut hash,
        subquery.correlation_predicate.as_str().as_bytes(),
    );
    format!("decorrelated-index-omission:{}", hex(&hash.finalize()))
}

/// Binds each decorrelated fold to its exact sparse index outcome. The
/// reference does not define this planner-local identity; keep indexed and
/// omitted inputs distinct while retaining the subquery tuple in either case.
fn decorrelated_index_selection_fold_identity(
    subquery: &QueryDecorrelatedSubqueryDescription,
    selected_index: Option<&QueryPartialIndexDescription>,
    omission_identity: Option<&str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.decorrelated-index-selection-fold.v1\0");
    hash_part(&mut hash, subquery.identity.as_str().as_bytes());
    hash_part(&mut hash, subquery.source.as_str().as_bytes());
    hash_part(
        &mut hash,
        subquery.correlation_predicate.as_str().as_bytes(),
    );
    match (selected_index, omission_identity) {
        (Some(index), None) => {
            hash.update([1]);
            hash_part(&mut hash, partial_index_pair_identity(index).as_bytes());
        }
        (None, Some(identity)) => {
            hash.update([0]);
            hash_part(&mut hash, identity.as_bytes());
        }
        (Some(index), Some(identity)) => {
            // Retain both inputs if a future planner path reports an omission
            // alongside a selected index instead of silently collapsing them.
            hash.update([2]);
            hash_part(&mut hash, partial_index_pair_identity(index).as_bytes());
            hash_part(&mut hash, identity.as_bytes());
        }
        (None, None) => {
            // This is unreachable for current decorrelated inputs, which
            // always carry either an exact selected index or an omission.
            hash.update([0]);
            hash.update([0]);
        }
    }
    format!("decorrelated-index-fold:{}", hex(&hash.finalize()))
}

/// Refolds an omitted decorrelated index outcome against its sparse cost
/// ancestry. ORNA leaves this explain-only identity encoding open; retaining
/// the omission tuple here prevents later decorrelated folds from shifting
/// across an unindexed input.
fn query_decorrelated_omission_refold_identity(
    parent_identity: &str,
    input_identity: &str,
    subquery: &QueryDecorrelatedSubqueryDescription,
    omission_identity: &str,
    selection_identity: &str,
    cardinality: Cardinality,
    work: Option<u64>,
    work_overflow: bool,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-decorrelated-omission-refold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, subquery.identity.as_str().as_bytes());
    hash_part(&mut hash, subquery.source.as_str().as_bytes());
    hash_part(
        &mut hash,
        subquery.correlation_predicate.as_str().as_bytes(),
    );
    hash_part(&mut hash, omission_identity.as_bytes());
    hash_part(&mut hash, selection_identity.as_bytes());
    hash_optional_u64(&mut hash, cardinality.rows);
    hash_optional_u64(&mut hash, cardinality.bytes);
    hash_optional_u64(&mut hash, work);
    hash.update([u8::from(work_overflow)]);
    format!("decorrelated-omission-refold:{}", hex(&hash.finalize()))
}

/// Starts a snapshot-pinned chain for resolver-approved decorrelated inputs.
fn query_decorrelated_pin_chain_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-decorrelated-pin-chain-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!("decorrelated-pin-chain:{}", hex(&hash.finalize()))
}

/// Accumulates pinned subquery outcomes across sparse cost folds. ORNA leaves
/// this explain-only encoding open; include the snapshot, resolver tuple,
/// selected-or-omitted index fold, and accumulated parent so rebinds cannot
/// detach a later subquery from its earlier pin ancestry.
fn query_decorrelated_pin_chain_identity(
    parent_pin_identity: &str,
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
    input_identity: &str,
    subquery: &QueryDecorrelatedSubqueryDescription,
    selection_identity: &str,
    anchor_fold_identity: &str,
    omission_refold_identity: Option<&str>,
    cardinality: Cardinality,
    work: Option<u64>,
    work_overflow: bool,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-decorrelated-pin-chain.v1\0");
    hash_part(&mut hash, parent_pin_identity.as_bytes());
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, subquery.identity.as_str().as_bytes());
    hash_part(&mut hash, subquery.source.as_str().as_bytes());
    hash_part(
        &mut hash,
        subquery.correlation_predicate.as_str().as_bytes(),
    );
    hash_part(&mut hash, selection_identity.as_bytes());
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_optional_text(&mut hash, omission_refold_identity);
    hash_optional_u64(&mut hash, cardinality.rows);
    hash_optional_u64(&mut hash, cardinality.bytes);
    hash_optional_u64(&mut hash, work);
    hash.update([u8::from(work_overflow)]);
    format!("decorrelated-pin-chain:{}", hex(&hash.finalize()))
}

/// Binds a resolver-approved decorrelation to the sparse fold accumulated on
/// its lateral anchor. The reference leaves explain identity encoding open;
/// this domain-separated digest prevents equal child subqueries under distinct
/// anchors from being reported as the same paired fold.
fn query_decorrelated_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    subquery: &QueryDecorrelatedSubqueryDescription,
    predicate_pushdown_identity: Option<&str>,
    index_selection_fold_identity: Option<&str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.decorrelated-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, subquery.identity.as_str().as_bytes());
    hash_part(&mut hash, subquery.source.as_str().as_bytes());
    hash_part(
        &mut hash,
        subquery.correlation_predicate.as_str().as_bytes(),
    );
    hash_optional_text(&mut hash, predicate_pushdown_identity);
    hash_optional_text(&mut hash, index_selection_fold_identity);
    format!("decorrelated-anchor-fold:{}", hex(&hash.finalize()))
}

fn add_decorrelated_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    input_identity: &str,
    index_selection_identity: &str,
) {
    details.insert(
        "decorrelated_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "decorrelated_anchor_fold_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "decorrelated_anchor_fold_input_identity".to_owned(),
        PlanDetail::Text(input_identity.to_owned()),
    );
    details.insert(
        "decorrelated_anchor_fold_index_selection_identity".to_owned(),
        PlanDetail::Text(index_selection_identity.to_owned()),
    );
    details.insert(
        "decorrelated_anchor_fold_pairing".to_owned(),
        PlanDetail::Text("sparse_anchor_fold_and_resolved_subquery_input".to_owned()),
    );
}

fn add_decorrelated_omission_refold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    input_identity: &str,
    subquery: &QueryDecorrelatedSubqueryDescription,
    omission_identity: &str,
    selection_identity: &str,
) {
    details.insert(
        "decorrelated_omission_refold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "decorrelated_omission_refold_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "decorrelated_omission_refold_input_identity".to_owned(),
        PlanDetail::Text(input_identity.to_owned()),
    );
    details.insert(
        "decorrelated_omission_refold_subquery_identity".to_owned(),
        PlanDetail::Text(subquery.identity.as_str().to_owned()),
    );
    details.insert(
        "decorrelated_omission_refold_omission_identity".to_owned(),
        PlanDetail::Text(omission_identity.to_owned()),
    );
    details.insert(
        "decorrelated_omission_refold_selection_identity".to_owned(),
        PlanDetail::Text(selection_identity.to_owned()),
    );
    details.insert(
        "decorrelated_omission_refold_pairing".to_owned(),
        PlanDetail::Text("sparse_parent_fold_and_unindexed_subquery_input".to_owned()),
    );
}

fn add_decorrelated_pin_chain_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    parent_identity: &str,
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
    transition: &str,
    subquery_identity: Option<&str>,
    selection_identity: Option<&str>,
    anchor_fold_identity: Option<&str>,
    omission_refold_identity: Option<&str>,
) {
    details.insert(
        "decorrelated_pin_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "decorrelated_pin_chain_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "decorrelated_pin_chain_snapshot".to_owned(),
        PlanDetail::Text(snapshot.as_str().to_owned()),
    );
    details.insert(
        "decorrelated_pin_chain_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "decorrelated_pin_chain_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "decorrelated_pin_chain_pairing".to_owned(),
        PlanDetail::Text("snapshot_pin_subquery_outcome_and_sparse_cost_parent".to_owned()),
    );
    if let Some(identity) = subquery_identity {
        details.insert(
            "decorrelated_pin_chain_subquery_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = selection_identity {
        details.insert(
            "decorrelated_pin_chain_selection_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = anchor_fold_identity {
        details.insert(
            "decorrelated_pin_chain_anchor_fold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
    if let Some(identity) = omission_refold_identity {
        details.insert(
            "decorrelated_pin_chain_omission_refold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    }
}

/// Binds a source's complete resolver-ordered window chain to the accumulated
/// sparse anchor fold and exact right input. The reference leaves this
/// explain-only identity encoding open; domain separation makes the pairing
/// explicit in plans and cost-fold digests.
fn query_window_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    window_chain_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-window-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, window_chain_identity.as_bytes());
    format!("window-anchor-fold:{}", hex(&hash.finalize()))
}

fn add_window_anchor_fold_details(details: &mut BTreeMap<String, PlanDetail>, identity: &str) {
    details.insert(
        "window_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "window_anchor_fold_pairing".to_owned(),
        PlanDetail::Text("sparse_anchor_fold_and_exact_source_frame_chain".to_owned()),
    );
}

/// Combines the exact resolver pair and right-input identity with the
/// accumulated sparse left anchor. ORNA permits internal join reordering but
/// leaves this explain-only digest format unspecified.
fn query_join_pair_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-join-pair-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    format!("join-pair-anchor-fold:{}", hex(&hash.finalize()))
}

fn add_join_pair_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "join_pair_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "join_pair_anchor_fold_pairing".to_owned(),
        PlanDetail::Text("sparse_anchor_fold_and_resolved_join_pair".to_owned()),
    );
}

/// Binds an ordered pushed aggregate chain to both its exact resolver join
/// pair and the accumulated sparse left anchor. The explain identity encoding
/// is planner-local; including the pair and chain prevents pushdown identity
/// from drifting when sparse cost order changes.
fn query_paired_aggregate_pushdown_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    aggregate_chain_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-aggregate-pushdown-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, aggregate_chain_identity.as_bytes());
    format!(
        "paired-aggregate-pushdown-anchor-fold:{}",
        hex(&hash.finalize())
    )
}

fn add_paired_aggregate_pushdown_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_aggregate_pushdown_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_aggregate_pushdown_anchor_fold_pairing".to_owned(),
        PlanDetail::Text(
            "sparse_anchor_fold_resolved_join_pair_and_aggregate_chain".to_owned(),
        ),
    );
}

fn add_paired_aggregate_cascade_pair_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_aggregate_cascade_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_aggregate_cascade_pairing".to_owned(),
        PlanDetail::Text("exact_join_pair_and_ordered_window_aggregate_chain".to_owned()),
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedAggregateAnchorCascadeFold {
    identity: String,
    pair_count: u64,
    aggregate_stage_count: u64,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedScopedWindowLimitRestorationFold {
    identity: String,
    pair_identity: String,
    scoped_window_fold_identity: String,
    limit_restoration_fold_identity: String,
    window_pair_count: u64,
    window_stage_count: u64,
    limit_pair_count: u64,
    limit_stage_count: u64,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedScopedLimitWindowRestorationFold {
    identity: String,
    pair_identity: String,
    scoped_limit_identity: String,
    limit_cascade_fold_identity: String,
    window_restoration_fold_identity: String,
    limit_pair_count: u64,
    limit_stage_count: u64,
    window_pair_count: u64,
    window_stage_count: u64,
    spill_pair_count: u64,
    spill_stage_count: u64,
    unknown_working_set_count: u64,
    estimated_spill_bytes: Option<u64>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedScopedWindowCompactionFold {
    identity: String,
    latest_pair_identity: String,
    latest_window_scope_identity: String,
    aggregate_compaction_identity: String,
    window_pair_count: u64,
    window_stage_count: u64,
    aggregate_pair_count: u64,
    aggregate_stage_count: u64,
    overflowed: bool,
}

/// Appends an exact paired aggregate chain to the sparse planned sequence.
/// Inputs without a window chain carry the previous identity through; the
/// next aggregate chain restores it by folding the new exact pair.
fn query_paired_aggregate_anchor_cascade_fold(
    previous: Option<&QueryPairedAggregateAnchorCascadeFold>,
    pair_aggregate_anchor_fold_identity: &str,
    aggregate_stage_count: u64,
) -> QueryPairedAggregateAnchorCascadeFold {
    let mut overflowed = previous.is_some_and(|previous| previous.overflowed);
    let (pair_count, aggregate_stage_count) = if let Some(previous) = previous {
        let pair_count = previous.pair_count.checked_add(1).unwrap_or_else(|| {
            overflowed = true;
            u64::MAX
        });
        let aggregate_stage_count = previous
            .aggregate_stage_count
            .checked_add(aggregate_stage_count)
            .unwrap_or_else(|| {
                overflowed = true;
                u64::MAX
            });
        (pair_count, aggregate_stage_count)
    } else {
        (1, aggregate_stage_count)
    };
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-aggregate-anchor-cascade-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_aggregate_anchor_fold_identity.as_bytes());
    hash.update(pair_count.to_be_bytes());
    hash.update(aggregate_stage_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);
    QueryPairedAggregateAnchorCascadeFold {
        identity: format!(
            "paired-aggregate-anchor-cascade-fold:{}",
            hex(&hash.finalize())
        ),
        pair_count,
        aggregate_stage_count,
        overflowed,
    }
}

fn add_paired_aggregate_cascade_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedAggregateAnchorCascadeFold,
) {
    details.insert(
        "paired_aggregate_anchor_cascade_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_aggregate_anchor_cascade_pairing".to_owned(),
        PlanDetail::Text("planned_pair_order_sparse_window_restoration_chains".to_owned()),
    );
    details.insert(
        "paired_aggregate_anchor_cascade_pair_count".to_owned(),
        PlanDetail::Integer(fold.pair_count),
    );
    details.insert(
        "paired_aggregate_anchor_cascade_stage_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_stage_count),
    );
    details.insert(
        "paired_aggregate_anchor_cascade_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

/// Binds an exact logical pair and anchor-scoped window chain to the current
/// cumulative paired-aggregate compaction fold. ORNA leaves this explain-only
/// digest composition unspecified; source and frame scope remain explicit.
fn query_paired_scoped_window_compaction_pair_identity(
    pair: &QueryJoinPairIdentityDescription,
    window_scope_identity: &str,
    aggregate_compaction_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-scoped-window-compaction-pair.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, window_scope_identity.as_bytes());
    hash_part(&mut hash, aggregate_compaction_identity.as_bytes());
    format!(
        "paired-scoped-window-compaction-pair:{}",
        hex(&hash.finalize())
    )
}

/// Accumulates exact scoped window chains over the paired aggregate
/// compaction history. A window chain advances both identities; joins without
/// a window chain carry the latest scope and aggregate fold unchanged.
fn query_paired_scoped_window_compaction_fold(
    previous: Option<&QueryPairedScopedWindowCompactionFold>,
    pair_identity: &str,
    window_scope_identity: &str,
    window_stage_count: u64,
    aggregate_fold: &QueryPairedAggregateAnchorCascadeFold,
) -> QueryPairedScopedWindowCompactionFold {
    let mut overflowed = previous.is_some_and(|fold| fold.overflowed) || aggregate_fold.overflowed;
    let window_pair_count = previous
        .map_or(1, |fold| fold.window_pair_count.checked_add(1).unwrap_or_else(|| {
            overflowed = true;
            u64::MAX
        }));
    let window_stage_count = previous.map_or(window_stage_count, |fold| {
        fold.window_stage_count
            .checked_add(window_stage_count)
            .unwrap_or_else(|| {
                overflowed = true;
                u64::MAX
            })
    });

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-scoped-window-compaction-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|fold| fold.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, window_scope_identity.as_bytes());
    hash_part(&mut hash, aggregate_fold.identity.as_bytes());
    hash.update(window_pair_count.to_be_bytes());
    hash.update(window_stage_count.to_be_bytes());
    hash.update(aggregate_fold.pair_count.to_be_bytes());
    hash.update(aggregate_fold.aggregate_stage_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);

    QueryPairedScopedWindowCompactionFold {
        identity: format!(
            "paired-scoped-window-compaction-fold:{}",
            hex(&hash.finalize())
        ),
        latest_pair_identity: pair_identity.to_owned(),
        latest_window_scope_identity: window_scope_identity.to_owned(),
        aggregate_compaction_identity: aggregate_fold.identity.clone(),
        window_pair_count,
        window_stage_count,
        aggregate_pair_count: aggregate_fold.pair_count,
        aggregate_stage_count: aggregate_fold.aggregate_stage_count,
        overflowed,
    }
}

fn add_paired_scoped_window_compaction_pair_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_scoped_window_compaction_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_scoped_window_compaction_pairing".to_owned(),
        PlanDetail::Text(
            "exact_join_pair_scoped_window_anchor_and_cumulative_aggregate_compaction".to_owned(),
        ),
    );
}

fn add_paired_scoped_window_compaction_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedScopedWindowCompactionFold,
    advanced_on_window_pair: bool,
) {
    details.insert(
        "paired_scoped_window_compaction_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_scoped_window_compaction_fold_pairing".to_owned(),
        PlanDetail::Text(
            "sparse_scoped_window_chains_over_paired_aggregate_compaction_folds".to_owned(),
        ),
    );
    details.insert(
        "paired_scoped_window_compaction_fold_transition".to_owned(),
        PlanDetail::Text(if advanced_on_window_pair {
            "advanced_scoped_window_pair".to_owned()
        } else {
            "carried_across_sparse_input".to_owned()
        }),
    );
    details.insert(
        "paired_scoped_window_compaction_window_anchor_fold_identity".to_owned(),
        PlanDetail::Text(fold.latest_window_scope_identity.clone()),
    );
    details.insert(
        "paired_scoped_window_compaction_latest_pair_identity".to_owned(),
        PlanDetail::Text(fold.latest_pair_identity.clone()),
    );
    details.insert(
        "paired_scoped_window_compaction_aggregate_fold_identity".to_owned(),
        PlanDetail::Text(fold.aggregate_compaction_identity.clone()),
    );
    for (key, value) in [
        (
            "paired_scoped_window_compaction_window_pair_count",
            fold.window_pair_count,
        ),
        (
            "paired_scoped_window_compaction_window_stage_count",
            fold.window_stage_count,
        ),
        (
            "paired_scoped_window_compaction_aggregate_pair_count",
            fold.aggregate_pair_count,
        ),
        (
            "paired_scoped_window_compaction_aggregate_stage_count",
            fold.aggregate_stage_count,
        ),
    ] {
        details.insert(key.to_owned(), PlanDetail::Integer(value));
    }
    details.insert(
        "paired_scoped_window_compaction_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

fn query_paired_window_spill_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    window_chain_identity: &str,
    spill_chain_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-spill-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, window_chain_identity.as_bytes());
    hash_part(&mut hash, spill_chain_identity.as_bytes());
    format!("paired-window-spill-anchor-fold:{}", hex(&hash.finalize()))
}

fn add_paired_window_spill_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_window_spill_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_window_spill_anchor_fold_pairing".to_owned(),
        PlanDetail::Text(
            "sparse_anchor_fold_resolved_join_pair_window_chain_and_spill_chain".to_owned(),
        ),
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueryAggregateSpillTotals {
    stage_count: u64,
    unknown_working_set_count: u64,
    estimated_bytes: Option<u64>,
    estimated_io_blocks: Option<u64>,
    estimated_io_work: Option<u64>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryWindowSpillCascadeFold {
    identity: String,
    spill_pair_count: u64,
    totals: QueryAggregateSpillTotals,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryWindowSpillRuleCostFold {
    identity: String,
    pair_count: u64,
    stage_count: u64,
    unknown_working_set_count: u64,
    estimated_bytes: Option<u64>,
    estimated_io_blocks: Option<u64>,
    estimated_io_work: Option<u64>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedWindowSpillRuleCostFold {
    identity: String,
    pair_count: u64,
    totals: QueryAggregateSpillTotals,
    rules: BTreeMap<String, QueryWindowSpillRuleCostFold>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedBoundedLimitWindowSpillFold {
    identity: String,
    pair_identity: String,
    bounded_limit_identity: String,
    limit_cascade_fold_identity: String,
    window_spill_fold_identity: String,
    bounded_rows: u64,
    limit_pair_count: u64,
    limit_stage_count: u64,
    spill_pair_count: u64,
    spill_stage_count: u64,
    unknown_working_set_count: u64,
    estimated_bytes: Option<u64>,
    estimated_io_blocks: Option<u64>,
    estimated_io_work: Option<u64>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedLimitAggregateSpillRestorationFold {
    identity: String,
    pair_identity: String,
    limit_fold_identity: String,
    aggregate_restoration_fold_identity: String,
    limit_pair_count: u64,
    limit_stage_count: u64,
    aggregate_pair_count: u64,
    aggregate_stage_count: u64,
    spill_pair_count: u64,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedAggregateSpillRestorationFold {
    identity: String,
    previous_identity: Option<String>,
    aggregate_fold_identity: String,
    spill_fold_identity: Option<String>,
    aggregate_pair_count: u64,
    aggregate_stage_count: u64,
    spill_pair_count: u64,
    spill_totals: Option<QueryAggregateSpillTotals>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedWindowCompactionSpillFold {
    identity: String,
    window_pair_count: u64,
    window_stage_count: u64,
    spill_pair_count: u64,
    spill_totals: Option<QueryAggregateSpillTotals>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedLimitAggregateRestorationFold {
    identity: String,
    aggregate_pair_count: u64,
    aggregate_stage_count: u64,
    limit_pair_count: u64,
    limit_stage_count: u64,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedCheckpointCostRestorationFold {
    identity: String,
    chain_count: u64,
    checkpoint_count: u64,
    left_segment_count: u64,
    right_segment_count: u64,
    left_compaction_count: u64,
    right_compaction_count: u64,
    branch_scopes: BTreeSet<(String, u64)>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedSegmentRotationCostRestorationFold {
    identity: String,
    chain_count: u64,
    rotation_count: u64,
    nested_checkpoint_fold_count: u64,
    branch_scopes: BTreeSet<(String, u64)>,
    source_streams: BTreeSet<(String, String, u64, u64)>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedStreamCompactionCostRestorationFold {
    identity: String,
    chain_count: u64,
    occurrence_count: u64,
    checkpoint_side_count: u64,
    absent_checkpoint_side_count: u64,
    nested_checkpoint_fold_count: u64,
    nested_rotation_fold_count: u64,
    branch_scopes: BTreeSet<(String, u64)>,
    source_streams: BTreeSet<(String, String, u64, u64)>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedCheckpointSpillCostRestorationFold {
    identity: String,
    chain_count: u64,
    occurrence_count: u64,
    restore_count: u64,
    spill_count: u64,
    source_stream_count: u64,
    compaction_count: u64,
    checkpoint_side_count: u64,
    absent_checkpoint_side_count: u64,
    both_sides_absent_occurrence_count: u64,
    nested_stream_fold_count: u64,
    branch_scopes: BTreeSet<(String, u64)>,
    restores: BTreeSet<(String, String, u64, u64)>,
    spills: BTreeSet<(String, String, u64, u64, u64, String)>,
    source_streams: BTreeSet<(String, String, u64, u64, u64, String, u64, String)>,
    compactions: BTreeSet<(String, String, u64, u64, u64, String, u64, String, u64)>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedStreamRotationCostRestorationFold {
    identity: String,
    chain_count: u64,
    occurrence_count: u64,
    rotation_count: u64,
    source_stream_count: u64,
    present_stream_side_count: u64,
    absent_stream_side_count: u64,
    both_stream_sides_absent_occurrence_count: u64,
    nested_spill_fold_count: u64,
    branch_scopes: BTreeSet<(String, u64)>,
    source_streams: BTreeSet<(String, String, u64, u64, String)>,
    rotations: BTreeSet<(String, String, u64, String, u64, String, u64, String, u64, u64)>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedWalRotationCostRestorationFold {
    identity: String,
    chain_count: u64,
    occurrence_count: u64,
    restore_count: u64,
    handoff_count: u64,
    stream_count: u64,
    compaction_count: u64,
    merge_count: u64,
    checkpoint_side_count: u64,
    absent_checkpoint_side_count: u64,
    both_checkpoint_sides_absent_count: u64,
    directional_wal_identity_count: u64,
    segment_identity_count: u64,
    nested_stream_rotation_fold_count: u64,
    branch_scopes: BTreeSet<(String, u64)>,
    restores: BTreeSet<(String, String, u64, u64)>,
    handoffs: BTreeSet<(String, String, u64, u64, u64)>,
    streams: BTreeSet<(String, String, u64, u64, u64, u64)>,
    compactions: BTreeSet<(String, String, u64, u64, u64, u64, u64)>,
    merges: BTreeSet<(String, String, u64, u64, u64, u64, u64, u64)>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedWindowCostRestorationFold {
    identity: String,
    parent_identity: Option<String>,
    pair_count: u64,
    window_pair_count: u64,
    cost_restore_fold_count: u64,
    window_identities: BTreeMap<String, String>,
    spill_pair_identities: BTreeMap<String, String>,
    spill_fold_identity: Option<String>,
    cost_restore_fold_identities: BTreeMap<String, String>,
    overflowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryPairedLimitWindowCompactionFold {
    identity: String,
    limit_aggregate_identity: Option<String>,
    window_compaction_identity: Option<String>,
    limit_pair_count: u64,
    limit_stage_count: u64,
    aggregate_pair_count: u64,
    aggregate_stage_count: u64,
    window_pair_count: u64,
    window_stage_count: u64,
    spill_pair_count: u64,
    spill_totals: Option<QueryAggregateSpillTotals>,
    overflowed: bool,
}

fn query_paired_window_spill_cascade_fold(
    previous: Option<&QueryWindowSpillCascadeFold>,
    pair_spill_fold_identity: &str,
    pair_totals: QueryAggregateSpillTotals,
) -> QueryWindowSpillCascadeFold {
    let mut overflowed = pair_totals.overflowed;
    let (spill_pair_count, mut totals) = if let Some(previous) = previous {
        let spill_pair_count = previous.spill_pair_count.checked_add(1).unwrap_or_else(|| {
            overflowed = true;
            u64::MAX
        });
        let prior = previous.totals;
        let mut totals = QueryAggregateSpillTotals {
            stage_count: prior.stage_count.checked_add(pair_totals.stage_count).unwrap_or_else(|| {
                overflowed = true;
                u64::MAX
            }),
            unknown_working_set_count: prior
                .unknown_working_set_count
                .checked_add(pair_totals.unknown_working_set_count)
                .unwrap_or_else(|| {
                    overflowed = true;
                    u64::MAX
                }),
            estimated_bytes: merge_query_spill_estimate(
                prior.estimated_bytes,
                pair_totals.estimated_bytes,
                &mut overflowed,
            ),
            estimated_io_blocks: merge_query_spill_estimate(
                prior.estimated_io_blocks,
                pair_totals.estimated_io_blocks,
                &mut overflowed,
            ),
            estimated_io_work: merge_query_spill_estimate(
                prior.estimated_io_work,
                pair_totals.estimated_io_work,
                &mut overflowed,
            ),
            overflowed: prior.overflowed || pair_totals.overflowed,
        };
        totals.overflowed |= overflowed;
        (spill_pair_count, totals)
    } else {
        (1, pair_totals)
    };
    totals.overflowed |= overflowed;

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-spill-cascade-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_spill_fold_identity.as_bytes());
    hash.update(spill_pair_count.to_be_bytes());
    hash_query_aggregate_spill_totals(&mut hash, totals);
    QueryWindowSpillCascadeFold {
        identity: format!("paired-window-spill-cascade-fold:{}", hex(&hash.finalize())),
        spill_pair_count,
        totals,
    }
}

fn merge_query_spill_estimate(
    prior: Option<u64>,
    next: Option<u64>,
    overflowed: &mut bool,
) -> Option<u64> {
    match (prior, next) {
        (Some(prior), Some(next)) => prior.checked_add(next).or_else(|| {
            *overflowed = true;
            None
        }),
        _ => None,
    }
}

fn hash_query_aggregate_spill_totals(hash: &mut Sha256, totals: QueryAggregateSpillTotals) {
    hash.update(totals.stage_count.to_be_bytes());
    hash.update(totals.unknown_working_set_count.to_be_bytes());
    hash_optional_u64(hash, totals.estimated_bytes);
    hash_optional_u64(hash, totals.estimated_io_blocks);
    hash_optional_u64(hash, totals.estimated_io_work);
    hash.update([u8::from(totals.overflowed)]);
}

fn add_paired_window_spill_cascade_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryWindowSpillCascadeFold,
) {
    details.insert(
        "paired_window_spill_cascade_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_window_spill_cascade_fold_pairing".to_owned(),
        PlanDetail::Text("planned_pair_order_sparse_spill_anchor_chains".to_owned()),
    );
    details.insert(
        "window_spill_cascade_pair_count".to_owned(),
        PlanDetail::Integer(fold.spill_pair_count),
    );
    details.insert(
        "window_spill_cascade_stage_count".to_owned(),
        PlanDetail::Integer(fold.totals.stage_count),
    );
    details.insert(
        "window_spill_cascade_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(fold.totals.unknown_working_set_count),
    );
    details.insert(
        "window_spill_cascade_overflowed".to_owned(),
        PlanDetail::Boolean(fold.totals.overflowed),
    );
    for (key, value) in [
        (
            "window_spill_cascade_estimated_bytes",
            fold.totals.estimated_bytes,
        ),
        (
            "window_spill_cascade_estimated_io_blocks",
            fold.totals.estimated_io_blocks,
        ),
        (
            "window_spill_cascade_estimated_io_work",
            fold.totals.estimated_io_work,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
    details.insert(
        "window_spill_cascade_estimate_status".to_owned(),
        PlanDetail::Text(if fold.totals.overflowed {
            "overflow".to_owned()
        } else if fold.totals.unknown_working_set_count > 0 {
            "unknown_working_set".to_owned()
        } else {
            "computed".to_owned()
        }),
    );
}

/// Folds each window aggregate's spill work below the paired spill cascade.
/// The reference leaves explain-only digest structure open; this adapter uses
/// the exact aggregate identity as the local rule key and preserves sparse
/// pair carry independently from each rule's computed totals.
fn query_paired_window_spill_rule_cost_fold(
    previous: Option<&QueryPairedWindowSpillRuleCostFold>,
    pair: &QueryJoinPairIdentityDescription,
    spill_chain_identity: &str,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
    cascade: &QueryWindowSpillCascadeFold,
) -> QueryPairedWindowSpillRuleCostFold {
    let mut rules = previous.map_or_else(BTreeMap::new, |fold| fold.rules.clone());
    for aggregate in window_aggregates
        .iter()
        .filter(|aggregate| aggregate.source == pair.right_source)
    {
        let Some(spill) = window_spills.iter().find(|spill| {
            spill.join_pair_identity == pair.identity
                && spill.source == pair.right_source
                && spill.window_aggregate_identity == aggregate.identity
        }) else {
            continue;
        };
        let (estimated_bytes, estimated_io_blocks, estimated_io_work) =
            query_window_spill_estimate(spill);
        let pair_totals = QueryAggregateSpillTotals {
            stage_count: 1,
            unknown_working_set_count: u64::from(spill.estimated_working_set_bytes.is_none()),
            estimated_bytes,
            estimated_io_blocks,
            estimated_io_work,
            overflowed: spill.estimated_working_set_bytes.is_some() && estimated_io_work.is_none(),
        };
        let previous_rule = rules.get(aggregate.identity.as_str());
        let fold =
            query_window_spill_rule_cost_fold(previous_rule, pair, aggregate, spill, pair_totals);
        rules.insert(aggregate.identity.as_str().to_owned(), fold);
    }

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-spill-rule-cost-fold.v1\0");
    hash_optional_text(&mut hash, previous.map(|fold| fold.identity.as_str()));
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, spill_chain_identity.as_bytes());
    hash_part(&mut hash, cascade.identity.as_bytes());
    hash.update((rules.len() as u64).to_be_bytes());
    for (aggregate_identity, rule_fold) in &rules {
        hash_part(&mut hash, aggregate_identity.as_bytes());
        hash_part(&mut hash, rule_fold.identity.as_bytes());
    }
    hash.update(cascade.spill_pair_count.to_be_bytes());
    hash_query_aggregate_spill_totals(&mut hash, cascade.totals);

    QueryPairedWindowSpillRuleCostFold {
        identity: format!(
            "paired-window-spill-rule-cost-fold:{}",
            hex(&hash.finalize())
        ),
        pair_count: cascade.spill_pair_count,
        totals: cascade.totals,
        rules,
    }
}

fn query_window_spill_rule_cost_fold(
    previous: Option<&QueryWindowSpillRuleCostFold>,
    pair: &QueryJoinPairIdentityDescription,
    aggregate: &QueryWindowAggregatePushdownDescription,
    spill: &QueryWindowSpillDescription,
    pair_totals: QueryAggregateSpillTotals,
) -> QueryWindowSpillRuleCostFold {
    let mut overflowed = previous.is_some_and(|fold| fold.overflowed) || pair_totals.overflowed;
    let (
        pair_count,
        stage_count,
        unknown_working_set_count,
        estimated_bytes,
        estimated_io_blocks,
        estimated_io_work,
    ) = if let Some(previous) = previous {
        let next_count = |prior: u64, next: u64, overflowed: &mut bool| {
            prior.checked_add(next).unwrap_or_else(|| {
                *overflowed = true;
                u64::MAX
            })
        };
        (
            next_count(previous.pair_count, 1, &mut overflowed),
            next_count(
                previous.stage_count,
                pair_totals.stage_count,
                &mut overflowed,
            ),
            next_count(
                previous.unknown_working_set_count,
                pair_totals.unknown_working_set_count,
                &mut overflowed,
            ),
            merge_query_spill_estimate(
                previous.estimated_bytes,
                pair_totals.estimated_bytes,
                &mut overflowed,
            ),
            merge_query_spill_estimate(
                previous.estimated_io_blocks,
                pair_totals.estimated_io_blocks,
                &mut overflowed,
            ),
            merge_query_spill_estimate(
                previous.estimated_io_work,
                pair_totals.estimated_io_work,
                &mut overflowed,
            ),
        )
    } else {
        (
            1,
            pair_totals.stage_count,
            pair_totals.unknown_working_set_count,
            pair_totals.estimated_bytes,
            pair_totals.estimated_io_blocks,
            pair_totals.estimated_io_work,
        )
    };

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-window-spill-rule-cost-fold.v1\0");
    hash_optional_text(&mut hash, previous.map(|fold| fold.identity.as_str()));
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, aggregate.identity.as_str().as_bytes());
    hash_part(&mut hash, aggregate.aggregate.as_str().as_bytes());
    hash_part(&mut hash, aggregate.frame_identity.as_str().as_bytes());
    hash_part(&mut hash, aggregate.frame_start.as_detail().as_bytes());
    hash_part(&mut hash, aggregate.frame_end.as_detail().as_bytes());
    hash_part(&mut hash, spill.identity.as_str().as_bytes());
    hash_optional_u64(&mut hash, spill.estimated_working_set_bytes);
    hash.update(spill.memory_budget_bytes.to_be_bytes());
    hash.update(pair_count.to_be_bytes());
    hash.update(stage_count.to_be_bytes());
    hash.update(unknown_working_set_count.to_be_bytes());
    hash_optional_u64(&mut hash, estimated_bytes);
    hash_optional_u64(&mut hash, estimated_io_blocks);
    hash_optional_u64(&mut hash, estimated_io_work);
    hash.update([u8::from(overflowed)]);

    QueryWindowSpillRuleCostFold {
        identity: format!("window-spill-rule-cost-fold:{}", hex(&hash.finalize())),
        pair_count,
        stage_count,
        unknown_working_set_count,
        estimated_bytes,
        estimated_io_blocks,
        estimated_io_work,
        overflowed,
    }
}

fn add_paired_window_spill_rule_cost_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedWindowSpillRuleCostFold,
    advanced_on_pair: bool,
) {
    details.insert(
        "paired_window_spill_rule_cost_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_window_spill_rule_cost_pairing".to_owned(),
        PlanDetail::Text("paired_window_spill_chain_with_nested_aggregate_rule_folds".to_owned()),
    );
    details.insert(
        "paired_window_spill_rule_cost_transition".to_owned(),
        PlanDetail::Text(if advanced_on_pair {
            "advanced_exact_pair_spill_rules".to_owned()
        } else {
            "carried_across_sparse_pair".to_owned()
        }),
    );
    details.insert(
        "paired_window_spill_rule_cost_pair_count".to_owned(),
        PlanDetail::Integer(fold.pair_count),
    );
    details.insert(
        "paired_window_spill_rule_cost_rule_count".to_owned(),
        PlanDetail::Integer(fold.rules.len() as u64),
    );
    details.insert(
        "paired_window_spill_rule_cost_stage_count".to_owned(),
        PlanDetail::Integer(fold.totals.stage_count),
    );
    details.insert(
        "paired_window_spill_rule_cost_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(fold.totals.unknown_working_set_count),
    );
    details.insert(
        "paired_window_spill_rule_cost_overflowed".to_owned(),
        PlanDetail::Boolean(fold.totals.overflowed),
    );
    details.insert(
        "paired_window_spill_rule_cost_estimate_status".to_owned(),
        PlanDetail::Text(if fold.totals.overflowed {
            "overflow".to_owned()
        } else if fold.totals.unknown_working_set_count > 0 {
            "unknown_working_set".to_owned()
        } else {
            "computed".to_owned()
        }),
    );
    for (key, value) in [
        (
            "paired_window_spill_rule_cost_estimated_bytes",
            fold.totals.estimated_bytes,
        ),
        (
            "paired_window_spill_rule_cost_estimated_io_blocks",
            fold.totals.estimated_io_blocks,
        ),
        (
            "paired_window_spill_rule_cost_estimated_io_work",
            fold.totals.estimated_io_work,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }

    let Some(PlanDetail::Text(rule_identity)) = details.get("window_aggregate_identity") else {
        return;
    };
    let Some(rule) = fold.rules.get(rule_identity) else {
        return;
    };
    details.insert(
        "paired_window_spill_rule_cost_rule_identity".to_owned(),
        PlanDetail::Text(rule_identity.clone()),
    );
    details.insert(
        "paired_window_spill_rule_cost_identity".to_owned(),
        PlanDetail::Text(rule.identity.clone()),
    );
    details.insert(
        "paired_window_spill_rule_cost_rule_pair_count".to_owned(),
        PlanDetail::Integer(rule.pair_count),
    );
    details.insert(
        "paired_window_spill_rule_cost_rule_stage_count".to_owned(),
        PlanDetail::Integer(rule.stage_count),
    );
    details.insert(
        "paired_window_spill_rule_cost_rule_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(rule.unknown_working_set_count),
    );
    details.insert(
        "paired_window_spill_rule_cost_rule_overflowed".to_owned(),
        PlanDetail::Boolean(rule.overflowed),
    );
    details.insert(
        "paired_window_spill_rule_cost_rule_estimate_status".to_owned(),
        PlanDetail::Text(if rule.overflowed {
            "overflow".to_owned()
        } else if rule.unknown_working_set_count > 0 {
            "unknown_working_set".to_owned()
        } else {
            "computed".to_owned()
        }),
    );
    for (key, value) in [
        (
            "paired_window_spill_rule_cost_rule_estimated_bytes",
            rule.estimated_bytes,
        ),
        (
            "paired_window_spill_rule_cost_rule_estimated_io_blocks",
            rule.estimated_io_blocks,
        ),
        (
            "paired_window_spill_rule_cost_rule_estimated_io_work",
            rule.estimated_io_work,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
}

/// The reference leaves planner-local join-cost digests unspecified. Bind a
/// rule-local spill history to the exact join edge and both cost ancestors so
/// nested rules cannot be mistaken for costs from a neighboring pair.
fn query_paired_window_spill_rule_cost_pair_identity(
    left_cost_identity: &str,
    right_cost_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    fold: &QueryPairedWindowSpillRuleCostFold,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-spill-rule-cost-pair.v1\0");
    hash_part(&mut hash, left_cost_identity.as_bytes());
    hash_part(&mut hash, right_cost_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, fold.identity.as_bytes());
    hash.update((fold.rules.len() as u64).to_be_bytes());
    for (rule_identity, rule_fold) in &fold.rules {
        hash_part(&mut hash, rule_identity.as_bytes());
        hash_part(&mut hash, rule_fold.identity.as_bytes());
    }
    format!(
        "paired-window-spill-rule-cost-pair:{}",
        hex(&hash.finalize())
    )
}

fn add_paired_window_spill_rule_cost_pair_identity_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_window_spill_rule_cost_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_window_spill_rule_cost_pair_identity_policy".to_owned(),
        PlanDetail::Text("join_ancestors_exact_pair_nested_rule_folds_sha256_v1".to_owned()),
    );
}

/// Folds each exact paired window chain together with the cumulative spill
/// history observed at its compaction boundaries. Window chains without a
/// spill still advance the pair identity, while sparse joins without a window
/// chain carry the last combined identity and estimates unchanged.
fn query_paired_window_compaction_spill_pair_identity(
    pair: &QueryJoinPairIdentityDescription,
    window_anchor_fold_identity: &str,
    window_chain_identity: &str,
    spill_chain_identity: Option<&str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-compaction-spill-pair.v1\0");
    hash_part(&mut hash, window_anchor_fold_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, window_chain_identity.as_bytes());
    hash_optional_text(&mut hash, spill_chain_identity);
    format!(
        "paired-window-compaction-spill-pair:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_window_compaction_spill_fold(
    previous: Option<&QueryPairedWindowCompactionSpillFold>,
    pair_identity: &str,
    window_fold: &QueryPairedAggregateAnchorCascadeFold,
    spill_fold: Option<&QueryWindowSpillCascadeFold>,
) -> QueryPairedWindowCompactionSpillFold {
    let spill_totals = spill_fold.map(|fold| fold.totals);
    let spill_pair_count = spill_fold.map_or(0, |fold| fold.spill_pair_count);
    let overflowed = window_fold.overflowed
        || spill_totals.is_some_and(|totals| totals.overflowed);

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-compaction-spill-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, window_fold.identity.as_bytes());
    hash_optional_text(&mut hash, spill_fold.map(|fold| fold.identity.as_str()));
    hash.update(window_fold.pair_count.to_be_bytes());
    hash.update(window_fold.aggregate_stage_count.to_be_bytes());
    hash.update(spill_pair_count.to_be_bytes());
    if let Some(totals) = spill_totals {
        hash.update([1]);
        hash_query_aggregate_spill_totals(&mut hash, totals);
    } else {
        hash.update([0]);
    }
    hash.update([u8::from(overflowed)]);

    QueryPairedWindowCompactionSpillFold {
        identity: format!(
            "paired-window-compaction-spill-fold:{}",
            hex(&hash.finalize())
        ),
        window_pair_count: window_fold.pair_count,
        window_stage_count: window_fold.aggregate_stage_count,
        spill_pair_count,
        spill_totals,
        overflowed,
    }
}

fn add_paired_window_compaction_spill_pair_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_window_compaction_spill_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_window_compaction_spill_pairing".to_owned(),
        PlanDetail::Text(
            "exact_join_pair_window_chain_and_optional_compaction_spill_chain".to_owned(),
        ),
    );
}

fn add_paired_window_compaction_spill_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedWindowCompactionSpillFold,
    advanced_on_window_pair: bool,
) {
    details.insert(
        "paired_window_compaction_spill_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_window_compaction_spill_fold_pairing".to_owned(),
        PlanDetail::Text(
            "sparse_paired_window_chains_with_cumulative_compaction_spill_history".to_owned(),
        ),
    );
    details.insert(
        "paired_window_compaction_spill_fold_transition".to_owned(),
        PlanDetail::Text(if advanced_on_window_pair {
            "advanced_window_pair".to_owned()
        } else {
            "carried_across_sparse_input".to_owned()
        }),
    );
    details.insert(
        "paired_window_compaction_spill_window_pair_count".to_owned(),
        PlanDetail::Integer(fold.window_pair_count),
    );
    details.insert(
        "paired_window_compaction_spill_window_stage_count".to_owned(),
        PlanDetail::Integer(fold.window_stage_count),
    );
    details.insert(
        "paired_window_compaction_spill_pair_count".to_owned(),
        PlanDetail::Integer(fold.spill_pair_count),
    );
    let totals = fold.spill_totals;
    details.insert(
        "paired_window_compaction_spill_stage_count".to_owned(),
        PlanDetail::Integer(totals.map_or(0, |totals| totals.stage_count)),
    );
    details.insert(
        "paired_window_compaction_spill_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(totals.map_or(0, |totals| totals.unknown_working_set_count)),
    );
    details.insert(
        "paired_window_compaction_spill_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    let status = match totals {
        None => "no_spill_pairs",
        Some(totals) if fold.overflowed || totals.overflowed => "overflow",
        Some(totals) if totals.unknown_working_set_count > 0 => "unknown_working_set",
        Some(_) => "computed",
    };
    details.insert(
        "paired_window_compaction_spill_estimate_status".to_owned(),
        PlanDetail::Text(status.to_owned()),
    );
    for (key, value) in [
        (
            "paired_window_compaction_spill_estimated_bytes",
            totals.and_then(|totals| totals.estimated_bytes),
        ),
        (
            "paired_window_compaction_spill_estimated_io_blocks",
            totals.and_then(|totals| totals.estimated_io_blocks),
        ),
        (
            "paired_window_compaction_spill_estimated_io_work",
            totals.and_then(|totals| totals.estimated_io_work),
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
}

/// Binds the latest bounded limit input to the cumulative paired window-spill
/// cascade. The reference leaves this explain-only composition open; keep the
/// minimum declared per-pair limit cap beside both component identities so a
/// later sparse spill cannot detach from the limit that constrained its scope.
fn query_paired_bounded_limit_window_spill_pair_identity(
    limit_identity: &str,
    bounded_rows: u64,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    spill_fold: &QueryWindowSpillCascadeFold,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-bounded-limit-window-spill-pair.v1\0");
    hash_part(&mut hash, limit_identity.as_bytes());
    hash.update(bounded_rows.to_be_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash_part(&mut hash, spill_fold.identity.as_bytes());
    format!(
        "paired-bounded-limit-window-spill-pair:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_bounded_limit_window_spill_fold(
    previous: Option<&QueryPairedBoundedLimitWindowSpillFold>,
    pair_identity: &str,
    limit_identity: &str,
    bounded_rows: u64,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    spill_fold: &QueryWindowSpillCascadeFold,
) -> QueryPairedBoundedLimitWindowSpillFold {
    if let Some(previous) = previous
        && previous.bounded_limit_identity == limit_identity
        && previous.bounded_rows == bounded_rows
        && previous.limit_cascade_fold_identity == limit_fold.identity
        && previous.window_spill_fold_identity == spill_fold.identity
    {
        return previous.clone();
    }

    let totals = spill_fold.totals;
    let overflowed = limit_fold.overflowed || totals.overflowed;
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-bounded-limit-window-spill-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, limit_identity.as_bytes());
    hash.update(bounded_rows.to_be_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash_part(&mut hash, spill_fold.identity.as_bytes());
    hash.update(limit_fold.pair_count.to_be_bytes());
    hash.update(limit_fold.stage_count.to_be_bytes());
    hash.update(spill_fold.spill_pair_count.to_be_bytes());
    hash_query_aggregate_spill_totals(&mut hash, totals);
    hash.update([u8::from(overflowed)]);

    QueryPairedBoundedLimitWindowSpillFold {
        identity: format!(
            "paired-bounded-limit-window-spill-fold:{}",
            hex(&hash.finalize())
        ),
        pair_identity: pair_identity.to_owned(),
        bounded_limit_identity: limit_identity.to_owned(),
        limit_cascade_fold_identity: limit_fold.identity.clone(),
        window_spill_fold_identity: spill_fold.identity.clone(),
        bounded_rows,
        limit_pair_count: limit_fold.pair_count,
        limit_stage_count: limit_fold.stage_count,
        spill_pair_count: spill_fold.spill_pair_count,
        spill_stage_count: totals.stage_count,
        unknown_working_set_count: totals.unknown_working_set_count,
        estimated_bytes: totals.estimated_bytes,
        estimated_io_blocks: totals.estimated_io_blocks,
        estimated_io_work: totals.estimated_io_work,
        overflowed,
    }
}

fn query_paired_bounded_limit_window_spill_transition(
    previous: Option<&QueryPairedBoundedLimitWindowSpillFold>,
    limit_identity: &str,
    bounded_rows: u64,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    spill_fold: &QueryWindowSpillCascadeFold,
) -> &'static str {
    let limit_advanced = previous.is_none_or(|previous| {
        previous.bounded_limit_identity != limit_identity
            || previous.bounded_rows != bounded_rows
            || previous.limit_cascade_fold_identity != limit_fold.identity
    });
    let spill_advanced = previous.is_none_or(|previous| {
        previous.window_spill_fold_identity != spill_fold.identity
    });
    match (limit_advanced, spill_advanced) {
        (true, true) => "advanced_bounded_limit_and_window_spill",
        (true, false) => "advanced_bounded_limit",
        (false, true) => "advanced_window_spill",
        (false, false) => "carried_across_sparse_input",
    }
}

fn add_paired_bounded_limit_window_spill_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedBoundedLimitWindowSpillFold,
    transition: &str,
) {
    for (key, value) in [
        (
            "paired_bounded_limit_window_spill_fold_identity",
            fold.identity.as_str(),
        ),
        (
            "paired_bounded_limit_window_spill_pair_identity",
            fold.pair_identity.as_str(),
        ),
        (
            "paired_bounded_limit_window_spill_limit_identity",
            fold.bounded_limit_identity.as_str(),
        ),
        (
            "paired_bounded_limit_window_spill_limit_cascade_fold_identity",
            fold.limit_cascade_fold_identity.as_str(),
        ),
        (
            "paired_bounded_limit_window_spill_window_spill_fold_identity",
            fold.window_spill_fold_identity.as_str(),
        ),
    ] {
        details.insert(key.to_owned(), PlanDetail::Text(value.to_owned()));
    }
    details.insert(
        "paired_bounded_limit_window_spill_pairing".to_owned(),
        PlanDetail::Text(
            "latest_bounded_limit_with_cumulative_paired_window_spill_cascade".to_owned(),
        ),
    );
    details.insert(
        "paired_bounded_limit_window_spill_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    for (key, value) in [
        (
            "paired_bounded_limit_window_spill_bounded_rows",
            fold.bounded_rows,
        ),
        (
            "paired_bounded_limit_window_spill_limit_pair_count",
            fold.limit_pair_count,
        ),
        (
            "paired_bounded_limit_window_spill_limit_stage_count",
            fold.limit_stage_count,
        ),
        (
            "paired_bounded_limit_window_spill_spill_pair_count",
            fold.spill_pair_count,
        ),
        (
            "paired_bounded_limit_window_spill_spill_stage_count",
            fold.spill_stage_count,
        ),
        (
            "paired_bounded_limit_window_spill_unknown_working_set_count",
            fold.unknown_working_set_count,
        ),
    ] {
        details.insert(key.to_owned(), PlanDetail::Integer(value));
    }
    details.insert(
        "paired_bounded_limit_window_spill_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    let status = if fold.overflowed {
        "overflow"
    } else if fold.unknown_working_set_count > 0 {
        "unknown_working_set"
    } else if fold.spill_pair_count == 0 {
        "no_spill_pairs"
    } else {
        "computed"
    };
    details.insert(
        "paired_bounded_limit_window_spill_estimate_status".to_owned(),
        PlanDetail::Text(status.to_owned()),
    );
    for (key, value) in [
        (
            "paired_bounded_limit_window_spill_estimated_bytes",
            fold.estimated_bytes,
        ),
        (
            "paired_bounded_limit_window_spill_estimated_io_blocks",
            fold.estimated_io_blocks,
        ),
        (
            "paired_bounded_limit_window_spill_estimated_io_work",
            fold.estimated_io_work,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
}

/// Combines the latest exact limit scope with cumulative paired window
/// restoration. ORNA leaves this explain-only digest open, so bind the exact
/// limit anchor and both accumulated component folds; sparse inputs carry it.
fn query_paired_scoped_limit_window_restoration_pair_identity(
    limit_scope_identity: &str,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    window_fold: &QueryPairedWindowCompactionSpillFold,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-scoped-limit-window-restoration-pair.v1\0");
    hash_part(&mut hash, limit_scope_identity.as_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash_part(&mut hash, window_fold.identity.as_bytes());
    format!(
        "paired-scoped-limit-window-restoration-pair:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_scoped_limit_window_restoration_fold(
    previous: Option<&QueryPairedScopedLimitWindowRestorationFold>,
    pair_identity: &str,
    limit_scope_identity: &str,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    window_fold: &QueryPairedWindowCompactionSpillFold,
) -> QueryPairedScopedLimitWindowRestorationFold {
    if let Some(previous) = previous
        && previous.scoped_limit_identity == limit_scope_identity
        && previous.limit_cascade_fold_identity == limit_fold.identity
        && previous.window_restoration_fold_identity == window_fold.identity
    {
        return previous.clone();
    }

    let totals = window_fold.spill_totals;
    let overflowed = limit_fold.overflowed
        || window_fold.overflowed
        || totals.is_some_and(|totals| totals.overflowed);
    let spill_stage_count = totals.map_or(0, |totals| totals.stage_count);
    let unknown_working_set_count = totals.map_or(0, |totals| totals.unknown_working_set_count);
    let estimated_spill_bytes = totals.and_then(|totals| totals.estimated_bytes);

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-scoped-limit-window-restoration-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, limit_scope_identity.as_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash_part(&mut hash, window_fold.identity.as_bytes());
    hash.update(limit_fold.pair_count.to_be_bytes());
    hash.update(limit_fold.stage_count.to_be_bytes());
    hash.update(window_fold.window_pair_count.to_be_bytes());
    hash.update(window_fold.window_stage_count.to_be_bytes());
    hash.update(window_fold.spill_pair_count.to_be_bytes());
    hash.update(spill_stage_count.to_be_bytes());
    hash.update(unknown_working_set_count.to_be_bytes());
    hash_optional_u64(&mut hash, estimated_spill_bytes);
    hash.update([u8::from(overflowed)]);

    QueryPairedScopedLimitWindowRestorationFold {
        identity: format!(
            "paired-scoped-limit-window-restoration-fold:{}",
            hex(&hash.finalize())
        ),
        pair_identity: pair_identity.to_owned(),
        scoped_limit_identity: limit_scope_identity.to_owned(),
        limit_cascade_fold_identity: limit_fold.identity.clone(),
        window_restoration_fold_identity: window_fold.identity.clone(),
        limit_pair_count: limit_fold.pair_count,
        limit_stage_count: limit_fold.stage_count,
        window_pair_count: window_fold.window_pair_count,
        window_stage_count: window_fold.window_stage_count,
        spill_pair_count: window_fold.spill_pair_count,
        spill_stage_count,
        unknown_working_set_count,
        estimated_spill_bytes,
        overflowed,
    }
}

fn query_paired_scoped_limit_window_restoration_transition(
    previous: Option<&QueryPairedScopedLimitWindowRestorationFold>,
    limit_scope_identity: &str,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    window_fold: &QueryPairedWindowCompactionSpillFold,
) -> &'static str {
    let limit_advanced = previous.is_none_or(|previous| {
        previous.scoped_limit_identity != limit_scope_identity
            || previous.limit_cascade_fold_identity != limit_fold.identity
    });
    let window_advanced = previous.is_none_or(|previous| {
        previous.window_restoration_fold_identity != window_fold.identity
    });
    match (limit_advanced, window_advanced) {
        (true, true) => "advanced_limit_scope_and_window_restoration",
        (true, false) => "advanced_limit_scope",
        (false, true) => "advanced_window_restoration",
        (false, false) => "carried_across_sparse_input",
    }
}

fn add_paired_scoped_limit_window_restoration_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedScopedLimitWindowRestorationFold,
    transition: &str,
) {
    details.insert(
        "paired_scoped_limit_window_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_scoped_limit_window_restoration_pair_identity".to_owned(),
        PlanDetail::Text(fold.pair_identity.clone()),
    );
    details.insert(
        "paired_scoped_limit_window_restoration_pairing".to_owned(),
        PlanDetail::Text(
            "latest_exact_limit_scope_with_cumulative_paired_window_restoration".to_owned(),
        ),
    );
    details.insert(
        "paired_scoped_limit_window_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_scoped_limit_window_restoration_limit_scope_identity".to_owned(),
        PlanDetail::Text(fold.scoped_limit_identity.clone()),
    );
    details.insert(
        "paired_scoped_limit_window_restoration_limit_cascade_fold_identity".to_owned(),
        PlanDetail::Text(fold.limit_cascade_fold_identity.clone()),
    );
    details.insert(
        "paired_scoped_limit_window_restoration_window_fold_identity".to_owned(),
        PlanDetail::Text(fold.window_restoration_fold_identity.clone()),
    );
    for (key, value) in [
        (
            "paired_scoped_limit_window_restoration_limit_pair_count",
            fold.limit_pair_count,
        ),
        (
            "paired_scoped_limit_window_restoration_limit_stage_count",
            fold.limit_stage_count,
        ),
        (
            "paired_scoped_limit_window_restoration_window_pair_count",
            fold.window_pair_count,
        ),
        (
            "paired_scoped_limit_window_restoration_window_stage_count",
            fold.window_stage_count,
        ),
        (
            "paired_scoped_limit_window_restoration_spill_pair_count",
            fold.spill_pair_count,
        ),
        (
            "paired_scoped_limit_window_restoration_spill_stage_count",
            fold.spill_stage_count,
        ),
        (
            "paired_scoped_limit_window_restoration_unknown_working_set_count",
            fold.unknown_working_set_count,
        ),
    ] {
        details.insert(key.to_owned(), PlanDetail::Integer(value));
    }
    details.insert(
        "paired_scoped_limit_window_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    let status = if fold.overflowed {
        "overflow"
    } else if fold.unknown_working_set_count > 0 {
        "unknown_working_set"
    } else if fold.spill_pair_count == 0 {
        "no_spill_pairs"
    } else {
        "computed"
    };
    details.insert(
        "paired_scoped_limit_window_restoration_estimate_status".to_owned(),
        PlanDetail::Text(status.to_owned()),
    );
    if let Some(bytes) = fold.estimated_spill_bytes {
        details.insert(
            "paired_scoped_limit_window_restoration_estimated_spill_bytes".to_owned(),
            PlanDetail::Integer(bytes),
        );
    } else {
        details.remove("paired_scoped_limit_window_restoration_estimated_spill_bytes");
    }
}

/// Joins sparse aggregate restoration and spill histories into one identity.
/// Each exact aggregate chain advances the fold; spill counts and estimates
/// remain the accumulated values from the last and any later spill pairs.
fn query_paired_aggregate_spill_restoration_fold(
    previous: Option<&QueryPairedAggregateSpillRestorationFold>,
    pair_identity: &str,
    aggregate_fold: &QueryPairedAggregateAnchorCascadeFold,
    spill_fold: Option<&QueryWindowSpillCascadeFold>,
) -> QueryPairedAggregateSpillRestorationFold {
    let spill_totals = spill_fold.map(|fold| fold.totals);
    let spill_pair_count = spill_fold.map_or(0, |fold| fold.spill_pair_count);
    let aggregate_fold_identity = aggregate_fold.identity.clone();
    let spill_fold_identity = spill_fold.map(|fold| fold.identity.clone());
    let previous_identity = previous.map(|fold| fold.identity.clone());
    let overflowed = aggregate_fold.overflowed
        || spill_totals.is_some_and(|totals| totals.overflowed);

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-aggregate-spill-restoration-fold.v1\0");
    hash_optional_text(&mut hash, previous_identity.as_deref());
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, aggregate_fold_identity.as_bytes());
    hash_optional_text(&mut hash, spill_fold_identity.as_deref());
    hash.update(aggregate_fold.pair_count.to_be_bytes());
    hash.update(aggregate_fold.aggregate_stage_count.to_be_bytes());
    hash.update(spill_pair_count.to_be_bytes());
    if let Some(totals) = spill_totals {
        hash.update([1]);
        hash_query_aggregate_spill_totals(&mut hash, totals);
    } else {
        hash.update([0]);
    }
    hash.update([u8::from(overflowed)]);

    QueryPairedAggregateSpillRestorationFold {
        identity: format!(
            "paired-aggregate-spill-restoration-fold:{}",
            hex(&hash.finalize())
        ),
        previous_identity,
        aggregate_fold_identity,
        spill_fold_identity,
        aggregate_pair_count: aggregate_fold.pair_count,
        aggregate_stage_count: aggregate_fold.aggregate_stage_count,
        spill_pair_count,
        spill_totals,
        overflowed,
    }
}

fn add_paired_aggregate_spill_restoration_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedAggregateSpillRestorationFold,
) {
    details.insert(
        "paired_aggregate_spill_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    if let Some(identity) = fold.previous_identity.as_deref() {
        details.insert(
            "paired_aggregate_spill_restoration_previous_fold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    } else {
        details.remove("paired_aggregate_spill_restoration_previous_fold_identity");
    }
    details.insert(
        "paired_aggregate_spill_restoration_aggregate_fold_identity".to_owned(),
        PlanDetail::Text(fold.aggregate_fold_identity.clone()),
    );
    if let Some(identity) = fold.spill_fold_identity.as_deref() {
        details.insert(
            "paired_aggregate_spill_restoration_spill_fold_identity".to_owned(),
            PlanDetail::Text(identity.to_owned()),
        );
    } else {
        details.remove("paired_aggregate_spill_restoration_spill_fold_identity");
    }
    details.insert(
        "paired_aggregate_spill_restoration_fold_pairing".to_owned(),
        PlanDetail::Text(
            "planned_sparse_aggregate_restoration_chains_with_accumulated_exact_spill_pairs"
                .to_owned(),
        ),
    );
    details.insert(
        "paired_aggregate_spill_restoration_aggregate_pair_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_pair_count),
    );
    details.insert(
        "paired_aggregate_spill_restoration_aggregate_stage_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_stage_count),
    );
    details.insert(
        "paired_aggregate_spill_restoration_spill_pair_count".to_owned(),
        PlanDetail::Integer(fold.spill_pair_count),
    );
    details.insert(
        "paired_aggregate_spill_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    let status = match fold.spill_totals {
        None => "no_spill_pairs",
        Some(totals) if fold.overflowed || totals.overflowed => "overflowed",
        Some(totals) if totals.unknown_working_set_count > 0 => "unknown_working_set",
        Some(_) => "computed",
    };
    details.insert(
        "paired_aggregate_spill_restoration_estimate_status".to_owned(),
        PlanDetail::Text(status.to_owned()),
    );
    let totals = fold.spill_totals;
    details.insert(
        "paired_aggregate_spill_restoration_spill_stage_count".to_owned(),
        PlanDetail::Integer(totals.map_or(0, |totals| totals.stage_count)),
    );
    details.insert(
        "paired_aggregate_spill_restoration_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(totals.map_or(0, |totals| totals.unknown_working_set_count)),
    );
    for (key, value) in [
        (
            "paired_aggregate_spill_restoration_estimated_bytes",
            totals.and_then(|totals| totals.estimated_bytes),
        ),
        (
            "paired_aggregate_spill_restoration_estimated_io_blocks",
            totals.and_then(|totals| totals.estimated_io_blocks),
        ),
        (
            "paired_aggregate_spill_restoration_estimated_io_work",
            totals.and_then(|totals| totals.estimated_io_work),
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
}

/// Folds exact paired limit chains and aggregate restoration chains together.
/// An event from either chain advances identity while the other chain's
/// cumulative identity and counts carry unchanged through sparse joins.
fn query_paired_limit_aggregate_restoration_fold(
    previous: Option<&QueryPairedLimitAggregateRestorationFold>,
    pair_identity: &str,
    limit_fold: Option<&QueryJoinLimitAnchorCascadeFold>,
    aggregate_fold: Option<&QueryPairedAggregateAnchorCascadeFold>,
) -> QueryPairedLimitAggregateRestorationFold {
    let aggregate_pair_count = aggregate_fold.map_or(0, |fold| fold.pair_count);
    let aggregate_stage_count = aggregate_fold.map_or(0, |fold| fold.aggregate_stage_count);
    let limit_pair_count = limit_fold.map_or(0, |fold| fold.pair_count);
    let limit_stage_count = limit_fold.map_or(0, |fold| fold.stage_count);
    let overflowed = limit_fold.is_some_and(|fold| fold.overflowed)
        || aggregate_fold.is_some_and(|fold| fold.overflowed);

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-aggregate-restoration-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_optional_text(&mut hash, limit_fold.map(|fold| fold.identity.as_str()));
    hash_optional_text(
        &mut hash,
        aggregate_fold.map(|fold| fold.identity.as_str()),
    );
    hash.update(aggregate_pair_count.to_be_bytes());
    hash.update(aggregate_stage_count.to_be_bytes());
    hash.update(limit_pair_count.to_be_bytes());
    hash.update(limit_stage_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);

    QueryPairedLimitAggregateRestorationFold {
        identity: format!(
            "paired-limit-aggregate-restoration-fold:{}",
            hex(&hash.finalize())
        ),
        aggregate_pair_count,
        aggregate_stage_count,
        limit_pair_count,
        limit_stage_count,
        overflowed,
    }
}

fn add_paired_limit_aggregate_restoration_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedLimitAggregateRestorationFold,
) {
    details.insert(
        "paired_limit_aggregate_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_limit_aggregate_restoration_fold_pairing".to_owned(),
        PlanDetail::Text(
            "planned_aggregate_restoration_chains_with_accumulated_exact_limit_pairs".to_owned(),
        ),
    );
    details.insert(
        "paired_limit_aggregate_restoration_aggregate_pair_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_pair_count),
    );
    details.insert(
        "paired_limit_aggregate_restoration_aggregate_stage_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_stage_count),
    );
    details.insert(
        "paired_limit_aggregate_restoration_limit_pair_count".to_owned(),
        PlanDetail::Integer(fold.limit_pair_count),
    );
    details.insert(
        "paired_limit_aggregate_restoration_limit_stage_count".to_owned(),
        PlanDetail::Integer(fold.limit_stage_count),
    );
    details.insert(
        "paired_limit_aggregate_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

/// Combines the cumulative paired limit route with the latest sparse
/// aggregate spill restoration fold. Either component can advance alone;
/// sparse joins carry both component identities without changing the fold.
fn query_paired_limit_aggregate_spill_restoration_pair_identity(
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    aggregate_fold: &QueryPairedAggregateSpillRestorationFold,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-aggregate-spill-restoration-pair.v1\0");
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash_part(&mut hash, aggregate_fold.identity.as_bytes());
    format!(
        "paired-limit-aggregate-spill-restoration-pair:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_limit_aggregate_spill_restoration_fold(
    previous: Option<&QueryPairedLimitAggregateSpillRestorationFold>,
    pair_identity: &str,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    aggregate_fold: &QueryPairedAggregateSpillRestorationFold,
) -> QueryPairedLimitAggregateSpillRestorationFold {
    if let Some(previous) = previous
        && previous.limit_fold_identity == limit_fold.identity
        && previous.aggregate_restoration_fold_identity == aggregate_fold.identity
    {
        return previous.clone();
    }

    let overflowed = limit_fold.overflowed || aggregate_fold.overflowed;
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-aggregate-spill-restoration-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash_part(&mut hash, aggregate_fold.identity.as_bytes());
    hash.update(limit_fold.pair_count.to_be_bytes());
    hash.update(limit_fold.stage_count.to_be_bytes());
    hash.update(aggregate_fold.aggregate_pair_count.to_be_bytes());
    hash.update(aggregate_fold.aggregate_stage_count.to_be_bytes());
    hash.update(aggregate_fold.spill_pair_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);

    QueryPairedLimitAggregateSpillRestorationFold {
        identity: format!(
            "paired-limit-aggregate-spill-restoration-fold:{}",
            hex(&hash.finalize())
        ),
        pair_identity: pair_identity.to_owned(),
        limit_fold_identity: limit_fold.identity.clone(),
        aggregate_restoration_fold_identity: aggregate_fold.identity.clone(),
        limit_pair_count: limit_fold.pair_count,
        limit_stage_count: limit_fold.stage_count,
        aggregate_pair_count: aggregate_fold.aggregate_pair_count,
        aggregate_stage_count: aggregate_fold.aggregate_stage_count,
        spill_pair_count: aggregate_fold.spill_pair_count,
        overflowed,
    }
}

fn query_paired_limit_aggregate_spill_restoration_transition(
    previous: Option<&QueryPairedLimitAggregateSpillRestorationFold>,
    limit_fold: &QueryJoinLimitAnchorCascadeFold,
    aggregate_fold: &QueryPairedAggregateSpillRestorationFold,
) -> &'static str {
    let limit_advanced = previous.is_none_or(|previous| {
        previous.limit_fold_identity != limit_fold.identity
    });
    let aggregate_advanced = previous.is_none_or(|previous| {
        previous.aggregate_restoration_fold_identity != aggregate_fold.identity
    });
    match (limit_advanced, aggregate_advanced) {
        (true, true) => "advanced_limit_and_aggregate_restoration",
        (true, false) => "advanced_limit_identity",
        (false, true) => "advanced_aggregate_restoration",
        (false, false) => "carried_across_sparse_input",
    }
}

fn add_paired_limit_aggregate_spill_restoration_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedLimitAggregateSpillRestorationFold,
    transition: &str,
) {
    details.insert(
        "paired_limit_aggregate_spill_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_pair_identity".to_owned(),
        PlanDetail::Text(fold.pair_identity.clone()),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_pairing".to_owned(),
        PlanDetail::Text(
            "paired_limit_anchor_cascade_with_sparse_aggregate_spill_restoration".to_owned(),
        ),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_limit_fold_identity".to_owned(),
        PlanDetail::Text(fold.limit_fold_identity.clone()),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_aggregate_fold_identity".to_owned(),
        PlanDetail::Text(fold.aggregate_restoration_fold_identity.clone()),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_limit_pair_count".to_owned(),
        PlanDetail::Integer(fold.limit_pair_count),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_limit_stage_count".to_owned(),
        PlanDetail::Integer(fold.limit_stage_count),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_aggregate_pair_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_pair_count),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_aggregate_stage_count".to_owned(),
        PlanDetail::Integer(fold.aggregate_stage_count),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_spill_pair_count".to_owned(),
        PlanDetail::Integer(fold.spill_pair_count),
    );
    details.insert(
        "paired_limit_aggregate_spill_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

/// Scopes the latest exact window chain to the cumulative paired limit and
/// aggregate restoration history. Sparse joins retain both component folds;
/// a new window scope or restoration event advances the composite identity.
fn query_paired_scoped_window_limit_restoration_pair_identity(
    window_fold: &QueryPairedScopedWindowCompactionFold,
    limit_fold: &QueryPairedLimitAggregateRestorationFold,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-scoped-window-limit-restoration-pair.v1\0");
    hash_part(&mut hash, window_fold.latest_pair_identity.as_bytes());
    hash_part(
        &mut hash,
        window_fold.latest_window_scope_identity.as_bytes(),
    );
    hash_part(&mut hash, window_fold.identity.as_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    format!(
        "paired-scoped-window-limit-restoration-pair:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_scoped_window_limit_restoration_fold(
    previous: Option<&QueryPairedScopedWindowLimitRestorationFold>,
    pair_identity: &str,
    window_fold: &QueryPairedScopedWindowCompactionFold,
    limit_fold: &QueryPairedLimitAggregateRestorationFold,
) -> QueryPairedScopedWindowLimitRestorationFold {
    if let Some(previous) = previous
        && previous.scoped_window_fold_identity == window_fold.identity
        && previous.limit_restoration_fold_identity == limit_fold.identity
    {
        return previous.clone();
    }

    let overflowed = window_fold.overflowed || limit_fold.overflowed;
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-scoped-window-limit-restoration-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_part(&mut hash, window_fold.identity.as_bytes());
    hash_part(&mut hash, limit_fold.identity.as_bytes());
    hash.update(window_fold.window_pair_count.to_be_bytes());
    hash.update(window_fold.window_stage_count.to_be_bytes());
    hash.update(limit_fold.limit_pair_count.to_be_bytes());
    hash.update(limit_fold.limit_stage_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);

    QueryPairedScopedWindowLimitRestorationFold {
        identity: format!(
            "paired-scoped-window-limit-restoration-fold:{}",
            hex(&hash.finalize())
        ),
        pair_identity: pair_identity.to_owned(),
        scoped_window_fold_identity: window_fold.identity.clone(),
        limit_restoration_fold_identity: limit_fold.identity.clone(),
        window_pair_count: window_fold.window_pair_count,
        window_stage_count: window_fold.window_stage_count,
        limit_pair_count: limit_fold.limit_pair_count,
        limit_stage_count: limit_fold.limit_stage_count,
        overflowed,
    }
}

fn query_paired_scoped_window_limit_restoration_transition(
    previous: Option<&QueryPairedScopedWindowLimitRestorationFold>,
    window_fold: &QueryPairedScopedWindowCompactionFold,
    limit_fold: &QueryPairedLimitAggregateRestorationFold,
) -> &'static str {
    let window_advanced = previous
        .is_none_or(|previous| previous.scoped_window_fold_identity != window_fold.identity);
    let limit_advanced = previous
        .is_none_or(|previous| previous.limit_restoration_fold_identity != limit_fold.identity);
    match (window_advanced, limit_advanced) {
        (true, true) => "advanced_window_and_limit_restoration",
        (true, false) => "advanced_window_scope",
        (false, true) => "advanced_limit_restoration",
        (false, false) => "carried_across_sparse_input",
    }
}

fn add_paired_scoped_window_limit_restoration_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedScopedWindowLimitRestorationFold,
    transition: &str,
) {
    details.insert(
        "paired_scoped_window_limit_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_pair_identity".to_owned(),
        PlanDetail::Text(fold.pair_identity.clone()),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_pairing".to_owned(),
        PlanDetail::Text(
            "latest_scoped_window_chain_with_paired_limit_aggregate_restoration".to_owned(),
        ),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_window_fold_identity".to_owned(),
        PlanDetail::Text(fold.scoped_window_fold_identity.clone()),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_limit_fold_identity".to_owned(),
        PlanDetail::Text(fold.limit_restoration_fold_identity.clone()),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_window_pair_count".to_owned(),
        PlanDetail::Integer(fold.window_pair_count),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_window_stage_count".to_owned(),
        PlanDetail::Integer(fold.window_stage_count),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_limit_pair_count".to_owned(),
        PlanDetail::Integer(fold.limit_pair_count),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_limit_stage_count".to_owned(),
        PlanDetail::Integer(fold.limit_stage_count),
    );
    details.insert(
        "paired_scoped_window_limit_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

fn query_paired_limit_window_compaction_pair_identity(
    limit_aggregate_pair_identity: Option<&str>,
    window_compaction_pair_identity: Option<&str>,
) -> Option<String> {
    if limit_aggregate_pair_identity.is_none() && window_compaction_pair_identity.is_none() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-window-compaction-pair.v1\0");
    hash_optional_text(&mut hash, limit_aggregate_pair_identity);
    hash_optional_text(&mut hash, window_compaction_pair_identity);
    Some(format!(
        "paired-limit-window-compaction-pair:{}",
        hex(&hash.finalize())
    ))
}

/// Combines cumulative limit/aggregate restoration and window compaction
/// histories. Either exact pair advances the identity; all counters and spill
/// estimates reflect the accumulated chains, including through sparse joins.
fn query_paired_limit_window_compaction_fold(
    previous: Option<&QueryPairedLimitWindowCompactionFold>,
    pair_identity: &str,
    limit_aggregate_fold: Option<&QueryPairedLimitAggregateRestorationFold>,
    window_compaction_fold: Option<&QueryPairedWindowCompactionSpillFold>,
) -> QueryPairedLimitWindowCompactionFold {
    let limit_pair_count = limit_aggregate_fold.map_or(0, |fold| fold.limit_pair_count);
    let limit_stage_count = limit_aggregate_fold.map_or(0, |fold| fold.limit_stage_count);
    let aggregate_pair_count = window_compaction_fold
        .map(|fold| fold.window_pair_count)
        .or_else(|| limit_aggregate_fold.map(|fold| fold.aggregate_pair_count))
        .unwrap_or(0);
    let aggregate_stage_count = window_compaction_fold
        .map(|fold| fold.window_stage_count)
        .or_else(|| limit_aggregate_fold.map(|fold| fold.aggregate_stage_count))
        .unwrap_or(0);
    let window_pair_count = window_compaction_fold.map_or(0, |fold| fold.window_pair_count);
    let window_stage_count = window_compaction_fold.map_or(0, |fold| fold.window_stage_count);
    let spill_pair_count = window_compaction_fold.map_or(0, |fold| fold.spill_pair_count);
    let spill_totals = window_compaction_fold.and_then(|fold| fold.spill_totals);
    let overflowed = limit_aggregate_fold.is_some_and(|fold| fold.overflowed)
        || window_compaction_fold.is_some_and(|fold| fold.overflowed)
        || spill_totals.is_some_and(|totals| totals.overflowed);

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-window-compaction-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_identity.as_bytes());
    hash_optional_text(
        &mut hash,
        limit_aggregate_fold.map(|fold| fold.identity.as_str()),
    );
    hash_optional_text(
        &mut hash,
        window_compaction_fold.map(|fold| fold.identity.as_str()),
    );
    for count in [
        limit_pair_count,
        limit_stage_count,
        aggregate_pair_count,
        aggregate_stage_count,
        window_pair_count,
        window_stage_count,
        spill_pair_count,
    ] {
        hash.update(count.to_be_bytes());
    }
    if let Some(totals) = spill_totals {
        hash.update([1]);
        hash_query_aggregate_spill_totals(&mut hash, totals);
    } else {
        hash.update([0]);
    }
    hash.update([u8::from(overflowed)]);

    QueryPairedLimitWindowCompactionFold {
        identity: format!(
            "paired-limit-window-compaction-fold:{}",
            hex(&hash.finalize())
        ),
        limit_aggregate_identity: limit_aggregate_fold.map(|fold| fold.identity.clone()),
        window_compaction_identity: window_compaction_fold.map(|fold| fold.identity.clone()),
        limit_pair_count,
        limit_stage_count,
        aggregate_pair_count,
        aggregate_stage_count,
        window_pair_count,
        window_stage_count,
        spill_pair_count,
        spill_totals,
        overflowed,
    }
}

fn add_paired_limit_window_compaction_pair_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_limit_window_compaction_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_limit_window_compaction_pairing".to_owned(),
        PlanDetail::Text(
            "exact_limit_aggregate_restore_and_window_compaction_spill_pair_events".to_owned(),
        ),
    );
}

fn add_paired_limit_window_compaction_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedLimitWindowCompactionFold,
    advanced_on_pair: bool,
) {
    details.insert(
        "paired_limit_window_compaction_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_limit_window_compaction_fold_pairing".to_owned(),
        PlanDetail::Text(
            "sparse_limit_restore_chains_with_nested_aggregate_compaction_and_spill_history"
                .to_owned(),
        ),
    );
    for (key, value) in [
        (
            "paired_limit_window_compaction_limit_aggregate_fold_identity",
            fold.limit_aggregate_identity.as_deref(),
        ),
        (
            "paired_limit_window_compaction_window_spill_fold_identity",
            fold.window_compaction_identity.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(key);
        }
    }
    details.insert(
        "paired_limit_window_compaction_fold_transition".to_owned(),
        PlanDetail::Text(if advanced_on_pair {
            "advanced_restore_or_compaction_pair".to_owned()
        } else {
            "carried_across_sparse_input".to_owned()
        }),
    );
    for (key, value) in [
        (
            "paired_limit_window_compaction_limit_pair_count",
            fold.limit_pair_count,
        ),
        (
            "paired_limit_window_compaction_limit_stage_count",
            fold.limit_stage_count,
        ),
        (
            "paired_limit_window_compaction_aggregate_pair_count",
            fold.aggregate_pair_count,
        ),
        (
            "paired_limit_window_compaction_aggregate_stage_count",
            fold.aggregate_stage_count,
        ),
        (
            "paired_limit_window_compaction_window_pair_count",
            fold.window_pair_count,
        ),
        (
            "paired_limit_window_compaction_window_stage_count",
            fold.window_stage_count,
        ),
        (
            "paired_limit_window_compaction_spill_pair_count",
            fold.spill_pair_count,
        ),
    ] {
        details.insert(key.to_owned(), PlanDetail::Integer(value));
    }
    let totals = fold.spill_totals;
    details.insert(
        "paired_limit_window_compaction_spill_stage_count".to_owned(),
        PlanDetail::Integer(totals.map_or(0, |totals| totals.stage_count)),
    );
    details.insert(
        "paired_limit_window_compaction_spill_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(totals.map_or(0, |totals| totals.unknown_working_set_count)),
    );
    details.insert(
        "paired_limit_window_compaction_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    let status = match totals {
        None => "no_spill_pairs",
        Some(totals) if fold.overflowed || totals.overflowed => "overflow",
        Some(totals) if totals.unknown_working_set_count > 0 => "unknown_working_set",
        Some(_) => "computed",
    };
    details.insert(
        "paired_limit_window_compaction_estimate_status".to_owned(),
        PlanDetail::Text(status.to_owned()),
    );
    for (key, value) in [
        (
            "paired_limit_window_compaction_estimated_bytes",
            totals.and_then(|totals| totals.estimated_bytes),
        ),
        (
            "paired_limit_window_compaction_estimated_io_blocks",
            totals.and_then(|totals| totals.estimated_io_blocks),
        ),
        (
            "paired_limit_window_compaction_estimated_io_work",
            totals.and_then(|totals| totals.estimated_io_work),
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
}

/// Sums spill estimates in the resolver-ordered aggregate chain for one
/// resolved pair. Missing working sets keep each corresponding total unknown;
/// checked arithmetic avoids turning overflow into a plausible cost.
fn query_aggregate_spill_totals(
    pair: &QueryJoinPairIdentityDescription,
    source: &ObjectRef,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
) -> Option<QueryAggregateSpillTotals> {
    let matching = window_aggregates
        .iter()
        .filter(|aggregate| aggregate.source == *source)
        .filter_map(|aggregate| {
            window_spills.iter().find(|spill| {
                spill.join_pair_identity == pair.identity
                    && spill.source == *source
                    && spill.window_aggregate_identity == aggregate.identity
            })
            .map(|spill| (aggregate, spill))
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }

    let mut estimated_bytes = Some(0_u64);
    let mut estimated_io_blocks = Some(0_u64);
    let mut estimated_io_work = Some(0_u64);
    let mut unknown_working_set_count = 0_u64;
    let mut overflowed = false;
    for (_, spill) in &matching {
        if spill.estimated_working_set_bytes.is_none() {
            unknown_working_set_count = unknown_working_set_count.saturating_add(1);
        }
        let (bytes, blocks, work) = query_window_spill_estimate(spill);
        for (total, next) in [
            (&mut estimated_bytes, bytes),
            (&mut estimated_io_blocks, blocks),
            (&mut estimated_io_work, work),
        ] {
            *total = match (*total, next) {
                (Some(total), Some(next)) => match total.checked_add(next) {
                    Some(sum) => Some(sum),
                    None => {
                        overflowed = true;
                        None
                    }
                },
                _ => None,
            };
        }
        if spill.estimated_working_set_bytes.is_some() && work.is_none() {
            overflowed = true;
        }
    }
    Some(QueryAggregateSpillTotals {
        stage_count: u64::try_from(matching.len()).unwrap_or(u64::MAX),
        unknown_working_set_count,
        estimated_bytes,
        estimated_io_blocks,
        estimated_io_work,
        overflowed,
    })
}

/// Binds an exact aggregate restoration pair to its optional exact spill pair.
fn query_paired_aggregate_spill_restoration_pair_identity(
    aggregate_pair_identity: Option<&str>,
    spill_pair_identity: Option<&str>,
) -> Option<String> {
    if aggregate_pair_identity.is_none() && spill_pair_identity.is_none() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-aggregate-spill-restoration-pair.v1\0");
    hash_optional_text(&mut hash, aggregate_pair_identity);
    hash_optional_text(&mut hash, spill_pair_identity);
    Some(format!(
        "paired-aggregate-spill-restoration-pair:{}",
        hex(&hash.finalize())
    ))
}

fn add_paired_aggregate_spill_restoration_pair_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    aggregate_pair_identity: Option<&str>,
    spill_pair_identity: Option<&str>,
) {
    details.insert(
        "paired_aggregate_spill_restoration_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_aggregate_spill_restoration_pair_resolution".to_owned(),
        PlanDetail::Text("exact_aggregate_chain_with_optional_exact_spill_chain".to_owned()),
    );
    details.insert(
        "paired_aggregate_spill_restoration_pair_has_spill".to_owned(),
        PlanDetail::Boolean(spill_pair_identity.is_some()),
    );
    for (key, value) in [
        (
            "paired_aggregate_spill_restoration_pair_aggregate_identity",
            aggregate_pair_identity,
        ),
        (
            "paired_aggregate_spill_restoration_pair_spill_identity",
            spill_pair_identity,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(key);
        }
    }
}

fn query_paired_aggregate_spill_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    aggregate_chain_identity: &str,
    spill_chain_identity: &str,
    totals: QueryAggregateSpillTotals,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-aggregate-spill-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, aggregate_chain_identity.as_bytes());
    hash_part(&mut hash, spill_chain_identity.as_bytes());
    hash.update(totals.stage_count.to_be_bytes());
    hash.update(totals.unknown_working_set_count.to_be_bytes());
    hash_optional_u64(&mut hash, totals.estimated_bytes);
    hash_optional_u64(&mut hash, totals.estimated_io_blocks);
    hash_optional_u64(&mut hash, totals.estimated_io_work);
    hash.update([u8::from(totals.overflowed)]);
    format!("paired-aggregate-spill-anchor-fold:{}", hex(&hash.finalize()))
}

fn add_paired_aggregate_spill_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    totals: &QueryAggregateSpillTotals,
) {
    details.insert(
        "paired_aggregate_spill_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_aggregate_spill_anchor_fold_pairing".to_owned(),
        PlanDetail::Text(
            "sparse_anchor_fold_resolved_join_pair_aggregate_chain_and_spill_totals".to_owned(),
        ),
    );
    details.insert(
        "aggregate_spill_stage_count".to_owned(),
        PlanDetail::Integer(totals.stage_count),
    );
    details.insert(
        "aggregate_spill_unknown_working_set_count".to_owned(),
        PlanDetail::Integer(totals.unknown_working_set_count),
    );
    details.insert(
        "aggregate_spill_overflowed".to_owned(),
        PlanDetail::Boolean(totals.overflowed),
    );
    for (key, value) in [
        ("aggregate_spill_estimated_bytes", totals.estimated_bytes),
        (
            "aggregate_spill_estimated_io_blocks",
            totals.estimated_io_blocks,
        ),
        (
            "aggregate_spill_estimated_io_work",
            totals.estimated_io_work,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
    details.insert(
        "aggregate_spill_estimate_status".to_owned(),
        PlanDetail::Text(if totals.overflowed {
            "overflow".to_owned()
        } else if totals.unknown_working_set_count > 0 {
            "unknown_working_set".to_owned()
        } else {
            "computed".to_owned()
        }),
    );
}

/// Binds the exact limit chain to the optional aggregate chain on that pair.
fn query_paired_limit_aggregate_restoration_pair_identity(
    limit_pair_identity: Option<&str>,
    aggregate_pair_identity: Option<&str>,
) -> Option<String> {
    if limit_pair_identity.is_none() && aggregate_pair_identity.is_none() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-aggregate-restoration-pair.v1\0");
    hash_optional_text(&mut hash, limit_pair_identity);
    hash_optional_text(&mut hash, aggregate_pair_identity);
    Some(format!(
        "paired-limit-aggregate-restoration-pair:{}",
        hex(&hash.finalize())
    ))
}

fn add_paired_limit_aggregate_restoration_pair_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
    limit_pair_identity: Option<&str>,
    aggregate_pair_identity: Option<&str>,
) {
    details.insert(
        "paired_limit_aggregate_restoration_pair_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_limit_aggregate_restoration_pair_resolution".to_owned(),
        PlanDetail::Text("exact_limit_chain_with_optional_exact_aggregate_chain".to_owned()),
    );
    details.insert(
        "paired_limit_aggregate_restoration_pair_has_limit".to_owned(),
        PlanDetail::Boolean(limit_pair_identity.is_some()),
    );
    details.insert(
        "paired_limit_aggregate_restoration_pair_has_aggregate".to_owned(),
        PlanDetail::Boolean(aggregate_pair_identity.is_some()),
    );
    for (key, value) in [
        (
            "paired_limit_aggregate_restoration_pair_limit_identity",
            limit_pair_identity,
        ),
        (
            "paired_limit_aggregate_restoration_pair_aggregate_identity",
            aggregate_pair_identity,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(key);
        }
    }
}

/// Binds an exact pair's ordered limit chain to its right-input identity and
/// the accumulated sparse left anchor. ORNA leaves this explain-only digest
/// encoding open; the domain-separated result stays stable as known joins are
/// cost-reordered around an unknown tail.
fn query_paired_limit_pushdown_anchor_fold_identity(
    anchor_fold_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    limit_chain_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-pushdown-anchor-fold.v1\0");
    hash_part(&mut hash, anchor_fold_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, limit_chain_identity.as_bytes());
    format!(
        "paired-limit-pushdown-anchor-fold:{}",
        hex(&hash.finalize())
    )
}

fn add_paired_limit_pushdown_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_limit_pushdown_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_limit_pushdown_anchor_fold_pairing".to_owned(),
        PlanDetail::Text("sparse_anchor_fold_resolved_join_pair_and_limit_chain".to_owned()),
    );
}

/// Binds a logical join fold to the exact pair's ordered input-limit fold.
/// ORNA leaves this explain-only identity encoding open; hashing both parent
/// digests keeps a limit chain from being reassociated with another join.
fn query_paired_join_limit_anchor_fold_identity(
    pair: &QueryJoinPairIdentityDescription,
    join_anchor_fold_identity: &str,
    limit_anchor_fold_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-join-limit-anchor-fold.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(&mut hash, pair.predicate.as_ref().map(ExpressionRef::as_str));
    hash_part(&mut hash, join_anchor_fold_identity.as_bytes());
    hash_part(&mut hash, limit_anchor_fold_identity.as_bytes());
    format!(
        "paired-join-limit-anchor-fold:{}",
        hex(&hash.finalize())
    )
}

fn add_paired_join_limit_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_join_limit_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_join_limit_anchor_fold_pairing".to_owned(),
        PlanDetail::Text("sparse_join_pair_anchor_and_exact_limit_anchor_chain".to_owned()),
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryJoinLimitAnchorCascadeFold {
    identity: String,
    pair_count: u64,
    stage_count: u64,
    overflowed: bool,
}

fn query_paired_join_limit_anchor_cascade_fold(
    previous: Option<&QueryJoinLimitAnchorCascadeFold>,
    pair_anchor_fold_identity: &str,
    limit_stage_count: u64,
) -> QueryJoinLimitAnchorCascadeFold {
    let mut overflowed = previous.is_some_and(|previous| previous.overflowed);
    let (pair_count, stage_count) = if let Some(previous) = previous {
        let pair_count = previous.pair_count.checked_add(1).unwrap_or_else(|| {
            overflowed = true;
            u64::MAX
        });
        let stage_count = previous
            .stage_count
            .checked_add(limit_stage_count)
            .unwrap_or_else(|| {
                overflowed = true;
                u64::MAX
            });
        (pair_count, stage_count)
    } else {
        (1, limit_stage_count)
    };
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-join-limit-anchor-cascade-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_anchor_fold_identity.as_bytes());
    hash.update(pair_count.to_be_bytes());
    hash.update(stage_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);
    QueryJoinLimitAnchorCascadeFold {
        identity: format!(
            "paired-join-limit-anchor-cascade-fold:{}",
            hex(&hash.finalize())
        ),
        pair_count,
        stage_count,
        overflowed,
    }
}

fn add_paired_join_limit_anchor_cascade_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryJoinLimitAnchorCascadeFold,
) {
    details.insert(
        "paired_join_limit_anchor_cascade_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_join_limit_anchor_cascade_pairing".to_owned(),
        PlanDetail::Text("planned_pair_order_sparse_limit_anchor_chains".to_owned()),
    );
    details.insert(
        "paired_join_limit_anchor_cascade_pair_count".to_owned(),
        PlanDetail::Integer(fold.pair_count),
    );
    details.insert(
        "paired_join_limit_anchor_cascade_stage_count".to_owned(),
        PlanDetail::Integer(fold.stage_count),
    );
    details.insert(
        "paired_join_limit_anchor_cascade_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

/// Combines the exact pair's anchored ordered limit chain with the window
/// chain attached to that same right input. ORNA leaves this explain-only
/// digest open; the input identities prevent either chain from sliding across
/// a sparse join boundary.
fn query_paired_limit_window_anchor_fold_identity(
    pair: &QueryJoinPairIdentityDescription,
    limit_anchor_fold_identity: &str,
    window_chain_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-window-anchor-fold.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(&mut hash, pair.predicate.as_ref().map(ExpressionRef::as_str));
    hash_part(&mut hash, limit_anchor_fold_identity.as_bytes());
    hash_part(&mut hash, window_chain_identity.as_bytes());
    format!(
        "paired-limit-window-anchor-fold:{}",
        hex(&hash.finalize())
    )
}

fn add_paired_limit_window_anchor_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "paired_limit_window_anchor_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "paired_limit_window_anchor_fold_pairing".to_owned(),
        PlanDetail::Text(
            "exact_join_pair_ordered_limit_chain_and_right_window_chain".to_owned(),
        ),
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryLimitWindowCascadeFold {
    identity: String,
    pair_count: u64,
    stage_count: u64,
    estimated_post_limit_rows: Option<u64>,
    estimated_post_limit_bytes: Option<u64>,
    unknown_estimate_pair_count: u64,
    overflowed: bool,
}

/// Appends a pair only when it has both an ordered input-limit chain and a
/// window chain. Pairs without both continue to carry the accumulated fold
/// through the surrounding sparse join plan.
fn query_paired_limit_window_cascade_fold(
    previous: Option<&QueryLimitWindowCascadeFold>,
    pair_anchor_fold_identity: &str,
    limit_stage_count: u64,
    post_limit_cardinality: Cardinality,
) -> QueryLimitWindowCascadeFold {
    let mut overflowed = false;
    let (pair_count, stage_count, rows, bytes, unknown_estimate_pair_count) =
        if let Some(previous) = previous {
            let pair_count = previous.pair_count.checked_add(1).unwrap_or_else(|| {
                overflowed = true;
                u64::MAX
            });
            let stage_count = previous
                .stage_count
                .checked_add(limit_stage_count)
                .unwrap_or_else(|| {
                    overflowed = true;
                    u64::MAX
                });
            let unknown_estimate_pair_count = previous
                .unknown_estimate_pair_count
                .checked_add(if post_limit_cardinality.rows.is_none()
                    || post_limit_cardinality.bytes.is_none()
                {
                    1
                } else {
                    0
                })
                .unwrap_or_else(|| {
                    overflowed = true;
                    u64::MAX
                });
            (
                pair_count,
                stage_count,
                merge_limit_window_estimate(
                    previous.estimated_post_limit_rows,
                    post_limit_cardinality.rows,
                    &mut overflowed,
                ),
                merge_limit_window_estimate(
                    previous.estimated_post_limit_bytes,
                    post_limit_cardinality.bytes,
                    &mut overflowed,
                ),
                unknown_estimate_pair_count,
            )
        } else {
            (
                1,
                limit_stage_count,
                post_limit_cardinality.rows,
                post_limit_cardinality.bytes,
                if post_limit_cardinality.rows.is_none()
                    || post_limit_cardinality.bytes.is_none()
                {
                    1
                } else {
                    0
                },
            )
        };
    let overflowed = overflowed || previous.is_some_and(|previous| previous.overflowed);
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-limit-window-cascade-fold.v1\0");
    hash_optional_text(
        &mut hash,
        previous.map(|previous| previous.identity.as_str()),
    );
    hash_part(&mut hash, pair_anchor_fold_identity.as_bytes());
    hash.update(pair_count.to_be_bytes());
    hash.update(stage_count.to_be_bytes());
    hash_optional_u64(&mut hash, rows);
    hash_optional_u64(&mut hash, bytes);
    hash.update(unknown_estimate_pair_count.to_be_bytes());
    hash.update([u8::from(overflowed)]);
    QueryLimitWindowCascadeFold {
        identity: format!(
            "paired-limit-window-cascade-fold:{}",
            hex(&hash.finalize())
        ),
        pair_count,
        stage_count,
        estimated_post_limit_rows: rows,
        estimated_post_limit_bytes: bytes,
        unknown_estimate_pair_count,
        overflowed,
    }
}

fn merge_limit_window_estimate(
    previous: Option<u64>,
    current: Option<u64>,
    overflowed: &mut bool,
) -> Option<u64> {
    match (previous, current) {
        (Some(previous), Some(current)) => previous.checked_add(current).or_else(|| {
            *overflowed = true;
            None
        }),
        _ => None,
    }
}

fn add_paired_limit_window_cascade_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryLimitWindowCascadeFold,
) {
    details.insert(
        "paired_limit_window_cascade_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_limit_window_cascade_fold_pairing".to_owned(),
        PlanDetail::Text("planned_pair_order_sparse_window_anchor_chains".to_owned()),
    );
    details.insert(
        "limit_window_cascade_pair_count".to_owned(),
        PlanDetail::Integer(fold.pair_count),
    );
    details.insert(
        "limit_window_cascade_stage_count".to_owned(),
        PlanDetail::Integer(fold.stage_count),
    );
    details.insert(
        "limit_window_cascade_unknown_estimate_pair_count".to_owned(),
        PlanDetail::Integer(fold.unknown_estimate_pair_count),
    );
    details.insert(
        "limit_window_cascade_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    for (key, value) in [
        (
            "limit_window_cascade_estimated_post_limit_rows",
            fold.estimated_post_limit_rows,
        ),
        (
            "limit_window_cascade_estimated_post_limit_bytes",
            fold.estimated_post_limit_bytes,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Integer(value));
        } else {
            details.remove(key);
        }
    }
    details.insert(
        "limit_window_cascade_estimate_status".to_owned(),
        PlanDetail::Text(if fold.overflowed {
            "overflow".to_owned()
        } else if fold.unknown_estimate_pair_count > 0 {
            "unknown_estimate".to_owned()
        } else {
            "computed".to_owned()
        }),
    );
}

fn add_decorrelated_predicate_pushdown_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "decorrelated_predicate_pushdown_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "decorrelated_predicate_pushdown_pairing".to_owned(),
        PlanDetail::Text("resolver_subquery_and_exact_index_pair".to_owned()),
    );
}

fn add_decorrelated_index_selection_omission_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "decorrelated_index_selection_omission_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "decorrelated_index_selection_omission_reason".to_owned(),
        PlanDetail::Text("no_exact_table_and_predicate_candidate".to_owned()),
    );
}

fn add_partial_index_pair_identity_details(
    details: &mut BTreeMap<String, PlanDetail>,
    candidate: &QueryPartialIndexDescription,
) {
    details.insert(
        "predicate_pushdown_identity".to_owned(),
        PlanDetail::Text(partial_index_pair_identity(candidate)),
    );
    details.insert(
        "predicate_pushdown_pairing".to_owned(),
        PlanDetail::Text("exact_table_index_and_predicate".to_owned()),
    );
}

fn add_partial_index_selection_tie_break_details(details: &mut BTreeMap<String, PlanDetail>) {
    details.insert(
        "index_selection_tie_break".to_owned(),
        PlanDetail::Text("lexicographically_smallest_matching_index_identity".to_owned()),
    );
}

/// The reference defines structured plan rows and a digest of plan inputs,
/// but leaves join-fold identity open. Seed a cascade from the pinned source
/// and any aggregate work already attached to that source.
fn query_join_cost_fold_seed(
    snapshot: &SnapshotRef,
    source: &ObjectRef,
    statistics: Option<&QuerySourceStatistics>,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-join-cost-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, source.as_str().as_bytes());
    hash_query_statistics(&mut hash, statistics);
    hash_query_window_inputs(&mut hash, source, window_aggregates);
    format!("join-seed:{}", hex(&hash.finalize()))
}

fn query_join_cost_input_identity(
    snapshot: &SnapshotRef,
    join: &QueryJoinDescription,
    selected_index: Option<&QueryPartialIndexDescription>,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    decorrelated_subquery: Option<&QueryDecorrelatedSubqueryDescription>,
    decorrelated_predicate_pushdown_identity: Option<&str>,
    decorrelated_index_omission_identity: Option<&str>,
    limit_pushdown_chain_identity: Option<&str>,
    window_spill_chain_identity: Option<&str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-join-cost-input.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, join.source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        join.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_query_statistics(&mut hash, join.statistics.as_ref());
    if let Some(identity) = decorrelated_predicate_pushdown_identity {
        hash.update([2]);
        hash_part(&mut hash, identity.as_bytes());
    } else {
        if let Some(subquery) = decorrelated_subquery {
            hash.update([1]);
            hash_part(&mut hash, subquery.identity.as_str().as_bytes());
            hash_part(&mut hash, subquery.source.as_str().as_bytes());
            hash_part(
                &mut hash,
                subquery.correlation_predicate.as_str().as_bytes(),
            );
        } else {
            hash.update([0]);
        }
        if let Some(index) = selected_index {
            hash.update([1]);
            hash_part(
                &mut hash,
                partial_index_pair_identity(index).as_bytes(),
            );
        } else {
            hash.update([0]);
        }
    }
    if let Some(identity) = decorrelated_index_omission_identity {
        hash.update([3]);
        hash_part(&mut hash, identity.as_bytes());
    }
    hash_query_window_inputs(&mut hash, &join.source, window_aggregates);
    if let Some(identity) = limit_pushdown_chain_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    if let Some(identity) = window_spill_chain_identity {
        hash.update([4]);
        hash_part(&mut hash, identity.as_bytes());
    }
    format!("join-input:{}", hex(&hash.finalize()))
}

/// Starts the paired-index anchor chain at its current sparse cost ancestry.
fn query_paired_index_anchor_chain_seed_identity(parent_cost_identity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-anchor-chain-seed.v1\0");
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!("paired-index-anchor-chain:{}", hex(&hash.finalize()))
}

/// Seeds a route-restoration chain from the planned cost ancestry. The
/// explain contract does not prescribe how reordered pair routes are restored;
/// this digest keeps the route outcomes keyed by their stable logical pairs.
fn query_paired_index_cost_restoration_seed_identity(parent_cost_identity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-cost-restoration-seed.v1\0");
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!(
        "paired-index-cost-restoration:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_checkpoint_cost_restoration_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-checkpoint-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!(
        "paired-checkpoint-cost-restoration:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_checkpoint_segment_compaction_chain_identity(
    chain: &QueryPairedCheckpointSegmentCompactionChainDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-checkpoint-segment-compaction-chain.v1\0");
    hash_part(&mut hash, chain.join_pair_identity.as_str().as_bytes());
    hash_part(&mut hash, chain.branch.name.as_bytes());
    hash.update(chain.branch.generation.to_be_bytes());
    hash.update((chain.steps.len() as u64).to_be_bytes());
    for step in &chain.steps {
        hash_part(&mut hash, step.checkpoint_identity.as_str().as_bytes());
        hash_optional_text(
            &mut hash,
            step.left_segment_identity
                .as_ref()
                .map(ObjectRef::as_str),
        );
        hash_optional_text(
            &mut hash,
            step.right_segment_identity
                .as_ref()
                .map(ObjectRef::as_str),
        );
        hash_optional_text(
            &mut hash,
            step.left_compaction_identity
                .as_ref()
                .map(ObjectRef::as_str),
        );
        hash_optional_text(
            &mut hash,
            step.right_compaction_identity
                .as_ref()
                .map(ObjectRef::as_str),
        );
    }
    format!("paired-checkpoint-segment-chain:{}", hex(&hash.finalize()))
}

fn add_checkpoint_fold_count(total: &mut u64, increment: usize, overflowed: &mut bool) {
    let increment = u64::try_from(increment).unwrap_or_else(|_| {
        *overflowed = true;
        u64::MAX
    });
    *total = total.checked_add(increment).unwrap_or_else(|| {
        *overflowed = true;
        u64::MAX
    });
}

fn query_paired_checkpoint_cost_restoration_fold(
    previous: Option<&QueryPairedCheckpointCostRestorationFold>,
    parent_identity: &str,
    parent_cost_identity: &str,
    paired_index_cost_compaction_identity: Option<&str>,
    chain: &QueryPairedCheckpointSegmentCompactionChainDescription,
    chain_identity: &str,
) -> QueryPairedCheckpointCostRestorationFold {
    let mut fold = previous.cloned().unwrap_or_else(|| {
        QueryPairedCheckpointCostRestorationFold {
            identity: parent_identity.to_owned(),
            chain_count: 0,
            checkpoint_count: 0,
            left_segment_count: 0,
            right_segment_count: 0,
            left_compaction_count: 0,
            right_compaction_count: 0,
            branch_scopes: BTreeSet::new(),
            overflowed: false,
        }
    });
    add_checkpoint_fold_count(&mut fold.chain_count, 1, &mut fold.overflowed);
    add_checkpoint_fold_count(
        &mut fold.checkpoint_count,
        chain.steps.len(),
        &mut fold.overflowed,
    );
    add_checkpoint_fold_count(
        &mut fold.left_segment_count,
        chain.steps.iter().filter(|step| step.left_segment_identity.is_some()).count(),
        &mut fold.overflowed,
    );
    add_checkpoint_fold_count(
        &mut fold.right_segment_count,
        chain.steps.iter().filter(|step| step.right_segment_identity.is_some()).count(),
        &mut fold.overflowed,
    );
    add_checkpoint_fold_count(
        &mut fold.left_compaction_count,
        chain.steps.iter().filter(|step| step.left_compaction_identity.is_some()).count(),
        &mut fold.overflowed,
    );
    add_checkpoint_fold_count(
        &mut fold.right_compaction_count,
        chain.steps.iter().filter(|step| step.right_compaction_identity.is_some()).count(),
        &mut fold.overflowed,
    );
    fold.branch_scopes
        .insert((chain.branch.name.clone(), chain.branch.generation));

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-checkpoint-cost-restoration-fold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_optional_text(&mut hash, paired_index_cost_compaction_identity);
    hash_part(&mut hash, chain_identity.as_bytes());
    hash.update(fold.chain_count.to_be_bytes());
    hash.update(fold.checkpoint_count.to_be_bytes());
    hash.update(fold.left_segment_count.to_be_bytes());
    hash.update(fold.right_segment_count.to_be_bytes());
    hash.update(fold.left_compaction_count.to_be_bytes());
    hash.update(fold.right_compaction_count.to_be_bytes());
    hash.update((fold.branch_scopes.len() as u64).to_be_bytes());
    hash.update([u8::from(fold.overflowed)]);
    fold.identity = format!(
        "paired-checkpoint-cost-restoration:{}",
        hex(&hash.finalize())
    );
    fold
}

fn add_paired_checkpoint_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedCheckpointCostRestorationFold,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    chain_identity: Option<&str>,
    chain: Option<&QueryPairedCheckpointSegmentCompactionChainDescription>,
) {
    details.insert(
        "paired_checkpoint_cost_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_chain_count".to_owned(),
        PlanDetail::Integer(fold.chain_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_branch_scope_count".to_owned(),
        PlanDetail::Integer(fold.branch_scopes.len() as u64),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_checkpoint_count".to_owned(),
        PlanDetail::Integer(fold.checkpoint_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_left_segment_count".to_owned(),
        PlanDetail::Integer(fold.left_segment_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_right_segment_count".to_owned(),
        PlanDetail::Integer(fold.right_segment_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_left_compaction_count".to_owned(),
        PlanDetail::Integer(fold.left_compaction_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_right_compaction_count".to_owned(),
        PlanDetail::Integer(fold.right_compaction_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_policy".to_owned(),
        PlanDetail::Text("branch_local_cost_fold_with_ordered_sparse_checkpoint_segments".to_owned()),
    );
    if let Some(pair_identity) = pair_identity {
        details.insert(
            "paired_checkpoint_cost_restoration_pair_identity".to_owned(),
            PlanDetail::Text(pair_identity.to_owned()),
        );
    }
    if let Some(chain_identity) = chain_identity {
        details.insert(
            "paired_checkpoint_cost_restoration_step_chain_identity".to_owned(),
            PlanDetail::Text(chain_identity.to_owned()),
        );
    }
    if let Some(chain) = chain {
        details.insert(
            "paired_checkpoint_cost_restoration_branch_identity".to_owned(),
            PlanDetail::Text(chain.branch.name.clone()),
        );
        details.insert(
            "paired_checkpoint_cost_restoration_branch_generation".to_owned(),
            PlanDetail::Integer(chain.branch.generation),
        );
    }
}

fn add_paired_checkpoint_cost_restoration_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedCheckpointCostRestorationFold,
) {
    details.insert(
        "paired_checkpoint_cost_restoration_output_policy".to_owned(),
        PlanDetail::Text("branch_local_cost_fold_with_ordered_sparse_checkpoint_segments".to_owned()),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_chain_count".to_owned(),
        PlanDetail::Integer(fold.chain_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_branch_scope_count".to_owned(),
        PlanDetail::Integer(fold.branch_scopes.len() as u64),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_checkpoint_count".to_owned(),
        PlanDetail::Integer(fold.checkpoint_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_left_segment_count".to_owned(),
        PlanDetail::Integer(fold.left_segment_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_right_segment_count".to_owned(),
        PlanDetail::Integer(fold.right_segment_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_left_compaction_count".to_owned(),
        PlanDetail::Integer(fold.left_compaction_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_right_compaction_count".to_owned(),
        PlanDetail::Integer(fold.right_compaction_count),
    );
    details.insert(
        "paired_checkpoint_cost_restoration_output_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

fn query_paired_segment_rotation_cost_restoration_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-segment-rotation-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!(
        "paired-segment-rotation-cost-restoration:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_segment_rotation_chain_identity(
    chain: &QueryPairedSegmentRotationChainDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-segment-rotation-chain.v1\0");
    hash_part(&mut hash, chain.join_pair_identity.as_str().as_bytes());
    hash_part(&mut hash, chain.branch.name.as_bytes());
    hash.update(chain.branch.generation.to_be_bytes());
    hash.update((chain.rotations.len() as u64).to_be_bytes());
    for rotation in &chain.rotations {
        hash_part(&mut hash, rotation.checkpoint_identity.as_str().as_bytes());
        hash.update(rotation.source_stream_ordinal.to_be_bytes());
        hash.update(rotation.fold_ordinal.to_be_bytes());
        hash.update(rotation.order.to_be_bytes());
        hash_part(&mut hash, rotation.left_segment_identity.as_str().as_bytes());
        hash_part(&mut hash, rotation.right_segment_identity.as_str().as_bytes());
    }
    format!("paired-segment-rotation-chain:{}", hex(&hash.finalize()))
}

fn query_paired_segment_rotation_cost_restoration_fold(
    previous: Option<&QueryPairedSegmentRotationCostRestorationFold>,
    parent_identity: &str,
    parent_cost_identity: &str,
    checkpoint_cost_restoration_identity: Option<&str>,
    chain: &QueryPairedSegmentRotationChainDescription,
    chain_identity: &str,
) -> QueryPairedSegmentRotationCostRestorationFold {
    let mut fold = previous.cloned().unwrap_or_else(|| {
        QueryPairedSegmentRotationCostRestorationFold {
            identity: parent_identity.to_owned(),
            chain_count: 0,
            rotation_count: 0,
            nested_checkpoint_fold_count: 0,
            branch_scopes: BTreeSet::new(),
            source_streams: BTreeSet::new(),
            overflowed: false,
        }
    });
    add_checkpoint_fold_count(&mut fold.chain_count, 1, &mut fold.overflowed);
    add_checkpoint_fold_count(
        &mut fold.rotation_count,
        chain.rotations.len(),
        &mut fold.overflowed,
    );
    if checkpoint_cost_restoration_identity.is_some() {
        add_checkpoint_fold_count(
            &mut fold.nested_checkpoint_fold_count,
            1,
            &mut fold.overflowed,
        );
    }
    fold.branch_scopes
        .insert((chain.branch.name.clone(), chain.branch.generation));
    for rotation in &chain.rotations {
        fold.source_streams.insert((
            chain.join_pair_identity.as_str().to_owned(),
            chain.branch.name.clone(),
            chain.branch.generation,
            rotation.source_stream_ordinal,
        ));
    }

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-segment-rotation-cost-restoration-fold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_optional_text(&mut hash, checkpoint_cost_restoration_identity);
    hash_part(&mut hash, chain_identity.as_bytes());
    hash.update(fold.chain_count.to_be_bytes());
    hash.update(fold.rotation_count.to_be_bytes());
    hash.update(fold.nested_checkpoint_fold_count.to_be_bytes());
    hash.update((fold.branch_scopes.len() as u64).to_be_bytes());
    hash.update((fold.source_streams.len() as u64).to_be_bytes());
    hash.update([u8::from(fold.overflowed)]);
    fold.identity = format!(
        "paired-segment-rotation-cost-restoration:{}",
        hex(&hash.finalize())
    );
    fold
}

fn add_paired_segment_rotation_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedSegmentRotationCostRestorationFold,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    chain_identity: Option<&str>,
    checkpoint_cost_restoration_identity: Option<&str>,
    chain: Option<&QueryPairedSegmentRotationChainDescription>,
) {
    details.insert(
        "paired_segment_rotation_cost_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_parent_identity".to_owned(),
        PlanDetail::Text(parent_identity.to_owned()),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_parent_cost_identity".to_owned(),
        PlanDetail::Text(parent_cost_identity.to_owned()),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_chain_count".to_owned(),
        PlanDetail::Integer(fold.chain_count),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_rotation_count".to_owned(),
        PlanDetail::Integer(fold.rotation_count),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_nested_checkpoint_fold_count".to_owned(),
        PlanDetail::Integer(fold.nested_checkpoint_fold_count),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_branch_scope_count".to_owned(),
        PlanDetail::Integer(fold.branch_scopes.len() as u64),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_source_stream_count".to_owned(),
        PlanDetail::Integer(fold.source_streams.len() as u64),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_policy".to_owned(),
        PlanDetail::Text(
            "branch_local_checkpoint_nested_rotation_fold_preserves_source_ordinals".to_owned(),
        ),
    );
    for (key, value) in [
        (
            "paired_segment_rotation_cost_restoration_pair_identity",
            pair_identity,
        ),
        (
            "paired_segment_rotation_cost_restoration_chain_identity",
            chain_identity,
        ),
        (
            "paired_segment_rotation_cost_restoration_checkpoint_fold_identity",
            checkpoint_cost_restoration_identity,
        ),
    ] {
        if let Some(value) = value {
            details.insert(key.to_owned(), PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(key);
        }
    }
    if let Some(chain) = chain {
        details.insert(
            "paired_segment_rotation_cost_restoration_branch_identity".to_owned(),
            PlanDetail::Text(chain.branch.name.clone()),
        );
        details.insert(
            "paired_segment_rotation_cost_restoration_branch_generation".to_owned(),
            PlanDetail::Integer(chain.branch.generation),
        );
    }
}

fn add_paired_segment_rotation_cost_restoration_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedSegmentRotationCostRestorationFold,
) {
    details.insert(
        "paired_segment_rotation_cost_restoration_output_policy".to_owned(),
        PlanDetail::Text(
            "branch_local_checkpoint_nested_rotation_fold_preserves_source_ordinals".to_owned(),
        ),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_chain_count".to_owned(),
        PlanDetail::Integer(fold.chain_count),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_rotation_count".to_owned(),
        PlanDetail::Integer(fold.rotation_count),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_nested_checkpoint_fold_count".to_owned(),
        PlanDetail::Integer(fold.nested_checkpoint_fold_count),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_branch_scope_count".to_owned(),
        PlanDetail::Integer(fold.branch_scopes.len() as u64),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_source_stream_count".to_owned(),
        PlanDetail::Integer(fold.source_streams.len() as u64),
    );
    details.insert(
        "paired_segment_rotation_cost_restoration_output_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

fn query_paired_stream_compaction_cost_restoration_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-stream-compaction-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!("paired-stream-compaction-cost-restoration:{}", hex(&hash.finalize()))
}

fn query_paired_stream_compaction_chain_identity(
    chain: &QueryPairedStreamCompactionChainDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-stream-compaction-chain.v1\0");
    hash_part(&mut hash, chain.join_pair_identity.as_str().as_bytes());
    hash_part(&mut hash, chain.branch.name.as_bytes());
    hash.update(chain.branch.generation.to_be_bytes());
    hash.update((chain.occurrences.len() as u64).to_be_bytes());
    for occurrence in &chain.occurrences {
        hash_part(&mut hash, occurrence.checkpoint_identity.as_str().as_bytes());
        hash.update(occurrence.restore_ordinal.to_be_bytes());
        hash.update(occurrence.handoff_ordinal.to_be_bytes());
        hash.update(occurrence.source_stream_ordinal.to_be_bytes());
        hash.update(occurrence.compaction_ordinal.to_be_bytes());
        hash.update(occurrence.fold_ordinal.to_be_bytes());
        hash.update(occurrence.order.to_be_bytes());
        hash_optional_text(
            &mut hash,
            occurrence.left_checkpoint_identity.as_ref().map(ObjectRef::as_str),
        );
        hash_optional_text(
            &mut hash,
            occurrence.right_checkpoint_identity.as_ref().map(ObjectRef::as_str),
        );
        hash_part(&mut hash, occurrence.redo_fold_identity.as_str().as_bytes());
        hash_part(&mut hash, occurrence.left_segment_identity.as_str().as_bytes());
        hash_part(&mut hash, occurrence.right_segment_identity.as_str().as_bytes());
    }
    format!("paired-stream-compaction-chain:{}", hex(&hash.finalize()))
}

fn query_paired_stream_compaction_cost_restoration_fold(
    previous: Option<&QueryPairedStreamCompactionCostRestorationFold>,
    parent_identity: &str,
    parent_cost_identity: &str,
    checkpoint_cost_restoration_identity: Option<&str>,
    rotation_cost_restoration_identity: Option<&str>,
    chain: &QueryPairedStreamCompactionChainDescription,
    chain_identity: &str,
) -> QueryPairedStreamCompactionCostRestorationFold {
    let mut fold = previous.cloned().unwrap_or_else(|| {
        QueryPairedStreamCompactionCostRestorationFold {
            identity: parent_identity.to_owned(),
            chain_count: 0,
            occurrence_count: 0,
            checkpoint_side_count: 0,
            absent_checkpoint_side_count: 0,
            nested_checkpoint_fold_count: 0,
            nested_rotation_fold_count: 0,
            branch_scopes: BTreeSet::new(),
            source_streams: BTreeSet::new(),
            overflowed: false,
        }
    });
    add_checkpoint_fold_count(&mut fold.chain_count, 1, &mut fold.overflowed);
    add_checkpoint_fold_count(
        &mut fold.occurrence_count,
        chain.occurrences.len(),
        &mut fold.overflowed,
    );
    if checkpoint_cost_restoration_identity.is_some() {
        add_checkpoint_fold_count(&mut fold.nested_checkpoint_fold_count, 1, &mut fold.overflowed);
    }
    if rotation_cost_restoration_identity.is_some() {
        add_checkpoint_fold_count(&mut fold.nested_rotation_fold_count, 1, &mut fold.overflowed);
    }
    fold.branch_scopes.insert((chain.branch.name.clone(), chain.branch.generation));
    for occurrence in &chain.occurrences {
        let checkpoint_sides = u64::from(occurrence.left_checkpoint_identity.is_some())
            + u64::from(occurrence.right_checkpoint_identity.is_some());
        add_checkpoint_fold_count(
            &mut fold.checkpoint_side_count,
            checkpoint_sides as usize,
            &mut fold.overflowed,
        );
        add_checkpoint_fold_count(
            &mut fold.absent_checkpoint_side_count,
            (2 - checkpoint_sides) as usize,
            &mut fold.overflowed,
        );
        fold.source_streams.insert((
            chain.join_pair_identity.as_str().to_owned(),
            chain.branch.name.clone(),
            chain.branch.generation,
            occurrence.source_stream_ordinal,
        ));
    }

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-stream-compaction-cost-restoration-fold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_optional_text(&mut hash, checkpoint_cost_restoration_identity);
    hash_optional_text(&mut hash, rotation_cost_restoration_identity);
    hash_part(&mut hash, chain_identity.as_bytes());
    hash.update(fold.chain_count.to_be_bytes());
    hash.update(fold.occurrence_count.to_be_bytes());
    hash.update(fold.checkpoint_side_count.to_be_bytes());
    hash.update(fold.absent_checkpoint_side_count.to_be_bytes());
    hash.update(fold.nested_checkpoint_fold_count.to_be_bytes());
    hash.update(fold.nested_rotation_fold_count.to_be_bytes());
    hash.update((fold.branch_scopes.len() as u64).to_be_bytes());
    hash.update((fold.source_streams.len() as u64).to_be_bytes());
    hash.update([u8::from(fold.overflowed)]);
    fold.identity = format!(
        "paired-stream-compaction-cost-restoration:{}",
        hex(&hash.finalize())
    );
    fold
}

fn add_paired_stream_compaction_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedStreamCompactionCostRestorationFold,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    chain_identity: Option<&str>,
    checkpoint_cost_restoration_identity: Option<&str>,
    rotation_cost_restoration_identity: Option<&str>,
    chain: Option<&QueryPairedStreamCompactionChainDescription>,
) {
    let text_values = [
        ("fold_identity", fold.identity.as_str()),
        ("parent_identity", parent_identity),
        ("parent_cost_identity", parent_cost_identity),
        ("transition", transition),
        (
            "policy",
            "branch_local_checkpoint_rotation_nested_stream_compaction_preserves_full_occurrences",
        ),
    ];
    for (suffix, value) in text_values {
        details.insert(
            format!("paired_stream_compaction_cost_restoration_{suffix}"),
            PlanDetail::Text(value.to_owned()),
        );
    }
    let integer_values = [
        ("chain_count", fold.chain_count),
        ("occurrence_count", fold.occurrence_count),
        ("checkpoint_side_count", fold.checkpoint_side_count),
        ("absent_checkpoint_side_count", fold.absent_checkpoint_side_count),
        ("nested_checkpoint_fold_count", fold.nested_checkpoint_fold_count),
        ("nested_rotation_fold_count", fold.nested_rotation_fold_count),
        ("branch_scope_count", fold.branch_scopes.len() as u64),
        ("source_stream_count", fold.source_streams.len() as u64),
    ];
    for (suffix, value) in integer_values {
        details.insert(
            format!("paired_stream_compaction_cost_restoration_{suffix}"),
            PlanDetail::Integer(value),
        );
    }
    details.insert(
        "paired_stream_compaction_cost_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
    for (key, value) in [
        ("pair_identity", pair_identity),
        ("chain_identity", chain_identity),
        ("checkpoint_fold_identity", checkpoint_cost_restoration_identity),
        ("rotation_fold_identity", rotation_cost_restoration_identity),
    ] {
        let key = format!("paired_stream_compaction_cost_restoration_{key}");
        if let Some(value) = value {
            details.insert(key, PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(&key);
        }
    }
    if let Some(chain) = chain {
        details.insert(
            "paired_stream_compaction_cost_restoration_branch_identity".to_owned(),
            PlanDetail::Text(chain.branch.name.clone()),
        );
        details.insert(
            "paired_stream_compaction_cost_restoration_branch_generation".to_owned(),
            PlanDetail::Integer(chain.branch.generation),
        );
    }
}

fn add_paired_stream_compaction_cost_restoration_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedStreamCompactionCostRestorationFold,
) {
    details.insert(
        "paired_stream_compaction_cost_restoration_output_policy".to_owned(),
        PlanDetail::Text(
            "branch_local_checkpoint_rotation_nested_stream_compaction_preserves_full_occurrences"
                .to_owned(),
        ),
    );
    let integer_values = [
        ("chain_count", fold.chain_count),
        ("occurrence_count", fold.occurrence_count),
        ("checkpoint_side_count", fold.checkpoint_side_count),
        ("absent_checkpoint_side_count", fold.absent_checkpoint_side_count),
        ("nested_checkpoint_fold_count", fold.nested_checkpoint_fold_count),
        ("nested_rotation_fold_count", fold.nested_rotation_fold_count),
        ("branch_scope_count", fold.branch_scopes.len() as u64),
        ("source_stream_count", fold.source_streams.len() as u64),
    ];
    for (suffix, value) in integer_values {
        details.insert(
            format!("paired_stream_compaction_cost_restoration_output_{suffix}"),
            PlanDetail::Integer(value),
        );
    }
    details.insert(
        "paired_stream_compaction_cost_restoration_output_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_stream_compaction_cost_restoration_output_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

fn hash_query_checkpoint_generation(
    hash: &mut Sha256,
    generation: Option<&QueryCheckpointGenerationDescription>,
) {
    if let Some(generation) = generation {
        hash.update([1]);
        hash.update(generation.generation.to_be_bytes());
        if let Some(position) = &generation.position {
            hash.update([1]);
            hash_part(hash, position);
        } else {
            hash.update([0]);
        }
    } else {
        hash.update([0]);
    }
}

fn query_paired_checkpoint_spill_cost_restoration_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-checkpoint-spill-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!("paired-checkpoint-spill-cost-restoration:{}", hex(&hash.finalize()))
}

fn query_paired_checkpoint_spill_restore_chain_identity(
    chain: &QueryPairedCheckpointSpillRestoreChainDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-checkpoint-spill-restore-chain.v1\0");
    hash_part(&mut hash, chain.join_pair_identity.as_str().as_bytes());
    hash_part(&mut hash, chain.branch.name.as_bytes());
    hash.update(chain.branch.generation.to_be_bytes());
    hash.update((chain.occurrences.len() as u64).to_be_bytes());
    for occurrence in &chain.occurrences {
        hash_part(&mut hash, occurrence.checkpoint_identity.as_str().as_bytes());
        hash.update(occurrence.restore_ordinal.to_be_bytes());
        hash.update(occurrence.spill_ordinal.to_be_bytes());
        hash_part(&mut hash, occurrence.spill_identity.as_str().as_bytes());
        hash.update(occurrence.source_stream_ordinal.to_be_bytes());
        hash_part(&mut hash, occurrence.source_stream_identity.as_str().as_bytes());
        hash.update(occurrence.compaction_ordinal.to_be_bytes());
        hash.update(occurrence.handoff_ordinal.to_be_bytes());
        hash.update(occurrence.fold_ordinal.to_be_bytes());
        hash.update(occurrence.order.to_be_bytes());
        hash_query_checkpoint_generation(&mut hash, occurrence.left_checkpoint_generation.as_ref());
        hash_query_checkpoint_generation(&mut hash, occurrence.right_checkpoint_generation.as_ref());
        hash_part(&mut hash, occurrence.redo_fold_identity.as_str().as_bytes());
    }
    format!("paired-checkpoint-spill-restore-chain:{}", hex(&hash.finalize()))
}

fn query_paired_checkpoint_spill_cost_restoration_fold(
    previous: Option<&QueryPairedCheckpointSpillCostRestorationFold>,
    parent_identity: &str,
    parent_cost_identity: &str,
    stream_cost_restoration_identity: Option<&str>,
    chain: &QueryPairedCheckpointSpillRestoreChainDescription,
    chain_identity: &str,
) -> QueryPairedCheckpointSpillCostRestorationFold {
    let mut fold = previous.cloned().unwrap_or_else(|| {
        QueryPairedCheckpointSpillCostRestorationFold {
            identity: parent_identity.to_owned(),
            chain_count: 0,
            occurrence_count: 0,
            restore_count: 0,
            spill_count: 0,
            source_stream_count: 0,
            compaction_count: 0,
            checkpoint_side_count: 0,
            absent_checkpoint_side_count: 0,
            both_sides_absent_occurrence_count: 0,
            nested_stream_fold_count: 0,
            branch_scopes: BTreeSet::new(),
            restores: BTreeSet::new(),
            spills: BTreeSet::new(),
            source_streams: BTreeSet::new(),
            compactions: BTreeSet::new(),
            overflowed: false,
        }
    });
    add_checkpoint_fold_count(&mut fold.chain_count, 1, &mut fold.overflowed);
    add_checkpoint_fold_count(
        &mut fold.occurrence_count,
        chain.occurrences.len(),
        &mut fold.overflowed,
    );
    if stream_cost_restoration_identity.is_some() {
        add_checkpoint_fold_count(&mut fold.nested_stream_fold_count, 1, &mut fold.overflowed);
    }
    fold.branch_scopes.insert((chain.branch.name.clone(), chain.branch.generation));
    for occurrence in &chain.occurrences {
        let pair_identity = chain.join_pair_identity.as_str().to_owned();
        let branch_name = chain.branch.name.clone();
        let branch_generation = chain.branch.generation;
        fold.restores.insert((
            pair_identity.clone(),
            branch_name.clone(),
            branch_generation,
            occurrence.restore_ordinal,
        ));
        fold.spills.insert((
            pair_identity.clone(),
            branch_name.clone(),
            branch_generation,
            occurrence.restore_ordinal,
            occurrence.spill_ordinal,
            occurrence.spill_identity.as_str().to_owned(),
        ));
        fold.source_streams.insert((
            pair_identity.clone(),
            branch_name.clone(),
            branch_generation,
            occurrence.restore_ordinal,
            occurrence.spill_ordinal,
            occurrence.spill_identity.as_str().to_owned(),
            occurrence.source_stream_ordinal,
            occurrence.source_stream_identity.as_str().to_owned(),
        ));
        fold.compactions.insert((
            pair_identity,
            branch_name,
            branch_generation,
            occurrence.restore_ordinal,
            occurrence.spill_ordinal,
            occurrence.spill_identity.as_str().to_owned(),
            occurrence.source_stream_ordinal,
            occurrence.source_stream_identity.as_str().to_owned(),
            occurrence.compaction_ordinal,
        ));
        let sides = u64::from(occurrence.left_checkpoint_generation.is_some())
            + u64::from(occurrence.right_checkpoint_generation.is_some());
        add_checkpoint_fold_count(&mut fold.checkpoint_side_count, sides as usize, &mut fold.overflowed);
        add_checkpoint_fold_count(
            &mut fold.absent_checkpoint_side_count,
            (2 - sides) as usize,
            &mut fold.overflowed,
        );
        if sides == 0 {
            add_checkpoint_fold_count(
                &mut fold.both_sides_absent_occurrence_count,
                1,
                &mut fold.overflowed,
            );
        }
    }
    fold.restore_count = fold.restores.len() as u64;
    fold.spill_count = fold.spills.len() as u64;
    fold.source_stream_count = fold.source_streams.len() as u64;
    fold.compaction_count = fold.compactions.len() as u64;

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-checkpoint-spill-cost-restoration-fold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_optional_text(&mut hash, stream_cost_restoration_identity);
    hash_part(&mut hash, chain_identity.as_bytes());
    hash.update(fold.chain_count.to_be_bytes());
    hash.update(fold.occurrence_count.to_be_bytes());
    hash.update(fold.restore_count.to_be_bytes());
    hash.update(fold.spill_count.to_be_bytes());
    hash.update(fold.source_stream_count.to_be_bytes());
    hash.update(fold.compaction_count.to_be_bytes());
    hash.update(fold.checkpoint_side_count.to_be_bytes());
    hash.update(fold.absent_checkpoint_side_count.to_be_bytes());
    hash.update(fold.both_sides_absent_occurrence_count.to_be_bytes());
    hash.update(fold.nested_stream_fold_count.to_be_bytes());
    hash.update((fold.branch_scopes.len() as u64).to_be_bytes());
    hash.update([u8::from(fold.overflowed)]);
    fold.identity = format!(
        "paired-checkpoint-spill-cost-restoration:{}",
        hex(&hash.finalize())
    );
    fold
}

fn add_paired_checkpoint_spill_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedCheckpointSpillCostRestorationFold,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    chain_identity: Option<&str>,
    stream_cost_restoration_identity: Option<&str>,
    chain: Option<&QueryPairedCheckpointSpillRestoreChainDescription>,
) {
    let prefix = "paired_checkpoint_spill_cost_restoration_";
    for (suffix, value) in [
        ("fold_identity", fold.identity.as_str()),
        ("parent_identity", parent_identity),
        ("parent_cost_identity", parent_cost_identity),
        ("transition", transition),
        (
            "policy",
            "branch_local_nested_spill_restore_preserves_sparse_checkpoint_generations_and_source_order",
        ),
    ] {
        details.insert(format!("{prefix}{suffix}"), PlanDetail::Text(value.to_owned()));
    }
    for (suffix, value) in [
        ("chain_count", fold.chain_count),
        ("occurrence_count", fold.occurrence_count),
        ("restore_count", fold.restore_count),
        ("spill_count", fold.spill_count),
        ("source_stream_count", fold.source_stream_count),
        ("compaction_count", fold.compaction_count),
        ("checkpoint_side_count", fold.checkpoint_side_count),
        ("absent_checkpoint_side_count", fold.absent_checkpoint_side_count),
        (
            "both_sides_absent_occurrence_count",
            fold.both_sides_absent_occurrence_count,
        ),
        ("nested_stream_fold_count", fold.nested_stream_fold_count),
        ("branch_scope_count", fold.branch_scopes.len() as u64),
    ] {
        details.insert(format!("{prefix}{suffix}"), PlanDetail::Integer(value));
    }
    details.insert(
        format!("{prefix}overflowed"),
        PlanDetail::Boolean(fold.overflowed),
    );
    for (suffix, value) in [
        ("pair_identity", pair_identity),
        ("chain_identity", chain_identity),
        ("stream_fold_identity", stream_cost_restoration_identity),
    ] {
        let key = format!("{prefix}{suffix}");
        if let Some(value) = value {
            details.insert(key, PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(&key);
        }
    }
    if let Some(chain) = chain {
        details.insert(
            format!("{prefix}branch_identity"),
            PlanDetail::Text(chain.branch.name.clone()),
        );
        details.insert(
            format!("{prefix}branch_generation"),
            PlanDetail::Integer(chain.branch.generation),
        );
    }
}

fn add_paired_checkpoint_spill_cost_restoration_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedCheckpointSpillCostRestorationFold,
) {
    let prefix = "paired_checkpoint_spill_cost_restoration_output_";
    details.insert(
        format!("{prefix}policy"),
        PlanDetail::Text(
            "branch_local_nested_spill_restore_preserves_sparse_checkpoint_generations_and_source_order"
                .to_owned(),
        ),
    );
    for (suffix, value) in [
        ("fold_identity", PlanDetail::Text(fold.identity.clone())),
        ("chain_count", PlanDetail::Integer(fold.chain_count)),
        ("occurrence_count", PlanDetail::Integer(fold.occurrence_count)),
        ("restore_count", PlanDetail::Integer(fold.restore_count)),
        ("spill_count", PlanDetail::Integer(fold.spill_count)),
        ("source_stream_count", PlanDetail::Integer(fold.source_stream_count)),
        ("compaction_count", PlanDetail::Integer(fold.compaction_count)),
        ("checkpoint_side_count", PlanDetail::Integer(fold.checkpoint_side_count)),
        ("absent_checkpoint_side_count", PlanDetail::Integer(fold.absent_checkpoint_side_count)),
        (
            "both_sides_absent_occurrence_count",
            PlanDetail::Integer(fold.both_sides_absent_occurrence_count),
        ),
        ("nested_stream_fold_count", PlanDetail::Integer(fold.nested_stream_fold_count)),
        ("branch_scope_count", PlanDetail::Integer(fold.branch_scopes.len() as u64)),
        ("overflowed", PlanDetail::Boolean(fold.overflowed)),
    ] {
        details.insert(format!("{prefix}{suffix}"), value);
    }
}

fn query_paired_stream_rotation_cost_restoration_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-stream-rotation-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!("paired-stream-rotation-cost-restoration:{}", hex(&hash.finalize()))
}

fn query_paired_stream_rotation_chain_identity(
    chain: &QueryPairedStreamRotationChainDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-stream-rotation-chain.v1\0");
    hash_part(&mut hash, chain.join_pair_identity.as_str().as_bytes());
    hash_part(&mut hash, chain.branch.name.as_bytes());
    hash.update(chain.branch.generation.to_be_bytes());
    hash.update((chain.occurrences.len() as u64).to_be_bytes());
    for occurrence in &chain.occurrences {
        hash_part(&mut hash, occurrence.checkpoint_identity.as_str().as_bytes());
        hash_part(&mut hash, occurrence.rotation_identity.as_str().as_bytes());
        hash.update(occurrence.source_stream_ordinal.to_be_bytes());
        hash_part(&mut hash, occurrence.source_stream_identity.as_str().as_bytes());
        hash.update(occurrence.rotation_ordinal.to_be_bytes());
        hash.update(occurrence.fold_ordinal.to_be_bytes());
        hash.update(occurrence.order.to_be_bytes());
        hash_optional_text(
            &mut hash,
            occurrence.left_stream_identity.as_ref().map(ObjectRef::as_str),
        );
        hash_optional_text(
            &mut hash,
            occurrence.right_stream_identity.as_ref().map(ObjectRef::as_str),
        );
    }
    format!("paired-stream-rotation-chain:{}", hex(&hash.finalize()))
}

fn query_paired_stream_rotation_cost_restoration_fold(
    previous: Option<&QueryPairedStreamRotationCostRestorationFold>,
    parent_identity: &str,
    parent_cost_identity: &str,
    spill_cost_restoration_identity: Option<&str>,
    chain: &QueryPairedStreamRotationChainDescription,
    chain_identity: &str,
) -> QueryPairedStreamRotationCostRestorationFold {
    let mut fold = previous.cloned().unwrap_or_else(|| {
        QueryPairedStreamRotationCostRestorationFold {
            identity: parent_identity.to_owned(),
            chain_count: 0,
            occurrence_count: 0,
            rotation_count: 0,
            source_stream_count: 0,
            present_stream_side_count: 0,
            absent_stream_side_count: 0,
            both_stream_sides_absent_occurrence_count: 0,
            nested_spill_fold_count: 0,
            branch_scopes: BTreeSet::new(),
            source_streams: BTreeSet::new(),
            rotations: BTreeSet::new(),
            overflowed: false,
        }
    });
    add_checkpoint_fold_count(&mut fold.chain_count, 1, &mut fold.overflowed);
    add_checkpoint_fold_count(
        &mut fold.occurrence_count,
        chain.occurrences.len(),
        &mut fold.overflowed,
    );
    if spill_cost_restoration_identity.is_some() {
        add_checkpoint_fold_count(&mut fold.nested_spill_fold_count, 1, &mut fold.overflowed);
    }
    fold.branch_scopes
        .insert((chain.branch.name.clone(), chain.branch.generation));
    for occurrence in &chain.occurrences {
        let pair = chain.join_pair_identity.as_str().to_owned();
        let branch_name = chain.branch.name.clone();
        let generation = chain.branch.generation;
        fold.source_streams.insert((
            pair.clone(),
            branch_name.clone(),
            generation,
            occurrence.source_stream_ordinal,
            occurrence.source_stream_identity.as_str().to_owned(),
        ));
        fold.rotations.insert((
            pair,
            branch_name,
            generation,
            occurrence.checkpoint_identity.as_str().to_owned(),
            occurrence.source_stream_ordinal,
            occurrence.source_stream_identity.as_str().to_owned(),
            occurrence.rotation_ordinal,
            occurrence.rotation_identity.as_str().to_owned(),
            occurrence.fold_ordinal,
            occurrence.order,
        ));
        let sides = usize::from(occurrence.left_stream_identity.is_some())
            + usize::from(occurrence.right_stream_identity.is_some());
        add_checkpoint_fold_count(
            &mut fold.present_stream_side_count,
            sides,
            &mut fold.overflowed,
        );
        add_checkpoint_fold_count(
            &mut fold.absent_stream_side_count,
            2 - sides,
            &mut fold.overflowed,
        );
        if sides == 0 {
            add_checkpoint_fold_count(
                &mut fold.both_stream_sides_absent_occurrence_count,
                1,
                &mut fold.overflowed,
            );
        }
    }
    fold.rotation_count = fold.rotations.len() as u64;
    fold.source_stream_count = fold.source_streams.len() as u64;

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-stream-rotation-cost-restoration-fold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_optional_text(&mut hash, spill_cost_restoration_identity);
    hash_part(&mut hash, chain_identity.as_bytes());
    hash.update(fold.chain_count.to_be_bytes());
    hash.update(fold.occurrence_count.to_be_bytes());
    hash.update(fold.rotation_count.to_be_bytes());
    hash.update(fold.source_stream_count.to_be_bytes());
    hash.update(fold.present_stream_side_count.to_be_bytes());
    hash.update(fold.absent_stream_side_count.to_be_bytes());
    hash.update(fold.both_stream_sides_absent_occurrence_count.to_be_bytes());
    hash.update(fold.nested_spill_fold_count.to_be_bytes());
    hash.update((fold.branch_scopes.len() as u64).to_be_bytes());
    hash.update([u8::from(fold.overflowed)]);
    fold.identity = format!(
        "paired-stream-rotation-cost-restoration:{}",
        hex(&hash.finalize())
    );
    fold
}

fn add_paired_stream_rotation_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedStreamRotationCostRestorationFold,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    chain_identity: Option<&str>,
    spill_cost_restoration_identity: Option<&str>,
    chain: Option<&QueryPairedStreamRotationChainDescription>,
) {
    let prefix = "paired_stream_rotation_cost_restoration_";
    for (suffix, value) in [
        ("fold_identity", fold.identity.as_str()),
        ("parent_identity", parent_identity),
        ("parent_cost_identity", parent_cost_identity),
        ("transition", transition),
        (
            "policy",
            "branch_local_nested_stream_rotation_preserves_sparse_sides_and_source_coordinates",
        ),
    ] {
        details.insert(format!("{prefix}{suffix}"), PlanDetail::Text(value.to_owned()));
    }
    for (suffix, value) in [
        ("chain_count", fold.chain_count),
        ("occurrence_count", fold.occurrence_count),
        ("rotation_count", fold.rotation_count),
        ("source_stream_count", fold.source_stream_count),
        ("present_stream_side_count", fold.present_stream_side_count),
        ("absent_stream_side_count", fold.absent_stream_side_count),
        (
            "both_stream_sides_absent_occurrence_count",
            fold.both_stream_sides_absent_occurrence_count,
        ),
        ("nested_spill_fold_count", fold.nested_spill_fold_count),
        ("branch_scope_count", fold.branch_scopes.len() as u64),
    ] {
        details.insert(format!("{prefix}{suffix}"), PlanDetail::Integer(value));
    }
    details.insert(
        format!("{prefix}overflowed"),
        PlanDetail::Boolean(fold.overflowed),
    );
    for (suffix, value) in [
        ("pair_identity", pair_identity),
        ("chain_identity", chain_identity),
        ("spill_fold_identity", spill_cost_restoration_identity),
    ] {
        let key = format!("{prefix}{suffix}");
        if let Some(value) = value {
            details.insert(key, PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(&key);
        }
    }
    if let Some(chain) = chain {
        details.insert(
            format!("{prefix}branch_identity"),
            PlanDetail::Text(chain.branch.name.clone()),
        );
        details.insert(
            format!("{prefix}branch_generation"),
            PlanDetail::Integer(chain.branch.generation),
        );
    }
}

fn add_paired_stream_rotation_cost_restoration_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedStreamRotationCostRestorationFold,
) {
    let prefix = "paired_stream_rotation_cost_restoration_output_";
    details.insert(
        format!("{prefix}policy"),
        PlanDetail::Text(
            "branch_local_nested_stream_rotation_preserves_sparse_sides_and_source_coordinates"
                .to_owned(),
        ),
    );
    for (suffix, value) in [
        ("fold_identity", PlanDetail::Text(fold.identity.clone())),
        ("chain_count", PlanDetail::Integer(fold.chain_count)),
        ("occurrence_count", PlanDetail::Integer(fold.occurrence_count)),
        ("rotation_count", PlanDetail::Integer(fold.rotation_count)),
        ("source_stream_count", PlanDetail::Integer(fold.source_stream_count)),
        ("present_stream_side_count", PlanDetail::Integer(fold.present_stream_side_count)),
        ("absent_stream_side_count", PlanDetail::Integer(fold.absent_stream_side_count)),
        (
            "both_stream_sides_absent_occurrence_count",
            PlanDetail::Integer(fold.both_stream_sides_absent_occurrence_count),
        ),
        ("nested_spill_fold_count", PlanDetail::Integer(fold.nested_spill_fold_count)),
        ("branch_scope_count", PlanDetail::Integer(fold.branch_scopes.len() as u64)),
        ("overflowed", PlanDetail::Boolean(fold.overflowed)),
    ] {
        details.insert(format!("{prefix}{suffix}"), value);
    }
}

fn query_paired_wal_rotation_cost_restoration_seed_identity(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-wal-rotation-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!("paired-wal-rotation-cost-restoration:{}", hex(&hash.finalize()))
}

fn query_paired_wal_rotation_chain_identity(
    chain: &QueryPairedWalRotationChainDescription,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-wal-rotation-chain.v1\0");
    hash_part(&mut hash, chain.join_pair_identity.as_str().as_bytes());
    hash_part(&mut hash, chain.branch.name.as_bytes());
    hash.update(chain.branch.generation.to_be_bytes());
    hash.update((chain.occurrences.len() as u64).to_be_bytes());
    for occurrence in &chain.occurrences {
        hash_part(&mut hash, occurrence.checkpoint_identity.as_str().as_bytes());
        hash.update(occurrence.restore_ordinal.to_be_bytes());
        hash.update(occurrence.handoff_ordinal.to_be_bytes());
        hash.update(occurrence.stream_ordinal.to_be_bytes());
        hash.update(occurrence.compaction_ordinal.to_be_bytes());
        hash.update(occurrence.merge_ordinal.to_be_bytes());
        hash.update(occurrence.fold_ordinal.to_be_bytes());
        hash.update(occurrence.order.to_be_bytes());
        hash_query_checkpoint_generation(
            &mut hash,
            occurrence.left_checkpoint_generation.as_ref(),
        );
        hash_query_checkpoint_generation(
            &mut hash,
            occurrence.right_checkpoint_generation.as_ref(),
        );
        hash_part(
            &mut hash,
            occurrence.left_redo_fold_identity.as_str().as_bytes(),
        );
        hash_part(
            &mut hash,
            occurrence.right_redo_fold_identity.as_str().as_bytes(),
        );
        hash_part(
            &mut hash,
            occurrence.left_write_ahead_identity.as_str().as_bytes(),
        );
        hash_part(
            &mut hash,
            occurrence.right_write_ahead_identity.as_str().as_bytes(),
        );
        hash_part(&mut hash, occurrence.left_segment_identity.as_str().as_bytes());
        hash_part(&mut hash, occurrence.right_segment_identity.as_str().as_bytes());
    }
    format!("paired-wal-rotation-chain:{}", hex(&hash.finalize()))
}

fn query_paired_wal_rotation_cost_restoration_fold(
    previous: Option<&QueryPairedWalRotationCostRestorationFold>,
    parent_identity: &str,
    parent_cost_identity: &str,
    stream_rotation_cost_restoration_identity: Option<&str>,
    chain: &QueryPairedWalRotationChainDescription,
    chain_identity: &str,
) -> QueryPairedWalRotationCostRestorationFold {
    let mut fold = previous.cloned().unwrap_or_else(|| {
        QueryPairedWalRotationCostRestorationFold {
            identity: parent_identity.to_owned(),
            chain_count: 0,
            occurrence_count: 0,
            restore_count: 0,
            handoff_count: 0,
            stream_count: 0,
            compaction_count: 0,
            merge_count: 0,
            checkpoint_side_count: 0,
            absent_checkpoint_side_count: 0,
            both_checkpoint_sides_absent_count: 0,
            directional_wal_identity_count: 0,
            segment_identity_count: 0,
            nested_stream_rotation_fold_count: 0,
            branch_scopes: BTreeSet::new(),
            restores: BTreeSet::new(),
            handoffs: BTreeSet::new(),
            streams: BTreeSet::new(),
            compactions: BTreeSet::new(),
            merges: BTreeSet::new(),
            overflowed: false,
        }
    });
    add_checkpoint_fold_count(&mut fold.chain_count, 1, &mut fold.overflowed);
    add_checkpoint_fold_count(
        &mut fold.occurrence_count,
        chain.occurrences.len(),
        &mut fold.overflowed,
    );
    if stream_rotation_cost_restoration_identity.is_some() {
        add_checkpoint_fold_count(
            &mut fold.nested_stream_rotation_fold_count,
            1,
            &mut fold.overflowed,
        );
    }
    fold.branch_scopes
        .insert((chain.branch.name.clone(), chain.branch.generation));
    for occurrence in &chain.occurrences {
        let pair = chain.join_pair_identity.as_str().to_owned();
        let branch_name = chain.branch.name.clone();
        let generation = chain.branch.generation;
        fold.restores.insert((
            pair.clone(),
            branch_name.clone(),
            generation,
            occurrence.restore_ordinal,
        ));
        fold.handoffs.insert((
            pair.clone(),
            branch_name.clone(),
            generation,
            occurrence.restore_ordinal,
            occurrence.handoff_ordinal,
        ));
        fold.streams.insert((
            pair.clone(),
            branch_name.clone(),
            generation,
            occurrence.restore_ordinal,
            occurrence.handoff_ordinal,
            occurrence.stream_ordinal,
        ));
        fold.compactions.insert((
            pair.clone(),
            branch_name.clone(),
            generation,
            occurrence.restore_ordinal,
            occurrence.handoff_ordinal,
            occurrence.stream_ordinal,
            occurrence.compaction_ordinal,
        ));
        fold.merges.insert((
            pair,
            branch_name,
            generation,
            occurrence.restore_ordinal,
            occurrence.handoff_ordinal,
            occurrence.stream_ordinal,
            occurrence.compaction_ordinal,
            occurrence.merge_ordinal,
        ));
        let sides = u64::from(occurrence.left_checkpoint_generation.is_some())
            + u64::from(occurrence.right_checkpoint_generation.is_some());
        add_checkpoint_fold_count(
            &mut fold.checkpoint_side_count,
            sides as usize,
            &mut fold.overflowed,
        );
        add_checkpoint_fold_count(
            &mut fold.absent_checkpoint_side_count,
            (2 - sides) as usize,
            &mut fold.overflowed,
        );
        if sides == 0 {
            add_checkpoint_fold_count(
                &mut fold.both_checkpoint_sides_absent_count,
                1,
                &mut fold.overflowed,
            );
        }
        add_checkpoint_fold_count(
            &mut fold.directional_wal_identity_count,
            2,
            &mut fold.overflowed,
        );
        add_checkpoint_fold_count(
            &mut fold.segment_identity_count,
            2,
            &mut fold.overflowed,
        );
    }
    fold.restore_count = fold.restores.len() as u64;
    fold.handoff_count = fold.handoffs.len() as u64;
    fold.stream_count = fold.streams.len() as u64;
    fold.compaction_count = fold.compactions.len() as u64;
    fold.merge_count = fold.merges.len() as u64;

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-wal-rotation-cost-restoration-fold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_optional_text(&mut hash, stream_rotation_cost_restoration_identity);
    hash_part(&mut hash, chain_identity.as_bytes());
    hash.update(fold.chain_count.to_be_bytes());
    hash.update(fold.occurrence_count.to_be_bytes());
    hash.update(fold.restore_count.to_be_bytes());
    hash.update(fold.handoff_count.to_be_bytes());
    hash.update(fold.stream_count.to_be_bytes());
    hash.update(fold.compaction_count.to_be_bytes());
    hash.update(fold.merge_count.to_be_bytes());
    hash.update(fold.checkpoint_side_count.to_be_bytes());
    hash.update(fold.absent_checkpoint_side_count.to_be_bytes());
    hash.update(fold.both_checkpoint_sides_absent_count.to_be_bytes());
    hash.update(fold.directional_wal_identity_count.to_be_bytes());
    hash.update(fold.segment_identity_count.to_be_bytes());
    hash.update(fold.nested_stream_rotation_fold_count.to_be_bytes());
    hash.update((fold.branch_scopes.len() as u64).to_be_bytes());
    hash.update([u8::from(fold.overflowed)]);
    fold.identity = format!(
        "paired-wal-rotation-cost-restoration:{}",
        hex(&hash.finalize())
    );
    fold
}

fn add_paired_wal_rotation_cost_restoration_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedWalRotationCostRestorationFold,
    parent_identity: &str,
    parent_cost_identity: &str,
    transition: &str,
    pair_identity: Option<&str>,
    chain_identity: Option<&str>,
    stream_rotation_cost_restoration_identity: Option<&str>,
    chain: Option<&QueryPairedWalRotationChainDescription>,
) {
    let prefix = "paired_wal_rotation_cost_restoration_";
    for (suffix, value) in [
        ("fold_identity", fold.identity.as_str()),
        ("parent_identity", parent_identity),
        ("parent_cost_identity", parent_cost_identity),
        ("transition", transition),
        (
            "policy",
            "branch_local_nested_wal_rotation_preserves_restore_merge_coordinates_and_atomic_pairs",
        ),
    ] {
        details.insert(format!("{prefix}{suffix}"), PlanDetail::Text(value.to_owned()));
    }
    for (suffix, value) in [
        ("chain_count", fold.chain_count),
        ("occurrence_count", fold.occurrence_count),
        ("restore_count", fold.restore_count),
        ("handoff_count", fold.handoff_count),
        ("stream_count", fold.stream_count),
        ("compaction_count", fold.compaction_count),
        ("merge_count", fold.merge_count),
        ("checkpoint_side_count", fold.checkpoint_side_count),
        ("absent_checkpoint_side_count", fold.absent_checkpoint_side_count),
        (
            "both_checkpoint_sides_absent_count",
            fold.both_checkpoint_sides_absent_count,
        ),
        ("directional_wal_identity_count", fold.directional_wal_identity_count),
        ("segment_identity_count", fold.segment_identity_count),
        (
            "nested_stream_rotation_fold_count",
            fold.nested_stream_rotation_fold_count,
        ),
        ("branch_scope_count", fold.branch_scopes.len() as u64),
    ] {
        details.insert(format!("{prefix}{suffix}"), PlanDetail::Integer(value));
    }
    details.insert(
        format!("{prefix}overflowed"),
        PlanDetail::Boolean(fold.overflowed),
    );
    for (suffix, value) in [
        ("pair_identity", pair_identity),
        ("chain_identity", chain_identity),
        ("stream_rotation_fold_identity", stream_rotation_cost_restoration_identity),
    ] {
        let key = format!("{prefix}{suffix}");
        if let Some(value) = value {
            details.insert(key, PlanDetail::Text(value.to_owned()));
        } else {
            details.remove(&key);
        }
    }
    if let Some(chain) = chain {
        details.insert(
            format!("{prefix}branch_identity"),
            PlanDetail::Text(chain.branch.name.clone()),
        );
        details.insert(
            format!("{prefix}branch_generation"),
            PlanDetail::Integer(chain.branch.generation),
        );
    }
}

fn add_paired_wal_rotation_cost_restoration_output_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedWalRotationCostRestorationFold,
) {
    let prefix = "paired_wal_rotation_cost_restoration_output_";
    details.insert(
        format!("{prefix}policy"),
        PlanDetail::Text(
            "branch_local_nested_wal_rotation_preserves_restore_merge_coordinates_and_atomic_pairs"
                .to_owned(),
        ),
    );
    for (suffix, value) in [
        ("fold_identity", PlanDetail::Text(fold.identity.clone())),
        ("chain_count", PlanDetail::Integer(fold.chain_count)),
        ("occurrence_count", PlanDetail::Integer(fold.occurrence_count)),
        ("restore_count", PlanDetail::Integer(fold.restore_count)),
        ("handoff_count", PlanDetail::Integer(fold.handoff_count)),
        ("stream_count", PlanDetail::Integer(fold.stream_count)),
        ("compaction_count", PlanDetail::Integer(fold.compaction_count)),
        ("merge_count", PlanDetail::Integer(fold.merge_count)),
        ("checkpoint_side_count", PlanDetail::Integer(fold.checkpoint_side_count)),
        ("absent_checkpoint_side_count", PlanDetail::Integer(fold.absent_checkpoint_side_count)),
        (
            "both_checkpoint_sides_absent_count",
            PlanDetail::Integer(fold.both_checkpoint_sides_absent_count),
        ),
        ("directional_wal_identity_count", PlanDetail::Integer(fold.directional_wal_identity_count)),
        ("segment_identity_count", PlanDetail::Integer(fold.segment_identity_count)),
        (
            "nested_stream_rotation_fold_count",
            PlanDetail::Integer(fold.nested_stream_rotation_fold_count),
        ),
        ("branch_scope_count", PlanDetail::Integer(fold.branch_scopes.len() as u64)),
        ("overflowed", PlanDetail::Boolean(fold.overflowed)),
    ] {
        details.insert(format!("{prefix}{suffix}"), value);
    }
}

fn query_paired_index_cost_compaction_seed_identity(parent_cost_identity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-cost-compaction-seed.v1\0");
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!(
        "paired-index-cost-compaction:{}",
        hex(&hash.finalize())
    )
}

/// Compacts one resolver-paired route into a bounded identity chain. The
/// explain reference does not prescribe compaction metadata, so this adapter
/// commits both indexed and scan outcomes, their logical pair tuple, and the
/// current cost ancestry. Sparse non-paired folds carry the chain unchanged.
fn query_paired_index_cost_compaction_chain_identity(
    parent_chain_identity: &str,
    parent_cost_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    selection_identity: &str,
    refold_identity: &str,
    has_index: bool,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-cost-compaction.v1\0");
    hash_part(&mut hash, parent_chain_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, selection_identity.as_bytes());
    hash_part(&mut hash, refold_identity.as_bytes());
    hash.update([u8::from(has_index)]);
    format!(
        "paired-index-cost-compaction:{}",
        hex(&hash.finalize())
    )
}

fn query_paired_decorrelation_cost_restoration_seed_identity(
    parent_cost_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-decorrelation-cost-restoration-seed.v1\0");
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    format!(
        "paired-decorrelation-cost-restoration:{}",
        hex(&hash.finalize())
    )
}

/// Restores paired route and decorrelation outcomes in one sparse planned-cost
/// sequence. Either event can advance the digest; joins with neither event
/// leave it unchanged. ORNA leaves this explain-only digest unspecified, so
/// include each resolver tuple and its already-computed refold identity under
/// a dedicated domain.
fn query_paired_decorrelation_cost_restoration_chain_identity(
    parent_chain_identity: &str,
    parent_cost_identity: &str,
    pair: Option<&QueryJoinPairIdentityDescription>,
    selection_identity: Option<&str>,
    refold_identity: Option<&str>,
    subquery: Option<&QueryDecorrelatedSubqueryDescription>,
    pin_chain_identity: Option<&str>,
    anchor_fold_identity: Option<&str>,
    omission_refold_identity: Option<&str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-decorrelation-cost-restoration.v1\0");
    hash_part(&mut hash, parent_chain_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    if let Some(pair) = pair {
        hash.update([1]);
        hash_part(&mut hash, pair.identity.as_str().as_bytes());
        hash_part(&mut hash, pair.left_source.as_str().as_bytes());
        hash_part(&mut hash, pair.right_source.as_str().as_bytes());
        hash_optional_text(
            &mut hash,
            pair.predicate.as_ref().map(ExpressionRef::as_str),
        );
        hash_optional_text(&mut hash, selection_identity);
        hash_optional_text(&mut hash, refold_identity);
    } else {
        hash.update([0]);
    }
    if let Some(subquery) = subquery {
        hash.update([1]);
        hash_part(&mut hash, subquery.identity.as_str().as_bytes());
        hash_part(&mut hash, subquery.source.as_str().as_bytes());
        hash_part(
            &mut hash,
            subquery.correlation_predicate.as_str().as_bytes(),
        );
        hash_optional_text(&mut hash, pin_chain_identity);
        hash_optional_text(&mut hash, anchor_fold_identity);
        hash_optional_text(&mut hash, omission_refold_identity);
    } else {
        hash.update([0]);
    }
    format!(
        "paired-decorrelation-cost-restoration:{}",
        hex(&hash.finalize())
    )
}

/// Appends one paired index-or-scan outcome after a planned cost fold. Unlike
/// the exact-index anchor chain, this also appends paired scans: retaining the
/// no-match slot prevents later exact-index identities from shifting across a
/// sparse restoration.
fn query_paired_index_cost_restoration_chain_identity(
    parent_chain_identity: &str,
    parent_cost_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    selection_identity: &str,
    refold_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-cost-restoration.v1\0");
    hash_part(&mut hash, parent_chain_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, selection_identity.as_bytes());
    hash_part(&mut hash, refold_identity.as_bytes());
    format!(
        "paired-index-cost-restoration:{}",
        hex(&hash.finalize())
    )
}

/// Accumulates exact paired-index refolds on the current sparse cost ancestry.
/// The reference leaves this explain-only identity open; keep each selected
/// route attached to its prior paired anchor while allowing scan-only joins to
/// pass the chain through unchanged.
fn query_paired_index_anchor_chain_identity(
    parent_chain_identity: &str,
    parent_cost_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    selection_identity: &str,
    refold_identity: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-anchor-chain.v1\0");
    hash_part(&mut hash, parent_chain_identity.as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, selection_identity.as_bytes());
    hash_part(&mut hash, refold_identity.as_bytes());
    format!("paired-index-anchor-chain:{}", hex(&hash.finalize()))
}

/// Refolds a paired exact-index outcome against the current sparse cost
/// ancestry. The reference does not define this plan-detail digest; carrying
/// both parent and child identities makes later refolds auditable by pair.
fn query_paired_index_refold_identity(
    parent_identity: &str,
    input_identity: &str,
    pair: &QueryJoinPairIdentityDescription,
    selection_identity: &str,
    paired_predicate_identity: Option<&str>,
    cardinality: Cardinality,
    work: Option<u64>,
    work_overflow: bool,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-index-refold.v1\0");
    hash_part(&mut hash, parent_identity.as_bytes());
    hash_part(&mut hash, input_identity.as_bytes());
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, pair.left_source.as_str().as_bytes());
    hash_part(&mut hash, pair.right_source.as_str().as_bytes());
    hash_optional_text(
        &mut hash,
        pair.predicate.as_ref().map(ExpressionRef::as_str),
    );
    hash_part(&mut hash, selection_identity.as_bytes());
    hash_optional_text(&mut hash, paired_predicate_identity);
    hash_optional_u64(&mut hash, cardinality.rows);
    hash_optional_u64(&mut hash, cardinality.bytes);
    hash_optional_u64(&mut hash, work);
    hash.update([u8::from(work_overflow)]);
    format!("paired-index-refold:{}", hex(&hash.finalize()))
}

fn query_paired_window_cost_restoration_seed(
    snapshot: &SnapshotRef,
    parent_cost_identity: &str,
    source: &ObjectRef,
    window_identity: &str,
) -> QueryPairedWindowCostRestorationFold {
    let window_identities = BTreeMap::from([(
        source.as_str().to_owned(),
        window_identity.to_owned(),
    )]);
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-cost-restoration-seed.v1\0");
    hash_part(&mut hash, snapshot.as_str().as_bytes());
    hash_part(&mut hash, parent_cost_identity.as_bytes());
    hash_part(&mut hash, source.as_str().as_bytes());
    hash_part(&mut hash, window_identity.as_bytes());
    QueryPairedWindowCostRestorationFold {
        identity: format!("paired-window-cost-restoration:{}", hex(&hash.finalize())),
        parent_identity: None,
        pair_count: 0,
        window_pair_count: 1,
        cost_restore_fold_count: 0,
        window_identities,
        spill_pair_identities: BTreeMap::new(),
        spill_fold_identity: None,
        cost_restore_fold_identities: BTreeMap::new(),
        overflowed: false,
    }
}

/// The reference leaves planner-local composition open. Bind each window and
/// restore descriptor to its exact join edge, both cost ancestors, and the
/// prior combined fold so nested identities remain scoped after reordering.
fn query_paired_window_cost_restoration_pair_identity(
    previous: Option<&QueryPairedWindowCostRestorationFold>,
    left_cost_identity: &str,
    right_cost_identity: &str,
    pair: Option<&QueryJoinPairIdentityDescription>,
    source: &ObjectRef,
    predicate: Option<&ExpressionRef>,
    window_identity: Option<&str>,
    spill_pair_identity: Option<&str>,
    restore_chain_identities: &[(&str, Option<String>)],
) -> Option<String> {
    if previous.is_none()
        && window_identity.is_none()
        && spill_pair_identity.is_none()
        && restore_chain_identities
            .iter()
            .all(|(_, identity)| identity.is_none())
    {
        return None;
    }

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-cost-restoration-pair.v1\0");
    hash_optional_text(&mut hash, previous.map(|fold| fold.identity.as_str()));
    hash_part(&mut hash, left_cost_identity.as_bytes());
    hash_part(&mut hash, right_cost_identity.as_bytes());
    if let Some(pair) = pair {
        hash.update([1]);
        hash_part(&mut hash, pair.identity.as_str().as_bytes());
        hash_part(&mut hash, pair.left_source.as_str().as_bytes());
        hash_part(&mut hash, pair.right_source.as_str().as_bytes());
        hash_optional_text(&mut hash, pair.predicate.as_ref().map(ExpressionRef::as_str));
    } else {
        hash.update([0]);
        hash_part(&mut hash, source.as_str().as_bytes());
        hash_optional_text(&mut hash, predicate.map(ExpressionRef::as_str));
    }
    hash_optional_text(&mut hash, window_identity);
    hash_optional_text(&mut hash, spill_pair_identity);
    hash.update((restore_chain_identities.len() as u64).to_be_bytes());
    for (kind, identity) in restore_chain_identities {
        hash_part(&mut hash, kind.as_bytes());
        hash_optional_text(&mut hash, identity.as_deref());
    }
    Some(format!(
        "paired-window-cost-restoration-pair:{}",
        hex(&hash.finalize())
    ))
}

fn query_paired_window_cost_restoration_fold(
    previous: Option<&QueryPairedWindowCostRestorationFold>,
    pair_identity: &str,
    pair_key: &str,
    window_identity: Option<&str>,
    spill_pair_identity: Option<&str>,
    restore_fold_identities: &[(&str, Option<&str>)],
) -> QueryPairedWindowCostRestorationFold {
    let mut overflowed = previous.is_some_and(|fold| fold.overflowed);
    let mut window_identities =
        previous.map_or_else(BTreeMap::new, |fold| fold.window_identities.clone());
    let mut spill_pair_identities = previous
        .map_or_else(BTreeMap::new, |fold| fold.spill_pair_identities.clone());
    let mut cost_restore_fold_identities = previous
        .map_or_else(BTreeMap::new, |fold| fold.cost_restore_fold_identities.clone());
    if let Some(window_identity) = window_identity {
        window_identities.insert(pair_key.to_owned(), window_identity.to_owned());
    }
    if let Some(spill_pair_identity) = spill_pair_identity {
        spill_pair_identities.insert(pair_key.to_owned(), spill_pair_identity.to_owned());
    }
    let spill_fold_identity = if spill_pair_identities.is_empty() {
        None
    } else {
        let mut spill_hash = Sha256::new();
        spill_hash.update(b"orna.sys.query-paired-window-cost-restoration-spill-fold.v1\0");
        spill_hash.update((spill_pair_identities.len() as u64).to_be_bytes());
        for (pair, spill_identity) in &spill_pair_identities {
            hash_part(&mut spill_hash, pair.as_bytes());
            hash_part(&mut spill_hash, spill_identity.as_bytes());
        }
        Some(format!(
            "paired-window-cost-restoration-spill-fold:{}",
            hex(&spill_hash.finalize())
        ))
    };
    for (kind, identity) in restore_fold_identities {
        if let Some(identity) = identity {
            cost_restore_fold_identities.insert((*kind).to_owned(), (*identity).to_owned());
        }
    }
    let pair_count = previous.map_or(1, |fold| {
        fold.pair_count.checked_add(1).unwrap_or_else(|| {
            overflowed = true;
            u64::MAX
        })
    });
    let window_pair_count = previous.map_or(u64::from(window_identity.is_some()), |fold| {
        fold.window_pair_count
            .checked_add(u64::from(window_identity.is_some()))
            .unwrap_or_else(|| {
                overflowed = true;
                u64::MAX
            })
    });
    let parent_identity = previous.map(|fold| fold.identity.clone());

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-paired-window-cost-restoration-fold.v1\0");
    hash_optional_text(&mut hash, parent_identity.as_deref());
    hash_part(&mut hash, pair_identity.as_bytes());
    hash.update(pair_count.to_be_bytes());
    hash.update(window_pair_count.to_be_bytes());
    hash.update((window_identities.len() as u64).to_be_bytes());
    for (pair, window) in &window_identities {
        hash_part(&mut hash, pair.as_bytes());
        hash_part(&mut hash, window.as_bytes());
    }
    hash.update((spill_pair_identities.len() as u64).to_be_bytes());
    for (pair, spill_identity) in &spill_pair_identities {
        hash_part(&mut hash, pair.as_bytes());
        hash_part(&mut hash, spill_identity.as_bytes());
    }
    hash_optional_text(&mut hash, spill_fold_identity.as_deref());
    hash.update((cost_restore_fold_identities.len() as u64).to_be_bytes());
    for (kind, identity) in &cost_restore_fold_identities {
        hash_part(&mut hash, kind.as_bytes());
        hash_part(&mut hash, identity.as_bytes());
    }
    hash.update([u8::from(overflowed)]);

    QueryPairedWindowCostRestorationFold {
        identity: format!("paired-window-cost-restoration:{}", hex(&hash.finalize())),
        parent_identity,
        pair_count,
        window_pair_count,
        cost_restore_fold_count: cost_restore_fold_identities.len() as u64,
        window_identities,
        spill_pair_identities,
        spill_fold_identity,
        cost_restore_fold_identities,
        overflowed,
    }
}

fn add_paired_window_cost_restoration_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    fold: &QueryPairedWindowCostRestorationFold,
    pair_identity: &str,
    spill_pair_identity: Option<&str>,
    transition: &str,
) {
    details.insert(
        "paired_window_cost_restoration_fold_identity".to_owned(),
        PlanDetail::Text(fold.identity.clone()),
    );
    details.insert(
        "paired_window_cost_restoration_pair_identity".to_owned(),
        PlanDetail::Text(pair_identity.to_owned()),
    );
    if let Some(parent_identity) = fold.parent_identity.as_ref() {
        details.insert(
            "paired_window_cost_restoration_parent_identity".to_owned(),
            PlanDetail::Text(parent_identity.clone()),
        );
    }
    details.insert(
        "paired_window_cost_restoration_transition".to_owned(),
        PlanDetail::Text(transition.to_owned()),
    );
    details.insert(
        "paired_window_cost_restoration_pairing".to_owned(),
        PlanDetail::Text("exact_window_chains_across_paired_cost_restore_folds".to_owned()),
    );
    details.insert(
        "paired_window_cost_restoration_pair_count".to_owned(),
        PlanDetail::Integer(fold.pair_count),
    );
    details.insert(
        "paired_window_cost_restoration_window_pair_count".to_owned(),
        PlanDetail::Integer(fold.window_pair_count),
    );
    details.insert(
        "paired_window_cost_restoration_window_count".to_owned(),
        PlanDetail::Integer(fold.window_identities.len() as u64),
    );
    details.insert(
        "paired_window_cost_restoration_spill_pair_count".to_owned(),
        PlanDetail::Integer(fold.spill_pair_identities.len() as u64),
    );
    if let Some(spill_fold_identity) = fold.spill_fold_identity.as_ref() {
        details.insert(
            "paired_window_cost_restoration_spill_fold_identity".to_owned(),
            PlanDetail::Text(spill_fold_identity.clone()),
        );
    } else {
        details.remove("paired_window_cost_restoration_spill_fold_identity");
    }
    details.insert(
        "paired_window_cost_restoration_spill_pairing".to_owned(),
        PlanDetail::Text(
            "exact_spill_pairs_bound_to_nested_window_cost_restore_folds".to_owned(),
        ),
    );
    if let Some(spill_pair_identity) = spill_pair_identity {
        details.insert(
            "paired_window_cost_restoration_spill_pair_identity".to_owned(),
            PlanDetail::Text(spill_pair_identity.to_owned()),
        );
    }
    details.insert(
        "paired_window_cost_restoration_restore_fold_count".to_owned(),
        PlanDetail::Integer(fold.cost_restore_fold_count),
    );
    details.insert(
        "paired_window_cost_restoration_overflowed".to_owned(),
        PlanDetail::Boolean(fold.overflowed),
    );
}

fn query_join_cost_fold(
    left_identity: &str,
    right_identity: &str,
    pair: Option<&QueryJoinPairIdentityDescription>,
    paired_predicate_pushdown_identity: Option<&str>,
    paired_index_refold_identity: Option<&str>,
    decorrelated_predicate_pushdown_identity: Option<&str>,
    decorrelated_anchor_fold_identity: Option<&str>,
    decorrelated_omission_refold_identity: Option<&str>,
    window_anchor_fold_identity: Option<&str>,
    join_pair_anchor_fold_identity: Option<&str>,
    paired_limit_pushdown_anchor_fold_identity: Option<&str>,
    paired_aggregate_pushdown_anchor_fold_identity: Option<&str>,
    paired_window_spill_anchor_fold_identity: Option<&str>,
    paired_aggregate_spill_anchor_fold_identity: Option<&str>,
    paired_aggregate_anchor_cascade_fold_identity: Option<&str>,
    paired_scoped_window_compaction_fold_identity: Option<&str>,
    paired_window_spill_cascade_fold_identity: Option<&str>,
    paired_window_spill_rule_cost_fold_identity: Option<&str>,
    paired_window_spill_rule_cost_pair_identity: Option<&str>,
    paired_window_cost_restoration_pair_identity: Option<&str>,
    paired_aggregate_spill_restoration_fold_identity: Option<&str>,
    paired_limit_aggregate_spill_restoration_fold_identity: Option<&str>,
    paired_window_compaction_spill_fold_identity: Option<&str>,
    paired_limit_aggregate_restoration_fold_identity: Option<&str>,
    paired_scoped_window_limit_restoration_fold_identity: Option<&str>,
    paired_scoped_limit_window_restoration_fold_identity: Option<&str>,
    paired_bounded_limit_window_spill_fold_identity: Option<&str>,
    paired_limit_window_compaction_fold_identity: Option<&str>,
    paired_limit_window_cascade_fold_identity: Option<&str>,
    paired_join_limit_anchor_cascade_fold_identity: Option<&str>,
    cardinality: Cardinality,
    work: Option<u64>,
    work_overflow: bool,
    paired_checkpoint_cost_restoration_fold_identity: Option<&str>,
    paired_segment_rotation_cost_restoration_fold_identity: Option<&str>,
    paired_stream_compaction_cost_restoration_fold_identity: Option<&str>,
    paired_checkpoint_spill_cost_restoration_fold_identity: Option<&str>,
    paired_stream_rotation_cost_restoration_fold_identity: Option<&str>,
    paired_wal_rotation_cost_restoration_fold_identity: Option<&str>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-join-cost-fold.v1\0");
    hash_part(&mut hash, left_identity.as_bytes());
    hash_part(&mut hash, right_identity.as_bytes());
    if let Some(pair) = pair {
        hash.update([1]);
        hash_part(&mut hash, pair.identity.as_str().as_bytes());
        hash_part(&mut hash, pair.left_source.as_str().as_bytes());
        hash_part(&mut hash, pair.right_source.as_str().as_bytes());
        hash_optional_text(
            &mut hash,
            pair.predicate.as_ref().map(ExpressionRef::as_str),
        );
    } else {
        hash.update([0]);
    }
    hash_optional_text(&mut hash, paired_predicate_pushdown_identity);
    hash_optional_text(&mut hash, paired_index_refold_identity);
    hash_optional_text(&mut hash, decorrelated_predicate_pushdown_identity);
    hash_optional_text(&mut hash, decorrelated_anchor_fold_identity);
    hash_optional_text(&mut hash, decorrelated_omission_refold_identity);
    hash_optional_text(&mut hash, window_anchor_fold_identity);
    hash_optional_text(&mut hash, join_pair_anchor_fold_identity);
    if let Some(identity) = paired_limit_pushdown_anchor_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    hash_optional_text(&mut hash, paired_aggregate_pushdown_anchor_fold_identity);
    if let Some(identity) = paired_window_spill_anchor_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    hash_optional_text(&mut hash, paired_aggregate_spill_anchor_fold_identity);
    hash_optional_text(&mut hash, paired_aggregate_anchor_cascade_fold_identity);
    hash_optional_text(&mut hash, paired_scoped_window_compaction_fold_identity);
    hash_optional_text(&mut hash, paired_window_spill_cascade_fold_identity);
    hash_optional_text(&mut hash, paired_window_spill_rule_cost_fold_identity);
    hash_optional_text(&mut hash, paired_window_spill_rule_cost_pair_identity);
    hash_optional_text(&mut hash, paired_window_cost_restoration_pair_identity);
    hash_optional_text(
        &mut hash,
        paired_aggregate_spill_restoration_fold_identity,
    );
    hash_optional_text(
        &mut hash,
        paired_limit_aggregate_spill_restoration_fold_identity,
    );
    hash_optional_text(&mut hash, paired_window_compaction_spill_fold_identity);
    hash_optional_text(
        &mut hash,
        paired_limit_aggregate_restoration_fold_identity,
    );
    hash_optional_text(
        &mut hash,
        paired_scoped_window_limit_restoration_fold_identity,
    );
    hash_optional_text(
        &mut hash,
        paired_scoped_limit_window_restoration_fold_identity,
    );
    hash_optional_text(
        &mut hash,
        paired_bounded_limit_window_spill_fold_identity,
    );
    hash_optional_text(&mut hash, paired_limit_window_compaction_fold_identity);
    hash_optional_text(&mut hash, paired_limit_window_cascade_fold_identity);
    hash_optional_text(&mut hash, paired_join_limit_anchor_cascade_fold_identity);
    hash_optional_u64(&mut hash, cardinality.rows);
    hash_optional_u64(&mut hash, cardinality.bytes);
    hash_optional_u64(&mut hash, work);
    hash.update([u8::from(work_overflow)]);
    if let Some(identity) = paired_checkpoint_cost_restoration_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    if let Some(identity) = paired_segment_rotation_cost_restoration_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    if let Some(identity) = paired_stream_compaction_cost_restoration_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    if let Some(identity) = paired_checkpoint_spill_cost_restoration_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    if let Some(identity) = paired_stream_rotation_cost_restoration_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    if let Some(identity) = paired_wal_rotation_cost_restoration_fold_identity {
        hash.update([1]);
        hash_part(&mut hash, identity.as_bytes());
    }
    format!("join-fold:{}", hex(&hash.finalize()))
}

fn hash_query_statistics(hash: &mut Sha256, statistics: Option<&QuerySourceStatistics>) {
    if let Some(statistics) = statistics {
        hash.update([1]);
        hash_optional_u64(hash, statistics.estimated_rows);
        hash_optional_u64(hash, statistics.estimated_bytes);
        if let Some(branch) = &statistics.mutable_branch {
            hash.update([1]);
            hash_part(hash, branch.name.as_bytes());
            hash.update(branch.generation.to_be_bytes());
        } else {
            hash.update([0]);
        }
    } else {
        hash.update([0]);
    }
}

fn hash_query_window_inputs(
    hash: &mut Sha256,
    source: &ObjectRef,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
) {
    if let Some(identity) = query_window_pushdown_chain_identity(source, window_aggregates) {
        hash.update([1]);
        hash_part(hash, identity.as_bytes());
    } else {
        hash.update([0]);
    }
}

/// ORNA leaves this planner-local chain identity encoding unspecified. Keep
/// the source and resolver-ordered members in the digest so sparse cost
/// reordering cannot transfer an aggregate chain to a neighboring input.
fn query_window_pushdown_chain_identity(
    source: &ObjectRef,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
) -> Option<String> {
    let matching = window_aggregates
        .iter()
        .filter(|aggregate| aggregate.source == *source)
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-window-pushdown-chain.v1\0");
    hash_part(&mut hash, source.as_str().as_bytes());
    hash.update((matching.len() as u64).to_be_bytes());
    for aggregate in matching {
        hash_part(&mut hash, aggregate.identity.as_str().as_bytes());
        hash_part(&mut hash, aggregate.aggregate.as_str().as_bytes());
        hash_part(&mut hash, aggregate.frame_identity.as_str().as_bytes());
        hash_part(&mut hash, aggregate.frame_start.as_detail().as_bytes());
        hash_part(&mut hash, aggregate.frame_end.as_detail().as_bytes());
    }
    Some(format!("window-chain:{}", hex(&hash.finalize())))
}

fn add_window_pushdown_chain_details(
    details: &mut BTreeMap<String, PlanDetail>,
    identity: &str,
) {
    details.insert(
        "window_pushdown_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "window_pushdown_identity_policy".to_owned(),
        PlanDetail::Text("source_ordered_sha256_v1".to_owned()),
    );
}

fn query_window_spill_estimate(spill: &QueryWindowSpillDescription) -> (Option<u64>, Option<u64>, Option<u64>) {
    let spilled_bytes = spill
        .estimated_working_set_bytes
        .map(|working_set| working_set.saturating_sub(spill.memory_budget_bytes));
    let io_blocks = spilled_bytes.map(|bytes| ceil_div(bytes, 4096));
    let io_work = io_blocks.and_then(|blocks| blocks.checked_mul(2));
    (spilled_bytes, io_blocks, io_work)
}

/// Describes spill stages in resolver order of their window aggregate chain,
/// independent of the order in which the spill descriptors were supplied.
fn query_window_spill_chain_identity(
    pair: &QueryJoinPairIdentityDescription,
    source: &ObjectRef,
    window_aggregates: &[QueryWindowAggregatePushdownDescription],
    window_spills: &[QueryWindowSpillDescription],
) -> Option<String> {
    let matching = window_aggregates
        .iter()
        .filter(|aggregate| aggregate.source == *source)
        .filter_map(|aggregate| {
            window_spills.iter().find(|spill| {
                spill.join_pair_identity == pair.identity
                    && spill.source == *source
                    && spill.window_aggregate_identity == aggregate.identity
            })
            .map(|spill| (aggregate, spill))
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }

    let mut hash = Sha256::new();
    hash.update(b"orna.sys.query-window-spill-chain.v1\0");
    hash_part(&mut hash, pair.identity.as_str().as_bytes());
    hash_part(&mut hash, source.as_str().as_bytes());
    hash.update((matching.len() as u64).to_be_bytes());
    for (aggregate, spill) in matching {
        let (spilled_bytes, io_blocks, io_work) = query_window_spill_estimate(spill);
        hash_part(&mut hash, aggregate.identity.as_str().as_bytes());
        hash_part(&mut hash, spill.identity.as_str().as_bytes());
        hash_optional_u64(&mut hash, spill.estimated_working_set_bytes);
        hash.update(spill.memory_budget_bytes.to_be_bytes());
        hash_optional_u64(&mut hash, spilled_bytes);
        hash_optional_u64(&mut hash, io_blocks);
        hash_optional_u64(&mut hash, io_work);
    }
    Some(format!("window-spill-chain:{}", hex(&hash.finalize())))
}

fn apply_query_window_spills(
    operators: &mut [Operator],
    aggregate_range: std::ops::Range<usize>,
    pair: &QueryJoinPairIdentityDescription,
    chain_identity: &str,
    source: &ObjectRef,
    window_spills: &[QueryWindowSpillDescription],
) {
    for index in aggregate_range {
        let aggregate_identity = match operators[index].details.get("window_aggregate_identity") {
            Some(PlanDetail::Text(identity)) => identity,
            _ => continue,
        };
        let Some(spill) = window_spills.iter().find(|spill| {
            spill.join_pair_identity == pair.identity
                && spill.source == *source
                && spill.window_aggregate_identity.as_str() == aggregate_identity
        }) else {
            continue;
        };
        let (spilled_bytes, io_blocks, io_work) = query_window_spill_estimate(spill);
        let prior_work = operators[index].work;
        let new_work = prior_work.zip(io_work).and_then(|(work, io)| work.checked_add(io));
        let overflowed = prior_work.zip(io_work).is_some_and(|(work, io)| work.checked_add(io).is_none());
        operators[index].work = new_work;
        let details = &mut operators[index].details;
        details.insert(
            "window_spill_identity".to_owned(),
            PlanDetail::Text(spill.identity.as_str().to_owned()),
        );
        details.insert(
            "window_spill_pair_identity".to_owned(),
            PlanDetail::Text(pair.identity.as_str().to_owned()),
        );
        details.insert(
            "window_spill_estimate_policy".to_owned(),
            PlanDetail::Text("working_set_bytes_above_memory_budget".to_owned()),
        );
        details.insert(
            "window_spill_memory_budget_bytes".to_owned(),
            PlanDetail::Integer(spill.memory_budget_bytes),
        );
        details.insert(
            "window_spill_io_policy".to_owned(),
            PlanDetail::Text("one_write_and_one_read_per_4k_block".to_owned()),
        );
        details.insert(
            "window_spill_chain_identity".to_owned(),
            PlanDetail::Text(chain_identity.to_owned()),
        );
        if let Some(working_set_bytes) = spill.estimated_working_set_bytes {
            details.insert(
                "window_spill_working_set_bytes".to_owned(),
                PlanDetail::Integer(working_set_bytes),
            );
        } else {
            details.insert(
                "window_spill_estimate_status".to_owned(),
                PlanDetail::Text("unknown_working_set".to_owned()),
            );
        }
        if let Some(spilled_bytes) = spilled_bytes {
            details.insert(
                "window_spill_estimated_bytes".to_owned(),
                PlanDetail::Integer(spilled_bytes),
            );
            details.insert(
                "window_spill_required".to_owned(),
                PlanDetail::Boolean(spilled_bytes > 0),
            );
        }
        if let Some(io_blocks) = io_blocks {
            details.insert(
                "window_spill_estimated_io_blocks".to_owned(),
                PlanDetail::Integer(io_blocks),
            );
        }
        if let Some(io_work) = io_work {
            details.insert(
                "window_spill_estimated_io_work".to_owned(),
                PlanDetail::Integer(io_work),
            );
        }
        details.insert(
            "estimated_work_scope".to_owned(),
            PlanDetail::Text("one_unit_per_input_row_plus_spill_read_write_blocks".to_owned()),
        );
        if overflowed {
            record_work_overflow(details);
        }
    }
}

fn add_window_spill_chain_details(details: &mut BTreeMap<String, PlanDetail>, identity: &str) {
    details.insert(
        "window_spill_chain_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "window_spill_chain_policy".to_owned(),
        PlanDetail::Text("exact_pair_and_resolved_window_order".to_owned()),
    );
}

fn hash_optional_text(hash: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash.update([1]);
            hash_part(hash, value.as_bytes());
        }
        None => hash.update([0]),
    }
}

fn add_join_cost_fold_seed_details(details: &mut BTreeMap<String, PlanDetail>, identity: &str) {
    details.insert(
        "join_cost_fold_identity".to_owned(),
        PlanDetail::Text(identity.to_owned()),
    );
    details.insert(
        "join_cost_fold_identity_policy".to_owned(),
        PlanDetail::Text("snapshot_seed_then_planned_left_fold".to_owned()),
    );
}

fn add_join_cost_fold_details(
    details: &mut BTreeMap<String, PlanDetail>,
    left_identity: &str,
    right_identity: &str,
    fold_identity: &str,
) {
    details.insert(
        "join_cost_fold_identity".to_owned(),
        PlanDetail::Text(fold_identity.to_owned()),
    );
    details.insert(
        "join_cost_fold_left_identity".to_owned(),
        PlanDetail::Text(left_identity.to_owned()),
    );
    details.insert(
        "join_cost_fold_right_identity".to_owned(),
        PlanDetail::Text(right_identity.to_owned()),
    );
    details.insert(
        "join_cost_fold_identity_policy".to_owned(),
        PlanDetail::Text("snapshot_seed_then_planned_left_fold".to_owned()),
    );
}

fn push_scan(
    operators: &mut Vec<Operator>,
    object: ObjectRef,
    statistics: Option<&QuerySourceStatistics>,
) -> usize {
    let cardinality = source_cardinality(statistics);
    let mut details = BTreeMap::new();
    if let Some(branch) = statistics.and_then(|stats| stats.mutable_branch.as_ref()) {
        details.insert(
            "mutable_branch".to_owned(),
            PlanDetail::Text(branch.name.clone()),
        );
        details.insert(
            "branch_generation".to_owned(),
            PlanDetail::Integer(branch.generation),
        );
        details.insert(
            "statistics_scope".to_owned(),
            PlanDetail::Text("overlay_inclusive".to_owned()),
        );
    }
    let work = cardinality
        .rows
        .zip(cardinality.bytes)
        .and_then(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)));
    if cardinality
        .rows
        .zip(cardinality.bytes)
        .is_some_and(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)).is_none())
    {
        record_work_overflow(&mut details);
    }
    let index = operators.len();
    operators.push(Operator::new(
        PlanNodeKind::Scan,
        Some(object),
        None,
        details,
        Vec::new(),
        cardinality,
        work,
    ));
    index
}

// The portable plan has no cost-status field. Keep exact arithmetic overflow
// distinct from unavailable inputs in node details instead of saturating work.
fn record_work_overflow(details: &mut BTreeMap<String, PlanDetail>) {
    details.insert(
        "estimated_work_overflow".to_owned(),
        PlanDetail::Boolean(true),
    );
}

fn push_unary(
    operators: &mut Vec<Operator>,
    input: usize,
    kind: PlanNodeKind,
    predicate: Option<ExpressionRef>,
    details: BTreeMap<String, PlanDetail>,
    cardinality: Cardinality,
    work: Option<u64>,
) -> usize {
    let index = operators.len();
    operators.push(Operator::new(
        kind,
        None,
        predicate,
        details,
        vec![input],
        cardinality,
        work,
    ));
    index
}

fn join_cardinality(left: Cardinality, right: Cardinality, has_predicate: bool) -> Cardinality {
    let rows = left.rows.zip(right.rows).and_then(|(left, right)| {
        let divisor = if has_predicate { 10 } else { 1 };
        u64::try_from((u128::from(left) * u128::from(right)).div_ceil(divisor)).ok()
    });
    let bytes = rows.and_then(|rows| {
        let (left_rows, left_bytes) = (left.rows?, left.bytes?);
        let (right_rows, right_bytes) = (right.rows?, right.bytes?);
        let left_width = if left_rows == 0 {
            0
        } else {
            u128::from(left_bytes).div_ceil(u128::from(left_rows))
        };
        let right_width = if right_rows == 0 {
            0
        } else {
            u128::from(right_bytes).div_ceil(u128::from(right_rows))
        };
        u128::from(rows)
            .checked_mul(left_width + right_width)
            .and_then(|bytes| u64::try_from(bytes).ok())
    });
    Cardinality { rows, bytes }
}

fn scale_cardinality(cardinality: Cardinality, numerator: u64, denominator: u64) -> Cardinality {
    // No histogram/cardinality model is available at this layer. Predicate
    // and distinct stages use the caller-selected deterministic fallback
    // (currently 50%); estimates remain tagged details on those operators.
    Cardinality {
        rows: cardinality
            .rows
            .and_then(|rows| scale_count(rows, numerator, denominator)),
        bytes: cardinality
            .bytes
            .and_then(|bytes| scale_count(bytes, numerator, denominator)),
    }
}

fn disjunction_cardinality(cardinality: Cardinality, disjunct_count: u64) -> Cardinality {
    // Independent 50% arms leave one half of the input unmatched per branch.
    // At 64 arms, the unmatched integer row count is necessarily zero for a
    // u64 estimate, so both rows and bytes saturate to their input estimates.
    let (matched_numerator, total_denominator) = if disjunct_count >= u64::BITS.into() {
        (1, 1)
    } else {
        let denominator = 1u64 << disjunct_count;
        (denominator - 1, denominator)
    };
    Cardinality {
        rows: cardinality.rows.and_then(|rows| {
            scale_count(rows, matched_numerator, total_denominator).map(|matched| matched.min(rows))
        }),
        bytes: cardinality.bytes.and_then(|bytes| {
            scale_count(bytes, matched_numerator, total_denominator)
                .map(|matched| matched.min(bytes))
        }),
    }
}

fn disjunction_conjunct_cardinality_and_work(
    input: Cardinality,
    disjunct_count: u64,
    conjunct_count: u64,
) -> (Cardinality, Option<u64>) {
    let mut cardinality = disjunction_cardinality(input, disjunct_count);
    let mut work = input.rows.and_then(|rows| rows.checked_mul(disjunct_count));
    let mut overflowed = input.rows.is_some() && work.is_none();

    // A u64 row estimate reaches its rounded-up fixed point (one row) within
    // 64 halvings. Bound the loop even if a caller supplies a much larger
    // logical conjunct count; any remaining one-row stages are accumulated
    // with checked arithmetic below.
    let explicit_stages = conjunct_count.min(u64::BITS.into());
    for _ in 0..explicit_stages {
        if let (Some(total), Some(rows)) = (work, cardinality.rows) {
            match total.checked_add(rows) {
                Some(total) => work = Some(total),
                None => {
                    work = None;
                    overflowed = true;
                }
            }
        }
        cardinality = scale_cardinality(cardinality, 1, 2);
    }

    let remaining_stages = conjunct_count - explicit_stages;
    if remaining_stages > 0 {
        if let Some(rows_per_stage) = cardinality.rows {
            if let Some(total) = work {
                match rows_per_stage
                    .checked_mul(remaining_stages)
                    .and_then(|tail_work| total.checked_add(tail_work))
                {
                    Some(total) => work = Some(total),
                    None => {
                        work = None;
                        overflowed = true;
                    }
                }
            }
        }
    }

    if overflowed {
        work = None;
    }
    (cardinality, work)
}

fn conjunctive_disjunction_cardinality_and_work(
    input: Cardinality,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
) -> (Cardinality, Option<u64>) {
    let mut branch_cardinality = input;
    let explicit_stages = conjunct_count_per_disjunct.min(u64::BITS.into());
    let mut branch_work = input.rows.map(|_| 0u64);
    for _ in 0..explicit_stages {
        if let (Some(total), Some(rows)) = (branch_work, branch_cardinality.rows) {
            branch_work = total.checked_add(rows);
        }
        branch_cardinality = scale_cardinality(branch_cardinality, 1, 2);
    }

    // Repeated ceil-halving reaches one row (or zero for an empty input) in
    // at most 64 stages. Remaining one-row conjuncts are added in one checked
    // operation rather than iterating over an untrusted count.
    let remaining_stages = conjunct_count_per_disjunct - explicit_stages;
    if remaining_stages > 0 {
        if let (Some(total), Some(rows)) = (branch_work, branch_cardinality.rows) {
            branch_work = rows
                .checked_mul(remaining_stages)
                .and_then(|tail_work| total.checked_add(tail_work));
        }
    }

    let work = branch_work.and_then(|per_disjunct| per_disjunct.checked_mul(disjunct_count));
    let rows = input.rows.map(|rows| {
        conjunctive_disjunction_rows(rows, disjunct_count, conjunct_count_per_disjunct)
    });
    let bytes = input.bytes.map(|bytes| {
        conjunctive_disjunction_rows(bytes, disjunct_count, conjunct_count_per_disjunct)
    });
    (Cardinality { rows, bytes }, work)
}

fn disjunct_storm_shape_text(storm: &DisjunctStormCascadeDescription) -> String {
    let branches = storm
        .branches
        .iter()
        .enumerate()
        .map(|(index, branch)| {
            let limits = branch
                .nested_limits
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let rebinds = branch
                .limit_rebinds
                .iter()
                .map(|rebind| {
                    format!(
                        "@{}={}",
                        rebind.after_limit,
                        rebind
                            .storms
                            .iter()
                            .map(disjunct_storm_shape_text)
                            .collect::<Vec<_>>()
                            .join(">")
                    )
                })
                .collect::<Vec<_>>()
                .join("");
            let nested = branch
                .nested_storms
                .iter()
                .map(disjunct_storm_shape_text)
                .collect::<Vec<_>>()
                .join(">");
            if nested.is_empty() {
                format!(
                    "{}:[{}]/{}{}",
                    index + 1,
                    limits,
                    branch.conjunct_count,
                    rebinds
                )
            } else {
                format!(
                    "{}:[{}]/{}{}{{{}}}",
                    index + 1,
                    limits,
                    branch.conjunct_count,
                    rebinds,
                    nested
                )
            }
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("[{branches}]")
}

fn disjunct_storm_predicates(storms: &[DisjunctStormCascadeDescription]) -> Vec<ExpressionRef> {
    let mut pending = storms.iter().rev().collect::<Vec<_>>();
    let mut predicates = Vec::new();
    while let Some(storm) = pending.pop() {
        predicates.push(storm.predicate.clone());
        for branch in storm.branches.iter().rev() {
            pending.extend(branch.nested_storms.iter().rev());
            for rebind in branch.limit_rebinds.iter().rev() {
                pending.extend(rebind.storms.iter().rev());
            }
        }
    }
    predicates
}

/// Returns the number of rebound cascades at each one-based storm nesting
/// level. Every counted cascade has a byte-cap handoff from its immediate
/// post-limit branch estimate or the previous rebound output.
fn disjunct_storm_rebind_cascade_counts_by_depth(
    storm: &DisjunctStormCascadeDescription,
) -> BTreeMap<usize, usize> {
    let mut pending = vec![(storm, 1usize)];
    let mut rebind_counts = BTreeMap::new();
    while let Some((storm, depth)) = pending.pop() {
        for branch in &storm.branches {
            for rebind in &branch.limit_rebinds {
                if !rebind.storms.is_empty() {
                    let count = rebind_counts.entry(depth).or_insert(0usize);
                    *count = count.saturating_add(rebind.storms.len());
                }
            }
            pending.extend(
                branch
                    .limit_rebinds
                    .iter()
                    .flat_map(|rebind| rebind.storms.iter())
                    .chain(branch.nested_storms.iter())
                    .map(|nested| (nested, depth.saturating_add(1))),
            );
        }
    }
    rebind_counts
}

fn rebind_byte_cap_handoff_estimates_by_depth_text(
    estimates: &RebindByteCapHandoffEstimatesByDepth,
) -> String {
    estimates
        .iter()
        .map(|(depth, handoffs)| {
            let handoffs = handoffs
                .iter()
                .map(|handoff| {
                    let input_bytes = handoff
                        .input_bytes
                        .map_or_else(|| "?".to_owned(), |bytes| bytes.to_string());
                    let output_bytes = handoff
                        .output_bytes
                        .map_or_else(|| "?".to_owned(), |bytes| bytes.to_string());
                    format!("{input_bytes}>{output_bytes}")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{depth}:{handoffs}")
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn rebind_byte_cap_handoff_scopes_by_depth_text(
    routes: &[PlanByteCapHandoffRoute],
) -> String {
    let mut by_depth = BTreeMap::<usize, Vec<String>>::new();
    for route in routes {
        let input_bytes = route
            .input_bytes
            .map_or_else(|| "?".to_owned(), |bytes| bytes.to_string());
        let output_bytes = route
            .output_bytes
            .map_or_else(|| "?".to_owned(), |bytes| bytes.to_string());
        let (_, output_scope) = route.scope_labels();
        by_depth.entry(route.depth).or_default().push(format!(
            "{}={input_bytes}>{output_bytes}",
            output_scope
        ));
    }
    by_depth
        .into_iter()
        .map(|(depth, handoffs)| format!("{depth}:{}", handoffs.join(",")))
        .collect::<Vec<_>>()
        .join(";")
}

fn rebind_byte_cap_handoff_routes_by_depth_text(
    routes: &[PlanByteCapHandoffRoute],
) -> String {
    let mut by_depth = BTreeMap::<usize, Vec<String>>::new();
    for route in routes {
        let input_bytes = route
            .input_bytes
            .map_or_else(|| "?".to_owned(), |bytes| bytes.to_string());
        let output_bytes = route
            .output_bytes
            .map_or_else(|| "?".to_owned(), |bytes| bytes.to_string());
        by_depth.entry(route.depth).or_default().push(format!(
            "{}={input_bytes}>{output_bytes}",
            route.paired_scope_label()
        ));
    }
    by_depth
        .into_iter()
        .map(|(depth, handoffs)| format!("{depth}:{}", handoffs.join(",")))
        .collect::<Vec<_>>()
        .join(";")
}

fn rebind_byte_cap_handoff_route_records(
    estimates: &RebindByteCapHandoffEstimatesByDepth,
) -> Vec<PlanByteCapHandoffRoute> {
    estimates
        .iter()
        .flat_map(|(depth, handoffs)| {
            handoffs.iter().map(|handoff| {
                PlanByteCapHandoffRoute::from_typed_paths(
                    *depth,
                    handoff.input_path.clone(),
                    handoff.output_path.clone(),
                    handoff.input_bytes,
                    handoff.output_bytes,
                )
            })
        })
        .collect()
}

fn byte_cap_scope_path_label(path: &[PlanByteCapScopeSegment]) -> String {
    let mut scope = "root".to_owned();
    for segment in path {
        scope.push('/');
        scope.push_str(&segment.scope_component_label());
    }
    scope
}

fn stabilize_rebind_byte_cap_handoff_routes(
    estimates: &mut RebindByteCapHandoffEstimatesByDepth,
) {
    for handoffs in estimates.values_mut() {
        handoffs.sort_by(|left, right| {
            left.input_path
                .cmp(&right.input_path)
                .then_with(|| left.output_path.cmp(&right.output_path))
        });
    }
}

fn disjunct_storm_cascade_cardinality_and_work(
    input: Cardinality,
    storm: &DisjunctStormCascadeDescription,
    storm_depth: usize,
    storm_scope_path: &[PlanByteCapScopeSegment],
    byte_cap_handoff_estimates_by_depth: &mut RebindByteCapHandoffEstimatesByDepth,
) -> (Cardinality, Option<u64>, bool) {
    let (estimated, work, overflowed) = disjunct_storm_branch_cascade_cardinality_and_work(
        input,
        &storm.branches,
        storm_depth,
        storm_scope_path,
        byte_cap_handoff_estimates_by_depth,
    );
    (cap_cardinality_to_input(estimated, input), work, overflowed)
}

fn cap_cardinality_to_input(estimated: Cardinality, input: Cardinality) -> Cardinality {
    // Apply the cap independently per dimension, retaining unknown estimates
    // as unknown. Recursive callers pass their already bounded branch output
    // as the next input, so these immediate caps compose through nested rebinds.
    let rows = match (estimated.rows, input.rows) {
        (Some(estimated), Some(cap)) => Some(estimated.min(cap)),
        _ => None,
    };
    let bytes = match (estimated.bytes, input.bytes) {
        (Some(estimated), Some(cap)) => Some(estimated.min(cap)),
        _ => None,
    };
    Cardinality { rows, bytes }
}

fn disjunct_storm_branch_cascade_cardinality_and_work(
    input: Cardinality,
    branches: &[DisjunctStormBranchDescription],
    storm_depth: usize,
    storm_scope_path: &[PlanByteCapScopeSegment],
    byte_cap_handoff_estimates_by_depth: &mut RebindByteCapHandoffEstimatesByDepth,
) -> (Cardinality, Option<u64>, bool) {
    let mut work = Some(0u64);
    let mut overflowed = false;
    let mut remaining_rows = input.rows;
    let mut rows = input.rows.map(|_| 0u64);
    let mut remaining_bytes = input.bytes;
    let mut bytes = input.bytes.map(|_| 0u64);

    for (branch_index, branch) in branches.iter().enumerate() {
        let mut branch_cardinality = input;
        let mut branch_byte_scope_path = storm_scope_path.to_vec();
        branch_byte_scope_path.push(PlanByteCapScopeSegment::Branch {
            index: branch_index + 1,
        });
        for (limit_index, limit) in branch.nested_limits.iter().enumerate() {
            match (work, branch_cardinality.rows) {
                (Some(total), Some(limit_work)) => match total.checked_add(limit_work) {
                    Some(total) => work = Some(total),
                    None => {
                        work = None;
                        overflowed = true;
                    }
                },
                (Some(_), None) => work = None,
                (None, _) => {}
            }
            branch_cardinality = limit_cardinality(branch_cardinality, *limit);
            branch_byte_scope_path.push(PlanByteCapScopeSegment::Limit {
                position: limit_index + 1,
            });
            for (rebind_index, rebind) in branch
                .limit_rebinds
                .iter()
                .enumerate()
                .filter(|(_, rebind)| rebind.after_limit == limit_index + 1)
            {
                for (cascade_index, rebind_storm) in rebind.storms.iter().enumerate() {
                    let handoff_input_bytes = branch_cardinality.bytes;
                    let handoff_input_path = branch_byte_scope_path.clone();
                    // The typed destination path extends the actual bounded
                    // source path. This carries earlier cascade outputs
                    // through later cascades and limit positions.
                    let mut handoff_scope_path = handoff_input_path.clone();
                    handoff_scope_path.push(PlanByteCapScopeSegment::Rebind {
                        position: rebind_index + 1,
                    });
                    handoff_scope_path.push(PlanByteCapScopeSegment::Cascade {
                        index: cascade_index + 1,
                    });
                    let (rebound, rebound_work, rebound_overflowed) =
                        disjunct_storm_cascade_cardinality_and_work(
                            branch_cardinality,
                            rebind_storm,
                            storm_depth.saturating_add(1),
                            &handoff_scope_path,
                            byte_cap_handoff_estimates_by_depth,
                        );
                    byte_cap_handoff_estimates_by_depth
                        .entry(storm_depth)
                        .or_default()
                        .push(RebindByteCapHandoffEstimate {
                            input_path: handoff_input_path,
                            output_path: handoff_scope_path.clone(),
                            input_bytes: handoff_input_bytes,
                            output_bytes: rebound.bytes,
                        });
                    overflowed |= rebound_overflowed;
                    match (work, rebound_work) {
                        (Some(total), Some(rebound_work)) => match total.checked_add(rebound_work) {
                            Some(total) => work = Some(total),
                            None => {
                                work = None;
                                overflowed = true;
                            }
                        },
                        (Some(_), None) => work = None,
                        (None, _) => {}
                    }
                    branch_cardinality = rebound;
                    branch_byte_scope_path = handoff_scope_path;
                    // Keep produced outputs distinct from cascade targets so later handoffs
                    // retain provenance even when their estimates are unknown.
                    branch_byte_scope_path.push(
                        PlanByteCapScopeSegment::RebindCascadeOutput {
                            position: rebind_index + 1,
                            index: cascade_index + 1,
                        },
                    );
                }
            }
        }

        let (mut branch_output, branch_work) =
            conjunctive_disjunction_cardinality_and_work(branch_cardinality, 1, branch.conjunct_count);
        if branch_cardinality.rows.is_some() && branch_work.is_none() {
            overflowed = true;
        }
        match (work, branch_work) {
            (Some(total), Some(branch_work)) => match total.checked_add(branch_work) {
                Some(total) => work = Some(total),
                None => {
                    work = None;
                    overflowed = true;
                }
            },
            (Some(_), None) => work = None,
            (None, _) => {}
        }

        let mut prior_nested_storm_path = branch_byte_scope_path.clone();
        if !branch.nested_storms.is_empty() {
            // ORNA-PLAN does not name this intermediate scope. Keep a typed
            // boundary so unknown estimates still preserve the bounded
            // parent-branch output consumed by nested storms.
            prior_nested_storm_path.push(PlanByteCapScopeSegment::BranchOutput {
                index: branch_index + 1,
            });
        }
        for (nested_index, nested_storm) in branch.nested_storms.iter().enumerate() {
            let mut nested_scope_path = prior_nested_storm_path.clone();
            nested_scope_path.push(PlanByteCapScopeSegment::NestedStorm {
                index: nested_index + 1,
            });
            let (nested_output, nested_work, nested_overflowed) =
                disjunct_storm_cascade_cardinality_and_work(
                    branch_output,
                    nested_storm,
                    storm_depth.saturating_add(1),
                    &nested_scope_path,
                    byte_cap_handoff_estimates_by_depth,
                );
            overflowed |= nested_overflowed;
            match (work, nested_work) {
                (Some(total), Some(nested_work)) => match total.checked_add(nested_work) {
                    Some(total) => work = Some(total),
                    None => {
                        work = None;
                        overflowed = true;
                    }
                },
                (Some(_), None) => work = None,
                (None, _) => {}
            }
            branch_output = nested_output;
            prior_nested_storm_path = nested_scope_path;
            prior_nested_storm_path.push(PlanByteCapScopeSegment::NestedStormOutput {
                index: nested_index + 1,
            });
        }

        add_capped_estimate(&mut rows, &mut remaining_rows, branch_output.rows);
        add_capped_estimate(&mut bytes, &mut remaining_bytes, branch_output.bytes);
    }

    (Cardinality { rows, bytes }, work, overflowed)
}

fn add_capped_estimate(total: &mut Option<u64>, remaining: &mut Option<u64>, branch: Option<u64>) {
    match (*total, *remaining, branch) {
        (Some(total_rows), Some(remaining_rows), Some(branch_rows)) => {
            let matched = branch_rows.min(remaining_rows);
            *total = Some(total_rows + matched);
            *remaining = Some(remaining_rows - matched);
        }
        _ => {
            *total = None;
            *remaining = None;
        }
    }
}

fn conjunctive_disjunction_rows(
    input_rows: u64,
    disjunct_count: u64,
    conjunct_count_per_disjunct: u64,
) -> u64 {
    if input_rows == 0 {
        return 0;
    }
    if conjunct_count_per_disjunct >= u64::BITS.into() {
        // The probability per arm is below one row; each rounded arm admits
        // one row until the source estimate is exhausted.
        return input_rows.min(disjunct_count);
    }

    let denominator = 1u128 << conjunct_count_per_disjunct;
    let mut remaining = input_rows;
    for _ in 0..disjunct_count {
        if remaining == 0 {
            break;
        }
        let matched = u64::try_from(u128::from(remaining).div_ceil(denominator))
            .expect("a rounded branch match cannot exceed its input");
        remaining -= matched;
    }
    input_rows - remaining
}

fn limit_cardinality(cardinality: Cardinality, limit: u64) -> Cardinality {
    let rows = cardinality.rows.map(|rows| rows.min(limit));
    let bytes = match (cardinality.rows, cardinality.bytes, rows) {
        (Some(before), Some(bytes), Some(after)) if before > 0 => {
            scale_count(bytes, after, before)
        }
        (Some(0), Some(_), Some(_)) => Some(0),
        (_, bytes, _) => bytes,
    };
    Cardinality { rows, bytes }
}

fn scale_count(value: u64, numerator: u64, denominator: u64) -> Option<u64> {
    if denominator == 0 {
        return Some(0);
    }
    u64::try_from((u128::from(value) * u128::from(numerator)).div_ceil(u128::from(denominator)))
        .ok()
}

fn ceil_div(value: u64, divisor: u64) -> u64 {
    value / divisor + u64::from(value % divisor != 0)
}

fn sort_work(rows: u64) -> Option<u64> {
    // Comparison work is represented as rows * ceil(log2(rows)) integer units.
    let levels = if rows <= 1 {
        0
    } else {
        u64::from((rows - 1).ilog2() + 1)
    };
    rows.checked_mul(levels)
}

fn materialize_work(cardinality: Cardinality) -> Option<u64> {
    // The portable API has no device-specific write price: one unit per row
    // plus one unit per 4 KiB output block gives a stable relative estimate.
    cardinality
        .rows
        .zip(cardinality.bytes)
        .and_then(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)))
}

fn mutation_work(affected_rows: Option<u64>, write_bytes: Option<u64>) -> Option<u64> {
    affected_rows
        .zip(write_bytes)
        .and_then(|(rows, bytes)| rows.checked_add(ceil_div(bytes, 4096)))
}

fn partial_scan_or_mutation_work_lower_bound(operator: &Operator) -> Option<u64> {
    let has_row_byte_work_model = operator.kind == PlanNodeKind::Scan
        || (operator.kind == PlanNodeKind::Invoke
            && operator.details.contains_key("mutation"));
    if !has_row_byte_work_model
        || operator.details.contains_key("estimated_work_overflow")
    {
        return None;
    }

    // Scan and mutation estimates share a rows-plus-4-KiB-blocks work model.
    // When exactly one input is absent, the known nonnegative component is a
    // lower bound for aggregate overflow checks, but not an exact node cost.
    // Other operators have different work models, so their partial cardinality
    // must not be mistaken for independently priced work.
    match (operator.cardinality.rows, operator.cardinality.bytes) {
        (Some(rows), None) => Some(rows),
        (None, Some(bytes)) => Some(ceil_div(bytes, 4096)),
        _ => None,
    }
}

fn mutated_table_rows(
    kind: QueryMutationKind,
    before: Option<u64>,
    affected_rows: Option<u64>,
) -> Option<u64> {
    match kind {
        QueryMutationKind::Insert => before
            .zip(affected_rows)
            .and_then(|(before, affected)| before.checked_add(affected)),
        QueryMutationKind::Delete => before
            .zip(affected_rows)
            .and_then(|(before, affected)| before.checked_sub(affected)),
        // Orna updates and rekeys do not add or remove table rows. Their
        // returned mutation count is distinct from total table cardinality.
        QueryMutationKind::Update | QueryMutationKind::Rekey => before,
    }
}

/// Builds a function/effect explain plan from catalogue-supplied direct edges.
/// Statically known calls and reads are visible; non-executable type/import
/// edges remain available from the dependency relation instead of being
/// misrepresented as runtime work.
pub fn explain_function(
    function: &FunctionPlanDescription,
) -> Result<ExplainedPlan, ExplainError> {
    if invalid_reference(function.snapshot.as_str()) {
        return Err(ExplainError::InvalidSnapshot);
    }
    if invalid_reference(function.function.as_str()) {
        return Err(ExplainError::InvalidObject);
    }
    let function_object = ObjectRef::descriptive(function.function.as_str());
    let mut children = Vec::<Operator>::new();
    if function
        .dependencies
        .iter()
        .any(|edge| edge.from != function_object)
    {
        return Err(ExplainError::InvalidDependency);
    }
    let objects = function
        .dependencies
        .iter()
        .flat_map(|edge| [edge.from.clone(), edge.to.clone()])
        .chain(std::iter::once(function_object.clone()))
        .collect::<BTreeSet<_>>();
    let graph = DependencyGraph::new(
        function.snapshot.clone(),
        objects,
        function.dependencies.clone(),
    )
    .map_err(|_| ExplainError::InvalidDependency)?;
    let direct = graph
        .dependencies(&function_object, false, None)
        .map_err(|_| ExplainError::InvalidDependency)?;
    for edge in direct {
        if edge.from != function_object {
            return Err(ExplainError::InvalidDependency);
        }
        let kind = match edge.kind {
            DependencyKind::Call => PlanNodeKind::Invoke,
            DependencyKind::TableRead | DependencyKind::StorageProjection => PlanNodeKind::Scan,
            DependencyKind::TableWrite => PlanNodeKind::Invoke,
            DependencyKind::Assertion => PlanNodeKind::AssertionValidate,
            DependencyKind::ExtensionImport => PlanNodeKind::External,
            DependencyKind::RendererRequirement => PlanNodeKind::Project,
            DependencyKind::Import
            | DependencyKind::TypeReference
            | DependencyKind::Implementation
            | DependencyKind::PageEntry => continue,
        };
        children.push(Operator::new(
            kind,
            Some(edge.to),
            None,
            [
                (
                    "dependency_kind".to_owned(),
                    PlanDetail::Text(edge.kind.as_str().to_owned()),
                ),
                (
                    "confidence".to_owned(),
                    PlanDetail::Text(edge.confidence.as_str().to_owned()),
                ),
                (
                    "conditional".to_owned(),
                    PlanDetail::Boolean(edge.conditional),
                ),
            ]
            .into_iter()
            .collect(),
            Vec::new(),
            Cardinality::default(),
            None,
        ));
    }
    // Children are ordered by their typed edge identity, independent of the
    // order in which a catalogue implementation supplied those rows.
    children.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.object.cmp(&right.object))
    });
    let mut operators = Vec::with_capacity(children.len() + 1);
    let root = Operator::new(
        PlanNodeKind::Invoke,
        Some(function_object),
        None,
        BTreeMap::new(),
        (1..=children.len()).collect(),
        Cardinality::default(),
        None,
    );
    operators.push(root);
    operators.extend(children);
    build_plan(function.snapshot.clone(), operators, 0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Operator {
    kind: PlanNodeKind,
    object: Option<ObjectRef>,
    predicate: Option<ExpressionRef>,
    details: BTreeMap<String, PlanDetail>,
    inputs: Vec<usize>,
    cardinality: Cardinality,
    work: Option<u64>,
}

impl Operator {
    fn new(
        kind: PlanNodeKind,
        object: Option<ObjectRef>,
        predicate: Option<ExpressionRef>,
        details: BTreeMap<String, PlanDetail>,
        inputs: Vec<usize>,
        cardinality: Cardinality,
        work: Option<u64>,
    ) -> Self {
        Self {
            kind,
            object,
            predicate,
            details,
            inputs,
            cardinality,
            work,
        }
    }
}

fn build_plan(
    snapshot: SnapshotRef,
    operators: Vec<Operator>,
    root_index: usize,
) -> Result<ExplainedPlan, ExplainError> {
    if operators.is_empty() || operators.len() > MAX_PLAN_NODES {
        return Err(ExplainError::TooManyNodes);
    }
    if root_index >= operators.len() {
        return Err(ExplainError::InvalidPlanTree);
    }
    let mut order = Vec::with_capacity(operators.len());
    let mut stack = vec![root_index];
    let mut visited = BTreeSet::new();
    let mut parents = vec![None; operators.len()];
    while let Some(index) = stack.pop() {
        if index >= operators.len() || !visited.insert(index) {
            return Err(ExplainError::InvalidPlanTree);
        }
        order.push(index);
        for child in operators[index].inputs.iter().rev() {
            if *child >= operators.len() || parents[*child].replace(index).is_some() {
                return Err(ExplainError::InvalidPlanTree);
            }
            stack.push(*child);
        }
    }
    if order.len() != operators.len() || parents[root_index].is_some() {
        return Err(ExplainError::InvalidPlanTree);
    }
    let mut positions = vec![usize::MAX; operators.len()];
    for (position, index) in order.iter().enumerate() {
        positions[*index] = position;
    }
    let mut identity = Sha256::new();
    identity.update(b"orna.sys.plan.v1\0");
    hash_part(&mut identity, snapshot.as_str().as_bytes());
    for index in &order {
        let operator = &operators[*index];
        hash_part(&mut identity, operator.kind.as_ref_str().as_bytes());
        hash_part(
            &mut identity,
            operator
                .object
                .as_ref()
                .map_or("", ObjectRef::as_str)
                .as_bytes(),
        );
        hash_part(
            &mut identity,
            operator
                .predicate
                .as_ref()
                .map_or("", ExpressionRef::as_str)
                .as_bytes(),
        );
        for (name, detail) in &operator.details {
            hash_part(&mut identity, name.as_bytes());
            hash_plan_detail(&mut identity, detail);
        }
        hash_optional_u64(&mut identity, operator.cardinality.rows);
        hash_optional_u64(&mut identity, operator.cardinality.bytes);
        hash_optional_u64(&mut identity, operator.work);
        identity.update((operator.inputs.len() as u64).to_be_bytes());
        for child in &operator.inputs {
            identity.update((positions[*child] as u64).to_be_bytes());
        }
    }
    let digest = hex(&identity.finalize());
    let id = format!("plan:{digest}");
    let plan = PlanRef::descriptive(id.clone());
    let references = (0..operators.len())
        .map(|position| PlanNodeRef::descriptive(format!("{id}:{position}")))
        .collect::<Vec<_>>();

    let root = references[positions[root_index]].clone();
    // Keep the known lower bound independent of the nullable total: an
    // unknown estimate must not discard known cost across later plan tails.
    let mut known_work_overflow = false;
    let mut known_work_total = Some(0u64);
    let mut total_work = Some(0u64);
    let mut nodes = Vec::with_capacity(order.len());
    for (position, index) in order.iter().enumerate() {
        let operator = &operators[*index];
        let position_u32 = u32::try_from(position).map_err(|_| ExplainError::TooManyNodes)?;
        let parent_position = parents[*index].map(|parent| positions[parent]);
        let inputs = operator
            .inputs
            .iter()
            .map(|child| references[positions[*child]].clone())
            .collect();
        // Work is reported as exact integer units. `u64::MAX` is a valid
        // total; if a local estimate is unknown or the exact sum overflows,
        // omit only the plan total and keep known node contributions.
        total_work = match (total_work, operator.work) {
            (Some(total), Some(work)) => total.checked_add(work),
            _ => None,
        };
        if let Some(work) = operator
            .work
            .or_else(|| partial_scan_or_mutation_work_lower_bound(operator))
        {
            known_work_total = known_work_total.and_then(|known| known.checked_add(work));
            known_work_overflow |= known_work_total.is_none();
        }
        // A local marker proves this nonnegative contribution exceeds
        // u64::MAX, so the full plan cost overflows even across unknown tails,
        // downstream conjuncts, or a LIMIT that lowers only output cardinality.
        known_work_overflow |= operator.details.contains_key("estimated_work_overflow");
        let mut details = operator.details.clone();
        if let Some(work) = operator.work {
            // `sys.PlanNode` has no dedicated cost column. Keep each local
            // contribution in details so the explain tail adds to Plan.cost.
            details.insert("estimated_work".to_owned(), PlanDetail::Integer(work));
        }
        nodes.push(PlanNode {
            reference: references[position].clone(),
            plan: plan.clone(),
            parent: parent_position.map(|index| references[index].clone()),
            position: position_u32,
            kind: operator.kind,
            inputs,
            object: operator.object.clone(),
            estimated_rows: operator.cardinality.rows,
            actual_rows: None,
            estimated_bytes: operator.cardinality.bytes,
            actual_bytes: None,
            predicate: operator.predicate.clone(),
            details,
        });
    }
    if known_work_overflow {
        // The portable plan has a nullable total but no cost-status field.
        // An unknown operator does not erase overflow already proven by the
        // nonnegative known-work lower bound; later known tails can prove it
        // too. Preserve the marker even when the displayed total is unknown.
        nodes[positions[root_index]].details.insert(
            "estimated_cost_overflow".to_owned(),
            PlanDetail::Boolean(true),
        );
    }
    Ok(ExplainedPlan {
        plan: Plan {
            reference: plan,
            id,
            snapshot,
            root,
            estimated_cost: total_work.map(|work| work.to_string()),
            actual_available: false,
            warnings: Vec::new(),
        },
        nodes,
    })
}

fn hash_optional_u64(hash: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hash.update([1]);
            hash.update(value.to_be_bytes());
        }
        None => hash.update([0]),
    }
}

fn hash_plan_detail(hash: &mut Sha256, detail: &PlanDetail) {
    match detail {
        PlanDetail::Text(value) => {
            hash.update([0]);
            hash_part(hash, value.as_bytes());
        }
        PlanDetail::Integer(value) => {
            hash.update([1]);
            hash.update(value.to_be_bytes());
        }
        PlanDetail::Boolean(value) => {
            hash.update([2, u8::from(*value)]);
        }
        PlanDetail::Expressions(expressions) => {
            hash.update([3]);
            hash.update((expressions.len() as u64).to_be_bytes());
            for expression in expressions {
                hash_part(hash, expression.as_str().as_bytes());
            }
        }
        PlanDetail::Ordering(ordering) => {
            hash.update([4]);
            hash.update((ordering.len() as u64).to_be_bytes());
            for item in ordering {
                hash_part(hash, item.expression.as_str().as_bytes());
                hash_part(hash, item.direction.as_ref_str().as_bytes());
                hash_part(hash, item.null_order.as_ref_str().as_bytes());
            }
        }
        PlanDetail::ByteCapHandoffRoutes(routes) => {
            hash.update([5]);
            hash.update((routes.len() as u64).to_be_bytes());
            for route in routes {
                hash.update((route.depth as u64).to_be_bytes());
                hash_byte_cap_scope_path(hash, &route.input_path);
                hash_byte_cap_scope_path(hash, &route.output_path);
                let (input_scope, output_scope) = route.scope_labels();
                hash_part(hash, input_scope.as_bytes());
                hash_part(hash, output_scope.as_bytes());
                hash_optional_u64(hash, route.input_bytes);
                hash_optional_u64(hash, route.output_bytes);
            }
        }
    }
}

fn hash_byte_cap_scope_path(hash: &mut Sha256, path: &[PlanByteCapScopeSegment]) {
    hash.update((path.len() as u64).to_be_bytes());
    for segment in path {
        match segment {
            PlanByteCapScopeSegment::StormStage { index } => {
                hash.update([0]);
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::StormStageOutput { index } => {
                hash.update([6]);
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::Branch { index } => {
                hash.update([1]);
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::Limit { position } => {
                hash.update([2]);
                hash.update((*position as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::BranchOutput { index } => {
                hash.update([9]);
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::Rebind { position } => {
                hash.update([3]);
                hash.update((*position as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::Cascade { index } => {
                hash.update([4]);
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::RebindCascadeOutput { position, index } => {
                hash.update([8]);
                hash.update((*position as u64).to_be_bytes());
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::NestedStorm { index } => {
                hash.update([5]);
                hash.update((*index as u64).to_be_bytes());
            }
            PlanByteCapScopeSegment::NestedStormOutput { index } => {
                hash.update([7]);
                hash.update((*index as u64).to_be_bytes());
            }
        }
    }
}

impl PlanNodeKind {
    const fn as_ref_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::IndexLookup => "index_lookup",
            Self::Filter => "filter",
            Self::Project => "project",
            Self::Join => "join",
            Self::Aggregate => "aggregate",
            Self::Sort => "sort",
            Self::Limit => "limit",
            Self::Materialize => "materialize",
            Self::Invoke => "invoke",
            Self::AssertionValidate => "assertion_validate",
            Self::CheckpointUpdate => "checkpoint_update",
            Self::External => "external",
        }
    }
}

#[cfg(test)]
mod byte_cap_handoff_route_scope_tests {
    use super::*;

    #[test]
    fn scope_component_labels_preserve_each_typed_segment() {
        let segments = [
            (PlanByteCapScopeSegment::StormStage { index: 2 }, "storm2"),
            (
                PlanByteCapScopeSegment::StormStageOutput { index: 3 },
                "storm_stage_output3",
            ),
            (PlanByteCapScopeSegment::Branch { index: 4 }, "branch4"),
            (PlanByteCapScopeSegment::Limit { position: 5 }, "limit5"),
            (
                PlanByteCapScopeSegment::BranchOutput { index: 6 },
                "branch_output6",
            ),
            (PlanByteCapScopeSegment::Rebind { position: 7 }, "rebind7"),
            (PlanByteCapScopeSegment::Cascade { index: 8 }, "cascade8"),
            (
                PlanByteCapScopeSegment::RebindCascadeOutput {
                    position: 9,
                    index: 10,
                },
                "rebind_output9_10",
            ),
            (PlanByteCapScopeSegment::NestedStorm { index: 11 }, "nested11"),
            (
                PlanByteCapScopeSegment::NestedStormOutput { index: 12 },
                "nested_output12",
            ),
        ];

        for (segment, expected) in segments {
            assert_eq!(segment.scope_component_label(), expected);
        }
    }

    #[test]
    fn typed_paths_drive_scope_summaries_and_route_fingerprint() {
        let input_path = vec![
            PlanByteCapScopeSegment::StormStage { index: 1 },
            PlanByteCapScopeSegment::StormStageOutput { index: 1 },
            PlanByteCapScopeSegment::StormStage { index: 2 },
            PlanByteCapScopeSegment::Branch { index: 1 },
            PlanByteCapScopeSegment::Limit { position: 1 },
        ];
        let mut output_path = input_path.clone();
        output_path.extend([
            PlanByteCapScopeSegment::Rebind { position: 1 },
            PlanByteCapScopeSegment::Cascade { index: 1 },
        ]);
        let mut stale_route = PlanByteCapHandoffRoute::from_typed_paths(
            2,
            input_path,
            output_path,
            None,
            None,
        );
        stale_route.input_scope = "stale input scope".to_owned();
        stale_route.output_scope = "stale output scope".to_owned();

        let input_scope = "root/storm1/storm_stage_output1/storm2/branch1/limit1";
        let output_scope = format!("{input_scope}/rebind1/cascade1");
        assert_eq!(
            stale_route.paired_scope_label(),
            format!("{input_scope}=>{output_scope}")
        );
        assert_eq!(
            stale_route.named_scope_labels(),
            PlanByteCapScopeLabels {
                input: input_scope.to_owned(),
                output: output_scope.clone(),
            }
        );
        let serialized_route = serde_json::to_value(&stale_route).unwrap();
        assert_eq!(
            serialized_route["scope_labels"],
            serde_json::json!({ "input": input_scope, "output": output_scope })
        );
        assert!(serialized_route["scope_labels"].is_object());
        assert_eq!(
            rebind_byte_cap_handoff_scopes_by_depth_text(&[stale_route.clone()]),
            format!("2:{output_scope}=?>?")
        );
        assert_eq!(
            rebind_byte_cap_handoff_routes_by_depth_text(&[stale_route.clone()]),
            format!("2:{input_scope}=>{output_scope}=?>?")
        );

        let mut canonical_route = stale_route.clone();
        canonical_route.input_scope = input_scope.to_owned();
        canonical_route.output_scope = output_scope;
        let fingerprint = |route| {
            let mut hash = Sha256::new();
            hash_plan_detail(&mut hash, &PlanDetail::ByteCapHandoffRoutes(vec![route]));
            hash.finalize()
        };
        assert_eq!(fingerprint(stale_route), fingerprint(canonical_route));
    }
}
