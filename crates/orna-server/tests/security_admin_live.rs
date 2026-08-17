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
        PrivilegeClass, PrivilegeGrant, SecurityFunctionTarget, SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
};
use orna_compiler::{
    STD_INVOKE_ECHO_FUNCTION_ID, STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
    StandardApplicationCheckContext, check_standard_application, prepare_standard_application,
};
use orna_postgres::PostgresKernel;
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

        Ok(())
    })
    .await
}
