use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::SystemTime,
};

const RESOURCE_CANCELLATION_RUNNING: u8 = 0;
const RESOURCE_CANCELLATION_REQUESTED: u8 = 1;
const RESOURCE_CANCELLATION_COMMIT_STARTED: u8 = 2;
const RESOURCE_CANCELLATION_COMMITTED: u8 = 3;
const RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED: u8 = 4;
const RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED: u8 = 5;
const MAX_RESOURCE_CREDIT: u64 = 1024 * 1024 * 1024;

/// Coordinates cancellation with resource acceptance and terminal commits.
///
/// Each commit-start transition is a cancellation linearisation point:
/// cancellation wins only while the request is still running. Once either
/// acceptance or terminal commit starts, that commit wins even if a later
/// cancellation arrives.
#[derive(Clone, Debug)]
pub struct ResourceCancellation {
    state: Arc<Mutex<u8>>,
    notify: Arc<tokio::sync::Notify>,
}

impl ResourceCancellation {
    /// Creates a cancellation state for one resource request.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RESOURCE_CANCELLATION_RUNNING)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Requests cancellation and returns whether this request won the race.
    pub fn request_cancel(&self) -> bool {
        let won = {
            let mut state = self
                .state
                .lock()
                .expect("resource cancellation state is not poisoned");
            match *state {
                RESOURCE_CANCELLATION_RUNNING => {
                    *state = RESOURCE_CANCELLATION_REQUESTED;
                    true
                }
                RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED => {
                    *state = RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED;
                    false
                }
                _ => false,
            }
        };
        if won {
            self.notify.notify_waiters();
        }
        won
    }

    /// Returns whether cancellation has won the terminal commit race.
    pub fn is_requested(&self) -> bool {
        matches!(
            *self
                .state
                .lock()
                .expect("resource cancellation state is not poisoned"),
            RESOURCE_CANCELLATION_REQUESTED | RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED
        )
    }

    /// Returns whether cancellation arrived during acceptance commit.
    #[doc(hidden)]
    pub fn is_acceptance_cancellation_requested(&self) -> bool {
        *self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned")
            == RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED
    }

    /// Waits until cancellation wins the terminal commit race.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_requested() {
                return;
            }
            notified.await;
        }
    }

    /// Starts the durable acceptance commit if cancellation has not won.
    #[doc(hidden)]
    pub fn try_begin_acceptance_commit(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        if *state != RESOURCE_CANCELLATION_RUNNING {
            return false;
        }
        *state = RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED;
        true
    }

    /// Reopens terminal cancellation after durable acceptance has committed.
    #[doc(hidden)]
    pub fn acceptance_commit_finished(&self) {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        match *state {
            RESOURCE_CANCELLATION_ACCEPTANCE_COMMIT_STARTED => {
                *state = RESOURCE_CANCELLATION_RUNNING;
            }
            RESOURCE_CANCELLATION_ACCEPTANCE_CANCEL_REQUESTED => {
                *state = RESOURCE_CANCELLATION_REQUESTED;
            }
            _ => {}
        }
    }

    /// Starts the terminal commit if cancellation has not won.
    pub fn try_begin_commit(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        if *state != RESOURCE_CANCELLATION_RUNNING {
            return false;
        }
        *state = RESOURCE_CANCELLATION_COMMIT_STARTED;
        true
    }

    /// Records that the terminal transaction commit completed.
    pub fn commit_finished(&self) {
        let mut state = self
            .state
            .lock()
            .expect("resource cancellation state is not poisoned");
        if *state == RESOURCE_CANCELLATION_COMMIT_STARTED {
            *state = RESOURCE_CANCELLATION_COMMITTED;
        }
    }
}

use orna_artifact::client_plan::{CAPABILITY_FORMAT_VERSION, CapabilityClientPlan, ResourceKind};
use orna_client::{
    ClientExecutionError, ClientExecutionResult, ClientResourceCompletion,
    ClientResourceExecutionError, ClientResourceExecutor, ClientStateContext, ClientStateStore,
    evaluate_client_function_in_state_context_with_grants_and_arguments as evaluate_authorised_client_function_with_state_context_and_arguments,
    evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation as evaluate_authorised_client_function_with_state_context_and_arguments_and_executor,
    evaluate_client_function_with_grants as evaluate_authorised_client_function,
    evaluate_client_function_with_grants_and_arguments as evaluate_authorised_client_function_with_arguments,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, InspectEpochId, InvocationAuditEventId,
    InvocationId, ObjectId, PrincipalId, SecurityAuditEventId, SourceRevisionId,
    StandardLibraryRevisionId,
    catalogue::{FunctionDefinition, FunctionDomain, FunctionReturn},
    inspect::{InspectOutcomeKind, InspectPrivilege, InspectSnapshotOptions},
    invocation::{
        InvocationArgument, InvocationClientOffer, InvocationEventBody, InvocationFailure,
        InvocationFailurePhase, InvocationParameterSelector, InvocationRetryability,
        InvocationTarget as InvocationRequestTarget, InvokeEvent, InvokeValue,
        ProtectedInvocationDecision, decide_protected_invocation,
    },
    revision::{ActiveDatabaseRevision, CatalogueHashContext, RevisionPair, StandardExecutable},
    security::{
        AuthenticatedSession, AuthorisedInvocation, CATALOGUE_HEALTH_FUNCTION_ID,
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision, ExecuteDenial, ExecuteGrant,
        InspectDenial, InspectEpochScope, InvocationTarget, LocalPeerAuthenticationError,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, PrivilegeClass,
        PrivilegeDenial, PrivilegeGrant, RoleMembership, SecurityAdminAuditOperation,
        SecurityAuditDecision, SecurityAuditDenial, SecurityAuditEvent, SecurityAuditKind,
        SecurityAuditOutcome, SecurityFunctionTarget, SecuritySnapshot, SessionBindingError,
        TargetClass, UserStateAuditOperation,
    },
    state::UserStateCell,
    system::{
        SYS_INVOKE_FUNCTION_ID, SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_PRINCIPAL_TYPE_ID,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID, SystemFunctionDefinition, SystemFunctionKind,
        system_function_by_id, system_function_by_name,
    },
    types::TypeDescriptor,
    value::{FunctionArgument, OpaqueCodecRegistry, RecordValue, RuntimeType, RuntimeValue},
};
use orna_protocol::{
    CallFailure, InvocationEventBatch, InvocationEventRecord, ResourceArgument,
    ResourceKind as ProtocolResourceKind, ResourceRequest, RetainedInvokeRequest,
    decode_retained_invoke_request, encode_active_value,
};
use orna_standard::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_JSON_ENCODE_FUNCTION_ID, registered_opaque_codecs,
};
use tokio_postgres::{IsolationLevel, Row, Transaction, types::FromSqlOwned};

use super::PostgresSession;

use crate::{
    PostgresKernel, PostgresKernelError, RawServerTargetError,
    bootstrap::require_current_migrations,
    physical::establish_trusted_search_path,
    recovery::{load_verified_standard_library, recover_active_revision},
    server_execution::{
        SealedPresentationError, ServerSelectError, ServerSelectResult,
        execute_authorised_raw_server_select, execute_authorised_server_select,
        execute_standard_json_encode, execute_standard_parameter_echo,
        present_sealed_standard_output, raw_identity_selected_server_select_target_is_selected,
        raw_server_target_is_unavailable,
        raw_unique_text_selected_server_select_target_is_selected,
        run_authenticated_server_resource_stream, run_authenticated_standard_resource_stream,
    },
    server_mutation_execution::{
        RawServerReferenceMutation, ServerInsertError, execute_authorised_raw_server_insert,
        execute_authorised_raw_server_insert_with_arguments,
        execute_authorised_raw_server_reference_mutation, raw_server_delete_target_is_unavailable,
        raw_server_insert_target_is_selected, raw_server_insert_target_is_unavailable,
        raw_server_reference_mutation_target, raw_server_reference_value_update_target_is_selected,
        raw_server_update_target_is_unavailable,
    },
    server_runtime::{configure_and_recover, runtime_types_match},
    state::load_user_state_in_transaction,
};

/// The owned value result of one authenticated raw call.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedRawCallResult {
    /// One value evaluated by the CLIENT runtime.
    Client(RuntimeValue),
    /// Zero or more values returned in SERVER execution result order.
    Server(Vec<RuntimeValue>),
}

/// The owned result of one authenticated SERVER resource request.
///
/// A successful result contains the server-generated nested invocation identity
/// and only values validated against the active SERVER target. A failed result
/// carries the closed protocol failure class and no target, principal, grant,
/// argument, or internal error detail.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedServerResourceResult {
    /// The target executed and produced its complete value sequence.
    Completed {
        /// The connection-local resource stream.
        stream_id: u64,
        /// The caller's request correlation identity.
        request_id: InvocationId,
        /// The server-generated nested invocation identity.
        nested_invocation_id: InvocationId,
        /// The active revision pair used for execution.
        target_revision: RevisionPair,
        /// The validated resource result kind.
        resource_kind: ProtocolResourceKind,
        /// Values in server result order. A scalar has exactly one value.
        values: Vec<RuntimeValue>,
    },
    /// The request was denied or could not safely execute.
    Failed {
        /// The connection-local resource stream.
        stream_id: u64,
        /// The caller's request correlation identity.
        request_id: InvocationId,
        /// The closed public failure class.
        failure: CallFailure,
    },
}

/// The local resource kind retained by an authenticated producer.
///
/// This deliberately does not expose the wire protocol's resource enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedServerResourceKind {
    /// One scalar result item.
    Single,
    /// A bounded sequence of result items.
    Stream,
}

/// The metadata established before a resource producer is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedServerResourceAccepted {
    pub stream_id: u64,
    pub request_id: InvocationId,
    pub nested_invocation_id: InvocationId,
    pub target_revision: RevisionPair,
    pub resource_kind: AuthenticatedServerResourceKind,
}

/// Checked item and byte credit for one producer pull.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceCredit {
    pub item_count: u64,
    pub byte_count: u64,
}

impl ResourceCredit {
    /// Creates non-zero bounded credit.
    pub fn new(item_count: u64, byte_count: u64) -> Option<Self> {
        (item_count != 0
            && byte_count != 0
            && item_count <= MAX_RESOURCE_CREDIT
            && byte_count <= MAX_RESOURCE_CREDIT)
            .then_some(Self {
                item_count,
                byte_count,
            })
    }
}

/// The producer result of one pull command.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedServerResourceEvent {
    /// One bounded batch of decoded values.
    Values {
        batch_sequence: u64,
        item_count: u64,
        byte_count: u64,
        values: Vec<RuntimeValue>,
    },
    /// The transaction committed after all rows were consumed.
    Completed {
        final_batch_sequence: u64,
        total_items: u64,
        total_bytes: u64,
    },
    /// A redacted execution failure.
    Failed { failure: CallFailure },
    /// The transaction rolled back after cancellation won.
    Cancelled,
    /// The pulled row requires more byte credit and remains pending.
    Waiting { required_bytes: u64 },
}

/// The result of starting an authenticated SERVER resource producer.
#[derive(Debug)]
pub enum AuthenticatedServerResourceStart {
    /// Security and plan validation succeeded; the producer is live.
    Accepted(AuthenticatedServerResourceProducer),
    /// The request failed before acceptance and carries only its redacted class.
    Failed {
        stream_id: u64,
        request_id: InvocationId,
        failure: CallFailure,
    },
}

/// A command-driven producer whose transaction and PostgreSQL row stream are
/// owned by its task.
///
/// Dropping an abandoned producer requests cancellation. The worker remains
/// responsible for terminal audit and transaction ordering, including when the
/// caller drops the producer before it receives a terminal event.
pub struct AuthenticatedServerResourceProducer {
    accepted: AuthenticatedServerResourceAccepted,
    commands: tokio::sync::mpsc::Sender<ResourceProducerCommand>,
    cancellation: ResourceCancellation,
}

impl AuthenticatedServerResourceProducer {
    /// Returns the immutable acceptance metadata.
    pub fn accepted(&self) -> AuthenticatedServerResourceAccepted {
        self.accepted
    }

    /// Requests one bounded batch or terminal result.
    pub async fn pull(
        &self,
        credit: ResourceCredit,
    ) -> Result<AuthenticatedServerResourceEvent, PostgresKernelError> {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.commands
            .send(ResourceProducerCommand::Pull(ResourceProducerPull {
                credit,
                response,
            }))
            .await
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: self.accepted.request_id.canonical(),
                rule: "producer task terminated before pull response",
            })?;
        receiver
            .await
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: self.accepted.request_id.canonical(),
                rule: "producer task dropped pull response",
            })?
    }

    /// Requests cancellation and reports whether this call won the race.
    pub fn cancel(&self) -> bool {
        self.cancellation.request_cancel()
    }
}

impl Drop for AuthenticatedServerResourceProducer {
    fn drop(&mut self) {
        self.cancellation.request_cancel();
    }
}

impl std::fmt::Debug for AuthenticatedServerResourceProducer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedServerResourceProducer")
            .field("accepted", &self.accepted)
            .finish_non_exhaustive()
    }
}

enum ResourceProducerReady {
    Accepted(AuthenticatedServerResourceAccepted),
    Failed {
        stream_id: u64,
        request_id: InvocationId,
        failure: CallFailure,
    },
}

/// Requests cancellation if startup is dropped before the worker publishes
/// acceptance or a pre-acceptance failure. The worker must not be aborted here:
/// it owns the reserved request finalizer.
struct ResourceProducerStartGuard {
    cancellation: ResourceCancellation,
    armed: bool,
}

impl ResourceProducerStartGuard {
    fn new(cancellation: ResourceCancellation) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ResourceProducerStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.request_cancel();
        }
    }
}

/// Internal command sent to the task which owns the transaction.
#[derive(Debug)]
pub(crate) enum ResourceProducerCommand {
    Pull(ResourceProducerPull),
}

#[derive(Debug)]
pub(crate) struct ResourceProducerPull {
    pub(crate) credit: ResourceCredit,
    pub(crate) response:
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
}

/// The task exit used to finalize audit and transaction state.
pub(crate) enum ResourceProducerExit {
    Completed(ResourceProducerCompleted),
    Cancelled(ResourceProducerCancelled),
    Failed(ResourceProducerFailed),
}

pub(crate) struct ResourceProducerCompleted {
    pub(crate) response:
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    pub(crate) final_batch_sequence: u64,
    pub(crate) total_items: u64,
    pub(crate) total_bytes: u64,
}

pub(crate) struct ResourceProducerCancelled {
    pub(crate) response: Option<
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    >,
}

pub(crate) struct ResourceProducerFailed {
    pub(crate) response: Option<
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    >,
    pub(crate) error: PostgresKernelError,
}

#[derive(Default)]
struct ResourceProducerLifecycle {
    invocation: Option<InvocationId>,
    target: Option<InvocationTarget>,
    acceptance_committed: bool,
    failure: Option<CallFailure>,
    cancelled: bool,
    terminal_commit_started: bool,
    acceptance_commit_attempted: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceProducerFailureStage {
    None,
    PreAcceptance,
    PostAcceptance,
    PostAcceptanceAudit,
    PostAcceptanceAuditCancellation,
    PostAcceptanceCancelledExitAudit,
}

/// The owned redacted result of one sealed `sys.invoke` dispatch.
///
/// The completed variant carries the full Event batch so a server adapter can
/// deliver `InvocationStarted(0)`, `ValueBatch(1)`, and `InvocationCompleted(2)`
/// and then complete the call (`CALL_COMPLETED`). The other variants are
/// closed and disclose no target, signature, selector, value, binding, or
/// security evidence.
#[derive(Clone, Debug, PartialEq)]
pub enum SealedInvocationResult {
    /// The invocation completed with its complete Event sequence.
    Completed {
        /// The invocation identity shared by every retained Event.
        invocation: InvocationId,
        /// The complete `InvocationStarted(0)`, `ValueBatch(1)`,
        /// `InvocationCompleted(2)` Event batch.
        events: InvocationEventBatch,
    },
    /// The accepted invocation ended with one redacted failure Event sequence.
    Failed {
        /// The invocation identity shared by every retained Event.
        invocation: InvocationId,
        /// The complete `InvocationStarted(0)`, `InvocationFailed(1)` batch.
        events: InvocationEventBatch,
    },
    /// The invocation was denied without executing any artifact.
    Denied {
        /// The invocation identity.
        invocation: InvocationId,
    },
    /// The allowed invocation executed but its output requirement could not
    /// be presented (ADR 0057 step 7).
    ///
    /// This variant is closed: it discloses no target, requirement, value,
    /// presenter, or failure detail. The CLI maps it to the presentation
    /// error exit code 5.
    PresentationFailed {
        /// The invocation identity.
        invocation: InvocationId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SealedInvocationFailureClass {
    Bind,
    Target,
    Internal,
}

/// One closed durable `sys.invoke` decision for the PostgreSQL kernel.
///
/// This is private kernel state. It does not retain Request, bind, lifecycle,
/// delivery, or error-detail data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvocationAuditDecision {
    invocation: InvocationId,
    outcome: SecurityAuditOutcome,
    session_principal: PrincipalId,
    effective_principal: Option<PrincipalId>,
    authorising_principal: Option<PrincipalId>,
    target: Option<InvocationTarget>,
    security_audit_event: Option<SecurityAuditEventId>,
}

impl InvocationAuditDecision {
    /// Creates one decision from durable matching `EXECUTE` evidence.
    pub(crate) fn from_execute_evidence(
        invocation: InvocationId,
        evidence: &SecurityAuditEvent,
    ) -> Result<Self, PostgresKernelError> {
        let decision = evidence.decision();
        if decision.kind() != SecurityAuditKind::Execute {
            return Err(invocation_audit_invariant(
                &invocation.canonical(),
                "invocation decision requires EXECUTE audit evidence",
            ));
        }
        let target = require_invocation_audit_value(
            decision.target(),
            &invocation.canonical(),
            "EXECUTE evidence requires a target",
        )?;
        let session_principal = require_invocation_audit_value(
            decision.session_principal(),
            &invocation.canonical(),
            "EXECUTE evidence requires a session principal",
        )?;
        let result = Self {
            invocation,
            outcome: decision.outcome(),
            session_principal,
            effective_principal: decision.effective_principal(),
            authorising_principal: decision.authorising_principal(),
            target: Some(target),
            security_audit_event: Some(evidence.id()),
        };
        validate_invocation_audit_decision_shape(&result, &invocation.canonical())?;
        Ok(result)
    }

    /// Creates the closed unresolved target-denied decision.
    pub(crate) fn unresolved_denied(
        invocation: InvocationId,
        session_principal: PrincipalId,
    ) -> Self {
        Self {
            invocation,
            outcome: SecurityAuditOutcome::Denied,
            session_principal,
            effective_principal: None,
            authorising_principal: None,
            target: None,
            security_audit_event: None,
        }
    }
}

impl AuthenticatedRawCallResult {
    /// Transfers result values without cloning their payloads.
    pub fn into_values(self) -> Vec<RuntimeValue> {
        match self {
            Self::Client(value) => vec![value],
            Self::Server(values) => values,
        }
    }
}

/// The closed pre-accept result of one retained sealed invocation.
///
/// Entry denial and malformed/protocol-incompatible requests never receive an
/// invocation identity. An accepted continuation owns the pinned decode context
/// and can be handed to the post-accept preparation step exactly once.
#[doc(hidden)]
pub enum SealedInvocationPreflight {
    /// The request was rejected before acceptance.
    Rejected { failure: CallFailure },
    /// The request passed the protected entry and request checks.
    Accepted(SealedInvocationContinuation),
}

/// The private, one-shot continuation created after sealed preflight.
#[doc(hidden)]
pub struct SealedInvocationContinuation {
    kernel: PostgresKernel,
    authenticated_session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    registry: OpaqueCodecRegistry,
    decoded: orna_core::invocation::InvokeRequest,
    request: RetainedInvokeRequest,
    invocation: InvocationId,
    started_events: InvocationEventBatch,
}

impl SealedInvocationContinuation {
    /// Returns the identity that will be shared by every invocation event.
    pub const fn invocation(&self) -> InvocationId {
        self.invocation
    }

    /// Returns the start-only Event batch queued before target work.
    pub fn started_events(&self) -> &InvocationEventBatch {
        &self.started_events
    }
}

impl std::fmt::Debug for SealedInvocationContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedInvocationContinuation")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SealedInvocationPreflight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected { failure } => formatter
                .debug_struct("SealedInvocationPreflight::Rejected")
                .field("failure", failure)
                .finish(),
            Self::Accepted(continuation) => formatter
                .debug_tuple("SealedInvocationPreflight::Accepted")
                .field(continuation)
                .finish(),
        }
    }
}

/// The closed result of one accepted continuation after its start Event.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub enum SealedInvocationExecution {
    /// A normal sealed invocation result (including redacted failures).
    Result(SealedInvocationResult),
    /// Cancellation won before new evaluator/target work began.
    Cancelled { invocation: InvocationId },
}

/// The post-accept operation owns all pinned state needed for one execution.
#[doc(hidden)]
pub struct SealedInvocationOperation {
    kernel: PostgresKernel,
    authenticated_session: AuthenticatedSession,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    registry: OpaqueCodecRegistry,
    decoded: orna_core::invocation::InvokeRequest,
    request: RetainedInvokeRequest,
    invocation: InvocationId,
    started_events: InvocationEventBatch,
    outcome: SealedInvocationPreparedOutcome,
    consumed: bool,
}

impl SealedInvocationOperation {
    /// Returns the invocation identity.
    pub const fn invocation(&self) -> InvocationId {
        self.invocation
    }

    /// Returns the start-only Event batch. No target or result is included.
    pub fn started_events(&self) -> &InvocationEventBatch {
        &self.started_events
    }
    /// Returns the immutable active revision pinned during preflight.
    #[doc(hidden)]
    pub fn active_revision(&self) -> ActiveDatabaseRevision {
        self.active.clone()
    }
}

impl std::fmt::Debug for SealedInvocationOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedInvocationOperation")
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

enum SealedInvocationPreparedOutcome {
    TargetDenied {
        security_target: Option<InvocationTarget>,
        denial: Option<ExecuteDenial>,
    },
    BindFailure {
        target: PreparedSealedTarget,
        security_target: InvocationTarget,
        authorisation: AuthorisedInvocation,
    },
    Allowed {
        target: PreparedSealedTarget,
        security_target: InvocationTarget,
        authorisation: AuthorisedInvocation,
    },
}

#[derive(Clone)]
enum PreparedSealedTarget {
    Application {
        definition: FunctionDefinition,
    },
    System {
        definition: SystemFunctionDefinition,
    },
    VerifiedStandard {
        definition: FunctionDefinition,
        executable: StandardExecutable,
    },
}

impl PreparedSealedTarget {
    fn function(&self) -> FunctionId {
        match self {
            Self::Application { definition } | Self::VerifiedStandard { definition, .. } => {
                definition.id()
            }
            Self::System { definition } => definition.id(),
        }
    }

    fn from_resolved(target: SealedResolvedTarget<'_>) -> Self {
        match target {
            SealedResolvedTarget::Application(definition) => Self::Application {
                definition: definition.clone(),
            },
            SealedResolvedTarget::System(definition) => Self::System { definition },
            SealedResolvedTarget::VerifiedStandard {
                definition,
                executable,
            } => Self::VerifiedStandard {
                definition: definition.clone(),
                executable: executable.clone(),
            },
        }
    }
}

fn sealed_started_events(
    invocation: InvocationId,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    InvocationEventBatch::new(vec![InvocationEventRecord::new(1, started)])
        .map_err(PostgresKernelError::SealedInvocation)
}

impl PostgresKernel {
    /// Validates the protected entry and retained request before acceptance.
    ///
    /// This method does not resolve the requested target. An entry denial
    /// records only closed audit evidence before returning the public rejection;
    /// an accepted continuation carries the pinned decode context.
    #[doc(hidden)]
    pub async fn validate_sealed_sys_invoke(
        &self,
        authenticated_session: &AuthenticatedSession,
        connection_protocol_major: u16,
        request: &RetainedInvokeRequest,
    ) -> Result<SealedInvocationPreflight, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let system_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
            // ADR 0054 requires the protected entry decision to precede any
            // retained Request decoding. An unauthorized caller therefore gets
            // only the closed EXECUTE_DENIED result, even for malformed bytes.
            match security.authorise_system_function(authenticated_session, system_target) {
                ExecuteDecision::Denied(reason) => {
                    let invocation = InvocationId::new();
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            system_target,
                            reason,
                        ),
                    )
                    .await?;
                    append_unresolved_invocation_audit(
                        &transaction,
                        authenticated_session,
                        invocation,
                    )
                    .await?;
                    transaction
                        .commit()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    return Ok(SealedInvocationPreflight::Rejected {
                        failure: CallFailure::ExecuteDenied,
                    });
                }
                ExecuteDecision::Allowed(_) => {}
            }
            let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: active.pair().catalogue().canonical(),
                    rule: "sealed sys.invoke requires the accepted verified standard snapshot",
                }
            })?;
            let registry = registered_opaque_codecs(standard).map_err(|_| {
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.standard_library_revisions",
                    record: standard.revision().canonical(),
                    rule: "the verified standard snapshot must bind its opaque codec registry",
                }
            })?;
            let decoded = match decode_retained_invoke_request(&active, &registry, request) {
                Ok(decoded) => decoded,
                Err(_) => {
                    transaction
                        .rollback()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    return Ok(SealedInvocationPreflight::Rejected {
                        failure: CallFailure::InternalFailure,
                    });
                }
            };
            if decoded.client_offer().protocol_major() != connection_protocol_major {
                transaction
                    .rollback()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                return Ok(SealedInvocationPreflight::Rejected {
                    failure: CallFailure::InternalFailure,
                });
            }
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            let invocation = InvocationId::new();
            let started_events = sealed_started_events(invocation)?;
            Ok(SealedInvocationPreflight::Accepted(
                SealedInvocationContinuation {
                    kernel: self.clone(),
                    authenticated_session: authenticated_session.clone(),
                    active,
                    security,
                    registry,
                    decoded,
                    request: request.clone(),
                    invocation,
                    started_events,
                },
            ))
        }
        .await;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
    }
}

impl SealedInvocationContinuation {
    /// Prepares the accepted invocation after the caller has sent
    /// `CALL_ACCEPTED` and before target execution starts.
    ///
    /// Target resolution, binding, and the protected decision remain private in
    /// this outcome. The operation commits their durable audit before dispatch
    /// evaluates defaults or the target.
    #[doc(hidden)]
    pub async fn prepare_sealed_sys_invoke_after_accept(
        self,
    ) -> Result<SealedInvocationOperation, PostgresKernelError> {
        let SealedInvocationContinuation {
            kernel,
            authenticated_session,
            active,
            security,
            registry,
            decoded,
            request,
            invocation,
            started_events,
        } = self;
        let outcome = match resolve_sealed_target(&active, decoded.target()) {
            Some(target) => {
                let security_target = sealed_security_target(&active, target);
                match authorise_sealed_target(&security, &authenticated_session, security_target) {
                    ExecuteDecision::Allowed(authorisation) => {
                        let bind_ok = match &target {
                            SealedResolvedTarget::Application(definition)
                            | SealedResolvedTarget::VerifiedStandard { definition, .. } => {
                                bind_sealed_invoke_arguments(definition, decoded.arguments())
                                    .is_ok()
                            }
                            SealedResolvedTarget::System(_) => true,
                        };
                        let prepared_target = PreparedSealedTarget::from_resolved(target);
                        if bind_ok {
                            SealedInvocationPreparedOutcome::Allowed {
                                target: prepared_target,
                                security_target,
                                authorisation,
                            }
                        } else {
                            SealedInvocationPreparedOutcome::BindFailure {
                                target: prepared_target,
                                security_target,
                                authorisation,
                            }
                        }
                    }
                    ExecuteDecision::Denied(denial) => {
                        SealedInvocationPreparedOutcome::TargetDenied {
                            security_target: Some(security_target),
                            denial: Some(denial),
                        }
                    }
                }
            }
            None => SealedInvocationPreparedOutcome::TargetDenied {
                security_target: None,
                denial: None,
            },
        };
        Ok(SealedInvocationOperation {
            kernel,
            authenticated_session,
            active,
            security,
            registry,
            decoded,
            request,
            invocation,
            started_events,
            outcome,
            consumed: false,
        })
    }
}

impl SealedInvocationOperation {
    async fn append_prepared_audit(&self) -> Result<(), PostgresKernelError> {
        let mut database_session = self.kernel.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            if active.pair() != self.active.pair() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: self.invocation.canonical(),
                    rule: "sealed invocation active revision changed before audit",
                });
            }
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&security, &self.security) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    record: self.invocation.canonical(),
                    rule: "sealed invocation security snapshot changed before audit",
                });
            }
            match &self.outcome {
                SealedInvocationPreparedOutcome::Allowed {
                    target,
                    security_target,
                    authorisation,
                }
                | SealedInvocationPreparedOutcome::BindFailure {
                    target,
                    security_target,
                    authorisation,
                } => {
                    if target.function() != security_target.function()
                        || authorisation.target() != *security_target
                        || authorisation.target().revision() != self.active.pair()
                    {
                        return Err(PostgresKernelError::DurableInvariant {
                            relation: "_orna_kernel.invocation_audit_events",
                            record: self.invocation.canonical(),
                            rule: "prepared invocation authorisation must retain the pinned revision",
                        });
                    }
                    append_allowed_invocation_audit_evidence(
                        &transaction,
                        authorisation,
                        self.invocation,
                    )
                    .await?;
                }
                SealedInvocationPreparedOutcome::TargetDenied {
                    security_target: Some(target),
                    denial: Some(reason),
                } => {
                    let event_id = append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            &self.authenticated_session,
                            *target,
                            *reason,
                        ),
                    )
                    .await?;
                    append_linked_invocation_audit(&transaction, self.invocation, event_id).await?;
                }
                SealedInvocationPreparedOutcome::TargetDenied {
                    security_target: None,
                    denial: None,
                } => {
                    append_unresolved_invocation_audit(
                        &transaction,
                        &self.authenticated_session,
                        self.invocation,
                    )
                    .await?;
                }
                SealedInvocationPreparedOutcome::TargetDenied { .. } => {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.invocation_audit_events",
                        record: self.invocation.canonical(),
                        rule: "target denial must retain both target and denial or neither",
                    });
                }
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(())
        }
        .await;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
    }

    /// Executes the accepted invocation after its start Event is delivered.
    #[doc(hidden)]
    pub async fn execute_after_started(
        &mut self,
        resource_executor: Option<&mut dyn ClientResourceExecutor>,
        state: &mut ClientStateStore,
        capability_audit_appended: &mut bool,
        cancellation: &ResourceCancellation,
    ) -> Result<SealedInvocationExecution, PostgresKernelError> {
        if self.consumed {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "sealed invocation operation",
                record: self.invocation.canonical(),
                rule: "execute_after_started may only be called once",
            });
        }
        self.consumed = true;
        self.append_prepared_audit().await?;
        if cancellation.is_requested() {
            return Ok(SealedInvocationExecution::Cancelled {
                invocation: self.invocation,
            });
        }
        let bind_failure = matches!(
            &self.outcome,
            SealedInvocationPreparedOutcome::BindFailure { .. }
        );
        let target_denied = matches!(
            &self.outcome,
            SealedInvocationPreparedOutcome::TargetDenied { .. }
        );
        if bind_failure {
            return Ok(SealedInvocationExecution::Result(sealed_failure_result(
                self.invocation,
                SealedInvocationFailureClass::Bind,
            )?));
        }
        if target_denied {
            return Ok(SealedInvocationExecution::Result(
                SealedInvocationResult::Denied {
                    invocation: self.invocation,
                },
            ));
        }
        let result = self
            .kernel
            .dispatch_sealed_sys_invoke_with_resource_executor_and_state_internal(
                &self.authenticated_session,
                self.decoded.client_offer().protocol_major(),
                &self.request,
                resource_executor,
                state,
                self.invocation,
                capability_audit_appended,
                Some(&self.decoded),
                Some((&self.active, &self.security)),
                Some(&self.registry),
                Some(&self.outcome),
                true,
                Some(cancellation),
            )
            .await;
        match result {
            Ok(result) => Ok(SealedInvocationExecution::Result(result)),
            Err(PostgresKernelError::ClientExecution(
                ClientExecutionError::ResourceEvaluation {
                    source: ClientResourceExecutionError::Cancelled,
                    ..
                },
            )) => Ok(SealedInvocationExecution::Cancelled {
                invocation: self.invocation,
            }),
            Err(error) => Err(error),
        }
    }
}

impl PostgresKernel {
    /// Dispatches one authenticated parameter-free raw call inside one transaction.
    ///
    /// The kernel authorises the exact active target before it selects the
    /// function domain. An allowed CLIENT target evaluates through the current
    /// CLIENT evaluator. An allowed SERVER target must satisfy either the
    /// closed one-column raw SELECT boundary or the parameter-free raw INSERT
    /// boundary before it can return values.
    pub async fn dispatch_authenticated_raw_call(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        self.dispatch_authenticated_raw_call_with_arguments(authenticated_session, function, &[])
            .await
    }

