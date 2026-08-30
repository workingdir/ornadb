//! Security privilege grant validation and effective-class resolution.

use super::*;

/// Resolves the privilege classes one session holds for one object: its own
/// grants plus every active role's grants, keeping only class-wide grants
/// for a class-wide request and object-scoped grants for an object request.
pub(super) fn privilege_classes_for_session(
    snapshot: &SecuritySnapshot,
    session: &AuthenticatedSession,
    object: Option<FunctionId>,
) -> Vec<PrivilegeClass> {
    let mut principals = vec![session.principal()];
    principals.extend(session.active_roles().iter().copied());
    snapshot
        .privilege_grants()
        .filter(|grant| {
            principals.contains(&grant.grantee())
                && (grant.object().is_none() || grant.object() == object)
        })
        .map(|grant| grant.class())
        .collect()
}

/// Resolves the durable, class-wide INSPECT privileges held by one
/// authenticated session. The session principal and every explicitly active
/// role participate in the resolution; object-scoped grants are deliberately
/// excluded because INSPECT privileges are class-wide.
pub(crate) fn inspect_privileges_for_session(
    snapshot: &SecuritySnapshot,
    session: &AuthenticatedSession,
) -> Vec<orna_core::inspect::InspectPrivilege> {
    privilege_classes_for_session(snapshot, session, None)
        .into_iter()
        .filter_map(|class| match class {
            PrivilegeClass::Inspect(privilege) => Some(privilege),
            PrivilegeClass::Execute | PrivilegeClass::SecurityAdmin => None,
        })
        .collect()
}

/// Resolves the privilege classes one bare principal holds for one object.
pub(super) fn privilege_classes_for_principal(
    snapshot: &SecuritySnapshot,
    principal: PrincipalId,
    object: Option<FunctionId>,
) -> Vec<PrivilegeClass> {
    snapshot
        .privilege_grants()
        .filter(|grant| {
            grant.grantee() == principal && (grant.object().is_none() || grant.object() == object)
        })
        .map(|grant| grant.class())
        .collect()
}

pub(super) fn validate_privilege_grant_input(
    snapshot: &SecuritySnapshot,
    grantee: PrincipalId,
    class: PrivilegeClass,
    object: Option<FunctionId>,
    operation: &'static str,
) -> Result<(), PostgresKernelError> {
    PrivilegeGrant::new(grantee, class, object).map_err(|error| {
        admin_invariant(
            "_orna_kernel.security_privilege_grants",
            operation,
            match error {
                orna_core::security::PrivilegeGrantError::EmptyGrantee => {
                    "the privilege grantee must not be the empty identity"
                }
                orna_core::security::PrivilegeGrantError::EmptyObject => {
                    "the privilege grant object must not be the empty identity"
                }
                orna_core::security::PrivilegeGrantError::SecurityAdminObject => {
                    "the security_admin privilege grant must be class-wide"
                }
            },
        )
    })?;

    if let Some(object) = object
        && !snapshot
            .function_targets()
            .any(|candidate| candidate.function() == object)
        && system_function_by_id(object).is_none()
    {
        return Err(admin_invariant(
            "_orna_kernel.security_privilege_grants",
            operation,
            "the privilege grant object must exist",
        ));
    }
    Ok(())
}
