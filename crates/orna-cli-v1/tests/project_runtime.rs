use std::{
    fmt::Write,
    io::Write as _,
    process::{Command, Output, Stdio},
};

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::{Repository, inspect_metadata};
use orna_runtime_v1::{
    CheckpointKey, Component, ConsumerIdentity, Mutation, NoFault, RuntimeIdentity, RuntimeState,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn reference_project() -> TempDir {
    let directory = tempfile::tempdir().expect("reference project");
    for (name, source) in [
        ("main.orna", include_str!("fixtures/reference-project/main.orna")),
        ("library.orna", include_str!("fixtures/reference-project/library.orna")),
        ("warehouse.orna", include_str!("fixtures/reference-project/warehouse.orna")),
        ("sensors.orna", include_str!("fixtures/reference-project/sensors.orna")),
        ("values.orna", include_str!("fixtures/reference-project/values.orna")),
    ] {
        std::fs::write(directory.path().join(name), source).expect("reference source");
    }
    directory
}

fn invoke(directory: &std::path::Path, command: &str, argument: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.to_str().expect("UTF-8 path"),
            command,
            argument,
        ])
        .output()
        .expect("CLI process")
}

fn contains_ansi(output: &Output) -> bool {
    output.stdout.windows(2).any(|sequence| sequence == b"\x1b[")
        || output.stderr.windows(2).any(|sequence| sequence == b"\x1b[")
}

fn initialize_project(directory: &std::path::Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(output.status.success(), "init stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"initialized Orna repository\n");
    assert!(output.stderr.is_empty());
}

fn git_output_at(directory: &std::path::Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("Git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

fn commit_directory(directory: &std::path::Path, message: &str) {
    git_output_at(directory, &["config", "user.email", "kieran@drewett.dev"]);
    git_output_at(directory, &["config", "user.name", "kierandrewett"]);
    git_output_at(directory, &["config", "commit.gpgsign", "false"]);
    git_output_at(directory, &["add", "-A"]);
    git_output_at(directory, &["commit", "--quiet", "-m", message]);
}

fn function_fixture_project() -> TempDir {
    let directory = tempfile::tempdir().expect("function fixture project");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/function-expression.orna"),
    )
    .expect("reference Orna source");
    initialize_project(directory.path());
    directory
}

fn identity(directory: &std::path::Path) -> (RuntimeIdentity, [u8; 32]) {
    let repository = Repository::discover(directory).expect("repository");
    let metadata = inspect_metadata(&repository)
        .expect("metadata inspection")
        .expect("initialized metadata");
    let database_id = *metadata.database_id().as_bytes();
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        [
            database_id[0],
            database_id[1],
            database_id[2],
            database_id[3],
            database_id[4],
            database_id[5],
            database_id[6],
            database_id[7],
            database_id[8],
            database_id[9],
            database_id[10],
            database_id[11],
            database_id[12],
            database_id[13],
            database_id[14],
            database_id[15],
            repository_id[0],
            repository_id[1],
            repository_id[2],
            repository_id[3],
            repository_id[4],
            repository_id[5],
            repository_id[6],
            repository_id[7],
            repository_id[8],
            repository_id[9],
            repository_id[10],
            repository_id[11],
            repository_id[12],
            repository_id[13],
            repository_id[14],
            repository_id[15],
        ],
    )
}

fn field(row: &[u8], name: &str) -> Option<OvbRaw> {
    let value = Value::decode(row).expect("canonical stored row");
    let OvbRaw::Map(fields) = value.raw() else {
        panic!("stored reference row is not a record");
    };
    fields.iter().find_map(|(key, value)| match key {
        OvbRaw::Text(key) if key == name => Some(value.clone()),
        _ => None,
    })
}

fn text(row: &[u8], name: &str) -> String {
    match field(row, name).expect("row field") {
        OvbRaw::Text(value) => value,
        _ => panic!("row field is not text"),
    }
}

fn integer(row: &[u8], name: &str) -> i64 {
    match field(row, name).expect("row field") {
        OvbRaw::Int(value) => value.try_into().expect("bounded integer"),
        _ => panic!("row field is not integer"),
    }
}

fn decimal(coefficient: i64, exponent10: i64) -> OvbRaw {
    Value::decimal(coefficient.into(), exponent10.into())
        .expect("exact decimal")
        .raw()
        .clone()
}

fn checkpoint_component(value: impl Into<String>) -> Component {
    Component::new(value).expect("checkpoint component")
}

fn sensors_list_payloads() -> Vec<Vec<u8>> {
    [
        (0, decimal(1825, -2)),
        (1, decimal(1850, -2)),
        (2, decimal(1875, -2)),
    ]
    .into_iter()
    .map(|(sequence, value)| {
        let mut fields = vec![
            ("sensor", OvbRaw::Text("greenhouse".into())),
            ("sequence", OvbRaw::Int(sequence.into())),
            ("value", value),
        ];
        fields.sort_by_cached_key(|(name, _)| {
            Value::new(OvbRaw::Text((*name).into()))
                .expect("canonical field name")
                .encode()
                .expect("encoded field name")
        });
        Value::new(OvbRaw::Map(
            fields
                .into_iter()
                .map(|(name, value)| (OvbRaw::Text(name.into()), value))
                .collect(),
        ))
        .expect("canonical Sample payload")
        .encode()
        .expect("encoded Sample payload")
    })
    .collect()
}

fn list_stream_identity(name: &str, payloads: &[Vec<u8>]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ORNA-LIST-STREAM-IDENTITY\0");
    for payload in payloads {
        digest.update(
            u64::try_from(payload.len())
                .expect("payload length fits u64")
                .to_be_bytes(),
        );
        digest.update(payload);
    }
    format!("{name}:{:x}", digest.finalize())
}

fn sensors_checkpoint_key(database_id: [u8; 16]) -> CheckpointKey {
    let database = database_id.iter().fold(
        String::with_capacity(database_id.len() * 2),
        |mut database, byte| {
            write!(&mut database, "{byte:02x}").expect("write database identity");
            database
        },
    );
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: checkpoint_component(format!("database:{database}")),
            root: checkpoint_component("public-function"),
            function: checkpoint_component("sensors.ingest"),
            binding: checkpoint_component("arguments:[]"),
        },
        source_format: checkpoint_component("orna-stream-v1"),
        source: checkpoint_component(list_stream_identity(
            "example:sensors:v1",
            &sensors_list_payloads(),
        )),
        partition_format: checkpoint_component("literal-list"),
        partition: None,
        position_format: checkpoint_component("orna.list.v1"),
    }
}

/// ORNA-DIAG-002: `orna explain` returns extended diagnostic guidance through the CLI binary.
#[test]
fn binary_explain_returns_extended_orna_diagnostic_guidance() {
    let directory = tempfile::tempdir().expect("CLI working directory");
    let documented = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["explain", "ORNA-S010-IMPORT"])
        .output()
        .expect("CLI process");

    assert!(documented.status.success());
    assert_eq!(
        documented.stdout,
        b"ORNA-S010-IMPORT: imported module is unavailable\nThe project names a module that is not part of its captured dependencies, so the module cannot be checked or executed.\nhelp: use a captured standard dependency or remove the import\n"
    );
    assert!(documented.stderr.is_empty());

    let unknown = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["explain", "ORNA-NOT-A-CODE"])
        .output()
        .expect("CLI process");
    assert!(!unknown.status.success());
    assert_eq!(unknown.status.code(), Some(2), "unknown explain uses usage exit status");
    assert!(unknown.stdout.is_empty());
    assert_eq!(
        unknown.stderr,
        b"error[E1002]: diagnostic code is unknown\nhelp: supply a code printed by an Orna diagnostic, such as `ORNA-S010-IMPORT`\n"
    );
}