    /// Dispatches one authenticated raw call with zero arguments, one
    /// supported scalar or Reference argument, or one bounded pair of those
    /// values.
    ///
    /// Other argument shapes fail before PostgreSQL is opened. An admitted
    /// shape is authorised and audited before the active target or parameter
    /// declaration is inspected.
    pub async fn dispatch_authenticated_raw_call_with_arguments(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        validate_raw_call_argument_shape(function, arguments)?;
        self.dispatch_authenticated_raw_call_with_options(
            authenticated_session,
            function,
            arguments,
            None,
        )
        .await
    }

    /// Pauses raw dispatch after one active and security snapshot is recovered.
    ///
    /// The hook exposes one deterministic point to the integration harness. It
    /// is absent from production builds and does not alter transaction state.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn dispatch_authenticated_raw_call_with_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        self.dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
            authenticated_session,
            function,
            &[],
            reached,
            resume,
        )
        .await
    }

    /// Pauses raw dispatch with arguments after active recovery.
    ///
    /// The hook lets the integration harness alter only its disposable test
    /// database after recovery has verified the durable catalogue. It is absent
    /// from production builds and does not alter transaction state.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn dispatch_authenticated_raw_call_with_arguments_and_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        validate_raw_call_argument_shape(function, arguments)?;
        self.dispatch_authenticated_raw_call_with_options(
            authenticated_session,
            function,
            arguments,
            Some(RawDispatchTestBarrier { reached, resume }),
        )
        .await
    }

    async fn dispatch_authenticated_raw_call_with_options(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        test_barrier: Option<RawDispatchTestBarrier>,
    ) -> Result<AuthenticatedRawCallResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let mut transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            pause_after_raw_dispatch_recovery(test_barrier.as_ref()).await;
            let target = InvocationTarget::new(function, active.pair());

            let decision = match system_function_by_id(function) {
                // Catalogue health is the one separately admitted raw system
                // entry. Other registry entries may enter raw dispatch only
                // when their sealed security identity is admitted.
                Some(_) if function == CATALOGUE_HEALTH_FUNCTION_ID => {
                    security.authorise_catalogue_health(authenticated_session, target)
                }
                Some(definition) if is_admitted_security_identity(definition) => {
                    security.authorise_system_function(authenticated_session, target)
                }
                Some(_) => ExecuteDecision::Denied(ExecuteDenial::UnknownFunction),
                None if active.catalogue().function_by_id(function).is_none() => {
                    // A verified-standard target can enter only through the sealed
                    // invocation boundary. Raw dispatch has no standard target path.
                    ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
                }
                None => security.authorise_execute(authenticated_session, target),
            };
            let execution = match decision {
                ExecuteDecision::Denied(reason) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            target,
                            reason,
                        ),
                    )
                    .await?;
                    Err(PostgresKernelError::RawExecuteDenied {
                        pair: active.pair(),
                        function,
                        reason,
                    })
                }
                ExecuteDecision::Allowed(authorisation) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_allowed(&authorisation),
                    )
                    .await?;
                    match active.catalogue().function_by_id(function) {
                        None if function == CATALOGUE_HEALTH_FUNCTION_ID => {
                            if active.catalogue_hash_context().standard().is_none() {
                                Err(PostgresKernelError::DurableInvariant {
                                    relation: "_orna_kernel.active_revision",
                                    record: active.pair().catalogue().canonical(),
                                    rule: "catalogue health requires the accepted standard context",
                                })
                            } else if !arguments.is_empty() {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw call arguments require a supported active SERVER mutation target",
                                ))
                            } else {
                                Ok(AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(
                                    true,
                                )))
                            }
                        }
                        None if function == SYS_INVOKE_FUNCTION_ID => Err(
                            raw_call_target_unavailable(
                                function,
                                "sys.invoke requires its sealed request carrier",
                            ),
                        ),
                        Some(definition) if definition.domain() == FunctionDomain::Client => {
                            if arguments.len() != definition.parameters().len() {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw CLIENT arguments do not match the declared parameter set",
                                ))
                            } else {
                                evaluate_authorised_client_function_with_arguments(
                                    &active,
                                    &authorisation,
                                    arguments,
                                    &[],
                                    &self.capability_grants,
                                )
                                .map(|result| AuthenticatedRawCallResult::Client(result.into_value()))
                                .map_err(PostgresKernelError::ClientExecution)
                            }
                        }
                        Some(definition) if definition.domain() == FunctionDomain::Server => {
                            let reference_argument = matches!(
                                arguments,
                                [argument]
                                    if matches!(
                                        argument.value(),
                                        RuntimeValue::Reference { .. }
                                    )
                            );
                            let reference_mutation = reference_argument
                                .then(|| raw_server_reference_mutation_target(&active, function))
                                .flatten();
                            let reference_mutation = if matches!(arguments, [_, _])
                                && raw_server_reference_value_update_target_is_selected(
                                    &active, function,
                                )
                            {
                                Some(RawServerReferenceMutation::Update)
                            } else {
                                reference_mutation
                            };
                            let identity_selected_select = reference_argument
                                && raw_identity_selected_server_select_target_is_selected(
                                    &active, function,
                                );
                            let unique_text_selected_select =
                                raw_unique_text_selected_server_select_target_is_selected(
                                    &active, function,
                                );
                            if raw_server_insert_target_is_selected(&active, function) {
                                let savepoint = transaction
                                    .savepoint("raw_server_insert_execution")
                                    .await
                                    .map_err(PostgresKernelError::Database)?;
                                let insert = if arguments.is_empty() {
                                    execute_authorised_raw_server_insert(
                                        &savepoint,
                                        &active,
                                        &authorisation,
                                    )
                                    .await
                                } else {
                                    execute_authorised_raw_server_insert_with_arguments(
                                        &savepoint,
                                        &active,
                                        &authorisation,
                                        arguments,
                                    )
                                    .await
                                };
                                match insert {
                                    Ok(value) => {
                                        savepoint
                                            .commit()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Ok(AuthenticatedRawCallResult::Server(vec![value]))
                                    }
                                    Err(error) => {
                                        savepoint
                                            .rollback()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Err(classify_raw_server_insert_error(
                                            error,
                                            !arguments.is_empty(),
                                            function,
                                        ))
                                    }
                                }
                            } else if let Some(operation) = reference_mutation {
                                let savepoint = transaction
                                    .savepoint("raw_server_reference_mutation_execution")
                                    .await
                                    .map_err(PostgresKernelError::Database)?;
                                let mutation = execute_authorised_raw_server_reference_mutation(
                                    &savepoint,
                                    &active,
                                    &authorisation,
                                    operation,
                                    arguments,
                                )
                                .await;
                                match mutation {
                                    Ok(values) => {
                                        savepoint
                                            .commit()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Ok(AuthenticatedRawCallResult::Server(values))
                                    }
                                    Err(error) => {
                                        savepoint
                                            .rollback()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Err(classify_raw_server_reference_mutation_error(
                                            error, function,
                                        ))
                                    }
                                }
                            } else if !arguments.is_empty()
                                && !identity_selected_select
                                && !unique_text_selected_select
                            {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw call arguments require a supported active SERVER mutation target",
                                ))
                            } else {
                                let savepoint = transaction
                                    .savepoint("raw_server_select_execution")
                                    .await
                                    .map_err(PostgresKernelError::Database)?;
                                let server = execute_authorised_raw_server_select(
                                    &savepoint,
                                    &active,
                                    &authorisation,
                                    arguments,
                                )
                                .await
                                .map(AuthenticatedRawCallResult::Server);
                                match server {
                                    Ok(result) => {
                                        savepoint
                                            .commit()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Ok(result)
                                    }
                                    Err(error) => {
                                        savepoint
                                            .rollback()
                                            .await
                                            .map_err(PostgresKernelError::Database)?;
                                        Err(if identity_selected_select {
                                            classify_raw_identity_selected_server_error(
                                                error, function,
                                            )
                                        } else if unique_text_selected_select {
                                            classify_raw_unique_text_selected_server_error(
                                                error, function,
                                            )
                                        } else {
                                            classify_raw_server_error(error)
                                        })
                                    }
                                }
                            }
                        }
                        Some(_) if !arguments.is_empty() => Err(raw_call_target_unavailable(
                            function,
                            "raw call arguments require a supported active SERVER mutation target",
                        )),
                        Some(_) => Err(PostgresKernelError::DurableInvariant {
                            relation: "active catalogue",
                            record: function.canonical(),
                            rule: "active function domain is unsupported by raw dispatch",
                        }),
                        None => Err(PostgresKernelError::DurableInvariant {
                            relation: "active catalogue",
                            record: function.canonical(),
                            rule: "allowed raw target must exist in the active catalogue",
                        }),
                    }
                }
            };
            append_client_capability_audit(
                &transaction,
                authenticated_session,
                &active,
                target,
                &execution,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            execution
        }
        .await;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
    }

    /// Reserves one resource request identity before any target work starts.
    ///
    /// The reservation is committed independently of the execution transaction,
    /// so cancellation or rollback cannot make an identity reusable. The unique
    /// primary key serializes concurrent authenticated connections in PostgreSQL.
    async fn reserve_resource_request_id(
        &self,
        request_id: InvocationId,
    ) -> Result<bool, PostgresKernelError> {
        let mut session = self.open().await?;
        let reservation = async {
            let transaction = session
                .client
                .transaction()
                .await
                .map_err(PostgresKernelError::Database)?;
            let request_id = request_id.to_bytes().to_vec();
            let inserted = transaction
                .execute(
                    "INSERT INTO _orna_kernel.resource_request_history (request_id)
                     VALUES ($1)
                     ON CONFLICT (request_id) DO NOTHING",
                    &[&request_id],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(inserted == 1)
        }
        .await;
        let shutdown = session.shutdown().await;
        match (reservation, shutdown) {
            (Ok(reserved), Ok(())) => Ok(reserved),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// Dispatches one authenticated ORNA-RESOURCE target inside one transaction.
    ///
    /// The transport request is treated as untrusted metadata: the active
    /// revision, target definition, parameter set, result shape, and security
    /// principals are recovered by the kernel. The nested invocation identity
    /// is generated here and never taken from the request. Denials and failures
    /// return only the closed protocol failure class.
    /// Dispatches one authenticated ORNA-RESOURCE target without exposing
    /// cancellation state to callers that do not own the transport request.
    pub async fn dispatch_authenticated_server_resource(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
    ) -> Result<AuthenticatedServerResourceResult, PostgresKernelError> {
        let cancellation = ResourceCancellation::new();
        match self
            .dispatch_authenticated_server_resource_with_cancellation(
                authenticated_session,
                request,
                &cancellation,
            )
            .await?
        {
            Some(result) => Ok(result),
            None => Err(PostgresKernelError::DurableInvariant {
                relation: "resource cancellation",
                record: request.request_id.canonical(),
                rule: "uncancellable resource dispatch returned no terminal result",
            }),
        }
    }

    /// Dispatches one authenticated ORNA-RESOURCE target with cooperative
    /// cancellation owned by the transport adapter.
    pub async fn dispatch_authenticated_server_resource_with_cancellation(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<Option<AuthenticatedServerResourceResult>, PostgresKernelError> {
        self.dispatch_authenticated_server_resource_with_cancellation_and_test_barrier(
            authenticated_session,
            request,
            cancellation,
            None,
        )
        .await
    }

    /// Pauses direct resource dispatch after its active-revision validation.
    ///
    /// This hook is absent from production builds and exists only to make the
    /// active-pointer interleave deterministic in the integration harness.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn dispatch_authenticated_server_resource_with_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<AuthenticatedServerResourceResult, PostgresKernelError> {
        let cancellation = ResourceCancellation::new();
        match self
            .dispatch_authenticated_server_resource_with_cancellation_and_test_barrier(
                authenticated_session,
                request,
                &cancellation,
                Some(AuthenticatedResourceTestBarrier { reached, resume }),
            )
            .await?
        {
            Some(result) => Ok(result),
            None => Err(PostgresKernelError::DurableInvariant {
                relation: "resource cancellation",
                record: request.request_id.canonical(),
                rule: "uncancellable resource dispatch returned no terminal result",
            }),
        }
    }

    async fn dispatch_authenticated_server_resource_with_cancellation_and_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
        test_barrier: Option<AuthenticatedResourceTestBarrier>,
    ) -> Result<Option<AuthenticatedServerResourceResult>, PostgresKernelError> {
        validate_resource_state_context(request)?;
        if !self.reserve_resource_request_id(request.request_id).await? {
            return Ok(Some(AuthenticatedServerResourceResult::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            }));
        }
        let mut database_session = self.open().await?;
        let operation = async {
            let mut transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            lock_active_revision_for_resource(&transaction, active.pair()).await?;
            let execution_active = configure_and_recover(&transaction).await?;
            if execution_active.pair() != active.pair() {
                return Err(PostgresKernelError::SecurityRevisionMismatch {
                    expected: active.pair(),
                    active: execution_active.pair(),
                });
            }
            let execution_security =
                recover_security_snapshot_for_active(&transaction, &execution_active).await?;
            if !security_snapshots_match(&execution_security, &security) {
                return Err(PostgresKernelError::SecurityFunctionSetMismatch);
            }
            let active = execution_active;
            let security = execution_security;
            pause_after_authenticated_resource_validation(test_barrier.as_ref()).await;
            let invocation = InvocationId::new();
            let mut audit_decision = SecurityAuditOutcome::Denied;
            let mut completed_target = None;
            let failed = |failure| AuthenticatedServerResourceResult::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure,
            };
            let completed = |values| AuthenticatedServerResourceResult::Completed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: invocation,
                target_revision: active.pair(),
                resource_kind: request.resource_kind,
                values,
            };

            let result = if request.target_revision != active.pair() {
                append_unresolved_invocation_audit(
                    &transaction,
                    authenticated_session,
                    invocation,
                )
                .await?;
                failed(CallFailure::TargetUnavailable)
            } else {
                let entry_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
                match security.authorise_system_function(authenticated_session, entry_target) {
                    ExecuteDecision::Denied(reason) => {
                        let event_id = append_security_audit_event(
                            &transaction,
                            SecurityAuditDecision::execute_denied(
                                authenticated_session,
                                entry_target,
                                reason,
                            ),
                        )
                        .await?;
                        append_linked_invocation_audit(&transaction, invocation, event_id).await?;
                        failed(CallFailure::ExecuteDenied)
                    }
                    ExecuteDecision::Allowed(_) => {
                        match resolve_resource_target(&active, request.target_function_id) {
                            None => {
                                append_unresolved_invocation_audit(
                                    &transaction,
                                    authenticated_session,
                                    invocation,
                                )
                                .await?;
                                failed(CallFailure::TargetUnavailable)
                            }
                            Some(resolved_target) => {
                                let target = resolved_target.target();
                                completed_target = Some(target);
                                match security.authorise_execute(authenticated_session, target) {
                            ExecuteDecision::Denied(reason) => {
                                let event_id = append_security_audit_event(
                                    &transaction,
                                    SecurityAuditDecision::execute_denied(
                                        authenticated_session,
                                        target,
                                        reason,
                                    ),
                                )
                                .await?;
                                append_linked_invocation_audit(
                                    &transaction,
                                    invocation,
                                    event_id,
                                )
                                .await?;
                                failed(CallFailure::ExecuteDenied)
                            }
                            ExecuteDecision::Allowed(authorisation) => {
                                audit_decision = SecurityAuditOutcome::Allowed;
                                append_allowed_invocation_audit(
                                    &transaction,
                                    &security,
                                    authenticated_session,
                                    target,
                                    invocation,
                                )
                                .await?;
                                let definition = resolved_target.definition();
                                if !resource_target_shape_is_supported(
                                    definition,
                                    request.resource_kind,
                                ) {
                                    failed(CallFailure::TargetUnavailable)
                                } else {
                                    match bind_authenticated_resource_arguments(
                                        active.catalogue_hash_context(),
                                        definition,
                                        &request.arguments,
                                    ) {
                                        None => failed(CallFailure::TargetUnavailable),
                                        Some(arguments) => {
                                            if let Some(executable) = resolved_target.executable() {
                                                match execute_standard_parameter_echo(
                                                    definition,
                                                    executable.revision(),
                                                    &arguments,
                                                ) {
                                                    Ok(value) => completed(vec![value]),
                                                    Err(_) => failed(CallFailure::TargetUnavailable),
                                                }
                                            } else {
                                                let savepoint = transaction
                                                    .savepoint(
                                                        "authenticated_server_resource_execution",
                                                    )
                                                    .await
                                                    .map_err(PostgresKernelError::Database)?;
                                                let execution = execute_authorised_server_select(
                                                    &savepoint,
                                                    &active,
                                                    &authorisation,
                                                    &arguments,
                                                )
                                                .await;
                                                match execution {
                                                    Ok(server) => {
                                                        let values =
                                                            resource_values_from_server_result(
                                                                request.resource_kind,
                                                                server,
                                                            );
                                                        match values {
                                                            Some(values) => {
                                                                savepoint
                                                                    .commit()
                                                                    .await
                                                                    .map_err(PostgresKernelError::Database)?;
                                                                completed(values)
                                                            }
                                                            None => {
                                                                savepoint
                                                                    .rollback()
                                                                    .await
                                                                    .map_err(PostgresKernelError::Database)?;
                                                                failed(CallFailure::TargetUnavailable)
                                                            }
                                                        }
                                                    }
                                                    Err(error) => {
                                                        savepoint
                                                            .rollback()
                                                            .await
                                                            .map_err(PostgresKernelError::Database)?;
                                                        let failure = match error {
                                                            PostgresKernelError::ServerSelect(source)
                                                                if raw_server_target_is_unavailable(
                                                                    &source,
                                                                ) => CallFailure::TargetUnavailable,
                                                            _ => CallFailure::InternalFailure,
                                                        };
                                                        failed(failure)
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                }
                            }
                        }
                    }
                }
                }
            };
            if !cancellation.try_begin_commit() {
                transaction
                    .rollback()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                return Ok(None);
            }
            let (terminal, audit_target, item_count) = match &result {
                AuthenticatedServerResourceResult::Completed { values, .. } => (
                    ResourceAuditTerminalOutcome::Completed,
                    Some(completed_target.ok_or_else(|| {
                        sealed_target_invariant(
                            &active,
                            "completed resource target must retain its resolved invocation target",
                        )
                    })?),
                    Some(values.len() as u64),
                ),
                AuthenticatedServerResourceResult::Failed { .. } => (
                    ResourceAuditTerminalOutcome::Failed,
                    completed_target,
                    None,
                ),
            };
            append_resource_audit_event(
                &transaction,
                authenticated_session,
                request,
                invocation,
                audit_decision,
                terminal,
                audit_target,
                item_count,
                None,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            cancellation.commit_finished();
            Ok(Some(result))
        }
        .await;
        let result = finish_authenticated_server_select_session(
            operation,
            database_session.shutdown().await,
        );
        match result {
            Ok(Some(result)) => Ok(Some(result)),
            Ok(None) => {
                self.record_cancelled_resource_audit(authenticated_session, request)
                    .await?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Starts one bounded authenticated SERVER resource producer.
    ///
    /// The returned handle contains only owned command channels and acceptance
    /// metadata. The spawned task owns the RepeatableRead transaction and the
    /// PostgreSQL query stream for its entire lifetime.
    pub async fn start_authenticated_server_resource_producer(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::None,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PreAcceptance,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptance,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_audit_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptanceAudit,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_audit_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptanceAuditCancellation,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_exit_audit_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptanceCancelledExitAudit,
        )
        .await
    }

    async fn start_authenticated_server_resource_producer_with_failure_hook(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
        failure_stage: ResourceProducerFailureStage,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        validate_resource_state_context(request)?;
        if !self.reserve_resource_request_id(request.request_id).await? {
            return Ok(AuthenticatedServerResourceStart::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            });
        }
        let (commands, command_receiver) = tokio::sync::mpsc::channel(1);
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let request_id = request.request_id;
        let kernel = self.clone();
        let session = authenticated_session.clone();
        let request = request.clone();
        let worker_cancellation = cancellation.clone();
        tokio::spawn(async move {
            let _ = run_authenticated_server_resource_producer_task(
                kernel,
                session,
                request,
                worker_cancellation,
                failure_stage,
                command_receiver,
                ready_sender,
            )
            .await;
        });
        let mut start_guard = ResourceProducerStartGuard::new(cancellation.clone());
        match ready_receiver
            .await
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: request_id.canonical(),
                rule: "producer task terminated before acceptance",
            })?? {
            ResourceProducerReady::Accepted(accepted) => {
                start_guard.disarm();
                Ok(AuthenticatedServerResourceStart::Accepted(
                    AuthenticatedServerResourceProducer {
                        accepted,
                        commands,
                        cancellation: cancellation.clone(),
                    },
                ))
            }
            ResourceProducerReady::Failed {
                stream_id,
                request_id,
                failure,
            } => {
                start_guard.disarm();
                Ok(AuthenticatedServerResourceStart::Failed {
                    stream_id,
                    request_id,
                    failure,
                })
            }
        }
    }

    /// Dispatches one sealed `sys.invoke` Request inside one transaction.
    ///
    /// This boundary recovers the active revision and its security snapshot,
    /// decodes the retained Request against the opaque codec registry of the exact
    /// verified standard snapshot, makes the redacted protected decision, and then
    /// executes either an application CLIENT target through the local evaluator or
    /// a verified-standard target through its pinned executable. Completed
    /// invocations emit `InvocationStarted(0)`, `ValueBatch(1)`, and
    /// `InvocationCompleted(2)`. Denied requests return without executing an
    /// artifact. Every decision is appended as protected security and invocation
    /// audit evidence before the transaction commits; the invocation-audit row
    /// keeps the historical application `RevisionPair` as its durable standard
    /// pin.
    ///
    /// The invocation first passes the protected `sys.invoke` gate. Application
    /// CLIENT targets use the local evaluator, while application SERVER targets
    /// use the authenticated SERVER SELECT executor.
    pub async fn dispatch_sealed_sys_invoke(
        &self,
        authenticated_session: &AuthenticatedSession,
        connection_protocol_major: u16,
        request: &RetainedInvokeRequest,
    ) -> Result<SealedInvocationResult, PostgresKernelError> {
        self.dispatch_sealed_sys_invoke_with_resource_executor(
            authenticated_session,
            connection_protocol_major,
            request,
            None,
        )
        .await
    }

    /// Dispatches one sealed invocation with an optional host-owned resource
    /// executor for CLIENT resource expressions.
    #[doc(hidden)]
    pub async fn dispatch_sealed_sys_invoke_with_resource_executor(
        &self,
        authenticated_session: &AuthenticatedSession,
        connection_protocol_major: u16,
        request: &RetainedInvokeRequest,
        resource_executor: Option<&mut dyn ClientResourceExecutor>,
    ) -> Result<SealedInvocationResult, PostgresKernelError> {
        let mut state = ClientStateStore::new();
        let invocation = InvocationId::new();
        let mut capability_audit_appended = false;
        self.dispatch_sealed_sys_invoke_with_resource_executor_and_state(
            authenticated_session,
            connection_protocol_major,
            request,
            resource_executor,
            &mut state,
            invocation,
            &mut capability_audit_appended,
        )
        .await
    }

    /// Dispatches one sealed invocation with an optional host-owned resource
    /// executor for CLIENT resource expressions.
    #[doc(hidden)]
    pub async fn dispatch_sealed_sys_invoke_with_resource_executor_and_state(
        &self,
        authenticated_session: &AuthenticatedSession,
        connection_protocol_major: u16,
        request: &RetainedInvokeRequest,
        resource_executor: Option<&mut dyn ClientResourceExecutor>,
        state: &mut ClientStateStore,
        invocation: InvocationId,
        capability_audit_appended: &mut bool,
    ) -> Result<SealedInvocationResult, PostgresKernelError> {
        self.dispatch_sealed_sys_invoke_with_resource_executor_and_state_internal(
            authenticated_session,
            connection_protocol_major,
            request,
            resource_executor,
            state,
            invocation,
            capability_audit_appended,
            None,
            None,
            None,
            None,
            false,
            None,
        )
        .await
    }

    async fn dispatch_sealed_sys_invoke_with_resource_executor_and_state_internal(
        &self,
        authenticated_session: &AuthenticatedSession,
        connection_protocol_major: u16,
        request: &RetainedInvokeRequest,
        mut resource_executor: Option<&mut dyn ClientResourceExecutor>,
        state: &mut ClientStateStore,
        invocation: InvocationId,
        capability_audit_appended: &mut bool,
        pinned_decoded: Option<&orna_core::invocation::InvokeRequest>,
        pinned_context: Option<(&ActiveDatabaseRevision, &SecuritySnapshot)>,
        pinned_registry: Option<&OpaqueCodecRegistry>,
        prepared_outcome: Option<&SealedInvocationPreparedOutcome>,
        pre_audited: bool,
        cancellation: Option<&ResourceCancellation>,
    ) -> Result<SealedInvocationResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let mut invocation_audit_appended = pre_audited;
        let mut user_state_loaded = false;
        let mut user_state_revision = None;
        let mut loaded_user_state_cells: Option<Vec<UserStateCell>> = None;
        let operation = async {
            loop {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            if let Some((pinned_active, pinned_security)) = pinned_context {
                if active.pair() != pinned_active.pair() {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.active_revision",
                        record: invocation.canonical(),
                        rule: "sealed invocation active revision changed before execution",
                    });
                }
                if !security_snapshots_match(&security, pinned_security) {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.security_audit_events",
                        record: invocation.canonical(),
                        rule: "sealed invocation security snapshot changed before execution",
                    });
                }
            }
            let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: active.pair().catalogue().canonical(),
                    rule: "sealed sys.invoke requires the accepted verified standard snapshot",
                }
            })?;
            let registry = match pinned_registry {
                Some(registry) => registry.clone(),
                None => registered_opaque_codecs(standard).map_err(|_| {
                    PostgresKernelError::DurableInvariant {
                        relation: "_orna_kernel.standard_library_revisions",
                        record: standard.revision().canonical(),
                        rule: "the verified standard snapshot must bind its opaque codec registry",
                    }
                })?,
            };
            let decoded = match pinned_decoded {
                Some(decoded) => decoded.clone(),
                None => decode_retained_invoke_request(&active, &registry, request)
                    .map_err(PostgresKernelError::SealedInvocation)?,
            };
            let system_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
            let decision = match prepared_outcome {
                Some(SealedInvocationPreparedOutcome::Allowed { .. }) => {
                    ProtectedInvocationDecision::Allowed
                }
                Some(_) => {
                    return Err(sealed_target_invariant(
                        &active,
                        "prepared sealed dispatch requires an allowed pinned outcome",
                    ));
                }
                None => decide_protected_invocation(
                    &security,
                    authenticated_session,
                    system_target,
                    &active,
                    connection_protocol_major,
                    &decoded,
                ),
            };
            let result = match decision {
                ProtectedInvocationDecision::Allowed => {
                    let (target, security_target, authorisation) = match prepared_outcome {
                        Some(SealedInvocationPreparedOutcome::Allowed {
                            target,
                            security_target,
                            authorisation,
                        }) => (target.clone(), *security_target, authorisation.clone()),
                        Some(_) => {
                            return Err(sealed_target_invariant(
                                &active,
                                "prepared sealed dispatch requires an allowed pinned target",
                            ));
                        }
                        None => {
                            let resolved =
                                resolve_sealed_target(&active, decoded.target()).ok_or_else(|| {
                                    sealed_target_invariant(
                                        &active,
                                        "allowed sealed invocation target must resolve",
                                    )
                                })?;
                            let security_target = sealed_security_target(&active, resolved);
                            let authorisation = match authorise_sealed_target(
                                &security,
                                authenticated_session,
                                security_target,
                            ) {
                                ExecuteDecision::Allowed(authorisation) => authorisation,
                                ExecuteDecision::Denied(_) => {
                                    return Err(sealed_target_invariant(
                                        &active,
                                        "allowed sealed invocation must re-authorise its pinned target",
                                    ));
                                }
                            };
                            (
                                PreparedSealedTarget::from_resolved(resolved),
                                security_target,
                                authorisation,
                            )
                        }
                    };
                    let (values, security_target) = match &target {
                        PreparedSealedTarget::Application { definition } => {
                            match definition.domain() {
                                FunctionDomain::Client => {
                                    if !invocation_audit_appended {
                                        append_allowed_invocation_audit_evidence(
                                            &transaction,
                                            &authorisation,
                                            invocation,
                                        )
                                        .await?;
                                        invocation_audit_appended = true;
                                    }
                                    let arguments =
                                        bind_sealed_invoke_arguments(definition, decoded.arguments())?;
                                    let state_context = ClientStateContext::new(
                                        definition.id(),
                                        decoded
                                            .state_profile()
                                            .map_or_else(String::new, str::to_owned),
                                        String::new(),
                                    )
                                    .map_err(|_| {
                                        sealed_target_invariant(
                                            &active,
                                            "sealed invocation state profile must be canonical",
                                        )
                                    })?;
                                    if user_state_loaded {
                                        if user_state_revision != Some(active.pair()) {
                                            return Err(PostgresKernelError::DurableInvariant {
                                                relation: "_orna_kernel.active_revision",
                                                record: invocation.canonical(),
                                                rule: "sealed CLIENT USER state must retain its pinned active revision",
                                            });
                                        }
                                    } else {
                                        let cells = load_user_state_in_transaction(
                                            &transaction,
                                            authenticated_session,
                                            &active,
                                            &registry,
                                            state_context.root_function(),
                                            state_context.state_profile(),
                                            &[],
                                            &BTreeMap::new(),
                                        )
                                        .await?;
                                        append_security_audit_event(
                                            &transaction,
                                            SecurityAuditDecision::user_state_allowed(
                                                authenticated_session,
                                                UserStateAuditOperation::Load,
                                                state_context.root_function(),
                                                cells.len() as u64,
                                            ),
                                        )
                                        .await?;
                                        state.set_context(state_context.clone());
                                        state.load_user_state(&cells).map_err(|_| {
                                            PostgresKernelError::DurableInvariant {
                                                relation: "CLIENT state store",
                                                record: format!("{:?}", definition.id()),
                                                rule: "sealed CLIENT USER state load must populate the caller-owned store",
                                            }
                                        })?;
                                        loaded_user_state_cells = Some(cells);
                                        user_state_loaded = true;
                                        user_state_revision = Some(active.pair());
                                    }
                                    let execution = if let Some(executor) =
                                        resource_executor.as_deref_mut()
                                    {
                                        evaluate_authorised_client_function_with_state_context_and_arguments_and_executor(
                                            &active,
                                            &authorisation,
                                            &state_context,
                                            &arguments,
                                            &[],
                                            &self.capability_grants,
                                            state,
                                            invocation,
                                            executor,
                                        )
                                    } else {
                                        evaluate_authorised_client_function_with_state_context_and_arguments(
                                            &active,
                                            &authorisation,
                                            &state_context,
                                            &arguments,
                                            &[],
                                            &self.capability_grants,
                                            state,
                                        )
                                    }
                                    .map_err(PostgresKernelError::ClientExecution);
                                    let capability_denied = matches!(
                                        &execution,
                                        Err(PostgresKernelError::ClientExecution(
                                            ClientExecutionError::CapabilityDenied { .. }
                                        ))
                                    );
                                    if !*capability_audit_appended || capability_denied {
                                        append_client_capability_audit(
                                            &transaction,
                                            authenticated_session,
                                            &active,
                                            security_target,
                                            &execution,
                                        )
                                        .await?;
                                        if !capability_denied {
                                            *capability_audit_appended = true;
                                        }
                                    }
                                    let value = match execution {
                                        Ok(result) => result.into_value(),
                                        Err(error) => {
                                            let pending = match &error {
                                                PostgresKernelError::ClientExecution(
                                                    ClientExecutionError::ResourceEvaluation {
                                                        context,
                                                        source: ClientResourceExecutionError::Pending {
                                                            key,
                                                            generation,
                                                        },
                                                    },
                                                ) => Some((*context, *key, *generation)),
                                                _ => None,
                                            };
                                            let Some((context, key, generation)) = pending else {
                                                transaction
                                                    .commit()
                                                    .await
                                                    .map_err(PostgresKernelError::Database)?;
                                                return Err(error);
                                            };
                                            transaction
                                                .commit()
                                                .await
                                                .map_err(PostgresKernelError::Database)?;
                                            let Some(executor) = resource_executor.as_deref_mut() else {
                                                return Err(error);
                                            };
                                            let completion = loop {
                                                let completion = if cancellation.is_some_and(ResourceCancellation::is_requested) {
                                                    executor.cancel_pending().or_else(|| executor.poll())
                                                } else {
                                                    executor.poll()
                                                };
                                                let Some(completion) = completion else {
                                                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                                                    continue;
                                                };
                                                let (completion_key, completion_generation) = match &completion {
                                                    ClientResourceCompletion::Ready { key, generation, .. }
                                                    | ClientResourceCompletion::StreamValues { key, generation, .. }
                                                    | ClientResourceCompletion::StreamCompleted { key, generation, .. }
                                                    | ClientResourceCompletion::Pending { key, generation, .. }
                                                    | ClientResourceCompletion::Failed { key, generation, .. }
                                                    | ClientResourceCompletion::Cancelled { key, generation, .. } => (*key, *generation),
                                                };
                                                if completion_key != key || completion_generation != generation {
                                                    return Err(PostgresKernelError::ClientExecution(
                                                        ClientExecutionError::ResourceEvaluation {
                                                            context,
                                                            source: ClientResourceExecutionError::Failed(
                                                                "resource.executor.invalid_completion".to_owned(),
                                                            ),
                                                        },
                                                    ));
                                                }
                                                if matches!(completion, ClientResourceCompletion::Pending { .. }) {
                                                    return Err(PostgresKernelError::ClientExecution(
                                                        ClientExecutionError::ResourceEvaluation {
                                                            context,
                                                            source: ClientResourceExecutionError::Failed(
                                                                "resource.executor.invalid_completion".to_owned(),
                                                            ),
                                                        },
                                                    ));
                                                }
                                                break completion;
                                            };
                                            let Some(resource) = state.resource_mut(key) else {
                                                return Err(PostgresKernelError::ClientExecution(
                                                    ClientExecutionError::ResourceEvaluation {
                                                        context,
                                                        source: ClientResourceExecutionError::Failed(
                                                            "resource.executor.invalid_state".to_owned(),
                                                        ),
                                                    },
                                                ));
                                            };
                                            if resource.key() != key || resource.generation() != generation {
                                                return Err(PostgresKernelError::ClientExecution(
                                                    ClientExecutionError::ResourceEvaluation {
                                                        context,
                                                        source: ClientResourceExecutionError::Failed(
                                                            "resource.executor.invalid_state".to_owned(),
                                                        ),
                                                    },
                                                ));
                                            }
                                            let impossible = match resource.kind() {
                                                ResourceKind::Scalar => matches!(
                                                    &completion,
                                                    ClientResourceCompletion::StreamValues { .. }
                                                        | ClientResourceCompletion::StreamCompleted { .. }
                                                ),
                                                ResourceKind::Stream => {
                                                    matches!(&completion, ClientResourceCompletion::Ready { .. })
                                                }
                                            };
                                            if impossible {
                                                return Err(PostgresKernelError::ClientExecution(
                                                    ClientExecutionError::ResourceEvaluation {
                                                        context,
                                                        source: ClientResourceExecutionError::Failed(
                                                            "resource.executor.invalid_completion".to_owned(),
                                                        ),
                                                    },
                                                ));
                                            }
                                            if let Err(source) = resource.apply_completion(&active, completion) {
                                                return Err(PostgresKernelError::ClientExecution(
                                                    ClientExecutionError::ResourceEvaluation {
                                                        context,
                                                        source: ClientResourceExecutionError::Invalid(source),
                                                    },
                                                ));
                                            }
                                            continue;
                                        }
                                    };
                                    (vec![value], security_target)
                                }
                                FunctionDomain::Server => {
                                    if !invocation_audit_appended
                                        && append_allowed_invocation_audit_evidence(
                                            &transaction,
                                            &authorisation,
                                            invocation,
                                        )
                                        .await
                                        .is_err()
                                    {
                                        let _ = transaction.rollback().await;
                                        return sealed_failure_result(
                                            invocation,
                                            SealedInvocationFailureClass::Internal,
                                        );
                                    }
                                    invocation_audit_appended = true;
                                    if transaction.commit().await.is_err() {
                                        return sealed_failure_result(
                                            invocation,
                                            SealedInvocationFailureClass::Internal,
                                        );
                                    }
                                    return execute_sealed_server_after_audit(
                                        &mut database_session.client,
                                        &active,
                                        &security,
                                        &registry,
                                        authenticated_session,
                                        definition,
                                        &decoded,
                                        security_target,
                                        &authorisation,
                                        invocation,
                                    )
                                    .await;
                                }
                            }
                        }
                        PreparedSealedTarget::System { definition } => {
                            if !invocation_audit_appended
                                && !matches!(
                                    security.authorise_system_function(
                                        authenticated_session,
                                        security_target,
                                    ),
                                    ExecuteDecision::Allowed(_)
                                )
                            {
                                return Err(sealed_target_invariant(
                                    &active,
                                    "allowed sealed system invocation must re-authorise its target",
                                ));
                            }
                            let value = match definition.id() {
                                SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID => {
                                    let principal = self.session_principal(authenticated_session);
                                    RuntimeValue::Reference {
                                        target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
                                        object: ObjectId::from_bytes(principal.to_bytes()),
                                    }
                                }
                                SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID => {
                                    let principal = self.effective_principal(authenticated_session);
                                    RuntimeValue::Reference {
                                        target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
                                        object: ObjectId::from_bytes(principal.to_bytes()),
                                    }
                                }
                                SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID => {
                                    let descriptor =
                                        TypeDescriptor::set(TypeDescriptor::reference(
                                            SYS_SECURITY_PRINCIPAL_TYPE_ID,
                                        ))
                                        .map_err(|_| {
                                            sealed_target_invariant(
                                                &active,
                                                "sealed active_roles return descriptor must be valid",
                                            )
                                        })?;
                                    let values = self
                                        .active_roles(authenticated_session)
                                        .into_iter()
                                        .map(|principal| RuntimeValue::Reference {
                                            target: SYS_SECURITY_PRINCIPAL_TYPE_ID,
                                            object: ObjectId::from_bytes(principal.to_bytes()),
                                        })
                                        .collect();
                                    RuntimeValue::set(&active, descriptor, values).map_err(|_| {
                                        sealed_target_invariant(
                                            &active,
                                            "sealed active_roles return value must be valid",
                                        )
                                    })?
                                }
                                _ => {
                                    return Err(sealed_target_invariant(
                                        &active,
                                        "sealed system invocation target is not an admitted security identity",
                                    ));
                                }
                            };
                            (vec![value], security_target)
                        }
                        PreparedSealedTarget::VerifiedStandard {
                            definition,
                            executable,
                        } => {
                            let arguments =
                                bind_sealed_invoke_arguments(definition, decoded.arguments())?;
                            let value = match definition.id() {
                                STD_INVOKE_ECHO_FUNCTION_ID => execute_standard_parameter_echo(
                                    definition,
                                    executable.revision(),
                                    &arguments,
                                )?,
                                STD_JSON_ENCODE_FUNCTION_ID => execute_standard_json_encode(
                                    definition,
                                    executable.revision(),
                                    &arguments,
                                    &active,
                                    &registry,
                                )?,
                                _ => {
                                    return Err(sealed_target_invariant(
                                        &active,
                                        "verified standard invocation target has no execution engine",
                                    ));
                                }
                            };
                            (vec![value], security_target)
                        }
                    };
                    let events = match decoded.output_requirement() {
                        Some(requirement) => {
                            let mut values = values;
                            if values.len() != 1 {
                                return Err(sealed_target_invariant(
                                    &active,
                                    "sealed output requirements require exactly one result value",
                                ));
                            }
                            let value = values.pop().expect("one result value was checked");
                            match present_sealed_standard_output(
                                requirement,
                                value,
                                &active,
                                &registry,
                            ) {
                                Ok(presented) => Some(sealed_completed_events(
                                    authenticated_session.principal(),
                                    invocation,
                                    presented,
                                )?),
                                Err(
                                    SealedPresentationError::OutputResolution(_)
                                    | SealedPresentationError::NoPath,
                                ) => {
                                    if !invocation_audit_appended {
                                        append_allowed_invocation_audit(
                                            &transaction,
                                            &security,
                                            authenticated_session,
                                            security_target,
                                            invocation,
                                        )
                                        .await?;
                                        invocation_audit_appended = true;
                                    }
                                    None
                                }
                                Err(SealedPresentationError::Kernel(error)) => return Err(error),
                            }
                        }
                        None => Some(sealed_completed_events_from_values(
                            authenticated_session.principal(),
                            invocation,
                            values,
                        )?),
                    };
                    match events {
                        Some(events) => {
                            if !invocation_audit_appended {
                                append_allowed_invocation_audit(
                                    &transaction,
                                    &security,
                                    authenticated_session,
                                    security_target,
                                    invocation,
                                )
                                .await?;
                                invocation_audit_appended = true;
                            }
                            capture_sealed_invocation_snapshot(
                                &transaction,
                                &active,
                                &registry,
                                authenticated_session,
                                invocation,
                                security_target.function(),
                                &events,
                                decoded.client_offer(),
                                loaded_user_state_cells.as_deref(),
                            )
                            .await?;
                            SealedInvocationResult::Completed { invocation, events }
                        }
                        None => SealedInvocationResult::PresentationFailed { invocation },
                    }
                }
                ProtectedInvocationDecision::AllowedWithBindFailure => {
                    let target =
                        resolve_sealed_target(&active, decoded.target()).ok_or_else(|| {
                            sealed_target_invariant(
                                &active,
                                "bind-failed sealed invocation target must resolve",
                            )
                        })?;
                    append_allowed_invocation_audit(
                        &transaction,
                        &security,
                        authenticated_session,
                        sealed_security_target(&active, target),
                        invocation,
                    )
                    .await?;
                    SealedInvocationResult::Failed {
                        invocation,
                        events: sealed_failure_events(
                            invocation,
                            SealedInvocationFailureClass::Bind,
                        )?,
                    }
                }
                ProtectedInvocationDecision::EntryDenied => {
                    let entry_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
                    let reason = match security
                        .authorise_system_function(authenticated_session, entry_target)
                    {
                        ExecuteDecision::Denied(reason) => reason,
                        ExecuteDecision::Allowed(_) => ExecuteDenial::UnknownFunction,
                    };
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            entry_target,
                            reason,
                        ),
                    )
                    .await?;
                    append_invocation_audit_event(
                        &transaction,
                        InvocationAuditDecision::unresolved_denied(
                            invocation,
                            authenticated_session.principal(),
                        ),
                    )
                    .await?;
                    SealedInvocationResult::Denied { invocation }
                }
                ProtectedInvocationDecision::RequestRejected => {
                    append_invocation_audit_event(
                        &transaction,
                        InvocationAuditDecision::unresolved_denied(
                            invocation,
                            authenticated_session.principal(),
                        ),
                    )
                    .await?;
                    SealedInvocationResult::Denied { invocation }
                }
                ProtectedInvocationDecision::Denied => {
                    append_sealed_denied_audit(
                        &transaction,
                        &security,
                        authenticated_session,
                        &active,
                        decoded.target(),
                        invocation,
                    )
                    .await?;
                    SealedInvocationResult::Denied { invocation }
                }
                _ => {
                    append_unresolved_invocation_audit(
                        &transaction,
                        authenticated_session,
                        invocation,
                    )
                    .await?;
                    SealedInvocationResult::Denied { invocation }
                }
            };
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            break Ok(result);
            }
        }
        .await;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
    }

    /// Revalidates raw record arguments against one transactional active revision.
    ///
    /// An empty list performs no PostgreSQL operation. A non-empty list opens
    /// one read-only, repeatable-read transaction and returns only whether all
    /// record values remain canonical for its recovered active revision. This
    /// operation does not select, authorise, audit, or execute a target.
    pub async fn preflight_record_arguments(
        &self,
        records: Vec<RecordValue>,
    ) -> Result<RecordArgumentPreflight, PostgresKernelError> {
        if records.is_empty() {
            return Ok(RecordArgumentPreflight::NotRequired);
        }
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = recover_active_revision(&transaction).await?;
            let mut outcome = RecordArgumentPreflight::Current;
            for record in records {
                if encode_active_value(&active, &RuntimeValue::Record(record)).is_err() {
                    outcome = RecordArgumentPreflight::Stale;
                }
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(outcome)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Executes one authorised SERVER `SELECT` against one active snapshot.
    ///
    /// The operation records and commits its protected `EXECUTE` decision. It
    /// executes an allowed target through a savepoint. A target failure rolls
    /// back only that savepoint, then commits the allowed audit decision before
    /// it returns the original target error.
    pub async fn execute_authenticated_server_select(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            None,
            false,
        )
        .await
    }

    /// Pauses protected SERVER execution after security recovery for race proof.
    ///
    /// The hook exposes one deterministic point to the integration harness. It
    /// is absent from production builds and deliberately does not alter the
    /// transaction or decision authority.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_authenticated_server_select_with_test_barrier(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            Some(AuthenticatedSelectTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces driver shutdown after commit for cleanup-failure proof.
    ///
    /// The hook lets the integration harness prove that cleanup failure
    /// overrides a committed result. It is absent from production builds.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_authenticated_server_select_with_forced_post_commit_driver_shutdown(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        self.execute_authenticated_server_select_with_options(
            authenticated_session,
            function,
            arguments,
            None,
            true,
        )
        .await
    }

    async fn execute_authenticated_server_select_with_options(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
        arguments: &[FunctionArgument],
        test_barrier: Option<AuthenticatedSelectTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerSelectResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let mut transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            pause_after_authenticated_select_recovery(test_barrier.as_ref()).await;
            let target = InvocationTarget::new(function, active.pair());

            match security.authorise_execute(authenticated_session, target) {
                ExecuteDecision::Denied(reason) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            target,
                            reason,
                        ),
                    )
                    .await?;
                    transaction
                        .commit()
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    Err(PostgresKernelError::ServerExecuteDenied {
                        pair: active.pair(),
                        function,
                        reason,
                    })
                }
                ExecuteDecision::Allowed(authorisation) => {
                    append_security_audit_event(
                        &transaction,
                        SecurityAuditDecision::execute_allowed(&authorisation),
                    )
                    .await?;
                    let savepoint = transaction
                        .savepoint("server_select_execution")
                        .await
                        .map_err(PostgresKernelError::Database)?;
                    let execution = execute_authorised_server_select(
                        &savepoint,
                        &active,
                        &authorisation,
                        arguments,
                    )
                    .await;
                    match execution {
                        Ok(result) => {
                            savepoint
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            transaction
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            Ok(result)
                        }
                        Err(error) => {
                            savepoint
                                .rollback()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            transaction
                                .commit()
                                .await
                                .map_err(PostgresKernelError::Database)?;
                            Err(error)
                        }
                    }
                }
            }
        }
        .await;
        #[cfg(feature = "test-hooks")]
        if operation.is_ok() && force_post_commit_driver_shutdown {
            database_session.abort_driver();
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = force_post_commit_driver_shutdown;
        finish_authenticated_server_select_session(operation, database_session.shutdown().await)
    }

    /// Authenticates a kernel-supplied Linux peer UID with no selected roles.
    ///
    /// The operation appends and commits one protected audit record before it
    /// returns either the authenticated session or an expected typed denial.
    /// Database insertion, commit, or session shutdown failure replaces the
    /// authentication result with a kernel failure.
    pub async fn authenticate_local_peer(
        &self,
        uid: u32,
    ) -> Result<AuthenticatedSession, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let security = recover_security_snapshot(&transaction).await?;
            let mapped_principal = security
                .local_peer_credentials()
                .find(|credential| credential.uid() == uid)
                .map(LocalPeerCredential::principal);
            let authentication = security.authenticate_local_peer(uid);
            let decision = match &authentication {
                Ok(session) => SecurityAuditDecision::authentication_allowed(session),
                Err(reason) => SecurityAuditDecision::authentication_denied(
                    mapped_principal,
                    *reason,
                )
                .map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel security snapshot",
                    record: "local peer authentication".to_owned(),
                    rule: "mapped principal evidence must agree with the authentication result",
                })?,
            };
            append_security_audit_event(&transaction, decision).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(authentication)
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)?
            .map_err(PostgresKernelError::LocalPeerAuthentication)
    }

    /// Authorises and evaluates one CLIENT function against one active snapshot.
    ///
    /// The operation appends and commits the protected `EXECUTE` decision
    /// before it returns a value, a typed denial, or a pure evaluator failure.
    /// Database insertion, commit, or session shutdown failure replaces that
    /// operation result with a kernel failure.
    pub async fn evaluate_client_function(
        &self,
        authenticated_session: &AuthenticatedSession,
        function: FunctionId,
    ) -> Result<ClientExecutionResult, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = recover_active_revision(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let target = InvocationTarget::new(function, active.pair());
            let (decision, execution) =
                match security.authorise_execute(authenticated_session, target) {
                    ExecuteDecision::Allowed(authorisation) => {
                        let decision = SecurityAuditDecision::execute_allowed(&authorisation);
                        let execution = evaluate_authorised_client_function(
                            &active,
                            &authorisation,
                            &[],
                            &self.capability_grants,
                        )
                        .map_err(PostgresKernelError::ClientExecution);
                        (decision, execution)
                    }
                    ExecuteDecision::Denied(reason) => {
                        let decision = SecurityAuditDecision::execute_denied(
                            authenticated_session,
                            target,
                            reason,
                        );
                        let execution = Err(PostgresKernelError::ClientExecuteDenied {
                            pair: active.pair(),
                            function,
                            reason,
                        });
                        (decision, execution)
                    }
                };
            append_security_audit_event(&transaction, decision).await?;
            append_client_capability_audit(
                &transaction,
                authenticated_session,
                &active,
                target,
                &execution,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(execution)
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)?
    }

    /// Recovers the security decision snapshot for the active revision.
    pub async fn recover_security_snapshot(&self) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let snapshot = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(snapshot)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Recovers protected security audit history in database sequence order.
    pub async fn recover_security_audit_events(
        &self,
    ) -> Result<Vec<SecurityAuditEvent>, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            establish_trusted_search_path(&transaction).await?;
            require_current_migrations(&transaction).await?;
            let events = load_security_audit_events(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(events)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Records a redacted cancelled resource terminal in its own transaction.
    ///
    /// The request is untrusted metadata: no target is retained unless a later
    /// adapter path proves it independently. The kernel generates the nested
    /// invocation identity and writes the matching unresolved invocation audit
    /// row before committing the terminal record.
    pub async fn record_cancelled_resource_audit(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
    ) -> Result<(), PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let nested_invocation_id = InvocationId::new();
            append_unresolved_invocation_audit(
                &transaction,
                authenticated_session,
                nested_invocation_id,
            )
            .await?;
            append_resource_audit_event(
                &transaction,
                authenticated_session,
                request,
                nested_invocation_id,
                SecurityAuditOutcome::Denied,
                ResourceAuditTerminalOutcome::Cancelled,
                None,
                None,
                None,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Installs the fixed local service identity used by catalogue health.
    ///
    /// Repeating the exact UID is idempotent. A partial or conflicting durable
    /// identity fails without repair.
    pub async fn install_catalogue_health_service(
        &self,
        uid: u32,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            if active.catalogue_hash_context().standard().is_none() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: active.pair().catalogue().canonical(),
                    rule: "catalogue health service requires the accepted standard context",
                });
            }
            if active
                .catalogue()
                .function_by_id(CATALOGUE_HEALTH_FUNCTION_ID)
                .is_some()
            {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_functions",
                    record: CATALOGUE_HEALTH_FUNCTION_ID.canonical(),
                    rule: "application catalogue uses the reserved catalogue health identity",
                });
            }
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            match catalogue_health_service_uid(&current)? {
                None => {
                    if current
                        .local_peer_credentials()
                        .any(|credential| credential.uid() == uid)
                    {
                        return Err(catalogue_health_identity_error(
                            "_orna_kernel.security_local_peer_credentials",
                            "the catalogue health UID already selects another principal",
                        ));
                    }
                    insert_catalogue_health_identity(&transaction, uid).await?;
                }
                Some(installed_uid) if installed_uid == uid => {}
                Some(_) => {
                    return Err(catalogue_health_identity_error(
                        "_orna_kernel.security_local_peer_credentials",
                        "the reserved catalogue health service identity must be complete",
                    ));
                }
            }
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            require_catalogue_health_snapshot(&recovered, uid)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Grants the fixed catalogue-health service exactly one active application function.
    ///
    /// The expected pair prevents a stale source-apply caller from changing
    /// security for a later catalogue. The operation rebuilds the complete
    /// snapshot in one serializable transaction and is idempotent for the
    /// exact existing grant.
    pub async fn grant_catalogue_health_service_execute(
        &self,
        expected: RevisionPair,
        function: FunctionId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, expected).await?;
            let active = configure_and_recover(&transaction).await?;
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            let uid = catalogue_health_service_uid(&current)?.ok_or_else(|| {
                catalogue_health_identity_error(
                    "_orna_kernel.security_principals",
                    "the reserved catalogue health service identity must be complete",
                )
            })?;
            require_catalogue_health_snapshot(&current, uid)?;
            if function == CATALOGUE_HEALTH_FUNCTION_ID {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "active catalogue",
                    record: function.canonical(),
                    rule: "the catalogue health intrinsic cannot receive an application grant",
                });
            }
            if active.catalogue().function_by_id(function).is_none() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "active catalogue",
                    record: function.canonical(),
                    rule: "the requested function must exist in the active application catalogue",
                });
            }
            let mut grants = current.execute_grants().collect::<Vec<_>>();
            let requested_grant = ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                function,
            );
            if !grants.contains(&requested_grant) {
                grants.push(requested_grant);
            }
            let candidate = SecuritySnapshot::new_with_function_targets_and_local_peer_credentials(
                active.pair(),
                current.function_targets().collect(),
                current.principals().collect(),
                current.memberships().collect(),
                grants,
                current.local_peer_credentials().collect(),
            )
            .map_err(PostgresKernelError::SecuritySnapshot)?;
            require_complete_function_set(&active, &candidate)?;
            insert_execute_grant_if_absent(&transaction, requested_grant).await?;
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&candidate, &recovered) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    record: function.canonical(),
                    rule: "recovered fixed-service grant does not match the persisted security snapshot",
                });
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Atomically replaces all durable security decision records.
    pub async fn replace_security_snapshot(
        &self,
        snapshot: &SecuritySnapshot,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, snapshot.revision()).await?;
            let active = recover_active_revision(&transaction).await?;
            require_complete_function_set(&active, snapshot)?;
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            require_catalogue_health_identity_preserved(&current, snapshot)?;
            replace_security_rows(&transaction, snapshot).await?;
            let recovered = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }
}

