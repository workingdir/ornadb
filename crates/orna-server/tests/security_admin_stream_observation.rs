//! Authenticated, read-only current-observation coverage.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use orna_core::{
    PrincipalId,
    security::{AuthenticatedSession, Principal, PrincipalKind, PrincipalStatus, SecuritySnapshot},
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, RequestIdentity, RunObservationRegistration,
    RuntimeIdentity, RuntimeState, StreamObservationRegistration,
};
use orna_server::security_admin::{
    AuthenticatedCurrentRun, AuthenticatedCurrentRunError, AuthenticatedCurrentStreamError,
    resolve_authenticated_current_run, resolve_authenticated_current_stream,
};

const OWNER: PrincipalId = PrincipalId::from_bytes([0x31; 16]);
const OTHER: PrincipalId = PrincipalId::from_bytes([0x32; 16]);
const REVISION: [u8; 32] = [0x33; 32];

struct TemporaryRepository(PathBuf);

impl TemporaryRepository {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orna-current-stream-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("temporary repository directory");
        for arguments in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.email", "test@example.invalid"].as_slice(),
            ["config", "user.name", "test"].as_slice(),
            ["config", "commit.gpgsign", "false"].as_slice(),
        ] {
            assert!(
                Command::new("git")
                    .args(arguments)
                    .current_dir(&path)
                    .status()
                    .expect("git must run")
                    .success()
            );
        }
        Self(path)
    }

    fn repository(&self) -> Repository {
        Repository::discover(&self.0).expect("repository discovery")
    }
}

impl Drop for TemporaryRepository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session(principal: PrincipalId) -> AuthenticatedSession {
    SecuritySnapshot::new(
        REVISION,
        vec![],
        vec![Principal::new(
            principal,
            PrincipalKind::User,
            PrincipalStatus::Active,
        )],
        vec![],
        vec![],
    )
    .expect("security snapshot")
    .bind_authenticated_session(principal, vec![])
    .expect("authenticated session")
}

fn checkpoint(principal: PrincipalId) -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new(principal.canonical()).expect("principal component"),
            root: Component::new("root").expect("root component"),
            function: Component::new("callback").expect("function component"),
            binding: Component::new("binding").expect("binding component"),
        },
        source_format: Component::new("test").expect("source format"),
        source: Component::new("source").expect("source"),
        partition_format: Component::new("test").expect("partition format"),
        partition: Some(Component::new("partition").expect("partition")),
        position_format: Component::new("test").expect("position format"),
    }
}

#[tokio::test]
async fn resolves_only_the_authenticated_consumers_current_fenced_run_and_stream() {
    let repository = TemporaryRepository::new();
    let state = RuntimeState::open(
        &repository.repository(),
        RuntimeIdentity {
            database_id: [0x41; 16],
            repository_id: [0x42; 16],
        },
        [0x43; 32],
    )
    .await
    .expect("runtime state");
    let writer = state.acquire_lease([0x44; 16]).await.expect("writer lease");
    let key = checkpoint(OWNER);
    let started = state
        .begin_observed_request(
            RunObservationRegistration {
                request: RequestIdentity {
                    session_id: [0x45; 16],
                    request_id: [0x46; 16],
                },
                consumer_identity: key.consumer.clone(),
                function: "pkg.consume".into(),
                source_identity: Some("source".into()),
                invocation_id: [0x47; 16],
            },
            [0x48; 32],
            writer,
        )
        .await
        .expect("observed request admission");
    let run = started.run.expect("new run observation");
    let stream = state
        .register_stream_observation(StreamObservationRegistration {
            run: run.id,
            producer: "source-object".into(),
            consumer: Some("pkg.consume".into()),
            checkpoint: key.clone(),
        })
        .await
        .expect("stream observation");
    let reference = stream.reference(&run).expect("stream reference");
    let fence = state
        .runtime_observation_fence(writer)
        .await
        .expect("current-runtime fence");

    assert_eq!(
        resolve_authenticated_current_run(
            &state,
            &fence,
            &session(OWNER),
            &run.reference().expect("run reference")
        )
        .await,
        Ok(AuthenticatedCurrentRun {
            reference: run.reference().expect("run reference"),
            observation: run.clone(),
        }),
    );
    assert_eq!(
        resolve_authenticated_current_run(
            &state,
            &fence,
            &session(OTHER),
            &run.reference().expect("run reference")
        )
        .await,
        Err(AuthenticatedCurrentRunError::OwnershipDenied),
    );

    assert_eq!(
        resolve_authenticated_current_stream(&state, &fence, &session(OWNER), &reference).await,
        Ok(orna_server::security_admin::AuthenticatedCurrentStream {
            reference: reference.clone(),
            checkpoint: key,
        }),
    );
    assert_eq!(
        resolve_authenticated_current_stream(&state, &fence, &session(OTHER), &reference).await,
        Err(AuthenticatedCurrentStreamError::OwnershipDenied),
    );
}