#[test]
fn binary_repl_executes_a_pure_expression_at_the_cli_boundary() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    let submission = include_str!("fixtures/repl-arithmetic.orna").trim_end();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl", submission])
        .output()
        .expect("CLI process");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"3 : Int\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_reference_repl_respects_color_mode() {
    let directory = function_fixture_project();

    let run_repl = |mode: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(directory.path())
            .args([
                "--color",
                mode,
                "--db",
                directory.path().to_str().expect("UTF-8 path"),
                "repl",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("CLI process");
        child
            .stdin
            .take()
            .expect("REPL stdin")
            .write_all(
                format!(
                    "{}\n:quit\n",
                    include_str!("fixtures/repl-arithmetic.orna").trim_end()
                )
                .as_bytes(),
            )
            .expect("REPL input");
        child.wait_with_output().expect("CLI process output")
    };

    let always = run_repl("always");
    assert!(always.status.success(), "REPL stderr: {:?}", always.stderr);
    assert!(contains_ansi(&always), "always must color captured REPL output");
    let rendered = String::from_utf8_lossy(&always.stdout);
    assert!(
        rendered.contains("3") && rendered.contains("Int"),
        "REPL arithmetic result: {rendered}"
    );
    assert!(always.stderr.is_empty());

    let never = run_repl("never");
    assert!(never.status.success(), "REPL stderr: {:?}", never.stderr);
    assert!(!contains_ansi(&never), "never must not color REPL output");

    let auto = run_repl("auto");
    assert!(auto.status.success(), "REPL stderr: {:?}", auto.stderr);
    assert!(!contains_ansi(&auto), "captured auto output must not be colored");
}

#[test]
fn binary_reference_status_respects_color_mode() {
    let directory = function_fixture_project();
    let status_path = directory.path().to_str().expect("UTF-8 path");
    let status = |mode: &str| {
        Command::new(env!("CARGO_BIN_EXE_orna"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(directory.path())
            .args(["--color", mode, "--db", status_path, "status"])
            .output()
            .expect("CLI human status")
    };

    let always = status("always");
    assert!(always.status.success(), "status stderr: {:?}", always.stderr);
    assert!(contains_ansi(&always), "always must color human status output");

    let never = status("never");
    assert!(never.status.success(), "status stderr: {:?}", never.stderr);
    assert!(!contains_ansi(&never), "never must not color human status");

    let auto = status("auto");
    assert!(auto.status.success(), "status stderr: {:?}", auto.stderr);
    assert!(!contains_ansi(&auto), "captured auto status must not be colored");
    assert_eq!(never.stdout, auto.stdout);
    assert_eq!(never.stderr, auto.stderr);

    let default = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["--db", status_path, "status"])
        .output()
        .expect("CLI human status with default color");
    assert!(default.status.success(), "status stderr: {:?}", default.stderr);
    assert!(
        !contains_ansi(&default),
        "captured default Auto output must not be colored"
    );
    assert_eq!(never.stdout, default.stdout);
    assert_eq!(never.stderr, default.stderr);
}

#[test]
fn binary_repl_recovers_from_malformed_terminal_input() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    let mut submissions = include_str!("fixtures/repl-retained-result.orna").lines();
    let first_submission = submissions.next().expect("first Orna submission");
    let retained_result_submission = submissions.next().expect("retained result submission");
    let mut input = format!("{first_submission}\n").into_bytes();
    input.extend_from_slice(b"\xff\n");
    input.extend_from_slice(format!("{retained_result_submission}\n:quit\n").as_bytes());
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(&input)
        .expect("REPL input");
    let output = child.wait_with_output().expect("CLI process output");

    assert!(output.status.success());
    assert_eq!(
        output.stdout, b"> 2 : Int\n> error[ORNA-REPL-INPUT-UTF8]: submission is not valid UTF-8 text\nhelp: re-enter the submission using valid UTF-8\n> 2 : Int\n> ",
        "malformed terminal input must not consume the retained last result"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_repl_rejects_remote_endpoints_before_local_evaluation() {
    let submission = include_str!("fixtures/repl-arithmetic.orna").trim_end();
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", "orna://host/reference", "repl", submission])
        .output()
        .expect("CLI process");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"error[E2100]: remote REPL sessions are unavailable\nhelp: use a local Git worktree until the Orna transport is available\n"
    );
}

#[test]
fn binary_check_accepts_a_core_only_project_without_std() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/project-core-main.orna").trim_end(),
    )
    .expect("project source");
    initialize_project(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "check",
        ])
        .output()
        .expect("CLI process");

    assert!(output.status.success(), "check stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"project valid\n");
    assert!(output.stderr.is_empty());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the process-level regression keeps all command outcomes and host-boundary observations together"
)]
/// ORNA-UX-001, ORNA-UX-002, ORNA-UX-004: the CLI diagnostic leads with an Orna-level problem and actionable remedy.
fn binary_check_run_and_invoke_reject_uncaptured_standard_import_without_host_substitution() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/project-uncaptured-std-main.orna").trim_end(),
    )
    .expect("project source");
    initialize_project(directory.path());

    let check = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "check",
        ])
        .output()
        .expect("CLI process");
    assert!(!check.status.success());
    assert!(check.stdout.is_empty());
    let expected = b"error[ORNA-S010-IMPORT]: imported module is unavailable\nhelp: use a captured standard dependency or remove the import\n";
    assert_eq!(check.stderr, expected);

    let run = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
        ])
        .output()
        .expect("CLI process");
    assert!(!run.status.success());
    assert!(run.stdout.is_empty());
    assert_eq!(run.stderr, expected);

    let invoke = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "invoke",
            "main",
        ])
        .output()
        .expect("CLI process");
    assert!(!invoke.status.success());
    assert!(invoke.stdout.is_empty());
    assert_eq!(invoke.stderr, expected);

    let stream = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "sensors.ingest",
        ])
        .output()
        .expect("CLI process");
    assert!(!stream.status.success());
    assert!(stream.stdout.is_empty());
    assert_eq!(stream.stderr, expected);

    let repl = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "repl",
        ])
        .output()
        .expect("CLI process");
    assert!(!repl.status.success());
    assert!(repl.stdout.is_empty());
    assert_eq!(
        repl.stderr,
        b"error[ORNA-S010-IMPORT]: imported module is unavailable\nhelp: use a captured standard dependency or remove the import\n"
    );

    let repl_directory = tempfile::tempdir().expect("standard-free REPL project directory");
    std::fs::write(
        repl_directory.path().join("main.orna"),
        include_str!("fixtures/project-core-main.orna").trim_end(),
    )
    .expect("standard-free project source");
    initialize_project(repl_directory.path());

    let mut unlisted_repl = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            repl_directory.path().to_str().expect("UTF-8 path"),
            "repl",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("unlisted CLI process");
    unlisted_repl
        .stdin
        .take()
        .expect("unlisted REPL stdin")
        .write_all(
            format!(
                "{}\n:quit\n",
                include_str!("fixtures/repl-unlisted-std-import.orna").trim_end()
            )
            .as_bytes(),
        )
        .expect("unlisted REPL input");
    let unlisted_repl = unlisted_repl
        .wait_with_output()
        .expect("unlisted REPL process output");
    assert!(unlisted_repl.status.success());
    assert_eq!(unlisted_repl.stdout, b"> error[ORNA-S010-IMPORT]: imported module is unavailable\n  --> line 1, column 1\n  |\n1 | use std.collection;\n  | ^^^^^^^^^^^^^^^^^^^\nhelp: use a captured standard dependency or remove the import\n> ");
    assert!(unlisted_repl.stderr.is_empty());
}

