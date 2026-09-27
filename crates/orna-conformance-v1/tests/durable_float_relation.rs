use orna_conformance_v1::{DurableTransactionalEvaluator, SourceUnit, StageOutcome};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use std::path::Path;
use std::{fs, process::Command as ProcessCommand};
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) {
    assert!(
        ProcessCommand::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git command")
            .success()
    );
}

fn durable_repository() -> (TempDir, Repository) {
    let temp = TempDir::new().expect("temporary repository");
    git(temp.path(), &["init", "--quiet"]);
    git(
        temp.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(temp.path(), &["config", "user.name", "kierandrewett"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(
        temp.path().join("main.orna"),
        include_str!("fixtures/durable-float-repository-main.orna"),
    )
    .expect("source");
    git(temp.path(), &["add", "main.orna"]);
    git(temp.path(), &["commit", "--quiet", "-m", "initial"]);
    let repository = Repository::discover(temp.path()).expect("repository");
    (temp, repository)
}

fn source(fixture_id: &str, source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

#[tokio::test]
async fn durable_float_sum_reopens_persisted_rows_in_canonical_order() {
    let (_temp, repository) = durable_repository();
    let identity = RuntimeIdentity {
        database_id: [71; 16],
        repository_id: [72; 16],
    };
    let evaluator = DurableTransactionalEvaluator::new("parent", Limits::default());

    let write = source(
        "durable-float-sum-reopen",
        include_str!("fixtures/durable-float-sum-reopen.orna"),
    );
    assert!(matches!(
        evaluator
            .execute_source(&repository, identity, [73; 16], [74; 32], &write)
            .await,
        Ok(StageOutcome::Passed)
    ));

    let state = RuntimeState::open(&repository, identity, [74; 32])
        .await
        .expect("reopen after Float sum commit");
    assert_eq!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("persisted Float rows")
            .len(),
        3
    );
    drop(state);

    let reopened_read = source(
        "durable-float-sum-reopen-read",
        include_str!("fixtures/durable-float-sum-reopen-read.orna"),
    );
    let reopened_outcome = evaluator
        .execute_source(&repository, identity, [73; 16], [74; 32], &reopened_read)
        .await;
    assert!(
        matches!(&reopened_outcome, Ok(StageOutcome::Passed)),
        "{reopened_outcome:?}"
    );
}

#[tokio::test]
async fn durable_unsupported_float_aggregate_rolls_back_staged_rows() {
    let (_temp, repository) = durable_repository();
    let identity = RuntimeIdentity {
        database_id: [76; 16],
        repository_id: [77; 16],
    };
    let evaluator = DurableTransactionalEvaluator::new("parent", Limits::default());

    let outcome = evaluator
        .execute_source(
            &repository,
            identity,
            [78; 16],
            [79; 32],
            &source(
                "durable-float-min-rollback",
                include_str!("fixtures/durable-float-min-rollback.orna"),
            ),
        )
        .await
        .expect("durable unsupported aggregate execution");
    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-UNSUPPORTED"
    ));

    let state = RuntimeState::open(&repository, identity, [79; 32])
        .await
        .expect("reopen after unsupported aggregate rollback");
    assert!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("rolled-back Float rows")
            .is_empty()
    );
}

#[tokio::test]
async fn durable_failed_float_sum_rolls_back_only_tentative_rows() {
    let (_control_temp, control_repository) = durable_repository();
    let control_limits = Limits {
        max_collection_items: 3,
        ..Default::default()
    };
    let control_evaluator = DurableTransactionalEvaluator::new("parent", control_limits);
    let control_write = source(
        "durable-float-sum-limit-control",
        include_str!("fixtures/durable-float-sum-limit-control.orna"),
    );
    assert!(matches!(
        control_evaluator
            .execute_source(
                &control_repository,
                RuntimeIdentity {
                    database_id: [85; 16],
                    repository_id: [86; 16],
                },
                [87; 16],
                [88; 32],
                &control_write,
            )
            .await,
        Ok(StageOutcome::Passed)
    ));
    let control_state = RuntimeState::open(
        &control_repository,
        RuntimeIdentity {
            database_id: [85; 16],
            repository_id: [86; 16],
        },
        [88; 32],
    )
    .await
    .expect("reopen after limited control write");
    assert_eq!(
        control_state
            .committed_table_rows("Reading")
            .await
            .expect("limited control write rows")
            .len(),
        1
    );

    let (_temp, repository) = durable_repository();
    let identity = RuntimeIdentity {
        database_id: [80; 16],
        repository_id: [81; 16],
    };
    let seed = source(
        "durable-float-sum-limit-seed",
        include_str!("fixtures/durable-float-sum-limit-seed.orna"),
    );
    let default_evaluator = DurableTransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        default_evaluator
            .execute_source(&repository, identity, [82; 16], [83; 32], &seed)
            .await,
        Ok(StageOutcome::Passed)
    ));

    let limits = Limits {
        max_collection_items: 3,
        ..Default::default()
    };
    let evaluator = DurableTransactionalEvaluator::new("parent", limits);
    let write = source(
        "durable-float-sum-limit-rollback",
        include_str!("fixtures/durable-float-sum-limit-rollback.orna"),
    );

    let outcome = evaluator
        .execute_source(&repository, identity, [82; 16], [83; 32], &write)
        .await
        .expect("durable failed aggregate execution");
    assert!(matches!(
        outcome,
        StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"
    ));

    let state = RuntimeState::open(&repository, identity, [83; 32])
        .await
        .expect("reopen after failed Float aggregate rollback");
    assert_eq!(
        state
            .committed_table_rows("Reading")
            .await
            .expect("persisted Float rows after aggregate failure")
            .len(),
        4
    );
    for id in [1, 2, 3, 4] {
        let key = Value::int(id.into()).encode().expect("encoded Reading key");
        assert!(
            state
                .committed_table_row("Reading", &key)
                .await
                .expect("preexisting Float row")
                .is_some(),
            "preexisting row {id} was lost after aggregate failure"
        );
    }
    let tentative_key = Value::int(5.into())
        .encode()
        .expect("encoded tentative key");
    assert_eq!(
        state
            .committed_table_row("Reading", &tentative_key)
            .await
            .expect("tentative Float row lookup"),
        None,
        "tentative row escaped aggregate failure rollback"
    );
}
