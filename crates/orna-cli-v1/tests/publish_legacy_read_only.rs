//! ORNA-UPGRADE-001, ORNA-UPGRADE-003, ORNA-ROW-013: a format-1/2 snapshot
//! keeps its versioned readers but has no row writer. `orna publish` onto
//! such a `HEAD` fails early with a typed read-only diagnostic that names the
//! format-3 upgrade work item, and writes nothing: no commit, journal, loose
//! row, Git object, index or worktree change, and no runtime state change.
//! Runtime writes follow the format recorded at `HEAD` too, so a legacy
//! `HEAD` refuses them whatever metadata the worktree holds.
//!
//! Each case runs with the `format.orna` records the legacy writers actually
//! produced, OrnaDB 1.0's format-1 record and the previous draft's closed
//! format-2 record, as well as the bare `format N` marker. The worktree
//! carries uncommitted format-3 metadata, so the worktree alone would admit a
//! runtime and real pending rows exist: the case in which the retired loose
//! writer used to run. Inside a prepared `orna migrate copy` destination the
//! refusal names the unactivated format-3 candidate instead.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Output},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{
    NoFault, RuntimeError, RuntimeState, TableMutation, repository_runtime_identity,
};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::{cli_runtime_identity, git, git_output};

const DATABASE_ID: &str = "9f237b7e-6844-4498-bcd5-d24641c07449";

/// Committed `format.orna` contents and the format each one records.
const LEGACY_FORMAT_RECORDS: [(&str, u8); 3] = [
    ("format 2\n", 2),
    (
        include_str!("fixtures/format-context/format-1-ornadb-1.0.orna"),
        1,
    ),
    (
        include_str!("fixtures/format-context/format-2-draft-1.1.0.orna"),
        2,
    ),
];

fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    command
        .current_dir(directory)
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
    ] {
        command.env_remove(name);
    }
    command.args(arguments).output().expect("CLI process starts")
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("Git output is UTF-8")
}

/// Commits a legacy snapshot in a new repository at `root`: `format_record`
/// as `.orna/format.orna`, beside the legacy `database.orna` sidecar.
fn commit_legacy_snapshot(root: &Path, format_record: &str) {
    git(root, &["init", "--quiet", "--initial-branch=main"]);
    git(root, &["config", "user.name", "Orna test"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), "module main;\n").unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(root.join(".orna/format.orna"), format_record).unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        format!("{{database_id: \"{DATABASE_ID}\"}}\n"),
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "legacy snapshot"]);
}

/// A repository whose committed `HEAD` is a legacy snapshot, with format-3
/// metadata written over it in the worktree only.
fn legacy_head_with_format_three_worktree(format_record: &str) -> TempDir {
    let directory = TempDir::new().expect("temporary repository");
    let root = directory.path();
    commit_legacy_snapshot(root, format_record);

    fs::remove_file(root.join(".orna/format.orna")).unwrap();
    fs::write(
        root.join(".orna/database.orna"),
        format!("{{\n    repository_format: 3,\n    database_id: \"{DATABASE_ID}\",\n}}\n"),
    )
    .unwrap();
    directory
}

/// Every byte of the local runtime state, so a refused publication can prove
/// it neither opened nor froze the runtime.
fn runtime_state_files(repository: &Repository) -> BTreeMap<String, Vec<u8>> {
    let root = repository.runtime_paths().root();
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root).expect("runtime state directory exists") {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            files.insert(
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            );
        }
    }
    files
}

/// The Git-visible state a refused publication must leave untouched.
fn git_state(root: &Path) -> (String, String, String) {
    (
        text(git_output(root, &["rev-parse", "HEAD"], None)),
        text(git_output(
            root,
            &["status", "--porcelain", "--untracked-files=all"],
            None,
        )),
        text(git_output(root, &["count-objects", "-v"], None)),
    )
}

#[tokio::test]
async fn publish_refuses_a_legacy_head_without_writing() {
    for (format_record, format) in LEGACY_FORMAT_RECORDS {
        let directory = legacy_head_with_format_three_worktree(format_record);
        let root = directory.path();
        let repository = Repository::discover(root).unwrap();
        let head = repository.head().unwrap().unwrap();
        assert_eq!(
            repository
                .open_format_context_at_selector(head.as_str())
                .unwrap()
                .repository_format_number(),
            format,
            "the committed snapshot is a format-{format} compatibility reader"
        );

        // Rows left pending by a build that still admitted writes here.
        let (identity, initial_digest) = cli_runtime_identity(&repository);
        let state = RuntimeState::open(&repository, identity, initial_digest)
            .await
            .unwrap();
        let writer = state.acquire_lease(identity.repository_id).await.unwrap();
        let activation = state.begin_activation().await.unwrap();
        let mutation = TableMutation::new(
            [0x31; 16],
            "Contact",
            b"Alice".to_vec(),
            Some(b"pending row".to_vec()),
        )
        .unwrap();
        state
            .commit_table_activation(writer, &activation, &[mutation], [0x32; 32], &NoFault)
            .await
            .unwrap();
        assert_eq!(state.pending_count().await.unwrap(), 1);
        drop(state);

        let git_before = git_state(root);
        let runtime_before = runtime_state_files(&repository);

        for arguments in [&["publish"][..], &["--debug", "publish"][..]] {
            let output = run(root, arguments);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "{stderr}");
            assert!(output.stdout.is_empty(), "{stderr}");
            assert!(
                stderr.contains("error[E2300]: publication target is a read-only legacy snapshot"),
                "{stderr}"
            );
            assert!(
                stderr.contains(
                    "help: this build has no writer or in-place upgrade for format-1/2 snapshots; the format-3 upgrade is tracked as ornadb-5il1.184"
                ),
                "{stderr}"
            );
            assert!(!stderr.contains("migrate copy"), "{stderr}");
            if arguments.contains(&"--debug") {
                assert!(
                    stderr.contains(&format!(
                        "repository format {format} is a read-only legacy snapshot"
                    )),
                    "{stderr}"
                );
            }

            assert_eq!(git_state(root), git_before);
            assert_eq!(repository.read_publication_journal().unwrap(), None);
            assert_eq!(runtime_state_files(&repository), runtime_before);
            assert!(!root.join("Contact").exists());
        }

        let reopened = RuntimeState::open(&repository, identity, initial_digest)
            .await
            .unwrap();
        assert_eq!(reopened.pending_count().await.unwrap(), 1);
    }
}

