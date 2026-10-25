//! `orna mv` on a database the product created and committed: the rename
//! moves the table and field identities in the staged store inventory, leaves
//! the source edit for review and never writes a worktree sidecar
//! (ORNA-OBJECT-003, ORNA-CLI-001, ORNA-META-001). Failures leave the source
//! and the index as they were.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use orna_project_v1::{ObjectIdentityManifest, ObjectKind};
use orna_repository_v1::{
    ManagedFileChange, ManagedPath, PublicationJournal, PublicationJournalEntry, Repository,
};
use tempfile::TempDir;

const SOURCE: &str = "pub table Media(id: Str) { payload: Blob, size: Int, }";
const RENAMED: &str = "pub table Archive(id: Str) { payload: Blob, size: Int, }";

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
        "{step}: stdout {} stderr {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `orna init`, `source` as `main.orna`, and the first `orna commit`.
fn committed_project(source: &str) -> (TempDir, PathBuf, ObjectIdentityManifest) {
    committed_project_with(&[("main.orna", source)])
}

/// `orna init`, the given source modules, and the first `orna commit`.
fn committed_project_with(modules: &[(&str, &str)]) -> (TempDir, PathBuf, ObjectIdentityManifest) {
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
    for (path, source) in modules {
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

fn staged_identities(root: &Path) -> ObjectIdentityManifest {
    let repository = Repository::discover(root).expect("repository");
    let staged = repository
        .capture_staged_candidate()
        .expect("capture the index");
    ObjectIdentityManifest::read_staged(&repository, &staged)
        .expect("staged identities")
        .expect("a staged inventory")
}

/// The staged entries; Git may refresh its own index caches meanwhile.
fn index_bytes(root: &Path) -> Vec<u8> {
    git(root, &["ls-files", "--stage"]).into_bytes()
}

fn assert_refused_without_changes(output: &Output, root: &Path, source: &[u8], index: &[u8]) {
    assert!(!output.status.success(), "mv must fail");
    assert!(output.stdout.is_empty(), "failed mv emits no stdout");
    assert!(!output.stderr.is_empty(), "failed mv reports a diagnostic");
    assert_eq!(fs::read(root.join("main.orna")).unwrap(), source);
    assert_eq!(index_bytes(root), index);
    assert!(!root.join(".orna/object-identities.cbor").exists());
}

/// ORNA-CLI-001: `orna mv` preserves table and child-field identities at the
/// production binary boundary.
#[test]
fn mv_renames_a_committed_table_and_preserves_table_and_child_field_ids() {
    let (_directory, root, initial) = committed_project(SOURCE);
    let head = git(&root, &["rev-parse", "HEAD"]);

    let output = run_cli(&root, &["mv", "Media", "Archive"]);
    assert_cli_success(&output, "orna mv");

    assert_eq!(fs::read_to_string(root.join("main.orna")).unwrap(), RENAMED);
    let renamed = staged_identities(&root);
    for (old, new, kind) in [
        ("Media", "Archive", ObjectKind::Table),
        ("Media.id", "Archive.id", ObjectKind::Field),
        ("Media.payload", "Archive.payload", ObjectKind::Field),
        ("Media.size", "Archive.size", ObjectKind::Field),
    ] {
        assert_eq!(renamed.id(kind, new), initial.id(kind, old), "{old} moves");
        assert!(initial.id(kind, old).is_some());
        assert_eq!(renamed.id(kind, old), None);
    }
    assert!(renamed.records().iter().all(|record| !record.is_retired()));
    // Nothing is committed. As after `git mv`, the renamed source is staged
    // with the restaged store, so the worktree matches the index.
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&root, &["show", ":main.orna"]), RENAMED);
    assert_eq!(git(&root, &["diff", "--name-only"]), "");
    let staged = git(&root, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.lines().any(|path| path == "main.orna")
            && staged
                .lines()
                .all(|path| path == "main.orna" || path.starts_with(".orna/store/")),
        "{staged}"
    );
    assert!(!root.join(".orna/object-identities.cbor").exists());
}

/// A table in a nested module is named by its module namespace, and the
/// rename edits and stages that module only.
#[test]
fn mv_renames_a_table_in_a_nested_module() {
    const MAIN: &str = "use warehouse;\n";
    const WAREHOUSE: &str = "pub table Media(id: Str) { size: Int, }\n";
    let (_directory, root, initial) =
        committed_project_with(&[("main.orna", MAIN), ("warehouse.orna", WAREHOUSE)]);
    assert!(initial.id(ObjectKind::Table, "warehouse.Media").is_some());

    let output = run_cli(&root, &["mv", "warehouse.Media", "Archive"]);
    assert_cli_success(&output, "orna mv");

    const RENAMED_WAREHOUSE: &str = "pub table Archive(id: Str) { size: Int, }\n";
    assert_eq!(
        fs::read_to_string(root.join("warehouse.orna")).unwrap(),
        RENAMED_WAREHOUSE
    );
    assert_eq!(git(&root, &["show", ":warehouse.orna"]), RENAMED_WAREHOUSE);
    assert_eq!(git(&root, &["show", ":main.orna"]), MAIN);
    let renamed = staged_identities(&root);
    for (old, new, kind) in [
        ("warehouse.Media", "warehouse.Archive", ObjectKind::Table),
        (
            "warehouse.Media.id",
            "warehouse.Archive.id",
            ObjectKind::Field,
        ),
        (
            "warehouse.Media.size",
            "warehouse.Archive.size",
            ObjectKind::Field,
        ),
    ] {
        assert!(initial.id(kind, old).is_some());
        assert_eq!(renamed.id(kind, new), initial.id(kind, old), "{old} moves");
    }
    assert_cli_success(
        &run_cli(&root, &["commit", "-m", "rename"]),
        "orna commit after mv",
    );
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}

