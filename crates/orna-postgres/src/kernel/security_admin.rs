// Security administration keeps the accepted error layout across its public seam.
#![allow(clippy::result_large_err)]

//! ADR 0065: the protected `sys.security` kernel model methods.
//!
//! The session identity functions return typed facts from the bound
//! [`AuthenticatedSession`]; `effective_principal` equals the session
//! principal until a definer or policy mode exists (the equality is
//! documented, not invented). The checks decide one `EXECUTE` grant or one
//! privilege class against the recovered active security snapshot.
//!
//! Every mutation runs in one serializable transaction that mirrors
//! `grant_catalogue_health_service_execute`: it recovers the active revision
//! and security snapshot, requires the calling session to hold the
//! `SecurityAdmin` privilege class (the enforcement gate), applies the
//! durable row change, rebuilds the candidate snapshot through the
//! validating constructor, verifies the complete function set and the
//! recovered snapshot, appends the closed `SecurityAdmin` audit decision,
//! and commits.
//!
//! Enforcement-gate note: the kernel is authoritative here. The CLI host
//! (next wave) may additionally check the local service identity exactly
//! like the fixed-service grant path, and sealed `sys.invoke` routing of
//! these identities stays deferred; this module already fails closed for
//! any session that does not hold the privilege.

use orna_core::{
    FunctionId, PrincipalId,
    security::{
        AuthenticatedSession, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision,
        ExecuteDenial, InvocationTarget, Principal, PrincipalKind, PrincipalStatus, PrivilegeClass,
        PrivilegeDecision, PrivilegeDenial, PrivilegeGrant, RoleMembership,
        SecurityAdminAuditOperation, SecurityAuditDecision, SecuritySnapshot, authorise_privilege,
    },
    system::{
        SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
        SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_GRANT_ROLE_FUNCTION_ID, SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
        SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID, system_function_by_id,
    },
};
use tokio_postgres::{IsolationLevel, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    security::{
        append_security_audit_event, encode_principal_kind, encode_privilege_class,
        finish_security_session, lock_active_revision, recover_security_snapshot_for_active,
        require_complete_function_set, security_snapshots_match,
    },
    server_runtime::configure_and_recover,
};
#[path = "security_admin/mutation.rs"]
mod mutation;
#[path = "security_admin/privileges.rs"]
mod privileges;

use mutation::*;
pub(crate) use privileges::inspect_privileges_for_session;
use privileges::{
    privilege_classes_for_principal, privilege_classes_for_session, validate_privilege_grant_input,
};

impl PostgresKernel {
    /// Returns the authenticated session principal as a typed identity fact.
    pub fn session_principal(&self, session: &AuthenticatedSession) -> PrincipalId {
        session.principal()
    }

    /// Returns the effective principal of the authenticated session.
    ///
    /// Effective identity equals the session principal until a definer or
    /// policy mode exists.
    pub fn effective_principal(&self, session: &AuthenticatedSession) -> PrincipalId {
        session.principal()
    }

    /// Returns the sorted active roles bound to the authenticated session.
    pub fn active_roles(&self, session: &AuthenticatedSession) -> Vec<PrincipalId> {
        session.active_roles().to_vec()
    }

