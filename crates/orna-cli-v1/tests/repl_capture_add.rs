use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

use orna_runtime_v1::RuntimeState;
use orna_value_v1::{ContextValue, Raw, Value, ValueFormat};

const MUSIC_SOURCE: &str = r#"pub table media(id: Str) {
    title: Str,
    content: Blob,
}

pub fn add(id: Str, title: Str, content: Blob) =
    media.insert({ id: id, title: title, content: content });
"#;

fn run_repl(directory: &std::path::Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write trusted REPL submission");
    child.wait_with_output().expect("wait for Orna REPL")
}

fn git_head(directory: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("read Git HEAD");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("HEAD text")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn repl_capture_annotate_and_add_commit_one_durable_blob_row_without_git_commit() {
    let (directory, repository, relation_id) = format3_repository(MUSIC_SOURCE);
    let root = directory.path();
    let payload = b"synthetic audio captured by the trusted REPL";
    std::fs::write(root.join("recording.mp3"), payload).expect("write synthetic media");
    let head_before = git_head(root);

    let input = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", max_bytes: 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add(\"london\", \"Live in London\", audio)",
        root = root.to_string_lossy().as_ref(),
    );
    let output = run_repl(root, &input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{:?}", output.stderr);
    assert!(
        !stdout.contains("ORNA-REPL-EFFECT"),
        "trusted REPL rejected the captured insert: {stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(git_head(root), head_before, "REPL must not publish a Git commit");

    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen the durable runtime after the REPL process exits");
    let encoded_row = state
        .committed_table_row("media", b"london")
        .await
        .expect("read the durable row after restart")
        .unwrap_or_else(|| panic!("the captured row is durable; REPL output: {stdout}"));
    let format = repository
        .open_format_context()
        .expect("open committed format-3 context");
    let value_context = format
        .open_value_context(relation_id)
        .expect("issue native-graph-backed value context");
    let row = ContextValue::decode_in_context(&encoded_row, ValueFormat::Rov3, &value_context)
        .expect("decode the stored row in its repository context");
    let Raw::Array(fields) = row.raw() else {
        panic!("stored Music row is not an array");
    };
    let encoded_blob = Value::new(fields[2].clone())
        .expect("construct content field value")
        .encode()
        .expect("encode content field value");
    let blob_value = ContextValue::decode_in_context(
        &encoded_blob,
        ValueFormat::Rov3,
        &value_context,
    )
    .expect("decode the stored Blob reference");
    let blob = blob_value.blob().expect("content field is a Blob");
    assert_eq!(blob.media_type(), "audio/mpeg");
    assert_eq!(blob.read_to_end().expect("read protected captured bytes"), payload);
}

#[path = "support/format3.rs"]
mod format3;
use format3::*;
