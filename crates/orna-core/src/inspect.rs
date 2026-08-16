//! Closed `sys.inspect` snapshot, projection, and trace model.
//!
//! Work ADR 0064 defines the server-side Inspector core: immutable inspection
//! epochs captured during protected invocations, eight closed projections
//! over an epoch, and a sequence-addressable trace stream, per the spec
//! `api/inspect.md`. This module is the pure, backend-independent model
//! slice: the epoch, its projection row sets, the trace stream, and the
//! closed capture options. It imports neither `orna-protocol` nor
//! `orna-standard`, exactly like the rest of orna-core.
//!
//! A snapshot is an immutable inspection epoch, modeled after the existing
//! `VerifiedStandardLibrarySnapshot` pattern (verified, Arc-immutable, pinned
//! by a revision pair) rather than `sys.state` (which has no epoch concept).
//! Every capture is a new epoch; an epoch exposes no mutation API, so a
//! returned epoch is immutable by construction.
//!
//! The model is closed in three senses. First, every epoch is pinned: the
//! source and catalogue revisions active at capture time are retained as
//! opaque [`SourceRevisionId`]/[`CatalogueRevisionId`] facts. Second, every
//! projection row is a closed struct with checked construction: an invariant
//! violation (an empty epoch, a root node with a parent, a value batch with
//! no values) fails closed with [`InspectError`] instead of producing a
//! malformed row. Third, the trace stream is contiguous: [`InspectTrace::push`]
//! admits each event at exactly the next 0-based sequence for its invocation.
//!
//! Redaction follows the spec: values are redacted/classified independently
//! of structural visibility. The epoch constructor applies the capture
//! options — a structural-only capture (the default) forces every state-cell
//! value to `None`, while a capture with `include_values` retains the typed
//! values. The INSPECT privilege ladder that decides who may see a
//! classified dimension lives in [`crate::security::authorise_inspect`].

use std::{error::Error, fmt, sync::Arc, time::SystemTime};

use crate::{
    CatalogueRevisionId, FunctionId, InspectEpochId, InvocationId, PrincipalId,
    SecurityAuditEventId, SourceRevisionId, TypeId, invocation::InvokeValue,
    state::UserStateKeyWithoutPrincipal, types::TypeDescriptor,
};

/// The closed INSPECT privilege set (spec `api/inspect.md`).
///
/// The first three privileges form the invocation-scope ladder:
/// [`OwnInvocation`](Self::OwnInvocation) is the weakest rung,
/// [`SessionInvocations`](Self::SessionInvocations) the middle, and
/// [`AnyInvocation`](Self::AnyInvocation) the strongest. The remaining four
/// are orthogonal content classifiers: each grants exactly one redaction
/// dimension (typed values, source text, security details, runtime
/// internals) independently of structural visibility. The ladder decision
/// that consumes this set is [`crate::security::authorise_inspect`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectPrivilege {
    /// Inspect invocations owned by the session principal.
    OwnInvocation,
    /// Inspect own and session-scoped invocations.
    SessionInvocations,
    /// Inspect any invocation regardless of owner.
    AnyInvocation,
    /// Read typed values captured by an epoch.
    Values,
    /// Read captured source text.
    Source,
    /// Read security decision details.
    SecurityDetails,
    /// Read runtime internals such as bindings and contracts.
    RuntimeInternals,
}

/// The orthogonal content dimension one classifier privilege grants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectClassifier {
    /// The typed-values dimension.
    Values,
    /// The source-text dimension.
    Source,
    /// The security-details dimension.
    SecurityDetails,
    /// The runtime-internals dimension.
    RuntimeInternals,
}

impl InspectPrivilege {
    /// Returns the invocation-scope ladder rung when this privilege is a
    /// ladder privilege: `OwnInvocation` is 0, `SessionInvocations` is 1,
    /// `AnyInvocation` is 2. Classifier privileges return `None`.
    pub const fn ladder_rank(self) -> Option<u8> {
        match self {
            Self::OwnInvocation => Some(0),
            Self::SessionInvocations => Some(1),
            Self::AnyInvocation => Some(2),
            Self::Values | Self::Source | Self::SecurityDetails | Self::RuntimeInternals => None,
        }
    }

    /// Returns whether this privilege is an invocation-scope ladder rung.
    pub const fn is_invocation_scope(self) -> bool {
        self.ladder_rank().is_some()
    }

    /// Returns the orthogonal classification dimension when this privilege
    /// is a content classifier, and `None` for ladder privileges.
    pub const fn classifier(self) -> Option<InspectClassifier> {
        match self {
            Self::Values => Some(InspectClassifier::Values),
            Self::Source => Some(InspectClassifier::Source),
            Self::SecurityDetails => Some(InspectClassifier::SecurityDetails),
            Self::RuntimeInternals => Some(InspectClassifier::RuntimeInternals),
            Self::OwnInvocation | Self::SessionInvocations | Self::AnyInvocation => None,
        }
    }

    /// Returns whether this privilege is an orthogonal content classifier.
    pub const fn is_classifier(self) -> bool {
        self.classifier().is_some()
    }
}

/// The closed capture options of one inspection epoch.
///
/// The default is the structural-only snapshot: invocation nodes, calls,
/// resources, and state-cell identities are captured, while typed values,
/// source text, security details, and runtime internals are redacted unless
/// the matching classifier was selected at capture time. The epoch
/// constructor applies these options to its row sets exactly once, so a
/// returned epoch's redaction state is immutable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectSnapshotOptions {
    include_values: bool,
    include_source: bool,
    include_security: bool,
    include_runtime: bool,
}

impl InspectSnapshotOptions {
    /// Creates one closed options value from the four classifier switches.
    pub const fn new(
        include_values: bool,
        include_source: bool,
        include_security: bool,
        include_runtime: bool,
    ) -> Self {
        Self {
            include_values,
            include_source,
            include_security,
            include_runtime,
        }
    }

    /// Returns the structural-only default options.
    pub const fn structural() -> Self {
        Self {
            include_values: false,
            include_source: false,
            include_security: false,
            include_runtime: false,
        }
    }

    /// Returns whether typed values are captured and retained.
    pub const fn include_values(self) -> bool {
        self.include_values
    }

    /// Returns whether source text is captured and retained.
    pub const fn include_source(self) -> bool {
        self.include_source
    }

    /// Returns whether security decision details are captured and retained.
    pub const fn include_security(self) -> bool {
        self.include_security
    }

    /// Returns whether runtime internals are captured and retained.
    pub const fn include_runtime(self) -> bool {
        self.include_runtime
    }
}

impl Default for InspectSnapshotOptions {
    fn default() -> Self {
        Self::structural()
    }
}

/// The closed outcome of the captured invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectOutcomeKind {
    /// The invocation was authorised and completed.
    Allowed,
    /// The invocation was denied before execution.
    Denied,
    /// The invocation failed during execution.
    Failed,
    /// The invocation was cancelled.
    Cancelled,
}

/// The closed result summary of one captured invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectResultSummary {
    /// No typed value batch was captured.
    NoValues,
    /// One non-empty typed value batch was captured.
    ValueBatch {
        /// The count of typed values in the captured batch.
        value_count: u64,
    },
}

/// The closed captured summary of one inspection epoch.
///
/// `event_count` is the total number of captured invocation events,
/// `result` is the closed result summary, and `duration_nanoseconds` is the
/// recorded execution duration when the invocation completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectSnapshotSummary {
    event_count: u64,
    result: InspectResultSummary,
    duration_nanoseconds: Option<u64>,
}

impl InspectSnapshotSummary {
    /// Creates one checked captured summary.
    ///
    /// A `ValueBatch` summary must name at least one captured value: a batch
    /// is non-empty by construction, so a zero count fails closed.
    pub fn new(
        event_count: u64,
        result: InspectResultSummary,
        duration_nanoseconds: Option<u64>,
    ) -> Result<Self, InspectError> {
        if matches!(result, InspectResultSummary::ValueBatch { value_count: 0 }) {
            return Err(InspectError::EmptyValueBatch);
        }
        Ok(Self {
            event_count,
            result,
            duration_nanoseconds,
        })
    }

    /// Returns the total number of captured invocation events.
    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    /// Returns the closed result summary.
    pub const fn result(&self) -> InspectResultSummary {
        self.result
    }

    /// Returns the recorded execution duration when the invocation completed.
    pub const fn duration_nanoseconds(&self) -> Option<u64> {
        self.duration_nanoseconds
    }
}

/// The closed kind of one invocation node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectInvocationNodeKind {
    /// The root invocation of the epoch.
    Root,
    /// A nested invocation inside the root.
    Nested,
}

/// The closed phase of one invocation node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectInvocationPhase {
    /// The invocation became visible to the stream.
    Started,
    /// The invocation is executing.
    Executing,
    /// The invocation completed.
    Completed,
    /// The invocation failed.
    Failed,
    /// The invocation was cancelled.
    Cancelled,
}

