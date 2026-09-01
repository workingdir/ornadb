//! Direct local SQLite adapter for protected security administration.

use std::{io::Write, path::PathBuf};

use orna_core::{
    FunctionId, PrincipalId,
    security::{
        CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDecision, PrincipalStatus, PrivilegeClass,
        PrivilegeDecision, PrivilegeDenial, PrivilegeGrant, SecuritySnapshot,
    },
};
use orna_sqlite::{SqliteConfig, SqliteError, SqliteRevisionStore, SqliteSecurityMutation};

use crate::{
    InstalledSecurityAdminError, InstalledSecurityAdminErrorKind, InstalledSecurityAdminOperation,
    InstalledSecurityAdminOutcome, InstalledSecurityAdminRequest,
};

/// Runs one security-admin operation directly against a local SQLite database.
pub fn run_sqlite_security_admin(
    database_path: impl Into<PathBuf>,
    request: InstalledSecurityAdminRequest,
    stdout: &mut impl Write,
) -> Result<InstalledSecurityAdminOutcome, InstalledSecurityAdminError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            admin_error(InstalledSecurityAdminErrorKind::Internal, error.to_string())
        })?;
    runtime.block_on(run_sqlite_security_admin_async(
        database_path.into(),
        request,
        stdout,
    ))
}

/// Grants the fixed catalogue-health service one direct execute grant in SQLite.
pub fn run_sqlite_security_grant_execute(
    database_path: impl Into<PathBuf>,
    function: FunctionId,
) -> Result<(), InstalledSecurityAdminError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            admin_error(InstalledSecurityAdminErrorKind::Internal, error.to_string())
        })?;
    runtime.block_on(async move {
        let store = open_store(database_path.into()).await?;
        let active = store.recover().await.map_err(|error| {
            admin_error(InstalledSecurityAdminErrorKind::Internal, error.to_string())
        })?;
        store
            .security_snapshot(&active)
            .await
            .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Internal, error))?;
        let uid = nix::unistd::geteuid().as_raw();
        store.provision_local_peer(uid).await.map_err(|error| {
            admin_sqlite_error(InstalledSecurityAdminErrorKind::Internal, error)
        })?;
        let session = authenticate(&store, &active, uid).await?;
        store
            .apply_security_mutation(
                &active,
                &session,
                SqliteSecurityMutation::GrantExecute {
                    grantee: CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                    function,
                },
            )
            .await
            .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Kernel, error))?;
        Ok(())
    })
}

async fn run_sqlite_security_admin_async(
    database_path: PathBuf,
    request: InstalledSecurityAdminRequest,
    stdout: &mut impl Write,
) -> Result<InstalledSecurityAdminOutcome, InstalledSecurityAdminError> {
    let store = open_store(database_path).await?;
    let active = store.recover().await.map_err(|error| {
        admin_error(InstalledSecurityAdminErrorKind::Internal, error.to_string())
    })?;
    store
        .security_snapshot(&active)
        .await
        .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Internal, error))?;
    let uid = nix::unistd::geteuid().as_raw();
    store
        .provision_local_peer(uid)
        .await
        .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Internal, error))?;
    let session = authenticate(&store, &active, uid).await?;
    let snapshot = store
        .security_snapshot(&active)
        .await
        .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Kernel, error))?;

    match request.operation() {
        InstalledSecurityAdminOperation::SessionPrincipal => {
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": "session_principal",
                    "principal": session.principal().canonical(),
                }),
            )?;
        }
        InstalledSecurityAdminOperation::EffectivePrincipal => {
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": "effective_principal",
                    "principal": session.principal().canonical(),
                }),
            )?;
        }
        InstalledSecurityAdminOperation::ActiveRoles => {
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": "active_roles",
                    "roles": session
                        .active_roles()
                        .iter()
                        .map(|role| role.canonical())
                        .collect::<Vec<_>>(),
                }),
            )?;
        }
        InstalledSecurityAdminOperation::ListGrants { grantee } => {
            require_admin(&snapshot, &session)?;
            let grants = snapshot
                .execute_grants()
                .filter(|grant| grant.grantee() == grantee)
                .map(|grant| serde_json::json!({"function": grant.function().canonical()}))
                .collect::<Vec<_>>();
            let privileges = snapshot
                .privilege_grants()
                .filter(|grant| grant.grantee() == grantee)
                .map(|grant| {
                    serde_json::json!({
                        "class": grant.class().to_string(),
                        "object": grant.object().map(|function| function.canonical()),
                    })
                })
                .collect::<Vec<_>>();
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": "list_grants",
                    "principal": grantee.canonical(),
                    "grants": grants,
                    "privileges": privileges,
                }),
            )?;
        }
        InstalledSecurityAdminOperation::CanExecute {
            principal,
            function,
        } => {
            let decision = execute_decision(&snapshot, principal, function);
            let (result, reason) = execute_result(decision);
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": "can_execute",
                    "principal": principal.canonical(),
                    "function": function.canonical(),
                    "result": result,
                    "reason": reason,
                }),
            )?;
        }
        InstalledSecurityAdminOperation::HasPrivilege {
            principal,
            class,
            object,
        } => {
            let decision = privilege_decision(&snapshot, principal, class, object);
            let (result, reason) = match decision {
                PrivilegeDecision::Allowed { .. } => (true, None),
                PrivilegeDecision::Denied(denial) => (false, Some(denial.audit_reason())),
            };
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": "has_privilege",
                    "principal": principal.canonical(),
                    "class": class.to_string(),
                    "object": object.map(|function| function.canonical()),
                    "result": result,
                    "reason": reason,
                }),
            )?;
        }
        mutation => {
            let mutation = sqlite_mutation(mutation);
            let snapshot = store
                .apply_security_mutation(&active, &session, mutation)
                .await
                .map_err(|error| {
                    admin_sqlite_error(InstalledSecurityAdminErrorKind::Kernel, error)
                })?;
            write_json_line(
                stdout,
                &serde_json::json!({
                    "operation": mutation_name(mutation),
                    "principals": snapshot.principals().count(),
                    "roles": snapshot.memberships().count(),
                    "grants": snapshot.execute_grants().count(),
                    "privileges": snapshot.privilege_grants().count(),
                }),
            )?;
        }
    }
    Ok(InstalledSecurityAdminOutcome::Completed)
}

