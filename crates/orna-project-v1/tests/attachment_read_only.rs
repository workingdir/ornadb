use std::{error::Error, fs, path::Path, process::Command};

use orna_project_v1::{
    AttachmentError, AttachedDatabaseSession, PACKAGE_PIN_MANIFEST_PATH, PackageResolver,
    PinnedDatabase, ProjectLoader,
};
use orna_repository_v1::Repository;
use tempfile::TempDir;

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn repository(files: &[(&str, &str)]) -> (TempDir, Repository, String) {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "--quiet"]);
    git(directory.path(), &["config", "user.name", "kierandrewett"]);
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(directory.path(), &["config", "commit.gpgsign", "false"]);
    for (path, contents) in files {
        let path = directory.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    git(directory.path(), &["add", "--all"]);
    git(directory.path(), &["commit", "--quiet", "-m", "fixture snapshot"]);
    let commit = git(directory.path(), &["rev-parse", "HEAD"]);
    let repository = Repository::discover(directory.path()).unwrap();
    (directory, repository, commit)
}

fn write_commit(directory: &Path, path: &str, contents: &str) -> String {
    let path = directory.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    git(directory, &["add", "--all"]);
    git(directory, &["commit", "--quiet", "-m", "move package head"]);
    git(directory, &["rev-parse", "HEAD"])
}

