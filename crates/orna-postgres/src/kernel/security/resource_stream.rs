//! Authenticated SERVER resource production.

use super::resource_finalization::{
    commit_accepted_resource_cancelled_audit, commit_post_acceptance_resource_error_audit,
    commit_resource_audit, finalize_reserved_resource_request,
};
use super::resource_producer::{
    ResourceProducerCancelled, ResourceProducerCommand, ResourceProducerCompleted,
    ResourceProducerExit, ResourceProducerFailed, ResourceProducerFailureStage,
    ResourceProducerLifecycle, ResourceProducerReady, wait_for_resource_producer_pull_or_cancel,
};
use super::sealed_server_contract::{
    bind_authenticated_resource_arguments, resource_target_security_is_supported,
    resource_target_shape_is_supported,
};
use super::*;

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
        finish_authenticated_dispatch_session(operation, shutdown)
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
