//! `orna mv` keeps a table's ObjectId in a database the product created
//! (ORNA-OBJECT-003, ORNA-EVOL-001, ORNA-META-001; Gate A "identities
//! survive rename"): `orna init`, `orna commit`, `orna publish` a row,
//! `orna mv Music Archive`, review the source with Git, `orna commit`. The
//! renamed table keeps its table and field IDs in the committed store
//! inventory, so its relation and published rows stay where they were, and
//! tracked `.orna/` never holds anything but `database.orna` and `store`.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use orna_project_v1::{ObjectIdentityManifest, ObjectKind};
use orna_repository_v1::{Repository, Uuid};
use tempfile::TempDir;

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
const ARCHIVE_SOURCE: &str = "pub table Archive(id: Str) { title: Str, }\n";
const INSERT_LONDON: &str = r#"main.Music.insert({ id: "london", title: "Live in London" })"#;
const INSERT_PARIS: &str = r#"main.Archive.insert({ id: "paris", title: "Live in Paris" })"#;

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

/// Stages the reviewed source with Git and commits it with `orna commit`,
/// returning the `admitted table` lines it prints as `(table, relation)`.
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

fn insert_and_publish(root: &Path, insert: &str, message: &str) {
    let inserted = orna_repl(root, insert);
    assert_success(&inserted, "REPL insert");
    assert!(
        !transcript(&inserted).contains("error["),
        "the REPL accepts the row\n{}",
        transcript(&inserted)
    );
    let published = orna(root, &["publish", "--message", message]);
    assert_success(&published, "orna publish");
}

fn query_rows(root: &Path, relation: &str) -> u64 {
    let listed = orna(root, &["query", relation, "--format", "json"]);
    assert_success(&listed, "orna query");
    let report: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("orna query prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

fn tracked_metadata(root: &Path) -> String {
    git(root, &["ls-tree", "--name-only", "HEAD", ".orna/"])
}

fn head_identities(root: &Path) -> ObjectIdentityManifest {
    let repository = Repository::discover(root).expect("discover the repository");
    let head = repository
        .head()
        .expect("read HEAD")
        .expect("a committed HEAD");
    ObjectIdentityManifest::read_committed(&repository, &head).expect("committed identities")
}

fn relation_hex(id: &[u8; 16]) -> String {
    Uuid::from_bytes(*id).simple().to_string()
}

#[test]
fn mv_then_commit_keeps_the_table_object_id_and_its_relation() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    let admitted = commit_source(&root, "Declare Music");
    let [(table, relation)] = admitted.as_slice() else {
        panic!("orna commit admits exactly the Music table: {admitted:?}");
    };
    assert_eq!(table, "Music");
    insert_and_publish(&root, INSERT_LONDON, "Add london");
    assert_eq!(query_rows(&root, relation), 1);
    let before = head_identities(&root);
    let music_id = *before
        .id(ObjectKind::Table, "Music")
        .expect("Music has a committed identity");
    assert_eq!(&relation_hex(&music_id), relation);
    let head_before_mv = git(&root, &["rev-parse", "HEAD"]);

    let renamed = orna(&root, &["mv", "Music", "Archive"]);
    assert_success(&renamed, "orna mv");
    assert!(
        String::from_utf8_lossy(&renamed.stdout).contains("renamed Music to Archive"),
        "{}",
        transcript(&renamed)
    );
    // ORNA-OBJECT-003: the rename is a reviewable change, not a commit. The
    // source edit is left in the worktree and only the inventory is staged;
    // no sidecar is written.
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head_before_mv);
    assert_eq!(
        fs::read_to_string(root.join("main.orna")).unwrap(),
        ARCHIVE_SOURCE
    );
    assert!(!root.join(".orna/object-identities.cbor").exists());
    assert_eq!(git(&root, &["diff", "--name-only"]), "main.orna\n");
    let staged = git(&root, &["diff", "--cached", "--name-only"]);
    assert!(
        !staged.is_empty() && staged.lines().all(|path| path.starts_with(".orna/store/")),
        "mv stages only the store: {staged}"
    );

    let recommitted = commit_source(&root, "Rename Music to Archive");
    assert_eq!(
        recommitted,
        Vec::<(String, String)>::new(),
        "the renamed table keeps its relation, so nothing is newly admitted"
    );
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]).trim(), "3");
    assert_eq!(
        tracked_metadata(&root),
        ".orna/database.orna\n.orna/store\n"
    );
    assert_eq!(git(&root, &["status", "--porcelain"]), "");

    // Table and field IDs moved with the name; the old name is gone.
    let after = head_identities(&root);
    assert_eq!(after.id(ObjectKind::Table, "Archive"), Some(&music_id));
    assert_eq!(after.id(ObjectKind::Table, "Music"), None);
    for field in ["id", "title"] {
        assert_eq!(
            after.id(ObjectKind::Field, &format!("Archive.{field}")),
            before.id(ObjectKind::Field, &format!("Music.{field}")),
            "field {field} keeps its ID"
        );
    }
    assert_eq!(after.records().len(), before.records().len());
    let repository = Repository::discover(&root).unwrap();
    let head = repository.head().unwrap().unwrap();
    assert!(
        repository
            .committed_store_relations(&head)
            .unwrap()
            .contains(&music_id)
    );

    // The published row is still in the same relation, and the renamed
    // table publishes into it.
    assert_eq!(query_rows(&root, relation), 1);
    insert_and_publish(&root, INSERT_PARIS, "Add paris");
    assert_eq!(query_rows(&root, relation), 2);
    assert_eq!(
        tracked_metadata(&root),
        ".orna/database.orna\n.orna/store\n"
    );
}

/// Before the first commit nothing records the table's identity, so `orna
/// mv` refuses with E2101 and changes neither the source nor the staged
/// entries (ORNA-OBJECT-004: no committed identity metadata to tie the
/// rename to). Git may still refresh its own index caches.
#[test]
fn mv_before_the_first_commit_refuses_without_changing_anything() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let staged_before = git(&root, &["ls-files", "--stage"]);

    let refused = orna(&root, &["mv", "Music", "Archive"]);

    assert_eq!(refused.status.code(), Some(1), "{}", transcript(&refused));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("E2101"),
        "{}",
        transcript(&refused)
    );
    assert!(refused.stdout.is_empty());
    assert_eq!(
        fs::read_to_string(root.join("main.orna")).unwrap(),
        MUSIC_SOURCE
    );
    assert_eq!(git(&root, &["ls-files", "--stage"]), staged_before);
    assert!(!root.join(".orna/object-identities.cbor").exists());
}