#[test]
fn binary_managed_local_repl_executes_integer_list_aggregates() {
    let directory = tempfile::tempdir().expect("REPL working directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/project-run-zero.orna").trim_end(),
    )
    .expect("project source");
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI process");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(
            format!(
                "{}\n:quit\n",
                include_str!("fixtures/repl-list-aggregates.orna").trim_end()
            )
            .as_bytes(),
        )
        .expect("REPL input");
    let output = child.wait_with_output().expect("CLI process output");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"> 6 : Int\n> 1 : Int\n> 3 : Int\n> 0 : Int\n> null : Null\n> null : Null\n> ",
        "managed-local REPL must preserve typed aggregate results and null extrema"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_run_executes_a_named_public_project_function() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("library.orna"),
        include_str!("fixtures/run/library.orna"),
    )
    .expect("library source");
    initialize_project(directory.path());

    let output = invoke(directory.path(), "run", "library.answer");

    assert!(output.status.success(), "run stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_run_evaluates_a_sys_enum_constant_in_a_project_function() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("library.orna"),
        include_str!("fixtures/run/sys-enum-library.orna"),
    )
    .expect("library source");
    initialize_project(directory.path());

    let output = invoke(directory.path(), "run", "library.answer");

    assert!(output.status.success(), "run stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_run_rejects_unequal_sys_enum_constants_in_a_project_function() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("library.orna"),
        include_str!("fixtures/run/sys-enum-mismatch-library.orna"),
    )
    .expect("library source");
    initialize_project(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "--debug",
            "run",
            "library.answer",
        ])
        .output()
        .expect("CLI process");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failure code: ORNA-EVAL-DIVIDE-BY-ZERO"),
        "stderr: {stderr}"
    );
}

#[test]
fn binary_run_reports_false_assertion_with_its_spec_code_without_debug() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("library.orna"),
        include_str!("fixtures/run/assert-false-library.orna"),
    )
    .expect("library source");
    initialize_project(directory.path());

    let output = invoke(directory.path(), "run", "library.answer");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[ORNA-A091-008]: assertion is false"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("E2200"), "stderr: {stderr}");
}

#[test]
fn binary_run_without_a_target_executes_root_main() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/private-main.orna"),
    )
    .expect("root source");
    initialize_project(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
        ])
        .output()
        .expect("CLI process");

    assert!(output.status.success(), "run stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn binary_run_reads_specified_storage_publication_metadata_from_runtime_state() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/publication_metadata.orna"),
    )
        .expect("publication metadata fixture");
    initialize_project(directory.path());

    let repository = Repository::discover(directory.path()).expect("repository");
    let check = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "check",
        ])
        .output()
        .expect("CLI check");
    assert!(
        check.status.success(),
        "check stderr: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime state");
    let context = state.begin_activation().await.expect("capture activation");
    let lease = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .expect("writer lease");
    let payload = b"fixture".to_vec();
    let mutation = Mutation {
        id: [10; 16],
        digest: Sha256::digest(&payload).into(),
        payload,
    };
    state
        .commit_activation(
            lease,
            &context,
            &[mutation],
            Sha256::digest(b"publication metadata CLI proof").into(),
            &NoFault,
        )
        .await
        .expect("seed durable pending mutation");

    // The seeding writer has exited before the CLI starts. A live in-process
    // runtime still holds the worktree owner lock, and ORNA-TURSO-004/005
    // require a concurrent `orna run` to fail rather than share that writer.
    drop(state);

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "main.main",
        ])
        .output()
        .expect("CLI process");

    assert!(
        output.status.success(),
        "run stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn binary_run_refuses_a_concurrent_writer_while_another_runtime_holds_the_lease() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/private-main.orna"),
    )
    .expect("root source");
    initialize_project(directory.path());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime state");
    let _lease = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .expect("writer lease");

    // ORNA-TURSO-004 permits at most one owning writer, and ORNA-TURSO-005
    // requires a second `orna run` to coordinate or fail while it is live.
    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
        ])
        .output()
        .expect("CLI process");
    assert!(
        !refused.status.success(),
        "concurrent run unexpectedly succeeded: {:?}",
        refused.stdout
    );
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("error[E2200]"),
        "stderr: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    drop(state);

    let released = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
        ])
        .output()
        .expect("CLI process");
    assert!(
        released.status.success(),
        "run stderr: {}",
        String::from_utf8_lossy(&released.stderr)
    );
    assert_eq!(released.stdout, b"invocation completed\n");
    assert!(released.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn binary_run_debug_detail_names_the_held_runtime_writer_and_plain_output_does_not() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/private-main.orna"),
    )
    .expect("root source");
    initialize_project(directory.path());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime state");
    let _lease = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .expect("writer lease");

    // The lease holder is this test process; the refused `orna run` processes
    // are separate. `RuntimeError::LeaseHeld` has no public code, so its
    // `--debug` detail carries the display text (ORNA-UX-004).
    let plain = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
        ])
        .output()
        .expect("CLI process");
    assert!(!plain.status.success(), "plain run unexpectedly succeeded");
    let plain_stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(
        plain_stderr.contains("error[E2200]"),
        "stderr: {plain_stderr}"
    );
    assert!(
        !plain_stderr.contains("runtime failure:"),
        "plain stderr disclosed a debug detail: {plain_stderr}"
    );

    let debug = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "--debug",
            "run",
        ])
        .output()
        .expect("CLI process");
    assert!(!debug.status.success(), "debug run unexpectedly succeeded");
    assert!(debug.stdout.is_empty());
    let debug_stderr = String::from_utf8_lossy(&debug.stderr);
    assert!(
        debug_stderr.contains("error[E2200]"),
        "stderr: {debug_stderr}"
    );
    assert!(
        debug_stderr.contains("runtime failure: runtime writer is held"),
        "stderr: {debug_stderr}"
    );

    drop(state);
}

