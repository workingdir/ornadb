use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use orna_project_v1::{ObjectIdentityManifest, ObjectKind};
use orna_repository_v1::Repository;
use orna_semantic_v1::ModuleInput;
use tempfile::TempDir;

const SOURCE: &str = "pub table Media(id: Str) { payload: Blob, size: Int, }";

fn run_cli(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(args)
        .output()
        .expect("CLI process")
}

fn git(directory: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Orna test")
        .env("GIT_AUTHOR_EMAIL", "orna-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Orna test")
        .env("GIT_COMMITTER_EMAIL", "orna-test@example.invalid")
        .current_dir(directory)
        .args(args)
        .output()
        .expect("Git process")
}

fn git_succeeds(directory: &Path, args: &[&str]) -> Output {
    let output = git(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn source_project(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    fs::write(directory.path().join("main.orna"), source).expect("source file");
    let output = run_cli(directory.path(), &["init"]);
    assert!(output.status.success(), "init stderr: {:?}", output.stderr);
    git_succeeds(directory.path(), &["add", "--all"]);
    git_succeeds(
        directory.path(),
        &["commit", "--quiet", "-m", "initial source"],
    );
    directory
}

fn initialized_project() -> (TempDir, ObjectIdentityManifest) {
    initialized_project_with_source(SOURCE)
}

fn initialized_project_with_source(source: &str) -> (TempDir, ObjectIdentityManifest) {
    let directory = source_project(source);
    let repository = Repository::discover(directory.path()).expect("initialized repository");
    let module = ModuleInput::new("main.orna", source);
    let manifest = ObjectIdentityManifest::reconcile_and_write_worktree(
        &repository,
        std::slice::from_ref(&module),
    )
    .expect("initial object identities");
    git_succeeds(directory.path(), &["add", "--all"]);
    git_succeeds(
        directory.path(),
        &["commit", "--quiet", "-m", "initial object identities"],
    );
    (directory, manifest)
}

#[test]
fn mv_renames_a_committed_table_and_preserves_table_and_child_field_ids() {
    let (directory, initial_manifest) = initialized_project();
    let repository = Repository::discover(directory.path()).expect("repository");
    let committed_manifest = git_succeeds(
        directory.path(),
        &["show", "HEAD:.orna/object-identities.cbor"],
    )
    .stdout;
    assert_eq!(
        committed_manifest,
        initial_manifest
            .encode()
            .expect("initial manifest encoding")
    );
    let original_head = git_succeeds(directory.path(), &["rev-parse", "HEAD"]).stdout;
    let table_id = *initial_manifest
        .id(ObjectKind::Table, "Media")
        .expect("source table identity");
    let payload_id = *initial_manifest
        .id(ObjectKind::Field, "Media.payload")
        .expect("payload identity");
    let size_id = *initial_manifest
        .id(ObjectKind::Field, "Media.size")
        .expect("size identity");

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    assert!(output.status.success(), "mv stderr: {:?}", output.stderr);
    assert_eq!(
        fs::read(directory.path().join("main.orna")).unwrap(),
        b"pub table Archive(id: Str) { payload: Blob, size: Int, }"
    );

    let renamed_manifest = ObjectIdentityManifest::read_worktree(&repository)
        .expect("worktree manifest read")
        .expect("renamed worktree manifest");
    assert_eq!(
        renamed_manifest.id(ObjectKind::Table, "Archive"),
        Some(&table_id)
    );
    assert_eq!(
        renamed_manifest.id(ObjectKind::Field, "Archive.payload"),
        Some(&payload_id)
    );
    assert_eq!(
        renamed_manifest.id(ObjectKind::Field, "Archive.size"),
        Some(&size_id)
    );
    assert!(renamed_manifest.id(ObjectKind::Table, "Media").is_none());
    assert!(
        renamed_manifest
            .records()
            .iter()
            .any(|record| record.kind() == ObjectKind::Table
                && record.qualified_name() == "Archive"
                && !record.is_retired())
    );
    assert_eq!(
        git_succeeds(directory.path(), &["rev-parse", "HEAD"]).stdout,
        original_head
    );
    let changed_paths = git_succeeds(directory.path(), &["diff", "--name-only", "HEAD"]).stdout;
    let changed_paths = String::from_utf8(changed_paths).expect("changed path output");
    assert!(
        changed_paths.lines().any(|path| path == ".orna/object-identities.cbor"),
        "identity manifest is not an uncommitted change: {changed_paths}"
    );
    assert!(
        changed_paths.lines().any(|path| path == "main.orna"),
        "source is not an uncommitted change: {changed_paths}"
    );
}

#[test]
fn mv_requires_an_identity_manifest_and_leaves_source_unchanged_when_missing() {
    let directory = source_project(SOURCE);
    let original_source = fs::read(directory.path().join("main.orna")).unwrap();
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    assert!(!manifest_path.exists());

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    assert!(
        !output.status.success(),
        "mv without identities should fail"
    );
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    assert_eq!(
        fs::read(directory.path().join("main.orna")).unwrap(),
        original_source
    );
    assert!(!manifest_path.exists());
}

#[test]
fn mv_rejects_existing_destination_without_changing_source_or_manifest() {
    const SOURCE_WITH_DESTINATION: &str = "pub table Media(id: Str) { payload: Blob, }
pub table Archive(id: Str) { payload: Blob, }";
    let (directory, _) = initialized_project_with_source(SOURCE_WITH_DESTINATION);
    let source_path = directory.path().join("main.orna");
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    let original_source = fs::read(&source_path).unwrap();
    let original_manifest = fs::read(&manifest_path).unwrap();

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    assert!(
        !output.status.success(),
        "destination collision should fail"
    );
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
    assert_eq!(fs::read(&source_path).unwrap(), original_source);
    assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);
}

#[test]
fn mv_rejects_missing_source_and_target_without_changing_source_or_manifest() {
    let (directory, initial_manifest) = initialized_project();
    let original_source = fs::read(directory.path().join("main.orna")).unwrap();
    let original_manifest =
        fs::read(directory.path().join(".orna/object-identities.cbor")).unwrap();

    for args in [
        ["mv", "Unknown", "Archive"],
        ["mv", "Media", "Media.payload"],
    ] {
        let output = run_cli(directory.path(), &args);
        assert!(!output.status.success(), "invalid mv should fail");
        assert!(output.stdout.is_empty(), "failed mv emits no stdout");
        assert!(!output.stderr.is_empty(), "failed mv reports a diagnostic");
        assert_eq!(
            fs::read(directory.path().join("main.orna")).unwrap(),
            original_source
        );
        assert_eq!(
            fs::read(directory.path().join(".orna/object-identities.cbor")).unwrap(),
            original_manifest
        );
    }

    assert_eq!(
        ObjectIdentityManifest::read_worktree(
            &Repository::discover(directory.path()).expect("repository")
        )
        .expect("manifest read"),
        Some(initial_manifest)
    );
}