/// One closed `invocation_nodes` projection row.
///
/// The sealed v1 route models no nested calls, so a captured epoch carries
/// the root node only; the `Nested` kind and the parent rule are sealed in
/// so later slices can fill them without reshaping the model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationNodeRow {
    id: InvocationId,
    parent_id: Option<InvocationId>,
    kind: InspectInvocationNodeKind,
    phase: InspectInvocationPhase,
    target: FunctionId,
    sequence: u64,
}

impl InvocationNodeRow {
    /// Creates one checked invocation-node row.
    ///
    /// The parent rule is closed: a root node never carries a parent and a
    /// nested node always does. A row that violates the rule fails closed.
    pub fn new(
        id: InvocationId,
        parent_id: Option<InvocationId>,
        kind: InspectInvocationNodeKind,
        phase: InspectInvocationPhase,
        target: FunctionId,
        sequence: u64,
    ) -> Result<Self, InspectError> {
        match (kind, parent_id) {
            (InspectInvocationNodeKind::Root, Some(_)) => {
                return Err(InspectError::InvalidInvocationNodeParent { id });
            }
            (InspectInvocationNodeKind::Nested, None) => {
                return Err(InspectError::InvalidInvocationNodeParent { id });
            }
            (InspectInvocationNodeKind::Root, None)
            | (InspectInvocationNodeKind::Nested, Some(_)) => {}
        }
        Ok(Self {
            id,
            parent_id,
            kind,
            phase,
            target,
            sequence,
        })
    }

    /// Returns the invocation identity of this node.
    pub const fn id(&self) -> InvocationId {
        self.id
    }

    /// Returns the parent invocation when this node is nested.
    pub const fn parent_id(&self) -> Option<InvocationId> {
        self.parent_id
    }

    /// Returns the closed node kind.
    pub const fn kind(&self) -> InspectInvocationNodeKind {
        self.kind
    }

    /// Returns the closed node phase.
    pub const fn phase(&self) -> InspectInvocationPhase {
        self.phase
    }

    /// Returns the function targeted by this node.
    pub const fn target(&self) -> FunctionId {
        self.target
    }

    /// Returns the node sequence within the epoch.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// One closed `calls` projection row.
///
/// The sealed v1 route captures one root call and one `ValueBatch` summary,
/// so a captured epoch carries at most one call row.
#[derive(Clone, Debug, PartialEq)]
pub struct CallRow {
    invocation_id: InvocationId,
    schema: Option<InvokeValue>,
    value_count: u64,
    duration_nanoseconds: u64,
}

impl CallRow {
    /// Creates one checked call row.
    ///
    /// The batch rule is closed: a row whose schema names captured values
    /// must name at least one value (batches are non-empty), and a row
    /// without a schema may still carry captured values because the sealed
    /// v1 route emits value batches with no schema metadata. Violations fail
    /// closed.
    pub fn new(
        invocation_id: InvocationId,
        schema: Option<InvokeValue>,
        value_count: u64,
        duration_nanoseconds: u64,
    ) -> Result<Self, InspectError> {
        match (&schema, value_count) {
            (None, 0) => {}
            (Some(_), 0) => return Err(InspectError::EmptyValueBatch),
            (None, _) | (Some(_), _) => {}
        }
        Ok(Self {
            invocation_id,
            schema,
            value_count,
            duration_nanoseconds,
        })
    }

    /// Returns the invocation identity of this call.
    pub const fn invocation_id(&self) -> InvocationId {
        self.invocation_id
    }

    /// Returns the captured batch schema when a batch was captured.
    pub const fn schema(&self) -> Option<&InvokeValue> {
        self.schema.as_ref()
    }

    /// Returns the count of captured typed values.
    pub const fn value_count(&self) -> u64 {
        self.value_count
    }

    /// Returns the recorded execution duration in nanoseconds.
    pub const fn duration_nanoseconds(&self) -> u64 {
        self.duration_nanoseconds
    }
}

/// The closed kind of one captured resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectResourceKind {
    /// A durable USER state cell set.
    State,
    /// A pinned catalogue snapshot.
    Catalog,
    /// A pinned standard-library snapshot.
    Standard,
    /// A runtime binding.
    Runtime,
}

/// The closed status of one captured resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectResourceStatus {
    /// The resource is retained and usable.
    Active,
    /// The resource was invalidated.
    Invalidated,
    /// The resource was released.
    Released,
}

/// One closed `resources` projection row.
///
/// No resource tracking exists in v1, so the captured set is empty; the row
/// type is sealed so later slices can fill it without reshaping the model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceRow {
    kind: InspectResourceKind,
    status: InspectResourceStatus,
}

impl ResourceRow {
    /// Creates one resource row from its closed kind and status.
    pub const fn new(kind: InspectResourceKind, status: InspectResourceStatus) -> Self {
        Self { kind, status }
    }

    /// Returns the closed resource kind.
    pub const fn kind(&self) -> InspectResourceKind {
        self.kind
    }

    /// Returns the closed resource status.
    pub const fn status(&self) -> InspectResourceStatus {
        self.status
    }
}

/// One closed `state_cells` projection row.
///
/// The row carries the state-cell key without the session principal, the
/// persisted value type, the monotonic revision, and the boundary write
/// time. The typed value is redacted unless the epoch was captured with
/// INSPECT VALUES: the epoch constructor forces the value to `None` for a
/// structural-only capture, and the row is immutable afterwards.
#[derive(Clone, Debug, PartialEq)]
pub struct StateCellRow {
    key: UserStateKeyWithoutPrincipal,
    value_type: TypeId,
    revision: u64,
    updated_at: SystemTime,
    value: Option<InvokeValue>,
}

impl StateCellRow {
    /// Creates one row from its exact durable facts.
    ///
    /// The key is already validated by [`UserStateKeyWithoutPrincipal::new`],
    /// so this constructor is infallible. `value` carries the typed cell
    /// value; the epoch constructor redacts it unless values were captured.
    pub fn new(
        key: UserStateKeyWithoutPrincipal,
        value_type: TypeId,
        revision: u64,
        updated_at: SystemTime,
        value: Option<InvokeValue>,
    ) -> Self {
        Self {
            key,
            value_type,
            revision,
            updated_at,
            value,
        }
    }

    /// Returns the cell key without the session principal.
    pub const fn key(&self) -> &UserStateKeyWithoutPrincipal {
        &self.key
    }

    /// Returns the persisted type of the cell value.
    pub const fn value_type(&self) -> TypeId {
        self.value_type
    }

    /// Returns the monotonic revision of the cell.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the boundary write time of the cell.
    pub const fn updated_at(&self) -> SystemTime {
        self.updated_at
    }

    /// Returns the typed cell value when values were captured, and `None`
    /// when the row is redacted.
    pub const fn value(&self) -> Option<&InvokeValue> {
        self.value.as_ref()
    }

    /// Returns a copy of this row with the typed value redacted.
    fn redact(mut self) -> Self {
        self.value = None;
        self
    }
}

/// One closed `ui_nodes` projection row.
///
/// CLIENT execution is blocked in v1, so the captured set is empty; the row
/// type is sealed for the later CLIENT slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiNodeRow {
    function: FunctionId,
    call_site: String,
    runtime_contract: String,
}

impl UiNodeRow {
    /// Creates one checked UI-node row.
    ///
    /// The call-site and runtime-contract labels must be non-empty so the
    /// row cannot carry an unlabeled node.
    pub fn new(
        function: FunctionId,
        call_site: String,
        runtime_contract: String,
    ) -> Result<Self, InspectError> {
        if call_site.is_empty() {
            return Err(InspectError::EmptyUiNodeLabel { what: "call site" });
        }
        if runtime_contract.is_empty() {
            return Err(InspectError::EmptyUiNodeLabel {
                what: "runtime contract",
            });
        }
        Ok(Self {
            function,
            call_site,
            runtime_contract,
        })
    }

    /// Returns the function that owns this UI node.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the non-empty call-site label.
    pub fn call_site(&self) -> &str {
        &self.call_site
    }

    /// Returns the non-empty runtime-contract label.
    pub fn runtime_contract(&self) -> &str {
        &self.runtime_contract
    }
}

/// One closed `presentation_candidates` projection row.
///
/// The sealed v1 dispatch path resolves at most one accepted presenter, so a
/// captured epoch carries the final resolution: the accepted presenter, its
/// selected sink type, and the selected runtime when one was bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentationCandidateRow {
    presenter: String,
    accepted: bool,
    reason: String,
    selected_sink: Option<TypeDescriptor>,
    runtime: Option<String>,
}