async fn wait_for_resource_producer_pull_or_cancel(
    commands: &mut tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> Option<ResourceProducerPull> {
    let cancelled = cancellation.cancelled();
    let received = commands.recv();
    futures_util::pin_mut!(cancelled, received);
    match futures_util::future::select(cancelled, received).await {
        futures_util::future::Either::Left(((), _received)) => None,
        futures_util::future::Either::Right((
            Some(ResourceProducerCommand::Pull(pull)),
            _cancelled,
        )) => Some(pull),
        futures_util::future::Either::Right((None, _cancelled)) => None,
    }
}

async fn commit_resource_audit(
    transaction: Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    invocation: InvocationId,
    decision: SecurityAuditOutcome,
    terminal: ResourceAuditTerminalOutcome,
    target: Option<InvocationTarget>,
    item_count: Option<u64>,
    byte_count: Option<u64>,
) -> Result<(), PostgresKernelError> {
    append_resource_audit_event(
        &transaction,
        authenticated_session,
        request,
        invocation,
        decision,
        terminal,
        target,
        item_count,
        byte_count,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(PostgresKernelError::Database)
}

async fn commit_accepted_resource_cancelled_audit(
    transaction: Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    invocation: InvocationId,
    target: Option<InvocationTarget>,
) -> Result<(), PostgresKernelError> {
    commit_resource_audit(
        transaction,
        authenticated_session,
        request,
        invocation,
        SecurityAuditOutcome::Allowed,
        ResourceAuditTerminalOutcome::Cancelled,
        target,
        None,
        None,
    )
    .await
}

async fn commit_post_acceptance_resource_error_audit(
    kernel: &PostgresKernel,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    cancellation: &ResourceCancellation,
    invocation: InvocationId,
    target: Option<InvocationTarget>,
    lifecycle: &mut ResourceProducerLifecycle,
) -> Result<(), PostgresKernelError> {
    let mut session = kernel.open().await?;
    let operation = async {
        let transaction = session
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(PostgresKernelError::Database)?;
        require_current_migrations(&transaction).await?;
        let commit_started = cancellation.try_begin_commit();
        if commit_started {
            lifecycle.terminal_commit_started = true;
        }
        let cancellation_won = !commit_started && cancellation.is_requested();
        let terminal = if cancellation_won {
            ResourceAuditTerminalOutcome::Cancelled
        } else {
            ResourceAuditTerminalOutcome::Failed
        };
        append_resource_audit_event(
            &transaction,
            authenticated_session,
            request,
            invocation,
            SecurityAuditOutcome::Allowed,
            terminal,
            target,
            None,
            None,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(PostgresKernelError::Database)?;
        if !cancellation_won {
            cancellation.commit_finished();
        }
        Ok(())
    }
    .await;
    finish_authenticated_server_select_session(operation, session.shutdown().await)
}

fn send_resource_producer_ready(
    ready_sender: &mut Option<
        tokio::sync::oneshot::Sender<Result<ResourceProducerReady, PostgresKernelError>>,
    >,
    result: Result<ResourceProducerReady, PostgresKernelError>,
) {
    if let Some(sender) = ready_sender.take() {
        let _ = sender.send(result);
    }
}

fn finish_resource_producer_failure(
    lifecycle: &mut ResourceProducerLifecycle,
    result: Result<(), PostgresKernelError>,
) -> Result<(), PostgresKernelError> {
    if result.is_ok() {
        lifecycle.failure = Some(CallFailure::InternalFailure);
    }
    result
}

/// Repairs the durable evidence for a reserved resource request after the
/// worker transaction and session have been closed.
///
/// The reservation row is the per-request mutex. A normal terminal path has
/// already inserted the resource row, so the repair is a no-op in that case.
/// Otherwise this appends only closed invocation/resource identities and never
/// crosses request arguments, result values, credentials, or authority data.
async fn finalize_reserved_resource_request(
    kernel: &PostgresKernel,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    cancellation: &ResourceCancellation,
    lifecycle: &ResourceProducerLifecycle,
) -> Result<(), PostgresKernelError> {
    let mut database_session = kernel.open().await?;
    let operation = async {
        let transaction = database_session
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(PostgresKernelError::Database)?;
        require_current_migrations(&transaction).await?;
        let request_id = request.request_id.to_bytes().to_vec();
        let reserved = transaction
            .query_opt(
                "SELECT request_id
                 FROM _orna_kernel.resource_request_history
                 WHERE request_id = $1
                 FOR UPDATE",
                &[&request_id],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if reserved.is_none() {
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            return Ok(());
        }
        let terminal = transaction
            .query_opt(
                "SELECT 1
                 FROM _orna_kernel.resource_audit_events
                 WHERE request_id = $1",
                &[&request_id],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if terminal.is_some() {
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            cancellation.commit_finished();
            return Ok(());
        }

        let acceptance_committed = if lifecycle.acceptance_committed {
            true
        } else if lifecycle.acceptance_commit_attempted {
            let invocation =
                lifecycle
                    .invocation
                    .ok_or_else(|| PostgresKernelError::DurableInvariant {
                        relation: "resource producer",
                        record: request.request_id.canonical(),
                        rule: "attempted acceptance commit must retain its invocation identity",
                    })?;
            let target = lifecycle
                .target
                .ok_or_else(|| PostgresKernelError::DurableInvariant {
                    relation: "resource producer",
                    record: request.request_id.canonical(),
                    rule: "attempted acceptance commit must retain its target identity",
                })?;
            let invocation_id = invocation.to_bytes().to_vec();
            let row = transaction
                .query_opt(
                    "SELECT outcome,
                            session_principal_id,
                            function_id,
                            source_revision_id,
                            catalogue_revision_id,
                            security_audit_event_id
                     FROM _orna_kernel.invocation_audit_events
                     WHERE invocation_id = $1",
                    &[&invocation_id],
                )
                .await
                .map_err(PostgresKernelError::Database)?;
            match row {
                None => false,
                Some(row) => {
                    let outcome: String = row
                        .try_get("outcome")
                        .map_err(PostgresKernelError::Database)?;
                    let session_principal_id: Vec<u8> = row
                        .try_get("session_principal_id")
                        .map_err(PostgresKernelError::Database)?;
                    let function_id: Option<Vec<u8>> = row
                        .try_get("function_id")
                        .map_err(PostgresKernelError::Database)?;
                    let source_revision_id: Option<Vec<u8>> = row
                        .try_get("source_revision_id")
                        .map_err(PostgresKernelError::Database)?;
                    let catalogue_revision_id: Option<Vec<u8>> = row
                        .try_get("catalogue_revision_id")
                        .map_err(PostgresKernelError::Database)?;
                    let security_audit_event_id: Option<Vec<u8>> = row
                        .try_get("security_audit_event_id")
                        .map_err(PostgresKernelError::Database)?;
                    let accepted_identity_matches = outcome == "allowed"
                        && session_principal_id
                            == authenticated_session.principal().to_bytes().to_vec()
                        && function_id == Some(target.function().to_bytes().to_vec())
                        && source_revision_id
                            == Some(target.revision().source().to_bytes().to_vec())
                        && catalogue_revision_id
                            == Some(target.revision().catalogue().to_bytes().to_vec())
                        && security_audit_event_id.is_some();
                    if !accepted_identity_matches {
                        return Err(PostgresKernelError::DurableInvariant {
                            relation: "_orna_kernel.invocation_audit_events",
                            record: invocation.canonical(),
                            rule: "attempted acceptance commit retained invalid allowed evidence",
                        });
                    }
                    true
                }
            }
        } else {
            false
        };
        let invocation = if acceptance_committed {
            lifecycle
                .invocation
                .ok_or_else(|| PostgresKernelError::DurableInvariant {
                    relation: "resource producer",
                    record: request.request_id.canonical(),
                    rule: "accepted resource producer must retain its invocation identity",
                })?
        } else {
            lifecycle.invocation.unwrap_or_else(InvocationId::new)
        };
        let (decision, target) = if acceptance_committed {
            let target = lifecycle
                .target
                .ok_or_else(|| PostgresKernelError::DurableInvariant {
                    relation: "resource producer",
                    record: request.request_id.canonical(),
                    rule: "accepted resource producer must retain its target identity",
                })?;
            (SecurityAuditOutcome::Allowed, Some(target))
        } else {
            append_unresolved_invocation_audit(&transaction, authenticated_session, invocation)
                .await?;
            (SecurityAuditOutcome::Denied, None)
        };
        let finalizer_commit_started = cancellation.try_begin_commit();
        let terminal_commit_started = finalizer_commit_started || lifecycle.terminal_commit_started;
        let cancellation_won =
            lifecycle.cancelled || (!terminal_commit_started && cancellation.is_requested());
        let terminal = if cancellation_won {
            ResourceAuditTerminalOutcome::Cancelled
        } else {
            ResourceAuditTerminalOutcome::Failed
        };
        append_resource_audit_event(
            &transaction,
            authenticated_session,
            request,
            invocation,
            decision,
            terminal,
            target,
            None,
            None,
        )
        .await?;
        let result = transaction
            .commit()
            .await
            .map_err(PostgresKernelError::Database);
        if result.is_ok() && terminal_commit_started {
            cancellation.commit_finished();
        }
        result
    }
    .await;
    finish_authenticated_server_select_session(operation, database_session.shutdown().await)
}

async fn run_authenticated_server_resource_producer_task(
    kernel: PostgresKernel,
    authenticated_session: AuthenticatedSession,
    request: ResourceRequest,
    cancellation: ResourceCancellation,
    failure_stage: ResourceProducerFailureStage,
    commands: tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    ready_sender: tokio::sync::oneshot::Sender<Result<ResourceProducerReady, PostgresKernelError>>,
) -> Result<(), PostgresKernelError> {
    let mut lifecycle = ResourceProducerLifecycle::default();
    let mut ready_sender = Some(ready_sender);
    let worker_result = async {
        let mut database_session = kernel.open().await?;
        let operation = run_authenticated_server_resource_producer_task_body(
            kernel.clone(),
            authenticated_session.clone(),
            request.clone(),
            cancellation.clone(),
            failure_stage,
            &mut database_session,
            commands,
            &mut ready_sender,
            &mut lifecycle,
        )
        .await;
        let shutdown = database_session.shutdown().await;
        finish_authenticated_server_select_session(operation, shutdown)
    }
    .await;
    let finalizer = finalize_reserved_resource_request(
        &kernel,
        &authenticated_session,
        &request,
        &cancellation,
        &lifecycle,
    )
    .await;
    match worker_result {
        Err(error) => {
            if let Err(finalizer_error) = finalizer {
                if ready_sender.is_some() {
                    send_resource_producer_ready(&mut ready_sender, Err(finalizer_error));
                    return Ok(());
                }
                return Err(finalizer_error);
            }
            if ready_sender.is_some() {
                send_resource_producer_ready(&mut ready_sender, Err(error));
                Ok(())
            } else {
                Err(error)
            }
        }
        Ok(()) => match finalizer {
            Ok(()) => {
                send_resource_producer_ready(
                    &mut ready_sender,
                    Ok(ResourceProducerReady::Failed {
                        stream_id: request.stream_id,
                        request_id: request.request_id,
                        failure: lifecycle.failure.unwrap_or(CallFailure::InternalFailure),
                    }),
                );
                Ok(())
            }
            Err(error) => {
                if ready_sender.is_some() {
                    send_resource_producer_ready(&mut ready_sender, Err(error));
                    Ok(())
                } else {
                    Err(error)
                }
            }
        },
    }
}

async fn run_authenticated_server_resource_producer_task_body(
    kernel: PostgresKernel,
    authenticated_session: AuthenticatedSession,
    request: ResourceRequest,
    cancellation: ResourceCancellation,
    failure_stage: ResourceProducerFailureStage,
    database_session: &mut PostgresSession,
    mut commands: tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    ready_sender: &mut Option<
        tokio::sync::oneshot::Sender<Result<ResourceProducerReady, PostgresKernelError>>,
    >,
    lifecycle: &mut ResourceProducerLifecycle,
) -> Result<(), PostgresKernelError> {
    let transaction = database_session
        .client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
        .map_err(PostgresKernelError::Database)?;
    require_current_migrations(&transaction).await?;
    let active = configure_and_recover(&transaction).await?;
    let security = recover_security_snapshot_for_active(&transaction, &active).await?;
    let invocation = InvocationId::new();
    lifecycle.invocation = Some(invocation);
    if failure_stage == ResourceProducerFailureStage::PreAcceptance {
        transaction
            .query_one("SELECT no_such_resource_producer_column", &[])
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    let mut audit_decision = SecurityAuditOutcome::Denied;
    let mut authorisation = None;
    let mut bound_arguments = None;
    let mut completed_target = None;
    let mut standard_executable = None;
    let mut failure = None;

    if request.target_revision != active.pair() {
        append_unresolved_invocation_audit(&transaction, &authenticated_session, invocation)
            .await?;
        failure = Some(CallFailure::TargetUnavailable);
    } else {
        let entry_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
        match security.authorise_system_function(&authenticated_session, entry_target) {
            ExecuteDecision::Denied(reason) => {
                let event_id = append_security_audit_event(
                    &transaction,
                    SecurityAuditDecision::execute_denied(
                        &authenticated_session,
                        entry_target,
                        reason,
                    ),
                )
                .await?;
                append_linked_invocation_audit(&transaction, invocation, event_id).await?;
                failure = Some(CallFailure::ExecuteDenied);
            }
            ExecuteDecision::Allowed(_) => {
                match resolve_resource_target(&active, request.target_function_id) {
                    None => {
                        append_unresolved_invocation_audit(
                            &transaction,
                            &authenticated_session,
                            invocation,
                        )
                        .await?;
                        failure = Some(CallFailure::TargetUnavailable);
                    }
                    Some(resolved_target) => {
                        let target = resolved_target.target();
                        completed_target = Some(target);
                        lifecycle.target = Some(target);
                        match security.authorise_execute(&authenticated_session, target) {
                            ExecuteDecision::Denied(reason) => {
                                let event_id = append_security_audit_event(
                                    &transaction,
                                    SecurityAuditDecision::execute_denied(
                                        &authenticated_session,
                                        target,
                                        reason,
                                    ),
                                )
                                .await?;
                                append_linked_invocation_audit(&transaction, invocation, event_id)
                                    .await?;
                                failure = Some(CallFailure::ExecuteDenied);
                            }
                            ExecuteDecision::Allowed(allowed) => {
                                audit_decision = SecurityAuditOutcome::Allowed;
                                append_allowed_invocation_audit(
                                    &transaction,
                                    &security,
                                    &authenticated_session,
                                    target,
                                    invocation,
                                )
                                .await?;
                                let definition = resolved_target.definition();
                                if !resource_target_shape_is_supported(
                                    definition,
                                    request.resource_kind,
                                ) {
                                    failure = Some(CallFailure::TargetUnavailable);
                                } else {
                                    match bind_authenticated_resource_arguments(
                                        active.catalogue_hash_context(),
                                        definition,
                                        &request.arguments,
                                    ) {
                                        Some(arguments) => {
                                            standard_executable =
                                                resolved_target.executable().cloned();
                                            authorisation = Some(allowed);
                                            bound_arguments = Some(arguments);
                                        }
                                        None => failure = Some(CallFailure::TargetUnavailable),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(failure) = failure {
        lifecycle.failure = Some(failure);
        let commit_started = cancellation.try_begin_commit();
        if !commit_started {
            lifecycle.cancelled = true;
            return async {
                transaction
                    .rollback()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                kernel
                    .record_cancelled_resource_audit(&authenticated_session, &request)
                    .await
            }
            .await;
        }
        lifecycle.terminal_commit_started = true;
        let operation = async {
            append_resource_audit_event(
                &transaction,
                &authenticated_session,
                &request,
                invocation,
                audit_decision,
                ResourceAuditTerminalOutcome::Failed,
                completed_target,
                None,
                None,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)
        }
        .await;
        if operation.is_ok() {
            cancellation.commit_finished();
        }
        return operation;
    }

    let authorisation = authorisation.ok_or_else(|| {
        sealed_target_invariant(
            &active,
            "accepted resource producer must retain authorisation evidence",
        )
    })?;
    let bound_arguments = bound_arguments.ok_or_else(|| {
        sealed_target_invariant(
            &active,
            "accepted resource producer must retain bound arguments",
        )
    })?;
    // The accepted identity is externally visible, so commit its allowed
    // security/invocation/resource evidence before publishing Accepted. A
    // cancellation that already won before this boundary must not expose an
    // accepted stream without its durable audit row.
    if !cancellation.try_begin_acceptance_commit() {
        lifecycle.failure = Some(CallFailure::InternalFailure);
        return async {
            transaction
                .rollback()
                .await
                .map_err(PostgresKernelError::Database)?;
            kernel
                .record_cancelled_resource_audit(&authenticated_session, &request)
                .await
        }
        .await;
    }
    lifecycle.acceptance_commit_attempted = true;
    let operation = async {
        let request_id = request.request_id.to_bytes().to_vec();
        let reservation = transaction
            .query_opt(
                "SELECT request_id
                 FROM _orna_kernel.resource_request_history
                 WHERE request_id = $1
                 FOR UPDATE",
                &[&request_id],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
        if reservation.is_none() {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.resource_request_history",
                record: request.request_id.canonical(),
                rule: "accepted resource producer must retain its reservation",
            });
        }
        transaction
            .commit()
            .await
            .map_err(PostgresKernelError::Database)
    }
    .await;
    cancellation.acceptance_commit_finished();
    if operation.is_ok() {
        lifecycle.acceptance_committed = true;
    }
    if let Err(error) = operation {
        return Err(error);
    }
    let mut transaction_error = None;
    let transaction = match database_session
        .client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
        .map_err(PostgresKernelError::Database)
    {
        Ok(transaction) => Some(transaction),
        Err(error) => {
            transaction_error = Some(error);
            None
        }
    };
    if let Some(_error) = transaction_error {
        drop(transaction);
        let operation = commit_post_acceptance_resource_error_audit(
            &kernel,
            &authenticated_session,
            &request,
            &cancellation,
            invocation,
            completed_target,
            lifecycle,
        )
        .await;
        return finish_resource_producer_failure(lifecycle, operation);
    }
    let transaction = transaction.ok_or_else(|| {
        sealed_target_invariant(
            &active,
            "resource producer transaction was absent without a start error",
        )
    })?;
    if let Err(_error) = require_current_migrations(&transaction).await {
        let _ = transaction.rollback().await;
        let operation = commit_post_acceptance_resource_error_audit(
            &kernel,
            &authenticated_session,
            &request,
            &cancellation,
            invocation,
            completed_target,
            lifecycle,
        )
        .await;
        return finish_resource_producer_failure(lifecycle, operation);
    }
    if failure_stage == ResourceProducerFailureStage::PostAcceptanceAuditCancellation {
        cancellation.request_cancel();
    }
    let execution_validation = async {
        lock_active_revision_for_resource(&transaction, active.pair()).await?;
        let execution_active = configure_and_recover(&transaction).await?;
        if execution_active.pair() != active.pair() {
            return Err(PostgresKernelError::SecurityRevisionMismatch {
                expected: active.pair(),
                active: execution_active.pair(),
            });
        }
        let execution_security =
            recover_security_snapshot_for_active(&transaction, &execution_active).await?;
        if !security_snapshots_match(&execution_security, &security) {
            return Err(PostgresKernelError::SecurityFunctionSetMismatch);
        }
        if matches!(
            failure_stage,
            ResourceProducerFailureStage::PostAcceptanceAudit
                | ResourceProducerFailureStage::PostAcceptanceAuditCancellation
        ) {
            transaction
                .query_one("SELECT no_such_post_acceptance_audit_column", &[])
                .await
                .map_err(PostgresKernelError::Database)?;
        }
        Ok::<(), PostgresKernelError>(())
    }
    .await;
    if execution_validation.is_err() {
        let _ = transaction.rollback().await;
        let operation = commit_post_acceptance_resource_error_audit(
            &kernel,
            &authenticated_session,
            &request,
            &cancellation,
            invocation,
            completed_target,
            lifecycle,
        )
        .await;
        return finish_resource_producer_failure(lifecycle, operation);
    }

    if cancellation.is_requested() {
        lifecycle.cancelled = true;
        let operation = commit_accepted_resource_cancelled_audit(
            transaction,
            &authenticated_session,
            &request,
            invocation,
            completed_target,
        )
        .await;
        return finish_resource_producer_failure(lifecycle, operation);
    }

    let accepted = AuthenticatedServerResourceAccepted {
        stream_id: request.stream_id,
        request_id: request.request_id,
        nested_invocation_id: invocation,
        target_revision: active.pair(),
        resource_kind: match request.resource_kind {
            ProtocolResourceKind::Single => AuthenticatedServerResourceKind::Single,
            ProtocolResourceKind::Stream => AuthenticatedServerResourceKind::Stream,
        },
    };
    send_resource_producer_ready(ready_sender, Ok(ResourceProducerReady::Accepted(accepted)));
    if failure_stage == ResourceProducerFailureStage::PostAcceptance {
        transaction
            .query_one(
                "SELECT no_such_post_acceptance_resource_producer_column",
                &[],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    if failure_stage == ResourceProducerFailureStage::PostAcceptanceCancelledExitAudit {
        cancellation.request_cancel();
    }

    let stream_result = if let Some(executable) = standard_executable.as_ref() {
        run_authenticated_standard_resource_stream(
            &active,
            &authorisation,
            executable,
            &bound_arguments,
            &mut commands,
            &cancellation,
        )
        .await
    } else {
        run_authenticated_server_resource_stream(
            &transaction,
            &active,
            &authorisation,
            &bound_arguments,
            &mut commands,
            &cancellation,
        )
        .await
    };
    let stream_result = match stream_result {
        Ok(result) => result,
        Err(error) => match wait_for_resource_producer_pull_or_cancel(&mut commands, &cancellation)
            .await
        {
            Some(pull) => ResourceProducerExit::Failed(ResourceProducerFailed {
                response: Some(pull.response),
                error,
            }),
            None => ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None }),
        },
    };
    let mut terminal_error = None;
    match stream_result {
        ResourceProducerExit::Completed(ResourceProducerCompleted {
            response,
            final_batch_sequence,
            total_items,
            total_bytes,
        }) => {
            let commit_started = cancellation.try_begin_commit();
            if commit_started {
                lifecycle.terminal_commit_started = true;
            } else {
                lifecycle.cancelled = true;
            }
            let operation = if commit_started {
                commit_resource_audit(
                    transaction,
                    &authenticated_session,
                    &request,
                    invocation,
                    SecurityAuditOutcome::Allowed,
                    ResourceAuditTerminalOutcome::Completed,
                    completed_target,
                    Some(total_items),
                    Some(total_bytes),
                )
                .await
            } else {
                commit_accepted_resource_cancelled_audit(
                    transaction,
                    &authenticated_session,
                    &request,
                    invocation,
                    completed_target,
                )
                .await
            };
            if commit_started && operation.is_ok() {
                cancellation.commit_finished();
            }
            match operation {
                Ok(()) if commit_started => {
                    let _ = response.send(Ok(AuthenticatedServerResourceEvent::Completed {
                        final_batch_sequence,
                        total_items,
                        total_bytes,
                    }));
                }
                Ok(()) => {
                    let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
                }
                Err(error) => {
                    let _ = response.send(Err(error));
                }
            }
        }
        ResourceProducerExit::Cancelled(ResourceProducerCancelled { response }) => {
            let commit_started = cancellation.try_begin_commit();
            lifecycle.cancelled = true;
            if commit_started {
                lifecycle.terminal_commit_started = true;
            }
            if failure_stage == ResourceProducerFailureStage::PostAcceptanceCancelledExitAudit {
                transaction
                    .query_one(
                        "SELECT no_such_post_acceptance_cancelled_exit_audit_column",
                        &[],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
            }
            let operation = commit_accepted_resource_cancelled_audit(
                transaction,
                &authenticated_session,
                &request,
                invocation,
                completed_target,
            )
            .await;
            if commit_started && operation.is_ok() {
                cancellation.commit_finished();
            }
            if let Some(response) = response {
                match operation {
                    Ok(()) => {
                        let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
                    }
                    Err(error) => {
                        let _ = response.send(Err(error));
                    }
                }
            } else if let Err(error) = operation {
                terminal_error = Some(error);
            }
        }
        ResourceProducerExit::Failed(ResourceProducerFailed { response, error }) => {
            let failure = if standard_executable.is_some() {
                CallFailure::TargetUnavailable
            } else {
                match &error {
                    PostgresKernelError::ServerSelect(source)
                        if raw_server_target_is_unavailable(source) =>
                    {
                        CallFailure::TargetUnavailable
                    }
                    _ => CallFailure::InternalFailure,
                }
            };
            let commit_started = cancellation.try_begin_commit();
            if commit_started {
                lifecycle.terminal_commit_started = true;
            } else {
                lifecycle.cancelled = true;
            }
            let operation = if commit_started {
                commit_resource_audit(
                    transaction,
                    &authenticated_session,
                    &request,
                    invocation,
                    SecurityAuditOutcome::Allowed,
                    ResourceAuditTerminalOutcome::Failed,
                    completed_target,
                    None,
                    None,
                )
                .await
            } else {
                commit_accepted_resource_cancelled_audit(
                    transaction,
                    &authenticated_session,
                    &request,
                    invocation,
                    completed_target,
                )
                .await
            };
            if commit_started && operation.is_ok() {
                cancellation.commit_finished();
            }
            if let Some(response) = response {
                match operation {
                    Ok(()) if commit_started => {
                        let _ =
                            response.send(Ok(AuthenticatedServerResourceEvent::Failed { failure }));
                    }
                    Ok(()) => {
                        let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
                    }
                    Err(error) => {
                        let _ = response.send(Err(error));
                    }
                }
            } else if let Err(error) = operation {
                terminal_error = Some(error);
            }
        }
    }
    match terminal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn sealed_server_result_kind(return_type: &FunctionReturn) -> Option<ProtocolResourceKind> {
    match return_type {
        FunctionReturn::Single(_) => Some(ProtocolResourceKind::Single),
        FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => Some(ProtocolResourceKind::Stream),
    }
}

fn resource_target_shape_is_supported(
    definition: &FunctionDefinition,
    kind: ProtocolResourceKind,
) -> bool {
    if definition.domain() != FunctionDomain::Server {
        return false;
    }
    match (kind, definition.return_type()) {
        (ProtocolResourceKind::Single, FunctionReturn::Single(_)) => true,
        (ProtocolResourceKind::Stream, FunctionReturn::Stream(_)) => true,
        _ => false,
    }
}

fn bind_authenticated_resource_arguments(
    context: &CatalogueHashContext,
    definition: &FunctionDefinition,
    arguments: &[ResourceArgument],
) -> Option<Vec<FunctionArgument>> {
    if arguments.len() != definition.parameters().len() {
        return None;
    }
    let mut previous = None;
    let mut bound = Vec::with_capacity(arguments.len());
    for argument in arguments {
        if previous.is_some_and(|previous| argument.parameter <= previous) {
            return None;
        }
        previous = Some(argument.parameter);
        let parameter = definition.parameter_by_id(argument.parameter)?;
        if matches!(argument.value, RuntimeValue::Opaque(_)) {
            return None;
        }
        let RuntimeType::Flat(actual) = argument.value.runtime_type() else {
            return None;
        };
        if !runtime_types_match(context, actual, parameter.resolved_type()) {
            return None;
        }
        bound.push(FunctionArgument::new(argument.parameter, argument.value.clone()).ok()?);
    }
    Some(bound)
}

fn resource_result_value_is_supported(value: &RuntimeValue) -> bool {
    !matches!(
        value,
        RuntimeValue::InvokeValue(_)
            | RuntimeValue::InvokeRequest(_)
            | RuntimeValue::InvokeEvent(_)
    )
}

fn resource_values_from_server_result(
    kind: ProtocolResourceKind,
    result: ServerSelectResult,
) -> Option<Vec<RuntimeValue>> {
    let rows = result.into_rows().into_rows();
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let [value] = row.into_values().try_into().ok()?;
        if !resource_result_value_is_supported(&value) {
            return None;
        }
        values.push(value);
    }
    if kind == ProtocolResourceKind::Single && values.len() != 1 {
        return None;
    }
    Some(values)
}

fn classify_sealed_server_error(error: &PostgresKernelError) -> SealedInvocationFailureClass {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(source) => {
            SealedInvocationFailureClass::Target
        }
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_target_is_unavailable(source) =>
        {
            SealedInvocationFailureClass::Target
        }
        PostgresKernelError::ServerUpdate(source)
            if raw_server_update_target_is_unavailable(source) =>
        {
            SealedInvocationFailureClass::Target
        }
        PostgresKernelError::ServerDelete(source)
            if raw_server_delete_target_is_unavailable(source) =>
        {
            SealedInvocationFailureClass::Target
        }
        _ => SealedInvocationFailureClass::Internal,
    }
}

async fn execute_sealed_server_target(
    transaction: &mut Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    kind: ProtocolResourceKind,
) -> Result<Vec<RuntimeValue>, SealedInvocationFailureClass> {
    let savepoint = transaction
        .savepoint("sealed_server_execution")
        .await
        .map_err(|_| SealedInvocationFailureClass::Internal)?;
    let function = authorisation.target().function();
    let mutation = if raw_server_insert_target_is_selected(active, function) {
        Some(None)
    } else if arguments.len() == 2
        && raw_server_reference_value_update_target_is_selected(active, function)
    {
        Some(Some(RawServerReferenceMutation::Update))
    } else {
        raw_server_reference_mutation_target(active, function).map(Some)
    };
    let values = match mutation {
        Some(None) => {
            let result = if arguments.is_empty() {
                execute_authorised_raw_server_insert(&savepoint, active, authorisation).await
            } else {
                execute_authorised_raw_server_insert_with_arguments(
                    &savepoint,
                    active,
                    authorisation,
                    arguments,
                )
                .await
            };
            match result {
                Ok(value) => Some(vec![value]),
                Err(error) => {
                    let class = classify_sealed_server_error(&error);
                    if savepoint.rollback().await.is_err() {
                        return Err(SealedInvocationFailureClass::Internal);
                    }
                    return Err(class);
                }
            }
        }
        Some(Some(operation)) => {
            match execute_authorised_raw_server_reference_mutation(
                &savepoint,
                active,
                authorisation,
                operation,
                arguments,
            )
            .await
            {
                Ok(values) => Some(values),
                Err(error) => {
                    let class = classify_sealed_server_error(&error);
                    if savepoint.rollback().await.is_err() {
                        return Err(SealedInvocationFailureClass::Internal);
                    }
                    return Err(class);
                }
            }
        }
        None => {
            match execute_authorised_server_select(&savepoint, active, authorisation, arguments)
                .await
            {
                Ok(server) => resource_values_from_server_result(kind, server),
                Err(error) => {
                    let class = classify_sealed_server_error(&error);
                    if savepoint.rollback().await.is_err() {
                        return Err(SealedInvocationFailureClass::Internal);
                    }
                    return Err(class);
                }
            }
        }
    };
    let Some(values) =
        values.filter(|values| values.len() == 1 || kind == ProtocolResourceKind::Stream)
    else {
        if savepoint.rollback().await.is_err() {
            return Err(SealedInvocationFailureClass::Internal);
        }
        return Err(SealedInvocationFailureClass::Target);
    };
    if values
        .iter()
        .any(|value| !resource_result_value_is_supported(value))
    {
        if savepoint.rollback().await.is_err() {
            return Err(SealedInvocationFailureClass::Internal);
        }
        return Err(SealedInvocationFailureClass::Target);
    }
    savepoint
        .commit()
        .await
        .map_err(|_| SealedInvocationFailureClass::Internal)?;
    Ok(values)
}

async fn execute_sealed_server_after_audit(
    client: &mut tokio_postgres::Client,
    active: &ActiveDatabaseRevision,
    pinned_security: &SecuritySnapshot,
    registry: &OpaqueCodecRegistry,
    authenticated_session: &AuthenticatedSession,
    definition: &FunctionDefinition,
    decoded: &orna_core::invocation::InvokeRequest,
    security_target: InvocationTarget,
    authorisation: &AuthorisedInvocation,
    invocation: InvocationId,
) -> Result<SealedInvocationResult, PostgresKernelError> {
    let mut transaction = match client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
    {
        Ok(transaction) => transaction,
        Err(_) => {
            return sealed_failure_result(invocation, SealedInvocationFailureClass::Internal);
        }
    };
    if require_current_migrations(&transaction).await.is_err() {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    if lock_active_revision(&transaction, active.pair())
        .await
        .is_err()
    {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    let execution_active = match configure_and_recover(&transaction).await {
        Ok(active) => active,
        Err(_) => {
            return finish_sealed_failure(
                transaction,
                invocation,
                SealedInvocationFailureClass::Internal,
            )
            .await;
        }
    };
    if execution_active.pair() != active.pair() {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    let execution_security =
        match recover_security_snapshot_for_active(&transaction, &execution_active).await {
            Ok(security) => security,
            Err(_) => {
                return finish_sealed_failure(
                    transaction,
                    invocation,
                    SealedInvocationFailureClass::Internal,
                )
                .await;
            }
        };
    if !security_snapshots_match(&execution_security, pinned_security) {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    let arguments = match bind_sealed_invoke_arguments(definition, decoded.arguments()) {
        Ok(arguments) => arguments,
        Err(_) => {
            return finish_sealed_failure(
                transaction,
                invocation,
                SealedInvocationFailureClass::Bind,
            )
            .await;
        }
    };
    let kind = match sealed_server_result_kind(definition.return_type()) {
        Some(kind) => kind,
        None => {
            return finish_sealed_failure(
                transaction,
                invocation,
                SealedInvocationFailureClass::Target,
            )
            .await;
        }
    };
    let values = match execute_sealed_server_target(
        &mut transaction,
        active,
        authorisation,
        &arguments,
        kind,
    )
    .await
    {
        Ok(values) => values,
        Err(failure) => {
            return finish_sealed_failure(transaction, invocation, failure).await;
        }
    };
    let events = match decoded.output_requirement() {
        Some(requirement) => {
            if values.len() != 1 {
                return finish_sealed_failure(
                    transaction,
                    invocation,
                    SealedInvocationFailureClass::Target,
                )
                .await;
            }
            let value = values
                .into_iter()
                .next()
                .expect("one result value was checked");
            match present_sealed_standard_output(requirement, value, active, registry) {
                Ok(presented) => match sealed_completed_events(
                    authenticated_session.principal(),
                    invocation,
                    presented,
                ) {
                    Ok(events) => events,
                    Err(_) => {
                        return finish_sealed_failure(
                            transaction,
                            invocation,
                            SealedInvocationFailureClass::Target,
                        )
                        .await;
                    }
                },
                Err(
                    SealedPresentationError::OutputResolution(_) | SealedPresentationError::NoPath,
                ) => {
                    if transaction.commit().await.is_err() {
                        return sealed_failure_result(
                            invocation,
                            SealedInvocationFailureClass::Internal,
                        );
                    }
                    return Ok(SealedInvocationResult::PresentationFailed { invocation });
                }
                Err(SealedPresentationError::Kernel(_)) => {
                    return finish_sealed_failure(
                        transaction,
                        invocation,
                        SealedInvocationFailureClass::Internal,
                    )
                    .await;
                }
            }
        }
        None => match sealed_completed_events_from_values(
            authenticated_session.principal(),
            invocation,
            values,
        ) {
            Ok(events) => events,
            Err(_) => {
                return finish_sealed_failure(
                    transaction,
                    invocation,
                    SealedInvocationFailureClass::Target,
                )
                .await;
            }
        },
    };
    if capture_sealed_invocation_snapshot(
        &transaction,
        active,
        registry,
        authenticated_session,
        invocation,
        security_target.function(),
        &events,
        decoded.client_offer(),
        None,
    )
    .await
    .is_err()
    {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    if transaction.commit().await.is_err() {
        return sealed_failure_result(invocation, SealedInvocationFailureClass::Internal);
    }
    Ok(SealedInvocationResult::Completed { invocation, events })
}

async fn lock_catalogue_health_identity(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "LOCK TABLE _orna_kernel.security_principals,
                        _orna_kernel.security_local_peer_credentials
             IN SHARE ROW EXCLUSIVE MODE",
        )
        .await
        .map_err(PostgresKernelError::Database)
}

async fn insert_catalogue_health_identity(
    transaction: &Transaction<'_>,
    uid: u32,
) -> Result<(), PostgresKernelError> {
    let principal = CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_principals (id, kind, status)
             VALUES ($1, 'service', 'active')",
            &[&principal],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_local_peer_credentials (uid, principal_id)
             VALUES ($1, $2)",
            &[&i64::from(uid), &principal],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn append_client_capability_audit<T>(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
    execution: &Result<T, PostgresKernelError>,
) -> Result<(), PostgresKernelError> {
    match execution {
        Err(PostgresKernelError::ClientExecution(ClientExecutionError::CapabilityDenied {
            context,
            capability,
        })) => {
            let decision = SecurityAuditDecision::capability_denied(
                authenticated_session,
                InvocationTarget::new(context.function(), active.pair()),
                capability,
            )
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record: context.function().canonical(),
                rule: "capability denial names must be valid redacted names",
            })?;
            append_security_audit_event(transaction, decision).await?;
        }
        _ => {
            // A non-denial result means the stored capability gate passed.
            // Record that decision even when the local evaluator then returns
            // an external-contract or another execution error.
            for capability in stored_capability_names(active, target.function())? {
                let decision = SecurityAuditDecision::capability_allowed(
                    authenticated_session,
                    target,
                    capability,
                )
                .map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    record: target.function().canonical(),
                    rule: "stored capability names must be valid redacted names",
                })?;
                append_security_audit_event(transaction, decision).await?;
            }
        }
    }
    Ok(())
}

fn stored_capability_names(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<Vec<String>, PostgresKernelError> {
    let Some(definition) = active.catalogue().function_by_id(function) else {
        return Ok(Vec::new());
    };
    let Some(revision) = active
        .function_revisions()
        .iter()
        .find(|revision| revision.id() == definition.current_revision())
    else {
        return Ok(Vec::new());
    };
    if revision.artifact().version() != CAPABILITY_FORMAT_VERSION {
        return Ok(Vec::new());
    }
    let plan = CapabilityClientPlan::decode(revision.artifact().payload()).map_err(|_| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.function_revisions",
            record: revision.id().canonical(),
            rule: "a successfully evaluated capability artifact must decode",
        }
    })?;
    Ok(plan
        .requirements()
        .iter()
        .map(|requirement| requirement.name().to_owned())
        .collect())
}

fn require_catalogue_health_identity_preserved(
    current: &SecuritySnapshot,
    candidate: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    match catalogue_health_service_uid(current)? {
        None => {
            if snapshot_contains_catalogue_health_identity(candidate) {
                return Err(catalogue_health_identity_error(
                    "_orna_kernel.security_principals",
                    "the reserved catalogue health service identity must be installed through its fixed setup",
                ));
            }
            Ok(())
        }
        Some(uid) => require_catalogue_health_snapshot(candidate, uid),
    }
}

fn snapshot_contains_catalogue_health_identity(snapshot: &SecuritySnapshot) -> bool {
    snapshot
        .principals()
        .any(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
        || snapshot
            .local_peer_credentials()
            .any(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
}

pub(crate) fn security_snapshots_match(left: &SecuritySnapshot, right: &SecuritySnapshot) -> bool {
    left.revision() == right.revision()
        && left.functions().eq(right.functions())
        && left.principals().eq(right.principals())
        && left.memberships().eq(right.memberships())
        && left.execute_grants().eq(right.execute_grants())
        && left.privilege_grants().eq(right.privilege_grants())
        && left
            .local_peer_credentials()
            .eq(right.local_peer_credentials())
}

fn require_catalogue_health_snapshot(
    snapshot: &SecuritySnapshot,
    uid: u32,
) -> Result<(), PostgresKernelError> {
    let principal = snapshot
        .principals()
        .find(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    let credential = snapshot.local_peer_credentials().find(|credential| {
        credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID || credential.uid() == uid
    });
    if principal
        != Some(Principal::new(
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            PrincipalKind::Service,
            PrincipalStatus::Active,
        ))
        || credential
            != Some(LocalPeerCredential::new(
                uid,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            ))
    {
        return Err(catalogue_health_identity_error(
            "_orna_kernel.security_principals",
            "the reserved catalogue health service identity must be preserved",
        ));
    }
    Ok(())
}

fn catalogue_health_service_uid(
    snapshot: &SecuritySnapshot,
) -> Result<Option<u32>, PostgresKernelError> {
    let principal = snapshot
        .principals()
        .find(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    let credential = snapshot
        .local_peer_credentials()
        .find(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    match (principal, credential) {
        (None, None) => Ok(None),
        (Some(principal), Some(credential))
            if principal
                == Principal::new(
                    CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                    PrincipalKind::Service,
                    PrincipalStatus::Active,
                ) =>
        {
            Ok(Some(credential.uid()))
        }
        (Some(_), None) => Err(catalogue_health_identity_error(
            "_orna_kernel.security_local_peer_credentials",
            "the reserved catalogue health service identity must be complete",
        )),
        _ => Err(catalogue_health_identity_error(
            "_orna_kernel.security_principals",
            "the reserved catalogue health principal must be an active service",
        )),
    }
}

fn catalogue_health_identity_error(
    relation: &'static str,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation,
        record: CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.canonical(),
        rule,
    }
}

/// The closed result of raw record-argument preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordArgumentPreflight {
    /// The call contains no record argument and needs no PostgreSQL preflight.
    NotRequired,
    /// Every record is canonical for the transaction's active revision.
    Current,
    /// At least one record is stale or incompatible with the active revision.
    Stale,
}

#[cfg(feature = "test-hooks")]
struct RawDispatchTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(feature = "test-hooks")]
async fn pause_after_raw_dispatch_recovery(test_barrier: Option<&RawDispatchTestBarrier>) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
struct RawDispatchTestBarrier;

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_raw_dispatch_recovery(_test_barrier: Option<&RawDispatchTestBarrier>) {}

#[cfg(feature = "test-hooks")]
struct AuthenticatedSelectTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(feature = "test-hooks")]
async fn pause_after_authenticated_select_recovery(
    test_barrier: Option<&AuthenticatedSelectTestBarrier>,
) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
struct AuthenticatedSelectTestBarrier;

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_authenticated_select_recovery(
    _test_barrier: Option<&AuthenticatedSelectTestBarrier>,
) {
}

#[cfg(feature = "test-hooks")]
struct AuthenticatedResourceTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(feature = "test-hooks")]
async fn pause_after_authenticated_resource_validation(
    test_barrier: Option<&AuthenticatedResourceTestBarrier>,
) {
    if let Some(test_barrier) = test_barrier {
        test_barrier.reached.wait().await;
        test_barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
struct AuthenticatedResourceTestBarrier;

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_authenticated_resource_validation(
    _test_barrier: Option<&AuthenticatedResourceTestBarrier>,
) {
}

pub(crate) fn finish_security_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn finish_authenticated_server_select_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    shutdown?;
    operation
}

fn classify_raw_server_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Select(source),
            }
        }
        error => error,
    }
}

fn classify_raw_identity_selected_server_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            raw_call_target_unavailable(
                function,
                "raw identity-selected SERVER target is unavailable",
            )
        }
        error => error,
    }
}

fn classify_raw_unique_text_selected_server_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(&source) => {
            raw_call_target_unavailable(
                function,
                "raw unique-Text-selected SERVER target is unavailable",
            )
        }
        error => error,
    }
}

fn validate_raw_call_argument_shape(
    function: FunctionId,
    arguments: &[FunctionArgument],
) -> Result<(), PostgresKernelError> {
    match arguments {
        [] => Ok(()),
        [argument] if raw_call_argument_is_supported(argument) => Ok(()),
        [first, second]
            if raw_call_argument_is_supported(first) && raw_call_argument_is_supported(second) =>
        {
            Ok(())
        }
        _ => Err(raw_call_target_unavailable(
            function,
            "raw calls accept zero arguments, one supported value, or one supported argument pair",
        )),
    }
}

fn raw_call_argument_is_supported(argument: &FunctionArgument) -> bool {
    matches!(
        argument.value(),
        RuntimeValue::Boolean(_)
            | RuntimeValue::Integer(_)
            | RuntimeValue::BigInt(_)
            | RuntimeValue::Float(_)
            | RuntimeValue::Text(_)
            | RuntimeValue::Bytes(_)
            | RuntimeValue::Reference { .. }
    )
}

fn raw_call_target_unavailable(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::RawCallTargetUnavailable { function, rule }
}

fn classify_raw_server_insert_error(
    error: PostgresKernelError,
    arguments_present: bool,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_argument_target_is_unavailable(&source, arguments_present) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER INSERT argument target is unavailable",
            )
        }
        PostgresKernelError::ServerInsert(source) if arguments_present => {
            PostgresKernelError::ServerInsert(source)
        }
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_target_is_unavailable(&source) =>
        {
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Insert(source),
            }
        }
        error => error,
    }
}

fn classify_raw_server_reference_mutation_error(
    error: PostgresKernelError,
    function: FunctionId,
) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerUpdate(source)
            if raw_server_update_target_is_unavailable(&source) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER UPDATE reference target is unavailable",
            )
        }
        PostgresKernelError::ServerDelete(source)
            if raw_server_delete_target_is_unavailable(&source) =>
        {
            raw_call_target_unavailable(
                function,
                "raw SERVER DELETE reference target is unavailable",
            )
        }
        error => error,
    }
}