/// A run interrupted after the source write resumes: the worktree already
/// names the new table, so `mv` leaves that file in place, stages the same
/// rename of the staged module and moves the table and field identities.
#[test]
fn mv_resumes_after_the_source_was_renamed() {
    let (_directory, root, initial) = committed_project(SOURCE);
    let source_path = root.join("main.orna");
    fs::write(&source_path, RENAMED).expect("simulate the source-only rename");
    let source_metadata = fs::metadata(&source_path).expect("source metadata");

    let output = run_cli(&root, &["mv", "Media", "Archive"]);
    assert_cli_success(&output, "resumed orna mv");

    assert_eq!(fs::read_to_string(&source_path).unwrap(), RENAMED);
    assert_eq!(git(&root, &["show", ":main.orna"]), RENAMED);
    let renamed = staged_identities(&root);
    for (old, new, kind) in [
        ("Media", "Archive", ObjectKind::Table),
        ("Media.id", "Archive.id", ObjectKind::Field),
        ("Media.payload", "Archive.payload", ObjectKind::Field),
        ("Media.size", "Archive.size", ObjectKind::Field),
    ] {
        assert!(initial.id(kind, old).is_some());
        assert_eq!(renamed.id(kind, new), initial.id(kind, old), "{old} moves");
        assert_eq!(renamed.id(kind, old), None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            source_metadata.ino(),
            fs::metadata(&source_path).unwrap().ino()
        );
    }

    // Once staged, the rename is complete: a second run finds no `Media`.
    let repeated = run_cli(&root, &["mv", "Media", "Archive"]);
    assert!(!repeated.status.success(), "the rename is already staged");
}

#[cfg(unix)]
#[test]
fn mv_leaves_everything_unchanged_when_the_source_write_fails() {
    use std::os::unix::fs::PermissionsExt;

    let (_directory, root, _) = committed_project(SOURCE);
    let source = fs::read(root.join("main.orna")).unwrap();
    let index = index_bytes(&root);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();

    let output = run_cli(&root, &["mv", "Media", "Archive"]);

    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert_refused_without_changes(&output, &root, &source, &index);
}

/// The source is written before the index is updated. A retained
/// publication journal owns the index (ORNA-PUB-007), so installing the
/// restaged store is refused after the source write; `mv` names that cause
/// and puts the source back, leaving the index untouched.
#[test]
fn mv_restores_the_source_when_staging_the_inventory_fails() {
    let (_directory, root, initial) = committed_project(SOURCE);
    let source_path = root.join("main.orna");
    let source = fs::read(&source_path).unwrap();
    #[cfg(unix)]
    let source_ctime = {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&source_path).unwrap();
        (metadata.ctime(), metadata.ctime_nsec())
    };
    let index = index_bytes(&root);
    let repository = Repository::discover(&root).expect("repository");
    let head = repository.head().unwrap().expect("committed HEAD");
    let managed = ManagedPath::new("generated/row.orna").unwrap();
    let candidate = repository
        .build_private_commit(
            &head,
            &[ManagedFileChange::new(
                managed.clone(),
                Some(b"candidate row\n".to_vec()),
            )],
            "orna: publish runtime data",
        )
        .unwrap();
    let journal = PublicationJournal::new_with_runtime_intent(
        head,
        candidate.commit().clone(),
        repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone(),
        [7; 16],
        vec![PublicationJournalEntry::new(
            managed,
            None,
            Some(b"candidate row\n".to_vec()),
        )],
    )
    .unwrap();
    repository.write_publication_journal(&journal).unwrap();

    let output = run_cli(&root, &["mv", "Media", "Archive"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("interrupted publication"),
        "the refusal comes from installing the restaged store: {stderr}"
    );
    assert_refused_without_changes(&output, &root, &source, &index);
    // The source file was written and then restored, not left untouched. A
    // write and a restore both advance the change time, whereas inode numbers
    // are reused by some filesystems (tmpfs), so only the ctime is compared.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&source_path).unwrap();
        assert_ne!(
            (metadata.ctime(), metadata.ctime_nsec()),
            source_ctime,
            "the source must be rewritten and restored, so its change time advances"
        );
    }
    assert_eq!(
        repository.read_publication_journal().unwrap(),
        Some(journal)
    );
    assert_eq!(staged_identities(&root), initial);
}

#[test]
fn mv_rejects_an_existing_destination_without_changes() {
    const SOURCE_WITH_DESTINATION: &str = "pub table Media(id: Str) { payload: Blob, }
pub table Archive(id: Str) { payload: Blob, }";
    let (_directory, root, _) = committed_project(SOURCE_WITH_DESTINATION);
    let source = fs::read(root.join("main.orna")).unwrap();
    let index = index_bytes(&root);

    let output = run_cli(&root, &["mv", "Media", "Archive"]);

    assert_refused_without_changes(&output, &root, &source, &index);
}

#[test]
fn mv_rejects_missing_source_and_invalid_target_without_changes() {
    let (_directory, root, initial) = committed_project(SOURCE);
    let source = fs::read(root.join("main.orna")).unwrap();
    let index = index_bytes(&root);

    for args in [
        ["mv", "Unknown", "Archive"],
        ["mv", "Media", "Media.payload"],
    ] {
        let output = run_cli(&root, &args);
        assert_refused_without_changes(&output, &root, &source, &index);
    }
    assert_eq!(staged_identities(&root), initial);
}
