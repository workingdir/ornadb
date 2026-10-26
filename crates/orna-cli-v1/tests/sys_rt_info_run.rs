//! `orna run` and `orna invoke` evaluate `sys.rt.info()` in a fresh `orna init`
//! project; `orna repl` evaluates it in a committed project with an admitted
//! table, because its effect path needs one. The run commits every coordinate
//! the runtime reports; invoke and the REPL show the record is attached under
//! their own host modes.

use std::{
    path::Path,
    process::{Command, Output},
};

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;
use tempfile::TempDir;

#[path = "support/format3.rs"]
#[allow(dead_code)]
mod format3;

const RUNTIME_INFO_SOURCE: &str = r#"pub table RuntimeEvidence(id: Int) {
    language_version: Str,
    sys_version: Str,
    canonical_orna_codec_version: Str,
    repository_layout_version: Str,
    storage_manifest_version: Str,
    presentation_protocol_version: Str,
    build_id: Str,
    unicode_version: Str,
    timezone_database_version: Str,
    read_only: Bool,
    run_mode: Bool,
    std_snapshot_pinned: Bool,
    runtime: sys.RuntimeId,
}

pub fn main() {
    let info = sys.rt.info();
    let timezone = case info.timezone_database_version {
        Some(version): version,
        null: "none",
    };
    let std_snapshot_pinned = case info.std_snapshot {
        Some(_): true,
        null: false,
    };
    RuntimeEvidence.insert({
        id: 1,
        language_version: info.language_version,
        sys_version: info.sys_version,
        canonical_orna_codec_version: info.canonical_orna_codec_version,
        repository_layout_version: info.repository_layout_version,
        storage_manifest_version: info.storage_manifest_version,
        presentation_protocol_version: info.presentation_protocol_version,
        build_id: info.build_id,
        unicode_version: info.unicode_version,
        timezone_database_version: timezone,
        read_only: info.read_only,
        run_mode: info.mode == sys.RuntimeMode.run,
        std_snapshot_pinned: std_snapshot_pinned,
        runtime: info.runtime
    });
}
"#;

const LANGUAGE_SOURCE: &str = r#"pub fn language(): Str {
    sys.rt.info().language_version
}
"#;

/// The format-3 fixture analyzes the bare declared surface without the system
/// catalogue, so the REPL table uses standard field types only.
const REPL_TABLE_SOURCE: &str = r#"pub table RuntimeEvidence(id: Int) {
    language_version: Str,
}
"#;

fn initialized(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), source).expect("project source");
    directory
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn assert_output(output: &Output, stdout: &str) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        stdout,
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
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

fn uuid_bytes(row: &[u8], name: &str) -> Vec<u8> {
    match row_field(row, name) {
        OvbRaw::Tag(37, value) => match *value {
            OvbRaw::Bytes(bytes) => bytes,
            other => panic!("field {name} is not uuid bytes: {other:?}"),
        },
        other => panic!("field {name} is not a uuid: {other:?}"),
    }
}

/// ORNA-SYS-006 (source/15-system.md:46): in a fresh project, `orna run`
/// evaluates `sys.rt.info()` and the committed evidence row carries the exact
/// coordinates of this runtime: language, system API, codec, storage and
/// protocol versions, build, Unicode and timezone datasets, and the host facts
/// the run attached (runtime, mode, read-only flag and standard snapshot pin).
#[tokio::test(flavor = "current_thread")]
async fn orna_run_commits_the_runtime_info_coordinates() {
    let project = initialized(RUNTIME_INFO_SOURCE);
    let directory = project.path();

    assert_output(
        &orna(directory, &["run", "main.main"]),
        "invocation completed\n",
    );

    let repository = Repository::discover(directory).expect("repository");
    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("open runtime state");
    let key = Value::int(1.into()).encode().expect("integer key");
    let evidence = state
        .committed_table_row("RuntimeEvidence", &key)
        .await
        .expect("read runtime evidence")
        .expect("runtime evidence row is committed");
    assert_eq!(text(&evidence, "language_version"), "1.1.0");
    assert_eq!(text(&evidence, "sys_version"), "1.1");
    assert_eq!(text(&evidence, "canonical_orna_codec_version"), "OVB-2");
    assert_eq!(text(&evidence, "repository_layout_version"), "3");
    assert_eq!(text(&evidence, "storage_manifest_version"), "store-3");
    assert_eq!(text(&evidence, "presentation_protocol_version"), "1");
    assert!(
        !text(&evidence, "build_id").is_empty(),
        "build id is reported"
    );
    let (major, minor, patch) = char::UNICODE_VERSION;
    assert_eq!(
        text(&evidence, "unicode_version"),
        format!("{major}.{minor}.{patch}")
    );
    assert_eq!(
        text(&evidence, "timezone_database_version"),
        "orna-iana-2024a"
    );
    assert_eq!(uuid_bytes(&evidence, "runtime").len(), 16);
    assert_eq!(row_field(&evidence, "read_only"), OvbRaw::Bool(false));
    assert_eq!(row_field(&evidence, "run_mode"), OvbRaw::Bool(true));
    assert_eq!(
        row_field(&evidence, "std_snapshot_pinned"),
        OvbRaw::Bool(false)
    );
}

/// `orna invoke` attaches the same record for a pure invocation, so a function
/// returning `sys.rt.info().language_version` completes.
#[test]
fn invoke_evaluates_runtime_info_in_a_fresh_project() {
    let project = initialized(LANGUAGE_SOURCE);
    assert_output(
        &orna(project.path(), &["invoke", "language"]),
        "invocation completed\n",
    );
}

/// The project REPL attaches the same record under its own mode, so the
/// language version and the mode are readable from submitted expressions.
/// Its effect path needs an admitted table with committed identities, so the
/// fixture is a committed format 3 project rather than a bare `orna init`.
#[test]
fn repl_evaluates_runtime_info_in_a_committed_project() {
    let (directory, _repository, _) =
        format3::format3_repository_for_table(REPL_TABLE_SOURCE, "RuntimeEvidence");
    let root = directory.path();
    assert_output(
        &orna(root, &["repl", "sys.rt.info().language_version"]),
        "\"1.1.0\" : Str\n",
    );
    assert_output(
        &orna(root, &["repl", "sys.rt.info().mode == sys.RuntimeMode.repl"]),
        "true : Bool\n",
    );
}
