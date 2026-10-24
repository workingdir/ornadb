//! A trusted REPL `Table.rekey` followed by `orna publish` must publish exactly
//! one logical row rekey, visible to `orna history --diff` as a single
//! `rekeyed` change rather than a removal plus an addition. A Blob-bearing row
//! moves with its committed Blob reference unchanged, and a rekey of long keys
//! publishes a log row whose long addresses are stored through the overflow
//! graph.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_repository_v1::{Repository, TypedKey, relation_admission::CommittedRekey};

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

/// Runs one trusted REPL submission and requires it to evaluate cleanly.
fn submit(directory: &Path, input: &str) {
    let output = run_repl(directory, input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert!(!stdout.contains("error["), "{stdout}");
}

/// Publishes the pending runtime changes and returns the new HEAD.
fn publish(directory: &Path) -> String {
    let before = head(directory);
    let published = run(directory, &["publish"]);
    assert_eq!(
        published.status.code(),
        Some(0),
        "publish failed: {}{}",
        String::from_utf8_lossy(&published.stdout),
        String::from_utf8_lossy(&published.stderr)
    );
    let after = head(directory);
    assert_ne!(before, after, "the publication advanced HEAD");
    after
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

/// The `orna history --diff` JSON report between two snapshots.
fn diff(directory: &Path, relation_id: [u8; 16], from: &str, to: &str) -> serde_json::Value {
    let relation = relation_hex(relation_id);
    let diff = run(
        directory,
        &["history", &relation, "--diff", from, to, "--format", "json"],
    );
    assert_eq!(
        diff.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    serde_json::from_slice(&diff.stdout).unwrap()
}

/// Requires a diff report to hold exactly one rekey, from `from_key` to `key`.
fn assert_one_rekey(report: &serde_json::Value, from_key: &str, key: &str) {
    assert_eq!(report["rekeyed"], serde_json::json!(1), "{report}");
    assert_eq!(report["added"], serde_json::json!(0), "{report}");
    assert_eq!(report["removed"], serde_json::json!(0), "{report}");
    assert_eq!(
        report["changes"][0]["change"],
        serde_json::json!("rekeyed"),
        "{report}"
    );
    assert_eq!(
        report["changes"][0]["from_key"],
        serde_json::json!(from_key),
        "{report}"
    );
    assert_eq!(
        report["changes"][0]["key"],
        serde_json::json!(key),
        "{report}"
    );
}

/// The committed rekey log of the published HEAD.
fn committed_rekeys(repository: &Repository) -> Vec<CommittedRekey> {
    let published = repository.head().expect("HEAD").expect("published HEAD");
    repository
        .read_committed_rekeys(&published)
        .expect("committed rekey log")
}

/// Requires one log entry to record a move of `relation_id`'s row.
fn assert_logged(
    rekey: &CommittedRekey,
    ordinal: u64,
    relation_id: [u8; 16],
    old: &str,
    new: &str,
) {
    assert_eq!(rekey.ordinal(), ordinal);
    assert_eq!(rekey.relation_id(), &relation_id);
    assert_eq!(rekey.old_key(), text_key(old).as_slice());
    assert_eq!(rekey.new_key(), text_key(new).as_slice());
}

/// The canonical ORP key bytes of a text primary key.
fn text_key(key: &str) -> Vec<u8> {
    TypedKey::Text(key.to_owned())
        .canonical_bytes()
        .expect("canonical text key")
}

/// The committed stored tuple of the row at a text key, if it exists.
fn stored_row(repository: &Repository, relation_id: [u8; 16], key: &str) -> Option<Vec<u8>> {
    committed_store_rows(repository, relation_id)
        .into_iter()
        .find(|row| row.key() == &TypedKey::Text(key.to_owned()))
        .map(|row| {
            row.value()
                .encoded_fields()
                .expect("an inline committed row")
                .to_vec()
        })
}

#[test]
fn repl_rekey_publishes_one_logical_rekey() {
    let (directory, repository, relation_id) = format3_repository(RELOCATE_SOURCE);
    let root = directory.path();
    submit(root, "main.add(\"london\", \"Live in London\").id");
    let before = publish(root);

    submit(root, "main.relocate(\"london\", \"paris\")");
    let after = publish(root);
    // The publication committed exactly one durable logical rekey with the
    // exact canonical old and new addresses.
    let log = committed_rekeys(&repository);
    assert_eq!(log.len(), 1, "one durable logical rekey: {log:?}");
    assert_logged(&log[0], 0, relation_id, "london", "paris");
    assert_one_rekey(&diff(root, relation_id, &before, &after), "london", "paris");

    // A later rekey appends to the committed log instead of replacing it, and
    // the diff of that publication reports only the new rekey.
    submit(root, "main.relocate(\"paris\", \"rome\")");
    let latest = publish(root);
    let log = committed_rekeys(&repository);
    assert_eq!(log.len(), 2, "the log keeps every logical rekey: {log:?}");
    assert_logged(&log[0], 0, relation_id, "london", "paris");
    assert_logged(&log[1], 1, relation_id, "paris", "rome");
    assert_one_rekey(&diff(root, relation_id, &after, &latest), "paris", "rome");

    // Across both publications the chain reads as one rekey, and the reverse
    // diff undoes it instead of failing.
    assert_one_rekey(&diff(root, relation_id, &before, &latest), "london", "rome");
    assert_one_rekey(&diff(root, relation_id, &latest, &before), "rome", "london");
}

#[test]
fn repl_rekey_of_a_blob_bearing_row_moves_its_committed_blob_unchanged() {
    let (directory, repository, relation_id) = format3_repository(RELOCATE_BLOB_SOURCE);
    let root = directory.path();
    std::fs::write(root.join("recording.mp3"), b"synthetic rekey payload").unwrap();
    let add = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add(\"london\", \"Live in London\", audio).id",
        root = root.to_string_lossy().as_ref(),
    );
    submit(root, &add);
    let before = publish(root);
    let committed = stored_row(&repository, relation_id, "london").expect("published row");

    submit(root, "main.relocate(\"london\", \"paris\").id");
    let after = publish(root);
    // The row moved whole: its stored tuple, Blob reference (length, SHA-256,
    // annotation and descriptor) included, is byte-identical at the new key.
    assert_eq!(stored_row(&repository, relation_id, "london"), None);
    assert_eq!(
        stored_row(&repository, relation_id, "paris"),
        Some(committed),
        "the rekeyed row keeps its committed Blob reference"
    );
    let log = committed_rekeys(&repository);
    assert_eq!(log.len(), 1, "one durable logical rekey: {log:?}");
    assert_logged(&log[0], 0, relation_id, "london", "paris");
    let report = diff(root, relation_id, &before, &after);
    assert_one_rekey(&report, "london", "paris");
    assert_eq!(
        report["changes"][0]["content_changed"],
        serde_json::json!(false),
        "{report}"
    );
}

#[test]
fn repl_rekeys_of_long_keys_publish_through_overflow_log_rows() {
    let (directory, repository, relation_id) = format3_repository(RELOCATE_SOURCE);
    let root = directory.path();
    // Each address spells about 5 KiB, so its hex spelling in the log row
    // exceeds the 8 KiB inline limit and is lowered to the overflow graph.
    let first = "o".repeat(5_000);
    let second = "n".repeat(5_000);
    submit(root, &format!("main.add({first:?}, \"Long\").id"));
    let before = publish(root);

    submit(root, &format!("main.relocate({first:?}, {second:?}).id"));
    let after = publish(root);
    let log = committed_rekeys(&repository);
    assert_eq!(log.len(), 1, "one durable logical rekey: {log:?}");
    assert_logged(&log[0], 0, relation_id, &first, &second);
    assert_one_rekey(&diff(root, relation_id, &before, &after), &first, &second);

    // Appending reads the overflow row back and keeps it.
    submit(root, &format!("main.relocate({second:?}, \"short\").id"));
    let latest = publish(root);
    let log = committed_rekeys(&repository);
    assert_eq!(log.len(), 2, "the log keeps every logical rekey: {log:?}");
    assert_logged(&log[0], 0, relation_id, &first, &second);
    assert_logged(&log[1], 1, relation_id, &second, "short");
    assert_one_rekey(&diff(root, relation_id, &before, &latest), &first, "short");
}