/// ORNA-UPGRADE-001, ORNA-UPGRADE-003: runtime admission follows the format
/// recorded at `HEAD`. The worktree's format-3 metadata alone would admit a
/// runtime, but a legacy `HEAD` refuses it with a typed refusal that carries
/// its code, format and remedy, before any runtime state is opened.
#[test]
fn runtime_writes_are_refused_on_a_legacy_head() {
    for (format_record, format) in LEGACY_FORMAT_RECORDS {
        let directory = legacy_head_with_format_three_worktree(format_record);
        let root = directory.path();
        fs::write(root.join("main.orna"), "pub fn main(): Int = 42;\n").unwrap();
        let repository = Repository::discover(root).unwrap();
        assert!(
            orna_repository_v1::inspect_metadata(&repository)
                .unwrap()
                .is_some(),
            "the worktree metadata alone admits a format-3 runtime"
        );

        let Err(RuntimeError::LegacyHeadReadOnly(refusal)) =
            repository_runtime_identity(&repository)
        else {
            panic!("a format-{format} HEAD must refuse runtime writes");
        };
        assert_eq!(refusal.format(), format);
        assert_eq!(refusal.code(), "ORNA-REPO-CONTEXT-015");
        assert_eq!(
            refusal.title(),
            "repository HEAD is a read-only legacy snapshot"
        );
        assert_eq!(
            refusal.remedy(),
            "this build has no writer or in-place upgrade for format-1/2 snapshots; the format-3 upgrade is tracked as ornadb-5il1.184"
        );
        assert_eq!(
            refusal.to_string(),
            format!(
                "repository format {format} at HEAD is a read-only compatibility snapshot; durable runtime writes are refused"
            )
        );

        // The CLI maps every runtime identity failure to one E2200 today; the
        // typed refusal above is what its mapping receives.
        let git_before = git_state(root);
        let output = run(root, &["run", "main.main"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(output.stdout.is_empty(), "{stderr}");
        assert!(
            stderr.contains("error[E2200]: repository runtime metadata is unavailable"),
            "{stderr}"
        );
        assert_eq!(git_state(root), git_before);
        assert!(!repository.runtime_paths().state_db().exists());
    }
}

/// The destination of `orna migrate copy` keeps the legacy commit as `HEAD`
/// and journals the format-3 candidate. Publishing there is still refused,
/// but the remedy names the prepared candidate rather than another copy, and
/// the journal, Git state and runtime state files are left as they were.
#[test]
fn publish_in_a_prepared_migration_copy_names_the_unactivated_candidate() {
    for (format_record, _) in LEGACY_FORMAT_RECORDS {
        let parent = TempDir::new().expect("temporary parent");
        let source = parent.path().join("source");
        fs::create_dir(&source).unwrap();
        commit_legacy_snapshot(&source, format_record);
        let destination = parent.path().join("migrated");
        let copy = run(
            &source,
            &[
                "migrate",
                "copy",
                destination.to_str().unwrap(),
                "--annotation-defaults",
                "0",
                "--no-persistent-consumers",
            ],
        );
        assert!(
            copy.status.success(),
            "{}",
            String::from_utf8_lossy(&copy.stderr)
        );

        let repository = Repository::discover(&destination).unwrap();
        let journal = repository
            .read_publication_journal()
            .unwrap()
            .expect("the migration copy journals its candidate");
        assert!(journal.migration_continuity().is_some());
        assert!(repository.holds_prepared_migration());
        let git_before = git_state(&destination);
        let runtime_before = runtime_state_files(&repository);

        let output = run(&destination, &["publish"]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(output.stdout.is_empty(), "{stderr}");
        assert!(
            stderr.contains("error[E2300]: publication target is a read-only legacy snapshot"),
            "{stderr}"
        );
        assert!(
            stderr.contains(
                "help: this build has no writer for format-1/2 snapshots; a format-3 migration candidate is prepared here but not activated; activation is tracked as ornadb-5il1.192"
            ),
            "{stderr}"
        );
        assert!(!stderr.contains("migrate copy"), "{stderr}");

        assert_eq!(git_state(&destination), git_before);
        assert_eq!(
            repository.read_publication_journal().unwrap(),
            Some(journal)
        );
        assert_eq!(runtime_state_files(&repository), runtime_before);
    }
}