impl PresentationCandidateRow {
    /// Creates one checked presentation-candidate row.
    ///
    /// The presenter name and the reason must be non-empty; a selected
    /// runtime, when present, must also be non-empty.
    pub fn new(
        presenter: String,
        accepted: bool,
        reason: String,
        selected_sink: Option<TypeDescriptor>,
        runtime: Option<String>,
    ) -> Result<Self, InspectError> {
        if presenter.is_empty() {
            return Err(InspectError::EmptyCandidateLabel { what: "presenter" });
        }
        if reason.is_empty() {
            return Err(InspectError::EmptyCandidateLabel { what: "reason" });
        }
        if runtime.as_deref() == Some("") {
            return Err(InspectError::EmptyCandidateLabel { what: "runtime" });
        }
        Ok(Self {
            presenter,
            accepted,
            reason,
            selected_sink,
            runtime,
        })
    }

    /// Returns the non-empty presenter name.
    pub fn presenter(&self) -> &str {
        &self.presenter
    }

    /// Returns whether this candidate was accepted.
    pub const fn accepted(&self) -> bool {
        self.accepted
    }

    /// Returns the non-empty acceptance or rejection reason.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Returns the selected sink type descriptor when one was resolved.
    pub const fn selected_sink(&self) -> Option<&TypeDescriptor> {
        self.selected_sink.as_ref()
    }

    /// Returns the selected runtime name when one was bound.
    pub fn runtime(&self) -> Option<&str> {
        self.runtime.as_deref()
    }
}

/// One closed `runtime_bindings` projection row.
///
/// The row records one offered runtime: its name and version, the type
/// descriptors it consumes, its negotiated contracts, whether it is trusted,
/// and its preference rank among the offers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeBindingRow {
    runtime_name: String,
    version: String,
    consumed_descriptors: Vec<TypeDescriptor>,
    contracts: Vec<(String, String, Vec<String>)>,
    trusted: bool,
    preference_rank: u32,
}

impl RuntimeBindingRow {
    /// Creates one checked runtime-binding row.
    ///
    /// The runtime name and version must be non-empty, and each contract
    /// must carry a non-empty name and version.
    pub fn new(
        runtime_name: String,
        version: String,
        consumed_descriptors: Vec<TypeDescriptor>,
        contracts: Vec<(String, String, Vec<String>)>,
        trusted: bool,
        preference_rank: u32,
    ) -> Result<Self, InspectError> {
        if runtime_name.is_empty() {
            return Err(InspectError::EmptyRuntimeLabel { what: "name" });
        }
        if version.is_empty() {
            return Err(InspectError::EmptyRuntimeLabel { what: "version" });
        }
        for (name, contract_version, _) in &contracts {
            if name.is_empty() || contract_version.is_empty() {
                return Err(InspectError::EmptyRuntimeContract);
            }
        }
        Ok(Self {
            runtime_name,
            version,
            consumed_descriptors,
            contracts,
            trusted,
            preference_rank,
        })
    }

    /// Returns the non-empty runtime name.
    pub fn runtime_name(&self) -> &str {
        &self.runtime_name
    }

    /// Returns the non-empty runtime version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the type descriptors this runtime consumes.
    pub fn consumed_descriptors(&self) -> &[TypeDescriptor] {
        &self.consumed_descriptors
    }

    /// Returns the negotiated contracts as `(name, version, properties)`.
    pub fn contracts(&self) -> &[(String, String, Vec<String>)] {
        &self.contracts
    }

    /// Returns whether this runtime is trusted.
    pub const fn trusted(&self) -> bool {
        self.trusted
    }

    /// Returns this runtime's preference rank among the offers.
    pub const fn preference_rank(&self) -> u32 {
        self.preference_rank
    }
}

/// The closed kind of one captured security decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectSecurityDecisionKind {
    /// A protected `EXECUTE` decision.
    Execute,
    /// A CLIENT capability requirement decision.
    Capability,
    /// A USER state operation decision.
    UserState,
    /// An INSPECT access decision.
    Inspect,
}

/// The closed outcome of one captured security decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectSecurityDecisionOutcome {
    /// The protected decision allowed the operation.
    Allowed,
    /// The protected decision denied the operation.
    Denied,
}

/// One closed `security_decisions` projection row.
///
/// The row records the decision kind and outcome, the involved principals,
/// the target function when one was named, the closed denial reason when the
/// decision was denied, and the opaque audit event references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityDecisionRow {
    kind: InspectSecurityDecisionKind,
    outcome: InspectSecurityDecisionOutcome,
    principals: Vec<PrincipalId>,
    target: Option<FunctionId>,
    denial_reason: Option<String>,
    audit_refs: Vec<SecurityAuditEventId>,
}

impl SecurityDecisionRow {
    /// Creates one checked security-decision row.
    ///
    /// The denial rule is closed: a denial must carry a non-empty reason and
    /// an allowed decision carries none. A violation fails closed.
    pub fn new(
        kind: InspectSecurityDecisionKind,
        outcome: InspectSecurityDecisionOutcome,
        principals: Vec<PrincipalId>,
        target: Option<FunctionId>,
        denial_reason: Option<String>,
        audit_refs: Vec<SecurityAuditEventId>,
    ) -> Result<Self, InspectError> {
        match (outcome, denial_reason.as_deref()) {
            (InspectSecurityDecisionOutcome::Denied, None | Some("")) => {
                return Err(InspectError::InvalidSecurityDecisionReason);
            }
            (InspectSecurityDecisionOutcome::Allowed, Some(_)) => {
                return Err(InspectError::InvalidSecurityDecisionReason);
            }
            (InspectSecurityDecisionOutcome::Denied, Some(_))
            | (InspectSecurityDecisionOutcome::Allowed, None) => {}
        }
        Ok(Self {
            kind,
            outcome,
            principals,
            target,
            denial_reason,
            audit_refs,
        })
    }

    /// Returns the closed decision kind.
    pub const fn kind(&self) -> InspectSecurityDecisionKind {
        self.kind
    }

    /// Returns the closed decision outcome.
    pub const fn outcome(&self) -> InspectSecurityDecisionOutcome {
        self.outcome
    }

    /// Returns the principals involved in the decision.
    pub fn principals(&self) -> &[PrincipalId] {
        &self.principals
    }

    /// Returns the target function when one was named.
    pub const fn target(&self) -> Option<FunctionId> {
        self.target
    }

    /// Returns the closed denial reason when the decision was denied.
    pub fn denial_reason(&self) -> Option<&str> {
        self.denial_reason.as_deref()
    }

    /// Returns the opaque audit event references.
    pub fn audit_refs(&self) -> &[SecurityAuditEventId] {
        &self.audit_refs
    }
}

/// The closed kind of one trace event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectTraceEventKind {
    /// The invocation started.
    InvocationStarted,
    /// A non-empty batch of typed values.
    ValueBatch,
    /// The invocation completed.
    InvocationCompleted,
    /// The invocation failed.
    InvocationFailed,
    /// The invocation was cancelled.
    InvocationCancelled,
}

/// The closed payload of one trace event.
#[derive(Clone, Debug, PartialEq)]
pub enum InspectTracePayload {
    /// The invocation became visible to the trace.
    Started,
    /// One non-empty batch of typed values.
    ValueBatch {
        /// The captured batch schema, when one was produced.
        schema: Option<InvokeValue>,
        /// The non-empty captured values.
        values: Vec<InvokeValue>,
    },
    /// The invocation completed with the stated duration.
    Completed {
        /// The recorded execution duration in nanoseconds.
        duration_nanoseconds: u64,
    },
    /// The invocation failed with a redacted code.
    Failed {
        /// The non-empty stable failure code.
        code: String,
    },
    /// The invocation was cancelled.
    Cancelled {
        /// The optional non-empty cancellation reason.
        reason: Option<String>,
    },
}

/// One closed trace event.
///
/// Sequence semantics are closed: events are contiguous per invocation and
/// 0-based, so a stream's Nth event carries sequence N. The standalone
/// constructor checks the event's self-contained facts; contiguity is
/// enforced by [`InspectTrace::push`] against the stream's next sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectTraceEvent {
    invocation_id: InvocationId,
    sequence: u64,
    payload: InspectTracePayload,
    recorded_at: SystemTime,
    observer_invocation: Option<InvocationId>,
    purpose: Option<String>,
}

impl InspectTraceEvent {
    /// Creates one checked trace event.
    ///
    /// The payload rules are closed: a value batch is non-empty, a failure
    /// carries a non-empty code, and a cancellation reason, when present, is
    /// non-empty. `purpose`, when present, must also be non-empty. The event
    /// kind is derived from the payload, so a kind and payload can never
    /// disagree.
    pub fn new(
        invocation_id: InvocationId,
        sequence: u64,
        payload: InspectTracePayload,
        recorded_at: SystemTime,
        observer_invocation: Option<InvocationId>,
        purpose: Option<String>,
    ) -> Result<Self, InspectError> {
        match &payload {
            InspectTracePayload::ValueBatch { values, .. } if values.is_empty() => {
                return Err(InspectError::EmptyValueBatch);
            }
            InspectTracePayload::Failed { code } if code.is_empty() => {
                return Err(InspectError::EmptyFailureCode);
            }
            InspectTracePayload::Cancelled {
                reason: Some(reason),
            } if reason.is_empty() => {
                return Err(InspectError::EmptyCancellationReason);
            }
            _ => {}
        }
        if purpose.as_deref() == Some("") {
            return Err(InspectError::EmptyPurpose);
        }
        Ok(Self {
            invocation_id,
            sequence,
            payload,
            recorded_at,
            observer_invocation,
            purpose,
        })
    }

