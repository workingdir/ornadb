use std::{fs, path::Path, process::Command};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    CheckpointKey, CheckpointResetRequest, Component, ConsumerIdentity, NoFault,
    RuntimeIdentity, RuntimeState, StreamAdministrationOutcome, TableMutation,
};
use orna_stream_v1::CheckpointPrecondition;
use tempfile::TempDir;

const MAIN: &str = include_str!("fixtures/history_governance_main.orna");

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> (TempDir, Repository) {
    let directory = TempDir::new().expect("create temporary repository");
    git(
        directory.path(),
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(
        directory.path(),
        &["config", "user.name", "kierandrewett"],
    );
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    fs::write(directory.path().join("main.orna"), MAIN).expect("write fixture module");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "history fixture"]);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn checkpoint_key() -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new("history-tests").unwrap(),
            root: Component::new("main").unwrap(),
            function: Component::new("consume").unwrap(),
            binding: Component::new("default").unwrap(),
        },
        source_format: Component::new("history-source-v1").unwrap(),
        source: Component::new("fixture-source").unwrap(),
        partition_format: Component::new("history-partition-v1").unwrap(),
        partition: None,
        position_format: Component::new("history-position-v1").unwrap(),
    }
}

#[tokio::test]
async fn governance_reads_and_row_attestations_follow_immutable_checkpoint_cuts() {
    assert!(MAIN.contains("history_fixture_marker"));
    let (_directory, repository) = repository();
    let state = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [41; 16],
            repository_id: [42; 16],
        },
        [43; 32],
    )
    .await
    .expect("open runtime");
    let writer = state.acquire_lease([44; 16]).await.expect("acquire writer");
    let key = checkpoint_key();
    assert_eq!(
        state.pause_stream(writer, key.clone()).await.unwrap(),
        StreamAdministrationOutcome::Paused { changed: true }
    );

    let generation_zero = state
        .select_historical_snapshot(0)
        .await
        .expect("select initialized generation");
    state
        .reset_checkpoint(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: CheckpointPrecondition {
                    version: 0,
                    committed: None,
                },
                to: orna_stream_v1::Position {
                    token: Component::new("position-one").unwrap(),
                },
                reason: "operator rewind".into(),
            },
        )
        .await
        .expect("reset paused stream");
    let activation = state.begin_activation().await.expect("capture activation");
    state
        .commit_table_activation(
            writer,
            &activation,
            &[TableMutation::new([45; 16], "records", vec![1], Some(b"row".to_vec()))
                .unwrap()],
            [46; 32],
            &NoFault,
        )
        .await
        .expect("commit generation one");
    let generation_one = state
        .select_historical_snapshot(1)
        .await
        .expect("select generation one");

    let admin_zero = state
        .admin_invocation_audits_at(&generation_zero)
        .await
        .unwrap();
    let resets_zero = state
        .checkpoint_reset_audits_at(&key, &generation_zero)
        .await
        .unwrap();
    assert!(admin_zero.rows().is_empty());
    assert!(resets_zero.rows().is_empty());
    assert_eq!(admin_zero.require_same_context(&resets_zero), Ok(()));

    let admin_one = state
        .admin_invocation_audits_at(&generation_one)
        .await
        .unwrap();
    let resets_one = state
        .checkpoint_reset_audits_at(&key, &generation_one)
        .await
        .unwrap();
    assert_eq!(admin_one.rows().len(), 2);
    assert_eq!(admin_one.rows()[0].observed_generation, 0);
    assert_eq!(admin_one.rows()[0].function, "sys.admin.pause_stream");
    assert_eq!(admin_one.rows()[1].function, "sys.admin.reset_checkpoint");
    assert_eq!(admin_one.snapshot_id(), generation_one.snapshot_id());
    assert_eq!(resets_one.rows().len(), 1);
    assert_eq!(resets_one.rows()[0].observed_generation, Some(0));
    assert_eq!(resets_one.rows()[0].reason, "operator rewind");
    assert_eq!(admin_one.require_same_context(&resets_one), Ok(()));
    assert_eq!(
        admin_zero.require_same_context(&resets_one),
        Err(orna_runtime_v1::RuntimeError::SnapshotContextMismatch)
    );

    let attestation_zero = state
        .attest_historical_snapshot(&generation_zero)
        .await
        .unwrap();
    let attestation_one = state
        .attest_historical_snapshot(&generation_one)
        .await
        .unwrap();
    assert_eq!(attestation_zero.row_count(), 0);
    assert_eq!(attestation_zero.table_count(), 0);
    assert_eq!(attestation_one.row_count(), 1);
    assert_eq!(attestation_one.table_count(), 1);
    assert_eq!(generation_one.generation(), 1);
    assert_eq!(attestation_one.generation(), 1);
    assert_eq!(attestation_one.snapshot_id(), generation_one.snapshot_id());
    assert_eq!(attestation_one.capture(), generation_one.capture());
    assert_eq!(attestation_one.admin_audit_sequence(), 2);
    assert_eq!(attestation_one.checkpoint_reset_audit_sequence(), 1);

    // Governance actions after a selected generation belong to the next
    // checkpoint cut. Unsafe provider text is retained only as a redaction
    // marker in the specialized reset history.
    state
        .reset_checkpoint(
            writer,
            CheckpointResetRequest {
                key: key.clone(),
                expected: CheckpointPrecondition {
                    version: 1,
                    committed: Some(orna_stream_v1::Position {
                        token: Component::new("position-one").unwrap(),
                    }),
                },
                to: orna_stream_v1::Position {
                    token: Component::new("position-two").unwrap(),
                },
                reason: "operator\0private detail".into(),
            },
        )
        .await
        .expect("record redacted reset after generation one");
    let activation = state.begin_activation().await.expect("capture next activation");
    state
        .commit_table_activation(
            writer,
            &activation,
            &[TableMutation::new([47; 16], "records", vec![1], None).unwrap()],
            [48; 32],
            &NoFault,
        )
        .await
        .expect("commit generation two delete");
    let generation_two = state
        .select_historical_snapshot(2)
        .await
        .expect("select generation two");

    assert_eq!(
        state
            .admin_invocation_audits_at(&generation_one)
            .await
            .unwrap()
            .rows()
            .len(),
        2,
        "generation one must not absorb later governance actions"
    );
    assert_eq!(generation_one.generation(), 1, "pins retain their selected generation");
    assert_eq!(
        state
            .select_historical_snapshot(u64::MAX)
            .await
            .unwrap_err(),
        orna_runtime_v1::RuntimeError::SnapshotNotFound
    );
    let admin_two = state
        .admin_invocation_audits_at(&generation_two)
        .await
        .unwrap();
    assert_eq!(admin_two.rows().len(), 3);
    assert_eq!(admin_two.rows()[2].observed_generation, 1);
    let resets_two = state
        .checkpoint_reset_audits_at(&key, &generation_two)
        .await
        .unwrap();
    assert_eq!(resets_two.rows().len(), 2);
    assert_eq!(resets_two.rows()[1].observed_generation, Some(1));
    assert_eq!(resets_two.rows()[1].reason, "<redacted>");
    assert!(resets_two.rows()[1].redacted);
    let attestation_two = state
        .attest_historical_snapshot(&generation_two)
        .await
        .unwrap();
    assert_eq!(attestation_two.generation(), 2);
    assert_eq!(attestation_two.row_count(), 0);
    assert_eq!(attestation_two.table_count(), 0);
    assert_eq!(attestation_two.admin_audit_sequence(), 3);
    assert_eq!(attestation_two.checkpoint_reset_audit_sequence(), 2);

    drop(state);
    let reopened = RuntimeState::open(
        &repository,
        RuntimeIdentity {
            database_id: [41; 16],
            repository_id: [42; 16],
        },
        [43; 32],
    )
    .await
    .expect("reopen runtime");
    assert_eq!(
        reopened
            .checkpoint_reset_audits_at(&key, &generation_one)
            .await
            .unwrap()
            .rows(),
        resets_one.rows()
    );
}
