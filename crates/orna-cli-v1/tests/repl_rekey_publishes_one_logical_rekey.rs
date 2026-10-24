//! A trusted REPL `Table.rekey` followed by `orna publish` must publish exactly
//! one logical row rekey, visible to `orna history --diff` as a single
//! `rekeyed` change rather than a removal plus an addition.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

#[path = "support/format3.rs"]
mod format3;
use format3::*;

const RELOCATE_SOURCE: &str = r#"pub table media(id: Str) {
    title: Str,
    content: Blob,
}

pub fn add(id: Str, title: Str, content: Blob) =
    media.insert({ id: id, title: title, content: content });

pub fn relocate(old: Str, new: Str) = media.rekey(old, new);
"#;

fn run_repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
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

fn run(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .output()
        .expect("run orna")
}

fn head(directory: &Path) -> String {
    String::from_utf8(git_output(directory, &["rev-parse", "HEAD"], None))
        .expect("HEAD text")
        .trim()
        .to_owned()
}

fn relation_hex(relation_id: [u8; 16]) -> String {
    relation_id.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn repl_rekey_publishes_one_logical_rekey() {
    let (directory, _repository, relation_id) = format3_repository(RELOCATE_SOURCE);
    let root = directory.path();
    std::fs::write(root.join("recording.mp3"), b"synthetic rekey payload").unwrap();

    let add = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add(\"london\", \"Live in London\", audio).id",
        root = root.to_string_lossy().as_ref(),
    );
    let added = run_repl(root, &add);
    assert_eq!(added.status.code(), Some(0), "{:?}", String::from_utf8_lossy(&added.stdout));
    assert!(added.stderr.is_empty(), "{:?}", added.stderr);
    let published_add = run(root, &["publish"]);
    assert_eq!(published_add.status.code(), Some(0), "publish after add failed");
    let before = head(root);

    let relocated = run_repl(root, "main.relocate(\"london\", \"paris\")");
    let relocated_stdout = String::from_utf8_lossy(&relocated.stdout);
    assert_eq!(relocated.status.code(), Some(0), "{relocated_stdout}");
    assert!(relocated.stderr.is_empty(), "{:?}", relocated.stderr);
    assert!(!relocated_stdout.contains("ORNA-REPL-EFFECT"), "{relocated_stdout}");

    let published_rekey = run(root, &["publish"]);
    assert_eq!(published_rekey.status.code(), Some(0), "publish after rekey failed");
    let after = head(root);
    assert_ne!(before, after, "publishing the rekey advanced HEAD");

    let relation = relation_hex(relation_id);
    let diff = run(
        root,
        &["history", &relation, "--diff", &before, &after, "--format", "json"],
    );
    assert_eq!(diff.status.code(), Some(0), "{}", String::from_utf8_lossy(&diff.stderr));
    let report: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(report["rekeyed"], serde_json::json!(1), "one logical rekey: {report}");
    assert_eq!(report["added"], serde_json::json!(0), "{report}");
    assert_eq!(report["removed"], serde_json::json!(0), "{report}");
}
