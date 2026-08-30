use super::*;
const LOCAL_HEALTH_UID: u32 = u32::MAX;

const LOCAL_USER_PRINCIPAL_DOMAIN: &[u8] = b"ornadb.local-user.v1\0";
pub(super) fn local_user_principal_id(uid: u32) -> PrincipalId {
    let mut digest = Sha256::new();
    digest.update(LOCAL_USER_PRINCIPAL_DOMAIN);
    digest.update(uid.to_be_bytes());
    let digest = digest.finalize();
    PrincipalId::from_bytes(
        digest[..16]
            .try_into()
            .expect("SHA-256 always provides sixteen bytes"),
    )
}

pub(super) fn local_user_security_snapshot(
    current: &SecuritySnapshot,
    active: &ActiveDatabaseRevision,
    uid: u32,
    principal: PrincipalId,
) -> Result<SecuritySnapshot, PostgresKernelError> {
    let current_uid_credential = current
        .local_peer_credentials()
        .find(|credential| credential.uid() == uid);
    if let Some(credential) = current_uid_credential
        && credential.principal() != principal
        && credential.principal() != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
    {
        return Err(local_user_invariant(
            "_orna_kernel.security_local_peer_credentials",
            uid.to_string(),
            "the local USER UID already selects another principal",
        ));
    }
    if let Some(credential) = current
        .local_peer_credentials()
        .find(|credential| credential.principal() == principal)
        && credential.uid() != uid
    {
        return Err(local_user_invariant(
            "_orna_kernel.security_local_peer_credentials",
            principal.canonical(),
            "the deterministic local USER identity already selects another UID",
        ));
    }
    if let Some(credential) = current
        .local_peer_credentials()
        .find(|credential| credential.uid() == LOCAL_HEALTH_UID)
        && credential.principal() != CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
    {
        return Err(local_user_invariant(
            "_orna_kernel.security_local_peer_credentials",
            LOCAL_HEALTH_UID.to_string(),
            "the reserved local health UID selects another principal",
        ));
    }

    let health_uid = catalogue_health_service_uid(current)?;
    let mut principals = current.principals().collect::<Vec<_>>();
    if health_uid.is_none() {
        principals.push(Principal::new(
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            PrincipalKind::Service,
            PrincipalStatus::Active,
        ));
    }
    if let Some(stored) = current.principals().find(|stored| stored.id() == principal) {
        if stored.kind() != PrincipalKind::User || stored.status() != PrincipalStatus::Active {
            return Err(local_user_invariant(
                "_orna_kernel.security_principals",
                principal.canonical(),
                "the deterministic local USER identity must be active and user-kind",
            ));
        }
    } else {
        principals.push(Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        ));
    }

    let memberships = current
        .memberships()
        .filter(|membership| membership.role() != principal && membership.member() != principal)
        .collect::<Vec<_>>();
    let execute_grants = current
        .execute_grants()
        .filter(|grant| grant.grantee() != principal)
        .collect::<Vec<_>>();
    let mut privilege_grants = current
        .privilege_grants()
        .filter(|grant| grant.grantee() != principal)
        .collect::<Vec<_>>();
    for class in [
        PrivilegeClass::Execute,
        PrivilegeClass::Inspect(InspectPrivilege::OwnInvocation),
        PrivilegeClass::Inspect(InspectPrivilege::SessionInvocations),
        PrivilegeClass::Inspect(InspectPrivilege::AnyInvocation),
        PrivilegeClass::Inspect(InspectPrivilege::Values),
        PrivilegeClass::Inspect(InspectPrivilege::Source),
        PrivilegeClass::Inspect(InspectPrivilege::SecurityDetails),
        PrivilegeClass::Inspect(InspectPrivilege::RuntimeInternals),
    ] {
        privilege_grants.push(
            PrivilegeGrant::new(principal, class, None)
                .expect("local USER privilege grant shape is valid"),
        );
    }

    let mut local_peer_credentials = current
        .local_peer_credentials()
        .filter(|credential| credential.uid() != uid && credential.principal() != principal)
        .collect::<Vec<_>>();
    if health_uid.is_none()
        || current_uid_credential.is_some_and(|credential| {
            credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
        })
    {
        if !local_peer_credentials
            .iter()
            .any(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
        {
            local_peer_credentials.push(LocalPeerCredential::new(
                LOCAL_HEALTH_UID,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            ));
        }
    }
    local_peer_credentials.push(LocalPeerCredential::new(uid, principal));

    SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        active.pair(),
        current.function_targets().collect(),
        principals,
        memberships,
        execute_grants,
        local_peer_credentials,
        privilege_grants,
    )
    .map_err(PostgresKernelError::SecuritySnapshot)
}

