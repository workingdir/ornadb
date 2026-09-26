//! Regression for rejecting a superseded writer before table evaluation.
//!
//! Normative authority: reference/Orna-1.0.0/source/12-checkpoints.md,
//! DELIVERY-1 steps 3-4 and ORNA-CP-005: activation work belongs to one
//! owner-fenced activation and must not publish after an owner change.

use std::{
    convert::Infallible,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    ActivationError, ActivationWork, NoFault, RuntimeError, RuntimeIdentity, RuntimeState,
    TableMutation, run_table_activation,
};
use tempfile::{Builder, TempDir};

fn repository() -> (TempDir, Repository) {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&target).expect("create workspace test artifact directory");
    let directory = Builder::new()
        .prefix("orna-v1-activation-owner-fence-")
        .tempdir_in(target)
        .expect("create runtime repository directory");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("start git init for runtime fixture");
    assert!(
        initialized.status.success(),
        "initialize runtime repository"
    );
    let repository = Repository::discover(directory.path()).expect("discover runtime repository");
    (directory, repository)
}

#[tokio::test]
async fn superseded_writer_is_rejected_before_evaluator_runs() {
    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        [3; 32],
    )
    .await
    .expect("open fresh runtime");
    let stale = state
        .acquire_lease([4; 16])
        .await
        .expect("acquire original writer");

    let initial = state
        .begin_table_activation(&["owner-fence"])
        .await
        .expect("begin initial table activation");
    let initial_mutation = TableMutation::new(
        [5; 16],
        "owner-fence",
        b"row".to_vec(),
        Some(b"committed".to_vec()),
    )
    .expect("construct initial row mutation");
    state
        .commit_table_activation(
            stale,
            initial.context(),
            &[initial_mutation],
            [6; 32],
            &NoFault,
        )
        .await
        .expect("commit initial table row");

    let committed_before = state
        .committed_table_rows("owner-fence")
        .await
        .expect("read committed rows before takeover");
    let pending_before = state
        .pending()
        .await
        .expect("read pending mutations before takeover");
    assert!(
        !pending_before.is_empty(),
        "fixture must have pending state"
    );
    let checkpoint_before = state
        .latest_checkpoint()
        .await
        .expect("read checkpoint before takeover")
        .expect("initial activation creates a checkpoint");

    state
        .takeover_lease(stale, [7; 16])
        .await
        .expect("replace original writer lease");

    let evaluator_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = evaluator_calls.clone();
    let result = run_table_activation(
        &state,
        stale,
        &["owner-fence"],
        &NoFault,
        move |_| async move {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            let mutation = TableMutation::new(
                [8; 16],
                "owner-fence",
                b"row".to_vec(),
                Some(b"stale write".to_vec()),
            )
            .expect("construct stale evaluator mutation");
            Ok::<_, Infallible>(ActivationWork::new(vec![mutation], [9; 32], ()))
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(ActivationError::Runtime(RuntimeError::OwnerLost))
    ));
    assert_eq!(
        evaluator_calls.load(Ordering::SeqCst),
        0,
        "a stale writer must be rejected before evaluator work"
    );
    assert_eq!(
        state
            .committed_table_rows("owner-fence")
            .await
            .expect("read committed rows after stale activation"),
        committed_before
    );
    assert_eq!(
        state
            .pending()
            .await
            .expect("read pending mutations after stale activation"),
        pending_before
    );
    assert_eq!(
        state
            .latest_checkpoint()
            .await
            .expect("read checkpoint after stale activation"),
        Some(checkpoint_before)
    );
}