fn raw_server_insert_argument_target_is_unavailable(
    error: &ServerInsertError,
    arguments_present: bool,
) -> bool {
    match error {
        ServerInsertError::NotCommitted { source, .. } => {
            raw_server_insert_argument_target_is_unavailable(source, arguments_present)
        }
        ServerInsertError::Argument { .. } => true,
        ServerInsertError::FunctionNotActive { .. }
        | ServerInsertError::FunctionSignature { .. }
        | ServerInsertError::Artifact { .. }
        | ServerInsertError::PlanDecode(_)
        | ServerInsertError::PlanInvariant { .. }
        | ServerInsertError::ReferenceEvidence { .. }
        | ServerInsertError::ComplexityLimit { .. } => arguments_present,
        _ => false,
    }
}

pub(crate) async fn append_security_audit_event(
    transaction: &Transaction<'_>,
    decision: SecurityAuditDecision,
) -> Result<SecurityAuditEventId, PostgresKernelError> {
    let event = SecurityAuditEventId::new();
    let event_id = event.to_bytes().to_vec();
    let kind = encode_security_audit_kind(decision.kind());
    let outcome = match decision.outcome() {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    };
    let session_principal = decision
        .session_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let effective_principal = decision
        .effective_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let authorising_principal = decision
        .authorising_principal()
        .map(|principal| principal.to_bytes().to_vec());
    let (function, source_revision, catalogue_revision) =
        encode_security_audit_identity_columns(&decision);
    let denial_reason = match decision.denial() {
        None => decision
            .user_state_operation()
            .zip(decision.user_state_cell_count())
            .map(|(operation, cell_count)| encode_user_state_audit_detail(operation, cell_count))
            .or_else(|| {
                decision
                    .capability_name()
                    .map(encode_capability_audit_denial)
            })
            .or_else(|| {
                decision
                    .inspect_requested()
                    .zip(decision.inspect_epoch_scope())
                    .map(|(requested, scope)| encode_inspect_audit_detail(requested, scope))
            })
            .or_else(|| {
                decision
                    .security_admin_operation()
                    .map(encode_security_admin_audit_detail)
            })
            .or_else(|| {
                decision
                    .source_apply_candidate()
                    .map(|_| encode_source_apply_audit_detail().to_owned())
            }),
        Some(SecurityAuditDenial::Authentication(reason)) => {
            Some(encode_authentication_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::Execute(reason)) => {
            Some(encode_execute_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::Capability { capability }) => {
            Some(encode_capability_audit_denial(&capability))
        }
        Some(SecurityAuditDenial::Inspect(reason)) => {
            Some(encode_inspect_audit_denial(reason).to_owned())
        }
        Some(SecurityAuditDenial::SecurityAdmin(reason)) => {
            encode_security_admin_audit_denied_detail(&decision, reason)
        }
    };
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_audit_events
                 (event_id, event_kind, outcome, session_principal_id,
                  effective_principal_id, authorising_principal_id, function_id,
                  source_revision_id, catalogue_revision_id, denial_reason)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &event_id,
                &kind,
                &outcome,
                &session_principal,
                &effective_principal,
                &authorising_principal,
                &function,
                &source_revision,
                &catalogue_revision,
                &denial_reason,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(event)
}

/// Appends one closed invocation decision in the caller's protected transaction.
///
/// PostgreSQL generates the relation sequence and recording time. The caller
/// cannot supply Request, bind, lifecycle, delivery, or diagnostic data.
pub(crate) async fn append_invocation_audit_event(
    transaction: &Transaction<'_>,
    decision: InvocationAuditDecision,
) -> Result<InvocationAuditEventId, PostgresKernelError> {
    let record = decision.invocation.canonical();
    validate_invocation_audit_decision_shape(&decision, &record)?;
    let security_events = load_security_audit_events(transaction).await?;
    validate_invocation_audit_evidence(&decision, &security_events, &record)?;
    if let Some(target) = decision.target {
        require_invocation_audit_target(transaction, target, &record).await?;
    }

    let event_id = InvocationAuditEventId::new();
    let event_id_bytes = event_id.to_bytes().to_vec();
    let invocation_id = decision.invocation.to_bytes().to_vec();
    let outcome = encode_invocation_audit_outcome(decision.outcome);
    let session_principal = decision.session_principal.to_bytes().to_vec();
    let effective_principal = decision
        .effective_principal
        .map(|principal| principal.to_bytes().to_vec());
    let authorising_principal = decision
        .authorising_principal
        .map(|principal| principal.to_bytes().to_vec());
    let (function, source_revision, catalogue_revision) = decision
        .target
        .map(|target| {
            (
                Some(target.function().to_bytes().to_vec()),
                Some(target.revision().source().to_bytes().to_vec()),
                Some(target.revision().catalogue().to_bytes().to_vec()),
            )
        })
        .unwrap_or((None, None, None));
    let security_audit_event = decision
        .security_audit_event
        .map(|event| event.to_bytes().to_vec());
    transaction
        .execute(
            "INSERT INTO _orna_kernel.invocation_audit_events
                 (event_id, invocation_id, outcome, session_principal_id,
                  effective_principal_id, authorising_principal_id, function_id,
                  source_revision_id, catalogue_revision_id, security_audit_event_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &event_id_bytes,
                &invocation_id,
                &outcome,
                &session_principal,
                &effective_principal,
                &authorising_principal,
                &function,
                &source_revision,
                &catalogue_revision,
                &security_audit_event,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(event_id)
}
/// The terminal state retained for one authenticated resource request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceAuditTerminalOutcome {
    /// The target returned its complete bounded result.
    Completed,
    /// The request was denied or execution failed before a result completed.
    Failed,
    /// Cancellation won before a terminal result was committed.
    Cancelled,
}

/// Appends one redacted resource terminal row in the caller's transaction.
///
/// This helper deliberately accepts only identity, decision, terminal, target,
/// and bounded count metadata. Arguments and returned values never cross this
/// boundary. A target is retained only when the caller has recovered an exact
/// active target; unresolved or stale targets must pass None.
pub(crate) async fn append_resource_audit_event(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    nested_invocation_id: InvocationId,
    decision: SecurityAuditOutcome,
    terminal: ResourceAuditTerminalOutcome,
    target: Option<InvocationTarget>,
    item_count: Option<u64>,
    byte_count: Option<u64>,
) -> Result<(), PostgresKernelError> {
    validate_resource_state_context(request)?;
    let item_count = item_count
        .map(|count| {
            i64::try_from(count).map_err(|_| {
                resource_audit_invariant(
                    &request.request_id.canonical(),
                    "resource item count must fit a signed 64-bit database count",
                )
            })
        })
        .transpose()?;
    let byte_count = byte_count
        .map(|count| {
            i64::try_from(count).map_err(|_| {
                resource_audit_invariant(
                    &request.request_id.canonical(),
                    "resource byte count must fit a signed 64-bit database count",
                )
            })
        })
        .transpose()?;
    let event_id = InvocationAuditEventId::new();
    let event_id_bytes = event_id.to_bytes().to_vec();
    let request_id = request.request_id.to_bytes().to_vec();
    let nested_invocation_id_bytes = nested_invocation_id.to_bytes().to_vec();
    let parent_invocation_id = request.parent_invocation_id.to_bytes().to_vec();
    let call_site_id = request.call_site_id.to_bytes().to_vec();
    let session_principal = authenticated_session.principal().to_bytes().to_vec();
    let (target_function, source_revision, catalogue_revision) = target
        .map(|target| {
            (
                Some(target.function().to_bytes().to_vec()),
                Some(target.revision().source().to_bytes().to_vec()),
                Some(target.revision().catalogue().to_bytes().to_vec()),
            )
        })
        .unwrap_or((None, None, None));
    let decision = encode_resource_audit_decision(decision);
    let terminal = encode_resource_audit_terminal(terminal);
    transaction
        .execute(
            "INSERT INTO _orna_kernel.resource_audit_events
                 (event_id, request_id, nested_invocation_id, parent_invocation_id,
                  call_site_id, target_function_id, source_revision_id,
                  catalogue_revision_id, session_principal_id, decision_outcome,
                  terminal_outcome, item_count, byte_count)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            &[
                &event_id_bytes,
                &request_id,
                &nested_invocation_id_bytes,
                &parent_invocation_id,
                &call_site_id,
                &target_function,
                &source_revision,
                &catalogue_revision,
                &session_principal,
                &decision,
                &terminal,
                &item_count,
                &byte_count,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

fn encode_resource_audit_decision(decision: SecurityAuditOutcome) -> &'static str {
    match decision {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    }
}

fn encode_resource_audit_terminal(terminal: ResourceAuditTerminalOutcome) -> &'static str {
    match terminal {
        ResourceAuditTerminalOutcome::Completed => "completed",
        ResourceAuditTerminalOutcome::Failed => "failed",
        ResourceAuditTerminalOutcome::Cancelled => "cancelled",
    }
}

fn validate_resource_state_context(request: &ResourceRequest) -> Result<(), PostgresKernelError> {
    ClientStateContext::new(
        request.target_function_id,
        request.state_profile.clone(),
        request.function_instance_key.clone(),
    )
    .map(|_| ())
    .map_err(|_| PostgresKernelError::DurableInvariant {
        relation: "resource request",
        record: request.request_id.canonical(),
        rule: "resource state context must contain valid text",
    })
}

fn resource_audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.resource_audit_events",
        record: record.to_owned(),
        rule,
    }
}

/// One privately resolved sealed invocation target for the PostgreSQL kernel.
///
/// This mirrors the closed resolution inside `orna-core` so the durable audit
/// and execution steps can re-derive the exact pinned target without exposing
/// any resolution phase. An application target carries no executable pin; a
/// verified-standard target carries the exact executable and standard
/// revisions of the pinned snapshot. A system target carries only its sealed
/// registry definition.
#[derive(Clone, Copy)]
enum SealedResolvedTarget<'a> {
    Application(&'a FunctionDefinition),
    System(SystemFunctionDefinition),
    VerifiedStandard {
        definition: &'a FunctionDefinition,
        executable: &'a StandardExecutable,
    },
}
pub(crate) fn is_admitted_security_identity(definition: SystemFunctionDefinition) -> bool {
    let id = definition.id();
    matches!(
        id,
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
            | SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID
            | SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID
    ) && matches!(definition.kind(), SystemFunctionKind::SecurityIdentity)
        && definition.security_signature().is_some_and(|signature| {
            signature.parameter_count() == 0
                && signature.returns_ref_principal()
                && !signature.returns_boolean()
                && signature.stream_item_type().is_none()
                && match id {
                    SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID => signature.returns_set(),
                    SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID
                    | SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID => !signature.returns_set(),
                    _ => false,
                }
        })
}

/// One canonical resource target resolved against the active application and
/// verified-standard catalogues. Standard targets retain both immutable pins.
enum ResolvedResourceTarget<'a> {
    Application {
        definition: &'a FunctionDefinition,
        target: InvocationTarget,
    },
    VerifiedStandard {
        definition: &'a FunctionDefinition,
        executable: &'a StandardExecutable,
        target: InvocationTarget,
    },
}

impl<'a> ResolvedResourceTarget<'a> {
    fn target(&self) -> InvocationTarget {
        match self {
            Self::Application { target, .. } | Self::VerifiedStandard { target, .. } => *target,
        }
    }

    fn definition(&self) -> &'a FunctionDefinition {
        match self {
            Self::Application { definition, .. } | Self::VerifiedStandard { definition, .. } => {
                definition
            }
        }
    }

    fn executable(&self) -> Option<&'a StandardExecutable> {
        match self {
            Self::Application { .. } => None,
            Self::VerifiedStandard { executable, .. } => Some(executable),
        }
    }
}