fn local_user_invariant(
    relation: &'static str,
    record: String,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation,
        record,
        rule,
    }
}

pub(super) async fn lock_catalogue_health_identity(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    transaction
        .batch_execute(
            "LOCK TABLE _orna_kernel.security_principals,
                        _orna_kernel.security_local_peer_credentials
             IN SHARE ROW EXCLUSIVE MODE",
        )
        .await
        .map_err(PostgresKernelError::Database)
}

pub(super) async fn insert_catalogue_health_identity(
    transaction: &Transaction<'_>,
    uid: u32,
) -> Result<(), PostgresKernelError> {
    let principal = CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes().to_vec();
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_principals (id, kind, status)
             VALUES ($1, 'service', 'active')",
            &[&principal],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    transaction
        .execute(
            "INSERT INTO _orna_kernel.security_local_peer_credentials (uid, principal_id)
             VALUES ($1, $2)",
            &[&i64::from(uid), &principal],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(())
}

pub(super) async fn append_client_capability_audit<T>(
    transaction: &Transaction<'_>,
    authenticated_session: &AuthenticatedSession,
    active: &ActiveDatabaseRevision,
    target: InvocationTarget,
    execution: &Result<T, PostgresKernelError>,
) -> Result<(), PostgresKernelError> {
    match execution {
        Err(PostgresKernelError::ClientExecution(ClientExecutionError::CapabilityDenied {
            context,
            capability,
        })) => {
            let decision = SecurityAuditDecision::capability_denied(
                authenticated_session,
                InvocationTarget::new(context.function(), active.pair()),
                capability,
            )
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_audit_events",
                record: context.function().canonical(),
                rule: "capability denial names must be valid redacted names",
            })?;
            append_security_audit_event(transaction, decision).await?;
        }
        _ => {
            // A non-denial result means the stored capability gate passed.
            // Record that decision even when the local evaluator then returns
            // an external-contract or another execution error.
            for capability in stored_capability_names(active, target.function())? {
                let decision = SecurityAuditDecision::capability_allowed(
                    authenticated_session,
                    target,
                    capability,
                )
                .map_err(|_| PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_audit_events",
                    record: target.function().canonical(),
                    rule: "stored capability names must be valid redacted names",
                })?;
                append_security_audit_event(transaction, decision).await?;
            }
        }
    }
    Ok(())
}

fn stored_capability_names(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<Vec<String>, PostgresKernelError> {
    let Some(definition) = active.catalogue().function_by_id(function) else {
        return Ok(Vec::new());
    };
    let Some(revision) = active
        .function_revisions()
        .iter()
        .find(|revision| revision.id() == definition.current_revision())
    else {
        return Ok(Vec::new());
    };
    if revision.artifact().version() != CAPABILITY_FORMAT_VERSION {
        return Ok(Vec::new());
    }
    let plan = CapabilityClientPlan::decode(revision.artifact().payload()).map_err(|_| {
        PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.function_revisions",
            record: revision.id().canonical(),
            rule: "a successfully evaluated capability artifact must decode",
        }
    })?;
    Ok(plan
        .requirements()
        .iter()
        .map(|requirement| requirement.name().to_owned())
        .collect())
}

pub(super) fn require_catalogue_health_identity_preserved(
    current: &SecuritySnapshot,
    candidate: &SecuritySnapshot,
) -> Result<(), PostgresKernelError> {
    match catalogue_health_service_uid(current)? {
        None => {
            if snapshot_contains_catalogue_health_identity(candidate) {
                return Err(catalogue_health_identity_error(
                    "_orna_kernel.security_principals",
                    "the reserved catalogue health service identity must be installed through its fixed setup",
                ));
            }
            Ok(())
        }
        Some(uid) => require_catalogue_health_snapshot(candidate, uid),
    }
}

