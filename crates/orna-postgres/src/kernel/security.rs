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
#[path = "security/authenticated_select.rs"]
mod authenticated_select;
#[path = "security/authentication.rs"]
mod authentication;
#[path = "security/client_evaluation.rs"]
mod client_evaluation;
#[path = "security/inspect_capture.rs"]
mod inspect_capture;
#[path = "security/local_identity.rs"]
mod local_identity;
#[path = "security/persistence.rs"]
mod persistence;
#[path = "security/raw_call.rs"]
mod raw_call;
#[path = "security/recovery.rs"]
mod recovery;
#[path = "security/resource.rs"]
mod resource;
#[path = "security/resource_cancellation.rs"]
mod resource_cancellation;
#[path = "security/resource_producer.rs"]
mod resource_producer;
#[path = "security/resource_stream.rs"]
mod resource_stream;
#[path = "security/revision_guard.rs"]
mod revision_guard;
#[path = "security/sealed_audit.rs"]
mod sealed_audit;
#[path = "security/sealed_events.rs"]
mod sealed_events;
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
    validate_resource_audit_lineage, validate_resource_lineage, validate_resource_state_context,
};
use inspect_capture::{
    capture_completed_resource_inspect_snapshot, capture_sealed_invocation_snapshot,
};
use local_identity::append_client_capability_audit;
pub(crate) use local_identity::security_snapshots_match;
use persistence::*;
pub(crate) use persistence::{
    encode_privilege_class, recover_invocation_audit_events, recover_security_snapshot_for_active,
};
pub use raw_call::{AuthenticatedRawCallResult, RecordArgumentPreflight};
pub use resource::{
    AuthenticatedServerResourceAccepted, AuthenticatedServerResourceEvent,
    AuthenticatedServerResourceKind, AuthenticatedServerResourceProducer,
    AuthenticatedServerResourceResult, AuthenticatedServerResourceStart, ResourceCredit,
};

pub use resource_cancellation::ResourceCancellation;
pub(crate) use resource_producer::{
    ResourceProducerCancelled, ResourceProducerCommand, ResourceProducerCompleted,
    ResourceProducerExit, ResourceProducerFailed, ResourceProducerPull,
};
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
use sealed_audit::{
    append_allowed_invocation_audit, append_allowed_invocation_audit_evidence,
    append_linked_invocation_audit, append_sealed_denied_audit, append_unresolved_invocation_audit,
};
pub(crate) use sealed_events::sealed_completed_events;
use sealed_events::{
    finish_sealed_failure, sealed_completed_events_from_values, sealed_failure_events,
    sealed_failure_result,
};
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

impl PostgresKernel {
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
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }
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

fn finish_authenticated_dispatch_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    shutdown?;
    operation
}

#[cfg(test)]
#[path = "security/tests.rs"]
mod tests;
