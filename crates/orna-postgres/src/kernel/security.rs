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
#[path = "security/audit_writer.rs"]
mod audit_writer;
#[path = "security/local_identity.rs"]
mod local_identity;
#[path = "security/persistence.rs"]
mod persistence;
#[path = "security/raw_call.rs"]
mod raw_call;
#[path = "security/resource.rs"]
mod resource;
#[path = "security/resource_cancellation.rs"]
mod resource_cancellation;
#[path = "security/resource_stream.rs"]
mod resource_stream;
#[path = "security/revision_guard.rs"]
mod revision_guard;
#[path = "security/sealed_invocation.rs"]
mod sealed_invocation;
#[path = "security/target_resolution.rs"]
mod target_resolution;

pub(crate) use audit::encode_principal_kind;
use audit::*;
pub use audit_writer::ResourceAuditTerminalOutcome;
#[cfg(test)]
use audit_writer::validate_resource_audit_nested_invocation;
pub(crate) use audit_writer::{
    append_invocation_audit_event, append_resource_audit_event, append_security_audit_event,
};
use audit_writer::{
    resource_audit_invariant, resource_parent_invocation_is_owned_in_transaction,
    resource_parent_invocation_unavailable, validate_resource_audit_lineage,
    validate_resource_lineage, validate_resource_state_context,
};
use local_identity::append_client_capability_audit;
pub(crate) use local_identity::security_snapshots_match;
use persistence::*;
pub(crate) use persistence::{
    encode_privilege_class, recover_invocation_audit_events, recover_security_snapshot_for_active,
};
pub use raw_call::{AuthenticatedRawCallResult, RecordArgumentPreflight};
use raw_call::{
    classify_raw_identity_selected_server_error, classify_raw_server_error,
    classify_raw_server_insert_error, classify_raw_server_reference_mutation_error,
    classify_raw_unique_text_selected_server_error, raw_call_target_unavailable,
    validate_raw_call_argument_shape,
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
use revision_guard::lock_active_revision_for_resource;
pub(crate) use revision_guard::{lock_active_revision, require_complete_function_set};
pub(crate) use sealed_invocation::InvocationAuditDecision;
use sealed_invocation::{
    PreparedSealedTarget, SealedInvocationFailureClass, SealedInvocationPreparedOutcome,
};
pub use sealed_invocation::{
    SealedInvocationContinuation, SealedInvocationExecution, SealedInvocationOperation,
    SealedInvocationPreflight, SealedInvocationResult,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::SystemTime,
};
pub(crate) use target_resolution::is_admitted_security_identity;
#[cfg(test)]
use target_resolution::resolve_resource_target_in_catalogues;
use target_resolution::{
    SealedResolvedTarget, authorise_sealed_target, bind_sealed_invoke_arguments,
    resolve_resource_target, resolve_sealed_target, sealed_security_target,
    sealed_target_invariant, sealed_target_security_is_supported,
};

const MAX_RESOURCE_CREDIT: u64 = 1024 * 1024 * 1024;

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

#[cfg(test)]
#[path = "security/tests.rs"]
mod tests;