/// Resolves one SERVER resource function to its closed security target.
///
/// A function present in both the active application and exact standard
/// catalogues is ambiguous. A standard function must have exactly one
/// executable whose immutable revision matches the catalogue definition;
/// missing, duplicate, stale, or otherwise unpinned evidence resolves to no
/// target and therefore cannot reach authorise_execute.
fn resolve_resource_target<'a>(
    active: &'a ActiveDatabaseRevision,
    function: FunctionId,
) -> Option<ResolvedResourceTarget<'a>> {
    resolve_resource_target_in_catalogues(
        active.pair(),
        active.catalogue(),
        active.catalogue_hash_context().standard(),
        function,
    )
}

fn resolve_resource_target_in_catalogues<'a>(
    pair: RevisionPair,
    application_catalogue: &'a orna_core::catalogue::CatalogueSnapshot,
    standard: Option<&'a orna_core::revision::VerifiedStandardLibrarySnapshot>,
    function: FunctionId,
) -> Option<ResolvedResourceTarget<'a>> {
    let application = application_catalogue.function_by_id(function);
    let standard_definition =
        standard.and_then(|snapshot| snapshot.catalogue().function_by_id(function));

    match (application, standard_definition) {
        (Some(_), Some(_)) | (None, None) => None,
        (Some(definition), None) => Some(ResolvedResourceTarget::Application {
            definition,
            target: InvocationTarget::new(function, pair),
        }),
        (None, Some(definition)) => {
            let snapshot = standard?;
            let mut executables = snapshot
                .executables()
                .iter()
                .filter(|executable| executable.function() == function);
            let executable = executables.next()?;
            if executables.next().is_some()
                || executable.revision().function() != function
                || executable.revision().id() != definition.current_revision()
            {
                return None;
            }
            Some(ResolvedResourceTarget::VerifiedStandard {
                definition,
                executable,
                target: InvocationTarget::verified_standard(
                    function,
                    pair,
                    snapshot.revision(),
                    executable.revision().id(),
                ),
            })
        }
    }
}

/// Resolves one sealed request target in the pinned application and
/// verified-standard catalogues, mirroring the private core resolution.
///
/// A function present in both catalogues is ambiguous and resolves to
/// neither, exactly as at the protected boundary. A verified-standard target
/// resolves only when its executable pin matches the snapshot's current
/// function revision.
fn resolve_sealed_target<'a>(
    active: &'a ActiveDatabaseRevision,
    selector: &InvocationRequestTarget,
) -> Option<SealedResolvedTarget<'a>> {
    let system_target = match selector {
        InvocationRequestTarget::FunctionId(id) => system_function_by_id(*id),
        InvocationRequestTarget::QualifiedName(name) => system_function_by_name(name),
        _ => None,
    };
    if let Some(definition) = system_target {
        return is_admitted_security_identity(definition)
            .then_some(SealedResolvedTarget::System(definition));
    }
    let application = active.catalogue();
    let standard = active.catalogue_hash_context().standard();
    let application_target = match selector {
        InvocationRequestTarget::FunctionId(id) => application.function_by_id(*id),
        InvocationRequestTarget::QualifiedName(name) => application.function_by_name(name),
        _ => None,
    };
    let standard_target = standard.and_then(|snapshot| match selector {
        InvocationRequestTarget::FunctionId(id) => snapshot.catalogue().function_by_id(*id),
        InvocationRequestTarget::QualifiedName(name) => snapshot.catalogue().function_by_name(name),
        _ => None,
    });
    match (application_target, standard_target) {
        (Some(_), Some(_)) | (None, None) => None,
        (Some(definition), None) => Some(SealedResolvedTarget::Application(definition)),
        (None, Some(definition)) => {
            let snapshot = standard.expect("a standard target requires the pinned snapshot");
            let executable = snapshot
                .executables()
                .iter()
                .find(|executable| executable.function() == definition.id())?;
            if executable.revision().id() != definition.current_revision() {
                return None;
            }
            Some(SealedResolvedTarget::VerifiedStandard {
                definition,
                executable,
            })
        }
    }
}

/// Returns the closed two-class security target for one privately resolved
/// sealed target.
fn sealed_security_target(
    active: &ActiveDatabaseRevision,
    target: SealedResolvedTarget<'_>,
) -> InvocationTarget {
    match target {
        SealedResolvedTarget::Application(definition) => {
            InvocationTarget::new(definition.id(), active.pair())
        }
        SealedResolvedTarget::System(definition) => {
            InvocationTarget::new(definition.id(), active.pair())
        }
        SealedResolvedTarget::VerifiedStandard {
            definition,
            executable,
        } => {
            let standard = active
                .catalogue_hash_context()
                .standard()
                .expect("a verified-standard target requires the pinned snapshot");
            InvocationTarget::verified_standard(
                definition.id(),
                active.pair(),
                standard.revision(),
                executable.revision().id(),
            )
        }
    }
}
fn authorise_sealed_target(
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
    target: InvocationTarget,
) -> ExecuteDecision {
    if system_function_by_id(target.function()).is_some_and(is_admitted_security_identity) {
        security.authorise_system_function(authenticated_session, target)
    } else {
        security.authorise_execute(authenticated_session, target)
    }
}

fn sealed_target_invariant(
    active: &ActiveDatabaseRevision,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "active catalogue",
        record: active.pair().catalogue().canonical(),
        rule,
    }
}

/// Binds one sealed request's checked arguments to the pinned function.
///
/// The protected decision already ran private prebind, so every selector
/// resolves and every value matches the declared type. This step re-checks
/// the boundary and constructs the typed arguments the closed engine accepts.
fn bind_sealed_invoke_arguments(
    definition: &FunctionDefinition,
    arguments: &[InvocationArgument],
) -> Result<Vec<FunctionArgument>, PostgresKernelError> {
    let mut bound = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let parameter = match argument.selector() {
            InvocationParameterSelector::ParameterId(id) => definition.parameter_by_id(*id),
            InvocationParameterSelector::Name(name) => definition.parameter_by_name(name),
            _ => None,
        };
        let Some(parameter) = parameter else {
            return Err(PostgresKernelError::ServerSelect(
                ServerSelectError::Argument {
                    parameter: None,
                    rule: "sealed invocation argument selector must resolve to a pinned parameter",
                },
            ));
        };
        let value = argument.value().clone().into_value();
        let argument = FunctionArgument::new(parameter.id(), value).map_err(|_| {
            PostgresKernelError::ServerSelect(ServerSelectError::Argument {
                parameter: Some(parameter.id()),
                rule: "sealed invocation argument must be one non-null typed value",
            })
        })?;
        bound.push(argument);
    }
    Ok(bound)
}

/// Builds the exact sealed Event sequence for one completed invocation.
///
/// The batch carries `InvocationStarted(0)`, an optional non-empty
/// `ValueBatch(1)`, and `InvocationCompleted` as one contiguous outer record
/// sequence. A server adapter delivers this batch on the `RESULT_VALUES`
/// channel and then completes the call.
///
/// ADR 0057 step 7 passes either the canonical echo value (no output
/// requirement) or the presented opaque value in the final `ValueBatch`.
pub(crate) fn sealed_completed_events(
    _principal: PrincipalId,
    invocation: InvocationId,
    value: RuntimeValue,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    sealed_completed_events_from_values(_principal, invocation, vec![value])
}

/// Builds completed events from validated SERVER result values.
fn sealed_completed_events_from_values(
    _principal: PrincipalId,
    invocation: InvocationId,
    values: Vec<RuntimeValue>,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    let mut records = vec![InvocationEventRecord::new(1, started)];
    let mut sequence = 1;
    if !values.is_empty() {
        let values = values
            .into_iter()
            .map(|value| InvokeValue::new(value).map_err(PostgresKernelError::InvocationCarrier))
            .collect::<Result<Vec<_>, _>>()?;
        let batch = InvokeEvent::new(
            invocation,
            sequence,
            InvocationEventBody::value_batch(None, values)
                .map_err(PostgresKernelError::InvocationCarrier)?,
        )
        .map_err(PostgresKernelError::InvocationCarrier)?;
        records.push(InvocationEventRecord::new(2, batch));
        sequence += 1;
    }
    let completed = InvokeEvent::new(
        invocation,
        sequence,
        InvocationEventBody::Completed {
            duration_nanoseconds: 0,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    records.push(InvocationEventRecord::new(sequence + 1, completed));
    InvocationEventBatch::new(records).map_err(PostgresKernelError::SealedInvocation)
}

fn sealed_failure_events(
    invocation: InvocationId,
    failure: SealedInvocationFailureClass,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    let (phase, code, message, retryability) = match failure {
        SealedInvocationFailureClass::Bind => (
            InvocationFailurePhase::Bind,
            "INVOKE_BIND_FAILED",
            "invocation arguments were not accepted",
            InvocationRetryability::No,
        ),
        SealedInvocationFailureClass::Target => (
            InvocationFailurePhase::Target,
            "INVOKE_TARGET_FAILED",
            "invocation target failed",
            InvocationRetryability::Unknown,
        ),
        SealedInvocationFailureClass::Internal => (
            InvocationFailurePhase::Internal,
            "INVOKE_INTERNAL_FAILURE",
            "invocation could not complete",
            InvocationRetryability::Unknown,
        ),
    };
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    let failure = InvocationFailure::new(phase, code, message, None, retryability)
        .map_err(PostgresKernelError::InvocationCarrier)?;
    let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
        .map_err(PostgresKernelError::InvocationCarrier)?;
    InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, failed),
    ])
    .map_err(PostgresKernelError::SealedInvocation)
}

fn sealed_failure_result(
    invocation: InvocationId,
    failure: SealedInvocationFailureClass,
) -> Result<SealedInvocationResult, PostgresKernelError> {
    let events = sealed_failure_events(invocation, failure)?;
    Ok(SealedInvocationResult::Failed { invocation, events })
}

async fn finish_sealed_failure(
    transaction: Transaction<'_>,
    invocation: InvocationId,
    failure: SealedInvocationFailureClass,
) -> Result<SealedInvocationResult, PostgresKernelError> {
    let events = sealed_failure_events(invocation, failure)?;
    let _ = transaction.rollback().await;
    Ok(SealedInvocationResult::Failed { invocation, events })
}

/// Captures one inspection epoch and its trace rows for a completed sealed
/// invocation in the caller's protected transaction.
///
/// ADR 0064 wires capture into the sealed dispatch: after the protected
/// decision and before/at execution, the produced Event batch becomes the
/// durable trace rows and one immutable snapshot epoch. v1 retains typed
/// state values in the protected epoch so a later `INSPECT VALUES` projection
/// can reveal them without reading mutable USER state; the projection still
/// redacts them unless the classifier is granted. Denied, bind-failed, and
/// presentation-failed invocations produce no Event batch and therefore no
/// epoch.
#[allow(clippy::too_many_arguments)]
async fn capture_sealed_invocation_snapshot(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    authenticated_session: &AuthenticatedSession,
    invocation: InvocationId,
    root_target: FunctionId,
    events: &InvocationEventBatch,
    client_offer: &InvocationClientOffer,
    loaded_user_state_cells: Option<&[UserStateCell]>,
) -> Result<InspectEpochId, PostgresKernelError> {
    crate::inspect::capture_inspect_snapshot_in_transaction(
        transaction,
        active,
        registry,
        authenticated_session,
        invocation,
        // Retain typed values in the immutable epoch. The installed
        // projection applies the independent `Values` classifier.
        InspectSnapshotOptions::new(true, false, false, false),
        authenticated_session.principal(),
        root_target,
        InspectOutcomeKind::Allowed,
        events,
        client_offer,
        None,
        loaded_user_state_cells,
    )
    .await
}

/// Appends the allowed `EXECUTE` security evidence and the linked allowed
/// invocation decision for one protected sealed decision.
///
/// The protected decision already allowed the invocation. This step re-runs
/// the pure authorisation to obtain the immutable decision evidence, appends
/// it, and links the invocation-audit row to that exact evidence.
async fn append_allowed_invocation_audit(
    transaction: &Transaction<'_>,
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
    target: InvocationTarget,
    invocation: InvocationId,
) -> Result<(), PostgresKernelError> {
    let authorisation = match authorise_sealed_target(security, authenticated_session, target) {
        ExecuteDecision::Allowed(authorisation) => authorisation,
        ExecuteDecision::Denied(_) => {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "active security snapshot",
                record: target.function().canonical(),
                rule: "allowed sealed invocation must re-authorise its pinned target",
            });
        }
    };
    let event_id = append_security_audit_event(
        transaction,
        SecurityAuditDecision::execute_allowed(&authorisation),
    )
    .await?;
    append_linked_invocation_audit(transaction, invocation, event_id).await
}

async fn append_allowed_invocation_audit_evidence(
    transaction: &Transaction<'_>,
    authorisation: &AuthorisedInvocation,
    invocation: InvocationId,
) -> Result<(), PostgresKernelError> {
    let event_id = append_security_audit_event(
        transaction,
        SecurityAuditDecision::execute_allowed(authorisation),
    )
    .await?;
    append_linked_invocation_audit(transaction, invocation, event_id).await
}

/// Appends the denied `EXECUTE` evidence and linked denied invocation
/// decision for one sealed target-level denial.
///
/// When the private denial reason cannot be re-derived without disclosing a
/// protected fact, only the closed unresolved invocation decision is
/// appended.
async fn append_sealed_denied_audit(
    transaction: &Transaction<'_>,
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
    active: &ActiveDatabaseRevision,
    selector: &InvocationRequestTarget,
    invocation: InvocationId,
) -> Result<(), PostgresKernelError> {
    let Some(target) = resolve_sealed_target(active, selector) else {
        return append_unresolved_invocation_audit(transaction, authenticated_session, invocation)
            .await;
    };
    let security_target = sealed_security_target(active, target);
    let reason = match authorise_sealed_target(security, authenticated_session, security_target) {
        ExecuteDecision::Denied(reason) => reason,
        ExecuteDecision::Allowed(_) => {
            // The protected denial came from a private rule that must not
            // disclose a target fact. Record the closed unresolved denial.
            return append_unresolved_invocation_audit(
                transaction,
                authenticated_session,
                invocation,
            )
            .await;
        }
    };
    let event_id = append_security_audit_event(
        transaction,
        SecurityAuditDecision::execute_denied(authenticated_session, security_target, reason),
    )
    .await?;
    append_linked_invocation_audit(transaction, invocation, event_id).await
}

/// Loads one appended security audit event and links it to the invocation
/// decision through the closed `EXECUTE` evidence contract.
async fn append_linked_invocation_audit(
    transaction: &Transaction<'_>,
    invocation: InvocationId,
    event_id: SecurityAuditEventId,
) -> Result<(), PostgresKernelError> {
    let events = load_security_audit_events(transaction).await?;
    let event = events
        .iter()
        .find(|event| event.id() == event_id)
        .ok_or_else(|| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_audit_events",
            record: event_id.canonical(),
            rule: "appended security audit evidence must recover in the same transaction",
        })?;
    append_invocation_audit_event(
        transaction,
        InvocationAuditDecision::from_execute_evidence(invocation, event)?,
    )
    .await?;
    Ok(())
}

/// Appends the closed unresolved denied invocation decision for one sealed
/// request that never reached a durable target.
async fn append_unresolved_invocation_audit(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    invocation: InvocationId,
) -> Result<(), PostgresKernelError> {
    append_invocation_audit_event(
        transaction,
        InvocationAuditDecision::unresolved_denied(invocation, authenticated_session.principal()),
    )
    .await?;
    Ok(())
}

/// Acquires the exclusive active-revision lock for transaction-bound execution.
///
/// Active-pointer writers use the same lock before replacement, so the caller
/// keeps its revision and security snapshot stable while the transaction is
/// open.
pub(crate) async fn lock_active_revision(
    transaction: &Transaction<'_>,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    validate_locked_active_revision(&row, expected)
}

/// Acquires a shared active-revision lock for one accepted resource producer.
///
/// Multiple resource producers can validate the same pinned revision together.
/// Active-pointer writers acquire `FOR UPDATE`, which conflicts with this lock
/// and waits until every producer releases its terminal transaction.
async fn lock_active_revision_for_resource(
    transaction: &Transaction<'_>,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_one(
            "SELECT source_revision_id, catalogue_revision_id
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR KEY SHARE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    validate_locked_active_revision(&row, expected)
}

fn validate_locked_active_revision(
    row: &Row,
    expected: RevisionPair,
) -> Result<(), PostgresKernelError> {
    let active = RevisionPair::new(
        orna_core::SourceRevisionId::from_bytes(exact_id(
            row,
            "source_revision_id",
            "active source revision is not exactly 16 bytes",
        )?),
        orna_core::CatalogueRevisionId::from_bytes(exact_id(
            row,
            "catalogue_revision_id",
            "active catalogue revision is not exactly 16 bytes",
        )?),
    );
    if expected != active {
        return Err(PostgresKernelError::SecurityRevisionMismatch { expected, active });
    }
    Ok(())
}

pub(crate) fn require_complete_function_set(
    active: &ActiveDatabaseRevision,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    if security_function_targets(active) != snapshot.functions().collect::<Vec<_>>() {
        return Err(PostgresKernelError::SecurityFunctionSetMismatch);
    }
    Ok(())
}

/// Returns the exact non-system `EXECUTE` target universe for one active revision.
///
/// The application catalogue and its pinned verified standard snapshot are both
/// identity authorities. The standard side is empty until standard functions
/// are admitted, but it remains part of this one ordered target set.
fn security_function_targets(active: &ActiveDatabaseRevision) -> Vec<FunctionId> {
    let mut functions = active
        .catalogue()
        .functions()
        .iter()
        .map(|function| function.id())
        .collect::<Vec<_>>();
    if let Some(standard) = active.catalogue_hash_context().standard() {
        functions.extend(
            standard
                .catalogue()
                .functions()
                .iter()
                .map(|function| function.id()),
        );
    }
    functions.retain(|function| system_function_by_id(*function).is_none());
    functions.sort_unstable();
    functions
}

