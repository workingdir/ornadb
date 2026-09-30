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

fn routed_module_source(
    session: &AttachedDatabaseSession,
    logical_path: &str,
) -> Option<String> {
    session
        .module_inputs()
        .into_iter()
        .find(|module| module.logical_path == logical_path)
        .map(|module| module.source)
}

fn routed_relation_source(
    session: &AttachedDatabaseSession,
    table_path: &str,
    database: &str,
) -> Option<(String, String)> {
    session
        .relation_sources(table_path)
        .into_iter()
        .find(|source| source.database() == database)
        .map(|source| {
            (
                source.commit().as_str().to_owned(),
                source.row().source().to_owned(),
            )
        })
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
    let later_head = write_commit(
        history_dir.path(),
        "main.orna",
        &include_str!("fixtures/attach-package.orna").replace("42", "88"),
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

    assert_eq!(head_before, later_head);
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
    assert!(routed_module_source(&session, "archive.orna")
        .is_some_and(|source| source.contains("= 42")));
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
    assert!(routed_module_source(&session, "archive.orna").is_none());
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
    assert!(routed_module_source(&in_flight, "archive.orna")
        .is_some_and(|source| source.contains("= 42")));
    assert!(matches!(
        in_flight.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    let detached_clone = session.clone();
    let replacement = PinnedDatabase::resolve(
        "archive",
        history_repository.clone(),
        &moved_head,
        loader,
    )
    .unwrap();
    session.attach_database(replacement).unwrap();
    assert_eq!(
        session.database("archive").unwrap().pin().commit().as_str(),
        moved_head
    );
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(routed_module_source(&session, "archive.orna")
        .is_some_and(|source| source.contains("= 99")));
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
    assert!(routed_module_source(&detached_clone, "archive.orna").is_none());
    let intermediate_clone = session.clone();
    session.detach_database("archive").unwrap();
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert!(routed_module_source(&session, "archive.orna").is_none());
    let newest =
        PinnedDatabase::resolve("archive", history_repository.clone(), &later_head, loader)
            .unwrap();
    session.attach_database(newest).unwrap();
    assert_eq!(
        session.database("archive").unwrap().pin().commit().as_str(),
        later_head
    );
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(routed_module_source(&session, "archive.orna")
        .is_some_and(|source| source.contains("= 88")));
    assert_eq!(
        intermediate_clone
            .database("archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        moved_head
    );
    assert!(matches!(
        intermediate_clone.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(routed_module_source(&intermediate_clone, "archive.orna")
        .is_some_and(|source| source.contains("= 99")));
    in_flight.detach_database("archive").unwrap();
    assert!(matches!(
        in_flight.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert!(routed_module_source(&in_flight, "archive.orna").is_none());
    assert_eq!(
        session.database("archive").unwrap().pin().commit().as_str(),
        later_head
    );
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(routed_module_source(&session, "archive.orna")
        .is_some_and(|source| source.contains("= 88")));
    assert_eq!(git(history_dir.path(), &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(history_dir.path(), &["status", "--porcelain"]), status_before);
    assert_eq!(
        fs::read_to_string(history_dir.path().join("main.orna")).unwrap(),
        worktree_source
    );
}

#[test]
fn cloned_sessions_route_relation_rows_to_their_own_pinned_snapshot() {
    let (_primary_dir, primary_repository, primary_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-routing-primary.orna"),
        ),
        (
            "contacts.orna",
            include_str!("fixtures/attach-routing-table.orna"),
        ),
        (
            "contacts/Contact/1.orna",
            include_str!("fixtures/attach-routing-primary-row.orna"),
        ),
    ]);
    let (archive_dir, archive_repository, archive_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-routing-package.orna"),
        ),
        (
            "contacts.orna",
            include_str!("fixtures/attach-routing-table.orna"),
        ),
        (
            "contacts/Contact/1.orna",
            include_str!("fixtures/attach-routing-package-row.orna"),
        ),
    ]);
    let replacement_commit = write_commit(
        archive_dir.path(),
        "contacts/Contact/1.orna",
        &include_str!("fixtures/attach-routing-package-row.orna").replace("42", "99"),
    );
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader)
        .unwrap();
    let archive = PinnedDatabase::resolve(
        "archive",
        archive_repository.clone(),
        &archive_commit,
        loader,
    )
    .unwrap();
    let mut session = AttachedDatabaseSession::new(primary).unwrap();
    session.attach_database(archive).unwrap();

    let initial_sources = session.relation_sources("contacts/Contact");
    assert_eq!(initial_sources.len(), 2);
    let initial_archive = initial_sources
        .iter()
        .find(|source| source.database() == "archive")
        .unwrap();
    assert_eq!(initial_archive.commit().as_str(), archive_commit);
    assert!(initial_archive.row().source().contains("value: 42"));
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));

    let mut old_clone = session.clone();
    session.detach_database("archive").unwrap();
    let detached_clone = session.clone();
    let detached_sources = session.relation_sources("contacts/Contact");
    assert_eq!(detached_sources.len(), 1);
    assert_eq!(detached_sources[0].database(), "app");
    assert_eq!(detached_sources[0].commit().as_str(), primary_commit);
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));

    let replacement = PinnedDatabase::resolve(
        "archive",
        archive_repository.clone(),
        &replacement_commit,
        loader,
    )
    .unwrap();
    session.attach_database(replacement).unwrap();
    let replacement_sources = session.relation_sources("contacts/Contact");
    assert_eq!(replacement_sources.len(), 2);
    let replacement_archive = replacement_sources
        .iter()
        .find(|source| source.database() == "archive")
        .unwrap();
    assert_eq!(replacement_archive.commit().as_str(), replacement_commit);
    assert!(replacement_archive.row().source().contains("value: 99"));
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));

    let old_sources = old_clone.relation_sources("contacts/Contact");
    assert_eq!(old_sources.len(), 2);
    let old_archive = old_sources
        .iter()
        .find(|source| source.database() == "archive")
        .unwrap();
    assert_eq!(old_archive.commit().as_str(), archive_commit);
    assert!(old_archive.row().source().contains("value: 42"));
    assert!(detached_clone
        .relation_sources("contacts/Contact")
        .iter()
        .all(|source| source.database() == "app"));
    assert!(matches!(
        detached_clone.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));

    old_clone.detach_database("archive").unwrap();
    assert!(old_clone
        .relation_sources("contacts/Contact")
        .iter()
        .all(|source| source.database() == "app"));
    assert!(matches!(
        old_clone.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    let final_sources = session.relation_sources("contacts/Contact");
    let final_archive = final_sources
        .iter()
        .find(|source| source.database() == "archive")
        .unwrap();
    assert_eq!(final_archive.commit().as_str(), replacement_commit);
    assert!(final_archive.row().source().contains("value: 99"));
}

#[test]
fn detaching_one_relation_alias_preserves_other_routes_in_each_clone() {
    let (_primary_dir, primary_repository, primary_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-routing-primary.orna"),
        ),
        (
            "contacts.orna",
            include_str!("fixtures/attach-routing-table.orna"),
        ),
        (
            "contacts/Contact/1.orna",
            include_str!("fixtures/attach-routing-primary-row.orna"),
        ),
    ]);
    let (archive_dir, archive_repository, archive_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-routing-package.orna"),
        ),
        (
            "contacts.orna",
            include_str!("fixtures/attach-routing-table.orna"),
        ),
        (
            "contacts/Contact/1.orna",
            include_str!("fixtures/attach-routing-package-row.orna"),
        ),
    ]);
    let archive_replacement_commit = write_commit(
        archive_dir.path(),
        "contacts/Contact/1.orna",
        &include_str!("fixtures/attach-routing-package-row.orna").replace("42", "99"),
    );
    let catalog_row = include_str!("fixtures/attach-routing-package-row.orna").replace("42", "17");
    let (_catalog_dir, catalog_repository, catalog_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-routing-package.orna"),
        ),
        (
            "contacts.orna",
            include_str!("fixtures/attach-routing-table.orna"),
        ),
        ("contacts/Contact/1.orna", &catalog_row),
    ]);

    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader)
        .unwrap();
    let archive = PinnedDatabase::resolve(
        "archive",
        archive_repository.clone(),
        &archive_commit,
        loader,
    )
    .unwrap();
    let catalog = PinnedDatabase::resolve(
        "catalog",
        catalog_repository.clone(),
        &catalog_commit,
        loader,
    )
    .unwrap();
    let mut session = AttachedDatabaseSession::new(primary).unwrap();
    session.attach_database(archive).unwrap();
    session.attach_database(catalog).unwrap();

    assert_eq!(
        routed_relation_source(&session, "contacts/Contact", "archive")
            .unwrap()
            .0,
        archive_commit
    );
    assert!(routed_relation_source(&session, "contacts/Contact", "archive")
        .unwrap()
        .1
        .contains("value: 42"));
    assert_eq!(
        routed_relation_source(&session, "contacts/Contact", "catalog")
            .unwrap()
            .0,
        catalog_commit
    );

    let mut old_clone = session.clone();
    session.detach_database("archive").unwrap();
    assert!(routed_relation_source(&session, "contacts/Contact", "archive").is_none());
    assert_eq!(
        routed_relation_source(&session, "contacts/Contact", "catalog")
            .unwrap()
            .0,
        catalog_commit
    );
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert!(matches!(
        session.validate_write_target("catalog"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));

    let detached_clone = session.clone();
    let archive_replacement = PinnedDatabase::resolve(
        "archive",
        archive_repository,
        &archive_replacement_commit,
        loader,
    )
    .unwrap();
    session.attach_database(archive_replacement).unwrap();
    assert_eq!(
        routed_relation_source(&session, "contacts/Contact", "archive")
            .unwrap()
            .0,
        archive_replacement_commit
    );
    assert!(routed_relation_source(&session, "contacts/Contact", "archive")
        .unwrap()
        .1
        .contains("value: 99"));
    assert_eq!(
        routed_relation_source(&session, "contacts/Contact", "catalog")
            .unwrap()
            .0,
        catalog_commit
    );

    assert_eq!(
        routed_relation_source(&old_clone, "contacts/Contact", "archive")
            .unwrap()
            .0,
        archive_commit
    );
    assert!(routed_relation_source(&old_clone, "contacts/Contact", "archive")
        .unwrap()
        .1
        .contains("value: 42"));
    assert_eq!(
        routed_relation_source(&detached_clone, "contacts/Contact", "catalog")
            .unwrap()
            .0,
        catalog_commit
    );
    assert!(routed_relation_source(&detached_clone, "contacts/Contact", "archive").is_none());

    old_clone.detach_database("catalog").unwrap();
    assert!(routed_relation_source(&old_clone, "contacts/Contact", "catalog").is_none());
    assert!(routed_relation_source(&old_clone, "contacts/Contact", "archive").is_some());
    assert!(matches!(
        old_clone.validate_write_target("catalog"),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert!(matches!(
        session.validate_write_target("archive"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
    assert!(matches!(
        session.validate_write_target("catalog"),
        Err(AttachmentError::AttachedSnapshotReadOnly)
    ));
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
