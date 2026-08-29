// These internal execution seams preserve the accepted error and state layouts.
#![allow(clippy::large_enum_variant)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
#![allow(clippy::let_and_return)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::question_mark)]
#![allow(clippy::nonminimal_bool)]
#![allow(clippy::match_like_matches_macro)]
#[path = "security/audit.rs"]
mod audit;
#[path = "security/persistence.rs"]
mod persistence;
#[path = "security/resource.rs"]
mod resource;
#[path = "security/resource_cancellation.rs"]
mod resource_cancellation;
#[path = "security/resource_stream.rs"]
mod resource_stream;

pub(crate) use audit::encode_principal_kind;
use audit::*;
use persistence::*;
pub(crate) use persistence::{
    encode_privilege_class, recover_invocation_audit_events, recover_security_snapshot_for_active,
};
pub use resource::{
    AuthenticatedServerResourceAccepted, AuthenticatedServerResourceEvent,
    AuthenticatedServerResourceKind, AuthenticatedServerResourceProducer,
    AuthenticatedServerResourceResult, AuthenticatedServerResourceStart, ResourceCredit,
};

pub use resource_cancellation::ResourceCancellation;
#[cfg(test)]
use resource_stream::{
    bind_authenticated_resource_arguments, classify_sealed_server_error,
    resource_target_shape_is_supported, resource_values_from_server_result,
    sealed_server_result_kind, sealed_server_stream_completed_event,
};
use resource_stream::{
    execute_sealed_server_after_audit, resource_target_security_is_supported,
    sealed_server_target_is_mutation, start_sealed_server_stream_producer,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::SystemTime,
};

const MAX_RESOURCE_CREDIT: u64 = 1024 * 1024 * 1024;

const LOCAL_USER_PRINCIPAL_DOMAIN: &[u8] = b"ornadb.local-user.v1\0";
const LOCAL_HEALTH_UID: u32 = u32::MAX;

