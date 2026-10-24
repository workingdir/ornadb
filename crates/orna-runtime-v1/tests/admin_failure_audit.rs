//! Admin failure audit rows record the admitted generation and a stable class,
//! and replaying a stored failure reproduces the original `RuntimeError`.
//!
//! Both tests drive the stream-reference administration boundary the live host
//! calls: the reference is pinned to the activation capture, and a committed
//! activation makes that capture stale before the administrative call runs.
use std::{path::PathBuf, process::Command};

use orna_foundation_v1::{CanonicalValue, CwdCapture};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, Mutation, NoFault, RequestIdentity,
    RunObservationRegistration, RuntimeError, RuntimeIdentity, RuntimeState,
    StreamObservationRegistration, WriterLease,
};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

fn repository() -> (TempDir, Repository) {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    std::fs::create_dir_all(&target).expect("create test artifact directory");
    let directory = Builder::new()
        .prefix("orna-runtime-admin-failure-audit-")
        .tempdir_in(target)
        .expect("create repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init");
    assert!(initialized.status.success(), "initialize repository");
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

async fn open(repository: &Repository, repository_id: [u8; 16]) -> RuntimeState {
    RuntimeState::open(
        repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id,
        },
        [3; 32],
    )
    .await
    .expect("open runtime")
}

fn key() -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new("principal").unwrap(),
            root: Component::new("root").unwrap(),
            function: Component::new("consume").unwrap(),
            binding: Component::new("binding").unwrap(),
        },
        source_format: Component::new("source-format").unwrap(),
        source: Component::new("source").unwrap(),
        partition_format: Component::new("partition-format").unwrap(),
        partition: Some(Component::new("partition").unwrap()),
        position_format: Component::new("position-format-v1").unwrap(),
    }
}

fn mutation(id: u8, payload: &[u8]) -> Mutation {
    Mutation {
        id: [id; 16],
        payload: payload.to_vec(),
        digest: Sha256::digest(payload).into(),
    }
}

fn generation_of(capture: &CwdCapture) -> u64 {
    u64::try_from(
        capture
            .generation()
            .to_string()
            .parse::<i64>()
            .expect("generation fits i64"),
    )
    .expect("generation is non-negative")
}

/// A live `sys.StreamRef` whose activation capture a later commit made stale.
struct StaleStreamReference {
    _directory: TempDir,
    state: RuntimeState,
    writer: WriterLease,
    reference: CanonicalValue,
    activation: CwdCapture,
    current_generation: u64,
}

async fn stale_stream_reference() -> StaleStreamReference {
    let (directory, repository) = repository();
    let state = open(&repository, [2; 16]).await;
    let writer = state.acquire_lease([4; 16]).await.expect("acquire writer");
    let request = RequestIdentity {
        session_id: [10; 16],
        request_id: [11; 16],
    };
    state
        .reserve_request(request, [12; 32])
        .await
        .expect("reserve request");
    let run = state
        .register_run_observation(RunObservationRegistration {
            request,
            consumer_identity: key().consumer,
            function: "pkg.consume".into(),
            source_identity: None,
            invocation_id: [13; 16],
        })
        .await
        .expect("register run");
    let stream = state
        .register_stream_observation(StreamObservationRegistration {
            run: run.id,
            producer: "source-object".into(),
            consumer: Some("pkg.consume".into()),
            checkpoint: key(),
        })
        .await
        .expect("register stream");
    let encoded = stream
        .reference(&run)
        .expect("stream reference")
        .as_row_ref()
        .encode()
        .expect("encode stream reference");
    let reference = CanonicalValue::decode(&encoded).expect("stream reference value");

    // The activation pins the capture the reference was issued under; a
    // committed activation then advances the CWD generation.
    let activation = state.capture().await.expect("activation capture");
    assert_eq!(
        activation, run.snapshot,
        "reference is pinned to the activation"
    );
    let advanced = state
        .commit(
            writer,
            &activation,
            &mutation(5, b"advance generation"),
            [6; 32],
            &NoFault,
        )
        .await
        .expect("commit advances the CWD generation");
    assert_ne!(activation, advanced, "commit must advance the CWD capture");
    assert_eq!(state.capture().await.expect("current capture"), advanced);
    let current_generation = generation_of(&advanced);
    assert!(current_generation > generation_of(&activation));
    StaleStreamReference {
        _directory: directory,
        state,
        writer,
        reference,
        activation,
        current_generation,
    }
}

/// The one audit row of the failed call: the generation its lock observed,
/// which is never a placeholder 0, and the stable `stale_capture` class.
async fn assert_stale_capture_row(
    fixture: &StaleStreamReference,
    invocation_id: [u8; 16],
    function: &str,
) {
    let audits = fixture.state.admin_invocation_audits().await.unwrap();
    assert_eq!(audits.len(), 1, "one row per failed invocation: {audits:?}");
    let row = &audits[0];
    assert_eq!(row.invocation_id, invocation_id);
    assert_eq!(row.function, function);
    assert_eq!(row.terminal_outcome, "failure:stale_capture");
    assert_eq!(row.observed_generation, fixture.current_generation);
    assert!(!row.succeeded);
}

#[tokio::test]
async fn stale_pause_records_admitted_generation_and_replays_same_error() {
    let fixture = stale_stream_reference().await;
    let pause = || {
        fixture
            .state
            .pause_stream_reference_at_capture_with_invocation_id(
                fixture.writer,
                fixture.reference.clone(),
                Some("maintenance boundary".into()),
                &fixture.activation,
                [8; 16],
            )
    };

    let error = pause().await.expect_err("stale capture must fail");
    assert!(
        matches!(error, RuntimeError::StaleCapture { .. }),
        "{error:?}"
    );
    assert_stale_capture_row(&fixture, [8; 16], "sys.admin.pause_stream_with_reason").await;

    // Replay of the same invocation must reproduce the stored class. The row
    // stores only the class (ORNA-ADMIN-001), so the replayed StaleCapture
    // carries the capture current at replay: compare the class, not that payload.
    let replay = pause().await.expect_err("replayed stale capture must fail");
    assert!(
        matches!(replay, RuntimeError::StaleCapture { .. }),
        "{replay:?}"
    );
}

#[tokio::test]
async fn stale_resume_records_admitted_generation_and_replays_same_error() {
    let fixture = stale_stream_reference().await;
    let resume = || {
        fixture
            .state
            .resume_stream_reference_at_capture_with_invocation_id(
                fixture.writer,
                fixture.reference.clone(),
                &fixture.activation,
                [9; 16],
            )
    };

    let error = resume().await.expect_err("stale capture must fail");
    assert!(
        matches!(error, RuntimeError::StaleCapture { .. }),
        "{error:?}"
    );
    assert_stale_capture_row(&fixture, [9; 16], "sys.admin.resume_stream").await;

    let replay = resume()
        .await
        .expect_err("replayed stale capture must fail");
    assert!(
        matches!(replay, RuntimeError::StaleCapture { .. }),
        "{replay:?}"
    );
}
