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
    pub predicate: Option<ExpressionRef>,
    pub projections: Vec<ExpressionRef>,
    pub distinct: bool,
    pub ordering: Vec<PlanOrdering>,
    pub limit: Option<u64>,
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
        }
    }
}

impl fmt::Display for ExplainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for ExplainError {}

/// Plans the currently supported table-query shape without changing how it is
/// evaluated. A scan remains the reference fallback; operators are logical
/// observations only. Cost/cardinality/actual fields remain absent until a
/// backend has measured or estimated them, as required by the 1.0 contract.
pub fn explain_query(query: &QueryPlanDescription) -> Result<ExplainedPlan, ExplainError> {
    if invalid_reference(query.snapshot.as_str()) {
        return Err(ExplainError::InvalidSnapshot);
    }
    if invalid_reference(query.source.as_str()) {
        return Err(ExplainError::InvalidObject);
    }
    if query
        .predicate
        .iter()
        .chain(query.projections.iter())
        .chain(query.ordering.iter().map(|ordering| &ordering.expression))
        .any(|expression| invalid_reference(expression.as_str()))
    {
        return Err(ExplainError::InvalidExpression);
    }
    if query
        .projections
        .len()
        .saturating_add(query.ordering.len())
        .saturating_add(usize::from(query.predicate.is_some()))
        > MAX_PLAN_EXPRESSIONS
    {
        return Err(ExplainError::TooManyExpressions);
    }

    let mut operators = Vec::<Operator>::new();
    operators.push(Operator {
        kind: PlanNodeKind::Scan,
        object: Some(query.source.clone()),
        predicate: None,
        details: BTreeMap::new(),
    });
    if let Some(predicate) = &query.predicate {
        operators.push(Operator {
            kind: PlanNodeKind::Filter,
            object: None,
            predicate: Some(predicate.clone()),
            details: BTreeMap::new(),
        });
    }
    if !query.projections.is_empty() {
        operators.push(Operator {
            kind: PlanNodeKind::Project,
            object: None,
            predicate: None,
            details: BTreeMap::from([(
                "expressions".to_owned(),
                PlanDetail::Expressions(query.projections.clone()),
            )]),
        });
    }
    if query.distinct {
        // The reference names no standalone DISTINCT plan kind. Aggregate is
        // the existing 1.0 logical operator for duplicate elimination.
        operators.push(Operator {
            kind: PlanNodeKind::Aggregate,
            object: None,
            predicate: None,
            details: BTreeMap::from([(
                "operation".to_owned(),
                PlanDetail::Text("distinct".to_owned()),
            )]),
        });
    }
    if !query.ordering.is_empty() {
        operators.push(Operator {
            kind: PlanNodeKind::Sort,
            object: None,
            predicate: None,
            details: BTreeMap::from([(
                "keys".to_owned(),
                PlanDetail::Ordering(query.ordering.clone()),
            )]),
        });
    }
    if let Some(limit) = query.limit {
        operators.push(Operator {
            kind: PlanNodeKind::Limit,
            object: None,
            predicate: None,
            details: BTreeMap::from([("limit".to_owned(), PlanDetail::Integer(limit))]),
        });
    }
    build_explained_plan(query.snapshot.clone(), operators)
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
        children.push(Operator {
            kind,
            object: Some(edge.to),
            predicate: None,
            details: [
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
        });
    }
    let mut operators = Vec::with_capacity(children.len() + 1);
    operators.push(Operator {
        kind: PlanNodeKind::Invoke,
        object: Some(function_object),
        predicate: None,
        details: BTreeMap::new(),
    });
    // Children are ordered by their typed edge identity, independent of the
    // order in which a catalogue implementation supplied those rows.
    children.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.object.cmp(&right.object))
    });
    operators.extend(children);
    build_function_plan(function.snapshot.clone(), operators)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Operator {
    kind: PlanNodeKind,
    object: Option<ObjectRef>,
    predicate: Option<ExpressionRef>,
    details: BTreeMap<String, PlanDetail>,
}

fn build_explained_plan(
    snapshot: SnapshotRef,
    mut leaf_to_root: Vec<Operator>,
) -> Result<ExplainedPlan, ExplainError> {
    if leaf_to_root.is_empty() {
        return Err(ExplainError::TooManyNodes);
    }
    leaf_to_root.reverse();
    build_plan(snapshot, leaf_to_root, true)
}

fn build_function_plan(
    snapshot: SnapshotRef,
    root_then_children: Vec<Operator>,
) -> Result<ExplainedPlan, ExplainError> {
    build_plan(snapshot, root_then_children, false)
}

fn build_plan(
    snapshot: SnapshotRef,
    operators: Vec<Operator>,
    chain: bool,
) -> Result<ExplainedPlan, ExplainError> {
    if operators.len() > MAX_PLAN_NODES {
        return Err(ExplainError::TooManyNodes);
    }
    let mut identity = Sha256::new();
    identity.update(b"orna.sys.plan.v1\0");
    hash_part(&mut identity, snapshot.as_str().as_bytes());
    identity.update([u8::from(chain)]);
    for operator in &operators {
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
    }
    let digest = hex(&identity.finalize());
    let id = format!("plan:{digest}");
    let plan = PlanRef::descriptive(id.clone());
    let references = (0..operators.len())
        .map(|position| PlanNodeRef::descriptive(format!("{id}:{position}")))
        .collect::<Vec<_>>();

    let mut nodes = Vec::with_capacity(operators.len());
    for (position, operator) in operators.into_iter().enumerate() {
        let position_u32 = u32::try_from(position).map_err(|_| ExplainError::TooManyNodes)?;
        let parent_position = if chain {
            position.checked_sub(1)
        } else if position > 0 {
            Some(0)
        } else {
            None
        };
        let input_position = if chain {
            position.checked_add(1).filter(|next| *next < references.len())
        } else {
            None
        };
        let mut inputs = input_position
            .map(|index| vec![references[index].clone()])
            .unwrap_or_default();
        if !chain && position == 0 {
            inputs.extend(references.iter().skip(1).cloned());
        }
        nodes.push(PlanNode {
            reference: references[position].clone(),
            plan: plan.clone(),
            parent: parent_position.map(|index| references[index].clone()),
            position: position_u32,
            kind: operator.kind,
            inputs,
            object: operator.object,
            estimated_rows: None,
            actual_rows: None,
            estimated_bytes: None,
            actual_bytes: None,
            predicate: operator.predicate,
            details: operator.details,
        });
    }
    let root = references
        .first()
        .cloned()
        .ok_or(ExplainError::TooManyNodes)?;
    Ok(ExplainedPlan {
        plan: Plan {
            reference: plan,
            id,
            snapshot,
            root,
            estimated_cost: None,
            actual_available: false,
            warnings: Vec::new(),
        },
        nodes,
    })
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
