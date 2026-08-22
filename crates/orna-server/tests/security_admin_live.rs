//! Live proof: the installed `orna security` admin surface runs against the
//! Compose PostgreSQL development service.
//!
//! The proof boots the standard chain through the V1-to-V2 upgrade, installs
//! one real application SERVER function on top of it, replaces the security
//! snapshot with one that maps the invoking local peer to the proof admin
//! principal holding the `SecurityAdmin` privilege, and drives
//! `run_security_admin_with_kernel`: identity reads, principal and role
//! mutation, privilege grant and check, execution check, and a disabled
//! principal.

#![cfg(unix)]

mod support;

#[path = "../../orna-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use orna_core::{
    FunctionId, PrincipalId,
    security::{
        ExecuteGrant, LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus,
        PrivilegeClass, PrivilegeDenial, PrivilegeGrant, RoleMembership, SecurityAdminAuditOperation,
        SecurityAuditKind, SecurityAuditOutcome, SecurityFunctionTarget, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
};
use orna_core::system::{
    SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
    SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID, SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
    SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
};
use orna_compiler::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    StandardApplicationCheckContext, check_standard_application, prepare_standard_application,
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_server::{
    InstalledSecurityAdminError, InstalledSecurityAdminOperation,
    InstalledSecurityAdminOutcome, InstalledSecurityAdminRequest, run_security_admin_with_kernel,
};
use postgres_test_support::{TestDatabase, TestResult, failure, with_test_database};

const ADMIN_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([0x4a; 16]);
const SECOND_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([0x4b; 16]);
const ROLE_PRINCIPAL: PrincipalId = PrincipalId::from_bytes([0x4c; 16]);

/// One real application SERVER function installed on top of the standard
/// library, so the security snapshot binds the exact active function set.
const APPLICATION_SOURCE: &str = "CREATE SCHEMA app;\n\
    CREATE TYPE app.flag AS OBJECT (value BOOLEAN NOT NULL);\n\
    CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN)\n\
    TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f;\n";

/// Asserts one live condition, failing the whole test with a typed error.
fn require(condition: bool, message: impl Into<String>) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

fn kernel(database: &TestDatabase) -> PostgresKernel {
    database.connection_string().parse().expect("kernel URL")
}

/// Boots the standard chain, installs one application function, and grants
/// the calling local peer the proof admin principal the `SecurityAdmin`
/// privilege plus one direct execute grant on the application function.
/// Returns the recovered application function identity.
///
/// Only the admin principal is seeded. The second user and the role are
/// created through the admin surface, exactly like the postgres-side proof.
async fn install_admin_snapshot(database: &TestDatabase) -> TestResult<FunctionId> {
    let kernel = kernel(database);
    kernel.bootstrap().await.map_err(|error| failure(format!("bootstrap failed: {error}")))?;
    let active = kernel
        .recover()
        .await
        .map_err(|error| failure(format!("recover failed: {error}")))?;
    let upgrade = orna_standard::prepare_standard_upgrade_v1_to_v2(&active)
        .map_err(|error| failure(format!("upgrade prepare failed: {error}")))?;
    kernel
        .apply_standard_upgrade(&upgrade)
        .await
        .map_err(|error| failure(format!("upgrade apply failed: {error}")))?;

    let active = kernel
        .recover()
        .await
        .map_err(|error| failure(format!("post-upgrade recover failed: {error}")))?;
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        failure("the standard snapshot is pinned by the V1-to-V2 upgrade")
    })?;

    // One real application function, checked against the pinned standard.
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", APPLICATION_SOURCE)])
        .map_err(|error| failure(format!("application bundle failed: {error}")))?;
    let report = check_standard_application(
        &bundle,
        &StandardApplicationCheckContext::try_new(
            active.catalogue(),
            upgrade.checked_standard_library(),
        )
        .map_err(|error| failure(format!("application context failed: {error}")))?,
    );
    require(
        report.diagnostics().is_empty(),
        format!("application source did not compile: {:?}", report.diagnostics()),
    )?;
    let applied = kernel
        .apply(&prepare_standard_application(&report, active.pair(), &active).map_err(
            |error| failure(format!("application prepare failed: {error}")),
        )?)
        .await
        .map_err(|error| failure(format!("application apply failed: {error}")))?;
    let application_function = applied
        .catalogue()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["app", "read"])
        .ok_or_else(|| failure("the application function was not recovered"))?
        .id();

    let uid = nix::unistd::geteuid().as_raw();
    let security = SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
        applied.pair(),
        vec![
            SecurityFunctionTarget::verified_standard(
                STD_INVOKE_ECHO_FUNCTION_ID,
                standard.revision(),
                STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            ),
            SecurityFunctionTarget::application(application_function),
        ],
        vec![Principal::new(ADMIN_PRINCIPAL, PrincipalKind::User, PrincipalStatus::Active)],
        vec![],
        // The direct execute grant gives the bare can_execute check one
        // genuinely allowed path; the role path below is a privilege class.
        vec![ExecuteGrant::new(ADMIN_PRINCIPAL, application_function)],
        vec![LocalPeerCredential::new(uid, ADMIN_PRINCIPAL)],
        vec![PrivilegeGrant::new(ADMIN_PRINCIPAL, PrivilegeClass::SecurityAdmin, None)
            .map_err(|error| failure(format!("admin grant failed: {error}")))?],
    )
    .map_err(|error| failure(format!("security snapshot failed: {error}")))?;
    kernel
        .replace_security_snapshot(&security)
        .await
        .map_err(|error| failure(format!("snapshot replace failed: {error}")))?;
    Ok(application_function)
}