#[tokio::test(flavor = "current_thread")]
async fn binary_invoke_routes_single_module_std_import_needing_publication_metadata_through_authority() {
    let (directory, _module_commit) = captured_standard_project_with(
        &[("math.orna", include_str!("fixtures/captured-standard-math.orna"))],
        &[(
            "main.orna",
            "use std.math.{increment};\npub fn answer(): Bool {\n    let rows = sys.Storage | map(storage => storage.pending_rows) | first() ?? -1;\n    assert rows == 1;\n    true\n}\n",
        )],
    );
    let repository = Repository::discover(directory.path()).expect("repository");
    let check = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "check",
        ])
        .output()
        .expect("CLI check");
    assert!(
        check.status.success(),
        "check stderr: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime state");
    let context = state.begin_activation().await.expect("capture activation");
    let lease = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .expect("writer lease");
    let payload = b"fixture".to_vec();
    let mutation = Mutation {
        id: [10; 16],
        digest: Sha256::digest(&payload).into(),
        payload,
    };
    state
        .commit_activation(
            lease,
            &context,
            &[mutation],
            Sha256::digest(b"std authority routing proof").into(),
            &NoFault,
        )
        .await
        .expect("seed durable pending mutation");

    let output = invoke(directory.path(), "invoke", "answer");

    assert!(output.status.success(), "invoke stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn binary_invoke_executes_std_function_calls_through_authority_route() {
    let (directory, _module_commit) = captured_standard_project_with(
        &[(
            "math.orna",
            include_str!("fixtures/captured-standard-math.orna"),
        )],
        &[(
            "main.orna",
            "use std.math.{increment};\npub fn answer(): Bool {\n    let rows = sys.Storage | map(storage => storage.pending_rows) | first() ?? -1;\n    assert rows == 1;\n    assert increment(1) == 2;\n    true\n}\n",
        )],
    );
    let repository = Repository::discover(directory.path()).expect("repository");
    let check = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "check",
        ])
        .output()
        .expect("CLI check");
    assert!(
        check.status.success(),
        "check stderr: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime state");
    let context = state.begin_activation().await.expect("capture activation");
    let lease = state
        .acquire_lease(runtime_identity.repository_id)
        .await
        .expect("writer lease");
    let payload = b"fixture".to_vec();
    let mutation = Mutation {
        id: [11; 16],
        digest: Sha256::digest(&payload).into(),
        payload,
    };
    state
        .commit_activation(
            lease,
            &context,
            &[mutation],
            Sha256::digest(b"std function call authority routing proof").into(),
            &NoFault,
        )
        .await
        .expect("seed durable pending mutation");

    let output = invoke(directory.path(), "invoke", "answer");

    assert!(
        output.status.success(),
        "invoke stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_run_without_a_target_reports_a_missing_root_main() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/no-main.orna"),
    )
    .expect("root source");
    initialize_project(directory.path());

    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
        ])
        .output()
        .expect("CLI process");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"error[E2103]: no root main(): main.orna declares no `main()` function\nhelp: declare `main()` in main.orna, or run a qualified function such as `orna run MODULE.FUNCTION`\n"
    );
}

#[test]
/// ORNA-RUN-002: bare `orna run` without a root `main()` diagnoses the missing entry in a committed project.
fn binary_run_without_a_target_reports_a_missing_root_main_in_a_committed_project() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/no-main.orna"),
    )
    .expect("root source");
    initialize_project(directory.path());
    commit_orna(directory.path(), "snapshot without main");

    let output = run_bare(directory.path());

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"error[E2103]: no root main(): main.orna declares no `main()` function\nhelp: declare `main()` in main.orna, or run a qualified function such as `orna run MODULE.FUNCTION`\n"
    );
}

#[test]
/// ORNA-RUN-002: bare `orna run` invokes the root `main()` of a committed project with tables.
fn binary_run_without_a_target_invokes_the_root_main_of_a_committed_project_with_tables() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub table Sample(id: Str) {\n    label: Str,\n}\npub fn main(): Int = 42;\n",
    )
    .expect("root source");
    initialize_project(directory.path());
    commit_orna(directory.path(), "snapshot with table and main");

    let output = run_bare(directory.path());

    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
/// ORNA-RUN-003: a qualified function in `ingest/main.orna` runs exactly like the same function in another file.
fn binary_run_qualified_function_in_an_ingest_module_matches_another_module() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::create_dir(directory.path().join("ingest")).expect("ingest directory");
    std::fs::write(
        directory.path().join("ingest/main.orna"),
        "pub fn answer(): Int = 42;\n",
    )
    .expect("ingest source");
    std::fs::write(
        directory.path().join("other.orna"),
        "pub fn answer(): Int = 42;\n",
    )
    .expect("other source");
    std::fs::write(
        directory.path().join("main.orna"),
        "use ingest;\nuse other;\npub fn main(): Int = other.answer();\n",
    )
    .expect("root source");
    initialize_project(directory.path());
    commit_orna(directory.path(), "snapshot with two modules");

    let ingest = invoke(directory.path(), "run", "ingest.answer");
    let other = invoke(directory.path(), "run", "other.answer");

    assert!(ingest.status.success(), "stderr: {:?}", ingest.stderr);
    assert!(other.status.success(), "stderr: {:?}", other.stderr);
    assert_eq!(ingest.stdout, other.stdout);
    assert_eq!(ingest.stderr, other.stderr);
}

fn run_bare(directory: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "run"])
        .output()
        .expect("CLI process")
}

fn commit_orna(directory: &std::path::Path, message: &str) {
    git_output_at(directory, &["config", "user.email", "kieran@drewett.dev"]);
    git_output_at(directory, &["config", "user.name", "kierandrewett"]);
    git_output_at(directory, &["config", "commit.gpgsign", "false"]);
    git_output_at(directory, &["add", "-A"]);
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.to_str().expect("UTF-8 path"),
            "commit",
            "-m",
            message,
        ])
        .output()
        .expect("CLI commit");
    assert!(output.status.success(), "commit stderr: {:?}", output.stderr);
}

#[test]
fn binary_run_rejects_private_function_targets_before_runtime_admission() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("library.orna"),
        include_str!("fixtures/run/library.orna"),
    )
    .expect("library source");
    initialize_project(directory.path());

    let output = invoke(directory.path(), "run", "main.hidden");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"error[E2000]: durable project invocation is not available\nhelp: use a supported table transaction; stream roots require the explicit stream runtime\n"
    );
}