use orna_artifact::client_plan::{CAPABILITY_FORMAT_VERSION, CapabilityClientPlan, ResourceKind};
use orna_client::{
    ClientExecutionError, ClientExecutionResult, ClientResourceCompletion,
    ClientResourceExecutionError, ClientResourceExecutor, ClientStateContext, ClientStateStore,
    client_function_arguments_match, client_security_context_digest,
    evaluate_client_function_in_state_context_with_grants_and_arguments as evaluate_authorised_client_function_with_state_context_and_arguments,
    evaluate_client_function_in_state_context_with_grants_and_arguments_and_executor_with_parent_invocation as evaluate_authorised_client_function_with_state_context_and_arguments_and_executor,
    evaluate_client_function_with_grants as evaluate_authorised_client_function,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, FunctionRevisionId, InspectEpochId, InvocationAuditEventId,
    InvocationId, ObjectId, PrincipalId, SecurityAuditEventId, SourceRevisionId,
    StandardLibraryRevisionId,
    catalogue::{FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity},
    inspect::{InspectOutcomeKind, InspectPrivilege, InspectSnapshotOptions},
    invocation::{
        InvocationArgument, InvocationClientOffer, InvocationEventBody, InvocationFailure,
        InvocationFailurePhase, InvocationOutputRequirement, InvocationParameterSelector,
        InvocationRetryability, InvocationTarget as InvocationRequestTarget, InvokeEvent,
        InvokeValue, ProtectedInvocationDecision, decide_protected_invocation,
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
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_EFFECTIVE_PRINCIPAL_FUNCTION_ID,
        SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID, SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
        SYS_SECURITY_PRINCIPAL_TYPE_ID, SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID, SYS_SECURITY_SESSION_PRINCIPAL_FUNCTION_ID,
        SystemFunctionDefinition, SystemFunctionKind, system_function_by_id,
        system_function_by_name,
    },
    types::TypeDescriptor,
    value::{
        FunctionArgument, OpaqueCodecRegistry, RecordValue, ResultRows, RuntimeType, RuntimeValue,
    },
};
use orna_protocol::{
    CallFailure, InvocationEventBatch, InvocationEventRecord, ResourceArgument,
    ResourceKind as ProtocolResourceKind, ResourceRequest, RetainedInvokeRequest,
    decode_retained_invoke_request, encode_active_value, encode_rows_value,
};
use orna_standard::{
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID, STD_INVOKE_ECHO_FUNCTION_ID,
    STD_JSON_ENCODE_FUNCTION_ID, registered_opaque_codecs,
};
use sha2::{Digest, Sha256};
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
        load_client_reference_loader, present_sealed_standard_output,
        raw_identity_selected_server_select_target_is_selected, raw_server_target_is_unavailable,
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
    SealedFailed(ResourceProducerSealedFailed),
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
pub(crate) struct ResourceProducerSealedFailed {
    response: Option<
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    >,
    failure: SealedInvocationFailureClass,
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
    #[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
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
#[derive(Debug)]
pub enum SealedInvocationExecution {
    /// A normal sealed invocation result (including redacted failures).
    Result(SealedInvocationResult),
    /// A bounded SERVER stream whose values are pulled by the raw adapter.
    ServerStream(AuthenticatedServerResourceProducer),
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
                        if !sealed_target_security_is_supported(target) {
                            SealedInvocationPreparedOutcome::TargetDenied {
                                security_target: Some(security_target),
                                denial: Some(ExecuteDenial::UnsupportedSecurityDefiner),
                            }
                        } else {
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
    /// Long-lived SERVER stream producers are spawned on `resource_runtime`,
    /// not on the short-lived worker runtime that calls this method.
    #[doc(hidden)]
    pub async fn execute_after_started(
        &mut self,
        resource_executor: Option<&mut dyn ClientResourceExecutor>,
        state: &mut ClientStateStore,
        capability_audit_appended: &mut bool,
        cancellation: &ResourceCancellation,
        resource_runtime: tokio::runtime::Handle,
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
        // Native STREAM and accepted mutation ROWS targets use a live
        // producer. Read-only ROWS targets use the existing sealed executor.
        if let SealedInvocationPreparedOutcome::Allowed {
            target: PreparedSealedTarget::Application { definition },
            authorisation,
            ..
        } = &self.outcome
            && definition.domain() == FunctionDomain::Server
            && (matches!(definition.return_type(), FunctionReturn::Stream(_))
                || (matches!(definition.return_type(), FunctionReturn::Rows(_))
                    && sealed_server_target_is_mutation(&self.active, definition.id())))
        {
            let arguments = match bind_sealed_invoke_arguments(definition, self.decoded.arguments())
            {
                Ok(arguments) => arguments,
                Err(_) => {
                    return Ok(SealedInvocationExecution::Result(sealed_failure_result(
                        self.invocation,
                        SealedInvocationFailureClass::Bind,
                    )?));
                }
            };
            let producer = start_sealed_server_stream_producer(
                self.kernel.clone(),
                self.active.clone(),
                self.security.clone(),
                authorisation.clone(),
                arguments,
                self.invocation,
                cancellation.clone(),
                resource_runtime,
            )
            .await;
            return match producer {
                Ok(producer) => Ok(SealedInvocationExecution::ServerStream(producer)),
                Err(failure) => Ok(SealedInvocationExecution::Result(sealed_failure_result(
                    self.invocation,
                    failure,
                )?)),
            };
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
            let decision = match decision {
                ExecuteDecision::Allowed(_)
                    if active
                        .catalogue()
                        .function_by_id(function)
                        .is_some_and(|definition| {
                            definition.domain() == FunctionDomain::Server
                                && !resource_target_security_is_supported(definition)
                        }) => ExecuteDecision::Denied(ExecuteDenial::UnsupportedSecurityDefiner),
                decision => decision,
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
                            if !client_function_arguments_match(&active, definition, arguments) {
                                Err(raw_call_target_unavailable(
                                    function,
                                    "raw CLIENT arguments do not match the declared parameter set",
                                ))
                            } else {
                                match load_client_reference_loader(
                                    &transaction,
                                    &active,
                                    authorisation.session_principal(),
                                    client_security_context_digest(&authorisation),
                                    arguments,
                                )
                                .await
                                {
                                    Ok(loader) => {
                                        let mut state = ClientStateStore::new();
                                        state.install_reference_loader(loader);
                                        let state_context =
                                            ClientStateContext::default_for(definition.id());
                                        evaluate_authorised_client_function_with_state_context_and_arguments(
                                            &active,
                                            &authorisation,
                                            &state_context,
                                            arguments,
                                            &[],
                                            &self.capability_grants,
                                            &mut state,
                                        )
                                        .map(|result| {
                                            AuthenticatedRawCallResult::Client(result.into_value())
                                        })
                                        .map_err(PostgresKernelError::ClientExecution)
                                    }
                                    Err(error) => Err(error),
                                }
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
                                    let loader = load_client_reference_loader(
                                        &transaction,
                                        &active,
                                        authorisation.session_principal(),
                                        client_security_context_digest(&authorisation),
                                        &arguments,
                                    )
                                    .await;
                                    let execution = match loader {
                                        Ok(loader) => {
                                            state.install_reference_loader(loader);
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
                                    state
                                        .bind_authenticated_session(authenticated_session.binding())
                                        .map_err(|_| PostgresKernelError::DurableInvariant {
                                            relation: "CLIENT state store",
                                            record: format!("{:?}", definition.id()),
                                            rule: "sealed CLIENT USER state session binding must be retained",
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
                                        executor.bind_current_invocation(invocation);
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
                                    execution
                                        }
                                        Err(error) => Err(error),
                                    };
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
                                                    continue;
                                                }
                                                let Some(resource) = state.resource(key) else {
                                                    return Err(PostgresKernelError::ClientExecution(
                                                        ClientExecutionError::ResourceEvaluation {
                                                            context,
                                                            source: ClientResourceExecutionError::Failed(
                                                                "resource.executor.invalid_state".to_owned(),
                                                            ),
                                                        },
                                                    ));
                                                };
                                                if resource.request_id() != Some(completion.request_id()) {
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
                                decoded.client_offer(),
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
                                decoded.output_requirement(),
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
    /// The request is untrusted metadata: no target or nested invocation identity
    /// is retained before RESOURCE_ACCEPTED.
    pub async fn record_cancelled_resource_audit(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
    ) -> Result<(), PostgresKernelError> {
        validate_resource_lineage(request)?;
        validate_resource_state_context(request)?;
        if !self
            .resource_parent_invocation_is_owned(authenticated_session, request)
            .await?
        {
            return Err(resource_parent_invocation_unavailable(request));
        }
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            establish_trusted_search_path(&transaction).await?;
            require_current_migrations(&transaction).await?;
            append_resource_audit_event(
                &transaction,
                authenticated_session,
                request,
                None,
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

    /// Provisions the deterministic local USER authority for one operating-system UID.
    ///
    /// This path is for the user-owned local server profile. It retains the
    /// catalogue-health identity, but does not grant the local user security
    /// administration or alter local peer authentication.
    pub async fn provision_local_user(
        &self,
        uid: u32,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        if uid == LOCAL_HEALTH_UID {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_local_peer_credentials",
                record: uid.to_string(),
                rule: "the local USER UID is reserved for the catalogue-health migration slot",
            });
        }
        let principal = local_user_principal_id(uid);
        if principal == PrincipalId::from_bytes([0; 16])
            || principal == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
        {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_principals",
                record: principal.canonical(),
                rule: "the deterministic local USER identity is reserved or empty",
            });
        }

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
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            let candidate = local_user_security_snapshot(&current, &active, uid, principal)?;
            require_complete_function_set(&active, &candidate)?;
            replace_security_rows(&transaction, &candidate).await?;
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&candidate, &recovered) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    record: principal.canonical(),
                    rule: "recovered local USER authority does not match the persisted security snapshot",
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
            let candidate = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                active.pair(),
                current.function_targets().collect(),
                current.principals().collect(),
                current.memberships().collect(),
                grants,
                current.local_peer_credentials().collect(),
                current.privilege_grants().collect(),
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

fn local_user_principal_id(uid: u32) -> PrincipalId {
    let mut digest = Sha256::new();
    digest.update(LOCAL_USER_PRINCIPAL_DOMAIN);
    digest.update(uid.to_be_bytes());
    let digest = digest.finalize();
    PrincipalId::from_bytes(
        digest[..16]
            .try_into()
            .expect("SHA-256 always provides sixteen bytes"),
    )
}

fn local_user_security_snapshot(
    current: &SecuritySnapshot,
    active: &ActiveDatabaseRevision,
    uid: u32,
    principal: PrincipalId,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let current_uid_credential = current
        .local_peer_credentials()
        .find(|credential| credential.uid() == uid);
    if let Some(credential) = current_uid_credential
        && credential.principal() != principal
        && credential.principal() != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
    {
        return Err(local_user_invariant(
            "_orna_kernel.security_local_peer_credentials",
            uid.to_string(),
            "the local USER UID already selects another principal",
        ));
    }
    if let Some(credential) = current
        .local_peer_credentials()
        .find(|credential| credential.principal() == principal)
        && credential.uid() != uid
    {
        return Err(local_user_invariant(
            "_orna_kernel.security_local_peer_credentials",
            principal.canonical(),
            "the deterministic local USER identity already selects another UID",
        ));
    }
    if let Some(credential) = current
        .local_peer_credentials()
        .find(|credential| credential.uid() == LOCAL_HEALTH_UID)
        && credential.principal() != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
    {
        return Err(local_user_invariant(
            "_orna_kernel.security_local_peer_credentials",
            LOCAL_HEALTH_UID.to_string(),
            "the reserved local health UID selects another principal",
        ));
    }

    let health_uid = catalogue_health_service_uid(current)?;
    let mut principals = current.principals().collect::<Vec<_>>();
    if health_uid.is_none() {
        principals.push(Principal::new(
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            PrincipalKind::Service,
            PrincipalStatus::Active,
        ));
    }
    if let Some(stored) = current.principals().find(|stored| stored.id() == principal) {
        if stored.kind() != PrincipalKind::User || stored.status() != PrincipalStatus::Active {
            return Err(local_user_invariant(
                "_orna_kernel.security_principals",
                principal.canonical(),
                "the deterministic local USER identity must be active and user-kind",
            ));
        }
    } else {
        principals.push(Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        ));
    }

    let memberships = current
        .memberships()
        .filter(|membership| membership.role() != principal && membership.member() != principal)
        .collect::<Vec<_>>();
    let execute_grants = current
        .execute_grants()
        .filter(|grant| grant.grantee() != principal)
        .collect::<Vec<_>>();
    let mut privilege_grants = current
        .privilege_grants()
        .filter(|grant| grant.grantee() != principal)
        .collect::<Vec<_>>();
    for class in [
        PrivilegeClass::Execute,
        PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
        PrivilegeClass::Inspect(InspectPrivilege::SessionInvocations),
        PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation),
        PrivilegeClass::Inspect(InspectPrivilege::Values),
        PrivilegeClass::Inspect(InspectPrivilege::Source),
        PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails),
        PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals),
    ] {
        privilege_grants.push(
            PrivilegeGrant::new(principal, class, None)
                .expect("local USER privilege grant shape is valid"),
        );
    }

    let mut local_peer_credentials = current
        .local_peer_credentials()
        .filter(|credential| credential.uid() != uid && credential.principal() != principal)
        .collect::<Vec<_>>();
    if health_uid.is_none()
        || current_uid_credential.is_some_and(|credential| {
            credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
        })
    {
        if !local_peer_credentials
            .iter()
            .any(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
        {
            local_peer_credentials.push(LocalPeerCredential::new(
                LOCAL_HEALTH_UID,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            ));
        }
    }
    local_peer_credentials.push(LocalPeerCredential::new(uid, principal));

    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        active.pair(),
        current.function_targets().collect(),
        principals,
        memberships,
        execute_grants,
        local_peer_credentials,
        privilege_grants,
    )
    .map_err(PostgresKernelError::SecuritySnapshot)
}

fn local_user_invariant(
    relation: &'static str,
    record: String,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation,
        record,
        rule,
    }
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
async fn resource_parent_invocation_is_owned_in_transaction(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    parent_invocation_id: InvocationId,
) -> Result<bool, PostgresKernelError> {
    let parent_invocation_id = parent_invocation_id.to_bytes().to_vec();
    let session_principal = authenticated_session.principal().to_bytes().to_vec();
    transaction
        .query_opt(
            "SELECT 1
             FROM _orna_kernel.invocation_audit_events
             WHERE invocation_id = $1
               AND session_principal_id = $2",
            &[&parent_invocation_id, &session_principal],
        )
        .await
        .map(|row| row.is_some())
        .map_err(PostgresKernelError::Database)
}

fn resource_parent_invocation_unavailable(request: &ResourceRequest) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation: "_orna_kernel.invocation_audit_events",
        record: request.request_id.canonical(),
        rule: "resource parent invocation must belong to authenticated session",
    }
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
    nested_invocation_id: Option<InvocationId>,
    decision: SecurityAuditOutcome,
    terminal: ResourceAuditTerminalOutcome,
    target: Option<InvocationTarget>,
    item_count: Option<u64>,
    byte_count: Option<u64>,
) -> Result<(), PostgresKernelError> {
    validate_resource_lineage(request)?;
    if !resource_parent_invocation_is_owned_in_transaction(
        transaction,
        authenticated_session,
        request.parent_invocation_id,
    )
    .await?
    {
        return Err(resource_parent_invocation_unavailable(request));
    }
    validate_resource_audit_nested_invocation(
        "resource request",
        request.request_id.canonical(),
        nested_invocation_id.map(InvocationId::to_bytes),
    )?;
    if nested_invocation_id.is_none()
        && (decision != SecurityAuditOutcome::Denied
            || !matches!(
                terminal,
                ResourceAuditTerminalOutcome::Failed | ResourceAuditTerminalOutcome::Cancelled
            ))
    {
        return Err(resource_audit_invariant(
            &request.request_id.canonical(),
            "resource audit without nested invocation must be a preaccept denied or cancelled terminal",
        ));
    }
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
    let nested_invocation_id_bytes = nested_invocation_id.map(|id| id.to_bytes().to_vec());
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

fn validate_resource_lineage(request: &ResourceRequest) -> Result<(), PostgresKernelError> {
    validate_resource_lineage_identities(
        "resource request",
        request.request_id.canonical(),
        request.request_id.to_bytes(),
        request.parent_invocation_id.to_bytes(),
        request.call_site_id.to_bytes(),
    )
}

fn validate_resource_audit_lineage(
    record: &str,
    request_id: [u8; 16],
    nested_invocation_id: Option<[u8; 16]>,
    parent_invocation_id: [u8; 16],
    call_site_id: [u8; 16],
) -> Result<(), PostgresKernelError> {
    validate_resource_lineage_identities(
        "_orna_kernel.resource_audit_events",
        record.to_owned(),
        request_id,
        parent_invocation_id,
        call_site_id,
    )?;
    validate_resource_audit_nested_invocation(
        "_orna_kernel.resource_audit_events",
        record.to_owned(),
        nested_invocation_id,
    )
}

fn validate_resource_audit_nested_invocation(
    relation: &'static str,
    record: String,
    nested_invocation_id: Option<[u8; 16]>,
) -> Result<(), PostgresKernelError> {
    if nested_invocation_id.is_some_and(|id| id == [0; 16]) {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource nested invocation identity must be non-zero",
        });
    }
    Ok(())
}

