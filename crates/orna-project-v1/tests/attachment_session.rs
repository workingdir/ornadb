use std::{fs, path::Path, process::Command};

use orna_project_v1::{
    AttachmentError, AttachedDatabaseSession, PACKAGE_PIN_MANIFEST_PATH, PackagePinManifest,
    PackageResolver, PinnedDatabase, ProjectLoader,
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

#[test]
fn attach_and_detach_refusals_preserve_the_primary_and_optional_std() {
    let (_primary_dir, primary_repository, primary_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-primary.orna"),
    )]);
    let (_package_dir, package_repository, package_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-package.orna"),
    )]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve(
        "app",
        primary_repository,
        &primary_commit,
        loader,
    )
    .unwrap();
    let archive = PinnedDatabase::resolve(
        "archive",
        package_repository.clone(),
        &package_commit,
        loader,
    )
    .unwrap();
    let std = PinnedDatabase::resolve("std", package_repository.clone(), &package_commit, loader)
        .unwrap();
    let host_sys = PinnedDatabase::resolve("sys", package_repository, &package_commit, loader)
        .unwrap();

    let mut session = AttachedDatabaseSession::new(primary).unwrap();
    session.attach_database(archive.clone()).unwrap();
    session.attach_database(std).unwrap();
    assert!(session.database("app").is_some());
    assert!(session.database("archive").is_some());
    assert!(session
        .module_inputs()
        .iter()
        .any(|module| module.logical_path == "archive.orna"));
    assert!(!session.standard_sources().is_empty());

    assert!(matches!(
        session.attach_database(archive),
        Err(AttachmentError::DuplicateAttachment)
    ));
    assert!(matches!(
        session.attach_database(host_sys),
        Err(AttachmentError::SystemDatabaseCannotAttach)
    ));
    assert!(matches!(
        session.detach_database("app"),
        Err(AttachmentError::PrimaryDatabaseCannotDetach)
    ));
    assert!(matches!(
        session.detach_database("sys"),
        Err(AttachmentError::SystemDatabaseCannotDetach)
    ));
    assert!(matches!(
        session.detach_database("missing"),
        Err(AttachmentError::AttachmentNotFound)
    ));
    assert!(matches!(
        session.detach_database("../outside"),
        Err(AttachmentError::InvalidName)
    ));
    assert!(session.database("app").is_some());
    assert!(session.database("archive").is_some());

    session.detach_database("std").unwrap();
    assert!(session.standard_sources().is_empty());
    session.detach_database("archive").unwrap();
    assert!(session.database("archive").is_none());
    assert!(!session
        .module_inputs()
        .iter()
        .any(|module| module.logical_path == "archive.orna"));
    assert!(matches!(
        session.detach_database("archive"),
        Err(AttachmentError::AttachmentNotFound)
    ));
    assert!(session.database("app").is_some());
}

#[test]
fn package_resolution_treats_missing_manifest_as_empty_and_refuses_wrong_repo_pins() {
    let (_empty_dir, empty_repository, empty_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-primary.orna"),
    )]);
    let loader = ProjectLoader::default();
    let empty_primary = PinnedDatabase::resolve(
        "app",
        empty_repository,
        &empty_commit,
        loader,
    )
    .unwrap();
    let no_packages = PackageResolver::new([], loader)
        .unwrap()
        .resolve_for_parent(empty_primary)
        .unwrap();
    assert_eq!(no_packages.attached().count(), 0);

    let (_malformed_dir, malformed_repository, malformed_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
        (".orna/packages/nested", "not a pin manifest"),
    ]);
    let malformed_primary = PinnedDatabase::resolve(
        "app",
        malformed_repository,
        &malformed_commit,
        loader,
    )
    .unwrap();
    assert!(matches!(
        PackageResolver::new([], loader)
            .unwrap()
            .resolve_for_parent(malformed_primary),
        Err(AttachmentError::MalformedManifest)
    ));

    let (_package_dir, package_repository, package_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-package.orna"),
    )]);
    let (_wrong_dir, wrong_repository, _) = repository(&[(
        "main.orna",
        &include_str!("fixtures/attach-package.orna").replace("42", "99"),
    )]);
    let package_manifest = format!("widgets {package_commit}\n");
    let (_primary_dir, primary_repository, primary_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
        (PACKAGE_PIN_MANIFEST_PATH, &package_manifest),
    ]);
    let primary = PinnedDatabase::resolve(
        "app",
        primary_repository.clone(),
        &primary_commit,
        loader,
    )
    .unwrap();

    assert!(matches!(
        PackageResolver::new([("widgets".to_owned(), wrong_repository)], loader)
            .unwrap()
            .resolve_for_parent(primary.clone()),
        Err(AttachmentError::PinUnavailable)
    ));
    let session = PackageResolver::new([("widgets".to_owned(), package_repository)], loader)
        .unwrap()
        .resolve_for_parent(primary)
        .unwrap();
    assert_eq!(
        session.database("widgets").unwrap().pin().commit().as_str(),
        package_commit
    );
    assert!(PackagePinManifest::parse(&format!("widgets {package_commit}\n")).is_ok());
    assert!(PackagePinManifest::parse("widgets 1.2.3\n").is_err());
    assert!(PackagePinManifest::parse(&format!("widgets {}\n", &package_commit[..12])).is_err());
    assert!(PackagePinManifest::parse(&format!(
        "widgets {}\n",
        package_commit.to_uppercase()
    ))
    .is_err());
}
