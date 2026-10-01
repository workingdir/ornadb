use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_project_v1::{AttachmentError, AttachedDatabaseSession, PinnedDatabase, ProjectLoader};
use orna_repository_v1::Repository;
use orna_value_v1::Value;
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

fn repository(source: &str) -> (TempDir, Repository, String) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("main.orna"), source).unwrap();
    git(directory.path(), &["init", "--quiet"]);
    git(
        directory.path(),
        &["config", "user.name", "kierandrewett"],
    );
    git(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    git(directory.path(), &["config", "commit.gpgsign", "false"]);
    git(directory.path(), &["add", "main.orna"]);
    git(directory.path(), &["commit", "--quiet", "-m", "route fixture"]);
    let commit = git(directory.path(), &["rev-parse", "HEAD"]);
    let repository = Repository::discover(directory.path()).unwrap();
    (directory, repository, commit)
}

fn commit_source(directory: &Path, source: &str) -> String {
    fs::write(directory.join("main.orna"), source).unwrap();
    git(directory, &["add", "main.orna"]);
    git(directory, &["commit", "--quiet", "-m", "advance package snapshot"]);
    git(directory, &["rev-parse", "HEAD"])
}

fn commit_module_source(directory: &Path, logical_path: &str, source: &str) -> String {
    fs::write(directory.join(logical_path), source).unwrap();
    git(directory, &["add", logical_path]);
    git(
        directory,
        &["commit", "--quiet", "-m", "advance nested module snapshot"],
    );
    git(directory, &["rev-parse", "HEAD"])
}

