//! The first connected path of a database created by the product itself
//! (ORNA-ROW-001, ORNA-META-001, ORNA-META-003, PUB-3): `orna init`, declare a
//! table, stage the source with Git, `orna commit`, insert a row in the REPL,
//! `orna publish`, then read the row back with `orna query`. No fixture
//! builds the store or the identity inventory.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use orna_foundation_v1::{OvbRaw, Value};
use tempfile::TempDir;

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
const NOTE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n";
const INSERT_LONDON: &str = r#"main.Music.insert({ id: "london", title: "Live in London" })"#;

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

/// `orna init` into a fresh directory, a test-local commit identity, and
/// `main.orna` replaced by `source`.
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

/// Stages the source with Git and commits it with `orna commit`, returning
/// the relation id `orna commit` reports for each newly admitted table.
fn commit_source(root: &Path, message: &str) -> Vec<(String, String)> {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", message]);
    assert_success(&committed, "orna commit");
    String::from_utf8_lossy(&committed.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("admitted table "))
        .map(|admitted| {
            let (table, relation) = admitted
                .split_once(" as relation ")
                .expect("an admitted table names its relation");
            (table.to_owned(), relation.to_owned())
        })
        .collect()
}

fn query_rows(root: &Path, arguments: &[&str]) -> u64 {
    let listed = orna(root, arguments);
    assert_success(&listed, "orna query");
    let report: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("orna query prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

fn tracked_metadata(root: &Path) -> String {
    git(root, &["ls-tree", "--name-only", "HEAD", ".orna/"])
}

#[test]
fn init_commit_repl_publish_and_query_through_product_commands() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    // Initialization neither stages nor commits (ORNA-META-003), so it
    // writes no store; the first commit creates it.
    assert!(!root.join(".orna/store").exists());
    assert!(!root.join(".orna/object-identities.cbor").exists());

    let admitted = commit_source(&root, "Declare Music");
    let [(table, relation)] = admitted.as_slice() else {
        panic!("orna commit admits exactly the Music table: {admitted:?}");
    };
    assert_eq!(table, "Music");
    assert_eq!(relation.len(), 32, "a relation id is 32 hex digits");
    // The first commit is a root commit on the initial branch, and tracked
    // `.orna/` holds only the metadata and the store (ORNA-META-001).
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(
        tracked_metadata(&root),
        ".orna/database.orna\n.orna/store\n"
    );
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
    // The table's relation exists and holds no rows yet.
    assert_eq!(
        query_rows(&root, &["query", relation, "--format", "json"]),
        0
    );

    let inserted = orna_repl(&root, INSERT_LONDON);
    assert_success(&inserted, "REPL insert");
    assert!(
        !transcript(&inserted).contains("error["),
        "the REPL accepts the row\n{}",
        transcript(&inserted)
    );

    let published = orna(&root, &["publish", "--message", "Add london"]);
    assert_success(&published, "orna publish");
    let report = String::from_utf8_lossy(&published.stdout);
    assert!(
        report.starts_with("published "),
        "{}",
        transcript(&published)
    );
    assert!(
        report
            .lines()
            .any(|line| line.contains("Music") && line.trim_end().ends_with("1 rows")),
        "{}",
        transcript(&published)
    );
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]).trim(), "2");
    assert_eq!(
        tracked_metadata(&root),
        ".orna/database.orna\n.orna/store\n"
    );

    assert_eq!(
        query_rows(&root, &["query", relation, "--format", "json"]),
        1
    );
    assert_eq!(
        query_rows(
            &root,
            &["query", relation, "--key", "london", "--format", "json"]
        ),
        1
    );
}

/// A store that knows a table's id but has no relation for it: publication
/// never creates relations (PUB-3 step 4 binds a table publication to its
/// captured root and schema), so it refuses the row with the remedy and
/// writes nothing.
#[test]
fn publish_refuses_a_row_whose_table_the_committed_store_lacks() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, NOTE_SOURCE);
    commit_source(&root, "Declare Note");
    let store_without_music = git(&root, &["rev-parse", "HEAD:.orna/store"])
        .trim()
        .to_owned();
    fs::write(
        root.join("main.orna"),
        format!("{NOTE_SOURCE}{MUSIC_SOURCE}"),
    )
    .unwrap();
    let admitted = commit_source(&root, "Declare Music");
    assert_eq!(admitted.len(), 1, "only Music is new: {admitted:?}");
    assert_success(&orna_repl(&root, INSERT_LONDON), "REPL insert");

    // With plain Git, commit the same source over the first store, which has
    // no relation for Music.
    let orna_entries = git(&root, &["ls-tree", "HEAD", ".orna/"])
        .lines()
        .map(|entry| {
            if entry.ends_with("\t.orna/store") {
                format!("040000 tree {store_without_music}\tstore\n")
            } else {
                let (header, path) = entry.split_once('\t').expect("a tree entry");
                format!("{header}\t{}\n", path.trim_start_matches(".orna/"))
            }
        })
        .collect::<String>();
    let orna_tree = git_with_input(&root, &["mktree"], &orna_entries);
    let root_entries = git(&root, &["ls-tree", "HEAD"])
        .lines()
        .map(|entry| {
            if entry.ends_with("\t.orna") {
                format!("040000 tree {}\t.orna\n", orna_tree.trim())
            } else {
                format!("{entry}\n")
            }
        })
        .collect::<String>();
    let tree = git_with_input(&root, &["mktree"], &root_entries);
    let previous = git(&root, &["rev-parse", "HEAD"]).trim().to_owned();
    let regressed = git(
        &root,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            &previous,
            "-m",
            "store without Music",
        ],
    )
    .trim()
    .to_owned();
    git(&root, &["update-ref", "HEAD", &regressed, &previous]);
    let index_before = git(&root, &["ls-files", "--stage"]);

    let refused = orna(&root, &["publish"]);
    assert_ne!(refused.status.code(), Some(0), "{}", transcript(&refused));
    let message = transcript(&refused);
    assert!(
        message.contains("error[E2300]")
            && message.contains("source row table has no relation in the committed store")
            && message.contains("orna commit"),
        "{message}"
    );
    // Nothing was published and the pending row is kept.
    assert_eq!(git(&root, &["rev-parse", "HEAD"]).trim(), regressed);
    assert_eq!(git(&root, &["ls-files", "--stage"]), index_before);
    let status = orna(&root, &["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("1 runtime mutation"),
        "{}",
        transcript(&status)
    );
}

