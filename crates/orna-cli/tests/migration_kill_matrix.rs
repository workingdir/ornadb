//! Gate F: idempotent carry, deterministic staging and the migration kill matrix.
//!
//! `orna migrate copy` (Gate F, `implementation/ACCEPTANCE.md:61-68`; UPGRADE-3
//! items 3-7 and ORNA-UPGRADE-002/004/005/006 in
//! `source/40-upgrade-contract.md`) builds one explicit format-3 candidate from
//! a legacy snapshot and publishes it as a disposable copy. This file asserts
//! three properties of that path against the real executable, and kills the
//! process at every durable boundary in between:
//!
//! 1. **Idempotent carry** — a second `migrate copy` against the same
//!    destination changes nothing: the same candidate commit, index, journal
//!    and carried checkpoint rows, and byte-identical standard output. The
//!    checkpoint the source acknowledged before the migration (version 2 of a
//!    three-item stream) is present exactly once and resumes at item 3.
//! 2. **Deterministic staging** — two independent preparations of one source
//!    produce the same committed bytes. The candidate commit is a synthetic
//!    record of one transition, so its time comes from the converted snapshot
//!    (`source_commit_time`) and not from the process clock; without that the
//!    same tree and message would still record two different commits.
//! 3. **Crash matrix** — a child is `SIGKILL`ed at each boundary below, then the
//!    same command is re-run from scratch. The recovered copy must equal the
//!    clean one byte for byte, the source must be untouched, and no carried
//!    checkpoint may be lost or duplicated.
//!
//! Boundaries, in the order the command crosses them:
//!
//! | Marker | Where it parks |
//! | --- | --- |
//! | `migration-staging` | staged graph, worktree and index written; source re-checked |
//! | `migration-candidate` | candidate commit object built; nothing pinned or journalled |
//! | `migration-journal` | staged copy holds its prepared publication journal |
//! | `migration-synced` | every staged byte fsynced; copy still under its staging name |
//! | `migration-published` | copy installed under its final name; carry not imported |
//! | `migration-carried` | carry committed in the destination runtime; not yet reported |
//! | `migration-discard-renamed` | `migrate abort` renamed the copy to its quarantine |
//!
//! Not asserted here, and why:
//!
//! - The publication journal's own `rename` in `write_publication_journal` is
//!   not a separate boundary. It is atomic in `orna-repository`, covered by the
//!   Gate D publication matrix, and a kill before it leaves a staging directory
//!   that the next attempt never reads.
//! - A source with no recorded `.orna/database.orna` identity is not covered.
//!   `prepare_format3_migration_in` mints a fresh random identity for it
//!   (`DatabaseId::new_v4`), so two preparations of that input cannot produce
//!   the same bytes; determinism here is a property of a source that records
//!   its identity, which the golden bundle does.
//! - A kill before publication leaves one `.orna-migration-copy-<uuid>` staging
//!   directory behind, and no later run reclaims it (its name is random and
//!   unowned). The matrix asserts the leftover is a sibling staging directory
//!   that was never published, not that it is cleaned up.

// The runtime's stream API returns large futures; each test awaits one at a
// time, so boxing them here would only move the state to the heap.
#![allow(clippy::large_futures)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use orna_repository::Repository;
use orna_runtime::{
    CheckpointKey, Component, ConsumerIdentity, ListStreamSource, RuntimeIdentity, RuntimeState,
    StreamCheckpoint, StreamHandler, StreamHandlerResult, StreamItem, StreamMutationBatch,
    WriterLease, runtime_identity_for_database_id,
};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const BUNDLE: &str = "tests/fixtures/legacy-format1.bundle";
const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
const CRASH_DIR: &str = "ORNA_TEST_PUBLICATION_CRASH_DIR";
const MARKER_FILE: &str = "publication-crash-marker";
/// The `migrate copy` boundaries this matrix kills at, in command order.
const COPY_BOUNDARIES: [&str; 6] = [
    "migration-staging",
    "migration-candidate",
    "migration-journal",
    "migration-synced",
    "migration-published",
    "migration-carried",
];
/// Boundaries after which the destination already exists under its final name.
const PUBLISHED_BOUNDARIES: [&str; 2] = ["migration-published", "migration-carried"];

