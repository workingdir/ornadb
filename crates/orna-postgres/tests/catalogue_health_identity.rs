mod support;

use orna_compiler::{
    StandardApplicationCheckContext, check_standard_application, prepare_standard_application,
};
use orna_core::{
    CatalogueRevisionId, FunctionId, SourceRevisionId,
    revision::{ActiveDatabaseRevision, DeployableRevision, RevisionPair},
    security::{
        CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, ExecuteDenial,
        ExecuteGrant, InvocationTarget, LocalPeerCredential, Principal, PrincipalKind,
        PrincipalStatus, SecurityAuditDenial, SecurityAuditKind, SecurityAuditOutcome,
        SecuritySnapshot,
    },
    source::{SourceBundle, SourceUnit},
    system::SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID,
    value::RuntimeValue,
};
use orna_postgres::{AuthenticatedRawCallResult, PostgresKernel, PostgresKernelError};
use support::{TestResult, failure, with_test_database};

const SERVICE_UID: u32 = 61_018;

/// One complete application source with two active SERVER functions.
const GRANT_SOURCE: &str = "CREATE SCHEMA grants_test;\n\
    CREATE TYPE grants_test.probe AS OBJECT (\n\
      stored BOOLEAN NOT NULL\n\
    );\n\
    CREATE SERVER FUNCTION grants_test.create_probe()\n\
    RETURNS ROWS (created REF grants_test.probe)\n\
    SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n\
    AS INSERT INTO grants_test.probe AS made (stored)\n\
    VALUES (TRUE) RETURNING REF(made);\n\
    CREATE SERVER FUNCTION grants_test.read_probes()\n\
    RETURNS ROWS (stored BOOLEAN)\n\
    SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
    AS SELECT probe.stored FROM grants_test.probe probe;\n";

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn installs_and_preserves_the_exact_catalogue_health_identity() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let active = kernel.apply_standard_upgrade(&upgrade).await?;

        let installed = kernel.install_catalogue_health_service(SERVICE_UID).await?;
        require_exact_identity(&installed, active.pair())?;
        let repeated = kernel.install_catalogue_health_service(SERVICE_UID).await?;
        require_exact_identity(&repeated, active.pair())?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;
        require(
            session.principal() == CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID
                && session.active_roles().is_empty(),
            "the installed recovery identity did not authenticate exactly",
        )?;
        let result = kernel
            .dispatch_authenticated_raw_call(&session, CATALOGUE_HEALTH_FUNCTION_ID)
            .await?;
        require(
            result == AuthenticatedRawCallResult::Client(RuntimeValue::Boolean(true)),
            "catalogue health did not return the exact Boolean value",
        )?;
        let audits = kernel.recover_security_audit_events().await?;
        require(
            audits.len() == 2,
            "catalogue health did not append exactly one EXECUTE audit after authentication",
        )?;
        let decision = audits[1].decision();
        require(
            decision.kind() == SecurityAuditKind::Execute
                && decision.outcome() == SecurityAuditOutcome::Allowed
                && decision.session_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.effective_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.authorising_principal() == Some(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID)
                && decision.target()
                    == Some(InvocationTarget::new(
                        CATALOGUE_HEALTH_FUNCTION_ID,
                        active.pair(),
                    ))
                && decision.denial().is_none(),
            "catalogue health audit facts differ",
        )?;

        let missing = SecuritySnapshot::new(active.pair(), vec![], vec![], vec![], vec![])?;
        let removal = kernel
            .replace_security_snapshot(&missing)
            .await
            .expect_err("complete security replacement must preserve the recovery identity");
        require(
            matches!(
                removal,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    rule: "the reserved catalogue health service identity must be preserved",
                    ..
                }
            ),
            "removing the recovery identity returned the wrong typed error",
        )?;
        require_exact_identity(
            &kernel.install_catalogue_health_service(SERVICE_UID).await?,
            active.pair(),
        )?;

        let inspection = database.open().await?;
        inspection
            .client()
            .execute(
                "DELETE FROM _orna_kernel.security_local_peer_credentials
                 WHERE principal_id = $1",
                &[&CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes().to_vec()],
            )
            .await?;
        inspection.shutdown().await?;

        let partial = kernel
            .install_catalogue_health_service(SERVICE_UID)
            .await
            .expect_err("a partial durable recovery identity must not be repaired");
        require(
            matches!(
                partial,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_local_peer_credentials",
                    rule: "the reserved catalogue health service identity must be complete",
                    ..
                }
            ),
            "partial recovery identity returned the wrong typed error",
        )?;
        let inspection = database.open().await?;
        let credential_count: i64 = inspection
            .client()
            .query_one(
                "SELECT count(*) FROM _orna_kernel.security_local_peer_credentials
                 WHERE principal_id = $1",
                &[&CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID.to_bytes().to_vec()],
            )
            .await?
            .get(0);
        inspection.shutdown().await?;
        require(
            credential_count == 0,
            "failed recovery identity setup repaired the protected credential",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn raw_dispatch_rejects_deferred_system_identity_before_allowed_audit() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let active = kernel.apply_standard_upgrade(&upgrade).await?;
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let session = kernel.authenticate_local_peer(SERVICE_UID).await?;

        let deferred = SYS_SECURITY_CREATE_PRINCIPAL_FUNCTION_ID;
        let denied = kernel
            .dispatch_authenticated_raw_call(&session, deferred)
            .await
            .expect_err("a deferred system identity must not enter raw dispatch");
        require(
            matches!(
                denied,
                PostgresKernelError::RawExecuteDenied {
                    pair,
                    function,
                    reason: ExecuteDenial::UnknownFunction,
                } if pair == active.pair() && function == deferred
            ),
            "deferred system raw dispatch returned the wrong public denial",
        )?;

        let execute = kernel
            .recover_security_audit_events()
            .await?
            .into_iter()
            .filter(|event| event.decision().kind() == SecurityAuditKind::Execute)
            .collect::<Vec<_>>();
        require(
            execute.len() == 1
                && execute[0].decision().outcome() == SecurityAuditOutcome::Denied
                && execute[0].decision().target()
                    == Some(InvocationTarget::new(deferred, active.pair()))
                && matches!(
                    execute[0].decision().denial(),
                    Some(SecurityAuditDenial::Execute(ExecuteDenial::UnknownFunction))
                ),
            "deferred system raw dispatch appended allowed EXECUTE evidence",
        )?;

        Ok(())
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn generic_security_replacement_cannot_create_the_recovery_identity() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let candidate = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            vec![],
            vec![Principal::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                PrincipalKind::Service,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(
                SERVICE_UID,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            )],
        )?;

        let error = kernel
            .replace_security_snapshot(&candidate)
            .await
            .expect_err("generic replacement must not install the recovery identity");
        require(
            matches!(
                error,
                PostgresKernelError::DurableInvariant {
                    relation: "_orna_kernel.security_principals",
                    rule: "the reserved catalogue health service identity must be installed through its fixed setup",
                    ..
                }
            ),
            "generic recovery identity installation returned the wrong typed error",
        )?;

        let recovered = kernel.recover_security_snapshot().await?;
        require(
            recovered.principals().next().is_none()
                && recovered.local_peer_credentials().next().is_none(),
            "failed generic installation changed protected security state",
        )
    })
    .await
}

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn fixed_service_grant_targets_exactly_one_active_application_function() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let empty = kernel.recover().await?;
        let upgrade = orna_standard::prepare_standard_upgrade(&empty)?;
        let standard = kernel.apply_standard_upgrade(&upgrade).await?;
        kernel.install_catalogue_health_service(SERVICE_UID).await?;
        let applied = kernel
            .apply(&application_candidate(GRANT_SOURCE, &standard, &upgrade)?)
            .await?;
        let pair = applied.pair();
        let create_probe = function_id(&applied, &["grants_test", "create_probe"])?;
        let read_probes = function_id(&applied, &["grants_test", "read_probes"])?;
        let fixed_grant = ExecuteGrant::new(CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, create_probe);

        // No application grant exists before the command.
        let before = kernel.recover_security_snapshot().await?;
        require(
            before.revision() == pair,
            "active revision changed before the grant",
        )?;
        require(
            before.functions().any(|id| id == create_probe)
                && before.functions().any(|id| id == read_probes),
            "applied functions are absent from the recovered security snapshot",
        )?;
        require(
            before.execute_grants().next().is_none(),
            "an application grant existed before the grant command",
        )?;

        // Grant exactly one active function.
        let granted = kernel
            .grant_catalogue_health_service_execute(pair, create_probe)
            .await?;
        require(
            granted.revision() == pair
                && granted.execute_grants().collect::<Vec<_>>() == [fixed_grant],
            "the returned snapshot must contain exactly the fixed-service grant",
        )?;

        // The recovered snapshot proves the same fact.
        let recovered = kernel.recover_security_snapshot().await?;
        require(
            recovered.revision() == pair
                && recovered.execute_grants().collect::<Vec<_>>() == [fixed_grant]
                && recovered.functions().any(|id| id == create_probe)
                && recovered.functions().any(|id| id == read_probes),
            "the recovered snapshot changed beyond the fixed-service grant",
        )?;

        // Repeating the same grant is idempotent.
        let repeated = kernel
            .grant_catalogue_health_service_execute(pair, create_probe)
            .await?;
        require(
            repeated.execute_grants().collect::<Vec<_>>() == [fixed_grant],
            "repeating the fixed-service grant changed the snapshot",
        )?;

        // A different function remains ungranted.
        let after = kernel.recover_security_snapshot().await?;
        let grants_after_repeat = after.execute_grants().collect::<Vec<_>>();
        require(
            grants_after_repeat == [fixed_grant]
                && !after
                    .execute_grants()
                    .any(|grant| grant.function() == read_probes),
            "a different active function received a grant",
        )?;

        // A wrong expected pair fails without changing the snapshot.
        let wrong_pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x11; 16]),
            CatalogueRevisionId::from_bytes([0x22; 16]),
        );
        let stale = kernel
            .grant_catalogue_health_service_execute(wrong_pair, create_probe)
            .await
            .expect_err("a stale expected pair must fail");
        require(
            matches!(stale, PostgresKernelError::SecurityRevisionMismatch { .. }),
            "a stale expected pair returned the wrong typed error",
        )?;
        let unchanged = kernel.recover_security_snapshot().await?;
        require(
            unchanged.execute_grants().collect::<Vec<_>>() == [fixed_grant],
            "a stale expected pair changed the security snapshot",
        )?;

        // An unknown function fails without changing the snapshot.
        let unknown = FunctionId::from_bytes([0x44; 16]);
        let missing = kernel
            .grant_catalogue_health_service_execute(pair, unknown)
            .await
            .expect_err("an unknown function must fail");
        require(
            matches!(missing, PostgresKernelError::DurableInvariant { .. }),
            "an unknown function returned the wrong typed error",
        )?;
        let unchanged = kernel.recover_security_snapshot().await?;
        require(
            unchanged.execute_grants().collect::<Vec<_>>() == [fixed_grant],
            "an unknown function changed the security snapshot",
        )?;

        Ok(())
    })
    .await
}

