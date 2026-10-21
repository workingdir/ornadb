//! Format-3 publication of rows and tables larger than one row page, through
//! product commands only: `orna init`, `orna commit`, REPL inserts and
//! updates, `orna publish`, then `orna query` and `orna history --diff` read
//! the rows back (store-format-3 §3 and §5, ORNA-ROW-003, ORNA-ROW-005).

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const NOTE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n";

/// Keeps the host's Git configuration and repository routing out of every
/// process this test starts.
fn isolate_git(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    isolate_git(&mut command);
    command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    isolate_git(&mut command);
    let mut child = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the orna REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the orna REPL exits")
}

fn git_bytes(root: &Path, arguments: &[&str]) -> Vec<u8> {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git(root: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_bytes(root, arguments)).expect("git prints UTF-8")
}

fn transcript(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, step: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{step} failed\n{}",
        transcript(output)
    );
    assert!(
        !transcript(output).contains("error["),
        "{step} reported an error\n{}",
        transcript(output)
    );
}

/// `orna init`, a test-local commit identity, `main.orna` replaced by
/// `source`, then the source staged with Git and committed with
/// `orna commit`. Returns the root and the relation the commit admitted.
fn committed_database(directory: &TempDir, source: &str) -> (PathBuf, String) {
    let root = directory.path().join("notes");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare Note"]);
    assert_success(&committed, "orna commit");
    let relation = String::from_utf8_lossy(&committed.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("admitted table Note as relation "))
        .expect("orna commit admits the Note table")
        .trim()
        .to_owned();
    (root, relation)
}

fn publish(root: &Path, message: &str) {
    let published = orna(root, &["publish", "--message", message]);
    assert_success(&published, "orna publish");
}

fn json(output: &Output, step: &str) -> serde_json::Value {
    assert_success(output, step);
    serde_json::from_slice(&output.stdout).expect("the command prints JSON")
}

fn query_rows(root: &Path, arguments: &[&str]) -> u64 {
    let report = json(&orna(root, arguments), "orna query");
    report["rows"].as_u64().expect("the report counts rows")
}

/// The kind, encoded length and object ID of every format-3 node `data`
/// blob reachable from the committed `.orna/store` (store-format-3 §1: every
/// node data is `[1, kind, ...]`).
fn store_nodes(root: &Path) -> Vec<(u8, usize, String)> {
    let listing = git(root, &["ls-tree", "-r", "HEAD", ".orna/store"]);
    let mut nodes = Vec::new();
    for line in listing.lines() {
        let (entry, path) = line.split_once('\t').expect("an ls-tree entry");
        if path != ".orna/store/data" && !path.ends_with("/data") {
            continue;
        }
        let oid = entry.split(' ').nth(2).expect("an ls-tree object id");
        let data = git_bytes(root, &["cat-file", "blob", oid]);
        assert_eq!(data[1], 0x01, "node data starts [1, kind, ...]");
        nodes.push((data[2], data.len(), oid.to_owned()));
    }
    nodes
}

/// The distinct row pages (kind-1 ordered leaves) of the committed store.
fn row_pages(root: &Path) -> BTreeSet<String> {
    store_nodes(root)
        .into_iter()
        .filter(|(kind, _, _)| *kind == 1)
        .map(|(_, _, oid)| oid)
        .collect()
}

/// A note body beyond the 8 KiB inline row limit and within the 16 KiB a
/// REPL string literal may hold.
fn long_body(label: &str) -> String {
    format!("{label}: {}", "the ridge log continues ".repeat(500))
}

