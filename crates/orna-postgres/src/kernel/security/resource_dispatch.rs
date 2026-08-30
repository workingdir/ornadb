//! Direct authenticated SERVER resource dispatch.

use super::resource_finalization::finish_direct_resource_failure;
use super::resource_producer::ResourceProducerLifecycle;
use super::sealed_server_contract::{
    bind_authenticated_resource_arguments, resource_target_security_is_supported,
    resource_target_shape_is_supported, resource_values_from_server_result,
};
use super::*;

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

impl PostgresKernel {
    /// Reserves one resource request identity before any target work starts.
    ///
    /// The reservation is committed independently of the execution transaction,
    /// so cancellation or rollback cannot make an identity reusable. The unique
    /// primary key serializes concurrent authenticated connections in PostgreSQL.
    pub(super) async fn reserve_resource_request_id(
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

    /// Checks the authenticated provenance of one resource parent before the
    /// request can reserve an identity or enter target dispatch.
    ///
    /// ADR 0078 carries only the parent invocation identity on the wire. The
    /// protected invocation-audit relation is the existing provenance source:
    /// it binds each kernel-generated invocation identity to the session
    /// principal that authenticated it. A missing row or a row owned by a
    /// different principal is therefore rejected without creating any
    /// resource reservation or audit evidence.
    pub(super) async fn resource_parent_invocation_is_owned(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
    ) -> Result<bool, PostgresKernelError> {
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
            let owned = resource_parent_invocation_is_owned_in_transaction(
                &transaction,
                authenticated_session,
                request.parent_invocation_id,
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(owned)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
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
            false,
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
                false,
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

    /// Injects a deterministic database failure after request reservation.
    ///
    /// This hook is present only in the test-hooks build so the integration
    /// harness can prove reserved direct requests are durably compensated.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn dispatch_authenticated_server_resource_with_forced_post_reservation_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
    ) -> Result<AuthenticatedServerResourceResult, PostgresKernelError> {
        let cancellation = ResourceCancellation::new();
        match self
            .dispatch_authenticated_server_resource_with_cancellation_and_test_barrier(
                authenticated_session,
                request,
                &cancellation,
                None,
                true,
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
        force_post_reservation_failure: bool,
    ) -> Result<Option<AuthenticatedServerResourceResult>, PostgresKernelError> {
        validate_resource_lineage(request)?;
        validate_resource_state_context(request)?;
        if !self
            .resource_parent_invocation_is_owned(authenticated_session, request)
            .await?
        {
            return Ok(Some(AuthenticatedServerResourceResult::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            }));
        }
        if !self.reserve_resource_request_id(request.request_id).await? {
            return Ok(Some(AuthenticatedServerResourceResult::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            }));
        }
        let mut lifecycle = ResourceProducerLifecycle::default();
        let mut database_session = match self.open().await {
            Ok(session) => session,
            Err(error) => {
                return finish_direct_resource_failure(
                    self,
                    authenticated_session,
                    request,
                    cancellation,
                    &lifecycle,
                    error,
                )
                .await;
            }
        };
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
            if force_post_reservation_failure {
                transaction
                    .query_one(
                        "SELECT no_such_direct_resource_post_reservation_column",
                        &[],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
            }
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
            let mut audit_decision = SecurityAuditOutcome::Denied;
            let mut completed_target = None;
            let failed = |failure| AuthenticatedServerResourceResult::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure,
            };
            let completed = |invocation, values| AuthenticatedServerResourceResult::Completed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                nested_invocation_id: invocation,
                target_revision: active.pair(),
                resource_kind: request.resource_kind,
                values,
            };

            let result = if request.target_revision != active.pair() {
                failed(CallFailure::TargetUnavailable)
            } else {
                let entry_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
                match security.authorise_system_function(authenticated_session, entry_target) {
                    ExecuteDecision::Denied(reason) => {
                        append_security_audit_event(
                            &transaction,
                            SecurityAuditDecision::execute_denied(
                                authenticated_session,
                                entry_target,
                                reason,
                            ),
                        )
                        .await?;
                        failed(CallFailure::ExecuteDenied)
                    }
                    ExecuteDecision::Allowed(_) => {
                        match resolve_resource_target(&active, request.target_function_id) {
                            None => {
                                failed(CallFailure::TargetUnavailable)
                            }
                            Some(resolved_target) => {
                                let target = resolved_target.target();
                                completed_target = Some(target);
                                lifecycle.target = Some(target);
                                match (
                                    security.authorise_execute(authenticated_session, target),
                                    resource_target_security_is_supported(resolved_target.definition()),
                                ) {
                            (ExecuteDecision::Denied(reason), _) => {
                                append_security_audit_event(
                                    &transaction,
                                    SecurityAuditDecision::execute_denied(
                                        authenticated_session,
                                        target,
                                        reason,
                                    ),
                                )
                                .await?;
                                failed(CallFailure::ExecuteDenied)
                            }
                            (ExecuteDecision::Allowed(_), false) => {
                                append_security_audit_event(
                                    &transaction,
                                    SecurityAuditDecision::execute_denied(
                                        authenticated_session,
                                        target,
                                        ExecuteDenial::UnsupportedSecurityDefiner,
                                    ),
                                )
                                .await?;
                                failed(CallFailure::ExecuteDenied)
                            }
                            (ExecuteDecision::Allowed(authorisation), true) => {
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
                                            let nested_invocation_id = InvocationId::new();
                                            append_allowed_invocation_audit(
                                                &transaction,
                                                &security,
                                                authenticated_session,
                                                target,
                                                nested_invocation_id,
                                            )
                                            .await?;
                                            lifecycle.invocation = Some(nested_invocation_id);
                                            audit_decision = SecurityAuditOutcome::Allowed;
                                            if let Some(executable) = resolved_target.executable() {
                                                match execute_standard_parameter_echo(
                                                    definition,
                                                    executable.revision(),
                                                    &arguments,
                                                ) {
                                                    Ok(value) => completed(nested_invocation_id, vec![value]),
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
                                                                completed(nested_invocation_id, values)
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
            lifecycle.terminal_commit_started = true;
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
                lifecycle.invocation,
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
        let result =
            finish_authenticated_dispatch_session(operation, database_session.shutdown().await);
        match result {
            Ok(Some(result)) => Ok(Some(result)),
            Ok(None) => match self
                .record_cancelled_resource_audit(authenticated_session, request)
                .await
            {
                Ok(()) => Ok(None),
                Err(error) => {
                    finish_direct_resource_failure(
                        self,
                        authenticated_session,
                        request,
                        cancellation,
                        &lifecycle,
                        error,
                    )
                    .await
                }
            },
            Err(error) => {
                finish_direct_resource_failure(
                    self,
                    authenticated_session,
                    request,
                    cancellation,
                    &lifecycle,
                    error,
                )
                .await
            }
        }
    }
}
