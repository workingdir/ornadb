use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use orna_project_v1::{ObjectIdentityManifest, ObjectKind, ProjectLoader};
use orna_repository_v1::Repository;
use orna_semantic_v1::ModuleInput;
use tempfile::TempDir;

const SOURCE: &str = "pub table Media(id: Str) { payload: Blob, size: Int, }";
const SOURCE_WITH_ROW: &str = "pub table Media(id: Str) { name: Str, age: Int, }";
const ROW_SOURCE: &str = "{ name: \"reachable\", age: 42 }";

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

fn initialized_project_with_loose_row() -> (TempDir, ObjectIdentityManifest) {
    let (directory, manifest) = initialized_project_with_source(SOURCE_WITH_ROW);
    let row_path = directory.path().join("Media/north/42.orna");
    fs::create_dir_all(row_path.parent().unwrap()).expect("loose row directory");
    fs::write(&row_path, ROW_SOURCE).expect("loose row");
    git_succeeds(directory.path(), &["add", "--all"]);
    git_succeeds(
        directory.path(),
        &["commit", "--quiet", "-m", "initial loose row"],
    );
    (directory, manifest)
}

#[test]
fn mv_moves_loose_rows_to_the_renamed_table_directory() {
    let (directory, _) = initialized_project_with_loose_row();

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    assert!(output.status.success(), "mv stderr: {:?}", output.stderr);
    assert!(!directory.path().join("Media").exists());
    assert_eq!(
        fs::read(directory.path().join("Archive/north/42.orna")).unwrap(),
        ROW_SOURCE.as_bytes()
    );

    let repository = Repository::discover(directory.path()).expect("repository");
    let project = ProjectLoader::default()
        .load(&repository)
        .expect("renamed project loads");
    assert_eq!(project.loose_rows().len(), 1);
    assert_eq!(
        project.loose_rows()[0].logical_path(),
        "Archive/north/42.orna"
    );
}
#[test]
fn mv_leaves_directories_without_owned_loose_rows_in_place() {
    let (without_rows, _) = initialized_project();

    let output = run_cli(without_rows.path(), &["mv", "Media", "Archive"]);

    assert!(output.status.success(), "mv stderr: {:?}", output.stderr);
    assert!(!without_rows.path().join("Media").exists());
    assert!(!without_rows.path().join("Archive").exists());

    let (with_notes, _) = initialized_project();
    let notes_directory = with_notes.path().join("Media");
    fs::create_dir(&notes_directory).expect("table notes directory");
    fs::write(notes_directory.join("notes.txt"), b"keep me").expect("table note");

    let output = run_cli(with_notes.path(), &["mv", "Media", "Archive"]);

    assert!(output.status.success(), "mv stderr: {:?}", output.stderr);
    assert_eq!(fs::read(notes_directory.join("notes.txt")).unwrap(), b"keep me");
    assert!(!with_notes.path().join("Archive").exists());
}

#[test]
fn mv_resumes_after_source_write_before_manifest_write() {
    let (directory, initial_manifest) = initialized_project_with_loose_row();
    let source_path = directory.path().join("main.orna");
    let source_after_crash = SOURCE_WITH_ROW.replace("table Media", "table Archive");
    fs::write(&source_path, &source_after_crash).expect("simulate source-only rename");
    let source_metadata = fs::metadata(&source_path).expect("source metadata");
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    let manifest_before_recovery = fs::read(&manifest_path).expect("old manifest");
    assert_eq!(manifest_before_recovery, initial_manifest.encode().unwrap());
    assert!(initial_manifest.id(ObjectKind::Table, "Media").is_some());
    assert!(initial_manifest.id(ObjectKind::Table, "Archive").is_none());
    let table_id = *initial_manifest
        .id(ObjectKind::Table, "Media")
        .expect("active source table identity");
    let name_id = *initial_manifest
        .id(ObjectKind::Field, "Media.name")
        .expect("active source name field identity");
    let age_id = *initial_manifest
        .id(ObjectKind::Field, "Media.age")
        .expect("active source age field identity");
    let old_row_path = directory.path().join("Media/north/42.orna");
    let row_contents = fs::read(&old_row_path).expect("old loose row");

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    assert!(output.status.success(), "mv stderr: {:?}", output.stderr);
    assert_eq!(
        fs::read(&source_path).unwrap(),
        source_after_crash.as_bytes()
    );
    assert_eq!(
        fs::read(directory.path().join("Archive/north/42.orna")).unwrap(),
        row_contents
    );
    assert!(!old_row_path.exists());
    let renamed_manifest = ObjectIdentityManifest::read_worktree(
        &Repository::discover(directory.path()).expect("repository"),
    )
    .expect("manifest read")
    .expect("renamed manifest");
    assert_eq!(
        renamed_manifest.id(ObjectKind::Table, "Archive"),
        Some(&table_id)
    );
    assert_eq!(
        renamed_manifest.id(ObjectKind::Field, "Archive.name"),
        Some(&name_id)
    );
    assert_eq!(
        renamed_manifest.id(ObjectKind::Field, "Archive.age"),
        Some(&age_id)
    );
    assert!(renamed_manifest.id(ObjectKind::Field, "Media.name").is_none());
    assert!(renamed_manifest.id(ObjectKind::Field, "Media.age").is_none());
    assert_ne!(manifest_before_recovery, fs::read(&manifest_path).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let recovered_metadata = fs::metadata(&source_path).expect("recovered source metadata");
        assert_eq!(source_metadata.ino(), recovered_metadata.ino());
    }
}

