//! Durable terminal evidence for authenticated SERVER resources.

use super::resource_producer::ResourceProducerLifecycle;
use super::*;

pub(super) async fn commit_resource_audit(
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

pub(super) async fn commit_accepted_resource_cancelled_audit(
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

pub(super) async fn commit_post_acceptance_resource_error_audit(
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
    finish_authenticated_dispatch_session(operation, session.shutdown().await)
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
pub(super) async fn finalize_reserved_resource_request(
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
    finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
}