    /// Returns the invocation identity this event belongs to.
    pub const fn invocation_id(&self) -> InvocationId {
        self.invocation_id
    }

    /// Returns the 0-based contiguous event sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the closed event kind derived from the payload.
    pub const fn kind(&self) -> InspectTraceEventKind {
        match &self.payload {
            InspectTracePayload::Started => InspectTraceEventKind::InvocationStarted,
            InspectTracePayload::ValueBatch { .. } => InspectTraceEventKind::ValueBatch,
            InspectTracePayload::Completed { .. } => InspectTraceEventKind::InvocationCompleted,
            InspectTracePayload::Failed { .. } => InspectTraceEventKind::InvocationFailed,
            InspectTracePayload::Cancelled { .. } => InspectTraceEventKind::InvocationCancelled,
        }
    }

    /// Returns the closed event payload.
    pub const fn payload(&self) -> &InspectTracePayload {
        &self.payload
    }

    /// Returns the recording time of this event.
    pub const fn recorded_at(&self) -> SystemTime {
        self.recorded_at
    }

    /// Returns the observing invocation when this event was produced by an
    /// inspector, enabling self-observation suppression.
    pub const fn observer_invocation(&self) -> Option<InvocationId> {
        self.observer_invocation
    }

    /// Returns the optional non-empty observation purpose.
    pub fn purpose(&self) -> Option<&str> {
        self.purpose.as_deref()
    }
}

/// One sequence-addressable, append-only trace stream.
///
/// The stream is closed: every event belongs to one invocation and is
/// admitted at exactly the next 0-based contiguous sequence. [`Self::push`]
/// fails closed on an event of another invocation or a non-contiguous
/// sequence, so a stream never contains a gap or a duplicate.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectTrace {
    invocation_id: InvocationId,
    events: Vec<InspectTraceEvent>,
}

impl InspectTrace {
    /// Creates one empty trace stream for one invocation.
    pub fn new(invocation_id: InvocationId) -> Self {
        Self {
            invocation_id,
            events: Vec::new(),
        }
    }

    /// Appends one event at the next contiguous sequence.
    ///
    /// The event must belong to this stream's invocation and must carry the
    /// stream's next sequence, which is 0 for the first event and the current
    /// length afterwards. Violations fail closed.
    pub fn push(&mut self, event: InspectTraceEvent) -> Result<(), InspectError> {
        if event.invocation_id() != self.invocation_id {
            return Err(InspectError::TraceInvocationMismatch {
                invocation_id: self.invocation_id,
                event_invocation_id: event.invocation_id(),
            });
        }
        let expected = self.events.len() as u64;
        if event.sequence() != expected {
            return Err(InspectError::NonContiguousTraceSequence {
                invocation_id: self.invocation_id,
                expected,
                actual: event.sequence(),
            });
        }
        self.events.push(event);
        Ok(())
    }

    /// Returns the invocation identity this stream records.
    pub const fn invocation_id(&self) -> InvocationId {
        self.invocation_id
    }

    /// Returns the admitted events in 0-based contiguous order.
    pub fn events(&self) -> &[InspectTraceEvent] {
        &self.events
    }

    /// Returns the next sequence that will be admitted.
    pub const fn next_sequence(&self) -> u64 {
        self.events.len() as u64
    }

    /// Iterates over events with `sequence > after` in contiguous order,
    /// mirroring `sys.inspect.trace(p_after_sequence)`.
    pub fn after_sequence(&self, after: u64) -> impl Iterator<Item = &InspectTraceEvent> {
        self.events
            .iter()
            .filter(move |event| event.sequence() > after)
    }
}

/// One immutable inspection epoch.
///
/// The epoch is captured during a protected invocation and pinned by the
/// source and catalogue revisions active at capture time. Every capture is a
/// new epoch; the epoch exposes no mutation API, so a returned epoch is
/// immutable by construction (Arc-backed, mirroring
/// `VerifiedStandardLibrarySnapshot`).
#[derive(Clone, Debug)]
pub struct InspectSnapshotEpoch {
    inner: Arc<InspectSnapshotEpochData>,
}

#[derive(Debug)]
struct InspectSnapshotEpochData {
    id: InspectEpochId,
    invocation_id: InvocationId,
    source_revision_id: SourceRevisionId,
    catalogue_revision_id: CatalogueRevisionId,
    owner: PrincipalId,
    recorded_at: SystemTime,
    root_target: FunctionId,
    outcome: InspectOutcomeKind,
    summary: InspectSnapshotSummary,
    invocation_nodes: Vec<InvocationNodeRow>,
    calls: Vec<CallRow>,
    resources: Vec<ResourceRow>,
    state_cells: Vec<StateCellRow>,
    ui_nodes: Vec<UiNodeRow>,
    presentation_candidates: Vec<PresentationCandidateRow>,
    runtime_bindings: Vec<RuntimeBindingRow>,
    security_decisions: Vec<SecurityDecisionRow>,
}

impl InspectSnapshotEpoch {
    /// Captures one checked inspection epoch from its exact facts.
    ///
    /// The epoch must carry at least one projection row: an epoch that
    /// captured nothing is not an inspection record and fails closed. The
    /// capture options apply exactly once: a structural-only capture (the
    /// default) redacts every state-cell value to `None`, while a capture
    /// with `include_values` retains the typed values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: InspectEpochId,
        invocation_id: InvocationId,
        source_revision_id: SourceRevisionId,
        catalogue_revision_id: CatalogueRevisionId,
        owner: PrincipalId,
        recorded_at: SystemTime,
        root_target: FunctionId,
        outcome: InspectOutcomeKind,
        summary: InspectSnapshotSummary,
        options: &InspectSnapshotOptions,
        invocation_nodes: Vec<InvocationNodeRow>,
        calls: Vec<CallRow>,
        resources: Vec<ResourceRow>,
        state_cells: Vec<StateCellRow>,
        ui_nodes: Vec<UiNodeRow>,
        presentation_candidates: Vec<PresentationCandidateRow>,
        runtime_bindings: Vec<RuntimeBindingRow>,
        security_decisions: Vec<SecurityDecisionRow>,
    ) -> Result<Self, InspectError> {
        let row_count = invocation_nodes.len()
            + calls.len()
            + resources.len()
            + state_cells.len()
            + ui_nodes.len()
            + presentation_candidates.len()
            + runtime_bindings.len()
            + security_decisions.len();
        if row_count == 0 {
            return Err(InspectError::EmptyEpoch { id });
        }
        let state_cells = if options.include_values() {
            state_cells
        } else {
            state_cells.into_iter().map(StateCellRow::redact).collect()
        };
        Ok(Self {
            inner: Arc::new(InspectSnapshotEpochData {
                id,
                invocation_id,
                source_revision_id,
                catalogue_revision_id,
                owner,
                recorded_at,
                root_target,
                outcome,
                summary,
                invocation_nodes,
                calls,
                resources,
                state_cells,
                ui_nodes,
                presentation_candidates,
                runtime_bindings,
                security_decisions,
            }),
        })
    }

    /// Returns this immutable epoch identity.
    pub fn id(&self) -> InspectEpochId {
        self.inner.id
    }

    /// Returns the invocation identity this epoch captures.
    pub fn invocation_id(&self) -> InvocationId {
        self.inner.invocation_id
    }

    /// Returns the source revision pinned at capture time.
    pub fn source_revision_id(&self) -> SourceRevisionId {
        self.inner.source_revision_id
    }

    /// Returns the catalogue revision pinned at capture time.
    pub fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        self.inner.catalogue_revision_id
    }

    /// Returns the principal that owns this epoch.
    pub fn owner(&self) -> PrincipalId {
        self.inner.owner
    }

    /// Returns the capture time of this epoch.
    pub fn recorded_at(&self) -> SystemTime {
        self.inner.recorded_at
    }

    /// Returns the root function targeted by the captured invocation.
    pub fn root_target(&self) -> FunctionId {
        self.inner.root_target
    }

    /// Returns the closed outcome of the captured invocation.
    pub fn outcome(&self) -> InspectOutcomeKind {
        self.inner.outcome
    }

    /// Returns the closed captured summary.
    pub fn summary(&self) -> InspectSnapshotSummary {
        self.inner.summary
    }

    /// Returns the `invocation_nodes` projection rows.
    pub fn invocation_nodes(&self) -> &[InvocationNodeRow] {
        &self.inner.invocation_nodes
    }

    /// Returns the `calls` projection rows.
    pub fn calls(&self) -> &[CallRow] {
        &self.inner.calls
    }

    /// Returns the `resources` projection rows.
    pub fn resources(&self) -> &[ResourceRow] {
        &self.inner.resources
    }

    /// Returns the `state_cells` projection rows.
    pub fn state_cells(&self) -> &[StateCellRow] {
        &self.inner.state_cells
    }

    /// Returns the `ui_nodes` projection rows.
    pub fn ui_nodes(&self) -> &[UiNodeRow] {
        &self.inner.ui_nodes
    }

    /// Returns the `presentation_candidates` projection rows.
    pub fn presentation_candidates(&self) -> &[PresentationCandidateRow] {
        &self.inner.presentation_candidates
    }

    /// Returns the `runtime_bindings` projection rows.
    pub fn runtime_bindings(&self) -> &[RuntimeBindingRow] {
        &self.inner.runtime_bindings
    }

    /// Returns the `security_decisions` projection rows.
    pub fn security_decisions(&self) -> &[SecurityDecisionRow] {
        &self.inner.security_decisions
    }
}

