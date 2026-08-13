mod support;

use orna_core::{
    security::{
        CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_SERVICE_PRINCIPAL_ID, InvocationTarget,
        LocalPeerCredential, Principal, PrincipalKind, PrincipalStatus, SecurityAuditKind,
        SecurityAuditOutcome, SecuritySnapshot,
    },
    value::RuntimeValue,
};
use orna_kernel_postgres::{AuthenticatedRawCallResult, PostgresKernel, PostgresKernelError};
use support::{TestResult, failure, with_test_database};

const SERVICE_UID: u32 = 61_018;

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