fn git(directory: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_bytes(directory, arguments))
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn git_bytes(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} in {}: {}",
        directory.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_succeeds(directory: &Path, arguments: &[&str]) -> bool {
    Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap()
        .status
        .success()
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in &digest {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

fn read_sha256(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|bytes| sha256(&bytes))
}

/// Byte copy of a repository directory, `.git` included, so every boundary
/// starts from the same commit graph, index and runtime carry.
fn duplicate(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from).unwrap();
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&from).unwrap(), &to).unwrap();
        } else if file_type.is_dir() {
            duplicate(&from, &to);
        } else if file_type.is_file() {
            fs::copy(&from, &to).unwrap();
        }
    }
}

/// Path-sorted digest of every worktree file, `.git` excluded.
///
/// `.git` is excluded because the runtime directory inside it holds `state.db`,
/// which `SQLite` may touch on open without changing the repository content this
/// function is checking.
fn worktree_manifest(root: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, directory: &Path, entries: &mut Vec<(String, String)>) {
        let mut children = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if relative == ".git" {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_dir() {
                entries.push((relative, "<dir>".to_owned()));
                walk(root, &path, entries);
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                entries.push((relative, format!("-> {}", target.display())));
            } else {
                entries.push((relative, sha256(&fs::read(&path).unwrap())));
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries
}

/// Everything the migration commits, or causes to be committed, in a copy.
///
/// Object ids are hashes of their content, so the ordered `rev-list
/// --objects --all` listing fixes the whole graph; the per-object content
/// comparison then names the first object whose bytes differ instead of losing
/// that to a hash comparison. The index is compared as raw bytes because the
/// migration installs the source's exact index, and the journal as raw bytes
/// because it records the candidate head, coordinates and managed entries.
#[derive(Debug, Eq, PartialEq)]
struct CommittedCopy {
    head: String,
    index_sha256: Option<String>,
    journal_sha256: Option<String>,
    refs: Vec<String>,
    objects: Vec<(String, Vec<u8>)>,
    worktree: Vec<(String, String)>,
}

fn committed_copy(root: &Path) -> CommittedCopy {
    let graph = git(root, &["rev-list", "--objects", "--all"]);
    let mut objects = Vec::new();
    for line in graph.lines() {
        let object = line.split(' ').next().expect("rev-list names an object");
        objects.push((
            line.to_owned(),
            git_bytes(root, &["cat-file", "-p", object]),
        ));
    }
    CommittedCopy {
        head: git(root, &["rev-parse", "HEAD"]),
        index_sha256: read_sha256(&root.join(".git/index")),
        journal_sha256: read_sha256(&root.join(".git/orna/publication-journal.bin")),
        refs: git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .map(str::to_owned)
            .collect(),
        objects,
        worktree: worktree_manifest(root),
    }
}

/// The prepared candidate's format-transition record, read from the commit
/// itself rather than from the journal that names it.
struct CandidateRecord {
    commit: String,
    tree: String,
    database: Vec<u8>,
    record: Vec<u8>,
    carries_legacy_format: bool,
}

fn candidate_record(root: &Path) -> CandidateRecord {
    let candidates = git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:strip=3)",
            "refs/orna/migration-candidates/",
        ],
    );
    let mut lines = candidates.lines();
    let commit = lines
        .next()
        .expect("the copy pins exactly one migration candidate")
        .to_owned();
    assert!(lines.next().is_none(), "one candidate per prepared copy");
    assert!(
        git_succeeds(root, &["cat-file", "-e", &format!("{commit}^{{commit}}")],),
        "the pinned candidate is a commit"
    );
    let tree = git(root, &["rev-parse", &format!("{commit}^{{tree}}")]);
    CandidateRecord {
        database: git_bytes(root, &["show", &format!("{commit}:.orna/database.orna")]),
        record: git_bytes(root, &["show", &format!("{commit}:.orna/migration.json")]),
        carries_legacy_format: git_succeeds(
            root,
            &["cat-file", "-e", &format!("{commit}:.orna/format.orna")],
        ),
        commit,
        tree,
    }
}

