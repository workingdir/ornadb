//! Security-admin mutation validation, persistence, and audited execution.

use super::*;

/// One closed `sys.security` admin mutation and its sealed audit identity.
pub(super) enum SecurityAdminMutation {
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
    pub(super) fn operation(&self) -> SecurityAdminAuditOperation {
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

    pub(super) fn target(&self) -> FunctionId {
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
    /// express: roles only through `create_role`, revoke targets must resolve
    /// to existing principals/functions, and the reserved catalogue health
    /// service identity is never administrable.
    pub(super) fn validate(self, snapshot: &SecuritySnapshot) -> Result<Self, PostgresKernelError> {
        match self {
            Self::CreatePrincipal { principal, kind } => {
                if principal == PrincipalId::from_bytes([0; 16]) {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "create_principal",
                        "the principal identity must not be the empty identity",
                    ));
                }
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
                if role == PrincipalId::from_bytes([0; 16]) {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "create_role",
                        "the principal identity must not be the empty identity",
                    ));
                }
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
            Self::RevokeRole { role, member } => {
                let role_principal = snapshot
                    .principals()
                    .find(|candidate| candidate.id() == role)
                    .ok_or_else(|| {
                        admin_invariant(
                            "_orna_kernel.security_principals",
                            "revoke_role",
                            "the role to revoke must exist",
                        )
                    })?;
                if role_principal.kind() != PrincipalKind::Role {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "revoke_role",
                        "the role target must identify a role",
                    ));
                }
                if !snapshot
                    .principals()
                    .any(|candidate| candidate.id() == member)
                {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "revoke_role",
                        "the role member to revoke must exist",
                    ));
                }
                Ok(self)
            }
            Self::GrantRole { member, .. } => {
                if member == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                    return Err(admin_invariant(
                        "_orna_kernel.security_role_memberships",
                        "grant_role",
                        "the reserved catalogue health service identity cannot become a role member",
                    ));
                }
                Ok(self)
            }
            Self::GrantPrivilege {
                grantee,
                class,
                object,
                ..
            } => {
                validate_privilege_grant_input(
                    snapshot,
                    grantee,
                    class,
                    object,
                    "grant_privilege",
                )?;
                if grantee == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID {
                    return Err(admin_invariant(
                        "_orna_kernel.security_privilege_grants",
                        "grant_privilege",
                        "the reserved catalogue health service identity cannot receive privilege grants",
                    ));
                }
                Ok(self)
            }
            Self::RevokePrivilege {
                grantee,
                class,
                object,
                ..
            } => {
                if !snapshot
                    .principals()
                    .any(|candidate| candidate.id() == grantee)
                {
                    return Err(admin_invariant(
                        "_orna_kernel.security_principals",
                        "revoke_privilege",
                        "the privilege grantee must exist",
                    ));
                }
                validate_privilege_grant_input(
                    snapshot,
                    grantee,
                    class,
                    object,
                    "revoke_privilege",
                )?;
                Ok(self)
            }
        }
    }

    /// Persists the durable row change and rebuilds the candidate snapshot
    /// through the validating constructor.
    pub(super) async fn apply(
        self,
        transaction: &Transaction<'_>,
        current: &SecuritySnapshot,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        match self {
            Self::CreatePrincipal { principal, kind } => {
                let mut principals = current.principals().collect::<Vec<_>>();
                principals.push(
                    Principal::try_new(principal, kind, PrincipalStatus::Active)
                        .map_err(PostgresKernelError::SecuritySnapshot)?,
                );
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
                principals.push(
                    Principal::try_new(role, PrincipalKind::Role, PrincipalStatus::Active)
                        .map_err(PostgresKernelError::SecuritySnapshot)?,
                );
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
                            orna_core::security::PrivilegeGrantError::SecurityAdminObject => {
                                "the security_admin privilege grant must be class-wide"
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
                            orna_core::security::PrivilegeGrantError::SecurityAdminObject => {
                                "the security_admin privilege grant must be class-wide"
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
pub(super) async fn run_security_admin_mutation(
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
        let bound_session = match current
            .bind_authenticated_session(session.principal(), session.active_roles().to_vec())
        {
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
pub(super) fn rebuild_candidate(
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

pub(super) fn admin_invariant(
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