fn snapshot_contains_catalogue_health_identity(snapshot: &SecuritySnapshot) -> bool {
    snapshot
        .principals()
        .any(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
        || snapshot
            .local_peer_credentials()
            .any(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
}

pub(crate) fn security_snapshots_match(left: &SecuritySnapshot, right: &SecuritySnapshot) -> bool {
    left.revision() == right.revision()
        && left.functions().eq(right.functions())
        && left.principals().eq(right.principals())
        && left.memberships().eq(right.memberships())
        && left.execute_grants().eq(right.execute_grants())
        && left.privilege_grants().eq(right.privilege_grants())
        && left
            .local_peer_credentials()
            .eq(right.local_peer_credentials())
}

pub(super) fn require_catalogue_health_snapshot(
    snapshot: &SecuritySnapshot,
    uid: u32,
) -> Result<(), PostgresKernelError> {
    let principal = snapshot
        .principals()
        .find(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    let credential = snapshot.local_peer_credentials().find(|credential| {
        credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID || credential.uid() == uid
    });
    if principal
        != Some(Principal::new(
            CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            PrincipalKind::Service,
            PrincipalStatus::Active,
        ))
        || credential
            != Some(LocalPeerCredential::new(
                uid,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            ))
    {
        return Err(catalogue_health_identity_error(
            "_orna_kernel.security_principals",
            "the reserved catalogue health service identity must be preserved",
        ));
    }
    Ok(())
}

pub(super) fn catalogue_health_service_uid(
    snapshot: &SecuritySnapshot,
) -> Result<Option<u32>, PostgresKernelError> {
    let principal = snapshot
        .principals()
        .find(|principal| principal.id() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    let credential = snapshot
        .local_peer_credentials()
        .find(|credential| credential.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID);
    match (principal, credential) {
        (None, None) => Ok(None),
        (Some(principal), Some(credential))
            if principal
                == Principal::new(
                    CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                    PrincipalKind::Service,
                    PrincipalStatus::Active,
                ) =>
        {
            Ok(Some(credential.uid()))
        }
        (Some(_), None) => Err(catalogue_health_identity_error(
            "_orna_kernel.security_local_peer_credentials",
            "the reserved catalogue health service identity must be complete",
        )),
        _ => Err(catalogue_health_identity_error(
            "_orna_kernel.security_principals",
            "the reserved catalogue health principal must be an active service",
        )),
    }
}

pub(super) fn catalogue_health_identity_error(
    relation: &'static str,
    rule: &'static str,
) -> PostgresKernelError {
    PostgresKernelError::DurableInvariant {
        relation,
        record: CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.canonical(),
        rule,
    }
}

impl PostgresKernel {
    /// Provisions the deterministic local USER authority for one operating-system UID.
    ///
    /// This path is for the user-owned local server profile. It retains the
    /// catalogue-health identity, but does not grant the local user security
    /// administration or alter local peer authentication.
    pub async fn provision_local_user(
        &self,
        uid: u32,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        if uid == LOCAL_HEALTH_UID {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_local_peer_credentials",
                record: uid.to_string(),
                rule: "the local USER UID is reserved for the catalogue-health migration slot",
            });
        }
        let principal = local_user_principal_id(uid);
        if principal == PrincipalId::from_bytes([0; 16])
            || principal == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
        {
            return Err(PostgresKernelError::DurableInvariant {
                relation: "_orna_kernel.security_principals",
                record: principal.canonical(),
                rule: "the deterministic local USER identity is reserved or empty",
            });
        }

        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            let candidate = local_user_security_snapshot(&current, &active, uid, principal)?;
            require_complete_function_set(&active, &candidate)?;
            replace_security_rows(&transaction, &candidate).await?;
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&candidate, &recovered) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_privilege_grants",
                    record: principal.canonical(),
                    rule: "recovered local USER authority does not match the persisted security snapshot",
                });
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Installs the fixed local service identity used by catalogue health.
    ///
    /// Repeating the exact UID is idempotent. A partial or conflicting durable
    /// identity fails without repair.
    pub async fn install_catalogue_health_service(
        &self,
        uid: u32,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            let active = configure_and_recover(&transaction).await?;
            if active.catalogue_hash_context().standard().is_none() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.active_revision",
                    record: active.pair().catalogue().canonical(),
                    rule: "catalogue health service requires the accepted standard context",
                });
            }
            if active
                .catalogue()
                .function_by_id(CATALOGUE_HEALTH_FUNCTION_ID)
                .is_some()
            {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.catalogue_functions",
                    record: CATALOGUE_HEALTH_FUNCTION_ID.canonical(),
                    rule: "application catalogue uses the reserved catalogue health identity",
                });
            }
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            match catalogue_health_service_uid(&current)? {
                None => {
                    if current
                        .local_peer_credentials()
                        .any(|credential| credential.uid() == uid)
                    {
                        return Err(catalogue_health_identity_error(
                            "_orna_kernel.security_local_peer_credentials",
                            "the catalogue health UID already selects another principal",
                        ));
                    }
                    insert_catalogue_health_identity(&transaction, uid).await?;
                }
                Some(installed_uid) if installed_uid == uid => {}
                Some(_) => {
                    return Err(catalogue_health_identity_error(
                        "_orna_kernel.security_local_peer_credentials",
                        "the reserved catalogue health service identity must be complete",
                    ));
                }
            }
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            require_catalogue_health_snapshot(&recovered, uid)?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Grants the fixed catalogue-health service exactly one active application function.
    ///
    /// The expected pair prevents a stale source-apply caller from changing
    /// security for a later catalogue. The operation rebuilds the complete
    /// snapshot in one serializable transaction and is idempotent for the
    /// exact existing grant.
    pub async fn grant_catalogue_health_service_execute(
        &self,
        expected: RevisionPair,
        function: FunctionId,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, expected).await?;
            let active = configure_and_recover(&transaction).await?;
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            let uid = catalogue_health_service_uid(&current)?.ok_or_else(|| {
                catalogue_health_identity_error(
                    "_orna_kernel.security_principals",
                    "the reserved catalogue health service identity must be complete",
                )
            })?;
            require_catalogue_health_snapshot(&current, uid)?;
            if function == CATALOGUE_HEALTH_FUNCTION_ID {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "active catalogue",
                    record: function.canonical(),
                    rule: "the catalogue health intrinsic cannot receive an application grant",
                });
            }
            if active.catalogue().function_by_id(function).is_none() {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "active catalogue",
                    record: function.canonical(),
                    rule: "the requested function must exist in the active application catalogue",
                });
            }
            let mut grants = current.execute_grants().collect::<Vec<_>>();
            let requested_grant = ExecuteGrant::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                function,
            );
            if !grants.contains(&requested_grant) {
                grants.push(requested_grant);
            }
            let candidate = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                active.pair(),
                current.function_targets().collect(),
                current.principals().collect(),
                current.memberships().collect(),
                grants,
                current.local_peer_credentials().collect(),
                current.privilege_grants().collect(),
            )
            .map_err(PostgresKernelError::SecuritySnapshot)?;
            require_complete_function_set(&active, &candidate)?;
            insert_execute_grant_if_absent(&transaction, requested_grant).await?;
            let recovered = recover_security_snapshot_for_active(&transaction, &active).await?;
            if !security_snapshots_match(&candidate, &recovered) {
                return Err(PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_execute_grants",
                    record: function.canonical(),
                    rule: "recovered fixed-service grant does not match the persisted security snapshot",
                });
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }

    /// Atomically replaces all durable security decision records.
    pub async fn replace_security_snapshot(
        &self,
        snapshot: &SecuritySnapshot,
    ) -> Result<SecuritySnapshot, PostgresKernelError> {
        let mut session = self.open().await?;
        let operation = async {
            let transaction = session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::Serializable)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            lock_active_revision(&transaction, snapshot.revision()).await?;
            let active = recover_active_revision(&transaction).await?;
            require_complete_function_set(&active, snapshot)?;
            lock_catalogue_health_identity(&transaction).await?;
            let current = recover_security_snapshot_for_active(&transaction, &active).await?;
            require_catalogue_health_identity_preserved(&current, snapshot)?;
            replace_security_rows(&transaction, snapshot).await?;
            let recovered = recover_security_snapshot(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)?;
            Ok(recovered)
        }
        .await;
        finish_security_session(operation, session.shutdown().await)
    }
}