/// What the destination runtime actually holds for the carried stream state.
///
/// Read with SQL so a duplicate row is visible as an extra row: the
/// destination's API reads one checkpoint per key and would hide it.
#[derive(Debug, Default, Eq, PartialEq)]
struct CarryRows {
    /// `(key_id, version, committed_position)` per `stream_checkpoint` row.
    checkpoints: Vec<(String, i64, Option<String>)>,
    bridges: i64,
    archive: i64,
    history: i64,
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn table_count(connection: &libsql::Connection, table: &str) -> i64 {
    let mut exists = connection
        .query(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            libsql::params![table],
        )
        .await
        .unwrap();
    let present = exists
        .next()
        .await
        .unwrap()
        .expect("one row")
        .get::<i64>(0)
        .unwrap();
    if present == 0 {
        return 0;
    }
    let mut rows = connection
        .query(&format!("SELECT count(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .expect("one row")
        .get::<i64>(0)
        .unwrap()
}

fn carry_rows(state_db: &Path) -> CarryRows {
    if !state_db.is_file() {
        return CarryRows::default();
    }
    runtime().block_on(async {
        let database = libsql::Builder::new_local(state_db).build().await.unwrap();
        let connection = database.connect().unwrap();
        let mut checkpoints = Vec::new();
        let mut rows = connection
            .query(
                "SELECT key_id, version, committed_position FROM stream_checkpoint ORDER BY key_id",
                (),
            )
            .await
            .unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            checkpoints.push((
                row.get::<String>(0).unwrap(),
                row.get::<i64>(1).unwrap(),
                row.get::<Option<String>>(2).unwrap(),
            ));
        }
        CarryRows {
            checkpoints,
            bridges: table_count(&connection, "stream_checkpoint_migration_bridge").await,
            archive: table_count(&connection, "stream_checkpoint_migration_archive").await,
            history: table_count(&connection, "stream_checkpoint_history").await,
        }
    })
}

/// The carried rows of a copy. A destination a killed run never published holds
/// none: there is no worktree to open and no runtime state to read.
fn destination_carry(root: &Path) -> CarryRows {
    if !root.is_dir() {
        return CarryRows::default();
    }
    carry_rows(
        &Repository::discover(root)
            .unwrap()
            .runtime_paths()
            .state_db(),
    )
}

fn source_carry(root: &Path) -> CarryRows {
    carry_rows(
        &Repository::discover(root)
            .unwrap()
            .runtime_paths()
            .state_db(),
    )
}

/// The legacy source has no migration continuity yet, so its runtime identity is
/// derived from the database id in its own `.orna/database.orna` sidecar.
fn source_runtime_identity(root: &Path) -> (RuntimeIdentity, [u8; 32]) {
    let sidecar = fs::read_to_string(root.join(".orna/database.orna")).unwrap();
    let uuid = sidecar.split('"').nth(1).expect("quoted database id");
    let hex: String = uuid.chars().filter(|character| *character != '-').collect();
    assert_eq!(hex.len(), 32, "database id is a 128-bit uuid");
    let mut database_id = [0_u8; 16];
    for (index, byte) in database_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    runtime_identity_for_database_id(database_id)
}

fn checkpoint_key() -> CheckpointKey {
    CheckpointKey {
        consumer: ConsumerIdentity {
            principal: Component::new("principal").unwrap(),
            root: Component::new("root").unwrap(),
            function: Component::new("consume").unwrap(),
            binding: Component::new("binding").unwrap(),
        },
        source_format: Component::new("source-format").unwrap(),
        source: Component::new("source").unwrap(),
        partition_format: Component::new("partition-format").unwrap(),
        partition: Some(Component::new("partition").unwrap()),
        position_format: Component::new("position-format").unwrap(),
    }
}

struct CommitEach {
    seen: Vec<Vec<u8>>,
    next_digest: [u8; 32],
}

impl StreamHandler for CommitEach {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult {
        self.seen.push(item.payload.clone());
        StreamHandlerResult::Commit(StreamMutationBatch {
            mutations: Vec::new(),
            next_digest: self.next_digest,
        })
    }
}

/// Gives the legacy source a durable persistent consumer that has committed two
/// of three items. Version 2 is the row the migration must carry exactly once.
fn give_source_a_persistent_consumer(root: &Path) -> StreamCheckpoint {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) = source_runtime_identity(root);
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open source runtime");
        let writer: WriterLease = state.acquire_lease([44; 16]).await.unwrap();
        let key = checkpoint_key();
        let mut source = ListStreamSource::new(
            key.clone(),
            vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()],
        );
        for expected in [b"1".as_slice(), b"2".as_slice()] {
            let mut handler = CommitEach {
                seen: Vec::new(),
                next_digest: state.capture().await.unwrap().generation_digest(),
            };
            state
                .run_stream_once(writer, &key, &mut source, &mut handler)
                .await
                .expect("consumer delivery commits");
            assert_eq!(handler.seen, vec![expected.to_vec()]);
        }
        state.stream_checkpoint(&key).await.unwrap()
    })
}