/// A closed inspection model failure.
///
/// The variants are model-shape failures that fail closed on invariant
/// violations; they never map to a client spec code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InspectError {
    /// An epoch captured no projection rows at all.
    EmptyEpoch {
        /// The rejected epoch identity.
        id: InspectEpochId,
    },
    /// A value batch must be non-empty.
    EmptyValueBatch,
    /// An invocation node violates the root/nested parent rule.
    InvalidInvocationNodeParent {
        /// The rejected node identity.
        id: InvocationId,
    },
    /// A UI node row carries an empty label.
    EmptyUiNodeLabel {
        /// The empty label kind.
        what: &'static str,
    },
    /// A presentation candidate row carries an empty label.
    EmptyCandidateLabel {
        /// The empty label kind.
        what: &'static str,
    },
    /// A runtime binding row carries an empty label.
    EmptyRuntimeLabel {
        /// The empty label kind.
        what: &'static str,
    },
    /// A runtime contract names an empty contract name or version.
    EmptyRuntimeContract,
    /// A security decision row carries an inconsistent denial reason.
    InvalidSecurityDecisionReason,
    /// A trace event carries an empty failure code.
    EmptyFailureCode,
    /// A trace event carries an empty cancellation reason.
    EmptyCancellationReason,
    /// A trace event carries an empty purpose.
    EmptyPurpose,
    /// A trace event belongs to another invocation's stream.
    TraceInvocationMismatch {
        /// The stream's invocation.
        invocation_id: InvocationId,
        /// The rejected event's invocation.
        event_invocation_id: InvocationId,
    },
    /// A trace event sequence is not the next contiguous sequence.
    NonContiguousTraceSequence {
        /// The stream's invocation.
        invocation_id: InvocationId,
        /// The expected next sequence.
        expected: u64,
        /// The rejected event's sequence.
        actual: u64,
    },
}

impl fmt::Display for InspectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEpoch { id } => {
                write!(
                    formatter,
                    "inspection epoch {id} captured no projection rows"
                )
            }
            Self::EmptyValueBatch => {
                formatter.write_str("an inspection value batch must not be empty")
            }
            Self::InvalidInvocationNodeParent { id } => write!(
                formatter,
                "inspection invocation node {id} violates the root/nested parent rule"
            ),
            Self::EmptyUiNodeLabel { what } => {
                write!(
                    formatter,
                    "inspection UI node {what} label must not be empty"
                )
            }
            Self::EmptyCandidateLabel { what } => write!(
                formatter,
                "inspection presentation candidate {what} must not be empty"
            ),
            Self::EmptyRuntimeLabel { what } => {
                write!(
                    formatter,
                    "inspection runtime binding {what} must not be empty"
                )
            }
            Self::EmptyRuntimeContract => formatter
                .write_str("inspection runtime contract name and version must not be empty"),
            Self::InvalidSecurityDecisionReason => {
                formatter.write_str("inspection security decision reason is inconsistent")
            }
            Self::EmptyFailureCode => {
                formatter.write_str("inspection trace failure code must not be empty")
            }
            Self::EmptyCancellationReason => {
                formatter.write_str("inspection trace cancellation reason must not be empty")
            }
            Self::EmptyPurpose => formatter.write_str("inspection trace purpose must not be empty"),
            Self::TraceInvocationMismatch {
                invocation_id,
                event_invocation_id,
            } => write!(
                formatter,
                "inspection trace event {event_invocation_id} does not belong to invocation \
                 {invocation_id}"
            ),
            Self::NonContiguousTraceSequence {
                invocation_id,
                expected,
                actual,
            } => write!(
                formatter,
                "inspection trace for {invocation_id} expected sequence {expected} but received \
                 {actual}"
            ),
        }
    }
}

impl Error for InspectError {}

#[cfg(test)]
mod tests {
    use crate::{StateSlotId, value::RuntimeValue};

    use super::*;

    const PRINCIPAL_A: u8 = 0x11;
    const ROOT_FUNCTION: u8 = 0x33;
    const NESTED_FUNCTION: u8 = 0x44;
    const INVOCATION: u8 = 0x55;
    const PARENT_INVOCATION: u8 = 0x66;
    const SLOT: u8 = 0x77;
    const TYPE_INT: u8 = 0x88;
    const SOURCE_REVISION: u8 = 0x99;
    const CATALOGUE_REVISION: u8 = 0xaa;
    const EPOCH: u8 = 0xbb;

    fn invocation_id(byte: u8) -> InvocationId {
        InvocationId::from_bytes([byte; 16])
    }

    fn function_id(byte: u8) -> FunctionId {
        FunctionId::from_bytes([byte; 16])
    }

    fn principal_id(byte: u8) -> PrincipalId {
        PrincipalId::from_bytes([byte; 16])
    }

    fn type_id(byte: u8) -> TypeId {
        TypeId::from_bytes([byte; 16])
    }

    fn epoch_id(byte: u8) -> InspectEpochId {
        InspectEpochId::from_bytes([byte; 16])
    }

    fn source_revision_id(byte: u8) -> SourceRevisionId {
        SourceRevisionId::from_bytes([byte; 16])
    }

    fn catalogue_revision_id(byte: u8) -> CatalogueRevisionId {
        CatalogueRevisionId::from_bytes([byte; 16])
    }

    fn state_slot_id(byte: u8) -> StateSlotId {
        StateSlotId::from_bytes([byte; 16])
    }

    fn value(integer: i32) -> InvokeValue {
        InvokeValue::new(RuntimeValue::Integer(integer)).expect("a scalar value must be admitted")
    }

    fn state_key() -> UserStateKeyWithoutPrincipal {
        UserStateKeyWithoutPrincipal::new(
            function_id(ROOT_FUNCTION),
            String::new(),
            function_id(NESTED_FUNCTION),
            "tab-2".to_owned(),
            state_slot_id(SLOT),
        )
        .expect("a valid test key")
    }

    fn root_node() -> InvocationNodeRow {
        InvocationNodeRow::new(
            invocation_id(INVOCATION),
            None,
            InspectInvocationNodeKind::Root,
            InspectInvocationPhase::Completed,
            function_id(ROOT_FUNCTION),
            0,
        )
        .expect("a valid root node")
    }

    fn call_row() -> CallRow {
        CallRow::new(invocation_id(INVOCATION), Some(value(7)), 1, 42).expect("a valid call row")
    }

    fn state_cell_row() -> StateCellRow {
        StateCellRow::new(
            state_key(),
            type_id(TYPE_INT),
            3,
            SystemTime::UNIX_EPOCH,
            Some(value(7)),
        )
    }

    fn default_summary() -> InspectSnapshotSummary {
        InspectSnapshotSummary::new(
            3,
            InspectResultSummary::ValueBatch { value_count: 1 },
            Some(42),
        )
        .expect("a valid test summary")
    }

