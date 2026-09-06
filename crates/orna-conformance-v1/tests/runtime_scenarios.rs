use orna_conformance_v1::{
    BoundedEvaluator, ConformanceAdapter, Corpus, DurableTransactionalEvaluator, EvidenceStatus,
    Harness, RuntimeAdapter, RuntimeEvaluator, Scenario, SourceUnit, StageOutcome,
};
use orna_evaluator_v1::Limits;
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_storage_v1::LoosePath;
use std::process::Command;
use std::{fs, path::Path, process::Command as ProcessCommand};
use tempfile::TempDir;

fn scenario(id: &str) -> Scenario {
    let corpus = Corpus::load_default().expect("frozen corpus loads");
    serde_json::from_value(
        corpus.scenarios["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|scenario| scenario["id"] == id)
            .unwrap()
            .clone(),
    )
    .unwrap()
}

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
    fs::write(temp.path().join("main.orna"), "module main;\n").expect("source");
    git(temp.path(), &["add", "main.orna"]);
    git(temp.path(), &["commit", "--quiet", "-m", "initial"]);
    let repository = Repository::discover(temp.path()).expect("repository");
    (temp, repository)
}

fn durable_source(fixture_id: &str, source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

#[test]
fn pipeline_precedence_checks_all_three_parse_and_execution_obligations() {
    let outcome = BoundedEvaluator::default().run_scenario(&scenario("PIPE-002"));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    let mut runtime = BoundedEvaluator::new(Limits {
        max_steps: 1,
        ..Limits::default()
    });
    assert!(matches!(
        runtime.run_scenario(&scenario("PIPE-002")),
        StageOutcome::Failed(_)
    ));
    let mut changed = scenario("PIPE-002");
    changed.then.push("an additional precedence rule".into());
    assert!(matches!(
        BoundedEvaluator::default().run_scenario(&changed),
        StageOutcome::Skipped { .. }
    ));
}

#[test]
fn pipeline_insertion_executes_ordinary_function_argument_order() {
    let outcome = BoundedEvaluator::default().run_scenario(&scenario("PIPE-001"));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
    let mut runtime = BoundedEvaluator::new(Limits {
        max_steps: 1,
        ..Limits::default()
    });
    assert!(matches!(
        runtime.run_scenario(&scenario("PIPE-001")),
        StageOutcome::Failed(_)
    ));
    let mut changed = scenario("PIPE-001");
    changed
        .when
        .push("an additional lowering obligation".into());
    assert!(matches!(
        BoundedEvaluator::default().run_scenario(&changed),
        StageOutcome::Skipped { .. }
    ));
}

#[test]
fn let_rebinding_executes_exact_runtime_checks_and_migration_diagnostic() {
    let outcome = BoundedEvaluator::default().run_scenario(&scenario("LET-REBIND-091"));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");
}

#[test]
fn scenario_limit_failure_is_not_a_pass() {
    for limits in [
        Limits {
            max_source_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_steps: 1,
            ..Limits::default()
        },
    ] {
        let mut runtime = BoundedEvaluator::new(limits);
        let outcome = runtime.run_scenario(&scenario("LET-REBIND-091"));
        assert!(
            matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"),
            "{outcome:?}"
        );
    }
}

#[test]
fn changed_or_unimplemented_scenario_contracts_are_not_reported_as_executed() {
    let mut changed = scenario("LET-REBIND-091");
    changed
        .then
        .push("an additional unimplemented obligation".into());
    let mut runtime = BoundedEvaluator::default();
    assert!(matches!(
        runtime.run_scenario(&changed),
        StageOutcome::Skipped { .. }
    ));
    assert!(matches!(
        runtime.run_scenario(&scenario("TXN-003")),
        StageOutcome::Skipped { .. }
    ));
}

#[test]
fn harness_distinguishes_executed_rebinding_from_unimplemented_scenarios() {
    let report = Harness::new(Corpus::load_default().unwrap())
        .run(&mut RuntimeAdapter::new(BoundedEvaluator::default()));
    let executed = report
        .scenarios
        .iter()
        .filter(|scenario| scenario.status == EvidenceStatus::Passed)
        .collect::<Vec<_>>();
    assert_eq!(
        executed
            .iter()
            .map(|scenario| scenario.scenario.as_str())
            .collect::<Vec<_>>(),
        ["LET-REBIND-091", "PIPE-001", "PIPE-002"]
    );
    assert_eq!(
        report
            .scenarios
            .iter()
            .filter(|scenario| scenario.status == EvidenceStatus::Skipped)
            .count(),
        141
    );
}

#[test]
fn transaction_scenarios_execute_through_the_real_table_evaluator() {
    let mut runtime = RuntimeAdapter::new(orna_conformance_v1::TransactionalEvaluator::default());
    assert!(matches!(
        runtime.run_scenario(&scenario("TXN-001")),
        StageOutcome::Passed
    ));
    assert!(matches!(
        runtime.run_scenario(&scenario("TXN-002")),
        StageOutcome::Passed
    ));
}

#[tokio::test]
async fn transaction_scenarios_cross_the_durable_runtime_boundary() {
    let rollback = scenario("TXN-001");
    assert_eq!(
        rollback.requirements,
        ["ORNA-TXN-001", "ORNA-TXN-002", "ORNA-TXN-003"]
    );
    let (_temp, repository) = durable_repository();
    let evaluator = DurableTransactionalEvaluator::new("parent", Limits::default());
    let identity = RuntimeIdentity {
        database_id: [41; 16],
        repository_id: [42; 16],
    };
    let outcome = evaluator
        .execute_source(
            &repository,
            identity,
            [43; 16],
            [44; 32],
            &durable_source(
                "TXN-001",
                "pub table Note(id: Int) { text: Str, } fn child() { Note.insert({ id: 7, text: \"nested\" }); } fn parent() { child(); assert false; }",
            ),
        )
        .await
        .expect("durable rollback execution");
    assert!(
        matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-ASSERT")
    );
    let state = RuntimeState::open(&repository, identity, [44; 32])
        .await
        .expect("reopen after rollback");
    assert!(state.committed_table_rows("Note").await.unwrap().is_empty());

    let commit = scenario("TXN-002");
    assert_eq!(commit.requirements, ["ORNA-TXN-001"]);
    let (_temp, repository) = durable_repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let identity = RuntimeIdentity {
        database_id: [51; 16],
        repository_id: [52; 16],
    };
    let outcome = evaluator
        .execute_source(
            &repository,
            identity,
            [53; 16],
            [54; 32],
            &durable_source(
                "TXN-002",
                "pub table Order(id: Int) { text: Str, } pub table Payment(id: Int) { text: Str, } pub table Audit(id: Int) { text: Str, } fn main() { Order.insert({ id: 1, text: \"order\" }); Payment.insert({ id: 1, text: \"payment\" }); Audit.insert({ id: 1, text: \"audit\" }); assert Order.count() == 1; assert Payment.count() == 1; assert Audit.count() == 1; }",
            ),
        )
        .await
        .expect("durable commit execution");
    assert!(matches!(outcome, StageOutcome::Passed));
    let state = RuntimeState::open(&repository, identity, [54; 32])
        .await
        .expect("reopen after commit");
    for table in ["Order", "Payment", "Audit"] {
        assert_eq!(state.committed_table_rows(table).await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn durable_source_publication_projects_the_frozen_prefix_into_git() {
    let (_temp, repository) = durable_repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let identity = RuntimeIdentity {
        database_id: [61; 16],
        repository_id: [62; 16],
    };
    let source = durable_source(
        "PUB-001",
        "pub table Note(id: Int) { text: Str, } fn main() { Note.insert({ id: 7, text: \"published\" }); }",
    );
    assert!(matches!(
        evaluator
            .execute_source(&repository, identity, [63; 16], [64; 32], &source)
            .await
            .unwrap(),
        StageOutcome::Passed
    ));
    let state = RuntimeState::open(&repository, identity, [64; 32])
        .await
        .expect("reopen runtime before publication");
    let checkpoint = state
        .latest_checkpoint()
        .await
        .unwrap()
        .expect("durable source checkpoint");
    let published = evaluator
        .publish_pending(
            &repository,
            &state,
            [65; 16],
            &checkpoint,
            |mutation| {
                let key = mutation
                    .key()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                LoosePath::for_key(mutation.table(), &[key])
            },
            "orna: publish durable source",
        )
        .await
        .unwrap();

    assert_eq!(repository.head().unwrap(), published.head().cloned());
    assert!(state.pending().await.unwrap().is_empty());
    let key = orna_foundation_v1::Value::int(7.into())
        .encode()
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let managed = LoosePath::for_key("Note", &[key]).unwrap();
    assert!(
        repository
            .managed_file_bytes(managed.as_managed_path())
            .unwrap()
            .is_some()
    );
}

#[test]
fn published_report_declares_only_the_scenarios_executed_by_the_composite_runner() {
    let output = Command::new(env!("CARGO_BIN_EXE_orna-conformance"))
        .output()
        .expect("conformance binary runs");
    assert!(output.status.success(), "conformance binary failed");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("conformance report is JSON");
    let declared = report["implementation_claim"]["executed_scenario_contracts"]
        .as_array()
        .expect("scenario execution claim is an array")
        .iter()
        .map(|value| value.as_str().expect("scenario ID is text"))
        .collect::<Vec<_>>();
    assert_eq!(
        declared,
        [
            "LET-REBIND-091",
            "PIPE-001",
            "PIPE-002",
            "TXN-001",
            "TXN-002",
            "LIVE-003",
            "SYS-RT-RENAME-100",
        ]
    );
    for scenario_id in [
        "LIVE-001",
        "LIVE-002",
        "LIVE-004",
        "STREAM-001",
        "STREAM-002",
    ] {
        let result = report["scenarios"]
            .as_array()
            .expect("scenario results are an array")
            .iter()
            .find(|result| result["scenario"] == scenario_id)
            .expect("scenario is present");
        assert_eq!(
            result["status"], "skipped",
            "{scenario_id} must remain skipped"
        );
    }
}
