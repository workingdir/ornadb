use super::*;

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