#[test]
fn binary_run_treats_a_path_like_target_as_an_ordinary_function_name() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::create_dir(directory.path().join("ingest")).expect("ingest directory");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("root source");
    std::fs::write(
        directory.path().join("ingest/main.orna"),
        include_str!("fixtures/run/main.orna"),
    )
    .expect("nested source");
    std::fs::write(
        directory.path().join("library.orna"),
        include_str!("fixtures/run/library.orna"),
    )
    .expect("library source");
    initialize_project(directory.path());

    let output = invoke(directory.path(), "run", "ingest/main.orna");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"error[E2000]: durable project invocation is not available\nhelp: use a supported table transaction; stream roots require the explicit stream runtime\n"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the process-level regression retains the complete byte-for-byte status boundary evidence"
)]
/// ORNA-CLI-001, ORNA-CLI-003: status's Git-compatible porcelain is exercised through the CLI binary.
fn binary_status_porcelain_preserves_git_worktree_bytes_and_hides_discovery_paths() {
    let repository = function_fixture_project();
    let source_path = repository.path().join("main.orna");
    assert!(
        Command::new("git")
            .args(["add", "main.orna"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(repository.path())
            .status()
            .expect("stage initial fixture")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "initial fixture",
            ])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(repository.path())
            .status()
            .expect("commit initial fixture")
            .success()
    );

    let original_source = include_str!("fixtures/function-expression.orna");
    let staged_source = include_str!("fixtures/function-expression-staged.orna");
    assert_ne!(
        staged_source, original_source,
        "fixture expression must match"
    );
    std::fs::write(&source_path, &staged_source).expect("staged fixture change");
    std::fs::write(repository.path().join("staged.orna"), &original_source)
        .expect("staged Orna source");
    assert!(
        Command::new("git")
            .args(["add", "main.orna", "staged.orna"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(repository.path())
            .status()
            .expect("stage fixture changes")
            .success()
    );
    let modified_source = include_str!("fixtures/function-expression-unstaged.orna");
    assert_ne!(
        modified_source, staged_source,
        "staged expression must match"
    );
    std::fs::write(&source_path, modified_source).expect("unstaged fixture change");
    std::fs::write(repository.path().join("untracked.orna"), &original_source)
        .expect("untracked Orna source");
    std::fs::create_dir(repository.path().join("nested")).expect("untracked directory");
    std::fs::write(
        repository.path().join("nested/untracked.orna"),
        &original_source,
    )
    .expect("nested untracked Orna source");

    let expected = Command::new("git")
        .args(["status", "--porcelain"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(repository.path())
        .output()
        .expect("git status");
    assert!(expected.status.success());
    let expected_text = std::str::from_utf8(&expected.stdout).expect("UTF-8 porcelain");
    assert!(expected_text.contains("MM main.orna"));
    assert!(expected_text.contains("A  staged.orna"));
    assert!(expected_text.contains("?? untracked.orna"));
    assert!(expected_text.contains("?? nested/"));
    let actual = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(repository.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("CLI status");
    assert!(actual.status.success());
    assert_eq!(actual.stdout, expected.stdout);
    assert!(actual.stderr.is_empty());
    let status_path = repository.path().to_str().expect("UTF-8 path");
    let porcelain_with_color = |mode: &str| {
        Command::new(env!("CARGO_BIN_EXE_orna"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(repository.path())
            .args([
                "--color",
                mode,
                "--db",
                status_path,
                "status",
                "--porcelain",
            ])
            .output()
            .expect("CLI colored status")
    };
    for (mode, output) in [
        ("always", porcelain_with_color("always")),
        ("never", porcelain_with_color("never")),
        ("auto", porcelain_with_color("auto")),
    ] {
        assert!(
            output.status.success(),
            "status --color {mode} stderr: {:?}",
            output.stderr
        );
        assert_eq!(
            output.stdout, expected.stdout,
            "status --color {mode} must preserve Git bytes"
        );
        assert!(
            output.stderr.is_empty(),
            "status --color {mode} stderr: {:?}",
            output.stderr
        );
        assert!(
            !contains_ansi(&output),
            "status --color {mode} must not emit ANSI"
        );
    }

    let expected_short = Command::new("git")
        .args(["status", "--short"])
        .current_dir(repository.path().join("nested"))
        .output()
        .expect("git short status");
    let mut expected_short_with_runtime_summary = expected_short.stdout.clone();
    expected_short_with_runtime_summary.extend_from_slice(b"RM 0 runtime mutations\n");
    let actual_short = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(repository.path().join("nested"))
        .args(["status", "--short"])
        .output()
        .expect("CLI short status");
    assert!(actual_short.status.success());
    assert_eq!(actual_short.stdout, expected_short_with_runtime_summary);
    assert!(actual_short.stderr.is_empty());

    let actual_explicit_short = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(repository.path().join("nested"))
        .args([
            "--db",
            repository.path().to_str().expect("UTF-8 path"),
            "status",
            "--short",
        ])
        .output()
        .expect("CLI explicit short status");
    assert!(actual_explicit_short.status.success());
    assert_eq!(
        actual_explicit_short.stdout,
        expected_short_with_runtime_summary
    );
    assert!(actual_explicit_short.stderr.is_empty());

    let outside = tempfile::tempdir().expect("non-repository directory");
    let failure = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(outside.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("CLI status failure");
    assert!(!failure.status.success());
    assert_eq!(
        failure.stderr,
        b"error[E2100]: local Git worktree could not be discovered\nhelp: run the command inside a Git worktree or provide a local project path\n"
    );
    assert!(failure.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&failure.stderr).contains("status repository"));

    let short_failure = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(outside.path())
        .args(["status", "--short"])
        .output()
        .expect("CLI short status failure");
    assert!(!short_failure.status.success());
    assert_eq!(short_failure.stderr, failure.stderr);
    assert!(short_failure.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&short_failure.stderr).contains("status repository"));
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "the durable workflow scenario must retain each ordered process observation and recovery boundary"
)]
async fn binary_reference_workflow_reopens_durable_rows_and_preserves_duplicate_failure() {
    let directory = reference_project();
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());
    assert_eq!(init.stdout, b"initialized Orna repository\n");
    assert!(init.stderr.is_empty());

    let first = invoke(directory.path(), "run", "seed");
    assert!(first.status.success(), "seed stderr: {:?}", first.stderr);
    assert_eq!(first.stdout, b"invocation completed\n");
    assert!(first.stderr.is_empty());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after seed");
    let generation_after_seed = state
        .capture()
        .await
        .expect("seed capture")
        .generation()
        .clone();
    assert_eq!(state.committed_table_rows("library.Book").await.unwrap().len(), 2);
    assert_eq!(state.committed_table_rows("library.Loan").await.unwrap().len(), 0);
    let stock = state.committed_table_rows("warehouse.Stock").await.unwrap();
    assert_eq!(stock.len(), 2);
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "north"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 12
    }));
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "south"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 4
    }));

    // The previous writer has exited; release its owner lock so this run is a
    // fresh process taking over a departed lease (ORNA-TASK-003).
    drop(state);

    let duplicate = invoke(directory.path(), "run", "seed");
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("error[E2200]"));
    assert!(
        !String::from_utf8_lossy(&duplicate.stderr)
            .contains(directory.path().to_string_lossy().as_ref())
    );
    let duplicate_state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after duplicate");
    assert_eq!(
        duplicate_state
            .capture()
            .await
            .expect("duplicate capture")
            .generation(),
        &generation_after_seed
    );
    assert_eq!(
        duplicate_state
            .committed_table_rows("library.Book")
            .await
            .unwrap()
            .len(),
        2
    );

    // Release the duplicate-check writer so the next run is a fresh process.
    drop(duplicate_state);

    let exercise = invoke(directory.path(), "run", "exercise");
    assert!(
        exercise.status.success(),
        "exercise stderr: {:?}",
        exercise.stderr
    );
    assert_eq!(exercise.stdout, b"invocation completed\n");
    assert!(exercise.stderr.is_empty());
    let final_state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after exercise");
    assert!(final_state.capture().await.unwrap().generation() > &generation_after_seed);
    let loans = final_state.committed_table_rows("library.Loan").await.unwrap();
    assert_eq!(loans.len(), 1);
    assert_eq!(text(&loans[0].1, "book_id"), "book-1");
    assert_eq!(text(&loans[0].1, "borrower"), "reader-1");
    let stock = final_state.committed_table_rows("warehouse.Stock").await.unwrap();
    assert_eq!(stock.len(), 2);
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "north"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 9
    }));
    assert!(stock.iter().any(|(_, row)| {
        text(row, "location") == "south"
            && text(row, "sku") == "pencil"
            && integer(row, "quantity") == 7
    }));

    let generation_after_exercise = final_state
        .capture()
        .await
        .expect("exercise capture")
        .generation()
        .clone();
    let books_after_exercise = final_state.committed_table_rows("library.Book").await.unwrap();
    let loans_after_exercise = final_state.committed_table_rows("library.Loan").await.unwrap();
    let stock_after_exercise = final_state.committed_table_rows("warehouse.Stock").await.unwrap();
    drop(final_state);

    let duplicate_exercise = invoke(directory.path(), "run", "exercise");
    assert!(!duplicate_exercise.status.success());
    let stderr = String::from_utf8_lossy(&duplicate_exercise.stderr);
    assert!(stderr.contains("error[E2200]"));
    assert!(stderr.contains("failed atomically"));
    assert!(!stderr.contains(directory.path().to_string_lossy().as_ref()));

    let after_rejected_exercise = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after rejected duplicate exercise");
    assert_eq!(
        after_rejected_exercise
            .capture()
            .await
            .expect("duplicate exercise capture")
            .generation(),
        &generation_after_exercise
    );
    assert_eq!(
        after_rejected_exercise
            .committed_table_rows("library.Book")
            .await
            .unwrap(),
        books_after_exercise
    );
    assert_eq!(
        after_rejected_exercise
            .committed_table_rows("library.Loan")
            .await
            .unwrap(),
        loans_after_exercise
    );
    assert_eq!(
        after_rejected_exercise
            .committed_table_rows("warehouse.Stock")
            .await
            .unwrap(),
        stock_after_exercise
    );
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "the durable mutation scenario keeps invalid and duplicate publication observations adjacent"
)]
async fn binary_reference_library_lend_rejects_invalid_and_duplicate_rows_without_publication() {
    let directory = reference_project();
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());

    let seed = invoke(directory.path(), "run", "seed");
    assert!(seed.status.success(), "seed stderr: {:?}", seed.stderr);

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let seeded = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after seed");
    let generation_after_seed = seeded
        .capture()
        .await
        .expect("seed capture")
        .generation()
        .clone();
    let books_after_seed = seeded.committed_table_rows("library.Book").await.unwrap();
    drop(seeded);

    let missing = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "library.lend",
            "missing-book",
            "reader-2",
        ])
        .output()
        .expect("CLI process");
    assert!(!missing.status.success());
    let missing_stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(missing_stderr.contains("error[E2200]"));
    assert!(missing_stderr.contains("failed atomically"));
    assert!(!missing_stderr.contains("missing-book"));
    assert!(!missing_stderr.contains("reader-2"));
    assert!(!missing_stderr.contains(directory.path().to_string_lossy().as_ref()));

    let after_missing = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after missing-book loan");
    assert_eq!(
        after_missing
            .capture()
            .await
            .expect("missing capture")
            .generation(),
        &generation_after_seed
    );
    assert_eq!(
        after_missing.committed_table_rows("library.Book").await.unwrap(),
        books_after_seed
    );
    assert!(
        after_missing
            .committed_table_rows("library.Loan")
            .await
            .unwrap()
            .is_empty()
    );
    drop(after_missing);

    let first_lend = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "library.lend",
            "book-1",
            "reader-1",
        ])
        .output()
        .expect("CLI process");
    assert!(
        first_lend.status.success(),
        "lend stderr: {:?}",
        first_lend.stderr
    );

    let lent = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after first loan");
    let generation_after_lend = lent
        .capture()
        .await
        .expect("lend capture")
        .generation()
        .clone();
    let loans_after_lend = lent.committed_table_rows("library.Loan").await.unwrap();
    assert_eq!(loans_after_lend.len(), 1);
    assert_eq!(text(&loans_after_lend[0].1, "book_id"), "book-1");
    assert_eq!(text(&loans_after_lend[0].1, "borrower"), "reader-1");
    drop(lent);

    let duplicate = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "run",
            "library.lend",
            "book-1",
            "reader-2",
        ])
        .output()
        .expect("CLI process");
    assert!(!duplicate.status.success());
    let duplicate_stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(duplicate_stderr.contains("error[E2200]"));
    assert!(duplicate_stderr.contains("failed atomically"));
    assert!(!duplicate_stderr.contains("book-1"));
    assert!(!duplicate_stderr.contains("reader-2"));
    assert!(!duplicate_stderr.contains(directory.path().to_string_lossy().as_ref()));

    let after_duplicate = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after duplicate loan");
    assert_eq!(
        after_duplicate
            .capture()
            .await
            .expect("duplicate capture")
            .generation(),
        &generation_after_lend
    );
    assert_eq!(
        after_duplicate.committed_table_rows("library.Loan").await.unwrap(),
        loans_after_lend
    );
}