/// After the first `orna commit` a product-made database is a complete
/// format-3 snapshot: `orna export` writes and verifies a complete copy, and
/// restoring it yields the same commit and source (W09-11).
#[test]
fn the_first_commit_exports_and_restores_a_complete_copy() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    commit_source(&root, "Declare Music");
    let head = git(&root, &["rev-parse", "HEAD"]).trim().to_owned();
    let archive = directory.path().join("archive");
    let archive = archive.to_str().expect("UTF-8 test path");

    let exported = orna(&root, &["export", archive, "--at", "HEAD"]);
    assert_success(&exported, "orna export");
    assert!(
        String::from_utf8_lossy(&exported.stdout).starts_with(&format!("exported {head} ")),
        "{}",
        transcript(&exported)
    );
    let checked = orna(&root, &["export", archive, "--check"]);
    assert_success(&checked, "orna export --check");
    assert!(
        String::from_utf8_lossy(&checked.stdout).starts_with(&format!("complete copy of {head} ")),
        "{}",
        transcript(&checked)
    );

    let restored = directory.path().join("restored");
    let restore = orna(
        directory.path(),
        &[
            "export",
            archive,
            "--restore",
            restored.to_str().expect("UTF-8 test path"),
            "--worktree",
        ],
    );
    assert_success(&restore, "orna export --restore");
    assert_eq!(git(&restored, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(
        fs::read_to_string(restored.join("main.orna")).expect("restored source"),
        MUSIC_SOURCE
    );
}

/// A commit that fails after staging admitted its tables committed nothing,
/// so the staged relations bind no rows: the user may drop one table and
/// change another, and the next `orna commit` admits exactly the edited
/// source (ORNA-ADMIN-003). `admitted table` is printed only for a commit
/// that exists.
#[test]
fn a_failed_first_commit_leaves_its_tables_free_to_change() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, &format!("{MUSIC_SOURCE}{NOTE_SOURCE}"));
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    // The local runtime cannot be opened, so the commit fails after staging.
    let runtime_state = root.join(".git/orna/state.db");
    fs::create_dir_all(&runtime_state).expect("block the runtime state file");
    let failed = orna(&root, &["commit", "-m", "Declare tables"]);
    assert_ne!(failed.status.code(), Some(0), "{}", transcript(&failed));
    assert!(
        transcript(&failed).contains("local runtime could not be opened")
            && !transcript(&failed).contains("admitted table"),
        "{}",
        transcript(&failed)
    );
    assert!(
        git_fails(&root, &["rev-parse", "--verify", "--quiet", "HEAD"]),
        "the branch is still unborn"
    );

    fs::remove_dir(&runtime_state).expect("unblock the runtime state file");
    fs::write(
        root.join("main.orna"),
        "pub table Music(id: Str) { title: Int, }\n",
    )
    .expect("edit main.orna");
    let admitted = commit_source(&root, "Declare Music");
    let [(table, relation)] = admitted.as_slice() else {
        panic!("orna commit admits exactly the edited Music table: {admitted:?}");
    };
    assert_eq!(table, "Music");
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(
        query_rows(&root, &["query", relation, "--format", "json"]),
        0
    );
}

/// A repository that still tracks the pre-store identity sidecar (version 1,
/// kinds under their old internal codes) keeps the table identity it recorded:
/// the first commit reads code 1 as a table and admits it under the recorded UUID.
#[test]
fn a_legacy_identity_sidecar_keeps_its_table_id_through_the_first_commit() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    let sidecar = Value::new(OvbRaw::Array(vec![
        OvbRaw::Int(1u64.into()),
        OvbRaw::Array(vec![OvbRaw::Array(vec![
            OvbRaw::Int(1u64.into()),
            OvbRaw::Text("Music".into()),
            OvbRaw::Bytes(vec![7; 16]),
            OvbRaw::Bool(false),
        ])]),
    ]))
    .and_then(|value| value.encode())
    .expect("encode the legacy sidecar");
    fs::write(root.join(".orna/object-identities.cbor"), sidecar)
        .expect("write the legacy sidecar");
    git(
        &root,
        &[
            "add",
            "main.orna",
            ".orna/database.orna",
            ".orna/object-identities.cbor",
        ],
    );
    let committed = orna(&root, &["commit", "-m", "first commit"]);
    assert_success(&committed, "orna commit");
    let admitted: Vec<_> = String::from_utf8_lossy(&committed.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("admitted table "))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        admitted,
        vec![format!("Music as relation {}", "07".repeat(16))]
    );
}

fn git_fails(root: &Path, arguments: &[&str]) -> bool {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    !command
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts")
        .status
        .success()
}

fn git_with_input(root: &Path, arguments: &[&str], input: &str) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let mut child = command
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the git process starts");
    child
        .stdin
        .take()
        .expect("git stdin")
        .write_all(input.as_bytes())
        .expect("write git input");
    let output = child.wait_with_output().expect("the git process exits");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git prints UTF-8")
}
