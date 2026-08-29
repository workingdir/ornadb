//! Authenticated and sealed SERVER resource production.

use super::*;
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
    invocation: Option<InvocationId>,
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
    invocation: Option<InvocationId>,
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
    invocation: Option<InvocationId>,
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

pub(super) async fn finish_direct_resource_failure(
    kernel: &PostgresKernel,
    authenticated_session: &AuthenticatedSession,
    request: &ResourceRequest,
    cancellation: &ResourceCancellation,
    lifecycle: &ResourceProducerLifecycle,
    error: PostgresKernelError,
) -> Result<Option<AuthenticatedServerResourceResult>, PostgresKernelError> {
    match finalize_reserved_resource_request(
        kernel,
        authenticated_session,
        request,
        cancellation,
        lifecycle,
    )
    .await
    {
        Ok(()) => Err(error),
        Err(finalizer_error) => Err(finalizer_error),
    }
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
    validate_resource_lineage(request)?;
    let mut database_session = kernel.open().await?;
    let operation = async {
        let transaction = database_session
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await
            .map_err(PostgresKernelError::Database)?;
        establish_trusted_search_path(&transaction).await?;
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
            Some(
                lifecycle
                    .invocation
                    .ok_or_else(|| PostgresKernelError::DurableInvariant {
                        relation: "resource producer",
                        record: request.request_id.canonical(),
                        rule: "accepted resource producer must retain its invocation identity",
                    })?,
            )
        } else {
            None
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

pub(super) async fn run_authenticated_server_resource_producer_task(
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
    let mut invocation = None;
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
        failure = Some(CallFailure::TargetUnavailable);
    } else {
        let entry_target = InvocationTarget::new(SYS_INVOKE_FUNCTION_ID, active.pair());
        match security.authorise_system_function(&authenticated_session, entry_target) {
            ExecuteDecision::Denied(reason) => {
                append_security_audit_event(
                    &transaction,
                    SecurityAuditDecision::execute_denied(
                        &authenticated_session,
                        entry_target,
                        reason,
                    ),
                )
                .await?;
                failure = Some(CallFailure::ExecuteDenied);
            }
            ExecuteDecision::Allowed(_) => {
                match resolve_resource_target(&active, request.target_function_id) {
                    None => {
                        failure = Some(CallFailure::TargetUnavailable);
                    }
                    Some(resolved_target) => {
                        let target = resolved_target.target();
                        completed_target = Some(target);
                        lifecycle.target = Some(target);
                        match (
                            security.authorise_execute(&authenticated_session, target),
                            resource_target_security_is_supported(resolved_target.definition()),
                        ) {
                            (ExecuteDecision::Denied(reason), _) => {
                                append_security_audit_event(
                                    &transaction,
                                    SecurityAuditDecision::execute_denied(
                                        &authenticated_session,
                                        target,
                                        reason,
                                    ),
                                )
                                .await?;
                                failure = Some(CallFailure::ExecuteDenied);
                            }
                            (ExecuteDecision::Allowed(_), false) => {
                                append_security_audit_event(
                                    &transaction,
                                    SecurityAuditDecision::execute_denied(
                                        &authenticated_session,
                                        target,
                                        ExecuteDenial::UnsupportedSecurityDefiner,
                                    ),
                                )
                                .await?;
                                failure = Some(CallFailure::ExecuteDenied);
                            }
                            (ExecuteDecision::Allowed(allowed), true) => {
                                let definition = resolved_target.definition();
                                let arguments = if resource_target_shape_is_supported(
                                    definition,
                                    request.resource_kind,
                                ) {
                                    bind_authenticated_resource_arguments(
                                        active.catalogue_hash_context(),
                                        definition,
                                        &request.arguments,
                                    )
                                } else {
                                    None
                                };
                                match arguments {
                                    None => failure = Some(CallFailure::TargetUnavailable),
                                    Some(arguments) => {
                                        let nested_invocation_id = InvocationId::new();
                                        append_allowed_invocation_audit(
                                            &transaction,
                                            &security,
                                            &authenticated_session,
                                            target,
                                            nested_invocation_id,
                                        )
                                        .await?;
                                        lifecycle.invocation = Some(nested_invocation_id);
                                        invocation = Some(nested_invocation_id);
                                        audit_decision = SecurityAuditOutcome::Allowed;
                                        standard_executable = resolved_target.executable().cloned();
                                        authorisation = Some(allowed);
                                        bound_arguments = Some(arguments);
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
    let invocation = invocation.ok_or_else(|| {
        sealed_target_invariant(
            &active,
            "accepted resource producer must retain its invocation identity",
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
            Some(invocation),
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
            Some(invocation),
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
            Some(invocation),
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
            Some(invocation),
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
                match capture_completed_resource_inspect_snapshot(
                    &transaction,
                    &active,
                    &authenticated_session,
                    invocation,
                    completed_target,
                )
                .await
                {
                    Ok(_) => {
                        commit_resource_audit(
                            transaction,
                            &authenticated_session,
                            &request,
                            Some(invocation),
                            SecurityAuditOutcome::Allowed,
                            ResourceAuditTerminalOutcome::Completed,
                            completed_target,
                            Some(total_items),
                            Some(total_bytes),
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            } else {
                commit_accepted_resource_cancelled_audit(
                    transaction,
                    &authenticated_session,
                    &request,
                    Some(invocation),
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
                Some(invocation),
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
                    Some(invocation),
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
                    Some(invocation),
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
        ResourceProducerExit::SealedFailed(_) => {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: request.request_id.canonical(),
                rule: "sealed mutation failure reached generic resource finalizer",
            });
        }
    }
    match terminal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(super) fn sealed_server_target_is_mutation(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> bool {
    raw_server_insert_target_is_selected(active, function)
        || raw_server_reference_mutation_target(active, function).is_some()
}

pub(super) fn sealed_server_result_kind(
    return_type: &FunctionReturn,
) -> Option<ProtocolResourceKind> {
    match return_type {
        FunctionReturn::Single(_) => Some(ProtocolResourceKind::Single),
        FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => Some(ProtocolResourceKind::Stream),
    }
}

fn sealed_rows_preservation_is_supported(
    active: &ActiveDatabaseRevision,
    return_type: &FunctionReturn,
) -> bool {
    matches!(return_type, FunctionReturn::Rows(_))
        && active
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| {
                let revision = standard.revision();
                revision == STANDARD_LIBRARY_V8_REVISION_ID
                    || revision == STANDARD_LIBRARY_V9_REVISION_ID
                    || revision == STANDARD_LIBRARY_V9_REVISION_ID
            })
}

pub(super) fn resource_target_security_is_supported(definition: &FunctionDefinition) -> bool {
    definition.security() == FunctionSecurity::Invoker
}

pub(super) fn resource_target_shape_is_supported(
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

pub(super) fn bind_authenticated_resource_arguments(
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

pub(super) fn resource_values_from_server_result(
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

pub(super) fn classify_sealed_server_error(
    error: &PostgresKernelError,
) -> SealedInvocationFailureClass {
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

/// Internal result boundary for one sealed SERVER target.
///
/// Direct `ROWS` invocations retain the complete [`ResultRows`] until the
/// caller encodes it as one registered opaque value. Resource and mutation
/// callers continue through the existing flattened value sequence.
enum SealedServerTargetResult {
    Values(Vec<RuntimeValue>),
    Rows(ResultRows),
}
async fn execute_sealed_server_target(
    transaction: &mut Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    kind: ProtocolResourceKind,
    preserve_rows: bool,
) -> Result<SealedServerTargetResult, SealedInvocationFailureClass> {
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
    let result = match mutation {
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
                Ok(value) => Some(SealedServerTargetResult::Values(vec![value])),
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
                Ok(values) => Some(SealedServerTargetResult::Values(values)),
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
                Ok(server) if preserve_rows => {
                    Some(SealedServerTargetResult::Rows(server.into_rows()))
                }
                Ok(server) => resource_values_from_server_result(kind, server)
                    .map(SealedServerTargetResult::Values),
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
    let Some(result) = result else {
        if savepoint.rollback().await.is_err() {
            return Err(SealedInvocationFailureClass::Internal);
        }
        return Err(SealedInvocationFailureClass::Target);
    };
    let result = match result {
        SealedServerTargetResult::Values(values) => {
            if values.len() != 1 && kind != ProtocolResourceKind::Stream {
                if savepoint.rollback().await.is_err() {
                    return Err(SealedInvocationFailureClass::Internal);
                }
                return Err(SealedInvocationFailureClass::Target);
            }
            if values
                .iter()
                .any(|value| !resource_result_value_is_supported(value))
            {
                if savepoint.rollback().await.is_err() {
                    return Err(SealedInvocationFailureClass::Internal);
                }
                return Err(SealedInvocationFailureClass::Target);
            }
            SealedServerTargetResult::Values(values)
        }
        SealedServerTargetResult::Rows(rows) => SealedServerTargetResult::Rows(rows),
    };
    savepoint
        .commit()
        .await
        .map_err(|_| SealedInvocationFailureClass::Internal)?;
    Ok(result)
}

pub(super) fn sealed_server_stream_completed_event(
    final_batch_sequence: u64,
    total_items: u64,
    total_bytes: u64,
) -> AuthenticatedServerResourceEvent {
    AuthenticatedServerResourceEvent::Completed {
        final_batch_sequence,
        total_items,
        total_bytes,
    }
}

/// Executes one accepted mutation target and serves its bounded returned rows
/// through the existing sealed pull protocol.
async fn run_sealed_server_mutation_stream(
    transaction: &mut Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    commands: &mut tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> ResourceProducerExit {
    let failed = |response, failure| {
        ResourceProducerExit::SealedFailed(ResourceProducerSealedFailed {
            response: Some(response),
            failure,
        })
    };
    let mut values = None;
    let mut next_value = 0usize;
    let mut batch_sequence = 0u64;
    let mut total_items = 0u64;
    let mut total_bytes = 0u64;

    loop {
        let command = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None });
            }
            command = commands.recv() => command,
        };
        let Some(ResourceProducerCommand::Pull(ResourceProducerPull { credit, response })) =
            command
        else {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None });
        };
        if cancellation.is_requested() {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            });
        }

        if values.is_none() {
            let mutation_values = match execute_sealed_server_target(
                transaction,
                active,
                authorisation,
                arguments,
                ProtocolResourceKind::Stream,
                false,
            )
            .await
            {
                Ok(SealedServerTargetResult::Values(values)) => values,
                Ok(SealedServerTargetResult::Rows(_)) => {
                    return failed(response, SealedInvocationFailureClass::Internal);
                }
                Err(failure) => return failed(response, failure),
            };
            if mutation_values.len() > 1 {
                return failed(response, SealedInvocationFailureClass::Internal);
            }
            values = Some(mutation_values);
            if cancellation.is_requested() {
                return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: Some(response),
                });
            }
        }

        let mutation_values = values.as_ref().expect("sealed mutation values are loaded");
        let Some(value) = mutation_values.get(next_value).cloned() else {
            return ResourceProducerExit::Completed(ResourceProducerCompleted {
                response,
                final_batch_sequence: batch_sequence.saturating_sub(1),
                total_items,
                total_bytes,
            });
        };
        let byte_count = match encode_active_value(active, &value) {
            Ok(encoded) => match u64::try_from(encoded.len()) {
                Ok(byte_count) => byte_count,
                Err(_) => return failed(response, SealedInvocationFailureClass::Internal),
            },
            Err(_) => return failed(response, SealedInvocationFailureClass::Internal),
        };
        if credit.item_count == 0 || byte_count > credit.byte_count {
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                    required_bytes: byte_count,
                }))
                .is_err()
            {
                return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                });
            }
            continue;
        }
        if cancellation.is_requested() {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            });
        }
        let next_index = match next_value.checked_add(1) {
            Some(next_index) => next_index,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let total_items_next = match total_items.checked_add(1) {
            Some(total_items) => total_items,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let total_bytes_next = match total_bytes.checked_add(byte_count) {
            Some(total_bytes) => total_bytes,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let next_batch_sequence = match batch_sequence.checked_add(1) {
            Some(next_batch_sequence) => next_batch_sequence,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let event = AuthenticatedServerResourceEvent::Values {
            batch_sequence,
            item_count: 1,
            byte_count,
            values: vec![value],
        };
        next_value = next_index;
        total_items = total_items_next;
        total_bytes = total_bytes_next;
        batch_sequence = next_batch_sequence;
        if response.send(Ok(event)).is_err() {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None });
        }
    }
}

async fn run_sealed_server_stream_producer(
    kernel: PostgresKernel,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    authorisation: AuthorisedInvocation,
    arguments: Vec<FunctionArgument>,
    invocation: InvocationId,
    cancellation: ResourceCancellation,
    mut commands: tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    ready: tokio::sync::oneshot::Sender<Result<(), SealedInvocationFailureClass>>,
) {
    let mut database_session = match kernel.open().await {
        Ok(session) => session,
        Err(_) => {
            let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
            return;
        }
    };
    let mut transaction = match database_session
        .client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
    {
        Ok(transaction) => transaction,
        Err(_) => {
            let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
            return;
        }
    };
    let validation = async {
        require_current_migrations(&transaction).await?;
        lock_active_revision(&transaction, active.pair()).await?;
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
        Ok::<_, PostgresKernelError>(())
    }
    .await;
    if validation.is_err() {
        let _ = transaction.rollback().await;
        let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
        let _ = database_session.shutdown().await;
        return;
    }
    if cancellation.is_requested() {
        let _ = transaction.rollback().await;
        let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
        let _ = database_session.shutdown().await;
        return;
    }
    if ready.send(Ok(())).is_err() {
        let _ = transaction.rollback().await;
        let _ = database_session.shutdown().await;
        return;
    }

    let mutation_target =
        sealed_server_target_is_mutation(&active, authorisation.target().function());
    let stream_result = if mutation_target {
        Ok(run_sealed_server_mutation_stream(
            &mut transaction,
            &active,
            &authorisation,
            &arguments,
            &mut commands,
            &cancellation,
        )
        .await)
    } else {
        run_authenticated_server_resource_stream(
            &transaction,
            &active,
            &authorisation,
            &arguments,
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
    match stream_result {
        ResourceProducerExit::Completed(ResourceProducerCompleted {
            response,
            final_batch_sequence,
            total_items,
            total_bytes,
        }) => {
            if !cancellation.try_begin_commit() {
                let _ = transaction.rollback().await;
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
            } else {
                let commit = transaction.commit().await;
                if commit.is_ok() {
                    cancellation.commit_finished();
                    let _ = response.send(Ok(sealed_server_stream_completed_event(
                        final_batch_sequence,
                        total_items,
                        total_bytes,
                    )));
                } else {
                    let _ = response.send(Err(PostgresKernelError::DurableInvariant {
                        relation: "sealed invocation producer",
                        record: invocation.canonical(),
                        rule: "sealed server stream transaction commit failed",
                    }));
                }
            }
        }
        ResourceProducerExit::Cancelled(ResourceProducerCancelled { response }) => {
            let _ = transaction.rollback().await;
            if let Some(response) = response {
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
            }
        }
        ResourceProducerExit::Failed(ResourceProducerFailed { response, error }) => {
            let _ = transaction.rollback().await;
            if let Some(response) = response {
                let failure = if classify_sealed_server_error(&error)
                    == SealedInvocationFailureClass::Target
                {
                    CallFailure::TargetUnavailable
                } else {
                    CallFailure::InternalFailure
                };
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Failed { failure }));
            }
        }
        ResourceProducerExit::SealedFailed(ResourceProducerSealedFailed { response, failure }) => {
            let _ = transaction.rollback().await;
            if let Some(response) = response {
                let failure = match failure {
                    SealedInvocationFailureClass::Target | SealedInvocationFailureClass::Bind => {
                        CallFailure::TargetUnavailable
                    }
                    SealedInvocationFailureClass::Internal => CallFailure::InternalFailure,
                };
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Failed { failure }));
            }
        }
    }
    let _ = database_session.shutdown().await;
}

pub(super) async fn start_sealed_server_stream_producer(
    kernel: PostgresKernel,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    authorisation: AuthorisedInvocation,
    arguments: Vec<FunctionArgument>,
    invocation: InvocationId,
    cancellation: ResourceCancellation,
    runtime_handle: tokio::runtime::Handle,
) -> Result<AuthenticatedServerResourceProducer, SealedInvocationFailureClass> {
    let target_revision = active.pair();
    let (commands, receiver) = tokio::sync::mpsc::channel(1);
    let (ready, ready_receiver) = tokio::sync::oneshot::channel();
    runtime_handle.spawn(run_sealed_server_stream_producer(
        kernel,
        active,
        security,
        authorisation,
        arguments,
        invocation,
        cancellation.clone(),
        receiver,
        ready,
    ));
    match ready_receiver.await {
        Ok(Ok(())) => Ok(AuthenticatedServerResourceProducer {
            accepted: AuthenticatedServerResourceAccepted {
                stream_id: 0,
                request_id: invocation,
                nested_invocation_id: invocation,
                target_revision,
                resource_kind: AuthenticatedServerResourceKind::Stream,
            },
            commands,
            cancellation,
        }),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(SealedInvocationFailureClass::Internal),
    }
}

pub(super) async fn execute_sealed_server_after_audit(
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
    let preserve_rows = sealed_rows_preservation_is_supported(active, definition.return_type());
    let target_result = match execute_sealed_server_target(
        &mut transaction,
        active,
        authorisation,
        &arguments,
        kind,
        preserve_rows,
    )
    .await
    {
        Ok(result) => result,
        Err(failure) => {
            return finish_sealed_failure(transaction, invocation, failure).await;
        }
    };
    let values = match target_result {
        SealedServerTargetResult::Values(values) => values,
        SealedServerTargetResult::Rows(rows) => {
            let value = match encode_rows_value(active, registry, &rows) {
                Ok(value) => value,
                Err(_) => {
                    return finish_sealed_failure(
                        transaction,
                        invocation,
                        SealedInvocationFailureClass::Internal,
                    )
                    .await;
                }
            };
            vec![value]
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
            match present_sealed_standard_output(
                requirement,
                value,
                decoded.client_offer(),
                active,
                registry,
            ) {
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
        decoded.output_requirement(),
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