/// store-format-3 §5: a row entry over 8 KiB is stored through tag 60113 and
/// a kind-3 overflow node whose bytes sit behind an OGB-2 byte index, so its
/// row page stays small. The CLI publishes, lists, looks up, diffs and
/// updates the row (A07-storage-rows-05).
#[test]
fn a_row_over_8_kib_publishes_through_the_overflow_graph() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation) = committed_database(&directory, NOTE_SOURCE);
    let body = long_body("first");
    assert!(body.len() > 8 * 1024 && body.len() < 16 * 1024);

    let inserted = orna_repl(
        &root,
        &format!(r#"main.Note.insert({{ id: "ridge", body: "{body}" }})"#),
    );
    assert_success(&inserted, "REPL insert");
    publish(&root, "Add the ridge log");

    let nodes = store_nodes(&root);
    assert!(
        nodes.iter().any(|(kind, _, _)| *kind == 3),
        "the long row is a kind-3 overflow node: {nodes:?}"
    );
    assert!(
        nodes.iter().any(|(kind, _, _)| *kind == 4),
        "the overflow bytes sit behind a byte index: {nodes:?}"
    );
    let largest_page = nodes
        .iter()
        .filter(|(kind, _, _)| *kind == 1)
        .map(|(_, length, _)| *length)
        .max()
        .expect("the store has row pages");
    assert!(
        largest_page < 1024,
        "the row page names the overflow node instead of holding the body: {largest_page} bytes"
    );

    assert_eq!(
        query_rows(&root, &["query", &relation, "--format", "json"]),
        1
    );
    assert_eq!(
        query_rows(
            &root,
            &["query", &relation, "--key", "ridge", "--format", "json"]
        ),
        1
    );
    let added = json(
        &orna(
            &root,
            &[
                "history", &relation, "--diff", "HEAD~1", "HEAD", "--format", "json",
            ],
        ),
        "orna history --diff",
    );
    assert_eq!(added["added"], 1, "{added}");

    let updated = orna_repl(
        &root,
        &format!(
            r#"main.Note.update("ridge", {{ body: "{}" }})"#,
            long_body("second")
        ),
    );
    assert_success(&updated, "REPL update");
    publish(&root, "Revise the ridge log");
    let changed = json(
        &orna(
            &root,
            &[
                "history", &relation, "--diff", "HEAD~1", "HEAD", "--format", "json",
            ],
        ),
        "orna history --diff",
    );
    assert_eq!(changed["changed"], 1, "{changed}");
    assert_eq!(changed["changes"][0]["content_changed"], true, "{changed}");
}

/// store-format-3 §3 and ORNA-ROW-006 (A07-storage-rows-04): an update to a
/// table of many row pages rewrites only the page that holds the row, and
/// every other page keeps its object; ORNA-ROW-010: rewriting a row to the
/// value it already holds publishes no change at all.
#[test]
fn an_update_to_a_large_table_rewrites_only_the_page_holding_the_row() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation) = committed_database(&directory, NOTE_SOURCE);
    let body =
        |index: usize, label: &str| format!("{label} {index}: {}", "the ridge log ".repeat(12));
    let inserts = (0..600)
        .map(|index| {
            format!(
                r#"main.Note.insert({{ id: "n{index:04}", body: "{}" }})"#,
                body(index, "first")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_success(&orna_repl(&root, &inserts), "REPL inserts");
    publish(&root, "Add six hundred notes");
    let pages_before = row_pages(&root);
    assert!(
        pages_before.len() >= 4,
        "the table spans several row pages: {}",
        pages_before.len()
    );
    assert_eq!(
        query_rows(&root, &["query", &relation, "--format", "json"]),
        600
    );

    let revise = format!(
        r#"main.Note.update("n0300", {{ body: "{}" }})"#,
        body(300, "second")
    );
    assert_success(&orna_repl(&root, &revise), "REPL update");
    publish(&root, "Revise one note");
    let pages_after = row_pages(&root);
    let rewritten = pages_after.difference(&pages_before).count();
    assert!(
        rewritten == 1 && pages_before.difference(&pages_after).count() == 1,
        "one of {} row pages is rewritten, not {rewritten}",
        pages_before.len()
    );
    let changed = json(
        &orna(
            &root,
            &[
                "history", &relation, "--diff", "HEAD~1", "HEAD", "--format", "json",
            ],
        ),
        "orna history --diff",
    );
    assert_eq!(changed["changed"], 1, "{changed}");
    assert_eq!(
        query_rows(&root, &["query", &relation, "--format", "json"]),
        600
    );

    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_success(&orna_repl(&root, &revise), "REPL update to the same body");
    let unchanged = orna(&root, &["publish", "--message", "Revise one note again"]);
    assert_success(&unchanged, "orna publish");
    assert!(
        String::from_utf8_lossy(&unchanged.stdout).contains("nothing to publish"),
        "{}",
        transcript(&unchanged)
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
}