#[test]
fn mv_resumes_after_source_and_row_directory_were_renamed() {
    let (directory, initial_manifest) = initialized_project_with_loose_row();
    let source_path = directory.path().join("main.orna");
    let source_after_crash = SOURCE_WITH_ROW.replace("table Media", "table Archive");
    fs::write(&source_path, &source_after_crash).expect("simulate source rename");
    let source_metadata = fs::metadata(&source_path).expect("source metadata");
    fs::rename(
        directory.path().join("Media"),
        directory.path().join("Archive"),
    )
    .expect("simulate row-directory rename");

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);

    assert!(output.status.success(), "mv stderr: {:?}", output.stderr);
    assert_eq!(
        fs::read(&source_path).unwrap(),
        source_after_crash.as_bytes()
    );
    assert_eq!(
        fs::read(directory.path().join("Archive/north/42.orna")).unwrap(),
        ROW_SOURCE.as_bytes()
    );
    assert!(!directory.path().join("Media").exists());
    let repository = Repository::discover(directory.path()).expect("repository");
    let manifest = ObjectIdentityManifest::read_worktree(&repository)
        .expect("manifest read")
        .expect("updated manifest");
    assert_eq!(
        manifest.id(ObjectKind::Table, "Archive"),
        initial_manifest.id(ObjectKind::Table, "Media")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let recovered_metadata = fs::metadata(&source_path).expect("recovered source metadata");
        assert_eq!(source_metadata.ino(), recovered_metadata.ino());
    }
}

#[test]
fn mv_refuses_an_existing_loose_row_destination() {
    let (directory, _) = initialized_project_with_loose_row();
    let source_path = directory.path().join("main.orna");
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    let source_before = fs::read(&source_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    let row_before = fs::read(directory.path().join("Media/north/42.orna")).unwrap();
    let destination = directory.path().join("Archive");
    fs::create_dir(&destination).expect("existing destination");
    fs::write(destination.join("preserve.txt"), b"existing").unwrap();

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    assert!(!output.status.success(), "destination collision should fail");
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(
        fs::read(directory.path().join("Media/north/42.orna")).unwrap(),
        row_before
    );
    assert_eq!(fs::read(destination.join("preserve.txt")).unwrap(), b"existing");
}

#[cfg(unix)]
#[test]
fn mv_refuses_a_symlinked_loose_row_directory() {
    use std::os::unix::fs::symlink;

    let (directory, _) = initialized_project_with_source(SOURCE_WITH_ROW);
    let source_path = directory.path().join("main.orna");
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    let source_before = fs::read(&source_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    let external = tempfile::tempdir().expect("external row directory");
    fs::write(external.path().join("north.orna"), b"external").unwrap();
    symlink(external.path(), directory.path().join("Media")).expect("row-directory symlink");

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "symlinked path should fail");
    assert!(
        stderr.contains("E2100") && stderr.contains("loose-row directory is unavailable or unsafe"),
        "unexpected symlink-path diagnostic: {stderr}"
    );
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(fs::read(external.path().join("north.orna")).unwrap(), b"external");
    assert!(!directory.path().join("Archive").exists());
}

#[cfg(unix)]
#[test]
fn mv_refuses_a_symlinked_row_destination() {
    use std::os::unix::fs::symlink;

    let (directory, _) = initialized_project_with_source(SOURCE_WITH_ROW);
    let source_path = directory.path().join("main.orna");
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    let source_before = fs::read(&source_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    let external = tempfile::tempdir().expect("external destination");
    fs::write(external.path().join("preserve.txt"), b"external").unwrap();
    let destination = directory.path().join("Archive");
    symlink(external.path(), &destination).expect("destination symlink");

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "symlink destination should fail");
    assert!(
        stderr.contains("E2100") && stderr.contains("loose-row directory is unavailable or unsafe"),
        "unexpected symlink-destination diagnostic: {stderr}"
    );
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(fs::read(external.path().join("preserve.txt")).unwrap(), b"external");
    assert!(fs::symlink_metadata(destination).unwrap().file_type().is_symlink());
}

#[cfg(unix)]
#[test]
fn mv_rolls_back_source_manifest_and_loose_rows_when_manifest_write_fails() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, _) = initialized_project_with_loose_row();
    let source_path = directory.path().join("main.orna");
    let manifest_dir = directory.path().join(".orna");
    let manifest_path = manifest_dir.join("object-identities.cbor");
    let source_before = fs::read(&source_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    let row_before = fs::read(directory.path().join("Media/north/42.orna")).unwrap();
    fs::set_permissions(&manifest_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);

    fs::set_permissions(&manifest_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!output.status.success(), "manifest write should fail");
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(
        fs::read(directory.path().join("Media/north/42.orna")).unwrap(),
        row_before
    );
    assert!(!directory.path().join("Archive").exists());
}

#[cfg(unix)]
#[test]
fn mv_rolls_back_when_source_write_fails() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, _) = initialized_project();
    let source_path = directory.path().join("main.orna");
    let manifest_path = directory.path().join(".orna/object-identities.cbor");
    let source_before = fs::read(&source_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();

    let output = run_cli(directory.path(), &["mv", "Media", "Archive"]);

    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!output.status.success(), "source write should fail");
    assert_eq!(fs::read(&source_path).unwrap(), source_before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert!(!directory.path().join("Archive").exists());
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
