use super::*;

/// Appends the allowed `EXECUTE` security evidence and the linked allowed
/// invocation decision for one protected sealed decision.
///
/// The protected decision already allowed the invocation. This step re-runs
/// the pure authorisation to obtain the immutable decision evidence, appends
/// it, and links the invocation-audit row to that exact evidence.
pub(super) async fn append_allowed_invocation_audit(
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

pub(super) async fn append_allowed_invocation_audit_evidence(
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
pub(super) async fn append_sealed_denied_audit(
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
pub(super) async fn append_linked_invocation_audit(
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
pub(super) async fn append_unresolved_invocation_audit(
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