async fn open_store(path: PathBuf) -> Result<SqliteRevisionStore, InstalledSecurityAdminError> {
    SqliteRevisionStore::open(&SqliteConfig::new(path))
        .await
        .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Internal, error))
}

async fn authenticate(
    store: &SqliteRevisionStore,
    active: &orna_core::revision::ActiveDatabaseRevision,
    uid: u32,
) -> Result<orna_core::security::AuthenticatedSession, InstalledSecurityAdminError> {
    store
        .authenticate_local_peer(active, uid)
        .await
        .map_err(|error| admin_sqlite_error(InstalledSecurityAdminErrorKind::Internal, error))
}

fn require_admin(
    snapshot: &SecuritySnapshot,
    session: &orna_core::security::AuthenticatedSession,
) -> Result<(), InstalledSecurityAdminError> {
    let decision =
        session_privilege_decision(snapshot, session, PrivilegeClass::SecurityAdmin, None);
    if matches!(decision, PrivilegeDecision::Denied(_)) {
        return Err(InstalledSecurityAdminError::with_code(
            InstalledSecurityAdminErrorKind::Kernel,
            "security administration was denied".to_owned(),
            "security_admin:missing-privilege",
        ));
    }
    Ok(())
}

fn session_privilege_decision(
    snapshot: &SecuritySnapshot,
    session: &orna_core::security::AuthenticatedSession,
    class: PrivilegeClass,
    object: Option<FunctionId>,
) -> PrivilegeDecision {
    if matches!(class, PrivilegeClass::Inspect(_)) && object.is_some() {
        return PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { requested: class });
    }
    let Ok(bound_session) =
        snapshot.bind_authenticated_session(session.principal(), session.active_roles().to_vec())
    else {
        return PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { requested: class });
    };
    let mut grantees = Vec::with_capacity(bound_session.active_roles().len() + 1);
    grantees.push(bound_session.principal());
    grantees.extend(bound_session.active_roles().iter().copied());
    let granted = snapshot
        .privilege_grants()
        .filter(|grant| {
            grantees.contains(&grant.grantee())
                && (grant.object().is_none() || grant.object() == object)
        })
        .map(PrivilegeGrant::class)
        .collect::<Vec<_>>();
    orna_core::security::authorise_privilege(bound_session.principal(), class, object, &granted)
}

fn privilege_denied(class: PrivilegeClass) -> PrivilegeDecision {
    PrivilegeDecision::Denied(PrivilegeDenial::MissingPrivilege { requested: class })
}
fn privilege_decision(
    snapshot: &SecuritySnapshot,
    principal: PrincipalId,
    class: PrivilegeClass,
    object: Option<FunctionId>,
) -> PrivilegeDecision {
    if !snapshot.principals().any(|candidate| {
        candidate.id() == principal && candidate.status() == PrincipalStatus::Active
    }) {
        return privilege_denied(class);
    }
    if matches!(class, PrivilegeClass::Inspect(_)) && object.is_some() {
        return privilege_denied(class);
    }
    let granted = snapshot
        .privilege_grants()
        .filter(|grant| {
            grant.grantee() == principal && (grant.object().is_none() || grant.object() == object)
        })
        .map(PrivilegeGrant::class)
        .collect::<Vec<_>>();
    orna_core::security::authorise_privilege(principal, class, object, &granted)
}

