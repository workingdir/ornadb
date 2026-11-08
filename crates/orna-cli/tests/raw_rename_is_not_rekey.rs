//! ORNA-MUT-011: a raw filesystem path rename is not evidence of semantic
//! continuity. Renaming a module file with ordinary filesystem operations, and
//! not through `orna mv`, MUST be interpreted as deletion plus insertion: the
//! table at the new path receives a fresh identity and the old identity is
//! gone from the committed inventory.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use orna_project::{ObjectIdentityManifest, ObjectKind};
use orna_repository::Repository;
use tempfile::TempDir;

const MAIN: &str = "use warehouse;\n";
const WAREHOUSE: &str = "pub table Media(id: Str) { size: Int, }\n";
const MAIN_RENAMED: &str = "use archive;\n";

/// Keeps the host's Git configuration and repository routing out of every
/// process this test starts.
fn isolated(program: &str) -> Command {
    let mut command = Command::new(program);
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
    command
}

fn run_cli(directory: &Path, args: &[&str]) -> Output {
    isolated(env!("CARGO_BIN_EXE_orna"))
        .current_dir(directory)
        .args(args)
        .output()
        .expect("CLI process")
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = isolated("git")
        .current_dir(directory)
        .args(args)
        .output()
        .expect("Git process");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git prints UTF-8")
}

fn assert_cli_success(output: &Output, step: &str) {
    assert!(
        output.status.success(),
        "{step} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `orna init`, the two source modules, and the first `orna commit`.
fn committed_project() -> (TempDir, PathBuf, ObjectIdentityManifest) {
    let directory = tempfile::tempdir().expect("project directory");
    let root = directory.path().join("media");
    assert_cli_success(
        &run_cli(
            directory.path(),
            &["init", root.to_str().expect("UTF-8 test path")],
        ),
        "orna init",
    );
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    for (path, source) in [("main.orna", MAIN), ("warehouse.orna", WAREHOUSE)] {
        fs::write(root.join(path), source).expect("source file");
        git(&root, &["add", path]);
    }
    git(&root, &["add", ".orna/database.orna"]);
    assert_cli_success(
        &run_cli(&root, &["commit", "-m", "initial source"]),
        "orna commit",
    );
    let repository = Repository::discover(&root).expect("initialized repository");
    let head = repository.head().unwrap().expect("committed HEAD");
    let manifest =
        ObjectIdentityManifest::read_committed(&repository, &head).expect("committed identities");
    (directory, root, manifest)
}

/// Renames the module file with filesystem operations only, repoints its
/// importer, and commits. No `orna mv` runs, so no rename intent exists.
#[test]
fn raw_module_path_rename_is_deletion_plus_insertion_not_identity_continuity() {
    let (_directory, root, initial) = committed_project();
    let old_table = initial
        .id(ObjectKind::Table, "warehouse.Media")
        .expect("initial table identity");

    fs::rename(root.join("warehouse.orna"), root.join("archive.orna"))
        .expect("raw filesystem rename");
    fs::write(root.join("main.orna"), MAIN_RENAMED).expect("repoint importer");
    git(&root, &["add", "-A"]);
    assert_cli_success(
        &run_cli(&root, &["commit", "-m", "raw rename of the module file"]),
        "orna commit after raw rename",
    );

    let repository = Repository::discover(&root).expect("repository");
    let head = repository.head().unwrap().expect("committed HEAD");
    let committed =
        ObjectIdentityManifest::read_committed(&repository, &head).expect("committed identities");
    assert_eq!(
        committed.id(ObjectKind::Table, "warehouse.Media"),
        None,
        "the vanished path loses its identity"
    );
    let new_table = committed
        .id(ObjectKind::Table, "archive.Media")
        .expect("the new path is a fresh table");
    assert_ne!(
        new_table, old_table,
        "a raw rename MUST NOT carry the old identity to the new path"
    );
}
