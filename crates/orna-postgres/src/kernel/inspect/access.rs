//! INSPECT session rebinding and authorization gates.

use super::*;

/// Rebinds a trusted session against the current security snapshot.
///
/// Authentication state is durable and can change after a caller obtains a
/// session. Invalid or stale principals and roles fail closed as a normal
/// INSPECT denial before any epoch or classifier grant is considered.
pub(super) async fn rebind_inspect_session(
    kernel: &PostgresKernel,
    security: &SecuritySnapshot,
    authenticated_session: &AuthenticatedSession,
) -> Result<AuthenticatedSession, PostgresKernelError> {
    match security.bind_authenticated_session(
        authenticated_session.principal(),
        authenticated_session.active_roles().to_vec(),
    ) {
        Ok(session) => Ok(session),
        Err(_) => {
            let reason = InspectDenial::MissingPrivilege;
            kernel
                .append_inspect_denial_audit(authenticated_session, None, reason)
                .await?;
            Err(PostgresKernelError::InspectDenied { reason })
        }
    }
}

/// Applies the structural INSPECT ownership/scope gate used while resolving
/// both latest and exact epochs.
pub(super) async fn require_inspect_epoch_access(
    kernel: &PostgresKernel,
    authenticated_session: &AuthenticatedSession,
    owner: PrincipalId,
    granted: &[InspectPrivilege],
) -> Result<(), PostgresKernelError> {
    let mut effective = Vec::with_capacity(granted.len() + 1);
    if !granted.contains(&InspectPrivilege::OwnInvocation) {
        effective.push(InspectPrivilege::OwnInvocation);
    }
    effective.extend_from_slice(granted);
    match authorise_inspect(
        authenticated_session.principal(),
        InspectPrivilege::OwnInvocation,
        Some(owner),
        &effective,
    ) {
        InspectDecision::Allowed { .. } => Ok(()),
        InspectDecision::Denied(reason) => {
            kernel
                .append_inspect_denial_audit(authenticated_session, Some(owner), reason)
                .await?;
            Err(PostgresKernelError::InspectDenied { reason })
        }
    }
}

/// Decides whether one session principal may apply one INSPECT privilege to
/// one epoch, failing closed with the closed denial reason.
pub(super) async fn require_inspect_privilege(
    kernel: &PostgresKernel,
    snapshot: &AuthenticatedInspectSnapshot,
    requested: InspectPrivilege,
) -> Result<(), PostgresKernelError> {
    match authorise_inspect(
        snapshot.session.principal(),
        requested,
        Some(snapshot.owner()),
        &snapshot.granted,
    ) {
        InspectDecision::Allowed { .. } => Ok(()),
        InspectDecision::Denied(reason) => {
            kernel
                .append_inspect_denial_audit(&snapshot.session, Some(snapshot.owner()), reason)
                .await?;
            Err(PostgresKernelError::InspectDenied { reason })
        }
    }
}

pub(super) fn finish_inspect_session<T>(
    operation: Result<T, PostgresKernelError>,
    shutdown: Result<(), PostgresKernelError>,
) -> Result<T, PostgresKernelError> {
    match (operation, shutdown) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}
