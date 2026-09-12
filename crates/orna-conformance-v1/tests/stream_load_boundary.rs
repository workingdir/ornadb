use orna_conformance_v1::{DurableTransactionalEvaluator, SourceUnit, StageOutcome};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeError, RuntimeIdentity, RuntimeState};
use std::{path::Path, process::Command};
use tempfile::TempDir;

fn git(path: &Path, arguments: &[&str]) {
    assert!(
        Command::new("git")
            .args(arguments)
            .current_dir(path)
            .status()
            .expect("git command")
            .success()
    );
}

fn repository() -> (TempDir, Repository) {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "--quiet"]);
    git(
        directory.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(
        directory.path(),
        &["config", "user.name", "conformance test"],
    );
    let repository = Repository::discover(directory.path()).expect("repository");
    (directory, repository)
}

fn identity() -> RuntimeIdentity {
    RuntimeIdentity {
        database_id: [71; 16],
        repository_id: [72; 16],
    }
}

fn stream_source() -> SourceUnit {
    SourceUnit {
        fixture_id: "stream-load-boundary".into(),
        source_id: "stream-load-boundary.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Reading(id: Int) { value: Int, }
            fn main() {
                Stream.from_list([1, 2], source_identity: "fixture:readings")
                    | for_each(value => {
                        Reading.insert({ id: value, value: value });
                    });
            }
        "#
        .into(),
    }
}

#[tokio::test]
async fn admitted_literal_stream_cannot_poll_or_invoke_its_callback_before_the_runner_owns_its_lease()
 {
    let (_directory, repository) = repository();
    let identity = identity();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let source = stream_source();
    let state = RuntimeState::open(&repository, identity, [73; 32])
        .await
        .expect("runtime state");

    assert_eq!(state.current_lease().await, Ok(None));
    assert_eq!(state.latest_checkpoint().await, Ok(None));
    assert!(state.pending().await.expect("initial mutations").is_empty());
    assert!(
        state
            .run_observations()
            .await
            .expect("initial runs")
            .is_empty()
    );
    assert!(
        state
            .stream_observations()
            .await
            .expect("initial streams")
            .is_empty()
    );
    assert!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("initial rows")
            .is_empty()
    );

    let blocker = state
        .acquire_lease([74; 16])
        .await
        .expect("blocking writer lease");
    let capture_before_admission = state.capture().await.expect("initial capture");
    assert_eq!(state.current_lease().await, Ok(Some(blocker)));

    // `execute_source` first admits this exact source, then routes its literal
    // Stream.from_list pipeline into execute_list_stream_source. The foreign
    // lease makes that real runner path stop before ListStreamSource polling or
    // ListTableHandler callback execution.
    assert!(matches!(
        evaluator
            .execute_source(&repository, identity, [75; 16], [73; 32], &source)
            .await,
        Err(RuntimeError::LeaseHeld)
    ));

    assert_eq!(state.current_lease().await, Ok(Some(blocker)));
    assert_eq!(state.capture().await, Ok(capture_before_admission));
    assert_eq!(state.latest_checkpoint().await, Ok(None));
    assert!(state.pending().await.expect("blocked mutations").is_empty());
    assert!(
        state
            .run_observations()
            .await
            .expect("blocked runs")
            .is_empty()
    );
    assert!(
        state
            .stream_observations()
            .await
            .expect("blocked streams")
            .is_empty()
    );
    assert!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("blocked rows")
            .is_empty(),
        "the for_each insert callback must not run before the runner owns the lease"
    );

    assert!(matches!(
        evaluator
            .execute_source(&repository, identity, blocker.owner_id, [73; 32], &source)
            .await,
        Ok(StageOutcome::Passed)
    ));

    assert_eq!(state.current_lease().await, Ok(Some(blocker)));
    let checkpoint = state
        .latest_checkpoint()
        .await
        .expect("runtime checkpoint")
        .expect("two list-item activation commits");
    assert_eq!(checkpoint.generation, 2);
    assert_eq!(checkpoint.mutation_sequence, 2);
    assert_eq!(state.pending().await.expect("stream mutations").len(), 2);
    for value in [1, 2] {
        let key = Value::int(value.into()).encode().expect("encoded key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("stream row")
                .is_some(),
            "the first runner-owned stream execution must invoke the callback for item {value}"
        );
    }
    assert!(
        state
            .run_observations()
            .await
            .expect("final runs")
            .is_empty()
    );
    assert!(
        state
            .stream_observations()
            .await
            .expect("final streams")
            .is_empty(),
        "the existing finite-list seam does not register sys.Stream observations"
    );
}
