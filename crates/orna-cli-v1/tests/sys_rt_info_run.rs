//! `orna run` evaluates `sys.rt.info()` in a fresh `orna init` project and
//! commits the compatibility coordinates the runtime reports.

use std::process::Command;

use orna_foundation_v1::{OvbRaw, Value};
use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;

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
}

pub fn main() {
    let info = sys.rt.info();
    RuntimeEvidence.insert({ id: 1, language_version: info.language_version, sys_version: info.sys_version, canonical_orna_codec_version: info.canonical_orna_codec_version, repository_layout_version: info.repository_layout_version, storage_manifest_version: info.storage_manifest_version, presentation_protocol_version: info.presentation_protocol_version });
}
"#;

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

/// ORNA-SYS-006 (source/15-system.md:46): in a fresh project, `orna run`
/// evaluates `sys.rt.info()` and the committed evidence row carries the exact
/// language, system API, codec, storage and protocol coordinates of this
/// runtime.
#[tokio::test(flavor = "current_thread")]
async fn orna_run_commits_the_runtime_info_coordinates() {
    let directory = tempfile::tempdir().expect("project directory");
    std::fs::write(directory.path().join("main.orna"), RUNTIME_INFO_SOURCE)
        .expect("runtime info fixture");
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
}