async fn replace_security_rows(
    transaction: &Transaction<'_>,
    snapshot: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "DELETE FROM _orna_kernel.security_local_peer_credentials;
             DELETE FROM _orna_kernel.security_execute_grants;
             DELETE FROM _orna_kernel.security_privilege_grants;
             DELETE FROM _orna_kernel.security_role_memberships;
             DELETE FROM _orna_kernel.security_principals;",
        )
        .await
        .map_err(PostgresKernelError::Database)?;

    for principal in snapshot.principals() {
        let id = principal.id().to_bytes().to_vec();
        let kind = encode_principal_kind(principal.kind());
        let status = encode_principal_status(principal.status());
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                 VALUES ($1, $2, $3)",
                &[&id, &kind, &status],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for credential in snapshot.local_peer_credentials() {
        let uid = i64::from(credential.uid());
        let principal = credential.principal().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_local_peer_credentials (uid, principal_id)
                 VALUES ($1, $2)",
                &[&uid, &principal],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for membership in snapshot.memberships() {
        let role = membership.role().to_bytes().to_vec();
        let member = membership.member().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_role_memberships (role_id, member_id)
                 VALUES ($1, $2)",
                &[&role, &member],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for grant in snapshot.execute_grants() {
        let grantee = grant.grantee().to_bytes().to_vec();
        let function = grant.function().to_bytes().to_vec();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
                 VALUES ($1, $2)",
                &[&grantee, &function],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    for grant in snapshot.privilege_grants() {
        let grantee = grant.grantee().to_bytes().to_vec();
        let class = encode_privilege_class(grant.class());
        let object = grant
            .object()
            .map(|function| function.to_bytes().to_vec())
            .unwrap_or_default();
        transaction
            .execute(
                "INSERT INTO _orna_kernel.security_privilege_grants
                     (grantee_id, privilege_class, object_id)
                 VALUES ($1, $2, $3)",
                &[&grantee, &class, &object],
            )
            .await
            .map_err(PostgresKernelError::Database)?;
    }
    Ok(())
}

async fn insert_execute_grant_if_absent(
    transaction: &Transaction<'_>,
    grant: ExecuteGrant,
) -> Result<(), PostgresKernelError> {
    let grantee = grant.grantee().to_bytes().to_vec();
    let function = grant.function().to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_execute_grants (grantee_id, function_id)
             VALUES ($1, $2)
             ON CONFLICT (grantee_id, function_id) DO NOTHING",
            &[&grantee, &function],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

async fn recover_security_snapshot(
    transaction: &Transaction<'_>,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let active = recover_active_revision(transaction).await?;
    recover_security_snapshot_for_active(transaction, &active).await
}

pub(crate) async fn recover_security_snapshot_for_active(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let function_targets = load_invocation_target_authorities(transaction, active).await?;
    let principals = load_principals(transaction).await?;
    let memberships = load_memberships(transaction).await?;
    let grants = load_grants(transaction).await?;
    let privilege_grants = load_privilege_grants(transaction).await?;
    let local_peer_credentials = load_local_peer_credentials(transaction).await?;

    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        active.pair(),
        function_targets,
        principals,
        memberships,
        grants,
        local_peer_credentials,
        privilege_grants,
    )
    .map_err(PostgresKernelError::SecuritySnapshot)
}

/// Loads the closed two-class `EXECUTE` target union for the active catalogue
/// revision from the durable target-authority relation.
///
/// Apply is the only writer of this relation, so every application and
/// standard row carries its exact pinned executable revision. Sealed system
/// identity rows are audit anchors only and do not enter the in-memory
/// application/standard target set.
///
/// Recovery validates the standard rows against the already-verified active
/// standard snapshot and fails closed on any absent, duplicated, mismatched,
/// or unverified standard target.
async fn load_invocation_target_authorities(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
) -> Result<Vec<SecurityFunctionTarget>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.invocation_target_authorities";
    let admitted_system_identities = [
        SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_ACTIVE_ROLES_FUNCTION_ID,
    ];
    let mut seen_system_identities = [false; 3];
    let catalogue = active.pair().catalogue().to_bytes().to_vec();
    let rows = transaction
        .query(
            "SELECT authority.function_id AS function_id,
                    authority.target_class AS target_class,
                    authority.function_revision_id AS function_revision_id,
                    authority.standard_library_revision_id AS standard_library_revision_id,
                    function.current_function_revision_id AS catalogue_current_revision_id
             FROM _orna_kernel.invocation_target_authorities AS authority
             LEFT JOIN _orna_kernel.catalogue_functions AS function
               ON function.catalogue_revision_id = authority.catalogue_revision_id
              AND function.function_id = authority.function_id
             WHERE authority.catalogue_revision_id = $1
             ORDER BY authority.function_id",
            &[&catalogue],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut targets = Vec::with_capacity(rows.len());
    for row in &rows {
        let function = FunctionId::from_bytes(exact_id(
            row,
            "function_id",
            "invocation target function identity is not exactly 16 bytes",
        )?);
        let executable = FunctionRevisionId::from_bytes(exact_id(
            row,
            "function_revision_id",
            "invocation target function revision identity is not exactly 16 bytes",
        )?);
        let class: String = row
            .try_get("target_class")
            .map_err(|source| row_decode(RELATION, function.canonical(), "target_class", source))?;
        let standard: Option<Vec<u8>> =
            row.try_get("standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        RELATION,
                        function.canonical(),
                        "standard_library_revision_id",
                        source,
                    )
                })?;
        let catalogue_revision: Option<Vec<u8>> = row
            .try_get("catalogue_current_revision_id")
            .map_err(|source| {
                row_decode(
                    RELATION,
                    function.canonical(),
                    "catalogue_current_revision_id",
                    source,
                )
            })?;
        let missing = || PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: function.canonical(),
            rule: "application invocation targets must resolve in the pinned application catalogue",
        };
        match class.as_str() {
            "application" => {
                if catalogue_revision.as_deref() != Some(executable.to_bytes().as_slice()) {
                    return Err(missing());
                }
                targets.push(SecurityFunctionTarget::application(function));
            }
            "standard" => {
                if catalogue_revision.is_some() {
                    // The same function identity cannot be both an application
                    // catalogue function and a standard executable target.
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "standard invocation targets must not duplicate an application catalogue function",
                    });
                }
                let bytes = standard.ok_or_else(|| PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: function.canonical(),
                    rule: "standard invocation target must pin an exact standard library revision",
                })?;
                let standard_revision = StandardLibraryRevisionId::from_bytes(
                    bytes.try_into().map_err(|_| PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "standard invocation target standard revision identity is not exactly 16 bytes",
                    })?,
                );
                targets.push(SecurityFunctionTarget::verified_standard(
                    function,
                    standard_revision,
                    executable,
                ));
            }
            "system" => {
                if catalogue_revision.is_some()
                    || standard.is_some()
                    || executable.to_bytes() != function.to_bytes()
                    || !system_function_by_id(function).is_some_and(is_admitted_security_identity)
                {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "system invocation targets must be sealed audit anchors",
                    });
                }
                let Some(index) = admitted_system_identities
                    .iter()
                    .position(|identity| *identity == function)
                else {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "system invocation targets must use exactly the admitted sealed identities",
                    });
                };
                if seen_system_identities[index] {
                    return Err(PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: function.canonical(),
                        rule: "system invocation targets must contain each admitted sealed identity exactly once",
                    });
                }
                seen_system_identities[index] = true;
            }
            _ => {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: function.canonical(),
                    rule: "invocation target class must be application, standard, or system",
                });
            }
        }
    }
    if seen_system_identities.iter().any(|seen| !seen) {
        return Err(PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: active.pair().catalogue().canonical(),
            rule: "system invocation targets must contain exactly the three admitted sealed identities",
        });
    }
    require_authorised_standard_targets(active, &targets)?;
    Ok(targets)
}

/// Fails closed unless every standard target resolves exactly once in the
/// exact verified standard snapshot pinned by the active application revision.
///
/// The active catalogue hash context already verified one standard snapshot.
/// A standard authority row must select that exact snapshot, name a function
/// present exactly once among its executables, and pin the executable revision
/// stored by that snapshot. The set of standard targets must also cover every
/// executable in that snapshot; a missing standard target fails recovery
/// closed, exactly as a duplicated or unverified one does.
fn require_authorised_standard_targets(
    active: &ActiveDatabaseRevision,
    targets: &[SecurityFunctionTarget],
) -> Result<(), PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.invocation_target_authorities";
    let standard_targets = targets
        .iter()
        .filter(|target| target.class() == TargetClass::VerifiedStandard)
        .collect::<Vec<_>>();
    let Some(standard) = active.catalogue_hash_context().standard() else {
        if standard_targets.is_empty() {
            return Ok(());
        }
        return Err(PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: "active catalogue".to_owned(),
            rule: "standard invocation targets require a pinned verified standard snapshot",
        });
    };
    let standard_revision = standard.revision();
    let mut executable_functions = standard
        .executables()
        .iter()
        .map(|executable| (executable.function(), executable.revision().id()))
        .collect::<Vec<_>>();
    executable_functions.sort_unstable_by_key(|(function, _)| *function);
    if standard_targets.len() != executable_functions.len() {
        return Err(PostgresKernelError::DurableInvariant {
            relation: RELATION,
            record: "active catalogue".to_owned(),
            rule: "standard invocation targets must exactly match the pinned verified standard executables",
        });
    }
    for (target, (function, executable)) in standard_targets.iter().zip(executable_functions) {
        if target.function() != function
            || target.standard_revision() != Some(standard_revision)
            || target.executable_revision() != Some(executable)
        {
            return Err(PostgresKernelError::DurableInvariant {
                relation: RELATION,
                record: target.function().canonical(),
                rule: "standard invocation target must resolve exactly once in the pinned verified standard snapshot",
            });
        }
    }
    Ok(())
}

async fn load_principals(
    transaction: &Transaction<'_>,
) -> Result<Vec<Principal>, PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT id, kind, status
             FROM _orna_kernel.security_principals
             ORDER BY id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    rows.iter()
        .map(|row| {
            let id = PrincipalId::from_bytes(exact_id(
                row,
                "id",
                "security principal identity is not exactly 16 bytes",
            )?);
            let kind = decode_principal_kind(row.try_get("kind").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_principals",
                    id.canonical(),
                    "kind",
                    source,
                )
            })?)?;
            let status = decode_principal_status(row.try_get("status").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_principals",
                    id.canonical(),
                    "status",
                    source,
                )
            })?)?;
            Ok(Principal::new(id, kind, status))
        })
        .collect()
}

async fn load_memberships(
    transaction: &Transaction<'_>,
) -> Result<Vec<RoleMembership>, PostgresKernelError> {
    transaction
        .query(
            "SELECT role_id, member_id
             FROM _orna_kernel.security_role_memberships
             ORDER BY member_id, role_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            Ok(RoleMembership::new(
                PrincipalId::from_bytes(exact_id(
                    row,
                    "role_id",
                    "security role identity is not exactly 16 bytes",
                )?),
                PrincipalId::from_bytes(exact_id(
                    row,
                    "member_id",
                    "security member identity is not exactly 16 bytes",
                )?),
            ))
        })
        .collect()
}

async fn load_grants(
    transaction: &Transaction<'_>,
) -> Result<Vec<ExecuteGrant>, PostgresKernelError> {
    transaction
        .query(
            "SELECT grantee_id, function_id
             FROM _orna_kernel.security_execute_grants
             ORDER BY grantee_id, function_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            Ok(ExecuteGrant::new(
                PrincipalId::from_bytes(exact_id(
                    row,
                    "grantee_id",
                    "security grantee identity is not exactly 16 bytes",
                )?),
                FunctionId::from_bytes(exact_id(
                    row,
                    "function_id",
                    "security grant function identity is not exactly 16 bytes",
                )?),
            ))
        })
        .collect()
}

/// Encodes one closed privilege class exactly as the pure model displays it.
pub(crate) fn encode_privilege_class(class: PrivilegeClass) -> &'static str {
    match class {
        PrivilegeClass::Execute => "execute",
        PrivilegeClass::SecurityAdmin => "security_admin",
        PrivilegeClass::Inspect(privilege) => match privilege {
            InspectPrivilege::OwnInvocation => "inspect:own-invocation",
            InspectPrivilege::SessionInvocations => "inspect:session-invocations",
            InspectPrivilege::AnyInvocation => "inspect:any-invocation",
            InspectPrivilege::Values => "inspect:values",
            InspectPrivilege::Source => "inspect:source",
            InspectPrivilege::SecurityDetails => "inspect:security-details",
            InspectPrivilege::RuntimeInternals => "inspect:runtime-internals",
        },
    }
}

/// Decodes one closed privilege-class display string from protected storage.
pub(crate) fn decode_privilege_class(value: &str) -> Result<PrivilegeClass, PostgresKernelError> {
    match value {
        "execute" => Ok(PrivilegeClass::Execute),
        "security_admin" => Ok(PrivilegeClass::SecurityAdmin),
        "inspect:own-invocation" => Ok(PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation)),
        "inspect:session-invocations" => Ok(PrivilegeClass::Inspect(
            InspectPrivilege::SessionInvocations,
        )),
        "inspect:any-invocation" => Ok(PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation)),
        "inspect:values" => Ok(PrivilegeClass::Inspect(InspectPrivilege::Values)),
        "inspect:source" => Ok(PrivilegeClass::Inspect(InspectPrivilege::Source)),
        "inspect:security-details" => {
            Ok(PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails))
        }
        "inspect:runtime-internals" => {
            Ok(PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals))
        }
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_privilege_grants",
            record: value.to_owned(),
            rule: "privilege class must be execute, security_admin, or one closed inspect sub-privilege",
        }),
    }
}

/// Loads the durable privilege-class grants in canonical key order.
///
/// The class-wide sentinel `''` stored in `object_id` recovers as no object.
pub(crate) async fn load_privilege_grants(
    transaction: &Transaction<'_>,
) -> Result<Vec<PrivilegeGrant>, PostgresKernelError> {
    const RELATION: &str = "_orna_kernel.security_privilege_grants";
    let rows = transaction
        .query(
            "SELECT grantee_id, privilege_class, object_id
             FROM _orna_kernel.security_privilege_grants
             ORDER BY grantee_id, privilege_class, object_id",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let mut grants = Vec::with_capacity(rows.len());
    for row in &rows {
        let grantee = PrincipalId::from_bytes(exact_id(
            row,
            "grantee_id",
            "security privilege grantee identity is not exactly 16 bytes",
        )?);
        let class: String = row.try_get("privilege_class").map_err(|source| {
            row_decode(RELATION, grantee.canonical(), "privilege_class", source)
        })?;
        let class = decode_privilege_class(&class)?;
        let object: Vec<u8> = row
            .try_get("object_id")
            .map_err(|source| row_decode(RELATION, grantee.canonical(), "object_id", source))?;
        let object = if object.is_empty() {
            None
        } else {
            let function: [u8; 16] =
                object
                    .try_into()
                    .map_err(|_| PostgresKernelError::DurableInvariant {
                        relation: RELATION,
                        record: grantee.canonical(),
                        rule: "security privilege grant object identity is not exactly 16 bytes",
                    })?;
            Some(FunctionId::from_bytes(function))
        };
        grants.push(
            PrivilegeGrant::new(grantee, class, object).map_err(|error| {
                PostgresKernelError::DurableInvariant {
                    relation: RELATION,
                    record: grantee.canonical(),
                    rule: match error {
                        orna_core::security::PrivilegeGrantError::EmptyGrantee => {
                            "security privilege grant must carry a non-empty grantee"
                        }
                        orna_core::security::PrivilegeGrantError::EmptyObject => {
                            "security privilege grant object identity must be non-empty"
                        }
                        orna_core::security::PrivilegeGrantError::SecurityAdminObject => {
                            "security_admin privilege grant must be class-wide"
                        }
                    },
                }
            })?,
        );
    }
    Ok(grants)
}

async fn load_local_peer_credentials(
    transaction: &Transaction<'_>,
) -> Result<Vec<LocalPeerCredential>, PostgresKernelError> {
    transaction
        .query(
            "SELECT uid, principal_id
             FROM _orna_kernel.security_local_peer_credentials
             ORDER BY uid",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(|row| {
            let stored_uid: i64 = row.try_get("uid").map_err(|source| {
                row_decode(
                    "_orna_kernel.security_local_peer_credentials",
                    "selected row".to_owned(),
                    "uid",
                    source,
                )
            })?;
            let uid =
                u32::try_from(stored_uid).map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_local_peer_credentials",
                    record: stored_uid.to_string(),
                    rule: "local peer UID must fit the unsigned 32-bit range",
                })?;
            let principal = PrincipalId::from_bytes(exact_id(
                row,
                "principal_id",
                "local peer principal identity is not exactly 16 bytes",
            )?);
            Ok(LocalPeerCredential::new(uid, principal))
        })
        .collect()
}

async fn load_security_audit_events(
    transaction: &Transaction<'_>,
) -> Result<Vec<SecurityAuditEvent>, PostgresKernelError> {
    let events = transaction
        .query(
            "SELECT sequence, event_id, recorded_at, event_kind, outcome,
                    session_principal_id, effective_principal_id,
                    authorising_principal_id, function_id, source_revision_id,
                    catalogue_revision_id, denial_reason
             FROM _orna_kernel.security_audit_events
             ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .iter()
        .map(decode_security_audit_event)
        .collect::<Result<Vec<_>, _>>()?;
    for event in &events {
        if let Some(candidate) = event.decision().source_apply_candidate() {
            require_source_apply_audit_target(
                transaction,
                candidate,
                &event.sequence().to_string(),
            )
            .await?;
        }
    }
    Ok(events)
}

async fn require_source_apply_audit_target(
    transaction: &Transaction<'_>,
    candidate: RevisionPair,
    record: &str,
) -> Result<(), PostgresKernelError> {
    let source = candidate.source().to_bytes().to_vec();
    let catalogue = candidate.catalogue().to_bytes().to_vec();
    let exists = transaction
        .query_opt(
            "SELECT 1
             FROM _orna_kernel.catalogue_revisions AS catalogue
             JOIN _orna_kernel.source_revisions AS source
               ON source.id = catalogue.source_revision_id
             WHERE catalogue.id = $1
               AND catalogue.source_revision_id = $2",
            &[&catalogue, &source],
        )
        .await
        .map_err(PostgresKernelError::Database)?
        .is_some();
    if !exists {
        return Err(audit_invariant(
            record,
            "source apply audit target pair must exist in protected revisions",
        ));
    }
    Ok(())
}

/// Validates every durable protected invocation decision during normal recovery.
///
/// The caller has already recovered one pinned active revision in the same
/// read-only transaction. This function validates the historical target pair,
/// complete row shape, and exact linked `EXECUTE` evidence without repairing
/// any durable state.
pub(crate) async fn recover_invocation_audit_events(
    transaction: &Transaction<'_>,
    _active: &ActiveDatabaseRevision,
) -> Result<(), PostgresKernelError> {
    require_invocation_audit_relation_columns(transaction).await?;
    let security_events = load_security_audit_events(transaction).await?;
    let rows = transaction
        .query(
            "SELECT sequence, event_id, recorded_at, invocation_id, outcome,
                    session_principal_id, effective_principal_id,
                    authorising_principal_id, function_id, source_revision_id,
                    catalogue_revision_id, security_audit_event_id
             FROM _orna_kernel.invocation_audit_events
             ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &rows {
        let decision = decode_invocation_audit_decision(row)?;
        let record = decision.invocation.canonical();
        validate_invocation_audit_decision_shape(&decision, &record)?;
        validate_invocation_audit_evidence(&decision, &security_events, &record)?;
        if let Some(target) = decision.target {
            require_invocation_audit_target(transaction, target, &record).await?;
        }
    }
    recover_resource_audit_events(transaction).await?;
    Ok(())
}

async fn recover_resource_audit_events(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    require_resource_audit_relation_columns(transaction).await?;
    let rows = transaction
        .query(
            "SELECT sequence, event_id, recorded_at, request_id,
                    nested_invocation_id, parent_invocation_id, call_site_id,
                    target_function_id, source_revision_id, catalogue_revision_id,
                    session_principal_id, decision_outcome, terminal_outcome,
                    item_count, byte_count
             FROM _orna_kernel.resource_audit_events
             ORDER BY sequence",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    for row in &rows {
        let sequence: i64 = resource_audit_column(row, "selected row", "sequence")?;
        let record = sequence.to_string();
        if sequence <= 0 {
            return Err(resource_audit_invariant(
                &record,
                "generated resource audit sequence must be positive",
            ));
        }
        let _: SystemTime = resource_audit_column(row, &record, "recorded_at")?;
        let _: [u8; 16] = resource_audit_id(row, &record, "event_id")?;
        let request_id = InvocationId::from_bytes(resource_audit_id(row, &record, "request_id")?);
        let nested =
            InvocationId::from_bytes(resource_audit_id(row, &record, "nested_invocation_id")?);
        let _: [u8; 16] = resource_audit_id(row, &record, "parent_invocation_id")?;
        let _: [u8; 16] = resource_audit_id(row, &record, "call_site_id")?;
        let target_function = resource_audit_optional_id(row, &record, "target_function_id")?
            .map(FunctionId::from_bytes);
        let source = resource_audit_optional_id(row, &record, "source_revision_id")?
            .map(SourceRevisionId::from_bytes);
        let catalogue = resource_audit_optional_id(row, &record, "catalogue_revision_id")?
            .map(CatalogueRevisionId::from_bytes);
        if (
            target_function.is_some(),
            source.is_some(),
            catalogue.is_some(),
        ) != (true, true, true)
            && (
                target_function.is_some(),
                source.is_some(),
                catalogue.is_some(),
            ) != (false, false, false)
        {
            return Err(resource_audit_invariant(
                &record,
                "target and pinned revision evidence must be present together",
            ));
        }
        let session_principal =
            PrincipalId::from_bytes(resource_audit_id(row, &record, "session_principal_id")?);
        let decision: String = resource_audit_column(row, &record, "decision_outcome")?;
        let terminal: String = resource_audit_column(row, &record, "terminal_outcome")?;
        if !matches!(decision.as_str(), "allowed" | "denied") {
            return Err(resource_audit_invariant(
                &record,
                "resource decision outcome must be allowed or denied",
            ));
        }
        if !matches!(terminal.as_str(), "completed" | "failed" | "cancelled") {
            return Err(resource_audit_invariant(
                &record,
                "resource terminal outcome must be completed, failed, or cancelled",
            ));
        }
        let item_count: Option<i64> = resource_audit_column(row, &record, "item_count")?;
        let byte_count: Option<i64> = resource_audit_column(row, &record, "byte_count")?;
        if item_count.is_some_and(|count| count < 0) || byte_count.is_some_and(|count| count < 0) {
            return Err(resource_audit_invariant(
                &record,
                "resource audit counts must be non-negative",
            ));
        }
        if terminal != "completed" && (item_count.is_some() || byte_count.is_some()) {
            return Err(resource_audit_invariant(
                &record,
                "only completed resource audits may retain result counts",
            ));
        }
        if terminal == "completed" && decision != "allowed" {
            return Err(resource_audit_invariant(
                &record,
                "completed resource audit requires an allowed decision",
            ));
        }
        let invocation = transaction
            .query_opt(
                "SELECT outcome, session_principal_id, function_id,
                        source_revision_id, catalogue_revision_id
                 FROM _orna_kernel.invocation_audit_events
                 WHERE invocation_id = $1",
                &[&nested.to_bytes().to_vec()],
            )
            .await
            .map_err(PostgresKernelError::Database)?
            .ok_or_else(|| {
                resource_audit_invariant(
                    &record,
                    "nested resource invocation audit evidence is missing",
                )
            })?;
        let invocation_outcome: String = resource_audit_column(&invocation, &record, "outcome")?;
        let expected_invocation_outcome = decision.as_str();
        if invocation_outcome != expected_invocation_outcome {
            return Err(resource_audit_invariant(
                &record,
                "nested invocation outcome does not match resource decision",
            ));
        }
        let invocation_session: Vec<u8> =
            resource_audit_column(&invocation, &record, "session_principal_id")?;
        if invocation_session != session_principal.to_bytes() {
            return Err(resource_audit_invariant(
                &record,
                "nested invocation session principal does not match resource audit",
            ));
        }
        if let (Some(function), Some(source), Some(catalogue)) =
            (target_function, source, catalogue)
        {
            let invocation_function: Option<Vec<u8>> =
                resource_audit_column(&invocation, &record, "function_id")?;
            let invocation_source: Option<Vec<u8>> =
                resource_audit_column(&invocation, &record, "source_revision_id")?;
            let invocation_catalogue: Option<Vec<u8>> =
                resource_audit_column(&invocation, &record, "catalogue_revision_id")?;
            if invocation_function.as_deref() != Some(function.to_bytes().as_slice())
                || invocation_source.as_deref() != Some(source.to_bytes().as_slice())
                || invocation_catalogue.as_deref() != Some(catalogue.to_bytes().as_slice())
            {
                return Err(resource_audit_invariant(
                    &record,
                    "nested invocation target does not match resource audit",
                ));
            }
            require_invocation_audit_target(
                transaction,
                InvocationTarget::new(function, RevisionPair::new(source, catalogue)),
                &record,
            )
            .await?;
        }
        let _ = request_id;
    }
    Ok(())
}

async fn require_resource_audit_relation_columns(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT attribute.attname
             FROM pg_catalog.pg_attribute AS attribute
             JOIN pg_catalog.pg_class AS class ON class.oid = attribute.attrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_kernel'
               AND class.relname = 'resource_audit_events'
               AND attribute.attnum > 0
               AND NOT attribute.attisdropped
             ORDER BY attribute.attnum",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let names = rows
        .iter()
        .map(|row| resource_audit_column(row, "relation", "attname"))
        .collect::<Result<Vec<String>, _>>()?;
    let expected = [
        "sequence",
        "event_id",
        "recorded_at",
        "request_id",
        "nested_invocation_id",
        "parent_invocation_id",
        "call_site_id",
        "target_function_id",
        "source_revision_id",
        "catalogue_revision_id",
        "session_principal_id",
        "decision_outcome",
        "terminal_outcome",
        "item_count",
        "byte_count",
    ];
    if names != expected {
        return Err(resource_audit_invariant(
            "relation",
            "resource audit relation has unsupported disclosure-bearing columns",
        ));
    }
    Ok(())
}

fn resource_audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let bytes: Option<Vec<u8>> = resource_audit_column(row, record, column)?;
    bytes
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                resource_audit_invariant(
                    record,
                    "resource audit identity must be exactly sixteen bytes",
                )
            })
        })
        .transpose()
}

fn resource_audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = resource_audit_column(row, record, column)?;
    bytes.try_into().map_err(|_| {
        resource_audit_invariant(
            record,
            "resource audit identity must be exactly sixteen bytes",
        )
    })
}

fn resource_audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.resource_audit_events",
            record: record.to_owned(),
            column,
            rule: "resource audit column must use its exact PostgreSQL type",
            source,
        })
}

fn decode_invocation_audit_decision(
    row: &Row,
) -> Result<InvocationAuditDecision, PostgresKernelError> {
    let sequence: i64 = invocation_audit_column(row, "selected row", "sequence")?;
    let record = sequence.to_string();
    if sequence <= 0 {
        return Err(invocation_audit_invariant(
            &record,
            "generated invocation audit sequence must be positive",
        ));
    }
    let _: SystemTime = invocation_audit_column(row, &record, "recorded_at")?;
    let _ = InvocationAuditEventId::from_bytes(invocation_audit_id(row, &record, "event_id")?);
    let invocation = InvocationId::from_bytes(invocation_audit_id(row, &record, "invocation_id")?);
    let outcome = decode_invocation_audit_outcome(
        invocation_audit_column(row, &record, "outcome")?,
        &record,
    )?;
    let session_principal =
        PrincipalId::from_bytes(invocation_audit_id(row, &record, "session_principal_id")?);
    let effective_principal = invocation_audit_optional_id(row, &record, "effective_principal_id")?
        .map(PrincipalId::from_bytes);
    let authorising_principal =
        invocation_audit_optional_id(row, &record, "authorising_principal_id")?
            .map(PrincipalId::from_bytes);
    let function =
        invocation_audit_optional_id(row, &record, "function_id")?.map(FunctionId::from_bytes);
    let source_revision = invocation_audit_optional_id(row, &record, "source_revision_id")?
        .map(SourceRevisionId::from_bytes);
    let catalogue_revision = invocation_audit_optional_id(row, &record, "catalogue_revision_id")?
        .map(CatalogueRevisionId::from_bytes);
    let security_audit_event =
        invocation_audit_optional_id(row, &record, "security_audit_event_id")?
            .map(SecurityAuditEventId::from_bytes);
    let target = match (function, source_revision, catalogue_revision) {
        (Some(function), Some(source), Some(catalogue)) => Some(InvocationTarget::new(
            function,
            RevisionPair::new(source, catalogue),
        )),
        (None, None, None) => None,
        _ => {
            return Err(invocation_audit_invariant(
                &record,
                "target and pinned revision evidence must be present together",
            ));
        }
    };
    Ok(InvocationAuditDecision {
        invocation,
        outcome,
        session_principal,
        effective_principal,
        authorising_principal,
        target,
        security_audit_event,
    })
}

fn validate_invocation_audit_decision_shape(
    decision: &InvocationAuditDecision,
    record: &str,
) -> Result<(), PostgresKernelError> {
    if decision.effective_principal.is_some() != decision.authorising_principal.is_some() {
        return Err(invocation_audit_invariant(
            record,
            "effective and authorising principals must be present together",
        ));
    }
    if decision.target.is_some() != decision.security_audit_event.is_some() {
        return Err(invocation_audit_invariant(
            record,
            "target, pinned revision, and security audit evidence must be present together",
        ));
    }
    match (
        decision.outcome,
        decision.target,
        decision.effective_principal,
    ) {
        (SecurityAuditOutcome::Allowed, Some(_), Some(_)) => Ok(()),
        (SecurityAuditOutcome::Allowed, _, _) => Err(invocation_audit_invariant(
            record,
            "allowed invocation decision requires target and principal evidence",
        )),
        (SecurityAuditOutcome::Denied, None, None) => Ok(()),
        (SecurityAuditOutcome::Denied, Some(_), _) => Ok(()),
        (SecurityAuditOutcome::Denied, None, Some(_)) => Err(invocation_audit_invariant(
            record,
            "unresolved denied invocation cannot retain principal evidence",
        )),
    }
}

fn validate_invocation_audit_evidence(
    decision: &InvocationAuditDecision,
    security_events: &[SecurityAuditEvent],
    record: &str,
) -> Result<(), PostgresKernelError> {
    let Some(event_id) = decision.security_audit_event else {
        return Ok(());
    };
    let evidence = security_events
        .iter()
        .find(|event| event.id() == event_id)
        .ok_or_else(|| {
            invocation_audit_invariant(record, "linked security audit evidence is missing")
        })?;
    let security = evidence.decision();
    if security.kind() != SecurityAuditKind::Execute
        || security.outcome() != decision.outcome
        || security.session_principal() != Some(decision.session_principal)
        || security.effective_principal() != decision.effective_principal
        || security.authorising_principal() != decision.authorising_principal
        || security.target() != decision.target
    {
        return Err(invocation_audit_invariant(
            record,
            "linked security audit evidence does not match the invocation decision",
        ));
    }
    Ok(())
}

/// Validates one protected invocation-audit target through the durable
/// target-authority relation without writing or repairing any row.
///
/// The audited `RevisionPair` stays the durable standard pin: an `application`
/// authority row must resolve the function and its pinned executable revision
/// in that historical application catalogue, and a `standard` authority row
/// must resolve the audited function and its executable revision exactly once
/// in the exact verified standard snapshot pinned by that historical catalogue
/// revision. An absent authority row, mismatched revision pair, wrong standard
/// pin, absent or duplicate standard executable, or a row whose class cannot
/// resolve fails closed.
async fn require_invocation_audit_target(
    transaction: &Transaction<'_>,
    target: InvocationTarget,
    record: &str,
) -> Result<(), PostgresKernelError> {
    if system_function_by_id(target.function()).is_some_and(is_admitted_security_identity) {
        return Ok(());
    }
    let function = target.function().to_bytes().to_vec();
    let source = target.revision().source().to_bytes().to_vec();
    let catalogue = target.revision().catalogue().to_bytes().to_vec();
    let row = transaction
        .query_opt(
            "SELECT authority.target_class AS target_class,
                    authority.function_revision_id AS pinned_function_revision_id,
                    authority.standard_library_revision_id AS pinned_standard_library_revision_id,
                    revision.source_revision_id AS catalogue_source_revision_id,
                    revision.standard_library_revision_id AS catalogue_standard_library_revision_id,
                    function.current_function_revision_id AS catalogue_current_function_revision_id
             FROM _orna_kernel.invocation_target_authorities AS authority
             JOIN _orna_kernel.catalogue_revisions AS revision
               ON revision.id = authority.catalogue_revision_id
             LEFT JOIN _orna_kernel.catalogue_functions AS function
               ON function.catalogue_revision_id = authority.catalogue_revision_id
              AND function.function_id = authority.function_id
             WHERE authority.catalogue_revision_id = $1
               AND authority.function_id = $2",
            &[&catalogue, &function],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let Some(row) = row else {
        return Err(invocation_audit_invariant(
            record,
            "target function and pinned revision must exist together",
        ));
    };
    let relation = "_orna_kernel.invocation_audit_events";
    let catalogue_source: Vec<u8> =
        row.try_get("catalogue_source_revision_id")
            .map_err(|source| {
                row_decode(
                    relation,
                    record.to_owned(),
                    "catalogue_source_revision_id",
                    source,
                )
            })?;
    if catalogue_source != source {
        return Err(invocation_audit_invariant(
            record,
            "target function and pinned revision must exist together",
        ));
    }
    let class: String = row
        .try_get("target_class")
        .map_err(|source| row_decode(relation, record.to_owned(), "target_class", source))?;
    let pinned_revision: Vec<u8> =
        row.try_get("pinned_function_revision_id")
            .map_err(|source| {
                row_decode(
                    relation,
                    record.to_owned(),
                    "pinned_function_revision_id",
                    source,
                )
            })?;
    match class.as_str() {
        "application" => {
            let current: Option<Vec<u8>> = row
                .try_get("catalogue_current_function_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "catalogue_current_function_revision_id",
                        source,
                    )
                })?;
            if current.as_deref() != Some(pinned_revision.as_slice()) {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
        }
        "standard" => {
            let pinned_standard: Option<Vec<u8>> = row
                .try_get("pinned_standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "pinned_standard_library_revision_id",
                        source,
                    )
                })?;
            let catalogue_standard: Option<Vec<u8>> = row
                .try_get("catalogue_standard_library_revision_id")
                .map_err(|source| {
                    row_decode(
                        relation,
                        record.to_owned(),
                        "catalogue_standard_library_revision_id",
                        source,
                    )
                })?;
            if pinned_standard.as_deref() != catalogue_standard.as_deref() {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
            let bytes = catalogue_standard.ok_or_else(|| {
                invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                )
            })?;
            let standard_revision =
                StandardLibraryRevisionId::from_bytes(bytes.try_into().map_err(|_| {
                    invocation_audit_invariant(
                        record,
                        "target function and pinned revision must exist together",
                    )
                })?);
            let standard = load_verified_standard_library(transaction, standard_revision)
                .await
                .map_err(|_| {
                    invocation_audit_invariant(
                        record,
                        "target function and pinned revision must exist together",
                    )
                })?;
            let mut matches = standard
                .executables()
                .iter()
                .filter(|executable| executable.function() == target.function())
                .map(|executable| executable.revision().id().to_bytes().to_vec())
                .collect::<Vec<_>>();
            matches.sort_unstable();
            matches.dedup();
            if matches.len() != 1 || matches[0] != pinned_revision {
                return Err(invocation_audit_invariant(
                    record,
                    "target function and pinned revision must exist together",
                ));
            }
        }
        _ => {
            return Err(invocation_audit_invariant(
                record,
                "target function and pinned revision must exist together",
            ));
        }
    }
    Ok(())
}

fn encode_invocation_audit_outcome(outcome: SecurityAuditOutcome) -> &'static str {
    match outcome {
        SecurityAuditOutcome::Allowed => "allowed",
        SecurityAuditOutcome::Denied => "denied",
    }
}

fn decode_invocation_audit_outcome(
    outcome: String,
    record: &str,
) -> Result<SecurityAuditOutcome, PostgresKernelError> {
    match outcome.as_str() {
        "allowed" => Ok(SecurityAuditOutcome::Allowed),
        "denied" => Ok(SecurityAuditOutcome::Denied),
        _ => Err(invocation_audit_invariant(
            record,
            "invocation outcome must be allowed or denied",
        )),
    }
}

