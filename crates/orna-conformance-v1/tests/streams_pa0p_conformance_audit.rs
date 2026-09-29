//! Evidence-grade Streams chapter audit against the frozen Orna 1.0.0 text.
//!
//! Authority: `reference/Orna-1.0.0/source/11-streams.md`.
//! Verified normative line anchors: ORNA-CONSUMER-001..009 at lines 32-58,
//! ORNA-CONNECTOR-001 at line 48, and ORNA-STREAM-001..005 at lines 63-71.
//!
//! Initial executable evidence covers ORNA-STREAM-001/002/004 with a checked-in
//! source fixture. The same runtime path exposes an identity gap: its consumer
//! function key is the spelling `main`, whereas ORNA-CONSUMER-001 requires the
//! invoked function's stable ObjectId. This file records that observed value;
//! it does not claim the identity requirement is conformant.

#[path = "../src/test_support.rs"]
mod test_support;

use std::{fs, path::Path, process::Command};

use orna_conformance_v1::{DurableTransactionalEvaluator, SourceUnit, StageOutcome};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_project_v1::ProjectLoader;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use tempfile::TempDir;

const FINITE_STREAM: &str = include_str!("fixtures/streams-pa0p-finite.orna");

fn repository(source: &str) -> (TempDir, Repository) {
    let directory = tempfile::tempdir_in("/var/tmp").expect("temporary repository");
    let initialized = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main", "--template="])
        .current_dir(directory.path())
        .output()
        .expect("start git init");
    assert!(initialized.status.success(), "initialize temporary repository");
    test_support::configure_fixture_git_identity(directory.path());
    fs::write(directory.path().join("main.orna"), source).expect("write checked-in source fixture");
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "fixture"]);
    let repository = Repository::discover(directory.path()).expect("discover repository");
    (directory, repository)
}

fn git(path: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .expect("start fixture git command");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [0x51; 16],
        repository_id: [0x52; 16],
    }
}

fn source_unit() -> SourceUnit {
    SourceUnit {
        fixture_id: "streams-pa0p-finite".into(),
        source_id: "main.orna".into(),
        parse_as: "module_unit".into(),
        source: FINITE_STREAM.into(),
    }
}

#[tokio::test]
async fn finite_fixture_is_inert_until_dispatch_and_commits_each_item() {
    let (_directory, repository) = repository(FINITE_STREAM);
    let identity = identity();
    let state = RuntimeState::open(&repository, identity, [0x53; 32])
        .await
        .expect("open runtime state");
    let before_load = state.capture().await.expect("capture before module load");

    let loaded = ProjectLoader::default()
        .load(&repository)
        .expect("load finite stream module");
    assert_eq!(
        loaded
            .identities()
            .iter()
            .map(|module| module.logical_path())
            .collect::<Vec<_>>(),
        ["main.orna"]
    );
    assert_eq!(state.capture().await, Ok(before_load));
    assert_eq!(state.latest_checkpoint().await, Ok(None));
    assert!(state.stream_observations().await.unwrap().is_empty());
    assert!(state.committed_table_rows("Reading").await.unwrap().is_empty());

    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    assert_eq!(evaluator.admit_list_stream_source(&source_unit()), StageOutcome::Passed);
    assert_eq!(
        evaluator
            .execute_list_stream_source(
                &repository,
                identity,
                [0x54; 16],
                [0x53; 32],
                &source_unit(),
            )
            .await
            .expect("explicit finite dispatch"),
        StageOutcome::Passed
    );

    let streams = state.stream_observations().await.expect("stream evidence");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].items_committed, 2);
    assert_eq!(
        state
            .latest_checkpoint()
            .await
            .expect("checkpoint read")
            .expect("one checkpoint per finite item")
            .generation,
        2
    );
    for value in [1, 2] {
        let key = Value::int(value.into()).encode().expect("encoded table key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("read delivered row")
                .is_some()
        );
    }

    // Evidence-grade gap: the bounded source adapter keys the consumer by the
    // entry spelling. ORNA-CONSUMER-001 instead requires the function ObjectId.
    assert_eq!(streams[0].checkpoint.consumer.function.as_str(), "main");
}
