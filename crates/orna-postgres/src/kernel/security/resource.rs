//! Authenticated SERVER resource producer interface.

use super::resource_stream::{
    bind_authenticated_resource_arguments, finish_direct_resource_failure,
    resource_target_security_is_supported, resource_target_shape_is_supported,
    resource_values_from_server_result, run_authenticated_server_resource_producer_task,
};
use super::*;

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
    pub(super) accepted: AuthenticatedServerResourceAccepted,
    pub(super) commands: tokio::sync::mpsc::Sender<ResourceProducerCommand>,
    pub(super) cancellation: ResourceCancellation,
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
        // Cancellation may close the worker response channel after a pull is
        // queued. Preserve the cancellation outcome; unrelated task exits stay
        // durable invariant failures.
        if self
            .commands
            .send(ResourceProducerCommand::Pull(ResourceProducerPull {
                credit,
                response,
            }))
            .await
            .is_err()
        {
            if self.cancellation.is_requested() {
                return Ok(AuthenticatedServerResourceEvent::Cancelled);
            }
            return Err(PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: self.accepted.request_id.canonical(),
                rule: "producer task terminated before pull response",
            });
        }
        match receiver.await {
            Ok(result) => result,
            Err(_) if self.cancellation.is_requested() => {
                Ok(AuthenticatedServerResourceEvent::Cancelled)
            }
            Err(_) => Err(PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: self.accepted.request_id.canonical(),
                rule: "producer task dropped pull response",
            }),
        }
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

impl PostgresKernel {
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
        let result = finish_authenticated_server_select_session(
            operation,
            database_session.shutdown().await,
        );
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
        validate_resource_lineage(request)?;
        validate_resource_state_context(request)?;
        if !self
            .resource_parent_invocation_is_owned(authenticated_session, request)
            .await?
        {
            return Ok(AuthenticatedServerResourceStart::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            });
        }
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
}
