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
        SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
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
            let bound_session = match security.bind_authenticated_session(
                session.principal(),
                session.active_roles().to_vec(),
            ) {
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

/// One closed `sys.security` admin mutation and its sealed audit identity.
enum SecurityAdminMutation {
    CreatePrincipal {
        principal: PrincipalId,
        kind: PrincipalKind,
    },
    DisablePrincipal {
        principal: PrincipalId,
    },
    CreateRole {
        role: PrincipalId,
    },
    GrantRole {
        role: PrincipalId,
        member: PrincipalId,
    },
    RevokeRole {
        role: PrincipalId,
        member: PrincipalId,
    },
    GrantPrivilege {
        grantee: PrincipalId,
        class: PrivilegeClass,
        object: Option<FunctionId>,
    },
    RevokePrivilege {
        grantee: PrincipalId,
        class: PrivilegeClass,
        object: Option<FunctionId>,
    },
}

impl SecurityAdminMutation {
    fn operation(&self) -> SecurityAdminAuditOperation {
        match self {
            Self::CreatePrincipal { .. } => SecurityAdminAuditOperation::CreatePrincipal,
            Self::DisablePrincipal { .. } => SecurityAdminAuditOperation::DisablePrincipal,
            Self::CreateRole { .. } => SecurityAdminAuditOperation::CreateRole,
            Self::GrantRole { .. } => SecurityAdminAuditOperation::GrantRole,
            Self::RevokeRole { .. } => SecurityAdminAuditOperation::RevokeRole,
            Self::GrantPrivilege { .. } => SecurityAdminAuditOperation::GrantPrivilege,
            Self::RevokePrivilege { .. } => SecurityAdminAuditOperation::RevokePrivilege,
        }
    }

    fn target(&self) -> FunctionId {
        match self {
            Self::CreatePrincipal { .. } => SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
            Self::DisablePrincipal { .. } => SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
            Self::CreateRole { .. } => SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
            Self::GrantRole { .. } => SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
            Self::RevokeRole { .. } => SYS_SECURITY_REVOKE_ROLE_FUNCTION_ID,
            Self::GrantPrivilege { .. } => SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
            Self::RevokePrivilege { .. } => SYS_SECURITY_REVOKE_PRIVILEGE_FUNCTION_ID,
        }
    }

    /// Applies the closed input-level checks the snapshot constructor cannot
    /// express: roles only through `create_role`, and the reserved catalogue
    /// health service identity is never administrable.
    fn validate(self, snapshot: &SecuritySnapshot) -> Result<Self, PostgresKernelError> {
        match self {
            Self::CreatePrincipal { principal, kind } => {
                if kind == PrincipalKind::Role {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "create_principal",
                        "roles must be created through create_role",
                    ));
                }
                if principal == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "create_principal",
                        "the reserved catalogue health service identity cannot be created through the admin surface",
                    ));
                }
                Ok(self)
            }
            Self::CreateRole { role } => {
                if role == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "create_role",
                        "the reserved catalogue health service identity cannot be created through the admin surface",
                    ));
                }
                Ok(self)
            }
            Self::DisablePrincipal { principal } => {
                if principal == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "disable_principal",
                        "the reserved catalogue health service identity cannot be disabled",
                    ));
                }
                if !snapshot
                    .principals()
                    .any(|candidate| candidate.id() == principal)
                {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "disable_principal",
                        "the principal to disable must exist",
                    ));
                }
                Ok(self)
            }
            _ => Ok(self),
        }
    }

    /// Persists the durable row change and rebuilds the candidate snapshot
    /// through the validating constructor.
    async fn apply(
        self,
        transaction: &Transaction<'_>,
        current: &SecuritySnapshot,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        match self {
            Self::CreatePrincipal { principal, kind } => {
                let mut principals = current.principals().collect::<Vec<_>>();
                principals.push(Principal::new(principal, kind, PrincipalStatus::Active));
                let candidate = rebuild_candidate(
                    current,
                    principals,
                    current.memberships().collect(),
                    current.privilege_grants().collect(),
                )?;
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                         VALUES ($1, $2, 'active')",
                        &[&principal.to_bytes().to_vec(), &encode_principal_kind(kind)],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
            Self::DisablePrincipal { principal } => {
                let principals = current
                    .principals()
                    .map(|candidate| {
                        if candidate.id() == principal {
                            Principal::new(
                                candidate.id(),
                                candidate.kind(),
                                PrincipalStatus::Disabled,
                            )
                        } else {
                            candidate
                        }
                    })
                    .collect::<Vec<_>>();
                let candidate = rebuild_candidate(
                    current,
                    principals,
                    current.memberships().collect(),
                    current.privilege_grants().collect(),
                )?;
                transaction
                    .execute(
                        "UPDATE _orna_kernel.security_principals
                         SET status = 'disabled'
                         WHERE id = $1",
                        &[&principal.to_bytes().to_vec()],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
            Self::CreateRole { role } => {
                let mut principals = current.principals().collect::<Vec<_>>();
                principals.push(Principal::new(
                    role,
                    PrincipalKind::Role,
                    PrincipalStatus::Active,
                ));
                let candidate = rebuild_candidate(
                    current,
                    principals,
                    current.memberships().collect(),
                    current.privilege_grants().collect(),
                )?;
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.security_principals (id, kind, status)
                         VALUES ($1, 'role', 'active')",
                        &[&role.to_bytes().to_vec()],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
            Self::GrantRole { role, member } => {
                let membership = RoleMembership::new(role, member);
                let mut memberships = current.memberships().collect::<Vec<_>>();
                if !memberships.contains(&membership) {
                    memberships.push(membership);
                }
                let candidate = rebuild_candidate(
                    current,
                    current.principals().collect(),
                    memberships,
                    current.privilege_grants().collect(),
                )?;
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.security_role_memberships (role_id, member_id)
                         VALUES ($1, $2)
                         ON CONFLICT (role_id, member_id) DO NOTHING",
                        &[&role.to_bytes().to_vec(), &member.to_bytes().to_vec()],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
            Self::RevokeRole { role, member } => {
                let membership = RoleMembership::new(role, member);
                let memberships = current
                    .memberships()
                    .filter(|candidate| *candidate != membership)
                    .collect::<Vec<_>>();
                let candidate = rebuild_candidate(
                    current,
                    current.principals().collect(),
                    memberships,
                    current.privilege_grants().collect(),
                )?;
                transaction
                    .execute(
                        "DELETE FROM _orna_kernel.security_role_memberships
                         WHERE role_id = $1 AND member_id = $2",
                        &[&role.to_bytes().to_vec(), &member.to_bytes().to_vec()],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
            Self::GrantPrivilege {
                grantee,
                class,
                object,
            } => {
                let grant = PrivilegeGrant::new(grantee, class, object).map_err(|error| {
                    admin_invariant(
                        "_orna_kernel.security_privilege_grants",
                        "grant_privilege",
                        match error {
                            orna_core::security::PrivilegeGrantError::EmptyGrantee => {
                                "the privilege grantee must not be the empty identity"
                            }
                            orna_core::security::PrivilegeGrantError::EmptyObject => {
                                "the privilege grant object must not be the empty identity"
                            }
                        },
                    )
                })?;
                let mut grants = current.privilege_grants().collect::<Vec<_>>();
                if !grants.contains(&grant) {
                    grants.push(grant);
                }
                let candidate = rebuild_candidate(
                    current,
                    current.principals().collect(),
                    current.memberships().collect(),
                    grants,
                )?;
                let object_id = grant
                    .object()
                    .map(|function| function.to_bytes().to_vec())
                    .unwrap_or_default();
                transaction
                    .execute(
                        "INSERT INTO _orna_kernel.security_privilege_grants
                             (grantee_id, privilege_class, object_id)
                         VALUES ($1, $2, $3)
                         ON CONFLICT (grantee_id, privilege_class, object_id) DO NOTHING",
                        &[
                            &grantee.to_bytes().to_vec(),
                            &encode_privilege_class(class),
                            &object_id,
                        ],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
            Self::RevokePrivilege {
                grantee,
                class,
                object,
            } => {
                let grant = PrivilegeGrant::new(grantee, class, object).map_err(|error| {
                    admin_invariant(
                        "_orna_kernel.security_privilege_grants",
                        "revoke_privilege",
                        match error {
                            orna_core::security::PrivilegeGrantError::EmptyGrantee => {
                                "the privilege grantee must not be the empty identity"
                            }
                            orna_core::security::PrivilegeGrantError::EmptyObject => {
                                "the privilege grant object must not be the empty identity"
                            }
                        },
                    )
                })?;
                let grants = current
                    .privilege_grants()
                    .filter(|candidate| *candidate != grant)
                    .collect::<Vec<_>>();
                let candidate = rebuild_candidate(
                    current,
                    current.principals().collect(),
                    current.memberships().collect(),
                    grants,
                )?;
                let object_id = grant
                    .object()
                    .map(|function| function.to_bytes().to_vec())
                    .unwrap_or_default();
                transaction
                    .execute(
                        "DELETE FROM _orna_kernel.security_privilege_grants
                         WHERE grantee_id = $1 AND privilege_class = $2 AND object_id = $3",
                        &[
                            &grantee.to_bytes().to_vec(),
                            &encode_privilege_class(class),
                            &object_id,
                        ],
                    )
                    .await
                    .map_err(PostgresKernelError::Database)?;
                Ok(candidate)
            }
        }
    }
}

/// Runs one protected security-admin mutation in one serializable transaction.
///
/// The recovered snapshot is the decision authority for the enforcement
/// gate: the calling session must hold the `SecurityAdmin` privilege class,
/// directly or through an active role. A denied session still records the
/// closed denied audit decision in a committed transaction before the typed
/// denial is returned. An allowed mutation appends its audit in the same
/// transaction as the durable row change.
async fn run_security_admin_mutation(
    kernel: &PostgresKernel,
    session: &AuthenticatedSession,
    mutation: SecurityAdminMutation,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let mut database_session = kernel.open().await?;
    let operation = async {
        let transaction = database_session
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::Serializable)
            .start()
            .await
            .map_err(PostgresKernelError::Database)?;
        require_current_migrations(&transaction).await?;
        let active = configure_and_recover(&transaction).await?;
        lock_active_revision(&transaction, active.pair()).await?;
        let current = recover_security_snapshot_for_active(&transaction, &active).await?;
        let bound_session = match current.bind_authenticated_session(
            session.principal(),
            session.active_roles().to_vec(),
        ) {
            Ok(bound_session) => bound_session,
            Err(_) => {
                let reason = PrivilegeDenial::MissingPrivilege {
                    requested: PrivilegeClass::SecurityAdmin,
                };
                append_security_audit_event(
                    &transaction,
                    SecurityAuditDecision::security_admin_denied(
                        session,
                        mutation.operation(),
                        mutation.target(),
                        reason,
                    ),
                )
                .await?;
                transaction
                    .commit()
                    .await
                    .map_err(PostgresKernelError::Database)?;
                return Err(PostgresKernelError::SecurityAdminDenied { reason });
            }
        };

        let gate = authorise_privilege(
            bound_session.principal(),
            PrivilegeClass::SecurityAdmin,
            None,
            &privilege_classes_for_session(&current, &bound_session, None),
        );
        let PrivilegeDecision::Allowed { .. } = gate else {
            let reason = match gate {
                PrivilegeDecision::Denied(reason) => reason,
                PrivilegeDecision::Allowed { .. } => unreachable!("gate is denied above"),
            };
            append_security_audit_event(
                &transaction,
                SecurityAuditDecision::security_admin_denied(
                    &bound_session,
                    mutation.operation(),
                    mutation.target(),
                    reason,
                ),
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            return Err(PostgresKernelError::SecurityAdminDenied { reason });
        };

        let operation_kind = mutation.operation();
        let target = mutation.target();
        let mutation = mutation.validate(&current)?;
        let candidate = mutation.apply(&transaction, &current).await?;
        require_complete_function_set(&active, &candidate)?;
        let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
        if !security_snapshots_match(&candidate, &recovered) {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel security snapshot",
                record: target.canonical(),
                rule: "recovered security snapshot does not match the persisted candidate",
            });
        }
        append_security_audit_event(
            &transaction,
            SecurityAuditDecision::security_admin_allowed(
                &bound_session,
                PrivilegeDecision::Allowed {
                    requested: PrivilegeClass::SecurityAdmin,
                },
                operation_kind,
                target,
            )
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record: target.canonical(),
                rule: "security-admin audit decision must be an allowed SecurityAdmin decision",
            })?,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(PostgresKernelError::Database)?;
        Ok(recovered)
    }
    .await;
    finish_security_session(operation, database_session.shutdown().await)
}

/// Rebuilds one candidate snapshot through the validating constructor.
fn rebuild_candidate(
    current: &SecuritySnapshot,
    principals: Vec<Principal>,
    memberships: Vec<RoleMembership>,
    privilege_grants: Vec<PrivilegeGrant>,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        current.revision(),
        current.function_targets().collect(),
        principals,
        memberships,
        current.execute_grants().collect(),
        current.local_peer_credentials().collect(),
        privilege_grants,
    )
    .map_err(PostgresKernelError::SecuritySnapshot)
}

/// Resolves the privilege classes one session holds for one object: its own
/// grants plus every active role's grants, keeping only class-wide grants
/// for a class-wide request and object-scoped grants for an object request.
fn privilege_classes_for_session(
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
fn privilege_classes_for_principal(
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

fn admin_invariant(
    relation: &'static str,
    operation: &str,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation,
        record: operation.to_owned(),
        rule,
    }
}