fn validate_resource_lineage_identities(
    relation: &'static str,
    record: String,
    request_id: [u8; 16],
    parent_invocation_id: [u8; 16],
    call_site_id: [u8; 16],
) -> Result<(), PostgresKernelError> {
    if request_id == [0; 16] {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource request identity must be non-zero",
        });
    }
    if parent_invocation_id == [0; 16] {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource parent invocation identity must be non-zero",
        });
    }
    if call_site_id == [0; 16] {
        return Err(PostgresKernelError::DurableInvariant {
            relation,
            record,
            rule: "resource call-site identity must be non-zero",
        });
    }
    Ok(())
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
fn sealed_target_security_is_supported(target: SealedResolvedTarget<'_>) -> bool {
    match target {
        SealedResolvedTarget::Application(definition)
        | SealedResolvedTarget::VerifiedStandard { definition, .. } => {
            definition.security() == FunctionSecurity::Invoker
        }
        SealedResolvedTarget::System(_) => true,
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

/// Captures the structural epoch for one completed authenticated resource.
///
/// Resource requests do not carry an independent client offer. The nested
/// invocation therefore records no runtime-binding rows, while the immutable
/// invocation and trace carriers remain available to the Inspector.
async fn capture_completed_resource_inspect_snapshot(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authenticated_session: &AuthenticatedSession,
    invocation: InvocationId,
    root_target: Option<InvocationTarget>,
) -> Result<InspectEpochId, PostgresKernelError> {
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.active_revision",
            record: active.pair().catalogue().canonical(),
            rule: "completed resource capture requires the verified standard snapshot",
        }
    })?;
    let registry =
        registered_opaque_codecs(standard).map_err(|_| PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.standard_library_revisions",
            record: standard.revision().canonical(),
            rule: "completed resource capture requires the verified codec registry",
        })?;
    let root_target = root_target.ok_or_else(|| {
        sealed_target_invariant(active, "completed resource producer must retain its target")
    })?;
    let events = sealed_completed_events_from_values(
        authenticated_session.principal(),
        invocation,
        Vec::new(),
    )?;
    crate::inspect::capture_inspect_snapshot_in_transaction(
        transaction,
        active,
        &registry,
        authenticated_session,
        invocation,
        InspectSnapshotOptions::structural(),
        authenticated_session.principal(),
        root_target.function(),
        InspectOutcomeKind::Allowed,
        &events,
        None,
        None,
        None,
        None,
    )
    .await
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
    output_requirement: Option<&InvocationOutputRequirement>,
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
        Some(client_offer),
        None,
        loaded_user_state_cells,
        output_requirement,
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

#[cfg(test)]
#[path = "security/tests.rs"]
mod tests;