    #[allow(clippy::too_many_arguments)]
    fn epoch_with(
        options: &InspectSnapshotOptions,
        state_cells: Vec<StateCellRow>,
    ) -> InspectSnapshotEpoch {
        InspectSnapshotEpoch::new(
            epoch_id(EPOCH),
            invocation_id(INVOCATION),
            source_revision_id(SOURCE_REVISION),
            catalogue_revision_id(CATALOGUE_REVISION),
            principal_id(PRINCIPAL_A),
            SystemTime::UNIX_EPOCH,
            function_id(ROOT_FUNCTION),
            InspectOutcomeKind::Allowed,
            default_summary(),
            options,
            vec![root_node()],
            vec![call_row()],
            vec![],
            state_cells,
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect("a valid test epoch")
    }

    fn default_epoch() -> InspectSnapshotEpoch {
        epoch_with(
            &InspectSnapshotOptions::structural(),
            vec![state_cell_row()],
        )
    }

    fn trace_event(sequence: u64, payload: InspectTracePayload) -> InspectTraceEvent {
        InspectTraceEvent::new(
            invocation_id(INVOCATION),
            sequence,
            payload,
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("a valid test trace event")
    }

    #[test]
    fn privilege_ladder_ranks_are_ordered_own_session_any() {
        assert_eq!(InspectPrivilege::OwnInvocation.ladder_rank(), Some(0));
        assert_eq!(InspectPrivilege::SessionInvocations.ladder_rank(), Some(1));
        assert_eq!(InspectPrivilege::AnyInvocation.ladder_rank(), Some(2));
        assert!(
            InspectPrivilege::OwnInvocation.ladder_rank()
                < InspectPrivilege::SessionInvocations.ladder_rank()
        );
        assert!(
            InspectPrivilege::SessionInvocations.ladder_rank()
                < InspectPrivilege::AnyInvocation.ladder_rank()
        );
    }

    #[test]
    fn classifier_privileges_group_orthogonally() {
        assert_eq!(
            InspectPrivilege::Values.classifier(),
            Some(InspectClassifier::Values)
        );
        assert_eq!(
            InspectPrivilege::Source.classifier(),
            Some(InspectClassifier::Source)
        );
        assert_eq!(
            InspectPrivilege::SecurityDetails.classifier(),
            Some(InspectClassifier::SecurityDetails)
        );
        assert_eq!(
            InspectPrivilege::RuntimeInternals.classifier(),
            Some(InspectClassifier::RuntimeInternals)
        );
        for privilege in [
            InspectPrivilege::Values,
            InspectPrivilege::Source,
            InspectPrivilege::SecurityDetails,
            InspectPrivilege::RuntimeInternals,
        ] {
            assert!(privilege.is_classifier());
            assert!(!privilege.is_invocation_scope());
            assert_eq!(privilege.ladder_rank(), None);
        }
        for privilege in [
            InspectPrivilege::OwnInvocation,
            InspectPrivilege::SessionInvocations,
            InspectPrivilege::AnyInvocation,
        ] {
            assert!(!privilege.is_classifier());
            assert!(privilege.is_invocation_scope());
            assert_eq!(privilege.classifier(), None);
        }
    }

    #[test]
    fn snapshot_options_default_to_structural_only() {
        let options = InspectSnapshotOptions::default();
        assert!(!options.include_values());
        assert!(!options.include_source());
        assert!(!options.include_security());
        assert!(!options.include_runtime());
        assert_eq!(options, InspectSnapshotOptions::structural());
    }

    #[test]
    fn snapshot_options_expose_the_four_classifier_switches() {
        let options = InspectSnapshotOptions::new(true, false, true, false);
        assert!(options.include_values());
        assert!(!options.include_source());
        assert!(options.include_security());
        assert!(!options.include_runtime());
        let all = InspectSnapshotOptions::new(true, true, true, true);
        assert!(all.include_values() && all.include_source());
        assert!(all.include_security() && all.include_runtime());
    }

    #[test]
    fn epoch_construction_fails_closed_on_an_empty_epoch() {
        let error = InspectSnapshotEpoch::new(
            epoch_id(EPOCH),
            invocation_id(INVOCATION),
            source_revision_id(SOURCE_REVISION),
            catalogue_revision_id(CATALOGUE_REVISION),
            principal_id(PRINCIPAL_A),
            SystemTime::UNIX_EPOCH,
            function_id(ROOT_FUNCTION),
            InspectOutcomeKind::Allowed,
            default_summary(),
            &InspectSnapshotOptions::structural(),
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .expect_err("an epoch with no projection rows must fail closed");
        assert_eq!(
            error,
            InspectError::EmptyEpoch {
                id: epoch_id(EPOCH)
            }
        );
    }

    #[test]
    fn epoch_is_immutable_and_exposes_its_capture_facts() {
        let epoch = default_epoch();
        assert_eq!(epoch.id(), epoch_id(EPOCH));
        assert_eq!(epoch.invocation_id(), invocation_id(INVOCATION));
        assert_eq!(
            epoch.source_revision_id(),
            source_revision_id(SOURCE_REVISION)
        );
        assert_eq!(
            epoch.catalogue_revision_id(),
            catalogue_revision_id(CATALOGUE_REVISION)
        );
        assert_eq!(epoch.owner(), principal_id(PRINCIPAL_A));
        assert_eq!(epoch.recorded_at(), SystemTime::UNIX_EPOCH);
        assert_eq!(epoch.root_target(), function_id(ROOT_FUNCTION));
        assert_eq!(epoch.outcome(), InspectOutcomeKind::Allowed);
        assert_eq!(epoch.summary(), default_summary());
        let again = default_epoch();
        assert_eq!(epoch.invocation_nodes(), again.invocation_nodes());
        assert_eq!(epoch.calls(), again.calls());
        assert_eq!(epoch.state_cells(), again.state_cells());
    }

    #[test]
    fn epoch_redacts_state_cell_values_without_include_values() {
        let epoch = default_epoch();
        assert_eq!(epoch.state_cells().len(), 1);
        let row = &epoch.state_cells()[0];
        assert_eq!(row.value(), None);
        assert_eq!(row.value_type(), type_id(TYPE_INT));
        assert_eq!(row.revision(), 3);
        assert_eq!(row.updated_at(), SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn epoch_retains_state_cell_values_with_include_values() {
        let captured = value(7);
        let epoch = epoch_with(
            &InspectSnapshotOptions::new(true, false, false, false),
            vec![state_cell_row()],
        );
        assert_eq!(epoch.state_cells().len(), 1);
        assert_eq!(epoch.state_cells()[0].value(), Some(&captured));
    }

    #[test]
    fn epoch_row_accessors_expose_each_projection_set() {
        let epoch = default_epoch();
        assert_eq!(epoch.invocation_nodes().len(), 1);
        assert_eq!(epoch.calls().len(), 1);
        assert!(epoch.resources().is_empty());
        assert_eq!(epoch.state_cells().len(), 1);
        assert!(epoch.ui_nodes().is_empty());
        assert!(epoch.presentation_candidates().is_empty());
        assert!(epoch.runtime_bindings().is_empty());
        assert!(epoch.security_decisions().is_empty());
        assert_eq!(epoch.invocation_nodes()[0].id(), invocation_id(INVOCATION));
        assert_eq!(epoch.calls()[0].invocation_id(), invocation_id(INVOCATION));
        assert_eq!(
            epoch.state_cells()[0].key().function(),
            function_id(NESTED_FUNCTION)
        );
    }

    #[test]
    fn summary_rejects_a_zero_count_value_batch() {
        let error = InspectSnapshotSummary::new(
            2,
            InspectResultSummary::ValueBatch { value_count: 0 },
            None,
        )
        .expect_err("a value batch is non-empty by construction");
        assert_eq!(error, InspectError::EmptyValueBatch);
        let no_values = InspectSnapshotSummary::new(1, InspectResultSummary::NoValues, None)
            .expect("a no-values summary is valid");
        assert_eq!(no_values.result(), InspectResultSummary::NoValues);
        assert_eq!(no_values.duration_nanoseconds(), None);
        let summary = default_summary();
        assert_eq!(summary.event_count(), 3);
        assert_eq!(
            summary.result(),
            InspectResultSummary::ValueBatch { value_count: 1 }
        );
        assert_eq!(summary.duration_nanoseconds(), Some(42));
    }

    #[test]
    fn invocation_node_row_requires_a_root_without_parent() {
        let error = InvocationNodeRow::new(
            invocation_id(INVOCATION),
            Some(invocation_id(PARENT_INVOCATION)),
            InspectInvocationNodeKind::Root,
            InspectInvocationPhase::Completed,
            function_id(ROOT_FUNCTION),
            0,
        )
        .expect_err("a root node cannot carry a parent");
        assert!(matches!(
            error,
            InspectError::InvalidInvocationNodeParent { .. }
        ));
    }

    #[test]
    fn invocation_node_row_requires_a_nested_node_with_a_parent() {
        let error = InvocationNodeRow::new(
            invocation_id(INVOCATION),
            None,
            InspectInvocationNodeKind::Nested,
            InspectInvocationPhase::Started,
            function_id(NESTED_FUNCTION),
            1,
        )
        .expect_err("a nested node must carry its parent");
        assert!(matches!(
            error,
            InspectError::InvalidInvocationNodeParent { .. }
        ));
        let nested = InvocationNodeRow::new(
            invocation_id(INVOCATION),
            Some(invocation_id(PARENT_INVOCATION)),
            InspectInvocationNodeKind::Nested,
            InspectInvocationPhase::Started,
            function_id(NESTED_FUNCTION),
            1,
        )
        .expect("a nested node with a parent is valid");
        assert_eq!(nested.id(), invocation_id(INVOCATION));
        assert_eq!(nested.parent_id(), Some(invocation_id(PARENT_INVOCATION)));
        assert_eq!(nested.kind(), InspectInvocationNodeKind::Nested);
        assert_eq!(nested.phase(), InspectInvocationPhase::Started);
        assert_eq!(nested.target(), function_id(NESTED_FUNCTION));
        assert_eq!(nested.sequence(), 1);
    }

    #[test]
    fn call_row_accepts_captured_values_without_a_schema() {
        // The sealed v1 route emits value batches with values but no schema
        // metadata, so a captured batch must be valid without a schema.
        let call = CallRow::new(invocation_id(INVOCATION), None, 1, 42)
            .expect("captured values without a schema are valid");
        assert_eq!(call.invocation_id(), invocation_id(INVOCATION));
        assert_eq!(call.schema(), None);
        assert_eq!(call.value_count(), 1);
        assert_eq!(call.duration_nanoseconds(), 42);
    }

    #[test]
    fn call_row_requires_values_when_a_schema_is_captured() {
        let error = CallRow::new(invocation_id(INVOCATION), Some(value(7)), 0, 42)
            .expect_err("a captured schema names at least one value");
        assert_eq!(error, InspectError::EmptyValueBatch);
        let schema = value(7);
        let call = call_row();
        assert_eq!(call.invocation_id(), invocation_id(INVOCATION));
        assert_eq!(call.schema(), Some(&schema));
        assert_eq!(call.value_count(), 1);
        assert_eq!(call.duration_nanoseconds(), 42);
        let bare = CallRow::new(invocation_id(INVOCATION), None, 0, 0)
            .expect("a call row with no captured batch is valid");
        assert_eq!(bare.schema(), None);
        assert_eq!(bare.value_count(), 0);
    }

    #[test]
    fn resource_row_carries_a_closed_kind_and_status() {
        let state = ResourceRow::new(InspectResourceKind::State, InspectResourceStatus::Active);
        assert_eq!(state.kind(), InspectResourceKind::State);
        assert_eq!(state.status(), InspectResourceStatus::Active);
        let runtime = ResourceRow::new(
            InspectResourceKind::Runtime,
            InspectResourceStatus::Invalidated,
        );
        assert_eq!(runtime.kind(), InspectResourceKind::Runtime);
        assert_eq!(runtime.status(), InspectResourceStatus::Invalidated);
        let released = ResourceRow::new(
            InspectResourceKind::Catalog,
            InspectResourceStatus::Released,
        );
        assert_eq!(released.status(), InspectResourceStatus::Released);
    }

    #[test]
    fn state_cell_row_exposes_key_facts_without_principal() {
        let captured = value(7);
        let row = state_cell_row();
        let key = row.key();
        assert_eq!(key.root_function(), function_id(ROOT_FUNCTION));
        assert_eq!(key.state_profile(), "");
        assert_eq!(key.function(), function_id(NESTED_FUNCTION));
        assert_eq!(key.instance_key(), "tab-2");
        assert_eq!(key.state_slot(), state_slot_id(SLOT));
        assert_eq!(row.value_type(), type_id(TYPE_INT));
        assert_eq!(row.revision(), 3);
        assert_eq!(row.updated_at(), SystemTime::UNIX_EPOCH);
        assert_eq!(row.value(), Some(&captured));
    }

    #[test]
    fn redacted_state_cell_row_carries_no_value() {
        let row = state_cell_row().redact();
        assert_eq!(row.value(), None);
        assert_eq!(row.value_type(), type_id(TYPE_INT));
        assert_eq!(row.revision(), 3);
    }

    #[test]
    fn ui_node_row_rejects_empty_labels() {
        assert!(matches!(
            UiNodeRow::new(
                function_id(NESTED_FUNCTION),
                String::new(),
                "orna_runtime_abi_v1".to_owned(),
            ),
            Err(InspectError::EmptyUiNodeLabel { .. })
        ));
        assert!(matches!(
            UiNodeRow::new(
                function_id(NESTED_FUNCTION),
                "main".to_owned(),
                String::new()
            ),
            Err(InspectError::EmptyUiNodeLabel { .. })
        ));
        let node = UiNodeRow::new(
            function_id(NESTED_FUNCTION),
            "main".to_owned(),
            "orna_runtime_abi_v1".to_owned(),
        )
        .expect("a fully labelled UI node is valid");
        assert_eq!(node.function(), function_id(NESTED_FUNCTION));
        assert_eq!(node.call_site(), "main");
        assert_eq!(node.runtime_contract(), "orna_runtime_abi_v1");
    }

    #[test]
    fn presentation_candidate_row_rejects_empty_labels_and_runtime() {
        assert!(matches!(
            PresentationCandidateRow::new(String::new(), true, "selected".to_owned(), None, None),
            Err(InspectError::EmptyCandidateLabel { .. })
        ));
        assert!(matches!(
            PresentationCandidateRow::new(
                "terminal_table".to_owned(),
                true,
                String::new(),
                None,
                None
            ),
            Err(InspectError::EmptyCandidateLabel { .. })
        ));
        assert!(matches!(
            PresentationCandidateRow::new(
                "terminal_table".to_owned(),
                true,
                "selected".to_owned(),
                None,
                Some(String::new()),
            ),
            Err(InspectError::EmptyCandidateLabel { .. })
        ));
        let sink = TypeDescriptor::named(type_id(TYPE_INT));
        let candidate = PresentationCandidateRow::new(
            "terminal_table".to_owned(),
            true,
            "single terminal runtime".to_owned(),
            Some(sink.clone()),
            Some("tty".to_owned()),
        )
        .expect("a fully labelled candidate is valid");
        assert_eq!(candidate.presenter(), "terminal_table");
        assert!(candidate.accepted());
        assert_eq!(candidate.reason(), "single terminal runtime");
        assert_eq!(candidate.selected_sink(), Some(&sink));
        assert_eq!(candidate.runtime(), Some("tty"));
    }

    #[test]
    fn runtime_binding_row_rejects_empty_labels_and_contracts() {
        assert!(matches!(
            RuntimeBindingRow::new(String::new(), "1.0".to_owned(), vec![], vec![], true, 0),
            Err(InspectError::EmptyRuntimeLabel { .. })
        ));
        assert!(matches!(
            RuntimeBindingRow::new("tty".to_owned(), String::new(), vec![], vec![], true, 0),
            Err(InspectError::EmptyRuntimeLabel { .. })
        ));
        assert!(matches!(
            RuntimeBindingRow::new(
                "tty".to_owned(),
                "1.0".to_owned(),
                vec![],
                vec![("".to_owned(), "v1".to_owned(), vec![])],
                true,
                0,
            ),
            Err(InspectError::EmptyRuntimeContract)
        ));
        let descriptor = TypeDescriptor::named(type_id(TYPE_INT));
        let binding = RuntimeBindingRow::new(
            "tty".to_owned(),
            "1.0".to_owned(),
            vec![descriptor.clone()],
            vec![("ui".to_owned(), "v1".to_owned(), vec!["window".to_owned()])],
            true,
            0,
        )
        .expect("a fully labelled binding is valid");
        assert_eq!(binding.runtime_name(), "tty");
        assert_eq!(binding.version(), "1.0");
        assert_eq!(binding.consumed_descriptors(), &[descriptor]);
        assert_eq!(
            binding.contracts(),
            &[("ui".to_owned(), "v1".to_owned(), vec!["window".to_owned()])]
        );
        assert!(binding.trusted());
        assert_eq!(binding.preference_rank(), 0);
    }

    #[test]
    fn security_decision_row_denial_requires_a_closed_reason() {
        assert!(matches!(
            SecurityDecisionRow::new(
                InspectSecurityDecisionKind::Execute,
                InspectSecurityDecisionOutcome::Denied,
                vec![principal_id(PRINCIPAL_A)],
                Some(function_id(ROOT_FUNCTION)),
                None,
                vec![],
            ),
            Err(InspectError::InvalidSecurityDecisionReason)
        ));
        assert!(matches!(
            SecurityDecisionRow::new(
                InspectSecurityDecisionKind::Execute,
                InspectSecurityDecisionOutcome::Denied,
                vec![principal_id(PRINCIPAL_A)],
                Some(function_id(ROOT_FUNCTION)),
                Some(String::new()),
                vec![],
            ),
            Err(InspectError::InvalidSecurityDecisionReason)
        ));
    }

    #[test]
    fn security_decision_row_allowed_carries_no_denial_reason() {
        assert!(matches!(
            SecurityDecisionRow::new(
                InspectSecurityDecisionKind::Execute,
                InspectSecurityDecisionOutcome::Allowed,
                vec![principal_id(PRINCIPAL_A)],
                Some(function_id(ROOT_FUNCTION)),
                Some("execute:missing-grant".to_owned()),
                vec![],
            ),
            Err(InspectError::InvalidSecurityDecisionReason)
        ));
        let audit_ref = SecurityAuditEventId::from_bytes([0xcc; 16]);
        let row = SecurityDecisionRow::new(
            InspectSecurityDecisionKind::UserState,
            InspectSecurityDecisionOutcome::Allowed,
            vec![principal_id(PRINCIPAL_A)],
            None,
            None,
            vec![audit_ref],
        )
        .expect("an allowed decision carries no reason");
        assert_eq!(row.kind(), InspectSecurityDecisionKind::UserState);
        assert_eq!(row.outcome(), InspectSecurityDecisionOutcome::Allowed);
        assert_eq!(row.principals(), &[principal_id(PRINCIPAL_A)]);
        assert_eq!(row.target(), None);
        assert_eq!(row.denial_reason(), None);
        assert_eq!(row.audit_refs(), &[audit_ref]);
    }

    #[test]
    fn trace_event_derives_its_closed_kind_from_the_payload() {
        assert_eq!(
            trace_event(0, InspectTracePayload::Started).kind(),
            InspectTraceEventKind::InvocationStarted
        );
        assert_eq!(
            trace_event(
                1,
                InspectTracePayload::ValueBatch {
                    schema: None,
                    values: vec![value(7)],
                },
            )
            .kind(),
            InspectTraceEventKind::ValueBatch
        );
        assert_eq!(
            trace_event(
                2,
                InspectTracePayload::Completed {
                    duration_nanoseconds: 42
                }
            )
            .kind(),
            InspectTraceEventKind::InvocationCompleted
        );
        assert_eq!(
            trace_event(
                3,
                InspectTracePayload::Failed {
                    code: "ORNA0501".to_owned()
                }
            )
            .kind(),
            InspectTraceEventKind::InvocationFailed
        );
        assert_eq!(
            trace_event(4, InspectTracePayload::Cancelled { reason: None }).kind(),
            InspectTraceEventKind::InvocationCancelled
        );
    }

    #[test]
    fn trace_event_rejects_an_empty_value_batch() {
        let error = InspectTraceEvent::new(
            invocation_id(INVOCATION),
            0,
            InspectTracePayload::ValueBatch {
                schema: None,
                values: vec![],
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect_err("a value batch must be non-empty");
        assert_eq!(error, InspectError::EmptyValueBatch);
    }

    #[test]
    fn trace_event_rejects_an_empty_failure_code() {
        let error = InspectTraceEvent::new(
            invocation_id(INVOCATION),
            0,
            InspectTracePayload::Failed {
                code: String::new(),
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect_err("a failure carries a non-empty code");
        assert_eq!(error, InspectError::EmptyFailureCode);
    }

    #[test]
    fn trace_event_rejects_empty_cancellation_reasons_and_purposes() {
        let error = InspectTraceEvent::new(
            invocation_id(INVOCATION),
            0,
            InspectTracePayload::Cancelled {
                reason: Some(String::new()),
            },
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect_err("an empty cancellation reason is rejected");
        assert_eq!(error, InspectError::EmptyCancellationReason);
        let error = InspectTraceEvent::new(
            invocation_id(INVOCATION),
            0,
            InspectTracePayload::Started,
            SystemTime::UNIX_EPOCH,
            None,
            Some(String::new()),
        )
        .expect_err("an empty purpose is rejected");
        assert_eq!(error, InspectError::EmptyPurpose);
    }

    #[test]
    fn trace_event_accessors_expose_the_closed_facts() {
        let event = InspectTraceEvent::new(
            invocation_id(INVOCATION),
            0,
            InspectTracePayload::Started,
            SystemTime::UNIX_EPOCH,
            Some(invocation_id(PARENT_INVOCATION)),
            Some("inspect".to_owned()),
        )
        .expect("a valid test event");
        assert_eq!(event.invocation_id(), invocation_id(INVOCATION));
        assert_eq!(event.sequence(), 0);
        assert_eq!(event.payload(), &InspectTracePayload::Started);
        assert_eq!(event.recorded_at(), SystemTime::UNIX_EPOCH);
        assert_eq!(
            event.observer_invocation(),
            Some(invocation_id(PARENT_INVOCATION))
        );
        assert_eq!(event.purpose(), Some("inspect"));
    }

    #[test]
    fn trace_stream_starts_at_zero_and_appends_contiguously() {
        let mut trace = InspectTrace::new(invocation_id(INVOCATION));
        assert_eq!(trace.next_sequence(), 0);
        trace
            .push(trace_event(0, InspectTracePayload::Started))
            .expect("first event at sequence 0");
        trace
            .push(trace_event(
                1,
                InspectTracePayload::ValueBatch {
                    schema: None,
                    values: vec![value(7)],
                },
            ))
            .expect("second event at sequence 1");
        trace
            .push(trace_event(
                2,
                InspectTracePayload::Completed {
                    duration_nanoseconds: 42,
                },
            ))
            .expect("third event at sequence 2");
        assert_eq!(trace.next_sequence(), 3);
        assert_eq!(trace.invocation_id(), invocation_id(INVOCATION));
        assert_eq!(trace.events().len(), 3);
        for (index, event) in trace.events().iter().enumerate() {
            assert_eq!(event.sequence(), index as u64);
        }
    }

    #[test]
    fn trace_stream_rejects_a_non_contiguous_sequence() {
        let mut trace = InspectTrace::new(invocation_id(INVOCATION));
        trace
            .push(trace_event(0, InspectTracePayload::Started))
            .expect("first event");
        let error = trace
            .push(trace_event(
                2,
                InspectTracePayload::Completed {
                    duration_nanoseconds: 1,
                },
            ))
            .expect_err("a gap in the sequence fails closed");
        assert_eq!(
            error,
            InspectError::NonContiguousTraceSequence {
                invocation_id: invocation_id(INVOCATION),
                expected: 1,
                actual: 2,
            }
        );
    }

    #[test]
    fn trace_stream_rejects_an_event_of_another_invocation() {
        let mut trace = InspectTrace::new(invocation_id(INVOCATION));
        let foreign = InspectTraceEvent::new(
            invocation_id(PARENT_INVOCATION),
            0,
            InspectTracePayload::Started,
            SystemTime::UNIX_EPOCH,
            None,
            None,
        )
        .expect("a valid foreign event");
        let error = trace
            .push(foreign)
            .expect_err("an event of another invocation is rejected");
        assert_eq!(
            error,
            InspectError::TraceInvocationMismatch {
                invocation_id: invocation_id(INVOCATION),
                event_invocation_id: invocation_id(PARENT_INVOCATION),
            }
        );
    }

    #[test]
    fn trace_stream_after_sequence_filters_in_contiguous_order() {
        let mut trace = InspectTrace::new(invocation_id(INVOCATION));
        for sequence in 0..5 {
            trace
                .push(trace_event(sequence, InspectTracePayload::Started))
                .expect("contiguous event");
        }
        let after: Vec<u64> = trace
            .after_sequence(2)
            .map(|event| event.sequence())
            .collect();
        assert_eq!(after, vec![3, 4]);
        assert!(trace.after_sequence(4).next().is_none());
        assert_eq!(trace.after_sequence(0).count(), 4);
    }

    #[test]
    fn every_error_variant_displays() {
        let errors = [
            InspectError::EmptyEpoch {
                id: epoch_id(EPOCH),
            },
            InspectError::EmptyValueBatch,
            InspectError::InvalidInvocationNodeParent {
                id: invocation_id(INVOCATION),
            },
            InspectError::EmptyUiNodeLabel { what: "call site" },
            InspectError::EmptyCandidateLabel { what: "presenter" },
            InspectError::EmptyRuntimeLabel { what: "name" },
            InspectError::EmptyRuntimeContract,
            InspectError::InvalidSecurityDecisionReason,
            InspectError::EmptyFailureCode,
            InspectError::EmptyCancellationReason,
            InspectError::EmptyPurpose,
            InspectError::TraceInvocationMismatch {
                invocation_id: invocation_id(INVOCATION),
                event_invocation_id: invocation_id(PARENT_INVOCATION),
            },
            InspectError::NonContiguousTraceSequence {
                invocation_id: invocation_id(INVOCATION),
                expected: 1,
                actual: 2,
            },
        ];
        for error in errors {
            let message = error.to_string();
            assert!(!message.is_empty(), "every variant must display");
            let _: &dyn std::error::Error = &error;
        }
    }

    #[test]
    fn privileges_and_classifiers_are_comparable_and_distinct() {
        let ladder = [
            InspectPrivilege::OwnInvocation,
            InspectPrivilege::SessionInvocations,
            InspectPrivilege::AnyInvocation,
        ];
        let classifiers = [
            InspectPrivilege::Values,
            InspectPrivilege::Source,
            InspectPrivilege::SecurityDetails,
            InspectPrivilege::RuntimeInternals,
        ];
        let mut all: Vec<InspectPrivilege> = ladder.to_vec();
        for privilege in classifiers {
            all.push(privilege);
        }
        let mut seen = Vec::new();
        for privilege in all {
            assert!(!seen.contains(&privilege), "every privilege is distinct");
            seen.push(privilege);
            assert_eq!(
                privilege.is_invocation_scope(),
                privilege.ladder_rank().is_some()
            );
            assert_eq!(privilege.is_classifier(), privilege.classifier().is_some());
        }
    }
}