#[tokio::test(flavor = "current_thread")]
async fn binary_generic_run_executes_a_renamed_finite_list_stream_root() {
    let directory = reference_project();
    let sensors_path = directory.path().join("sensors.orna");
    std::fs::write(&sensors_path, include_str!("fixtures/sensors-deliver.orna"))
        .expect("rename stream root");

    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());
    assert_eq!(init.stdout, b"initialized Orna repository\n");
    assert!(init.stderr.is_empty());

    let output = invoke(directory.path(), "run", "sensors.deliver");
    assert!(
        output.status.success(),
        "generic stream invocation failed: {:?}",
        output.stderr
    );
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after generic stream invocation");
    assert_eq!(
        state
            .committed_table_rows("sensors.Reading")
            .await
            .expect("read durable readings")
            .len(),
        3
    );
}

#[tokio::test(flavor = "current_thread")]
async fn core_transactions_and_assertions_work_without_std() {
    let directory = tempfile::tempdir().expect("core-only project");
    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/lib002-core-only.orna"),
    )
    .expect("core-only source");
    initialize_project(directory.path());

    let repository = Repository::discover(directory.path()).expect("repository");
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .expect("load without a standard module");
    assert!(!project.has_standard_imports());
    assert!(project.standard_profile().is_none());

    let seed = invoke(directory.path(), "run", "main.seed");
    assert!(seed.status.success(), "seed stderr: {:?}", seed.stderr);
    assert_eq!(seed.stdout, b"invocation completed\n");
    assert!(seed.stderr.is_empty());

    let violation = invoke(directory.path(), "run", "main.violate");
    assert!(!violation.status.success());
    assert!(violation.stdout.is_empty());

    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open core-only runtime");
    let committed = state
        .committed_table_rows("Note")
        .await
        .expect("read committed row");
    assert_eq!(committed.len(), 1);
}

fn captured_standard_project() -> (tempfile::TempDir, String) {
    captured_standard_project_with(
        &[("math.orna", include_str!("fixtures/captured-standard-math.orna"))],
        &[("main.orna", include_str!("fixtures/captured-standard-parent.orna"))],
    )
}

/// Pins `stdlib/std` to a snapshot holding the standard root plus `standard`, and
/// stages the project root holding `project` beside the pinned gitlink.
fn staged_standard_project_with(
    standard: &[(&str, &str)],
    project: &[(&str, &str)],
) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().expect("project");
    let module_path = directory.path().join("stdlib/std");
    std::fs::create_dir_all(&module_path).expect("standard module path");
    initialize_project(&module_path);
    std::fs::write(
        module_path.join("main.orna"),
        include_str!("fixtures/captured-standard-main.orna"),
    )
    .expect("standard database root");
    for &(name, source) in standard {
        std::fs::write(module_path.join(name), source).expect("standard source module");
    }
    commit_directory(&module_path, "standard module snapshot");
    let module_commit = git_output_at(&module_path, &["rev-parse", "HEAD"]);

    for &(name, source) in project {
        std::fs::write(directory.path().join(name), source).expect("importing project source");
    }
    std::fs::write(
        directory.path().join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://github.com/workingdir/ornadb-std.git\n",
    )
    .expect("pinned module declaration");
    initialize_project(directory.path());
    git_output_at(directory.path(), &["config", "user.email", "kieran@drewett.dev"]);
    git_output_at(directory.path(), &["config", "user.name", "kierandrewett"]);
    let mut arguments = vec!["add", ".gitmodules", ".orna"];
    arguments.extend(project.iter().map(|&(name, _)| name));
    git_output_at(directory.path(), &arguments);
    let link = format!("160000,{module_commit},stdlib/std");
    git_output_at(
        directory.path(),
        &["update-index", "--add", "--cacheinfo", &link],
    );
    (directory, module_commit)
}

