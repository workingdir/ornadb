//! Reading a format-1/2 (legacy) snapshot with `orna query` and `orna history`,
//! and refusing a snapshot format admission refuses.
//!
//! ORNA-UPGRADE-001 dispatches readers by the recorded repository format,
//! ORNA-UPGRADE-002 keeps a format-1/2 commit readable under its original
//! decoders, and ORNA-ROW-013 requires format-1/2 histories to retain versioned
//! readers. ACCEPTANCE Gate F covers both halves: the migrated/legacy reads and
//! the fail-early boundary for a snapshot whose recorded format its tracked
//! paths contradict (ORNA-META-001, ORNA-META-002,
//! `implementation/ACCEPTANCE.md` Gate A and Gate F).
//!
//! The first test reads the golden format-1 bundle
//! (`tests/fixtures/legacy-format1.bundle`): three compact tables (Int, Str,
//! Date) and one editable table whose rows are committed loose row units. The
//! second builds a format-3 repository with `orna` itself, publishes a row, then
//! commits a loose row file beside the store with plain Git: project admission
//! refuses that snapshot with ORNA-REPO-CONTEXT-004, and both commands must
//! report that refusal instead of answering the row from the store.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
/// The fixture's compact table ids: `Uuid::from_u64_pair(0x018f_0000_0000_7100,
/// n)` in the builder. Table 1 holds Int keys 1..=4, table 2 holds Str keys
/// `alpha`/`beta`/`gamma`, and table 3 holds Date keys -1/0/1.
const COMPACT_INT_TABLE: &str = "018f0000000071000000000000000001";
const COMPACT_STR_TABLE: &str = "018f0000000071000000000000000002";
const COMPACT_DATE_TABLE: &str = "018f0000000071000000000000000003";
/// The fixture's editable table, addressed by its key-path directory: 1.0.0
/// keeps its rows at `contacts/Contact/<key component>.orna`.
const LOOSE_TABLE: &str = "contacts/Contact";
/// The fixture's commit count is read from the clone rather than pinned here:
/// the bundle gains a commit whenever a table is added to it, and a pinned
/// count only reports that the fixture grew.
fn fixture_commits(root: &Path) -> usize {
    git(root, &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .expect("the fixture clone reports its commit count")
}
/// The commits whose tree carries the editable rows: the row-unit commit and
/// its child, the gitlink commit.
const FIXTURE_PRESENT_REVISIONS: usize = 2;

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
const INSERT_LONDON: &str = r#"main.Music.insert({ id: "london", title: "Live in London" })"#;
const LOOSE_ROW: &str = "{\n    title: \"Live in London\",\n}\n";

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
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
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

fn git(root: &Path, arguments: &[&str]) -> String {
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
    String::from_utf8(output.stdout).expect("git prints UTF-8")
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
}

/// Clones the committed format-1 bundle into `destination`.
fn clone_bundle(destination: &Path) {
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .arg("clone")
        .arg("--quiet")
        .arg(&bundle)
        .arg(destination)
        .output()
        .expect("the git clone process starts");
    assert!(
        output.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// One `orna query --format json` report.
fn query_report(root: &Path, arguments: &[&str]) -> serde_json::Value {
    let mut words = vec!["query"];
    words.extend_from_slice(arguments);
    words.extend_from_slice(&["--format", "json"]);
    let output = orna(root, &words);
    assert_success(&output, "orna query");
    serde_json::from_slice(&output.stdout).expect("orna query prints JSON")
}

fn rows(report: &serde_json::Value) -> u64 {
    report["rows"].as_u64().expect("the report counts rows")
}

#[test]
#[allow(clippy::too_many_lines)]
fn query_and_history_read_format1_rows_at_a_pinned_legacy_commit() {
    let directory = TempDir::new().expect("create the test directory");
    let root = directory.path().join("legacy");
    clone_bundle(&root);
    let head_before = git(&root, &["rev-parse", "HEAD"]);
    let status_before = git(&root, &["status", "--porcelain"]);

    // A compact table is addressed by its storage table id and answered from
    // its authoritative compact rows (1.0.0 `23-storage.md:88-106`).
    let int = query_report(&root, &[COMPACT_INT_TABLE, "--at", "HEAD"]);
    assert_eq!(rows(&int), 4, "Int keys 1..=4\n{int}");
    assert_eq!(int["legacy_source"], "compact", "{int}");
    assert_eq!(int["blobs"].as_u64(), Some(0), "{int}");
    assert_eq!(int["media_payload_bytes_read"].as_u64(), Some(0), "{int}");
    let string = query_report(&root, &[COMPACT_STR_TABLE, "--at", "HEAD"]);
    assert_eq!(rows(&string), 3, "Str keys alpha/beta/gamma\n{string}");
    let date = query_report(&root, &[COMPACT_DATE_TABLE, "--at", "HEAD"]);
    assert_eq!(rows(&date), 3, "Date keys -1/0/1\n{date}");

    // An editable table is addressed by its key-path directory and answered from
    // its committed loose row units (ORNA-ROW-001, ORNA-ROW-002).
    let loose = query_report(&root, &[LOOSE_TABLE, "--at", "HEAD"]);
    assert_eq!(rows(&loose), 2, "two committed row units\n{loose}");
    assert_eq!(loose["legacy_source"], "loose-row-unit", "{loose}");
    // The key a listing prints names the row: it is reconstructed from the path.
    let alice = query_report(
        &root,
        &[LOOSE_TABLE, "--key", "alice-smith", "--at", "HEAD"],
    );
    assert_eq!(rows(&alice), 1, "{alice}");
    // Bounds are the language's logical key order, so they select the rows of
    // the range and nothing else.
    let range = query_report(
        &root,
        &[
            LOOSE_TABLE,
            "--from",
            "text:alice-smith",
            "--until",
            "text:zzz",
            "--at",
            "HEAD",
        ],
    );
    assert_eq!(rows(&range), 2, "{range}");
    let missing = orna(
        &root,
        &["query", LOOSE_TABLE, "--key", "unknown", "--at", "HEAD"],
    );
    assert_eq!(missing.status.code(), Some(1), "{}", transcript(&missing));
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("Row is not committed"),
        "{}",
        transcript(&missing)
    );

    // A loose row's revisions are the commits whose tree carries its unit.
    let history = orna(
        &root,
        &[
            "history",
            LOOSE_TABLE,
            "alice-smith",
            "--at",
            "HEAD",
            "--limit",
            "16",
            "--format",
            "json",
        ],
    );
    assert_success(&history, "orna history");
    let revisions: Vec<serde_json::Value> =
        serde_json::from_slice(&history.stdout).expect("orna history prints JSON");
    assert_eq!(revisions.len(), fixture_commits(&root), "{revisions:?}");
    assert_eq!(
        revisions
            .iter()
            .filter(|revision| revision["present"] == serde_json::Value::Bool(true))
            .count(),
        FIXTURE_PRESENT_REVISIONS,
        "{revisions:?}"
    );
    assert_eq!(revisions[0]["present"], serde_json::Value::Bool(true));
    assert_eq!(
        revisions[revisions.len() - 1]["present"],
        serde_json::Value::Bool(false),
        "the initial commit predates the row\n{revisions:?}"
    );
    let counted = orna(
        &root,
        &[
            "history",
            LOOSE_TABLE,
            "alice-smith",
            "--at",
            "HEAD",
            "--limit",
            "16",
            "--count",
        ],
    );
    assert_success(&counted, "orna history --count");
    assert_eq!(
        String::from_utf8_lossy(&counted.stdout).trim(),
        fixture_commits(&root).to_string()
    );

    // A compact row lives inside committed segments, not in one file, so this
    // walk cannot report its revisions; the command names that limit instead of
    // reporting an empty history.
    let compact_history = orna(&root, &["history", COMPACT_INT_TABLE, "1", "--at", "HEAD"]);
    assert_eq!(
        compact_history.status.code(),
        Some(1),
        "{}",
        transcript(&compact_history)
    );
    assert!(
        String::from_utf8_lossy(&compact_history.stderr)
            .contains("Legacy compact rows have no file history"),
        "{}",
        transcript(&compact_history)
    );

    // Every read above is read-only: the snapshot and the worktree are unchanged.
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(&root, &["status", "--porcelain"]), status_before);
}

/// `orna init` into a fresh directory with a test-local commit identity.
fn initialized_database(directory: &TempDir, source: &str) -> PathBuf {
    let root = directory.path().join("library");
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
    root
}

/// Stages the source with Git and commits it with `orna commit`, returning the
/// relation id `orna commit` reports for the newly admitted table.
fn commit_source(root: &Path, message: &str) -> String {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", message]);
    assert_success(&committed, "orna commit");
    String::from_utf8_lossy(&committed.stdout)
        .lines()
        .find_map(|line| {
            let admitted = line.strip_prefix("admitted table ")?;
            let (table, relation) = admitted.split_once(" as relation ")?;
            assert_eq!(table, "Music");
            Some(relation.to_owned())
        })
        .expect("orna commit reports the admitted Music relation")
}

#[test]
fn query_and_history_refuse_a_format3_snapshot_tracking_a_loose_row() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    let relation = commit_source(&root, "Declare Music");
    assert_success(&orna_repl(&root, INSERT_LONDON), "REPL insert");
    let published = orna(&root, &["publish", "--message", "Add london"]);
    assert_success(&published, "orna publish");
    // The store answers this row before the snapshot also tracks a loose row
    // file, which is the defect: a mixed snapshot was answered from the store.
    assert_eq!(
        rows(&query_report(&root, &[&relation])),
        1,
        "the published row is in the store"
    );

    // Plain Git records a loose row file beside the format-3 store, which
    // ORNA-REPO-004 forbids: rows live only in `.orna/store`.
    fs::create_dir_all(root.join("Music")).expect("create the table directory");
    fs::write(root.join("Music/london.orna"), LOOSE_ROW).expect("write the loose row unit");
    git(&root, &["add", "Music/london.orna"]);
    git(
        &root,
        &["commit", "-m", "record a loose row beside the store"],
    );

    for arguments in [
        vec!["query", relation.as_str(), "--format", "json"],
        vec!["query", relation.as_str(), "--at", "HEAD"],
        vec!["history", relation.as_str(), "london"],
        vec!["history", relation.as_str(), "london", "--at", "HEAD"],
    ] {
        let refused = orna(&root, &arguments);
        assert_eq!(
            refused.status.code(),
            Some(1),
            "orna {arguments:?} must refuse the mixed snapshot\n{}",
            transcript(&refused)
        );
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            stderr.contains("error[ORNA-REPO-CONTEXT-004]"),
            "orna {arguments:?} keeps format admission's code\n{stderr}"
        );
        assert!(
            stderr.contains("Music/london.orna"),
            "orna {arguments:?} names the path that mixes the snapshot\n{stderr}"
        );
        assert!(
            refused.stdout.is_empty(),
            "orna {arguments:?} reports no rows\n{}",
            transcript(&refused)
        );
    }
}
