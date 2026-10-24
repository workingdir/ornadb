//! ORNA-REPO-CONTEXT-003, ORNA-UPGRADE-001: runtime admission follows the
//! format recorded at the committed `HEAD`, and is decided before any worktree
//! metadata is read. A `HEAD` that records a format this build does not know
//! is refused with its stable code, and the local runtime state is neither
//! opened nor changed, even when the worktree carries format-3 metadata that
//! would otherwise admit the runtime.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Output},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeError, RuntimeState, repository_runtime_identity};
use tempfile::TempDir;

#[path = "support/format3.rs"]
mod format3;
use format3::{cli_runtime_identity, git, git_output};

const DATABASE_ID: &str = "9f237b7e-6844-4498-bcd5-d24641c07449";

/// A committed `.orna/database.orna` recording format 4, which this build
/// does not know.
const UNKNOWN_HEAD_FORMAT_RECORD: &str =
    "{\n    repository_format: 4,\n    database_id: \"9f237b7e-6844-4498-bcd5-d24641c07449\",\n}\n";

fn run(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
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

/// A repository whose committed `HEAD` records an unknown format, with
/// format-3 metadata written over it in the worktree only.
fn unknown_head_with_format_three_worktree() -> TempDir {
    let directory = TempDir::new().expect("temporary repository");
    let root = directory.path();
    git(root, &["init", "--quiet", "--initial-branch=main"]);
    git(root, &["config", "user.name", "Orna test"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), "pub fn main(): Int = 42;\n").unwrap();
    fs::create_dir_all(root.join(".orna")).unwrap();
    fs::write(root.join(".orna/database.orna"), UNKNOWN_HEAD_FORMAT_RECORD).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "unknown format snapshot"]);

    fs::write(
        root.join(".orna/database.orna"),
        format!("{{\n    repository_format: 3,\n    database_id: \"{DATABASE_ID}\",\n}}\n"),
    )
    .unwrap();
    directory
}

/// Every byte of the local runtime state, so a refused run can prove it
/// neither opened nor changed the runtime.
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

/// The Git-visible state a refused run must leave untouched.
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

/// The committed `HEAD` names format 4, so `orna run` refuses with
/// ORNA-REPO-CONTEXT-003 before the worktree's format-3 metadata is consulted
/// and before any existing runtime state is opened or changed.
#[tokio::test]
async fn run_refuses_unknown_committed_format() {
    let directory = unknown_head_with_format_three_worktree();
    let root = directory.path();
    let repository = Repository::discover(root).unwrap();

    // The worktree alone would admit a runtime; create real runtime state
    // from it so a later refusal has bytes to protect.
    {
        let (identity, initial_digest) = cli_runtime_identity(&repository);
        let _state = RuntimeState::open(&repository, identity, initial_digest)
            .await
            .unwrap();
    }
    assert!(repository.runtime_paths().state_db().exists());
    let runtime_before = runtime_state_files(&repository);
    let git_before = git_state(root);

    let Err(RuntimeError::UnknownHeadFormat(refusal)) =
        repository_runtime_identity(&repository)
    else {
        panic!("a committed unknown HEAD format must refuse runtime admission");
    };
    assert_eq!(refusal.code(), "ORNA-REPO-CONTEXT-003");

    let output = run(root, &["run", "main.main"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty(), "{stderr}");
    assert!(
        stderr.contains("error[ORNA-REPO-CONTEXT-003]: repository format is unknown"),
        "{stderr}"
    );
    assert_eq!(git_state(root), git_before);
    assert_eq!(runtime_state_files(&repository), runtime_before);
}