fn execute_decision(
    snapshot: &SecuritySnapshot,
    principal: PrincipalId,
    function: FunctionId,
) -> ExecuteDecision {
    let Ok(session) = snapshot.bind_authenticated_session(principal, vec![]) else {
        return ExecuteDecision::Denied(orna_core::security::ExecuteDenial::InvalidSession);
    };
    snapshot.authorise_execute(
        &session,
        orna_core::security::InvocationTarget::new(function, snapshot.revision()),
    )
}

fn execute_result(decision: ExecuteDecision) -> (bool, Option<&'static str>) {
    match decision {
        ExecuteDecision::Allowed(_) => (true, None),
        ExecuteDecision::Denied(reason) => {
            let reason = match reason {
                orna_core::security::ExecuteDenial::InvalidSession => "execute:invalid-session",
                orna_core::security::ExecuteDenial::UnknownFunction => "execute:unknown-function",
                orna_core::security::ExecuteDenial::RevisionMismatch => "execute:revision-mismatch",
                orna_core::security::ExecuteDenial::MissingExecuteGrant => "execute:missing-grant",
                orna_core::security::ExecuteDenial::UnsupportedSecurityDefiner => {
                    "execute:unsupported-security-definer"
                }
            };
            (false, Some(reason))
        }
    }
}

fn sqlite_mutation(operation: InstalledSecurityAdminOperation) -> SqliteSecurityMutation {
    match operation {
        InstalledSecurityAdminOperation::CreatePrincipal { principal, kind } => {
            SqliteSecurityMutation::CreatePrincipal { principal, kind }
        }
        InstalledSecurityAdminOperation::DisablePrincipal { principal } => {
            SqliteSecurityMutation::DisablePrincipal { principal }
        }
        InstalledSecurityAdminOperation::CreateRole { role } => {
            SqliteSecurityMutation::CreateRole { role }
        }
        InstalledSecurityAdminOperation::GrantRole { role, member } => {
            SqliteSecurityMutation::GrantRole { role, member }
        }
        InstalledSecurityAdminOperation::RevokeRole { role, member } => {
            SqliteSecurityMutation::RevokeRole { role, member }
        }
        InstalledSecurityAdminOperation::GrantPrivilege {
            grantee,
            class,
            object,
        } => SqliteSecurityMutation::GrantPrivilege {
            grantee,
            class,
            object,
        },
        InstalledSecurityAdminOperation::RevokePrivilege {
            grantee,
            class,
            object,
        } => SqliteSecurityMutation::RevokePrivilege {
            grantee,
            class,
            object,
        },
        InstalledSecurityAdminOperation::SessionPrincipal
        | InstalledSecurityAdminOperation::EffectivePrincipal
        | InstalledSecurityAdminOperation::ActiveRoles
        | InstalledSecurityAdminOperation::ListGrants { .. }
        | InstalledSecurityAdminOperation::CanExecute { .. }
        | InstalledSecurityAdminOperation::HasPrivilege { .. } => {
            unreachable!("non-mutating operation reached SQLite mutation mapper")
        }
    }
}

fn mutation_name(mutation: SqliteSecurityMutation) -> &'static str {
    match mutation {
        SqliteSecurityMutation::CreatePrincipal { .. } => "create_principal",
        SqliteSecurityMutation::DisablePrincipal { .. } => "disable_principal",
        SqliteSecurityMutation::CreateRole { .. } => "create_role",
        SqliteSecurityMutation::GrantRole { .. } => "grant_role",
        SqliteSecurityMutation::RevokeRole { .. } => "revoke_role",
        SqliteSecurityMutation::GrantPrivilege { .. } => "grant_privilege",
        SqliteSecurityMutation::RevokePrivilege { .. } => "revoke_privilege",
        SqliteSecurityMutation::GrantExecute { .. } => "grant_execute",
    }
}

fn write_json_line(
    stdout: &mut impl Write,
    value: &serde_json::Value,
) -> Result<(), InstalledSecurityAdminError> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| {
        admin_error(
            InstalledSecurityAdminErrorKind::Rendering,
            format!("could not render security-admin output: {error}"),
        )
    })?;
    bytes.push(b'\n');
    stdout.write_all(&bytes).map_err(|error| {
        admin_error(
            InstalledSecurityAdminErrorKind::Rendering,
            format!("could not write security-admin output: {error}"),
        )
    })
}

fn admin_error(
    kind: InstalledSecurityAdminErrorKind,
    message: impl Into<String>,
) -> InstalledSecurityAdminError {
    InstalledSecurityAdminError::new(kind, message.into())
}

fn admin_sqlite_error(
    kind: InstalledSecurityAdminErrorKind,
    error: SqliteError,
) -> InstalledSecurityAdminError {
    admin_error(kind, format!("local SQLite backend error: {error}"))
}