/// Publishes the snapshot with a raw git commit. `orna commit` would refuse fixture-only table names,
/// so the run-time refusal stays the guard under test.
fn captured_standard_project_with(
    standard: &[(&str, &str)],
    project: &[(&str, &str)],
) -> (tempfile::TempDir, String) {
    let (directory, module_commit) = staged_standard_project_with(standard, project);
    git_output_at(directory.path(), &["commit", "--quiet", "-m", "capture std snapshot"]);
    (directory, module_commit)
}

/// Publishes the snapshot through `orna commit`, which writes the table identity inventory.
fn committed_standard_project_with(
    standard: &[(&str, &str)],
    project: &[(&str, &str)],
) -> (tempfile::TempDir, String) {
    let (directory, module_commit) = staged_standard_project_with(standard, project);
    commit_orna(directory.path(), "capture std snapshot");
    (directory, module_commit)
}

/// The pinned `std.list` closure: `list` imports `collection`, and `math` is the standard root's dependency.
fn pinned_list_closure() -> [(&'static str, &'static str); 3] {
    [
        ("collection.orna", include_str!("fixtures/captured-standard-collection.orna")),
        ("list.orna", include_str!("fixtures/captured-standard-list.orna")),
        ("math.orna", include_str!("fixtures/captured-standard-math.orna")),
    ]
}

#[test]
fn binary_checks_and_runs_an_import_from_the_pinned_standard_git_module() {
    let (directory, module_commit) = captured_standard_project();

    let repository = Repository::discover(directory.path()).expect("project repository");
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .expect("load pinned module");
    assert_eq!(project.standard_profile().unwrap().snapshot(), module_commit);
    assert!(
        project
            .standard_runtime_modules()
            .contains("std/math.orna")
    );
    orna_semantic_v1::Catalogue::authoritative_core()
        .with_standard_sources(
            project.standard_profile().unwrap(),
            project.standard_sources().iter().cloned(),
        )
        .expect("build captured standard catalogue");

    let check = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["check"])
        .output()
        .expect("CLI check");
    assert!(check.status.success(), "check stderr: {:?}", check.stderr);
    assert_eq!(check.stdout, b"project valid\n");
    assert!(check.stderr.is_empty());

    let run = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["--debug", "run", "main.answer"])
        .output()
        .expect("CLI invocation");
    assert!(run.status.success(), "run stderr: {:?}", run.stderr);
    assert_eq!(run.stdout, b"invocation completed\n");
    assert!(run.stderr.is_empty());
}

#[test]
fn binary_check_resolves_a_pinned_standard_collection_closure_through_the_production_catalogue() {
    let (directory, _module_commit) = captured_standard_project_with(
        &pinned_list_closure(),
        &[
            ("main.orna", include_str!("fixtures/captured-standard-multi-main.orna")),
            ("tiny.orna", include_str!("fixtures/captured-standard-multi-tiny.orna")),
        ],
    );

    let repository = Repository::discover(directory.path()).expect("project repository");
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .expect("load pinned standard closure");
    let catalogue = project
        .production_catalogue()
        .expect("production catalogue from the pinned standard profile");
    let analysis = orna_semantic_v1::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic_v1::SystemProviders::Production,
    );
    assert!(analysis.is_ok(), "analysis through production catalogue failed");
    let core_only = orna_semantic_v1::analyze_with_providers(
        project.modules(),
        &orna_semantic_v1::Catalogue::authoritative_core(),
        orna_semantic_v1::SystemProviders::Production,
    );
    assert!(
        !core_only.is_ok(),
        "core-only catalogue must not resolve the pinned standard closure"
    );

    let check = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .args(["check"])
        .output()
        .expect("CLI check");
    assert!(check.status.success(), "check stderr: {:?}", check.stderr);
    assert_eq!(check.stdout, b"project valid\n");
    assert!(check.stderr.is_empty());
}

#[test]
/// A run admits a pinned standard import in a project that also declares a table with an assertion. `answer` writes no rows, so the assertion is not evaluated on this path; commit-time evaluation is pinned separately.
fn binary_run_admits_a_standard_import_alongside_a_table_assertion() {
    let (directory, _module_commit) = committed_standard_project_with(
        &[(
            "math.orna",
            include_str!("fixtures/captured-standard-math.orna"),
        )],
        &[(
            "main.orna",
            "use std.math.{increment};\npub table Sample(id: Str) {\n    label: Str,\n    assert every(row => row.label != \"\");\n}\npub fn answer(): Int = increment(41);\n",
        )],
    );

    let output = invoke(directory.path(), "run", "main.answer");

    assert!(output.status.success(), "run stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
/// A table assertion naming a table the project does not declare (`Contact` exists only in a conformance fixture) is refused with a compile diagnostic; no run is admitted.
fn binary_run_refuses_an_assertion_naming_an_undeclared_table() {
    let (directory, _module_commit) = captured_standard_project_with(
        &[(
            "math.orna",
            include_str!("fixtures/captured-standard-math.orna"),
        )],
        &[(
            "main.orna",
            "use std.math.{increment};\npub table Sample(id: Str) {\n    label: Str,\n    assert every(row => exists(Contact, contact => contact.name == row.label));\n}\npub fn answer(): Int = increment(41);\n",
        )],
    );

    let output = invoke(directory.path(), "run", "main.answer");

    assert!(
        !output.status.success(),
        "fixture-only table name was admitted"
    );
    assert!(output.stdout.is_empty());
    let stderr = std::str::from_utf8(&output.stderr).expect("UTF-8 diagnostic");
    assert!(stderr.starts_with("error["), "{stderr}");
}

#[tokio::test(flavor = "current_thread")]
/// A committed row that violates a table assertion is refused on the run path. The run exits non-zero with empty stdout and
/// a diagnostic on stderr, the committed rows stay byte-identical, and a valid row is still accepted. The run path does not
/// yet name the violated assertion or a deterministic safe witness (ornadb-a3jt), so this test asserts neither that text nor
/// the internal code. The project imports a pinned standard module, so admission runs through the production catalogue.
async fn binary_run_refuses_a_committed_row_violating_a_table_assertion_and_keeps_rows_unchanged() {
    let (directory, _module_commit) = committed_standard_project_with(
        &[(
            "math.orna",
            include_str!("fixtures/captured-standard-math.orna"),
        )],
        &[(
            "main.orna",
            concat!(
                "use std.math.{increment};\n",
                "pub table Sample(id: Int) {\n",
                "    label: Str,\n",
                "    assert every(row => row.label != \"\");\n",
                "}\n",
                "fn insert_sample(id: Int, label: Str): Bool {\n",
                "    Sample.insert({ id: id, label: label });\n",
                "    true\n",
                "}\n",
                "pub fn seed_valid(): Bool = insert_sample(increment(0), \"present\");\n",
                "pub fn seed_violation(): Bool = insert_sample(increment(1), \"\");\n",
                "pub fn seed_more(): Bool = insert_sample(increment(2), \"present\");\n",
            ),
        )],
    );
    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());

    let accepted = invoke(directory.path(), "run", "main.seed_valid");
    assert!(
        accepted.status.success(),
        "valid run stderr: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    assert_eq!(accepted.stdout, b"invocation completed\n");
    assert!(accepted.stderr.is_empty());

    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime after valid row");
    let rows_before = state
        .committed_table_rows("Sample")
        .await
        .expect("read committed rows");
    drop(state);
    assert_eq!(rows_before.len(), 1);

    let refused = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.path().to_str().expect("UTF-8 path"),
            "--debug",
            "run",
            "main.seed_violation",
        ])
        .output()
        .expect("CLI process");
    assert!(!refused.status.success(), "violating row was admitted");
    assert!(refused.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.starts_with("error["), "stderr: {stderr}");

    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime after refused row");
    let rows_after = state
        .committed_table_rows("Sample")
        .await
        .expect("read committed rows");
    drop(state);
    assert_eq!(rows_after, rows_before);

    let accepted_again = invoke(directory.path(), "run", "main.seed_more");
    assert!(
        accepted_again.status.success(),
        "second valid run stderr: {}",
        String::from_utf8_lossy(&accepted_again.stderr)
    );
    assert_eq!(accepted_again.stdout, b"invocation completed\n");
    assert!(accepted_again.stderr.is_empty());

    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("open runtime after second valid row");
    assert_eq!(
        state
            .committed_table_rows("Sample")
            .await
            .expect("read committed rows")
            .len(),
        2
    );
}

