//! A trusted REPL `Table.rekey` followed by `orna publish` must publish exactly
//! one logical row rekey, visible to `orna history --diff` as a single
//! `rekeyed` change rather than a removal plus an addition. A rekey of a table
//! that carries a Blob field is rejected with a stable diagnostic and writes
//! nothing.

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
}

pub fn add(id: Str, title: Str) =
    media.insert({ id: id, title: title });

pub fn relocate(old: Str, new: Str) = media.rekey(old, new);
"#;

const RELOCATE_BLOB_SOURCE: &str = r#"pub table media(id: Str) {
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
    relation_id
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The canonical ORP key bytes of a text primary key.
fn text_key(key: &str) -> Vec<u8> {
    orna_repository_v1::TypedKey::Text(key.to_owned())
        .canonical_bytes()
        .expect("canonical text key")
}

#[test]
fn repl_rekey_publishes_one_logical_rekey() {
    let (directory, repository, relation_id) = format3_repository(RELOCATE_SOURCE);
    let root = directory.path();

    let added = run_repl(root, "main.add(\"london\", \"Live in London\").id");
    assert_eq!(
        added.status.code(),
        Some(0),
        "{:?}",
        String::from_utf8_lossy(&added.stdout)
    );
    assert!(added.stderr.is_empty(), "{:?}", added.stderr);
    let published_add = run(root, &["publish"]);
    assert_eq!(
        published_add.status.code(),
        Some(0),
        "publish after add failed"
    );
    let before = head(root);

    let relocated = run_repl(root, "main.relocate(\"london\", \"paris\")");
    let relocated_stdout = String::from_utf8_lossy(&relocated.stdout);
    assert_eq!(relocated.status.code(), Some(0), "{relocated_stdout}");
    assert!(relocated.stderr.is_empty(), "{:?}", relocated.stderr);
    assert!(
        !relocated_stdout.contains("ORNA-REPL-EFFECT"),
        "{relocated_stdout}"
    );

    let published_rekey = run(root, &["publish"]);
    assert_eq!(
        published_rekey.status.code(),
        Some(0),
        "publish after rekey failed: {}{}",
        String::from_utf8_lossy(&published_rekey.stdout),
        String::from_utf8_lossy(&published_rekey.stderr)
    );
    let after = head(root);
    assert_ne!(before, after, "publishing the rekey advanced HEAD");

    // The publication committed exactly one durable logical rekey with the
    // exact canonical old and new addresses.
    let published = repository.head().expect("HEAD").expect("published HEAD");
    let log = repository
        .read_committed_rekeys(&published)
        .expect("committed rekey log");
    assert_eq!(log.len(), 1, "one durable logical rekey: {log:?}");
    assert_eq!(log[0].ordinal(), 0);
    assert_eq!(log[0].relation_id(), &relation_id);
    assert_eq!(log[0].old_key(), text_key("london").as_slice());
    assert_eq!(log[0].new_key(), text_key("paris").as_slice());

    let relation = relation_hex(relation_id);
    let diff = run(
        root,
        &[
            "history", &relation, "--diff", &before, &after, "--format", "json",
        ],
    );
    assert_eq!(
        diff.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(
        report["rekeyed"],
        serde_json::json!(1),
        "one logical rekey: {report}"
    );
    assert_eq!(report["added"], serde_json::json!(0), "{report}");
    assert_eq!(report["removed"], serde_json::json!(0), "{report}");

    // A later rekey appends to the committed log instead of replacing it, and
    // the diff of that publication reports only the new rekey.
    let relocated_again = run_repl(root, "main.relocate(\"paris\", \"rome\")");
    let relocated_again_stdout = String::from_utf8_lossy(&relocated_again.stdout);
    assert_eq!(
        relocated_again.status.code(),
        Some(0),
        "{relocated_again_stdout}"
    );
    assert!(
        relocated_again.stderr.is_empty(),
        "{:?}",
        relocated_again.stderr
    );
    assert!(
        !relocated_again_stdout.contains("error["),
        "{relocated_again_stdout}"
    );
    let published_again = run(root, &["publish"]);
    assert_eq!(
        published_again.status.code(),
        Some(0),
        "publish after the second rekey failed: {}{}",
        String::from_utf8_lossy(&published_again.stdout),
        String::from_utf8_lossy(&published_again.stderr)
    );
    let latest = head(root);
    let published = repository.head().expect("HEAD").expect("published HEAD");
    let log = repository
        .read_committed_rekeys(&published)
        .expect("committed rekey log");
    assert_eq!(log.len(), 2, "the log keeps every logical rekey: {log:?}");
    assert_eq!(log[0].old_key(), text_key("london").as_slice());
    assert_eq!(log[1].ordinal(), 1);
    assert_eq!(log[1].relation_id(), &relation_id);
    assert_eq!(log[1].old_key(), text_key("paris").as_slice());
    assert_eq!(log[1].new_key(), text_key("rome").as_slice());
    let diff = run(
        root,
        &[
            "history", &relation, "--diff", &after, &latest, "--format", "json",
        ],
    );
    assert_eq!(
        diff.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(report["rekeyed"], serde_json::json!(1), "{report}");
    assert_eq!(report["added"], serde_json::json!(0), "{report}");
    assert_eq!(report["removed"], serde_json::json!(0), "{report}");
}

#[test]
fn repl_rekey_of_a_blob_bearing_table_is_rejected_without_writing() {
    let (directory, repository, relation_id) = format3_repository(RELOCATE_BLOB_SOURCE);
    let root = directory.path();
    std::fs::write(root.join("recording.mp3"), b"synthetic rekey payload").unwrap();
    let before = head(root);

    // The captured row stays pending rather than being published first: a
    // REPL activation cannot load committed Blob rows into evaluator rows,
    // and the rejection depends only on the table schema, before any row
    // lookup.
    let add = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add(\"london\", \"Live in London\", audio).id",
        root = root.to_string_lossy().as_ref(),
    );
    let added = run_repl(root, &add);
    assert_eq!(
        added.status.code(),
        Some(0),
        "{:?}",
        String::from_utf8_lossy(&added.stdout)
    );
    assert!(added.stderr.is_empty(), "{:?}", added.stderr);

    let relocated = run_repl(root, "main.relocate(\"london\", \"paris\")");
    let relocated_stdout = String::from_utf8_lossy(&relocated.stdout);
    assert_eq!(relocated.status.code(), Some(0), "{relocated_stdout}");
    assert!(relocated.stderr.is_empty(), "{:?}", relocated.stderr);
    assert!(
        relocated_stdout.contains("error[ORNA-EVAL-TABLE-REKEY-BLOB]"),
        "Blob-bearing rekey must fail with its stable code: {relocated_stdout}"
    );
    assert!(
        !relocated_stdout.contains("ORNA-REPL-EFFECT"),
        "{relocated_stdout}"
    );

    // The rejected rekey staged nothing: publishing commits the captured
    // insert alone, under its original key, and no logical rekey.
    let published_add = run(root, &["publish"]);
    assert_eq!(
        published_add.status.code(),
        Some(0),
        "publish after the rejected rekey failed: {}{}",
        String::from_utf8_lossy(&published_add.stdout),
        String::from_utf8_lossy(&published_add.stderr)
    );
    let after = head(root);
    assert_ne!(before, after, "the pending insert was published");
    let published = repository.head().expect("HEAD").expect("published HEAD");
    let log = repository
        .read_committed_rekeys(&published)
        .expect("committed rekey log");
    assert!(
        log.is_empty(),
        "a rejected rekey publishes no logical rekey: {log:?}"
    );

    let relation = relation_hex(relation_id);
    let diff = run(
        root,
        &[
            "history", &relation, "--diff", &before, &after, "--format", "json",
        ],
    );
    assert_eq!(
        diff.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert_eq!(report["added"], serde_json::json!(1), "{report}");
    assert_eq!(report["rekeyed"], serde_json::json!(0), "{report}");
    assert_eq!(report["removed"], serde_json::json!(0), "{report}");
    assert_eq!(
        report["changes"][0]["key"],
        serde_json::json!("london"),
        "{report}"
    );
}
