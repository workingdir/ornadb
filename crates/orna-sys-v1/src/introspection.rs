//! Structured planner and dependency-graph values for the portable `sys` API.
//!
//! The runtime/catalogue adapter owns object identity and query resolution. This
//! module only accepts already typed, snapshot-pinned inputs and turns them
//! into deterministic, bounded `sys.Plan` and `sys.Dependency` projections.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::Serialize;
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

/// A resolved, snapshot-pinned logical query input. Expressions are opaque
/// typed references, so plan details never echo literal query values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlanDescription {
    pub snapshot: SnapshotRef,
    pub source: ObjectRef,
    /// Snapshot statistics for `source`. If `mutable_branch` is set, counts
    /// must include the branch's current uncommitted overlay at `generation`.
    pub source_statistics: Option<QuerySourceStatistics>,
    /// Additional inputs joined in order after `source`.
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
        .chain(post_expansion_conjunct.iter().copied())
        .chain(disjunct_storms.iter().map(|storm| &storm.predicate))
        .chain(storm_cascade_predicates.iter().copied())
        .any(|expression| invalid_reference(expression.as_str()))
    {
        return Err(ExplainError::InvalidExpression);
    }
    if query
        .projections
        .len()
        .saturating_add(query.ordering.len())
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
    for join in &query.joins {
        let right = push_scan(&mut operators, join.source.clone(), join.statistics.as_ref());
        let right_cardinality = source_cardinality(join.statistics.as_ref());
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
        let mut details = BTreeMap::from([(
            "strategy".to_owned(),
            PlanDetail::Text("hash".to_owned()),
        )]);
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
    for (storm_index, storm) in disjunct_storm_cascades.iter().enumerate() {
        let storm_index = u64::try_from(storm_index + 1).map_err(|_| ExplainError::TooManyNodes)?;
        let branch_count =
            u64::try_from(storm.branches.len()).map_err(|_| ExplainError::TooManyNodes)?;
        let (cardinality, work, overflowed) =
            disjunct_storm_cascade_cardinality_and_work(current_cardinality, storm);
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
        let limit_chain_rebind_max_nested_depth = disjunct_storm_rebind_depth(storm);
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
    build_plan(query.snapshot.clone(), operators, current)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Cardinality {
    rows: Option<u64>,
    bytes: Option<u64>,
}

fn source_cardinality(statistics: Option<&QuerySourceStatistics>) -> Cardinality {
    statistics.map_or_else(Cardinality::default, |statistics| Cardinality {
        rows: statistics.estimated_rows,
        bytes: statistics.estimated_bytes,
    })
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

fn disjunct_storm_rebind_depth(storm: &DisjunctStormCascadeDescription) -> usize {
    let mut pending = vec![(storm, 1usize)];
    let mut max_rebind_depth = 0usize;
    while let Some((storm, depth)) = pending.pop() {
        for branch in &storm.branches {
            if !branch.limit_rebinds.is_empty() {
                max_rebind_depth = max_rebind_depth.max(depth);
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
    max_rebind_depth
}

fn disjunct_storm_cascade_cardinality_and_work(
    input: Cardinality,
    storm: &DisjunctStormCascadeDescription,
) -> (Cardinality, Option<u64>, bool) {
    let (estimated, work, overflowed) =
        disjunct_storm_branch_cascade_cardinality_and_work(input, &storm.branches);
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
) -> (Cardinality, Option<u64>, bool) {
    let mut work = Some(0u64);
    let mut overflowed = false;
    let mut remaining_rows = input.rows;
    let mut rows = input.rows.map(|_| 0u64);
    let mut remaining_bytes = input.bytes;
    let mut bytes = input.bytes.map(|_| 0u64);

    for branch in branches {
        let mut branch_cardinality = input;
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
            for rebind in branch
                .limit_rebinds
                .iter()
                .filter(|rebind| rebind.after_limit == limit_index + 1)
            {
                for rebind_storm in &rebind.storms {
                    let (rebound, rebound_work, rebound_overflowed) =
                        disjunct_storm_cascade_cardinality_and_work(
                            branch_cardinality,
                            rebind_storm,
                        );
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

        for nested_storm in &branch.nested_storms {
            let (nested_output, nested_work, nested_overflowed) =
                disjunct_storm_cascade_cardinality_and_work(branch_output, nested_storm);
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