/// A legacy source at one commit whose stream has acknowledged two of three
/// items. Every boundary run starts from a byte copy of this repository.
fn pristine_source(parent: &Path) -> PathBuf {
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join(BUNDLE);
    let source = parent.join("pristine");
    let clone = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("clone")
        .arg("--quiet")
        .arg(&bundle)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "git clone bundle: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let checkpoint = give_source_a_persistent_consumer(&source);
    assert_eq!(checkpoint.version, 2, "two acknowledged deliveries");
    source
}

fn cli_command(source: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["--db", source.to_str().unwrap(), "migrate", "copy"]);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(name);
    }
    command
}

fn run_copy(source: &Path, destination: &Path) -> Output {
    let mut command = cli_command(source);
    command
        .arg(destination)
        .args(["--annotation-defaults", "0", "--no-persistent-consumers"])
        .output()
        .expect("migration command starts")
}

fn run_abort(source: &Path, destination: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["--db", source.to_str().unwrap(), "migrate", "abort"])
        .arg(destination)
        .output()
        .expect("migration abort starts")
}

/// A crash child that is `SIGKILL`ed and reaped when dropped, so a failed
/// assertion mid-matrix never leaks a parked migration child.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl ChildGuard {
    fn kill_and_reap(&mut self) {
        self.0.kill().expect("SIGKILL migration child");
        let status = self.0.wait().expect("wait for the killed child");
        assert!(
            !status.success(),
            "the child must not have returned from the boundary"
        );
    }
}

/// Runs `migrate {arguments}` until it parks at `boundary` and kills it there,
/// returning once the process is reaped.
fn kill_at(source: &Path, arguments: &[&std::ffi::OsStr], boundary: &str, crash_dir: &Path) {
    let marker = crash_dir.join(MARKER_FILE);
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env(CRASH_AT, boundary)
        .env(CRASH_DIR, crash_dir)
        .args(["--db", source.to_str().unwrap(), "migrate"]);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(name);
    }
    command.args(arguments);
    let mut child = ChildGuard(command.spawn().expect("migration child starts"));
    let deadline = Instant::now() + Duration::from_mins(1);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "child never reached {boundary}; status: {:?}",
        child.0.try_wait().unwrap()
    );
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        boundary,
        "the child parked at a different boundary"
    );
    child.kill_and_reap();
}

/// The sibling directories a migration may stage into, next to `destination`.
fn staging_siblings(destination: &Path) -> Vec<PathBuf> {
    let parent = destination.parent().unwrap();
    let mut siblings = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".orna-migration-copy-"))
        })
        .collect::<Vec<_>>();
    siblings.sort();
    siblings
}

/// Asserts the copy opens and still holds the prepared migration record.
fn assert_prepared_copy_opens(destination: &Path, expected_head: &str) {
    let repository = Repository::discover(destination).expect("the copy opens");
    assert_eq!(
        repository.head().unwrap().unwrap().as_str(),
        expected_head,
        "the copy's HEAD is still the legacy snapshot it converts"
    );
    assert!(
        repository.holds_prepared_migration(),
        "the copy still reports its prepared migration"
    );
}

/// The one-line summary `migrate copy` prints on success.
fn expected_summary(stdout: &[u8], destination: &Path) -> String {
    let text = String::from_utf8(stdout.to_vec()).unwrap();
    text.replace(&destination.to_string_lossy().into_owned(), "<destination>")
}

