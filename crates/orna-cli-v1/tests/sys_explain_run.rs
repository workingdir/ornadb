//! `orna run` evaluates `sys.explain` against the run activation's pinned
//! catalogue in a fresh `orna init` project.

use std::process::Command;

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;

#[path = "support/format3.rs"]
#[allow(dead_code)]
mod format3;

fn orna(directory: &std::path::Path, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("--db")
        .arg(directory)
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn row_field(row: &[u8], name: &str) -> OvbRaw {
    let value = Value::decode(row).expect("canonical stored row");
    let OvbRaw::Map(fields) = value.raw() else {
        panic!("stored row is not a record");
    };
    fields
        .iter()
        .find_map(|(key, value)| match key {
            OvbRaw::Text(key) if key == name => Some(value.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("stored row has no field {name}"))
}

fn text(row: &[u8], name: &str) -> String {
    match row_field(row, name) {
        OvbRaw::Text(text) => text,
        other => panic!("field {name} is not text: {other:?}"),
    }
}

/// ORNA-SYS-053, ORNA-SYS-054, ORNA-SYS-055, ORNA-PLAN-002: in a fresh
/// project, `orna run` explains `sys.Storage`, a limited query and a project
/// table as structured `sys.Plan` records pinned to the run snapshot, with no
/// actual counters, and a stage the planner cannot represent fails with the
/// portable `sys.version.incompatible` code (ORNA-SYS-008, ORNA-SYS-097).
#[tokio::test(flavor = "current_thread")]
async fn orna_run_explains_queries_against_the_activation_catalogue() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/sys-explain-run.orna"),
    )
    .expect("explain fixture");
    let init = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("init")
        .arg(directory.path())
        .output()
        .expect("CLI initialization");
    assert!(
        init.status.success(),
        "init stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let run = orna(directory.path(), &["run", "main.main"]);
    assert!(
        run.status.success(),
        "run stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"invocation completed\n");

    let repository = Repository::discover(directory.path()).expect("repository");
    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let mut plans = Vec::new();
    for id in 1..=3 {
        let key = Value::int(id.into()).encode().expect("integer key");
        let row = state
            .committed_table_row("PlanEvidence", &key)
            .await
            .expect("read plan evidence")
            .unwrap_or_else(|| panic!("plan evidence row {id} is committed"));
        assert_eq!(row_field(&row, "actual"), OvbRaw::Bool(false), "row {id}");
        let plan = text(&row, "plan");
        assert!(!plan.is_empty(), "row {id} carries a plan id");
        plans.push(plan);
    }
    assert_ne!(plans[0], plans[1], "a different query has a different plan");
    assert_ne!(plans[0], plans[2], "a table scan has its own plan");

    let key = Value::int(1.into()).encode().expect("integer key");
    let failure = state
        .committed_table_row("FailureEvidence", &key)
        .await
        .expect("read failure evidence")
        .expect("failure evidence row is committed");
    assert_eq!(text(&failure, "code"), "sys.version.incompatible");
}