async fn require_invocation_audit_relation_columns(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let rows = transaction
        .query(
            "SELECT attribute.attname
             FROM pg_catalog.pg_attribute AS attribute
             JOIN pg_catalog.pg_class AS class ON class.oid = attribute.attrelid
             JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
             WHERE namespace.nspname = '_orna_kernel'
               AND class.relname = 'invocation_audit_events'
               AND attribute.attnum > 0
               AND NOT attribute.attisdropped
             ORDER BY attribute.attnum",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    let names = rows
        .iter()
        .map(|row| invocation_audit_column(row, "relation", "attname"))
        .collect::<Result<Vec<String>, _>>()?;
    let expected = [
        "sequence",
        "event_id",
        "recorded_at",
        "invocation_id",
        "outcome",
        "session_principal_id",
        "effective_principal_id",
        "authorising_principal_id",
        "function_id",
        "source_revision_id",
        "catalogue_revision_id",
        "security_audit_event_id",
    ];
    if names != expected {
        return Err(invocation_audit_invariant(
            "relation",
            "invocation audit relation has unsupported disclosure-bearing columns",
        ));
    }
    Ok(())
}

fn decode_security_audit_event(row: &Row) -> Result<SecurityAuditEvent, PostgresKernelError> {
    let sequence: i64 = audit_column(row, "selected row", "sequence")?;
    let record = sequence.to_string();
    let id = SecurityAuditEventId::from_bytes(audit_id(row, &record, "event_id")?);
    let recorded_at: SystemTime = audit_column(row, &record, "recorded_at")?;
    let kind: String = audit_column(row, &record, "event_kind")?;
    let outcome: String = audit_column(row, &record, "outcome")?;
    let session_principal =
        audit_optional_id(row, &record, "session_principal_id")?.map(PrincipalId::from_bytes);
    let effective_principal =
        audit_optional_id(row, &record, "effective_principal_id")?.map(PrincipalId::from_bytes);
    let authorising_principal =
        audit_optional_id(row, &record, "authorising_principal_id")?.map(PrincipalId::from_bytes);
    let function = audit_optional_id(row, &record, "function_id")?.map(FunctionId::from_bytes);
    let source_revision =
        audit_optional_id(row, &record, "source_revision_id")?.map(SourceRevisionId::from_bytes);
    let catalogue_revision = audit_optional_id(row, &record, "catalogue_revision_id")?
        .map(CatalogueRevisionId::from_bytes);
    let denial_reason: Option<String> = audit_column(row, &record, "denial_reason")?;

    let decision = match (kind.as_str(), outcome.as_str()) {
        ("authentication", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none()
                && denial_reason.is_none() =>
        {
            SecurityAuditDecision::recover_authentication_allowed(require_audit_value(
                session_principal,
                &record,
                "allowed authentication requires a session principal",
            )?)
        }
        ("authentication", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let reason = decode_authentication_audit_denial(
                require_audit_value(
                    denial_reason,
                    &record,
                    "denied authentication requires a reason",
                )?,
                &record,
            )?;
            SecurityAuditDecision::authentication_denied(session_principal, reason).map_err(
                |_| audit_invariant(&record, "authentication principal and reason must agree"),
            )?
        }
        ("execute", "allowed") if denial_reason.is_none() => {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            SecurityAuditDecision::recover_execute_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "allowed EXECUTE requires a session principal",
                )?,
                require_audit_value(
                    effective_principal,
                    &record,
                    "allowed EXECUTE requires an effective principal",
                )?,
                require_audit_value(
                    authorising_principal,
                    &record,
                    "allowed EXECUTE requires an authorising principal",
                )?,
                target,
            )
        }
        ("execute", "denied")
            if effective_principal.is_none() && authorising_principal.is_none() =>
        {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            let reason = decode_execute_audit_denial(
                require_audit_value(denial_reason, &record, "denied EXECUTE requires a reason")?,
                &record,
            )?;
            SecurityAuditDecision::recover_execute_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied EXECUTE requires a session principal",
                )?,
                target,
                reason,
            )
        }
        ("capability", outcome @ ("allowed" | "denied"))
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_some()
                && catalogue_revision.is_some() =>
        {
            let target = audit_target(function, source_revision, catalogue_revision, &record)?;
            let capability = decode_capability_audit_denial(
                require_audit_value(
                    denial_reason,
                    &record,
                    "capability audit requires a capability name",
                )?,
                &record,
            )?;
            let session_principal = require_audit_value(
                session_principal,
                &record,
                "capability audit requires a session principal",
            )?;
            let decision = match outcome {
                "allowed" => SecurityAuditDecision::recover_capability_allowed(
                    session_principal,
                    target,
                    capability,
                ),
                "denied" => SecurityAuditDecision::recover_capability_denied(
                    session_principal,
                    target,
                    capability,
                ),
                _ => unreachable!("capability outcome is closed by the outer match"),
            };
            decision.map_err(|_| {
                audit_invariant(
                    &record,
                    "capability audit name must be a qualified name with no arguments",
                )
            })?
        }
        ("user_state", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let operation_detail = require_audit_value(
                denial_reason,
                &record,
                "USER state audit requires an operation and cell count",
            )?;
            let (operation, cell_count) =
                decode_user_state_audit_detail(&operation_detail, &record)?;
            SecurityAuditDecision::recover_user_state_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "USER state audit requires a session principal",
                )?,
                operation,
                require_audit_value(
                    function,
                    &record,
                    "USER state audit requires a root function",
                )?,
                cell_count,
            )
        }
        ("inspect", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            // The protected columns retain only the closed capture detail in
            // the denial-reason column; the epoch owner is never stored.
            let (requested, scope) = decode_inspect_audit_detail(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "INSPECT audit requires a capture detail",
                )?,
                &record,
            )?;
            SecurityAuditDecision::recover_inspect_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "INSPECT audit requires a session principal",
                )?,
                requested,
                scope,
                None,
            )
        }
        ("inspect", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let reason = decode_inspect_audit_denial(
                require_audit_value(denial_reason, &record, "denied INSPECT requires a reason")?,
                &record,
            )?;
            SecurityAuditDecision::recover_inspect_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied INSPECT requires a session principal",
                )?,
                None,
                reason,
            )
        }
        ("source_apply", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_none()
                && source_revision.is_some()
                && catalogue_revision.is_some() =>
        {
            decode_source_apply_audit_detail(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "allowed source apply audit requires a committed detail",
                )?,
                &record,
            )?;
            let session_principal = require_audit_value(
                session_principal,
                &record,
                "source apply audit requires a session principal",
            )?;
            if session_principal != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                return Err(audit_invariant(
                    &record,
                    "source apply audit must use the catalogue-health service principal",
                ));
            }
            SecurityAuditDecision::recover_source_apply_allowed(
                session_principal,
                RevisionPair::new(
                    require_audit_value(
                        source_revision,
                        &record,
                        "source apply audit requires a source revision",
                    )?,
                    require_audit_value(
                        catalogue_revision,
                        &record,
                        "source apply audit requires a catalogue revision",
                    )?,
                ),
            )
        }
        ("security_admin", "allowed")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            // The protected columns retain only the closed operation detail
            // and the sealed target identity; argument payloads are never
            // stored.
            let operation = decode_security_admin_audit_detail(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "allowed security-admin audit requires an operation detail",
                )?,
                &record,
            )?;
            SecurityAuditDecision::recover_security_admin_allowed(
                require_audit_value(
                    session_principal,
                    &record,
                    "allowed security-admin audit requires a session principal",
                )?,
                operation,
                require_audit_value(
                    function,
                    &record,
                    "security-admin audit requires the sealed target identity",
                )?,
            )
        }
        ("security_admin", "denied")
            if effective_principal.is_none()
                && authorising_principal.is_none()
                && function.is_some()
                && source_revision.is_none()
                && catalogue_revision.is_none() =>
        {
            let (operation, reason) = decode_security_admin_audit_denial(
                &require_audit_value(
                    denial_reason,
                    &record,
                    "denied security-admin audit requires a reason",
                )?,
                &record,
            )?;
            SecurityAuditDecision::recover_security_admin_denied(
                require_audit_value(
                    session_principal,
                    &record,
                    "denied security-admin audit requires a session principal",
                )?,
                operation,
                require_audit_value(
                    function,
                    &record,
                    "security-admin audit requires the sealed target identity",
                )?,
                reason,
            )
        }
        _ => {
            return Err(audit_invariant(
                &record,
                "audit event shape is not recognised",
            ));
        }
    };

    Ok(SecurityAuditEvent::new(id, sequence, recorded_at, decision))
}

fn audit_target(
    function: Option<FunctionId>,
    source: Option<SourceRevisionId>,
    catalogue: Option<CatalogueRevisionId>,
    record: &str,
) -> Result<InvocationTarget, PostgresKernelError> {
    Ok(InvocationTarget::new(
        require_audit_value(function, record, "EXECUTE requires a function")?,
        RevisionPair::new(
            require_audit_value(source, record, "EXECUTE requires a source revision")?,
            require_audit_value(catalogue, record, "EXECUTE requires a catalogue revision")?,
        ),
    ))
}

fn decode_authentication_audit_denial(
    value: String,
    record: &str,
) -> Result<LocalPeerAuthenticationError, PostgresKernelError> {
    let invalid = |reason| LocalPeerAuthenticationError::InvalidPrincipal(reason);
    match value.as_str() {
        "authentication_unknown_uid" => Ok(LocalPeerAuthenticationError::UnknownUid),
        "authentication_unknown_session_principal" => {
            Ok(invalid(SessionBindingError::UnknownSessionPrincipal))
        }
        "authentication_disabled_session_principal" => {
            Ok(invalid(SessionBindingError::DisabledSessionPrincipal))
        }
        "authentication_role_cannot_authenticate" => {
            Ok(invalid(SessionBindingError::RoleCannotAuthenticate))
        }
        "authentication_duplicate_active_role" => {
            Ok(invalid(SessionBindingError::DuplicateActiveRole))
        }
        "authentication_unknown_active_role" => Ok(invalid(SessionBindingError::UnknownActiveRole)),
        "authentication_disabled_active_role" => {
            Ok(invalid(SessionBindingError::DisabledActiveRole))
        }
        "authentication_active_principal_is_not_role" => {
            Ok(invalid(SessionBindingError::ActivePrincipalIsNotRole))
        }
        "authentication_unreachable_active_role" => {
            Ok(invalid(SessionBindingError::UnreachableActiveRole))
        }
        _ => Err(audit_invariant(
            record,
            "authentication denial reason is unsupported",
        )),
    }
}

fn encode_authentication_audit_denial(reason: LocalPeerAuthenticationError) -> &'static str {
    match reason {
        LocalPeerAuthenticationError::UnknownUid => "authentication_unknown_uid",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::UnknownSessionPrincipal,
        ) => "authentication_unknown_session_principal",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DisabledSessionPrincipal,
        ) => "authentication_disabled_session_principal",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::RoleCannotAuthenticate,
        ) => "authentication_role_cannot_authenticate",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::DuplicateActiveRole,
        ) => "authentication_duplicate_active_role",
        LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::UnknownActiveRole) => {
            "authentication_unknown_active_role"
        }
        LocalPeerAuthenticationError::InvalidPrincipal(SessionBindingError::DisabledActiveRole) => {
            "authentication_disabled_active_role"
        }
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::ActivePrincipalIsNotRole,
        ) => "authentication_active_principal_is_not_role",
        LocalPeerAuthenticationError::InvalidPrincipal(
            SessionBindingError::UnreachableActiveRole,
        ) => "authentication_unreachable_active_role",
    }
}

fn decode_execute_audit_denial(
    value: String,
    record: &str,
) -> Result<orna_core::security::ExecuteDenial, PostgresKernelError> {
    use orna_core::security::ExecuteDenial;

    match value.as_str() {
        "execute_invalid_session" => Ok(ExecuteDenial::InvalidSession),
        "execute_unknown_function" => Ok(ExecuteDenial::UnknownFunction),
        "execute_revision_mismatch" => Ok(ExecuteDenial::RevisionMismatch),
        "execute_missing_grant" => Ok(ExecuteDenial::MissingExecuteGrant),
        _ => Err(audit_invariant(
            record,
            "EXECUTE denial reason is unsupported",
        )),
    }
}

fn encode_execute_audit_denial(reason: orna_core::security::ExecuteDenial) -> &'static str {
    use orna_core::security::ExecuteDenial;

    match reason {
        ExecuteDenial::InvalidSession => "execute_invalid_session",
        ExecuteDenial::UnknownFunction => "execute_unknown_function",
        ExecuteDenial::RevisionMismatch => "execute_revision_mismatch",
        ExecuteDenial::MissingExecuteGrant => "execute_missing_grant",
    }
}

fn encode_security_audit_identity_columns(
    decision: &SecurityAuditDecision,
) -> (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>) {
    if let Some(candidate) = decision.source_apply_candidate() {
        return (
            None,
            Some(candidate.source().to_bytes().to_vec()),
            Some(candidate.catalogue().to_bytes().to_vec()),
        );
    }
    match decision.target() {
        Some(target) => (
            Some(target.function().to_bytes().to_vec()),
            Some(target.revision().source().to_bytes().to_vec()),
            Some(target.revision().catalogue().to_bytes().to_vec()),
        ),
        None => (
            decision
                .user_state_root_function()
                .or_else(|| decision.security_admin_target())
                .map(|function| function.to_bytes().to_vec()),
            None,
            None,
        ),
    }
}

fn encode_security_audit_kind(kind: SecurityAuditKind) -> &'static str {
    match kind {
        SecurityAuditKind::Authentication => "authentication",
        SecurityAuditKind::Execute => "execute",
        SecurityAuditKind::Capability => "capability",
        SecurityAuditKind::UserState => "user_state",
        SecurityAuditKind::Inspect => "inspect",
        SecurityAuditKind::SecurityAdmin => "security_admin",
        SecurityAuditKind::SourceApply => "source_apply",
    }
}

fn encode_capability_audit_denial(capability: &str) -> String {
    format!("capability:{capability}")
}
fn encode_source_apply_audit_detail() -> &'static str {
    "source_apply:committed"
}

fn decode_source_apply_audit_detail(value: &str, record: &str) -> Result<(), PostgresKernelError> {
    if value == encode_source_apply_audit_detail() {
        Ok(())
    } else {
        Err(audit_invariant(
            record,
            "source apply audit detail is unsupported",
        ))
    }
}
fn encode_user_state_audit_detail(operation: UserStateAuditOperation, cell_count: u64) -> String {
    let operation = match operation {
        UserStateAuditOperation::Load => "load",
        UserStateAuditOperation::Write => "write",
    };
    format!("user_state:{operation}:cells={cell_count}")
}

fn decode_user_state_audit_detail(
    value: &str,
    record: &str,
) -> Result<(UserStateAuditOperation, u64), PostgresKernelError> {
    let Some(rest) = value.strip_prefix("user_state:") else {
        return Err(audit_invariant(
            record,
            "USER state audit detail must start with user_state:",
        ));
    };
    let Some((operation, count)) = rest.split_once(":cells=") else {
        return Err(audit_invariant(
            record,
            "USER state audit detail must contain an operation and cell count",
        ));
    };
    let operation = match operation {
        "load" => UserStateAuditOperation::Load,
        "write" => UserStateAuditOperation::Write,
        _ => {
            return Err(audit_invariant(
                record,
                "USER state audit operation must be load or write",
            ));
        }
    };
    let cell_count = count.parse::<u64>().map_err(|_| {
        audit_invariant(
            record,
            "USER state audit cell count must be a canonical unsigned integer",
        )
    })?;
    if encode_user_state_audit_detail(operation, cell_count) != value {
        return Err(audit_invariant(
            record,
            "USER state audit detail is not canonical",
        ));
    }
    Ok((operation, cell_count))
}

fn decode_capability_audit_denial(
    value: String,
    record: &str,
) -> Result<String, PostgresKernelError> {
    value
        .strip_prefix("capability:")
        .map(str::to_owned)
        .ok_or_else(|| audit_invariant(record, "capability denial reason is unsupported"))
}

/// Encodes one closed INSPECT denial reason exactly as the pure model names it.
fn encode_inspect_audit_denial(reason: InspectDenial) -> &'static str {
    reason.audit_reason()
}

/// Encodes the allowed INSPECT capture detail into the protected `denial_reason`
/// column, mirroring the USER state operation detail pattern: the column
/// carries a closed `inspect:...` detail for allowed rows and a closed
/// `inspect:...` denial reason for denied rows.
fn encode_inspect_audit_detail(requested: InspectPrivilege, scope: InspectEpochScope) -> String {
    format!(
        "inspect:requested={}:scope={}",
        encode_inspect_privilege(requested),
        encode_inspect_scope(scope)
    )
}

fn encode_inspect_privilege(privilege: InspectPrivilege) -> &'static str {
    match privilege {
        InspectPrivilege::OwnInvocation => "own-invocation",
        InspectPrivilege::SessionInvocations => "session-invocations",
        InspectPrivilege::AnyInvocation => "any-invocation",
        InspectPrivilege::Values => "values",
        InspectPrivilege::Source => "source",
        InspectPrivilege::SecurityDetails => "security-details",
        InspectPrivilege::RuntimeInternals => "runtime-internals",
    }
}

fn encode_inspect_scope(scope: InspectEpochScope) -> &'static str {
    match scope {
        InspectEpochScope::Own => "own",
        InspectEpochScope::Session => "session",
        InspectEpochScope::Foreign => "foreign",
    }
}

fn decode_inspect_privilege(
    value: &str,
    record: &str,
) -> Result<InspectPrivilege, PostgresKernelError> {
    match value {
        "own-invocation" => Ok(InspectPrivilege::OwnInvocation),
        "session-invocations" => Ok(InspectPrivilege::SessionInvocations),
        "any-invocation" => Ok(InspectPrivilege::AnyInvocation),
        "values" => Ok(InspectPrivilege::Values),
        "source" => Ok(InspectPrivilege::Source),
        "security-details" => Ok(InspectPrivilege::SecurityDetails),
        "runtime-internals" => Ok(InspectPrivilege::RuntimeInternals),
        _ => Err(audit_invariant(
            record,
            "INSPECT requested privilege is unsupported",
        )),
    }
}

fn decode_inspect_scope(
    value: &str,
    record: &str,
) -> Result<InspectEpochScope, PostgresKernelError> {
    match value {
        "own" => Ok(InspectEpochScope::Own),
        "session" => Ok(InspectEpochScope::Session),
        "foreign" => Ok(InspectEpochScope::Foreign),
        _ => Err(audit_invariant(
            record,
            "INSPECT epoch scope is unsupported",
        )),
    }
}

fn decode_inspect_audit_detail(
    value: &str,
    record: &str,
) -> Result<(InspectPrivilege, InspectEpochScope), PostgresKernelError> {
    let Some(rest) = value.strip_prefix("inspect:") else {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail must start with inspect:",
        ));
    };
    let Some((requested, scope)) = rest.split_once(":scope=") else {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail must carry a requested privilege and scope",
        ));
    };
    let Some(requested) = requested.strip_prefix("requested=") else {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail must carry a requested privilege",
        ));
    };
    let requested = decode_inspect_privilege(requested, record)?;
    let scope = decode_inspect_scope(scope, record)?;
    if encode_inspect_audit_detail(requested, scope) != value {
        return Err(audit_invariant(
            record,
            "INSPECT audit detail is not canonical",
        ));
    }
    Ok((requested, scope))
}

fn decode_inspect_audit_denial(
    value: String,
    record: &str,
) -> Result<InspectDenial, PostgresKernelError> {
    match value.as_str() {
        "inspect:missing-privilege" => Ok(InspectDenial::MissingPrivilege),
        "inspect:missing-epoch" => Ok(InspectDenial::MissingEpoch),
        "inspect:observer-suppressed" => Ok(InspectDenial::ObserverSuppressed),
        _ => Err(audit_invariant(
            record,
            "INSPECT denial reason is unsupported",
        )),
    }
}

/// Encodes one closed security-admin operation kind exactly as the pure
/// model names it.
fn encode_security_admin_audit_operation(operation: SecurityAdminAuditOperation) -> &'static str {
    match operation {
        SecurityAdminAuditOperation::CreatePrincipal => "create_principal",
        SecurityAdminAuditOperation::DisablePrincipal => "disable_principal",
        SecurityAdminAuditOperation::CreateRole => "create_role",
        SecurityAdminAuditOperation::GrantRole => "grant_role",
        SecurityAdminAuditOperation::RevokeRole => "revoke_role",
        SecurityAdminAuditOperation::GrantPrivilege => "grant_privilege",
        SecurityAdminAuditOperation::RevokePrivilege => "revoke_privilege",
    }
}

/// Encodes the allowed security-admin capture detail into the protected
/// `denial_reason` column, mirroring the INSPECT and USER state detail
/// patterns: the column carries a closed `security_admin:<operation>`
/// detail for allowed rows.
fn encode_security_admin_audit_detail(operation: SecurityAdminAuditOperation) -> String {
    format!(
        "security_admin:{}",
        encode_security_admin_audit_operation(operation)
    )
}

/// Encodes the denied security-admin capture detail: the closed operation
/// and the closed `missing-privilege` reason tail, so a denied row
/// round-trips both the operation and the denial without ever recording an
/// argument payload.
fn encode_security_admin_audit_denied_detail(
    decision: &SecurityAuditDecision,
    reason: PrivilegeDenial,
) -> Option<String> {
    let operation = decision.security_admin_operation()?;
    Some(encode_security_admin_audit_denied_detail_value(
        operation, reason,
    ))
}

fn decode_security_admin_audit_operation(
    value: &str,
    record: &str,
) -> Result<SecurityAdminAuditOperation, PostgresKernelError> {
    match value {
        "create_principal" => Ok(SecurityAdminAuditOperation::CreatePrincipal),
        "disable_principal" => Ok(SecurityAdminAuditOperation::DisablePrincipal),
        "create_role" => Ok(SecurityAdminAuditOperation::CreateRole),
        "grant_role" => Ok(SecurityAdminAuditOperation::GrantRole),
        "revoke_role" => Ok(SecurityAdminAuditOperation::RevokeRole),
        "grant_privilege" => Ok(SecurityAdminAuditOperation::GrantPrivilege),
        "revoke_privilege" => Ok(SecurityAdminAuditOperation::RevokePrivilege),
        _ => Err(audit_invariant(
            record,
            "security-admin audit operation is unsupported",
        )),
    }
}

fn decode_security_admin_audit_detail(
    value: &str,
    record: &str,
) -> Result<SecurityAdminAuditOperation, PostgresKernelError> {
    let Some(operation) = value.strip_prefix("security_admin:") else {
        return Err(audit_invariant(
            record,
            "security-admin audit detail must start with security_admin:",
        ));
    };
    if operation.contains(':') {
        return Err(audit_invariant(
            record,
            "allowed security-admin audit detail must carry only the operation",
        ));
    }
    let operation = decode_security_admin_audit_operation(operation, record)?;
    if encode_security_admin_audit_detail(operation) != value {
        return Err(audit_invariant(
            record,
            "security-admin audit detail is not canonical",
        ));
    }
    Ok(operation)
}

fn decode_security_admin_audit_denial(
    value: &str,
    record: &str,
) -> Result<(SecurityAdminAuditOperation, PrivilegeDenial), PostgresKernelError> {
    let Some(rest) = value.strip_prefix("security_admin:") else {
        return Err(audit_invariant(
            record,
            "security-admin denial reason must start with security_admin:",
        ));
    };
    let Some((operation, reason)) = rest.split_once(':') else {
        return Err(audit_invariant(
            record,
            "security-admin denial reason must carry an operation and a reason",
        ));
    };
    let operation = decode_security_admin_audit_operation(operation, record)?;
    let reason = match reason {
        "missing-privilege" => PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        },
        _ => {
            return Err(audit_invariant(
                record,
                "security-admin denial reason is unsupported",
            ));
        }
    };
    if encode_security_admin_audit_denied_detail_value(operation, reason) != value {
        return Err(audit_invariant(
            record,
            "security-admin denial reason is not canonical",
        ));
    }
    Ok((operation, reason))
}

fn encode_security_admin_audit_denied_detail_value(
    operation: SecurityAdminAuditOperation,
    reason: PrivilegeDenial,
) -> String {
    format!(
        "security_admin:{}:{}",
        encode_security_admin_audit_operation(operation),
        match reason {
            PrivilegeDenial::MissingPrivilege { .. } => "missing-privilege",
        }
    )
}

fn require_audit_value<T>(
    value: Option<T>,
    record: &str,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    value.ok_or_else(|| audit_invariant(record, rule))
}

#[allow(dead_code)]
fn require_invocation_audit_value<T>(
    value: Option<T>,
    record: &str,
    rule: &'static str,
) -> Result<T, PostgresKernelError> {
    value.ok_or_else(|| invocation_audit_invariant(record, rule))
}

fn invocation_audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let value: Option<Vec<u8>> = invocation_audit_column(row, record, column)?;
    value
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                invocation_audit_invariant(
                    record,
                    "invocation audit identity must be exactly sixteen bytes",
                )
            })
        })
        .transpose()
}

fn invocation_audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = invocation_audit_column(row, record, column)?;
    bytes.try_into().map_err(|_| {
        invocation_audit_invariant(
            record,
            "invocation audit identity must be exactly sixteen bytes",
        )
    })
}

fn invocation_audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.invocation_audit_events",
            record: record.to_owned(),
            column,
            rule: "invocation audit column must use its exact PostgreSQL type",
            source,
        })
}

fn invocation_audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.invocation_audit_events",
        record: record.to_owned(),
        rule,
    }
}

fn audit_optional_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<Option<[u8; 16]>, PostgresKernelError> {
    let value: Option<Vec<u8>> = audit_column(row, record, column)?;
    value
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                audit_invariant(record, "audit identity must be exactly sixteen bytes")
            })
        })
        .transpose()
}

fn audit_id(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = audit_column(row, record, column)?;
    bytes
        .try_into()
        .map_err(|_| audit_invariant(record, "audit event identity must be exactly sixteen bytes"))
}

fn audit_column<T: FromSqlOwned>(
    row: &Row,
    record: &str,
    column: &'static str,
) -> Result<T, PostgresKernelError> {
    row.try_get(column)
        .map_err(|source| PostgresKernelError::RowDecode {
            relation: "_orna_kernel.security_audit_events",
            record: record.to_owned(),
            column,
            rule: "security audit column must use its exact PostgreSQL type",
            source,
        })
}

fn audit_invariant(record: &str, rule: &'static str) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.security_audit_events",
        record: record.to_owned(),
        rule,
    }
}

fn exact_id(
    row: &Row,
    column: &'static str,
    rule: &'static str,
) -> Result<[u8; 16], PostgresKernelError> {
    let bytes: Vec<u8> = row.try_get(column).map_err(|source| {
        row_decode(
            "_orna_kernel security snapshot",
            "selected row".to_owned(),
            column,
            source,
        )
    })?;
    bytes
        .try_into()
        .map_err(|_| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel security snapshot",
            record: "selected row".to_owned(),
            rule,
        })
}

fn row_decode(
    relation: &'static str,
    record: String,
    column: &'static str,
    source: tokio_postgres::Error,
) -> PostgresKernelError {
    PostgresKernelError::RowDecode {
        relation,
        record,
        column,
        rule: "security snapshot column must use its exact PostgreSQL type",
        source,
    }
}

pub(crate) fn encode_principal_kind(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::User => "user",
        PrincipalKind::Role => "role",
        PrincipalKind::Service => "service",
    }
}

fn decode_principal_kind(value: String) -> Result<PrincipalKind, PostgresKernelError> {
    match value.as_str() {
        "user" => Ok(PrincipalKind::User),
        "role" => Ok(PrincipalKind::Role),
        "service" => Ok(PrincipalKind::Service),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal kind must be user, role, or service",
        }),
    }
}

fn encode_principal_status(status: PrincipalStatus) -> &'static str {
    match status {
        PrincipalStatus::Active => "active",
        PrincipalStatus::Disabled => "disabled",
    }
}

