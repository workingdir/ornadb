#![cfg(unix)]

use std::os::unix::net::UnixStream;

use orna_core::{
    PrincipalId,
    security::{
        LocalPeerAuthenticationError, LocalPeerCredential, Principal, PrincipalKind,
        PrincipalStatus, SecuritySnapshot,
    },
};
use orna_postgres::{PostgresKernel, PostgresKernelError};
use orna_server::{LocalAuthenticationError, authenticate_local_stream};

#[path = "../../orna-kernel-postgres/tests/support/mod.rs"]
mod postgres_test_support;

use postgres_test_support::{TestResult, failure, with_test_database};

const LOCAL_USER: PrincipalId = PrincipalId::from_bytes([0x41; 16]);

#[tokio::test]
#[ignore = "requires the Compose PostgreSQL development service"]
async fn authenticates_and_revokes_the_connected_streams_actual_peer() -> TestResult<()> {
    with_test_database(|database| async move {
        let kernel: PostgresKernel = database.connection_string().parse()?;
        kernel.bootstrap().await?;
        let active = kernel.recover().await?;
        let functions = active
            .catalogue()
            .functions()
            .iter()
            .map(|definition| definition.id())
            .collect();
        let uid = nix::unistd::getuid().as_raw();
        let granted = SecuritySnapshot::new_with_local_peer_credentials(
            active.pair(),
            functions,
            vec![Principal::new(
                LOCAL_USER,
                PrincipalKind::User,
                PrincipalStatus::Active,
            )],
            vec![],
            vec![],
            vec![LocalPeerCredential::new(uid, LOCAL_USER)],
        )?;
        kernel.replace_security_snapshot(&granted).await?;

        let (accepted, _client) = UnixStream::pair()?;
        let authenticated = authenticate_local_stream(&kernel, &accepted).await?;
        require(
            authenticated.principal() == LOCAL_USER && authenticated.active_roles().is_empty(),
            "the connected peer did not establish the exact empty-role session",
        )?;

        let revoked = SecuritySnapshot::new(
            active.pair(),
            granted.functions().collect(),
            granted.principals().collect(),
            vec![],
            vec![],
        )?;
        kernel.replace_security_snapshot(&revoked).await?;
        let (accepted_after_revocation, _client_after_revocation) = UnixStream::pair()?;
        let error = authenticate_local_stream(&kernel, &accepted_after_revocation)
            .await
            .expect_err("a removed peer mapping must fail authentication");
        require(
            matches!(
                error,
                LocalAuthenticationError::Kernel {
                    source: PostgresKernelError::LocalPeerAuthentication(
                        LocalPeerAuthenticationError::UnknownUid
                    )
                }
            ),
            "the revoked connected peer returned the wrong typed error",
        )?;

        let inspection = database.open().await?;
        let value: i32 = inspection.client().query_one("SELECT 1", &[]).await?.get(0);
        inspection.shutdown().await?;
        require(
            value == 1,
            "failed authentication left the temporary database unavailable",
        )
    })
    .await
}

fn require(condition: bool, message: &'static str) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(failure(message))
    }
}