#[test]
fn main_alias_and_prefix_alias_import_their_own_pinned_root_modules() {
    let (_primary_dir, primary_repository, primary_commit) =
        repository(include_str!("fixtures/attachment-route-primary.orna"));
    let (package_dir, package_repository, main_alias_commit) = repository(include_str!(
        "fixtures/attachment-route-package.orna"
    ));
    let longer_alias_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "43"),
    );

    let loader = ProjectLoader::default();
    let primary =
        PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader).unwrap();
    let main_alias = PinnedDatabase::resolve(
        "main",
        package_repository.clone(),
        &main_alias_commit,
        loader,
    )
    .unwrap();
    let longer_alias = PinnedDatabase::resolve(
        "main_archive",
        package_repository.clone(),
        &longer_alias_commit,
        loader,
    )
    .unwrap();
    let mut databases = AttachedDatabaseSession::new(primary).unwrap();
    databases.attach_database(main_alias).unwrap();
    databases.attach_database(longer_alias).unwrap();
    let modules = databases.module_inputs();
    assert_eq!(
        modules
            .iter()
            .filter(|module| module.logical_path == "main.orna")
            .count(),
        1,
        "the primary retains the root module route"
    );
    assert!(modules.iter().any(|module| {
        module.logical_path == "main.orna" && module.source.contains("primary_value")
    }));
    assert!(modules.iter().any(|module| {
        module.logical_path == "main/main.orna" && module.source.contains("= 42")
    }));
    assert!(modules.iter().any(|module| {
        module.logical_path == "main_archive.orna" && module.source.contains("= 43")
    }));

    let mut session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    // Prefix aliases are exact names; a failed near-match must leave the
    // evaluator able to recover by importing either real root alias.
    assert!(session.submit("use main_arch;").is_err());
    assert_eq!(session.submit("use main;"), Ok(None));
    assert_eq!(
        session.submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(session.submit("use main_archive;"), Ok(None));
    assert_eq!(
        session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        main_alias_commit
    );
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        longer_alias_commit
    );

    // Removing the prefix alias leaves the primary root and exact `main`
    // route available to a newly admitted evaluator.
    let mut without_archive = databases.clone();
    without_archive.detach_database("main_archive").unwrap();
    let remaining_modules = without_archive.module_inputs();
    assert!(remaining_modules.iter().any(|module| {
        module.logical_path == "main.orna" && module.source.contains("primary_value")
    }));
    assert!(remaining_modules.iter().any(|module| {
        module.logical_path == "main/main.orna" && module.source.contains("= 42")
    }));
    assert!(!remaining_modules
        .iter()
        .any(|module| module.logical_path == "main_archive.orna"));
    let mut main_only_session = AdmittedReplSession::from_attached_database_session(
        &without_archive,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(main_only_session.submit("use main;"), Ok(None));
    assert_eq!(
        main_only_session.submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert!(main_only_session.submit("use main_archive;").is_err());
    assert!(databases.database("main_archive").is_some());
    assert_eq!(
        session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );

    // An admitted REPL keeps the exact alias pins it was built from when the
    // mutable attachment session later detaches or replaces one of them.
    let historical_session = session.clone();
    assert!(matches!(
        databases.detach_database("main_arch"),
        Err(AttachmentError::AttachmentNotFound)
    ));
    let after_refused_detach = databases.module_inputs();
    assert!(after_refused_detach.iter().any(|module| {
        module.logical_path == "main/main.orna" && module.source.contains("= 42")
    }));
    assert!(after_refused_detach.iter().any(|module| {
        module.logical_path == "main_archive.orna" && module.source.contains("= 43")
    }));
    databases.detach_database("main").unwrap();
    // A stale near-prefix request after pruning `main` must not remove the
    // surviving, longer root alias.
    assert!(matches!(
        databases.detach_database("main_arch"),
        Err(AttachmentError::AttachmentNotFound)
    ));
    let after_exact_detach = databases.module_inputs();
    assert!(!after_exact_detach
        .iter()
        .any(|module| module.logical_path == "main/main.orna"));
    assert!(after_exact_detach.iter().any(|module| {
        module.logical_path == "main_archive.orna" && module.source.contains("= 43")
    }));
    let mut detached_session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert!(detached_session.submit("use main;").is_err());
    assert_eq!(detached_session.submit("use main_archive;"), Ok(None));
    assert_eq!(
        detached_session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );

    // Detaching the surviving archive from this already-pruned clone leaves
    // the primary root alone and does not mutate the source session.
    let mut archive_detached = databases.clone();
    archive_detached.detach_database("main_archive").unwrap();
    let after_archive_detach = archive_detached.module_inputs();
    assert_eq!(
        after_archive_detach
            .iter()
            .filter(|module| module.logical_path == "main.orna")
            .count(),
        1
    );
    assert!(after_archive_detach.iter().any(|module| {
        module.logical_path == "main.orna" && module.source.contains("primary_value")
    }));
    assert!(!after_archive_detach
        .iter()
        .any(|module| module.logical_path == "main/main.orna"));
    assert!(!after_archive_detach
        .iter()
        .any(|module| module.logical_path == "main_archive.orna"));
    assert!(archive_detached.database("main_archive").is_none());
    assert!(databases.database("main_archive").is_some());

    // Detach prunes one exact route immediately; restoring the same pin must
    // recover only that route while preserving the overlapping alias.
    let recovered_pin = PinnedDatabase::resolve(
        "main",
        package_repository.clone(),
        &main_alias_commit,
        loader,
    )
    .unwrap();
    databases.attach_database(recovered_pin).unwrap();
    let recovered_modules = databases.module_inputs();
    assert!(recovered_modules.iter().any(|module| {
        module.logical_path == "main/main.orna" && module.source.contains("= 42")
    }));
    assert!(recovered_modules.iter().any(|module| {
        module.logical_path == "main_archive.orna" && module.source.contains("= 43")
    }));
    assert_eq!(
        databases
            .database("main")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        main_alias_commit
    );
    databases.detach_database("main").unwrap();

    let replacement_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "99"),
    );
    let replacement = PinnedDatabase::resolve(
        "main",
        package_repository,
        &replacement_commit,
        loader,
    )
    .unwrap();
    databases.attach_database(replacement).unwrap();
    let mut replacement_session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert_eq!(replacement_session.submit("use main;"), Ok(None));
    assert_eq!(
        replacement_session.submit("main.package_value()"),
        Ok(Some(Value::int(99.into())))
    );
    assert_eq!(
        replacement_session.submit("use main_archive;"),
        Ok(None)
    );
    assert_eq!(
        replacement_session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
    assert_eq!(
        historical_session
            .clone()
            .submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(
        historical_session
            .clone()
            .submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );

    // Detaching from the source session affects later admissions only; both
    // already admitted evaluators retain their own archive pin and main pin.
    databases.detach_database("main_archive").unwrap();
    let after_source_detach = databases.module_inputs();
    assert!(after_source_detach.iter().any(|module| {
        module.logical_path == "main.orna" && module.source.contains("primary_value")
    }));
    assert!(after_source_detach.iter().any(|module| {
        module.logical_path == "main/main.orna" && module.source.contains("= 99")
    }));
    assert!(!after_source_detach
        .iter()
        .any(|module| module.logical_path == "main_archive.orna"));
    let mut after_detach_session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert!(after_detach_session
        .attached_databases()
        .unwrap()
        .database("main_archive")
        .is_none());
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        longer_alias_commit
    );
    assert_eq!(
        replacement_session
            .attached_databases()
            .unwrap()
            .database("main_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        longer_alias_commit
    );
    // A cloned admitted session retains the original pair of pins even after
    // the source map has detached the archive and replaced the `main` alias.
    assert_eq!(
        historical_session
            .attached_databases()
            .unwrap()
            .database("main")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        main_alias_commit
    );
    assert_eq!(
        historical_session
            .attached_databases()
            .unwrap()
            .database("main_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        longer_alias_commit
    );
    assert_eq!(after_detach_session.submit("use main;"), Ok(None));
    assert_eq!(
        after_detach_session.submit("main.package_value()"),
        Ok(Some(Value::int(99.into())))
    );
    assert!(after_detach_session
        .submit("use main_archive;")
        .is_err());
    assert_eq!(
        session.submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(
        session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
    assert_eq!(
        replacement_session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
}

#[test]
fn overlapping_attachment_aliases_decode_nested_module_paths_exactly() {
    let (_primary_dir, primary_repository, primary_commit) = repository(include_str!(
        "fixtures/attachment-route-primary.orna"
    ));
    let (package_dir, package_repository, _) = repository(include_str!(
        "fixtures/attachment-route-package-with-archive.orna"
    ));
    let main_alias_commit = commit_module_source(
        package_dir.path(),
        "archive.orna",
        include_str!("fixtures/attachment-route-package-archive.orna"),
    );
    let _longer_root_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "43"),
    );
    let longer_alias_commit = commit_module_source(
        package_dir.path(),
        "archive.orna",
        &include_str!("fixtures/attachment-route-package-archive.orna").replace("41", "44"),
    );

    let loader = ProjectLoader::default();
    let primary =
        PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader).unwrap();
    let main_alias = PinnedDatabase::resolve(
        "main",
        package_repository.clone(),
        &main_alias_commit,
        loader,
    )
    .unwrap();
    let longer_alias = PinnedDatabase::resolve(
        "main_archive",
        package_repository,
        &longer_alias_commit,
        loader,
    )
    .unwrap();
    let mut databases = AttachedDatabaseSession::new(primary).unwrap();
    databases.attach_database(main_alias).unwrap();
    databases.attach_database(longer_alias).unwrap();

    let modules = databases.module_inputs();
    for (path, value) in [
        ("main/main.orna", "42"),
        ("main/archive.orna", "41"),
        ("main_archive.orna", "43"),
        ("main_archive/archive.orna", "44"),
    ] {
        assert!(
            modules
                .iter()
                .any(|module| module.logical_path == path && module.source.contains(value)),
            "missing {path} with {value}; routes: {:?}",
            modules
                .iter()
                .map(|module| (&module.logical_path, &module.source))
                .collect::<Vec<_>>()
        );
    }

    let mut session =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert!(session.submit("use main_arch.archive;").is_err());
    assert_eq!(session.submit("use main;"), Ok(None));
    assert_eq!(
        session.submit("main.package_value()"),
        Ok(Some(Value::int(42.into())))
    );
    assert_eq!(session.submit("use main.archive as short_child;"), Ok(None));
    assert_eq!(
        session.submit("short_child.nested_value()"),
        Ok(Some(Value::int(41.into())))
    );
    assert_eq!(session.submit("use main_archive;"), Ok(None));
    assert_eq!(
        session.submit("main_archive.package_value()"),
        Ok(Some(Value::int(43.into())))
    );
    assert_eq!(
        session.submit("use main_archive.archive as long_child;"),
        Ok(None)
    );
    assert_eq!(
        session.submit("long_child.nested_value()"),
        Ok(Some(Value::int(44.into())))
    );
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        main_alias_commit
    );
    assert_eq!(
        session
            .attached_databases()
            .unwrap()
            .database("main_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        longer_alias_commit
    );
}

#[test]
fn nested_admitted_clones_preserve_archive_pin_identity_after_alias_churn() {
    let (_primary_dir, primary_repository, primary_commit) =
        repository(include_str!("fixtures/attachment-route-primary.orna"));
    let (package_dir, package_repository, main_commit) =
        repository(include_str!("fixtures/attachment-route-package.orna"));
    let archive_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "43"),
    );
    let replacement_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-route-package.orna").replace("42", "99"),
    );
    let loader = ProjectLoader::default();
    let primary =
        PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader).unwrap();
    let main = PinnedDatabase::resolve(
        "main",
        package_repository.clone(),
        &main_commit,
        loader,
    )
    .unwrap();
    let archive = PinnedDatabase::resolve(
        "main_archive",
        package_repository.clone(),
        &archive_commit,
        loader,
    )
    .unwrap();
    let mut databases = AttachedDatabaseSession::new(primary).unwrap();
    databases.attach_database(main).unwrap();
    databases.attach_database(archive).unwrap();
    let admitted =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();

    // Admission snapshots each alias pin. Later source-map churn affects only
    // new admissions, including when an older admitted snapshot is cloned.
    databases.detach_database("main").unwrap();
    databases
        .attach_database(
            PinnedDatabase::resolve(
                "main",
                package_repository,
                &replacement_commit,
                loader,
            )
            .unwrap(),
        )
        .unwrap();
    databases.detach_database("main_archive").unwrap();

    let mut current_admission =
        AdmittedReplSession::from_attached_database_session(&databases, Limits::default()).unwrap();
    assert_eq!(current_admission.submit("use main;"), Ok(None));
    assert_eq!(
        current_admission.submit("main.package_value()"),
        Ok(Some(Value::int(99.into())))
    );
    assert!(current_admission.submit("use main_archive;").is_err());

    let mut first_clone = admitted.clone();
    let mut second_clone = first_clone.clone();
    for clone in [&first_clone, &second_clone] {
        let attached = clone.attached_databases().unwrap();
        assert_eq!(
            attached.database("main").unwrap().pin().commit().as_str(),
            main_commit
        );
        assert_eq!(
            attached
                .database("main_archive")
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            archive_commit
        );
    }
    for clone in [&mut first_clone, &mut second_clone] {
        assert_eq!(clone.submit("use main;"), Ok(None));
        assert_eq!(
            clone.submit("main.package_value()"),
            Ok(Some(Value::int(42.into())))
        );
        assert_eq!(clone.submit("use main_archive;"), Ok(None));
        assert_eq!(
            clone.submit("main_archive.package_value()"),
            Ok(Some(Value::int(43.into())))
        );
    }
}