#[test]
#[allow(clippy::too_many_lines)]
fn migration_copy_of_the_golden_bundle_is_idempotent() {
    let parent = TempDir::new().unwrap();
    let source = pristine_source(parent.path());
    let source_before = worktree_manifest(&source);
    let source_head = git(&source, &["rev-parse", "HEAD"]);
    let source_index = read_sha256(&source.join(".git/index"));
    let source_carry_before = source_carry(&source);
    assert_eq!(
        source_carry_before.checkpoints.len(),
        1,
        "one acknowledged checkpoint to carry"
    );
    let destination = parent.path().join("migrated");

    let first = run_copy(&source, &destination);
    assert!(
        first.status.success(),
        "first migration: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stderr.is_empty());
    let first_summary = expected_summary(&first.stdout, &destination);
    assert!(
        first_summary.contains("carried_checkpoints=1"),
        "the acknowledged checkpoint is reported as carried: {first_summary}"
    );
    let first_copy = committed_copy(&destination);
    let first_record = candidate_record(&destination);
    let first_carry = destination_carry(&destination);
    assert_prepared_copy_opens(&destination, &source_head);

    // The candidate is the explicit transition: it replaces the legacy format
    // marker with the format-3 store record and the migration record, and it is
    // a child of the unchanged legacy commit (ORNA-UPGRADE-002, UPGRADE-3.5).
    assert!(
        !first_record.carries_legacy_format,
        "the candidate must not keep the legacy format marker"
    );
    assert_eq!(
        git(
            &destination,
            &["show", "-s", "--format=%P", &first_record.commit]
        ),
        source_head,
        "the candidate's parent is the legacy snapshot"
    );
    assert!(
        String::from_utf8_lossy(&first_record.record).contains("source_format: 1"),
        "the migration record names the source format"
    );
    assert!(
        String::from_utf8_lossy(&first_record.record).contains("payload_inspected: false"),
        "the annotation default is metadata-only (ORNA-UPGRADE-004)"
    );
    // The recovered identity is the legacy sidecar's, recorded in the
    // canonical format-3 database record (UPGRADE-3.4): every runtime identity
    // the carry depends on is derived from it.
    let legacy_database = fs::read_to_string(source.join(".orna/database.orna")).unwrap();
    let legacy_id = legacy_database
        .split('"')
        .nth(1)
        .expect("quoted database id");
    let candidate_database = String::from_utf8(first_record.database.clone()).unwrap();
    assert!(
        candidate_database.contains("repository_format: 3"),
        "the candidate records the format-3 coordinate: {candidate_database}"
    );
    assert!(
        candidate_database.contains(&format!("database_id: \"{legacy_id}\"")),
        "the candidate keeps the legacy database identity: {candidate_database}"
    );
    // One checkpoint, one bridge origin, every source history row archived, and
    // nothing re-inserted into the live history table.
    assert_eq!(first_carry.checkpoints.len(), 1);
    assert_eq!(first_carry.checkpoints[0].1, 2, "the acknowledged version");
    assert_eq!(first_carry.bridges, 1);
    assert_eq!(first_carry.archive, source_carry_before.history);
    assert_eq!(first_carry.history, 0);
    assert_eq!(
        first_carry.archive, source_carry_before.history,
        "every acknowledged history row is archived once"
    );

    // Second run: the destination already exists, so the command recovers it.
    let second = run_copy(&source, &destination);
    assert!(
        second.status.success(),
        "second migration: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(second.stderr.is_empty());
    assert_eq!(
        expected_summary(&second.stdout, &destination),
        first_summary,
        "a second run reports the same prepared copy"
    );
    assert_eq!(
        committed_copy(&destination),
        first_copy,
        "a second run must not change one committed byte"
    );
    assert_eq!(candidate_record(&destination).commit, first_record.commit);
    assert_eq!(candidate_record(&destination).tree, first_record.tree);
    assert_eq!(
        destination_carry(&destination),
        first_carry,
        "a second run must not lose or duplicate a carried row"
    );
    assert!(
        staging_siblings(&destination).is_empty(),
        "a completed migration leaves no staging directory behind"
    );

    // The source is untouched (ORNA-UPGRADE-006).
    assert_eq!(worktree_manifest(&source), source_before);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(read_sha256(&source.join(".git/index")), source_index);
    assert_eq!(source_carry(&source), source_carry_before);

    // The carried checkpoint is usable: delivery resumes at the third item.
    assert_eq!(deliver_from_destination(&destination), vec![b"3".to_vec()]);
}

fn deliver_from_destination(root: &Path) -> Vec<Vec<u8>> {
    runtime().block_on(async {
        let repository = Repository::discover(root).unwrap();
        let (identity, digest) =
            orna_runtime::migration_copy_runtime_identity(&repository).unwrap();
        let state = RuntimeState::open(&repository, identity, digest)
            .await
            .expect("open destination runtime");
        let writer = state.acquire_lease([45; 16]).await.unwrap();
        let key = checkpoint_key();
        let mut source = ListStreamSource::new(
            key.clone(),
            vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()],
        );
        let mut handler = CommitEach {
            seen: Vec::new(),
            next_digest: state.capture().await.unwrap().generation_digest(),
        };
        state
            .run_stream_once(writer, &key, &mut source, &mut handler)
            .await
            .expect("resumed delivery commits");
        handler.seen
    })
}

#[test]
fn two_independent_migrations_of_one_source_produce_identical_bytes() {
    let parent = TempDir::new().unwrap();
    let source = pristine_source(parent.path());
    let first = parent.path().join("first");
    let second = parent.path().join("second");
    for destination in [&first, &second] {
        let output = run_copy(&source, destination);
        assert!(
            output.status.success(),
            "migration: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert_eq!(
        candidate_record(&first).commit,
        candidate_record(&second).commit,
        "the candidate commit must be a function of its inputs, not of the clock"
    );
    assert_eq!(
        candidate_record(&first).tree,
        candidate_record(&second).tree
    );
    assert_eq!(
        committed_copy(&first),
        committed_copy(&second),
        "two independent migrations of one source must commit the same bytes"
    );
    assert_eq!(
        destination_carry(&first),
        destination_carry(&second),
        "two independent migrations must carry the same rows"
    );
    let first_summary = expected_summary(&run_copy(&source, &first).stdout, &first);

    // The recorded commit time is the converted snapshot's, so the commit is
    // reproducible from the legacy input alone.
    let expected = git(&source, &["log", "-1", "--format=%ct"]);
    assert_eq!(
        git(
            &first,
            &[
                "show",
                "-s",
                "--format=%at %ct",
                &candidate_record(&first).commit
            ]
        ),
        format!("{expected} {expected}"),
        "the candidate records the converted snapshot's time"
    );
    assert!(first_summary.contains("carried_checkpoints=1"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn migration_kill_at_each_boundary_recovers_the_same_copy() {
    let parent = TempDir::new().unwrap();
    let pristine = pristine_source(parent.path());
    let source_head = git(&pristine, &["rev-parse", "HEAD"]);
    let pristine_worktree = worktree_manifest(&pristine);
    let pristine_index = read_sha256(&pristine.join(".git/index"));
    let pristine_carry = source_carry(&pristine);

    // The reference: one uninterrupted run, from a byte copy of the same input.
    let reference_source = parent.path().join("reference-source");
    duplicate(&pristine, &reference_source);
    let reference_destination = parent.path().join("reference-destination");
    let reference_run = run_copy(&reference_source, &reference_destination);
    assert!(
        reference_run.status.success(),
        "reference migration: {}",
        String::from_utf8_lossy(&reference_run.stderr)
    );
    let reference_summary = expected_summary(&reference_run.stdout, &reference_destination);
    let reference_copy = committed_copy(&reference_destination);
    let reference_record = candidate_record(&reference_destination);
    let reference_carry = destination_carry(&reference_destination);
    assert_eq!(reference_carry.checkpoints.len(), 1);

    for boundary in COPY_BOUNDARIES {
        let case = TempDir::new().unwrap();
        let source = case.path().join("source");
        duplicate(&pristine, &source);
        let destination = case.path().join("migrated");
        let crash = case.path().join("crash");
        fs::create_dir(&crash).unwrap();

        let arguments = [
            std::ffi::OsStr::new("copy"),
            destination.as_os_str(),
            std::ffi::OsStr::new("--annotation-defaults"),
            std::ffi::OsStr::new("0"),
            std::ffi::OsStr::new("--no-persistent-consumers"),
        ];
        kill_at(&source, &arguments, boundary, &crash);

        // The source is untouched by a killed migration (ORNA-UPGRADE-006).
        assert_eq!(
            git(&source, &["rev-parse", "HEAD"]),
            source_head,
            "{boundary}"
        );
        assert_eq!(worktree_manifest(&source), pristine_worktree, "{boundary}");
        assert_eq!(
            read_sha256(&source.join(".git/index")),
            pristine_index,
            "{boundary}: the source index"
        );
        assert_eq!(source_carry(&source), pristine_carry, "{boundary}");

        let published = PUBLISHED_BOUNDARIES.contains(&boundary);
        assert_eq!(
            destination.exists(),
            published,
            "{boundary}: the destination's presence is the publish boundary"
        );
        if published {
            assert_prepared_copy_opens(&destination, &source_head);
            assert_eq!(
                candidate_record(&destination).commit,
                reference_record.commit,
                "{boundary}: the killed run had already built the reference candidate"
            );
        }
        let pre_restart_carry = destination_carry(&destination);
        if boundary == "migration-carried" {
            assert_eq!(
                pre_restart_carry, reference_carry,
                "the carry is already committed at this boundary"
            );
        } else {
            assert_eq!(
                pre_restart_carry,
                CarryRows::default(),
                "{boundary}: no carried row may exist before the import"
            );
        }
        // A pre-publication kill leaves exactly one staging sibling that was
        // never renamed into place; a later run does not reclaim it (its name is
        // random and unowned), so the assertion is that it is a private sibling
        // directory, never the destination.
        let staged = staging_siblings(&destination);
        if published {
            assert!(
                staged.is_empty(),
                "{boundary}: the rename consumed the staging directory"
            );
        } else {
            assert_eq!(
                staged.len(),
                1,
                "{boundary}: the killed run's staging directory is left behind"
            );
            assert_ne!(staged[0], destination);
            assert!(
                staged[0].is_dir(),
                "{boundary}: the leftover is a directory"
            );
        }

        // Restart from scratch: the same command must converge on the copy the
        // uninterrupted run produced.
        let recovered = run_copy(&source, &destination);
        assert!(
            recovered.status.success(),
            "{boundary} restart: {}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert!(
            recovered.stderr.is_empty(),
            "{boundary} restart: {}",
            String::from_utf8_lossy(&recovered.stderr)
        );
        assert_eq!(
            expected_summary(&recovered.stdout, &destination),
            reference_summary,
            "{boundary}: the restarted command reports the same copy"
        );
        assert_prepared_copy_opens(&destination, &source_head);
        assert_eq!(
            committed_copy(&destination),
            reference_copy,
            "{boundary}: the recovered copy must equal the clean one byte for byte"
        );
        assert_eq!(
            candidate_record(&destination).commit,
            reference_record.commit,
            "{boundary}: the recovered candidate is the clean candidate"
        );
        assert_eq!(
            destination_carry(&destination),
            reference_carry,
            "{boundary}: no carried row may be lost or duplicated"
        );
        assert_eq!(
            destination_carry(&destination).checkpoints[0].1,
            2,
            "{boundary}: the acknowledged checkpoint version survives"
        );
    }
}

#[test]
fn migration_abort_kill_after_the_quarantine_rename_finishes_the_discard() {
    let parent = TempDir::new().unwrap();
    let pristine = pristine_source(parent.path());
    let source_head = git(&pristine, &["rev-parse", "HEAD"]);
    let source = parent.path().join("source");
    duplicate(&pristine, &source);
    let reference_destination = parent.path().join("reference-destination");
    let reference_run = run_copy(&source, &reference_destination);
    assert!(
        reference_run.status.success(),
        "reference migration: {}",
        String::from_utf8_lossy(&reference_run.stderr)
    );
    let reference_copy = committed_copy(&reference_destination);

    let destination = parent.path().join("migrated");
    let prepared = run_copy(&source, &destination);
    assert!(
        prepared.status.success(),
        "prepared migration: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let quarantine =
        orna_repository::migration_copy::discard_quarantine_path(&destination).unwrap();
    let crash = parent.path().join("crash");
    fs::create_dir(&crash).unwrap();
    let arguments = [std::ffi::OsStr::new("abort"), destination.as_os_str()];
    kill_at(&source, &arguments, "migration-discard-renamed", &crash);

    assert!(
        !destination.exists(),
        "the discard committed to removing it"
    );
    assert!(quarantine.is_dir(), "the copy is only under its quarantine");
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), source_head);

    let finished = run_abort(&source, &destination);
    assert!(
        finished.status.success(),
        "finishing the interrupted discard: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    assert!(!quarantine.exists(), "the quarantine is removed");
    assert!(!destination.exists());

    // A prepared copy can be rebuilt identically after the discard.
    let rebuilt = run_copy(&source, &destination);
    assert!(
        rebuilt.status.success(),
        "migration after discard: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert_eq!(
        committed_copy(&destination),
        reference_copy,
        "a rebuilt copy equals the one the discard removed"
    );
}
