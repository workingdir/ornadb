use orna_conformance_v1::{
    BoundedEvaluator, ConformanceAdapter, Corpus, DurableTransactionalEvaluator, EvidenceStatus,
    Harness, ProjectEnvironment, ProjectExpectations, ProjectUnit, RuntimeAdapter,
    RuntimeEvaluator, Scenario, SourceUnit, StageOutcome,
};
use orna_evaluator_v1::Limits;
use orna_foundation_v1::Value;
use orna_protocol_v1::{
    DatabaseContext, Envelope, Limits as ProtocolLimits, Message, PresentationContext,
    canonical_request_fingerprint,
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RequestIdentity, RequestState, RuntimeError, RuntimeIdentity, RuntimeState};
use orna_storage_v1::LoosePath;
use std::process::{Command, Stdio};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command as ProcessCommand, Output},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

// The bounded profile includes the durable FAIL-001 witness, which performs
// 10,000 fenced retry transitions and reopens the runtime before publishing
// its report. Keep a finite subprocess bound, but allow slow SQLite/WAL
// filesystems to complete that bounded setup without changing any evidence
// status or claim rules.
const CONFORMANCE_PROCESS_TIMEOUT: Duration = Duration::from_secs(300);

fn run_conformance_with_timeout() -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-conformance"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("conformance binary starts");
    let stdout = child.stdout.take().expect("conformance stdout is piped");
    let stderr = child.stderr.take().expect("conformance stderr is piped");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stderr = stderr;
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + CONFORMANCE_PROCESS_TIMEOUT;

    loop {
        match child.try_wait().expect("conformance process status") {
            Some(status) => {
                let stdout = stdout_reader
                    .join()
                    .expect("conformance stdout reader")
                    .expect("conformance stdout read");
                let stderr = stderr_reader
                    .join()
                    .expect("conformance stderr reader")
                    .expect("conformance stderr read");
                return Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            None if Instant::now() >= deadline => {
                child
                    .kill()
                    .expect("terminate timed-out conformance process");
                let status = child.wait().expect("reap timed-out conformance process");
                let stdout = stdout_reader
                    .join()
                    .expect("conformance stdout reader after timeout")
                    .expect("conformance stdout read after timeout");
                let stderr = stderr_reader
                    .join()
                    .expect("conformance stderr reader after timeout")
                    .expect("conformance stderr read after timeout");
                panic!(
                    "conformance binary timed out after {:?} (status {status}); partial stdout: {}; stderr: {}",
                    CONFORMANCE_PROCESS_TIMEOUT,
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr),
                );
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

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
    let temp = TempDir::new_in("/var/tmp").expect("temporary repository");
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

fn durable_source(fixture_id: &str, source: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: fixture_id.into(),
        source_id: format!("{fixture_id}.orna"),
        parse_as: "module_unit".into(),
        source: source.into(),
    }
}

fn durable_project(fixture_id: &str, source: &str) -> ProjectUnit {
    ProjectUnit {
        fixture_id: fixture_id.into(),
        project_id: fixture_id.into(),
        environment_id: None,
        modules: vec![SourceUnit {
            fixture_id: fixture_id.into(),
            source_id: format!("{fixture_id}/main.orna"),
            parse_as: "module_unit".into(),
            source: source.into(),
        }],
        loose_rows: Vec::new(),
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

fn canonical_eval_fingerprint(
    unit: &SourceUnit,
    request: RequestIdentity,
    database: [u8; 16],
) -> [u8; 32] {
    canonical_request_fingerprint(
        request.session_id,
        &Envelope {
            request: Some(request.request_id),
            watch: None,
            message: Message::Eval {
                source: unit.source.clone(),
                database: DatabaseContext {
                    database,
                    snapshot: None,
                },
                presentation: PresentationContext {
                    locale: "en-GB".into(),
                    timezone: None,
                    width: None,
                    theme: "terminal/default".into(),
                    supported_kinds: vec!["text".into()],
                },
                fingerprint: [0; 32],
            },
            extensions: std::collections::BTreeMap::new(),
        },
        ProtocolLimits::default(),
    )
    .expect("canonical eval envelope is valid")
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
fn control_flow_executes_the_frozen_contract_in_independent_evaluators() {
    let outcome = BoundedEvaluator::default().run_scenario(&scenario("CFLOW-001"));
    assert!(matches!(outcome, StageOutcome::Passed), "{outcome:?}");

    let mut changed = scenario("CFLOW-001");
    changed.then.push("an additional control-flow rule".into());
    assert!(matches!(
        BoundedEvaluator::default().run_scenario(&changed),
        StageOutcome::Skipped { .. }
    ));
}

#[test]
fn control_flow_scenario_limit_failures_are_not_passes() {
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
        let outcome = runtime.run_scenario(&scenario("CFLOW-001"));
        assert!(
            matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-EVAL-LIMIT"),
            "{outcome:?}"
        );
    }
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
        ["CFLOW-001", "LET-REBIND-091", "PIPE-001", "PIPE-002"]
    );
    assert_eq!(
        report
            .scenarios
            .iter()
            .filter(|scenario| scenario.status == EvidenceStatus::Skipped)
            .count(),
        140
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
    let project = durable_project(
        "TXN-002",
        "pub table Order(id: Int) { text: Str, } pub table Payment(id: Int) { text: Str, } pub table Audit(id: Int) { text: Str, } fn main() { Order.insert({ id: 1, text: \"order\" }); Payment.insert({ id: 1, text: \"payment\" }); Audit.insert({ id: 1, text: \"audit\" }); assert Order.count() == 1; assert Payment.count() == 1; assert Audit.count() == 1; }",
    );
    let outcome = evaluator
        .execute_project(
            &repository,
            identity,
            [53; 16],
            [54; 32],
            &project,
            "main.main",
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
async fn durable_function_value_table_assertion_commits_and_rolls_back_candidate_rows() {
    let valid_source = durable_source(
        "ASSERT-FUNCTION-VALUE-VALID",
        r#"
            pub table Note(id: Int) {
                value: Int,
                assert valid_notes;
            }
            fn valid_notes(rows: Relation<Note>): Bool =
                rows | filter(note => note.value > 0) | count == 2;
            fn parent() {
                Note.insert({ id: 1, value: 10 });
                Note.insert({ id: 2, value: 20 });
            }
        "#,
    );
    let (_temp, repository) = durable_repository();
    let identity = RuntimeIdentity {
        database_id: [91; 16],
        repository_id: [92; 16],
    };
    let evaluator = DurableTransactionalEvaluator::new("parent", Limits::default());
    let valid_outcome = evaluator
        .execute_source(
            &repository,
            identity,
            [93; 16],
            [94; 32],
            &valid_source,
        )
        .await
        .expect("durable pure function assertion commit");
    assert!(
        matches!(valid_outcome, StageOutcome::Passed),
        "two positive Note rows must satisfy valid_notes: {valid_outcome:?}"
    );
    let valid_state = RuntimeState::open(&repository, identity, [94; 32])
        .await
        .expect("reopen after pure function assertion commit");
    let valid_rows = valid_state
        .committed_table_rows("Note")
        .await
        .expect("committed Note rows");
    assert_eq!(valid_rows.len(), 2);
    assert!(valid_rows
        .iter()
        .any(|(key, _)| key == &Value::int(1.into()).encode().unwrap()));
    assert!(valid_rows
        .iter()
        .any(|(key, _)| key == &Value::int(2.into()).encode().unwrap()));
    valid_state
        .recover_abandoned([93; 16], [95; 16])
        .await
        .expect("recover retained durable lease before rollback attempt");
    drop(valid_state);

    let invalid_source = durable_source(
        "ASSERT-FUNCTION-VALUE-INVALID",
        r#"
            pub table Note(id: Int) {
                value: Int,
                assert valid_notes;
            }
            fn valid_notes(rows: Relation<Note>): Bool =
                rows | filter(note => note.value > 0) | count == 2;
            fn parent() {
                Note.insert({ id: 3, value: 30 });
                Note.insert({ id: 4, value: -40 });
            }
        "#,
    );
    let invalid_outcome = evaluator
        .execute_source(
            &repository,
            identity,
            [95; 16],
            [94; 32],
            &invalid_source,
        )
        .await
        .expect("durable pure function assertion rollback");
    assert!(
        matches!(
            &invalid_outcome,
            StageOutcome::Failed(diagnostic)
                if diagnostic.code() == "ORNA-EVAL-TABLE-ASSERT"
        ),
        "invalid candidate must retain the table assertion diagnostic: {invalid_outcome:?}"
    );
    let invalid_state = RuntimeState::open(&repository, identity, [94; 32])
        .await
        .expect("reopen after pure function assertion rollback");
    let rows_after_rollback = invalid_state
        .committed_table_rows("Note")
        .await
        .expect("rolled-back Note rows");
    assert_eq!(
        rows_after_rollback.len(),
        2,
        "the assertion failure must preserve only the prior committed rows"
    );
    assert!(rows_after_rollback
        .iter()
        .all(|(key, _)| key == &Value::int(1.into()).encode().unwrap()
            || key == &Value::int(2.into()).encode().unwrap()));
}

#[tokio::test]
async fn assert_checkpoint_091_exposes_durable_runtime_adapter_evidence() {
    let contract = scenario("ASSERT-CHECKPOINT-091");
    assert_eq!(
        contract.title,
        "Assertion failure leaves a coupled stream checkpoint unchanged"
    );
    assert_eq!(
        contract.given,
        ["a replayable stream delivery writes rows that violate an assertion"]
    );
    assert_eq!(contract.when, ["commit the item activation"]);
    assert_eq!(
        contract.then,
        [
            "rows roll back",
            "the checkpoint does not advance",
            "the delivery remains replayable"
        ]
    );
    assert_eq!(contract.requirements, ["ORNA-ASSERT-047", "ORNA-CP-003"]);
    assert_eq!(
        contract.evidence_level,
        "implementation scenario, not executed by an Orna engine"
    );

    let (_temp, repository) = durable_repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let (outcome, rows_rolled_back, checkpoint, attempts, replayable_payload) = evaluator
        .execute_assert_checkpoint_091(
            &repository,
            RuntimeIdentity {
                database_id: [81; 16],
                repository_id: [82; 16],
            },
            [83; 16],
            [84; 32],
        )
        .await
        .expect("durable runtime adapter evidence");

    assert!(
        matches!(outcome, StageOutcome::Failed(ref diagnostic) if diagnostic.code() == "ORNA-LIST-STREAM-DELIVERY"),
        "the assertion-failing delivery must remain a runtime failure: {outcome:?}"
    );
    assert!(rows_rolled_back);
    assert_eq!(checkpoint.as_deref(), Some("1"));
    assert_eq!(attempts, 1);
    assert!(replayable_payload);
}

#[tokio::test]
async fn eval_003_replays_the_terminal_outcome_without_a_second_row() {
    let eval = scenario("EVAL-003");
    assert_eq!(
        eval.requirements,
        [
            "ORNA-EVAL-007",
            "ORNA-EVAL-008",
            "ORNA-EVAL-009",
            "ORNA-EVAL-010",
            "ORNA-EVAL-011"
        ]
    );
    assert_eq!(
        eval.evidence_level,
        "implementation scenario, not executed by an Orna engine"
    );

    let (_temp, repository) = durable_repository();
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let identity = RuntimeIdentity {
        database_id: [71; 16],
        repository_id: [72; 16],
    };
    let request = RequestIdentity {
        session_id: [73; 16],
        request_id: [74; 16],
    };
    let source = durable_source(
        "EVAL-003",
        "pub table Note(id: Int) { text: Str, } fn main() { Note.insert({ id: 7, text: \"once\" }); }",
    );
    let changed_source = durable_source(
        "EVAL-003",
        "pub table Note(id: Int) { text: Str, } fn main() { Note.insert({ id: 8, text: \"twice\" }); }",
    );
    let fingerprint = canonical_eval_fingerprint(&source, request, [71; 16]);
    let changed_fingerprint = canonical_eval_fingerprint(&changed_source, request, [71; 16]);
    let first = evaluator
        .execute_source_request(
            &repository,
            identity,
            [76; 16],
            [77; 32],
            request,
            fingerprint,
            &source,
        )
        .await
        .expect("first durable evaluation");
    assert!(matches!(first, StageOutcome::Passed));

    drop(evaluator);
    let evaluator = DurableTransactionalEvaluator::new("main", Limits::default());
    let replay = evaluator
        .execute_source_request(
            &repository,
            identity,
            [78; 16],
            [77; 32],
            request,
            fingerprint,
            &source,
        )
        .await
        .expect("matching replay");
    assert!(matches!(replay, StageOutcome::Passed));

    assert!(matches!(
        evaluator
            .execute_source_request(
                &repository,
                identity,
                [79; 16],
                [77; 32],
                request,
                changed_fingerprint,
                &changed_source,
            )
            .await,
        Err(RuntimeError::RequestFingerprintMismatch)
    ));

    let state = RuntimeState::open(&repository, identity, [77; 32])
        .await
        .expect("reopen durable runtime");
    let status = state
        .request_status(request, fingerprint)
        .await
        .expect("request status")
        .expect("completed request");
    assert_eq!(status.state, RequestState::Completed);
    assert!(status.terminal_outcome.is_some());
    let rows = state.committed_table_rows("Note").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, Value::int(7.into()).encode().unwrap());
    assert_ne!(rows[0].0, Value::int(8.into()).encode().unwrap());
    assert_eq!(state.pending().await.unwrap().len(), 1);
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
fn published_report_declares_bounded_runtime_adapter_scenarios_without_an_orna_engine_witness() {
    let output = run_conformance_with_timeout();
    assert!(
        !output.status.success(),
        "partial bounded conformance must fail its process gate"
    );
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
            "REPL-001",
            "TXN-001",
            "TXN-002",
            "CP-001",
            "LIVE-001",
            "LIVE-002",
            "LIVE-003",
            "LIVE-004",
            "SYS-RT-RENAME-100",
            "ASSERT-CHECKPOINT-091",
            "FAIL-001",
            "EVAL-003"
        ]
    );
    let scenarios = report["scenarios"]
        .as_array()
        .expect("scenario results are an array");
    assert_eq!(scenarios.len(), 144);
    for result in scenarios {
        if matches!(
            result["scenario"].as_str(),
            Some(
                "REPL-001"
                    | "TXN-001"
                    | "TXN-002"
                    | "CP-001"
                    | "LIVE-001"
                    | "LIVE-002"
                    | "LIVE-003"
                    | "LIVE-004"
                    | "SYS-RT-RENAME-100"
                    | "ASSERT-CHECKPOINT-091"
                    | "FAIL-001"
                    | "EVAL-003"
            )
        ) {
            assert_eq!(result["status"], "passed", "declared scenario must execute");
            assert_eq!(
                result["detail"],
                "scenario execution satisfied its adapter contract"
            );
        } else {
            assert_eq!(
                result["status"], "skipped",
                "{} must remain skipped",
                result["scenario"]
            );
            let expected = if matches!(
                result["scenario"].as_str(),
                Some("LET-REBIND-091" | "PIPE-001" | "PIPE-002")
            ) {
                "scenario execution skipped: no compiler-produced executable artifact crosses the semantic-to-runtime adapter; the bounded evaluator reinterprets source"
            } else if result["scenario"] == "CP-002" {
                "scenario execution skipped: the available bounded witness exercises assertion-validation rollback, not CP-002's immutable handler-inserts-then-errors path; no CP-002 pass is claimed"
            } else if result["scenario"] == "EVAL-001" {
                "scenario execution skipped: production remote Eval admits pure source but rejects table mutations, so it cannot satisfy ORNA-EVAL-003's required served-CWD activation transaction"
            } else {
                "scenario execution skipped: scenario lacks an authoritative compiler/runtime witness; direct bounded evaluator and table adapter coverage is not Orna-engine execution"
            };
            assert_eq!(result["detail"], expected);
        }
    }
    let live_keyed_update = scenarios
        .iter()
        .find(|result| result["scenario"] == "LIVE-001")
        .expect("LIVE-001 result is present");
    assert_eq!(
        live_keyed_update["requirements"],
        serde_json::json!(["ORNA-LIVE-001", "ORNA-LIVE-003"])
    );
    assert_eq!(live_keyed_update["status"], "passed");
    let live_unkeyed_update = scenarios
        .iter()
        .find(|result| result["scenario"] == "LIVE-002")
        .expect("LIVE-002 result is present");
    assert_eq!(
        live_unkeyed_update["requirements"],
        serde_json::json!(["ORNA-LIVE-002"])
    );
    assert_eq!(live_unkeyed_update["status"], "passed");
    let live_resync = scenarios
        .iter()
        .find(|result| result["scenario"] == "LIVE-003")
        .expect("LIVE-003 result is present");
    assert_eq!(
        live_resync["requirements"],
        serde_json::json!(["ORNA-LIVE-004"])
    );
    assert_eq!(live_resync["status"], "passed");
    let live_fallback = scenarios
        .iter()
        .find(|result| result["scenario"] == "LIVE-004")
        .expect("LIVE-004 result is present");
    assert_eq!(
        live_fallback["requirements"],
        serde_json::json!([
            "ORNA-LIVE-002",
            "ORNA-LIVE-004",
            "ORNA-WIRE-001",
            "ORNA-WIRE-002"
        ])
    );
    assert_eq!(live_fallback["status"], "passed");
    let runtime_root = scenarios
        .iter()
        .find(|result| result["scenario"] == "SYS-RT-RENAME-100")
        .expect("SYS-RT-RENAME-100 result is present");
    assert_eq!(
        runtime_root["requirements"],
        serde_json::json!(["ORNA-SYS-005", "ORNA-SYS-105"])
    );
    assert_eq!(runtime_root["status"], "passed");

    let durable_eval = scenarios
        .iter()
        .find(|result| result["scenario"] == "EVAL-003")
        .expect("EVAL-003 result is present");
    assert_eq!(
        durable_eval["requirements"],
        serde_json::json!([
            "ORNA-EVAL-007",
            "ORNA-EVAL-008",
            "ORNA-EVAL-009",
            "ORNA-EVAL-010",
            "ORNA-EVAL-011"
        ])
    );
    assert_eq!(durable_eval["status"], "passed");
    assert_eq!(
        report["implementation_claim"]["environment"]["runtime-stages"],
        "pure row/expression units, the authoritative duplicate-key fixture, SYS-RT-RENAME-100 system-name resolution, the LIVE-001 keyed update, LIVE-002 unkeyed fallback, LIVE-003 serving resynchronization, LIVE-004 universal subtree replacement, the CP-001 durable checkpoint atomicity contract, the ASSERT-CHECKPOINT-091 durable assertion/checkpoint rollback contract, the FAIL-001 stable failure-identity and attempts contract, and EVAL-003 durable request replay contracts execute through bounded runtime witnesses; CP-002's handler-failure retry contract remains explicitly skipped; these scenario results remain runtime-adapter evidence and are not compiler-produced or full Orna-engine execution"
    );
}

#[test]
fn remote_eval_contract_remains_skipped_without_an_authoritative_host_witness() {
    let output = run_conformance_with_timeout();
    assert!(
        !output.status.success(),
        "partial bounded conformance must fail its process gate"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("conformance report is JSON");
    let eval = report["scenarios"]
        .as_array()
        .expect("scenario results are an array")
        .iter()
        .find(|result| result["scenario"] == "EVAL-001")
        .expect("EVAL-001 result is present");

    assert_eq!(
        eval["requirements"],
        serde_json::json!(["ORNA-EVAL-001", "ORNA-EVAL-002", "ORNA-EVAL-003"])
    );
    assert_eq!(eval["status"], "skipped");
    assert_eq!(
        eval["detail"],
        "scenario execution skipped: production remote Eval admits pure source but rejects table mutations, so it cannot satisfy ORNA-EVAL-003's required served-CWD activation transaction"
    );
    assert!(
        !report["implementation_claim"]["executed_scenario_contracts"]
            .as_array()
            .expect("scenario execution claim is an array")
            .iter()
            .any(|scenario| scenario == "EVAL-001"),
        "remote Eval must not be claimed without a host execution witness"
    );
}
