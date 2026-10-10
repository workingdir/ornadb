#[path = "../src/test_support.rs"]
mod test_support;

use std::{fs, path::Path, process::Command};

use orna_repository_v1::{
    MigrationAnnotationDefaults, MigrationContinuityRecord, Repository, migration_copy,
};
use tempfile::TempDir;

fn git_output(directory: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_text(directory: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_output(directory, arguments))
        .unwrap()
        .trim()
        .to_owned()
}

fn init_repository(root: &Path) {
    git_output(root, &["init", "-b", "main"]);
    test_support::configure_fixture_git_identity(root);
    git_output(root, &["config", "commit.gpgsign", "false"]);
}

#[test]
fn populated_format1_dirty_workspace_is_preserved_in_copy() {
    let source = TempDir::new().unwrap();
    init_repository(source.path());
    fs::write(
        source.path().join("main.orna"),
        include_str!("fixtures/git-repository-main.orna"),
    )
    .unwrap();
    fs::write(source.path().join("tracked.txt"), b"committed bytes\n").unwrap();
    fs::create_dir_all(source.path().join(".orna")).unwrap();
    fs::write(
        source.path().join(".orna/format.orna"),
        include_str!("fixtures/format-context/format-1.orna"),
    )
    .unwrap();
    fs::write(
        source.path().join(".orna/database.orna"),
        include_str!("fixtures/format-context/database-sidecar.orna"),
    )
    .unwrap();
    git_output(source.path(), &["add", "."]);
    git_output(source.path(), &["commit", "-m", "format-1 source"]);

    let dependency = TempDir::new().unwrap();
    init_repository(dependency.path());
    fs::write(
        dependency.path().join("dependency.txt"),
        b"committed dependency\n",
    )
    .unwrap();
    git_output(dependency.path(), &["add", "."]);
    git_output(dependency.path(), &["commit", "-m", "dependency source"]);
    let dependency_url = dependency.path().to_str().unwrap();
    let dependency_add = Command::new("git")
        .current_dir(source.path())
        .args(["-c", "protocol.file.allow=always", "submodule", "add"])
        .arg(dependency_url)
        .arg("dependencies/example")
        .output()
        .unwrap();
    assert!(
        dependency_add.status.success(),
        "git submodule add: {}",
        String::from_utf8_lossy(&dependency_add.stderr)
    );
    git_output(source.path(), &["commit", "-am", "record dependency"]);
    let source_head = git_text(source.path(), &["rev-parse", "HEAD"]);

    // Keep a distinct staged tree and worktree, plus untracked, ignored and
    // dirty submodule content. Preparing the copy may read this state but must
    // neither settle nor overwrite any part of the source workspace.
    fs::write(source.path().join("tracked.txt"), b"staged bytes\n").unwrap();
    git_output(source.path(), &["add", "tracked.txt"]);
    fs::write(source.path().join("tracked.txt"), b"unstaged bytes\n").unwrap();
    fs::write(source.path().join("new-source.txt"), b"untracked source\n").unwrap();
    fs::write(source.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(source.path().join("ignored")).unwrap();
    fs::write(
        source.path().join("ignored/data.bin"),
        b"ignored source bytes",
    )
    .unwrap();
    let submodule = source.path().join("dependencies/example");
    fs::write(submodule.join("dependency.txt"), b"staged dependency\n").unwrap();
    git_output(&submodule, &["add", "dependency.txt"]);
    fs::write(submodule.join("dependency.txt"), b"unstaged dependency\n").unwrap();
    fs::write(submodule.join("local.txt"), b"untracked dependency\n").unwrap();

    let repository = Repository::discover(source.path()).unwrap();
    let source_database_id = repository
        .open_format_context()
        .unwrap()
        .database_id()
        .unwrap();
    let source_index = repository.index_generation().unwrap();
    let staged_tree = source_index.tree().unwrap().as_str().to_owned();
    let source_status = git_output(
        source.path(),
        &["status", "--porcelain=v2", "--untracked-files=all"],
    );
    let committed_bytes = git_output(
        source.path(),
        &["show", &format!("{source_head}:tracked.txt")],
    );
    let destination_parent = TempDir::new().unwrap();
    let destination = destination_parent.path().join("migrated");
    let defaults = MigrationAnnotationDefaults::new(1).unwrap();

    let copy = migration_copy::prepare_format3_migration(
        &repository,
        &destination,
        MigrationContinuityRecord::new(Vec::new()).unwrap(),
        defaults,
    )
    .unwrap();
    assert_eq!(copy.database_id(), source_database_id);
    let expected_database = include_str!("fixtures/format-context/database-template.orna").replace(
        "00000000-0000-4000-8000-000000000000",
        &source_database_id.to_string(),
    );
    let candidate_database = git_output(
        copy.destination(),
        &[
            "show",
            &format!(
                "{}:.orna/database.orna",
                copy.candidate_journal().new_head().as_str()
            ),
        ],
    );
    assert_eq!(candidate_database, expected_database.as_bytes());

    assert_eq!(repository.head().unwrap().unwrap().as_str(), source_head);
    assert_eq!(repository.index_generation().unwrap(), source_index);
    assert_eq!(
        git_output(
            source.path(),
            &["status", "--porcelain=v2", "--untracked-files=all"],
        ),
        source_status,
        "migration preparation leaves every staged, unstaged and untracked source change intact"
    );
    assert_eq!(
        git_output(source.path(), &["show", ":tracked.txt"]),
        b"staged bytes\n",
        "the staged source blob remains in the original index"
    );
    assert_eq!(
        fs::read(source.path().join("tracked.txt")).unwrap(),
        b"unstaged bytes\n"
    );
    assert_eq!(
        fs::read(submodule.join("dependency.txt")).unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(submodule.join("local.txt")).unwrap(),
        b"untracked dependency\n"
    );

    let migrated = Repository::discover(copy.destination()).unwrap();
    let migrated_head = migrated.head().unwrap().unwrap();
    assert_eq!(migrated_head.as_str(), source_head);
    assert_eq!(copy.source_commit().as_str(), source_head);
    assert_eq!(
        migrated
            .open_format_context()
            .unwrap()
            .repository_format_number(),
        1,
        "the old commit remains readable under its recorded format"
    );
    assert_eq!(
        git_output(
            copy.destination(),
            &["show", &format!("{}:tracked.txt", migrated_head.as_str())],
        ),
        committed_bytes,
        "the old committed blob keeps its byte identity in the copy"
    );
    assert_eq!(
        git_output(copy.destination(), &["show", ":tracked.txt"]),
        b"committed bytes\n",
        "the copy's settled index is the source commit, not a dirty worktree"
    );
    assert_eq!(copy.source_index_generation(), &source_index);
    assert_eq!(
        copy.source_index_generation().tree().unwrap().as_str(),
        staged_tree
    );
    assert_eq!(
        fs::read(copy.destination().join("tracked.txt")).unwrap(),
        b"unstaged bytes\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("new-source.txt")).unwrap(),
        b"untracked source\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("ignored/data.bin")).unwrap(),
        b"ignored source bytes"
    );
    assert_eq!(
        fs::read(
            copy.destination()
                .join("dependencies/example/dependency.txt")
        )
        .unwrap(),
        b"unstaged dependency\n"
    );
    assert_eq!(
        fs::read(copy.destination().join("dependencies/example/local.txt")).unwrap(),
        b"untracked dependency\n"
    );
    assert_eq!(copy.annotation_defaults(), defaults);
    assert_eq!(
        copy.annotation_defaults().entry_count(),
        1,
        "legacy annotation migration is a new semantic coordinate"
    );
    assert!(!copy.annotation_defaults().payload_inspected());
}