fn require_exact_identity(
    snapshot: &SecuritySnapshot,
    pair: orna_core::revision::RevisionPair,
) -> TestResult<()> {
    require(
        snapshot.revision() == pair,
        "recovery identity changed revision",
    )?;
    require(
        snapshot.functions().next().is_none(),
        "recovery identity invented an application function",
    )?;
    require(
        snapshot.principals().collect::<Vec<_>>()
            == [Principal::new(
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
                PrincipalKind::Service,
                PrincipalStatus::Active,
            )],
        "recovery identity principal facts differ",
    )?;
    require(
        snapshot.memberships().next().is_none() && snapshot.execute_grants().next().is_none(),
        "recovery identity invented a role or application grant",
    )?;
    require(
        snapshot.local_peer_credentials().collect::<Vec<_>>()
            == [LocalPeerCredential::new(
                SERVICE_UID,
                CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID,
            )],
        "recovery identity credential facts differ",
    )
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}

/// Prepare one deployable standard application revision from source.
fn application_candidate(
    source: &str,
    active: &ActiveDatabaseRevision,
    upgrade: &orna_standard::StandardUpgrade,
) -> TestResult<DeployableRevision> {
    let context = StandardApplicationCheckContext::try_new(
        active.catalogue(),
        upgrade.checked_standard_library(),
    )
    .map_err(|error| failure(format!("standard application context failed: {error}")))?;
    let bundle = SourceBundle::new([SourceUnit::new("main.orna", source)])?;
    let report = check_standard_application(&bundle, &context);
    if !report.diagnostics().is_empty() {
        return Err(failure(format!(
            "standard application diagnostics prevented candidate preparation: {:?}",
            report.diagnostics()
        )));
    }
    Ok(prepare_standard_application(
        &report,
        active.pair(),
        active,
    )?)
}

/// The canonical identity of one active catalogue function by exact name.
fn function_id(active: &ActiveDatabaseRevision, name: &[&str]) -> TestResult<FunctionId> {
    active
        .catalogue()
        .functions()
        .iter()
        .find(|function| {
            function
                .name()
                .parts()
                .iter()
                .map(String::as_str)
                .eq(name.iter().copied())
        })
        .map(|function| function.id())
        .ok_or_else(|| {
            failure(format!(
                "function {name:?} is absent from the active catalogue"
            ))
        })
}