#[test]
fn binary_invoke_runs_a_single_module_import_from_the_pinned_standard_module() {
    let (directory, _module_commit) = captured_standard_project();

    let output = invoke(directory.path(), "invoke", "answer");

    assert!(output.status.success(), "invoke stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_invoke_evaluates_a_multi_module_project_importing_the_pinned_standard_collection() {
    let (directory, _module_commit) = captured_standard_project_with(
        &pinned_list_closure(),
        &[
            ("main.orna", include_str!("fixtures/captured-standard-multi-main.orna")),
            ("tiny.orna", include_str!("fixtures/captured-standard-multi-tiny.orna")),
        ],
    );

    let output = invoke(directory.path(), "invoke", "seed");

    assert!(output.status.success(), "invoke stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"invocation completed\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn binary_invoke_rejects_a_negative_take_count_from_the_pinned_standard_list() {
    let (directory, _module_commit) = captured_standard_project_with(
        &pinned_list_closure(),
        &[("main.orna", include_str!("fixtures/captured-standard-negative-take-main.orna"))],
    );

    let output = invoke(directory.path(), "invoke", "seed");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("error["));
}

#[test]
fn binary_invoke_refuses_a_standard_glob_import_with_its_import_code() {
    let (directory, _module_commit) = captured_standard_project_with(
        &[("math.orna", include_str!("fixtures/captured-standard-math.orna"))],
        &[("main.orna", "use std.math.*;\npub fn answer(): Int = increment(41);\n")],
    );

    let output = invoke(directory.path(), "invoke", "answer");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[ORNA-S010-IMPORT]"), "stderr: {stderr}");
}

#[test]
fn binary_invoke_refuses_a_standard_module_alias_import_with_its_import_code() {
    let (directory, _module_commit) = captured_standard_project_with(
        &[("math.orna", include_str!("fixtures/captured-standard-math.orna"))],
        &[("main.orna", "use std.math as m;\npub fn answer(): Int = increment(41);\n")],
    );

    let output = invoke(directory.path(), "invoke", "answer");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error[E2101]"), "stderr: {stderr}");
}

#[test]
fn binary_invoke_rejects_a_braced_standard_name_missing_from_the_pinned_module() {
    let (directory, _module_commit) = captured_standard_project_with(
        &[("math.orna", include_str!("fixtures/captured-standard-math.orna"))],
        &[("main.orna", "use std.math.{no_such_function};\npub fn answer(): Int = 1;\n")],
    );

    let output = invoke(directory.path(), "invoke", "answer");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.is_empty(), "missing braced name must be diagnosed");
}

#[test]
fn binary_repl_imports_a_captured_standard_module() {
    use std::io::Write as _;

    let (directory, _module_commit) = captured_standard_project();
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory.path())
        .arg("repl")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("CLI REPL");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(b"use std.math.{increment};\nincrement(41)\n")
        .expect("write standard import");
    let output = child.wait_with_output().expect("CLI REPL output");
    assert!(output.status.success(), "repl stderr: {:?}", output.stderr);
    assert!(output.stderr.is_empty(), "repl stderr: {:?}", output.stderr);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("42"),
        "repl stdout: {:?}",
        output.stdout
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("error["));
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn binary_sensors_ingest_reopens_typed_rows_and_checkpoint() {
    let directory = reference_project();
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI process");
    assert!(init.status.success());
    assert_eq!(init.stdout, b"initialized Orna repository\n");
    assert!(init.stderr.is_empty());

    let first = invoke(directory.path(), "run", "sensors.ingest");
    assert!(first.status.success(), "sensors invocation failed");
    assert_eq!(first.stdout, b"invocation completed\n");
    assert!(first.stderr.is_empty());

    let repository = Repository::discover(directory.path()).expect("repository");
    let (runtime_identity, initial_digest) = identity(directory.path());
    let state = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after sensors invocation");
    let readings = state
        .committed_table_rows("sensors.Reading")
        .await
        .expect("read durable readings");
    assert_eq!(readings.len(), 3);
    for (sequence, value) in [
        (0, decimal(1825, -2)),
        (1, decimal(1850, -2)),
        (2, decimal(1875, -2)),
    ] {
        assert!(readings.iter().any(|(_, row)| {
            text(row, "sensor") == "greenhouse"
                && integer(row, "sequence") == sequence
                && field(row, "value") == Some(value.clone())
        }));
    }

    let key = sensors_checkpoint_key(runtime_identity.database_id);
    let checkpoint = state
        .stream_checkpoint(&key)
        .await
        .expect("read sensors checkpoint");
    assert_eq!(
        checkpoint
            .committed
            .as_ref()
            .map(|position| position.token.as_str()),
        Some("3")
    );
    drop(state);

    let second = invoke(directory.path(), "run", "sensors.ingest");
    assert!(second.status.success(), "second sensors invocation failed");
    assert_eq!(second.stdout, b"invocation completed\n");
    assert!(second.stderr.is_empty());

    let reopened = RuntimeState::open(&repository, runtime_identity, initial_digest)
        .await
        .expect("reopen runtime after second sensors invocation");
    assert_eq!(
        reopened
            .committed_table_rows("sensors.Reading")
            .await
            .expect("read durable readings after restart"),
        readings
    );
    assert_eq!(
        reopened
            .stream_checkpoint(&key)
            .await
            .expect("read sensors checkpoint after restart"),
        checkpoint
    );
}