    /// Decides whether one principal may execute one application function
    /// under the active security snapshot.
    ///
    /// The target is class-less, so the decision is an `Application` target
    /// decision; a pinned verified-standard target must be decided through
    /// the sealed dispatch surface.
    pub async fn can_execute(
        &self,
        principal: PrincipalId,
        function: FunctionId,
    ) -> Result<ExecuteDecision, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation =
            async {
                let transaction = database_session
                    .client
                    .build_transaction()
                    .isolation_level(IsolationLevel::RepeatableRead)
                    .read_only(true)
                    .start()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                require_current_migrations(&transaction).await?;
                let active = configure_and_recover(&transaction).await?;
                let security = recover_security_snapshot_for_active(&transaction, &active).await?;
                transaction
                    .commit()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                let session = match security.bind_authenticated_session(principal, vec![]) {
                    Ok(session) => session,
                    Err(_) => return Ok(ExecuteDecision::Denied(ExecuteDenial::InvalidSession)),
                };
                Ok(security
                    .authorise_execute(&session, InvocationTarget::new(function, active.pair())))
            }
            .await;
        finish_security_session(operation, database_session.shutdown().await)
    }

    /// Decides whether one principal holds one privilege class for one object
    /// under the active security snapshot.
    ///
    /// Only the principal's own durable grants are considered; the active
    /// roles of a session are resolved by the caller through the session
    /// gate.
    pub async fn has_privilege(
        &self,
        principal: PrincipalId,
        class: PrivilegeClass,
        object: Option<FunctionId>,
    ) -> Result<PrivilegeDecision, PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            if !security.principals().any(|candidate| {
                candidate.id() == principal && candidate.status() == PrincipalStatus::Active
            }) {
                return Ok(authorise_privilege(principal, class, object, &[]));
            }
            if matches!(class, PrivilegeClass::Inspect(_)) && object.is_some() {
                return Ok(authorise_privilege(principal, class, object, &[]));
            }
            if matches!(class, PrivilegeClass::SecurityAdmin) && object.is_some() {
                return Ok(authorise_privilege(principal, class, object, &[]));
            }
            Ok(authorise_privilege(
                principal,
                class,
                object,
                &privilege_classes_for_principal(&security, principal, object),
            ))
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)
    }

    /// Lists one principal's direct `EXECUTE` and privilege-class grants.
    ///
    /// The active security snapshot is recovered in a read-only transaction,
    /// and the authenticated session must hold the class-wide
    /// `SecurityAdmin` privilege before any grant data is returned. The two
    /// returned vectors retain the snapshot's canonical grant ordering.
    pub async fn list_grants(
        &self,
        session: &AuthenticatedSession,
        grantee: PrincipalId,
    ) -> Result<(Vec<orna_core::security::ExecuteGrant>, Vec<PrivilegeGrant>), PostgresKernelError>
    {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            let security = recover_security_snapshot_for_active(&transaction, &active).await?;
            let bound_session = match security
                .bind_authenticated_session(session.principal(), session.active_roles().to_vec())
            {
                Ok(bound_session) => bound_session,
                Err(_) => {
                    return Err(PostgresKernelError::SecurityAdminDenied {
                        reason: PrivilegeDenial::MissingPrivilege {
                            requested: PrivilegeClass::SecurityAdmin,
                        },
                    });
                }
            };
            let gate = authorise_privilege(
                bound_session.principal(),
                PrivilegeClass::SecurityAdmin,
                None,
                &privilege_classes_for_session(&security, &bound_session, None),
            );
            if let PrivilegeDecision::Denied(reason) = gate {
                return Err(PostgresKernelError::SecurityAdminDenied { reason });
            }

            let execute_grants = security
                .execute_grants()
                .filter(|grant| grant.grantee() == grantee)
                .collect();
            let privilege_grants = security
                .privilege_grants()
                .filter(|grant| grant.grantee() == grantee)
                .collect();
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok((execute_grants, privilege_grants))
        }
        .await;
        finish_security_session(operation, database_session.shutdown().await)
    }

    /// Creates one user or service principal (ADR 0065 `create_principal`).
    pub async fn create_principal(
        &self,
        session: &AuthenticatedSession,
        principal: PrincipalId,
        kind: PrincipalKind,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(
            self,
            session,
            SecurityAdminMutation::CreatePrincipal { principal, kind },
        )
        .await
    }

    /// Disables one existing principal (ADR 0065 `disable_principal`).
    pub async fn disable_principal(
        &self,
        session: &AuthenticatedSession,
        principal: PrincipalId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(
            self,
            session,
            SecurityAdminMutation::DisablePrincipal { principal },
        )
        .await
    }

    /// Creates one role principal (ADR 0065 `create_role`).
    pub async fn create_role(
        &self,
        session: &AuthenticatedSession,
        role: PrincipalId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(self, session, SecurityAdminMutation::CreateRole { role }).await
    }

    /// Grants one role membership (ADR 0065 `grant_role`).
    pub async fn grant_role(
        &self,
        session: &AuthenticatedSession,
        role: PrincipalId,
        member: PrincipalId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(
            self,
            session,
            SecurityAdminMutation::GrantRole { role, member },
        )
        .await
    }

    /// Revokes one role membership (ADR 0065 `revoke_role`).
    pub async fn revoke_role(
        &self,
        session: &AuthenticatedSession,
        role: PrincipalId,
        member: PrincipalId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(
            self,
            session,
            SecurityAdminMutation::RevokeRole { role, member },
        )
        .await
    }

    /// Grants one privilege class to one grantee (ADR 0065 `grant_privilege`).
    pub async fn grant_privilege(
        &self,
        session: &AuthenticatedSession,
        grantee: PrincipalId,
        class: PrivilegeClass,
        object: Option<FunctionId>,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(
            self,
            session,
            SecurityAdminMutation::GrantPrivilege {
                grantee,
                class,
                object,
            },
        )
        .await
    }

    /// Revokes one privilege class from one grantee (ADR 0065 `revoke_privilege`).
    pub async fn revoke_privilege(
        &self,
        session: &AuthenticatedSession,
        grantee: PrincipalId,
        class: PrivilegeClass,
        object: Option<FunctionId>,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        run_security_admin_mutation(
            self,
            session,
            SecurityAdminMutation::RevokePrivilege {
                grantee,
                class,
                object,
            },
        )
        .await
    }
}

#[cfg(test)]
#[path = "security_admin/tests.rs"]
mod tests;
