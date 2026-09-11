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
        &["config", "user.email", "test@example.invalid"],
    );
    git(temp.path(), &["config", "user.name", "conformance test"]);
    git(temp.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(temp.path().join("main.orna"), "module main;\n").expect("source");
    git(temp.path(), &["add", "main.orna"]);
    git(temp.path(), &["commit", "--quiet", "-m", "initial"]);
    let repository = Repository::discover(temp.path()).expect("repository");
    (temp, repository)
}

fn source(fixture_id: &str, body: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: format!(
            r#"
                pub table Reading(id: Int) {{ value: Float, }}
                fn total(): Float = Reading | map(reading => reading.value) | sum;
                fn parent() {{ {body} }}
            "#
        ),
    }
}

fn unsupported_min_source() -> SourceUnit {
    SourceUnit {
        fixture_id: "durable-float-min-rollback".into(),
        source_id: "durable-float-min-rollback.orna".into(),
        parse_as: "module_unit".into(),
        source: r#"
            pub table Reading(id: Int) { value: Float, }
            fn minimum() = Reading | map(reading => reading.value) | min();
            fn parent() {
                Reading.insert({ id: 1, value: 1.5f });
                minimum();
            }
        "#
        .into(),
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
        r#"
            Reading.insert({ id: 3, value: 1.0f });
            Reading.insert({ id: 2, value: -10000000000000000.0f });
            Reading.insert({ id: 1, value: 10000000000000000.0f });
            assert total() == 1.0f;
        "#,
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

    let reopened_read = source("durable-float-sum-reopen-read", "assert total() == 1.0f;");
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
            &unsupported_min_source(),
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
    let mut control_limits = Limits::default();
    control_limits.max_collection_items = 3;
    let control_evaluator = DurableTransactionalEvaluator::new("parent", control_limits);
    let control_write = source(
        "durable-float-sum-limit-control",
        "Reading.insert({ id: 99, value: 99.0f });",
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
        r#"
            Reading.insert({ id: 1, value: 1.0f });
            Reading.insert({ id: 2, value: 2.0f });
            Reading.insert({ id: 3, value: 3.0f });
            Reading.insert({ id: 4, value: 4.0f });
            assert total() == 10.0f;
        "#,
    );
    let default_evaluator = DurableTransactionalEvaluator::new("parent", Limits::default());
    assert!(matches!(
        default_evaluator
            .execute_source(&repository, identity, [82; 16], [83; 32], &seed)
            .await,
        Ok(StageOutcome::Passed)
    ));

    let mut limits = Limits::default();
    limits.max_collection_items = 3;
    let evaluator = DurableTransactionalEvaluator::new("parent", limits);
    let write = source(
        "durable-float-sum-limit-rollback",
        r#"
            Reading.insert({ id: 5, value: 5.0f });
            assert total() == 15.0f;
        "#,
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