#[test]
fn attached_history_reads_its_exact_commit_without_changing_repository_state() {
    let (_primary_dir, primary_repository, primary_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-primary.orna"),
    )]);
    let (history_dir, history_repository, history_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-package.orna"),
    )]);
    let moved_head = write_commit(
        history_dir.path(),
        "main.orna",
        &include_str!("fixtures/attach-package.orna").replace("42", "99"),
    );
    let worktree_source = include_str!("fixtures/attach-package.orna").replace("42", "123");
    fs::write(history_dir.path().join("main.orna"), &worktree_source).unwrap();
    let head_before = git(history_dir.path(), &["rev-parse", "HEAD"]);
    let status_before = git(history_dir.path(), &["status", "--porcelain"]);

    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader)
        .unwrap();
    let history = PinnedDatabase::resolve(
        "archive",
        history_repository.clone(),
        &history_commit,
        loader,
    )
    .unwrap();
    let standard = PinnedDatabase::resolve(
        "std",
        history_repository.clone(),
        &history_commit,
        loader,
    )
    .unwrap();
    let mut session = AttachedDatabaseSession::new(primary).unwrap();
    session.attach_database(history).unwrap();
    session.attach_database(standard).unwrap();

    assert_eq!(head_before, moved_head);
    assert!(session.validate_write_target("app").is_ok());
    assert!(session.is_writable_database("app"));
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(!session.is_writable_database("archive"));
    assert!(matches!(
        session.validate_write_target("std"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(matches!(
        session.validate_write_target("sys"),
        Err(AttachmentError::SystemDatabaseReadOnly)
    ));
    assert!(matches!(
        session.validate_write_target("missing"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert!(session
        .database("archive")
        .unwrap()
        .project()
        .modules()
        .iter()
        .any(|module| module.source.contains("= 42")));
    let mut in_flight = session.clone();
    assert!(matches!(
        session.detach_database("../outside"),
        Err(AttachmentError::InvalidName)
    ));
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(matches!(
        session.detach_database("app"),
        Err(AttachmentError::PrimaryDatabaseCannotDetach)
    ));
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(matches!(
        session.detach_database("sys"),
        Err(AttachmentError::SystemDatabaseCannotDetach)
    ));
    assert!(matches!(
        session.validate_write_target("sys"),
        Err(AttachmentError::SystemDatabaseReadOnly)
    ));
    session.detach_database("archive").unwrap();
    assert!(session.database("archive").is_none());
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert_eq!(
        in_flight
            .database("archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        history_commit
    );
    assert!(matches!(
        in_flight.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    let detached_clone = session.clone();
    let replacement =
        PinnedDatabase::resolve("archive", history_repository, &moved_head, loader).unwrap();
    session.attach_database(replacement).unwrap();
    assert_eq!(
        session.database("archive").unwrap().pin().commit().as_str(),
        moved_head
    );
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert_eq!(
        in_flight
            .database("archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        history_commit
    );
    assert!(detached_clone.database("archive").is_none());
    assert!(matches!(
        detached_clone.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    in_flight.detach_database("archive").unwrap();
    assert!(matches!(
        in_flight.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert_eq!(
        session.database("archive").unwrap().pin().commit().as_str(),
        moved_head
    );
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert_eq!(git(history_dir.path(), &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(history_dir.path(), &["status", "--porcelain"]), status_before);
    assert_eq!(
        fs::read_to_string(history_dir.path().join("main.orna")).unwrap(),
        worktree_source
    );
}

#[test]
fn package_pin_failures_are_redacted_and_fail_before_a_session_is_returned() {
    let (_valid_dir, _valid_repository, valid_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-package.orna"),
    )]);
    let manifest = format!("widgets {valid_commit}\n");
    let (_primary_dir, primary_repository, primary_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
        (PACKAGE_PIN_MANIFEST_PATH, &manifest),
    ]);
    let primary = PinnedDatabase::resolve(
        "app",
        primary_repository,
        &primary_commit,
        ProjectLoader::default(),
    )
    .unwrap();

    let missing_repository = PackageResolver::new(
        std::iter::empty::<(String, Repository)>(),
        ProjectLoader::default(),
    )
    .unwrap();
    let error = missing_repository
        .resolve_for_parent(primary.clone())
        .unwrap_err();
    assert!(matches!(error, AttachmentError::RepositoryUnavailable));
    assert_eq!(error.to_string(), "pinned package repository is unavailable");

    let (_wrong_dir, wrong_repository, _) = repository(&[(
        "main.orna",
        &include_str!("fixtures/attach-package.orna").replace("42", "77"),
    )]);
    let wrong_pin = PackageResolver::new(
        [("widgets".to_owned(), wrong_repository)],
        ProjectLoader::default(),
    )
    .unwrap();
    let error = wrong_pin.resolve_for_parent(primary).unwrap_err();
    assert!(matches!(error, AttachmentError::PinUnavailable));
    assert_eq!(error.to_string(), "pinned package commit is unavailable");

    let (invalid_dir, invalid_repository, invalid_commit) =
        repository(&[("README.md", "not an Orna project")]);
    let invalid_repository_path = invalid_dir.path().to_string_lossy().into_owned();
    let invalid_manifest = format!("broken {invalid_commit}\n");
    let (_invalid_parent_dir, invalid_parent_repository, invalid_parent_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
        (PACKAGE_PIN_MANIFEST_PATH, &invalid_manifest),
    ]);
    let invalid_parent = PinnedDatabase::resolve(
        "app",
        invalid_parent_repository,
        &invalid_parent_commit,
        ProjectLoader::default(),
    )
    .unwrap();
    let invalid_package = PackageResolver::new(
        [("broken".to_owned(), invalid_repository)],
        ProjectLoader::default(),
    )
    .unwrap();
    let error = invalid_package
        .resolve_for_parent(invalid_parent)
        .unwrap_err();
    assert!(matches!(error, AttachmentError::PinnedPackageInvalid));
    assert_eq!(
        error.to_string(),
        "pinned package is not a loadable database"
    );
    assert!(!format!("{error:?}").contains(&invalid_repository_path));
    assert!(Error::source(&error).is_none());
}

#[test]
fn failed_later_pin_does_not_return_a_partial_writable_session() {
    let (package_dir, package_repository, package_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-package.orna"),
    )]);
    let missing_commit = "a".repeat(40);
    let manifest = format!("widgets {package_commit}\nmissing {missing_commit}\n");
    let (primary_dir, primary_repository, primary_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
        (PACKAGE_PIN_MANIFEST_PATH, &manifest),
    ]);
    let parent_head_before = git(primary_dir.path(), &["rev-parse", "HEAD"]);
    let parent_status_before = git(primary_dir.path(), &["status", "--porcelain"]);
    let package_head_before = git(package_dir.path(), &["rev-parse", "HEAD"]);
    let package_status_before = git(package_dir.path(), &["status", "--porcelain"]);

    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve(
        "app",
        primary_repository,
        &primary_commit,
        loader,
    )
    .unwrap();
    let resolver =
        PackageResolver::new([("widgets".to_owned(), package_repository.clone())], loader)
            .unwrap();
    let error = resolver.resolve_for_parent(primary).unwrap_err();

    assert!(matches!(error, AttachmentError::RepositoryUnavailable));
    assert_eq!(
        git(primary_dir.path(), &["rev-parse", "HEAD"]),
        parent_head_before
    );
    assert_eq!(
        git(primary_dir.path(), &["status", "--porcelain"]),
        parent_status_before
    );
    assert_eq!(
        git(package_dir.path(), &["rev-parse", "HEAD"]),
        package_head_before
    );
    assert_eq!(
        git(package_dir.path(), &["status", "--porcelain"]),
        package_status_before
    );
}