/// Runs one installed security-admin operation through the hidden seam and
/// returns the rendered stdout.
async fn admin_run(
    database: &TestDatabase,
    operation: InstalledSecurityAdminOperation,
) -> TestResult<(
    Result<InstalledSecurityAdminOutcome, InstalledSecurityAdminError>,
    Vec<u8>,
)> {
    let mut stdout = Vec::new();
    let outcome = run_security_admin_with_kernel(
        kernel(database),
        InstalledSecurityAdminRequest::new(operation),
        &mut stdout,
    )
    .await;
    Ok((outcome, stdout))
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn proves_installed_security_admin_end_to_end() -> TestResult<()> {
    with_test_database(|database| async move {
        let application_function = install_admin_snapshot(&database).await?;

        // whoami: the local peer authenticates to the admin principal and the
        // identity read renders the canonical principal.
        let (outcome, stdout) = admin_run(
            &database,
            InstalledSecurityAdminOperation::SessionPrincipal,
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "the identity read must complete",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the identity stdout was not UTF-8"))?;
        require(
            text.contains(&format!("\"principal\":\"{}\"", ADMIN_PRINCIPAL.canonical())),
            "whoami did not render the admin principal: {text}",
        )?;

        // create_principal and create_role create fresh identities; the
        // seeded snapshot contains only the admin principal.
        let (outcome, _) = admin_run(
            &database,
            InstalledSecurityAdminOperation::CreatePrincipal {
                principal: SECOND_PRINCIPAL,
                kind: PrincipalKind::User,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "create_principal must complete",
        )?;
        let (outcome, _) = admin_run(
            &database,
            InstalledSecurityAdminOperation::CreateRole {
                role: ROLE_PRINCIPAL,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "create_role must complete",
        )?;

        // grant_role: the second principal joins the role.
        let (outcome, _) = admin_run(
            &database,
            InstalledSecurityAdminOperation::GrantRole {
                role: ROLE_PRINCIPAL,
                member: SECOND_PRINCIPAL,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "grant_role must complete",
        )?;

        // grant_privilege: the role gains the object-scoped execute class on
        // the application function through the admin surface.
        let (outcome, _) = admin_run(
            &database,
            InstalledSecurityAdminOperation::GrantPrivilege {
                grantee: ROLE_PRINCIPAL,
                class: PrivilegeClass::Execute,
                object: Some(application_function),
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "grant_privilege must complete",
        )?;

        // has_privilege: the role itself holds the granted class for the
        // object; the bare check considers the principal's own durable
        // grants, exactly like the postgres-side proof.
        let (outcome, stdout) = admin_run(
            &database,
            InstalledSecurityAdminOperation::HasPrivilege {
                principal: ROLE_PRINCIPAL,
                class: PrivilegeClass::Execute,
                object: Some(application_function),
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "has_privilege must complete",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the has_privilege stdout was not UTF-8"))?;
        require(
            text.contains("\"result\":true"),
            "has_privilege did not report the role's execute class: {text}",
        )?;

        // can_execute: the seeded direct execute grant allows the admin; a
        // principal without a direct grant fails closed with the stable
        // missing-grant reason.
        let (outcome, stdout) = admin_run(
            &database,
            InstalledSecurityAdminOperation::CanExecute {
                principal: ADMIN_PRINCIPAL,
                function: application_function,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "can_execute must complete",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the can_execute stdout was not UTF-8"))?;
        require(
            text.contains("\"result\":true"),
            "can_execute did not allow the direct grant: {text}",
        )?;
        let (outcome, stdout) = admin_run(
            &database,
            InstalledSecurityAdminOperation::CanExecute {
                principal: SECOND_PRINCIPAL,
                function: application_function,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "can_execute must complete for the ungranted principal",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the can_execute stdout was not UTF-8"))?;
        require(
            text.contains("\"result\":false")
                && text.contains("\"reason\":\"execute:missing-grant\""),
            "can_execute did not fail closed for the ungranted principal: {text}",
        )?;

        // disable_principal: the admin disables the second principal.
        let (outcome, _) = admin_run(
            &database,
            InstalledSecurityAdminOperation::DisablePrincipal {
                principal: SECOND_PRINCIPAL,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "disable_principal must complete",
        )?;

        // The injected-kernel seam starts after the installed-host inspection;
        // hostile service/package endpoint checks belong to the offline CLI
        // boundary and are intentionally not claimed by this proof.
        let snapshot = kernel(&database)
            .recover_security_snapshot()
            .await
            .map_err(|error| failure(format!("post-mutation security recover failed: {error}")))?;
        require(
            snapshot
                .principals()
                .any(|principal| {
                    principal.id() == SECOND_PRINCIPAL
                        && principal.status() == PrincipalStatus::Disabled
                }),
            "disable_principal did not persist the disabled principal state",
        )?;
        require(
            snapshot
                .memberships()
                .any(|membership| {
                    membership == RoleMembership::new(ROLE_PRINCIPAL, SECOND_PRINCIPAL)
                }),
            "grant_role did not persist the role membership",
        )?;
        require(
            snapshot.privilege_grants().any(|grant| {
                grant.grantee() == ROLE_PRINCIPAL
                    && grant.class() == PrivilegeClass::Execute
                    && grant.object() == Some(application_function)
            }),
            "grant_privilege did not persist the role's object-scoped execute grant",
        )?;

        // The role grant remains visible to the durable role check after the
        // later disable mutation; disabling the member does not rewrite the
        // role's own privilege row.
        let (outcome, stdout) = admin_run(
            &database,
            InstalledSecurityAdminOperation::HasPrivilege {
                principal: ROLE_PRINCIPAL,
                class: PrivilegeClass::Execute,
                object: Some(application_function),
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "post-mutation role has_privilege must complete",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the post-mutation has_privilege stdout was not UTF-8"))?;
        require(
            text.contains("\"result\":true"),
            "post-mutation role has_privilege lost the execute grant: {text}",
        )?;

        // A disabled principal remains unusable at the recovered execute
        // decision boundary, rather than merely carrying a disabled row.
        let (outcome, stdout) = admin_run(
            &database,
            InstalledSecurityAdminOperation::CanExecute {
                principal: SECOND_PRINCIPAL,
                function: application_function,
            },
        )
        .await?;
        require(
            outcome == Ok(InstalledSecurityAdminOutcome::Completed),
            "post-mutation disabled can_execute must complete",
        )?;
        let text = String::from_utf8(stdout)
            .map_err(|_| failure("the disabled can_execute stdout was not UTF-8"))?;
        require(
            text.contains("\"result\":false")
                && text.contains("\"reason\":\"execute:invalid-session\""),
            "disabled principal was not rejected by can_execute: {text}",
        )?;

        let expected_audits = [
            (
                SecurityAdminAuditOperation::CreatePrincipal,
                SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
                "security_admin:create_principal",
            ),
            (
                SecurityAdminAuditOperation::CreateRole,
                SYS_SECURITY_CREATE_ROLE_FUNCTION_ID,
                "security_admin:create_role",
            ),
            (
                SecurityAdminAuditOperation::GrantRole,
                SYS_SECURITY_GRANT_ROLE_FUNCTION_ID,
                "security_admin:grant_role",
            ),
            (
                SecurityAdminAuditOperation::GrantPrivilege,
                SYS_SECURITY_GRANT_PRIVILEGE_FUNCTION_ID,
                "security_admin:grant_privilege",
            ),
            (
                SecurityAdminAuditOperation::DisablePrincipal,
                SYS_SECURITY_DISABLE_PRINCIPAL_FUNCTION_ID,
                "security_admin:disable_principal",
            ),
        ];
        let events = kernel(&database)
            .recover_security_audit_events()
            .await
            .map_err(|error| failure(format!("security-admin audit recover failed: {error}")))?;
        let security_admin_events = events
            .iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::SecurityAdmin)
            .collect::<Vec<_>>();
        require(
            security_admin_events.len() == expected_audits.len(),
            format!(
                "expected {} security-admin audit events, found {}",
                expected_audits.len(),
                security_admin_events.len()
            ),
        )?;
        for (event, (operation, target, _)) in
            security_admin_events.iter().zip(expected_audits.iter())
        {
            let decision = event.decision();
            require(
                decision.outcome() == SecurityAuditOutcome::Allowed
                    && decision.session_principal() == Some(ADMIN_PRINCIPAL)
                    && decision.security_admin_operation() == Some(*operation)
                    && decision.security_admin_target() == Some(*target)
                    && decision.security_admin_denial().is_none(),
                format!(
                    "security-admin audit event {} lost exact operation/principal/target evidence",
                    event.sequence()
                ),
            )?;
        }

        // The protected row carries only the closed operation detail. Exact
        // equality rejects argument-bearing details (principal IDs, role
        // members, classes, objects, or any other payload), while the NULL
        // columns prove this event is not an invocation/revision record.
        let audit_session = database.open().await?;
        let rows = audit_session
            .client()
            .query(
                "SELECT event_kind, outcome, session_principal_id,
                        effective_principal_id, authorising_principal_id,
                        function_id, source_revision_id, catalogue_revision_id,
                        denial_reason
                 FROM _orna_kernel.security_audit_events
                 WHERE event_kind = 'security_admin'
                 ORDER BY sequence",
                &[],
            )
            .await?;
        audit_session.shutdown().await?;
        require(
            rows.len() == expected_audits.len(),
            "protected security-admin row count differs from recovered history",
        )?;
        for (row, (_, target, detail)) in rows.iter().zip(expected_audits.iter()) {
            let event_kind: String = row.try_get("event_kind")?;
            let outcome: String = row.try_get("outcome")?;
            let session_principal: Vec<u8> = row.try_get("session_principal_id")?;
            let effective_principal: Option<Vec<u8>> =
                row.try_get("effective_principal_id")?;
            let authorising_principal: Option<Vec<u8>> =
                row.try_get("authorising_principal_id")?;
            let function: Vec<u8> = row.try_get("function_id")?;
            let source_revision: Option<Vec<u8>> = row.try_get("source_revision_id")?;
            let catalogue_revision: Option<Vec<u8>> = row.try_get("catalogue_revision_id")?;
            let denial_reason: Option<String> = row.try_get("denial_reason")?;
            require(
                event_kind == "security_admin"
                    && outcome == "allowed"
                    && session_principal == ADMIN_PRINCIPAL.to_bytes().to_vec()
                    && effective_principal.is_none()
                    && authorising_principal.is_none()
                    && function == target.to_bytes().to_vec()
                    && source_revision.is_none()
                    && catalogue_revision.is_none()
                    && denial_reason.as_deref() == Some(*detail),
                format!("security-admin row contains unexpected payload or shape: {row:?}"),
            )?;
        }

        // A session bound while the admin grant existed must be re-authorised
        // against the current snapshot before a later mutation is accepted.
        let stale_session = snapshot
            .bind_authenticated_session(ADMIN_PRINCIPAL, vec![])
            .map_err(|error| failure(format!("stale admin session bind failed: {error}")))?;
        let without_security_admin =
            SecuritySnapshot::new_with_function_targets_local_peer_credentials_and_privilege_grants(
                snapshot.revision(),
                snapshot.function_targets().collect(),
                snapshot.principals().collect(),
                snapshot.memberships().collect(),
                snapshot.execute_grants().collect(),
                snapshot.local_peer_credentials().collect(),
                snapshot
                    .privilege_grants()
                    .filter(|grant| grant.class() != PrivilegeClass::SecurityAdmin)
                    .collect(),
            )
            .map_err(|error| failure(format!("security-admin removal snapshot failed: {error}")))?;
        kernel(&database)
            .replace_security_snapshot(&without_security_admin)
            .await
            .map_err(|error| failure(format!("security-admin removal snapshot replace failed: {error}")))?;

        let denied = kernel(&database)
            .grant_role(&stale_session, ROLE_PRINCIPAL, ADMIN_PRINCIPAL)
            .await
            .expect_err("a stale admin session must not grant a later role membership");
        require(
            matches!(
                denied,
                PostgresKernelError::SecurityAdminDenied {
                    reason: PrivilegeDenial::MissingPrivilege {
                        requested: PrivilegeClass::SecurityAdmin,
                    },
                }
            ),
            "stale admin mutation returned the wrong typed denial",
        )?;
        let after_denied = kernel(&database)
            .recover_security_snapshot()
            .await
            .map_err(|error| failure(format!("post-denial security recover failed: {error}")))?;
        require(
            !after_denied
                .memberships()
                .any(|membership| membership == RoleMembership::new(ROLE_PRINCIPAL, ADMIN_PRINCIPAL)),
            "stale admin mutation added the target role membership",
        )?;

        Ok(())
    })
    .await
}