fn decode_principal_status(value: String) -> Result<PrincipalStatus, PostgresKernelError> {
    match value.as_str() {
        "active" => Ok(PrincipalStatus::Active),
        "disabled" => Ok(PrincipalStatus::Disabled),
        _ => Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.security_principals",
            record: value,
            rule: "principal status must be active or disabled",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{
        CatalogueRevisionId, FieldId, ObjectId, ParameterId, TypeId,
        catalogue::{
            CatalogueSnapshot, EnumTypeDefinition, QualifiedSemanticName, SchemaDefinition,
        },
        security::PrivilegeDecision,
        system::{
            SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
        },
        value::{EnumValue, ResultColumn, ResultRow, ResultRows, RuntimeFloat},
    };
    use std::time::UNIX_EPOCH;

    const RAW_CALL_FUNCTION: FunctionId = FunctionId::from_bytes([0x61; 16]);
    const RAW_CALL_PARAMETER: ParameterId = ParameterId::from_bytes([0x62; 16]);

    #[test]
    fn resource_target_shape_matches_protocol_kind() {
        use orna_core::{
            catalogue::{
                FunctionReturn, FunctionReturnColumnDefinition, FunctionSecurity,
                FunctionTransaction, FunctionVolatility, QualifiedSemanticName,
            },
            types::{ResolvedType, StandardScalar},
        };

        let function = FunctionDefinition::new(
            RAW_CALL_FUNCTION,
            QualifiedSemanticName::new(["app", "resource"]).expect("function name"),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert!(resource_target_shape_is_supported(
            &function,
            ProtocolResourceKind::Single,
        ));
        assert!(!resource_target_shape_is_supported(
            &function,
            ProtocolResourceKind::Stream,
        ));

        let stream = FunctionDefinition::new(
            RAW_CALL_FUNCTION,
            QualifiedSemanticName::new(["app", "stream"]).expect("function name"),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Stream(ResolvedType::scalar(StandardScalar::Integer)),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert!(resource_target_shape_is_supported(
            &stream,
            ProtocolResourceKind::Stream,
        ));
        assert!(!resource_target_shape_is_supported(
            &stream,
            ProtocolResourceKind::Single,
        ));
        let rows = FunctionDefinition::new(
            RAW_CALL_FUNCTION,
            QualifiedSemanticName::new(["app", "finite_rows"]).expect("function name"),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
            )]),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert!(!resource_target_shape_is_supported(
            &rows,
            ProtocolResourceKind::Stream,
        ));

        let multi_rows = FunctionDefinition::new(
            RAW_CALL_FUNCTION,
            QualifiedSemanticName::new(["app", "multi_stream"]).expect("function name"),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Rows(vec![
                FunctionReturnColumnDefinition::new(
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
                FunctionReturnColumnDefinition::new(
                    "second",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                ),
            ]),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        assert!(!resource_target_shape_is_supported(
            &multi_rows,
            ProtocolResourceKind::Stream,
        ));
        assert_eq!(
            sealed_server_result_kind(rows.return_type()),
            Some(ProtocolResourceKind::Stream),
        );
    }

    #[test]
    fn resource_result_rejects_rows_with_extra_columns() {
        use orna_core::types::{ResolvedType, StandardScalar};

        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let rows = ResultRows::new(
            [
                ResultColumn::new(
                    "first",
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                )
                .expect("first column is valid"),
                ResultColumn::new(
                    "second",
                    ResolvedType::scalar(StandardScalar::Integer),
                    false,
                )
                .expect("second column is valid"),
            ],
            [ResultRow::new([
                RuntimeValue::Integer(1),
                RuntimeValue::Integer(2),
            ])],
        )
        .expect("two-column result rows are valid");
        let result = ServerSelectResult::new(
            pair,
            RAW_CALL_FUNCTION,
            FunctionRevisionId::from_bytes([0x73; 16]),
            rows,
        );

        assert!(resource_values_from_server_result(ProtocolResourceKind::Stream, result).is_none());
    }

    #[test]
    fn resource_arguments_require_canonical_complete_typed_set() {
        use orna_core::{
            catalogue::{
                FunctionSecurity, FunctionTransaction, FunctionVolatility, ParameterDefinition,
                QualifiedSemanticName,
            },
            types::{ResolvedType, StandardScalar},
        };

        let first = ParameterId::from_bytes([0x01; 16]);
        let second = ParameterId::from_bytes([0x02; 16]);
        let function = FunctionDefinition::new(
            RAW_CALL_FUNCTION,
            QualifiedSemanticName::new(["app", "resource"]).expect("function name"),
            FunctionDomain::Server,
            vec![
                ParameterDefinition::new(
                    first,
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    None,
                ),
                ParameterDefinition::new(
                    second,
                    "second",
                    1,
                    ResolvedType::scalar(StandardScalar::Boolean),
                    None,
                ),
            ],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::new(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let canonical = vec![
            ResourceArgument {
                parameter: first,
                value: RuntimeValue::Integer(7),
            },
            ResourceArgument {
                parameter: second,
                value: RuntimeValue::Boolean(true),
            },
        ];
        assert!(
            bind_authenticated_resource_arguments(
                &CatalogueHashContext::version_one(),
                &function,
                &canonical,
            )
            .is_some()
        );

        let wrong_order = vec![canonical[1].clone(), canonical[0].clone()];
        assert!(
            bind_authenticated_resource_arguments(
                &CatalogueHashContext::version_one(),
                &function,
                &wrong_order,
            )
            .is_none()
        );
        let wrong_type = vec![
            ResourceArgument {
                parameter: first,
                value: RuntimeValue::Boolean(true),
            },
            canonical[1].clone(),
        ];
        assert!(
            bind_authenticated_resource_arguments(
                &CatalogueHashContext::version_one(),
                &function,
                &wrong_type,
            )
            .is_none()
        );
        assert!(
            bind_authenticated_resource_arguments(
                &CatalogueHashContext::version_one(),
                &function,
                &canonical[..1],
            )
            .is_none()
        );
    }

    #[test]
    fn invocation_audit_decision_uses_only_closed_execute_evidence() {
        let target = InvocationTarget::new(
            FunctionId::from_bytes([0x81; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x82; 16]),
                CatalogueRevisionId::from_bytes([0x83; 16]),
            ),
        );
        let evidence = SecurityAuditEvent::new(
            SecurityAuditEventId::from_bytes([0x84; 16]),
            1,
            UNIX_EPOCH,
            SecurityAuditDecision::recover_execute_allowed(
                PrincipalId::from_bytes([0x85; 16]),
                PrincipalId::from_bytes([0x86; 16]),
                PrincipalId::from_bytes([0x87; 16]),
                target,
            ),
        );
        let decision = InvocationAuditDecision::from_execute_evidence(
            InvocationId::from_bytes([0x88; 16]),
            &evidence,
        )
        .expect("allowed EXECUTE evidence must create one invocation decision");
        assert_eq!(decision.outcome, SecurityAuditOutcome::Allowed);
        assert_eq!(decision.target, Some(target));
        assert_eq!(decision.security_audit_event, Some(evidence.id()));

        let authentication = SecurityAuditEvent::new(
            SecurityAuditEventId::from_bytes([0x89; 16]),
            2,
            UNIX_EPOCH,
            SecurityAuditDecision::recover_authentication_allowed(PrincipalId::from_bytes(
                [0x85; 16],
            )),
        );
        assert!(matches!(
            InvocationAuditDecision::from_execute_evidence(
                InvocationId::from_bytes([0x8a; 16]),
                &authentication,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.invocation_audit_events",
                rule: "invocation decision requires EXECUTE audit evidence",
                ..
            })
        ));

        let unresolved = InvocationAuditDecision::unresolved_denied(
            InvocationId::from_bytes([0x8b; 16]),
            PrincipalId::from_bytes([0x85; 16]),
        );
        validate_invocation_audit_decision_shape(&unresolved, "test")
            .expect("unresolved denied decision must remain closed");
    }

    #[test]
    fn raw_call_argument_shape_accepts_zero_one_and_supported_pairs() {
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &[])
            .expect("zero arguments must be accepted");
        for value in [
            RuntimeValue::Boolean(true),
            RuntimeValue::Integer(1),
            RuntimeValue::BigInt(2),
            RuntimeValue::Float(RuntimeFloat::new(3.5).expect("finite Float argument")),
            RuntimeValue::Text("text".to_string()),
            RuntimeValue::Bytes(vec![0x00, 0xff]),
        ] {
            let argument = FunctionArgument::new(RAW_CALL_PARAMETER, value)
                .expect("supported scalar argument is valid");
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&argument))
                .expect("one supported scalar argument must be accepted");
        }
        let reference = FunctionArgument::new(
            RAW_CALL_PARAMETER,
            RuntimeValue::Reference {
                target: TypeId::from_bytes([0x65; 16]),
                object: ObjectId::from_bytes([0x66; 16]),
            },
        )
        .expect("Reference argument is valid");
        assert_eq!(reference.parameter(), RAW_CALL_PARAMETER);
        assert_eq!(
            reference.value(),
            &RuntimeValue::Reference {
                target: TypeId::from_bytes([0x65; 16]),
                object: ObjectId::from_bytes([0x66; 16]),
            }
        );
        validate_raw_call_argument_shape(RAW_CALL_FUNCTION, std::slice::from_ref(&reference))
            .expect("one Reference argument must be accepted");

        let supported = [
            RuntimeValue::Boolean(false),
            RuntimeValue::Integer(1),
            RuntimeValue::BigInt(2),
            RuntimeValue::Float(RuntimeFloat::new(3.5).expect("finite Float argument")),
            RuntimeValue::Text("text".to_string()),
            RuntimeValue::Bytes(vec![0x00, 0xff]),
            reference.value().clone(),
        ];
        for (index, value) in supported.into_iter().enumerate() {
            let pair = [
                FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                    .expect("Boolean argument is valid"),
                FunctionArgument::new(ParameterId::from_bytes([0x70 + index as u8; 16]), value)
                    .expect("supported pair argument is valid"),
            ];
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &pair)
                .expect("a pair of supported arguments must be accepted");
        }
    }

    #[test]
    fn raw_call_argument_shape_rejects_other_argument_sets() {
        let enum_type = TypeId::from_bytes([0x67; 16]);
        let catalogue = CatalogueSnapshot::new_with_enum_types(
            CatalogueRevisionId::new(),
            vec![SchemaDefinition::new(
                orna_core::SchemaId::new(),
                QualifiedSemanticName::new(["app"]).expect("schema name"),
            )],
            Vec::new(),
            Vec::new(),
            vec![EnumTypeDefinition::new(
                enum_type,
                QualifiedSemanticName::new(["app", "stage"]).expect("qualified enum name"),
                ["lead"],
            )],
            Vec::new(),
        )
        .expect("enum catalogue");
        let enum_argument = FunctionArgument::new(
            RAW_CALL_PARAMETER,
            RuntimeValue::Enum(
                EnumValue::new(&catalogue, enum_type, "lead").expect("declared enum label"),
            ),
        )
        .expect("Enum argument is valid");
        assert!(matches!(
            validate_raw_call_argument_shape(
                RAW_CALL_FUNCTION,
                std::slice::from_ref(&enum_argument),
            )
            .expect_err("one Enum argument must be rejected"),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
            }
        ));

        let unsupported_pair = [
            FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                .expect("Boolean argument is valid"),
            enum_argument.clone(),
        ];
        assert!(matches!(
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &unsupported_pair)
                .expect_err("a pair with an Enum argument must be rejected"),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
            }
        ));

        let three = [
            FunctionArgument::new(RAW_CALL_PARAMETER, RuntimeValue::Boolean(true))
                .expect("Boolean argument is valid"),
            FunctionArgument::new(
                ParameterId::from_bytes([0x64; 16]),
                RuntimeValue::Boolean(false),
            )
            .expect("Boolean argument is valid"),
            FunctionArgument::new(
                ParameterId::from_bytes([0x65; 16]),
                RuntimeValue::Boolean(true),
            )
            .expect("Boolean argument is valid"),
        ];
        assert!(matches!(
            validate_raw_call_argument_shape(RAW_CALL_FUNCTION, &three)
                .expect_err("three arguments must be rejected"),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw calls accept zero arguments, one supported value, or one supported argument pair",
            }
        ));
    }

    #[test]
    fn raw_insert_argument_errors_classify_to_generic_unavailable() {
        let argument_error = PostgresKernelError::ServerInsert(
            crate::ServerInsertError::Argument {
                parameter: Some(RAW_CALL_PARAMETER),
                rule: "an argument was supplied for a parameter that this function does not declare",
            },
        );
        assert!(matches!(
            classify_raw_server_insert_error(argument_error, true, RAW_CALL_FUNCTION),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw SERVER INSERT argument target is unavailable",
            }
        ));

        let missing_required =
            PostgresKernelError::ServerInsert(crate::ServerInsertError::Argument {
                parameter: Some(RAW_CALL_PARAMETER),
                rule: "a required argument is missing",
            });
        assert!(matches!(
            classify_raw_server_insert_error(missing_required, false, RAW_CALL_FUNCTION),
            PostgresKernelError::RawCallTargetUnavailable {
                function: RAW_CALL_FUNCTION,
                rule: "raw SERVER INSERT argument target is unavailable",
            }
        ));
    }

    #[test]
    fn sealed_server_error_classification_preserves_internal_failures() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let target =
            PostgresKernelError::ServerUpdate(crate::ServerUpdateError::FunctionNotActive {
                pair,
                function: RAW_CALL_FUNCTION,
            });
        assert_eq!(
            classify_sealed_server_error(&target),
            SealedInvocationFailureClass::Target
        );

        let internal = PostgresKernelError::ServerUpdate(crate::ServerUpdateError::Unavailable {
            source: Box::new(PostgresKernelError::DurableInvariant {
                relation: "test relation",
                record: "test record".to_owned(),
                rule: "test rule",
            }),
        });
        assert_eq!(
            classify_sealed_server_error(&internal),
            SealedInvocationFailureClass::Internal
        );
    }

    #[test]
    fn raw_insert_parameter_free_target_failure_stays_typed() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let target_error =
            PostgresKernelError::ServerInsert(crate::ServerInsertError::FunctionNotActive {
                pair,
                function: RAW_CALL_FUNCTION,
            });
        assert!(matches!(
            classify_raw_server_insert_error(target_error, false, RAW_CALL_FUNCTION),
            PostgresKernelError::RawServerTargetUnavailable {
                source: RawServerTargetError::Insert(
                    crate::ServerInsertError::FunctionNotActive {
                        pair: actual_pair,
                        function: RAW_CALL_FUNCTION,
                    },
                ),
            } if actual_pair == pair
        ));
    }

    #[test]
    fn raw_insert_operational_error_stays_unchanged() {
        let operational = PostgresKernelError::ServerInsert(crate::ServerInsertError::Kernel {
            source: Box::new(PostgresKernelError::DurableInvariant {
                relation: "test relation",
                record: "test record".to_owned(),
                rule: "test rule",
            }),
        });
        assert!(matches!(
            classify_raw_server_insert_error(operational, true, RAW_CALL_FUNCTION),
            PostgresKernelError::ServerInsert(crate::ServerInsertError::Kernel {
                source,
            }) if matches!(
                *source,
                PostgresKernelError::DurableInvariant {
                    relation: "test relation",
                    ref record,
                    rule: "test rule",
                } if record == "test record"
            )
        ));
    }

    #[test]
    fn raw_insert_value_codec_error_stays_unchanged_with_arguments_present() {
        let unsupported = PostgresKernelError::ServerInsert(crate::ServerInsertError::ValueCodec(
            orna_protocol::ValueCodecError::UnsupportedValue,
        ));
        assert!(matches!(
            classify_raw_server_insert_error(unsupported, true, RAW_CALL_FUNCTION),
            PostgresKernelError::ServerInsert(crate::ServerInsertError::ValueCodec(
                orna_protocol::ValueCodecError::UnsupportedValue,
            ))
        ));
    }

    #[test]
    fn raw_insert_unique_reference_conflict_stays_typed_with_arguments_present() {
        const CONFLICT_OWNER: TypeId = TypeId::from_bytes([0x41; 16]);
        const CONFLICT_FIELD: FieldId = FieldId::from_bytes([0x42; 16]);
        const CONFLICT_REFERENCED: TypeId = TypeId::from_bytes([0x43; 16]);
        let config_error = "port=invalid"
            .parse::<tokio_postgres::Config>()
            .expect_err("invalid port must fail to parse");
        let conflict =
            PostgresKernelError::ServerInsert(crate::ServerInsertError::UniqueReferenceConflict {
                owner: CONFLICT_OWNER,
                field: CONFLICT_FIELD,
                referenced_type: CONFLICT_REFERENCED,
                source: config_error,
            });
        assert!(matches!(
            classify_raw_server_insert_error(conflict, true, RAW_CALL_FUNCTION),
            PostgresKernelError::ServerInsert(crate::ServerInsertError::UniqueReferenceConflict {
                owner: CONFLICT_OWNER,
                field: CONFLICT_FIELD,
                referenced_type: CONFLICT_REFERENCED,
                source,
            }) if source.as_db_error().is_none()
        ));
    }

    #[test]
    fn raw_call_results_transfer_owned_values_in_execution_order() {
        let client = AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(true));
        assert_eq!(client.into_values(), vec![RuntimeValue::Boolean(true)]);

        let server = AuthenticatedRawCallResult::Server(vec![
            RuntimeValue::Integer(1),
            RuntimeValue::Integer(2),
        ]);
        assert_eq!(
            server.into_values(),
            vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)]
        );
    }

    #[tokio::test]
    async fn empty_record_preflight_does_not_open_postgres() {
        let kernel = "host=127.0.0.1 port=1 dbname=absent"
            .parse::<PostgresKernel>()
            .expect("unavailable test configuration is valid");

        assert_eq!(
            kernel.preflight_record_arguments(Vec::new()).await.unwrap(),
            RecordArgumentPreflight::NotRequired,
        );
    }
    use orna_core::security::ExecuteDenial;

    #[test]
    fn audit_denial_decoder_maps_the_complete_closed_vocabulary() {
        let authentication = [
            (
                "authentication_unknown_uid",
                LocalPeerAuthenticationError::UnknownUid,
            ),
            (
                "authentication_unknown_session_principal",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::UnknownSessionPrincipal,
                ),
            ),
            (
                "authentication_disabled_session_principal",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::DisabledSessionPrincipal,
                ),
            ),
            (
                "authentication_role_cannot_authenticate",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::RoleCannotAuthenticate,
                ),
            ),
            (
                "authentication_duplicate_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::DuplicateActiveRole,
                ),
            ),
            (
                "authentication_unknown_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::UnknownActiveRole,
                ),
            ),
            (
                "authentication_disabled_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::DisabledActiveRole,
                ),
            ),
            (
                "authentication_active_principal_is_not_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::ActivePrincipalIsNotRole,
                ),
            ),
            (
                "authentication_unreachable_active_role",
                LocalPeerAuthenticationError::InvalidPrincipal(
                    SessionBindingError::UnreachableActiveRole,
                ),
            ),
        ];
        for (stored, expected) in authentication {
            assert_eq!(encode_authentication_audit_denial(expected), stored);
            assert_eq!(
                decode_authentication_audit_denial(stored.to_owned(), "41")
                    .expect("closed authentication reason must decode"),
                expected
            );
        }

        for (stored, expected) in [
            ("execute_invalid_session", ExecuteDenial::InvalidSession),
            ("execute_unknown_function", ExecuteDenial::UnknownFunction),
            ("execute_revision_mismatch", ExecuteDenial::RevisionMismatch),
            ("execute_missing_grant", ExecuteDenial::MissingExecuteGrant),
        ] {
            assert_eq!(encode_execute_audit_denial(expected), stored);
            assert_eq!(
                decode_execute_audit_denial(stored.to_owned(), "42")
                    .expect("closed EXECUTE reason must decode"),
                expected
            );
        }

        assert!(matches!(
            decode_authentication_audit_denial("authentication_other".to_owned(), "43"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "authentication denial reason is unsupported",
            }) if record == "43"
        ));
        assert!(matches!(
            decode_execute_audit_denial("execute_other".to_owned(), "44"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "EXECUTE denial reason is unsupported",
            }) if record == "44"
        ));
    }

    #[test]
    fn capability_audit_denial_codec_round_trips_the_redacted_qualified_name() {
        assert_eq!(
            encode_security_audit_kind(SecurityAuditKind::Authentication),
            "authentication"
        );
        assert_eq!(
            encode_security_audit_kind(SecurityAuditKind::Execute),
            "execute"
        );
        assert_eq!(
            encode_security_audit_kind(SecurityAuditKind::Capability),
            "capability"
        );
        assert_eq!(
            encode_security_audit_kind(SecurityAuditKind::SecurityAdmin),
            "security_admin"
        );

        for name in [
            "std.fs.read",
            "std.fs.write",
            "std.net.connect",
            "std.secret.use",
        ] {
            let stored = encode_capability_audit_denial(name);
            assert_eq!(stored, format!("capability:{name}"));
            assert_eq!(
                decode_capability_audit_denial(stored, "50")
                    .expect("redacted capability name must decode"),
                name
            );
        }

        assert!(matches!(
            decode_capability_audit_denial("execute_missing_grant".to_owned(), "51"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "capability denial reason is unsupported",
            }) if record == "51"
        ));
    }

    #[test]
    fn capability_audit_decisions_encode_the_redacted_name_for_both_outcomes() {
        let target = InvocationTarget::new(
            FunctionId::from_bytes([0x91; 16]),
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x92; 16]),
                CatalogueRevisionId::from_bytes([0x93; 16]),
            ),
        );
        let principal = PrincipalId::from_bytes([0x94; 16]);
        let allowed =
            SecurityAuditDecision::recover_capability_allowed(principal, target, "std.fs.read")
                .expect("closed capability name is valid");
        let denied =
            SecurityAuditDecision::recover_capability_denied(principal, target, "std.secret.use")
                .expect("closed capability name is valid");

        let encode = |decision: &SecurityAuditDecision| match decision.denial() {
            None => decision
                .capability_name()
                .map(encode_capability_audit_denial),
            Some(SecurityAuditDenial::Authentication(reason)) => {
                Some(encode_authentication_audit_denial(reason).to_owned())
            }
            Some(SecurityAuditDenial::Execute(reason)) => {
                Some(encode_execute_audit_denial(reason).to_owned())
            }
            Some(SecurityAuditDenial::Capability { capability }) => {
                Some(encode_capability_audit_denial(&capability))
            }
            Some(SecurityAuditDenial::Inspect(reason)) => {
                Some(encode_inspect_audit_denial(reason).to_owned())
            }
            Some(SecurityAuditDenial::SecurityAdmin(reason)) => {
                encode_security_admin_audit_denied_detail(decision, reason)
            }
        };

        assert_eq!(encode(&allowed), Some("capability:std.fs.read".to_owned()));
        assert_eq!(
            encode(&denied),
            Some("capability:std.secret.use".to_owned())
        );
        assert_eq!(allowed.kind(), SecurityAuditKind::Capability);
        assert_eq!(denied.kind(), SecurityAuditKind::Capability);
        assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
        assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
        assert_eq!(allowed.target(), Some(target));
        assert_eq!(denied.target(), Some(target));
    }

    #[test]
    fn source_apply_audit_codec_preserves_candidate_pair_and_committed_detail() {
        let principal = PrincipalId::from_bytes([0xa5; 16]);
        let candidate = RevisionPair::new(
            SourceRevisionId::from_bytes([0xa6; 16]),
            CatalogueRevisionId::from_bytes([0xa7; 16]),
        );
        let decision = SecurityAuditDecision::recover_source_apply_allowed(principal, candidate);

        assert_eq!(encode_security_audit_kind(decision.kind()), "source_apply");
        assert_eq!(encode_source_apply_audit_detail(), "source_apply:committed");
        decode_source_apply_audit_detail("source_apply:committed", "source-apply")
            .expect("committed source apply detail must decode");
        assert_eq!(
            encode_security_audit_identity_columns(&decision),
            (
                None,
                Some(candidate.source().to_bytes().to_vec()),
                Some(candidate.catalogue().to_bytes().to_vec()),
            )
        );
        assert_eq!(decision.outcome(), SecurityAuditOutcome::Allowed);
        assert_eq!(decision.session_principal(), Some(principal));
        assert_eq!(decision.source_apply_candidate(), Some(candidate));
        assert_eq!(decision.target(), None);
        assert_eq!(decision.denial(), None);
    }

    #[test]
    fn security_admin_audit_codec_round_trips_operation_and_denial() {
        let principal = PrincipalId::from_bytes([0x95; 16]);
        let snapshot = SecuritySnapshot::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x96; 16]),
                CatalogueRevisionId::from_bytes([0x97; 16]),
            ),
            vec![],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
        )
        .expect("security-admin codec snapshot is valid");
        let session = snapshot
            .bind_authenticated_session(principal, vec![])
            .expect("security-admin codec session binds");
        let operation = SecurityAdminAuditOperation::GrantPrivilege;
        let target = SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID;

        let allowed = SecurityAuditDecision::security_admin_allowed(
            &session,
            PrivilegeDecision::Allowed {
                requested: PrivilegeClass::SecurityAdmin,
            },
            operation,
            target,
        )
        .expect("allowed security-admin decision must construct");
        assert_eq!(
            encode_security_admin_audit_detail(operation),
            "security_admin:grant_privilege"
        );
        assert_eq!(
            decode_security_admin_audit_detail("security_admin:grant_privilege", "60")
                .expect("allowed security-admin detail must decode"),
            operation
        );
        assert!(matches!(
            decode_security_admin_audit_detail("security_admin:grant_privilege:missing-privilege", "61"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "allowed security-admin audit detail must carry only the operation",
            }) if record == "61"
        ));
        assert!(matches!(
            decode_security_admin_audit_detail("execute_missing_grant", "62"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "security-admin audit detail must start with security_admin:",
            }) if record == "62"
        ));
        assert_eq!(allowed.kind(), SecurityAuditKind::SecurityAdmin);
        assert_eq!(allowed.outcome(), SecurityAuditOutcome::Allowed);
        assert_eq!(allowed.security_admin_operation(), Some(operation));
        assert_eq!(allowed.security_admin_target(), Some(target));

        let reason = PrivilegeDenial::MissingPrivilege {
            requested: PrivilegeClass::SecurityAdmin,
        };
        let denied = SecurityAuditDecision::security_admin_denied(
            &session,
            SecurityAdminAuditOperation::CreatePrincipal,
            SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
            reason,
        );
        let stored = encode_security_admin_audit_denied_detail(&denied, reason)
            .expect("denied security-admin decision must carry its operation");
        assert_eq!(stored, "security_admin:create_principal:missing-privilege");
        let (operation, decoded_reason) = decode_security_admin_audit_denial(&stored, "63")
            .expect("denied security-admin detail must decode");
        assert_eq!(operation, SecurityAdminAuditOperation::CreatePrincipal);
        assert_eq!(decoded_reason, reason);
        assert!(matches!(
            decode_security_admin_audit_denial("security_admin:create_principal:granted", "64"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "security-admin denial reason is unsupported",
            }) if record == "64"
        ));
        assert!(matches!(
            decode_security_admin_audit_denial("security_admin:create_principal", "65"),
            Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                ref record,
                rule: "security-admin denial reason must carry an operation and a reason",
            }) if record == "65"
        ));
        assert_eq!(denied.kind(), SecurityAuditKind::SecurityAdmin);
        assert_eq!(denied.outcome(), SecurityAuditOutcome::Denied);
        assert_eq!(denied.security_admin_denial(), Some(reason));
        assert_eq!(
            denied.denial(),
            Some(SecurityAuditDenial::SecurityAdmin(reason))
        );
    }

    #[test]
    fn expected_security_result_does_not_hide_session_shutdown_failure() {
        let operation: Result<Result<(), LocalPeerAuthenticationError>, PostgresKernelError> =
            Ok(Err(LocalPeerAuthenticationError::UnknownUid));
        let shutdown = PostgresKernelError::DurableInvariant {
            relation: "test session",
            record: "shutdown".to_owned(),
            rule: "driver failed during shutdown",
        };

        assert!(matches!(
            finish_security_session(operation, Err(shutdown)),
            Err(PostgresKernelError::DurableInvariant {
                relation: "test session",
                ref record,
                rule: "driver failed during shutdown",
            }) if record == "shutdown"
        ));

        let operation: Result<Result<(), PostgresKernelError>, PostgresKernelError> =
            Ok(Err(PostgresKernelError::ClientExecuteDenied {
                pair: RevisionPair::new(
                    SourceRevisionId::from_bytes([0x11; 16]),
                    CatalogueRevisionId::from_bytes([0x12; 16]),
                ),
                function: FunctionId::from_bytes([0x13; 16]),
                reason: ExecuteDenial::MissingExecuteGrant,
            }));
        let shutdown = PostgresKernelError::DurableInvariant {
            relation: "test session",
            record: "shutdown".to_owned(),
            rule: "driver failed during shutdown",
        };
        assert!(matches!(
            finish_security_session(operation, Err(shutdown)),
            Err(PostgresKernelError::DurableInvariant {
                relation: "test session",
                ref record,
                rule: "driver failed during shutdown",
            }) if record == "shutdown"
        ));
    }

    #[test]
    fn sealed_completed_events_carry_the_value_in_the_final_value_batch() {
        let principal = PrincipalId::from_bytes([0x51; 16]);
        let invocation = InvocationId::from_bytes([0x52; 16]);
        let events = sealed_completed_events(principal, invocation, RuntimeValue::Integer(41))
            .expect("the completed events are valid");
        let records = events.records();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].outer_sequence(), 1);
        assert_eq!(records[1].outer_sequence(), 2);
        assert_eq!(records[2].outer_sequence(), 3);
        assert!(matches!(
            records[0].event().body(),
            InvocationEventBody::Started {
                visible_principal: None
            }
        ));
        match records[1].event().body() {
            InvocationEventBody::ValueBatch { schema, values } => {
                assert!(schema.is_none());
                let [value] = values.as_slice() else {
                    panic!("the ValueBatch must carry exactly one value");
                };
                assert_eq!(value.value(), &RuntimeValue::Integer(41));
            }
            other => panic!("expected a ValueBatch event, got {other:?}"),
        }
        assert!(matches!(
            records[2].event().body(),
            InvocationEventBody::Completed {
                duration_nanoseconds: 0
            }
        ));
    }

    #[test]
    fn sealed_completed_events_carry_all_server_values_in_one_batch() {
        let events = sealed_completed_events_from_values(
            PrincipalId::from_bytes([0x61; 16]),
            InvocationId::from_bytes([0x62; 16]),
            vec![RuntimeValue::Integer(1), RuntimeValue::Integer(2)],
        )
        .expect("server values form a valid event batch");
        assert_eq!(events.records().len(), 3);
        assert_eq!(events.records()[0].event().sequence(), 0);
        assert_eq!(events.records()[1].event().sequence(), 1);
        assert_eq!(events.records()[2].event().sequence(), 2);
        let InvocationEventBody::ValueBatch { schema, values } = events.records()[1].event().body()
        else {
            panic!("expected one server ValueBatch event");
        };
        assert!(schema.is_none());
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].value(), &RuntimeValue::Integer(1));
        assert_eq!(values[1].value(), &RuntimeValue::Integer(2));
    }

    #[test]
    fn sealed_completed_events_allow_an_empty_server_result() {
        let events = sealed_completed_events_from_values(
            PrincipalId::from_bytes([0x63; 16]),
            InvocationId::from_bytes([0x64; 16]),
            Vec::new(),
        )
        .expect("an empty server result still completes the invocation");
        assert_eq!(events.records().len(), 2);
        assert_eq!(events.records()[0].event().sequence(), 0);
        assert_eq!(events.records()[1].event().sequence(), 1);
        assert!(matches!(
            events.records()[1].event().body(),
            InvocationEventBody::Completed {
                duration_nanoseconds: 0
            }
        ));
    }

    #[test]
    fn sealed_failure_events_are_redacted_and_closed() {
        let invocation = InvocationId::from_bytes([0x71; 16]);
        let events = sealed_failure_events(invocation, SealedInvocationFailureClass::Target)
            .expect("the failure events are valid");
        assert_eq!(events.records().len(), 2);
        assert!(matches!(
            events.records()[0].event().body(),
            InvocationEventBody::Started {
                visible_principal: None
            }
        ));
        let InvocationEventBody::Failed(failure) = events.records()[1].event().body() else {
            panic!("expected an InvocationFailed event");
        };
        assert_eq!(failure.phase(), InvocationFailurePhase::Target);
        assert_eq!(failure.code(), "INVOKE_TARGET_FAILED");
        assert_eq!(failure.message(), "invocation target failed");
        assert!(failure.details().is_none());
        assert_eq!(failure.retryability(), InvocationRetryability::Unknown);
    }
    #[test]
    fn resource_targets_resolve_and_authorize_with_closed_class_pins() {
        use orna_core::{
            SchemaId,
            catalogue::{
                FunctionSecurity, FunctionTransaction, FunctionVolatility, SchemaDefinition,
            },
            security::{ExecuteGrant, Principal, PrincipalKind, PrincipalStatus},
            types::{ResolvedType, StandardScalar},
        };

        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0xa1; 16]),
            CatalogueRevisionId::from_bytes([0xa2; 16]),
        );
        let principal = PrincipalId::from_bytes([0xa3; 16]);
        let application_function = FunctionId::from_bytes([0xa4; 16]);
        let application_revision = FunctionRevisionId::from_bytes([0xa5; 16]);
        let application = FunctionDefinition::new(
            application_function,
            QualifiedSemanticName::new(["app", "resource"]).expect("application name"),
            FunctionDomain::Server,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            application_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        );
        let application_catalogue = CatalogueSnapshot::new_with_functions(
            pair.catalogue(),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0xa6; 16]),
                QualifiedSemanticName::new(["app"]).expect("application schema"),
            )],
            Vec::new(),
            vec![application],
        )
        .expect("application catalogue");
        let application_target = resolve_resource_target_in_catalogues(
            pair,
            &application_catalogue,
            None,
            application_function,
        )
        .expect("application resource target");
        assert_eq!(
            application_target.target(),
            InvocationTarget::new(application_function, pair)
        );
        let application_security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::application(application_function)],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            Vec::new(),
            vec![ExecuteGrant::new(principal, application_function)],
        )
        .expect("application security snapshot");
        let session = application_security
            .bind_authenticated_session(principal, Vec::new())
            .expect("application session");
        assert!(matches!(
            application_security.authorise_execute(&session, application_target.target()),
            ExecuteDecision::Allowed(_)
        ));

        let standard = orna_standard::verify_standard_library_v2_snapshot(
            orna_standard::retained_standard_library_v2_snapshot().expect("standard fixture"),
        )
        .expect("verified standard fixture");
        let standard_function = STD_INVOKE_ECHO_FUNCTION_ID;
        let standard_definition = standard
            .catalogue()
            .function_by_id(standard_function)
            .expect("standard echo definition");
        let standard_executable = standard
            .executables()
            .iter()
            .find(|executable| executable.function() == standard_function)
            .expect("standard echo executable");
        assert_eq!(
            standard_executable.revision().id(),
            standard_definition.current_revision()
        );
        let empty_catalogue = CatalogueSnapshot::new_with_functions(
            pair.catalogue(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("empty application catalogue");
        let standard_target = resolve_resource_target_in_catalogues(
            pair,
            &empty_catalogue,
            Some(&standard),
            standard_function,
        )
        .expect("verified standard resource target");
        let expected_standard_target = InvocationTarget::verified_standard(
            standard_function,
            pair,
            standard.revision(),
            standard_executable.revision().id(),
        );
        assert_eq!(standard_target.target(), expected_standard_target);
        let standard_security = SecuritySnapshot::new_with_function_targets(
            pair,
            vec![SecurityFunctionTarget::verified_standard(
                standard_function,
                standard.revision(),
                standard_executable.revision().id(),
            )],
            vec![Principal::new(
                principal,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            Vec::new(),
            vec![ExecuteGrant::new(principal, standard_function)],
        )
        .expect("standard security snapshot");
        let session = standard_security
            .bind_authenticated_session(principal, Vec::new())
            .expect("standard session");
        assert!(matches!(
            standard_security.authorise_execute(&session, expected_standard_target),
            ExecuteDecision::Allowed(_)
        ));
        assert_eq!(
            standard_security
                .authorise_execute(&session, InvocationTarget::new(standard_function, pair),),
            ExecuteDecision::Denied(ExecuteDenial::UnknownFunction)
        );
    }

    #[test]
    fn dropping_resource_producer_requests_cancellation() {
        let cancellation = ResourceCancellation::new();
        let (commands, _receiver) = tokio::sync::mpsc::channel(1);
        let producer = AuthenticatedServerResourceProducer {
            accepted: AuthenticatedServerResourceAccepted {
                stream_id: 1,
                request_id: InvocationId::from_bytes([0x91; 16]),
                nested_invocation_id: InvocationId::from_bytes([0x92; 16]),
                target_revision: RevisionPair::new(
                    SourceRevisionId::from_bytes([0x93; 16]),
                    CatalogueRevisionId::from_bytes([0x94; 16]),
                ),
                resource_kind: AuthenticatedServerResourceKind::Stream,
            },
            commands,
            cancellation: cancellation.clone(),
        };

        drop(producer);

        assert!(cancellation.is_requested());
    }

    #[test]
    fn dropping_resource_producer_start_guard_requests_cancellation() {
        let cancellation = ResourceCancellation::new();
        {
            let _guard = ResourceProducerStartGuard::new(cancellation.clone());
        }

        assert!(cancellation.is_requested());
    }

    #[test]
    fn resource_credit_is_nonzero_and_bounded() {
        assert!(ResourceCredit::new(1, 1).is_some());
        assert!(ResourceCredit::new(0, 1).is_none());
        assert!(ResourceCredit::new(1, 0).is_none());
        assert!(ResourceCredit::new(MAX_RESOURCE_CREDIT + 1, 1).is_none());
        assert!(ResourceCredit::new(1, MAX_RESOURCE_CREDIT + 1).is_none());
    }

    #[test]
    fn resource_cancellation_wins_before_terminal_commit() {
        let cancellation = ResourceCancellation::new();

        assert!(cancellation.request_cancel());
        assert!(!cancellation.try_begin_commit());
        assert!(!cancellation.request_cancel());
    }

    #[test]
    fn resource_acceptance_commit_preserves_cancellation() {
        let cancellation = ResourceCancellation::new();

        assert!(cancellation.try_begin_acceptance_commit());
        assert!(!cancellation.request_cancel());
        assert!(cancellation.is_acceptance_cancellation_requested());
        assert!(cancellation.is_requested());
        cancellation.acceptance_commit_finished();
        assert!(!cancellation.is_acceptance_cancellation_requested());
        assert!(cancellation.is_requested());
        assert!(!cancellation.try_begin_commit());
    }

    #[test]
    fn resource_terminal_commit_wins_over_late_cancellation() {
        let cancellation = ResourceCancellation::new();

        assert!(cancellation.try_begin_commit());
        assert!(!cancellation.request_cancel());
        cancellation.commit_finished();
        assert!(!cancellation.try_begin_commit());
    }
}
