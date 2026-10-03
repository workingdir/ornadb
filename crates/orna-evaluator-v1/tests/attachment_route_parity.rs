use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_project_v1::{
    AttachedDatabaseSession, AttachmentError, PackageResolver, PinnedDatabase, ProjectLoader,
    ReboundPathCheckpoint, PACKAGE_PIN_MANIFEST_PATH,
};
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

fn commit_snapshot(
    directory: &Path,
    main_source: &str,
    manifest: Option<&str>,
    message: &str,
) -> String {
    fs::write(directory.join("main.orna"), main_source).unwrap();
    let manifest_path = directory.join(PACKAGE_PIN_MANIFEST_PATH);
    if let Some(manifest) = manifest {
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(manifest_path, manifest).unwrap();
    } else if manifest_path.exists() {
        fs::remove_file(manifest_path).unwrap();
    }
    git(directory, &["add", "--all"]);
    git(directory, &["commit", "--quiet", "-m", message]);
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
    let (primary_dir, primary_repository, _) = repository(include_str!(
        "fixtures/attachment-alias-precedence-primary.orna"
    ));
    let primary_commit = commit_module_source(
        primary_dir.path(),
        "archive.orna",
        include_str!("fixtures/attachment-alias-precedence-primary-archive.orna"),
    );
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
        &include_str!("fixtures/attachment-route-package-with-archive.orna")
            .replace("42", "43"),
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
        ("archive.orna", "= 7"),
        ("main/main.orna", "use main.archive as child_archive;"),
        ("main/archive.orna", "41"),
        (
            "main_archive.orna",
            "use main_archive.archive as child_archive;",
        ),
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
fn attached_alias_imports_obey_local_explicit_and_wildcard_precedence() {
    let (_primary_dir, primary_repository, primary_commit) =
        repository(include_str!("fixtures/attachment-route-primary.orna"));
    let (package_dir, package_repository, _) = repository(include_str!(
        "fixtures/attachment-alias-precedence-package.orna"
    ));
    let short_alias_commit = git(package_dir.path(), &["rev-parse", "HEAD"]);
    let long_alias_commit = commit_source(
        package_dir.path(),
        &include_str!("fixtures/attachment-alias-precedence-package.orna").replace("41", "42"),
    );

    let loader = ProjectLoader::default();
    let primary =
        PinnedDatabase::resolve("app", primary_repository, &primary_commit, loader).unwrap();
    let short_alias = PinnedDatabase::resolve(
        "main",
        package_repository.clone(),
        &short_alias_commit,
        loader,
    )
    .unwrap();
    let long_alias = PinnedDatabase::resolve(
        "main_archive",
        package_repository,
        &long_alias_commit,
        loader,
    )
    .unwrap();
    let mut databases = AttachedDatabaseSession::new(primary).unwrap();
    databases.attach_database(short_alias).unwrap();
    databases.attach_database(long_alias).unwrap();

    let mut explicit = AdmittedReplSession::from_attached_database_session(
        &databases,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(explicit.submit("use main.*;"), Ok(None));
    assert_eq!(explicit.submit("use main_archive.{choice};"), Ok(None));
    assert_eq!(
        explicit.submit("choice()"),
        Ok(Some(Value::int(42.into())))
    );

    let mut local = AdmittedReplSession::from_attached_database_session(
        &databases,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(local.submit("let choice = 99;"), Ok(None));
    assert_eq!(local.submit("use main.*;"), Ok(None));
    assert_eq!(local.submit("use main_archive.*;"), Ok(None));
    assert_eq!(local.submit("choice"), Ok(Some(Value::int(99.into()))));

    let mut forward = AdmittedReplSession::from_attached_database_session(
        &databases,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(forward.submit("use main.*;"), Ok(None));
    assert_eq!(forward.submit("use main_archive.*;"), Ok(None));
    let forward_error = forward.submit("choice()").unwrap_err();

    let mut reverse = AdmittedReplSession::from_attached_database_session(
        &databases,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(reverse.submit("use main_archive.*;"), Ok(None));
    assert_eq!(reverse.submit("use main.*;"), Ok(None));
    let reverse_error = reverse.submit("choice()").unwrap_err();
    assert_eq!(forward_error.code(), reverse_error.code());
    assert_eq!(forward_error.code(), "ORNA-S011-AMBIGUOUS");
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

#[test]
fn fresh_nested_route_pair_chains_evaluate_rebound_closure_values() {
    let package_source = include_str!("fixtures/attachment-route-package.orna");
    let primary_source = include_str!("fixtures/attachment-route-primary.orna");
    let (package_dir, package_repository, _) = repository(package_source);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
        "archive_copy_archive_archive_archive",
    ];
    let snapshot = |value: &str, manifest: Option<&str>, message: &str| {
        let source = package_source.replace("42", value);
        commit_snapshot(package_dir.path(), &source, manifest, message)
    };

    let leaf_initial = snapshot("93", None, "initial terminal leaf");
    let leaf_middle = snapshot("94", None, "first pair terminal leaf");
    let leaf_final = snapshot("96", None, "final terminal leaf");
    let route_three_initial_manifest = format!("{} {}\n", aliases[4], leaf_initial);
    let route_three_initial = snapshot(
        "83",
        Some(&route_three_initial_manifest),
        "initial third-depth route",
    );
    let route_three_middle_manifest = format!("{} {}\n", aliases[4], leaf_middle);
    let route_three_middle = snapshot(
        "84",
        Some(&route_three_middle_manifest),
        "first pair third-depth route",
    );
    let route_three_final_manifest = format!("{} {}\n", aliases[4], leaf_final);
    let route_three_final = snapshot(
        "85",
        Some(&route_three_final_manifest),
        "second pair third-depth route",
    );
    let terminal_initial_manifest = format!("{} {}\n", aliases[3], route_three_initial);
    let terminal_initial = snapshot(
        "80",
        Some(&terminal_initial_manifest),
        "initial second-depth route",
    );
    let terminal_middle_manifest = format!("{} {}\n", aliases[3], route_three_middle);
    let terminal_middle = snapshot(
        "81",
        Some(&terminal_middle_manifest),
        "first pair second-depth route",
    );
    let deep_initial_manifest = format!("{} {}\n", aliases[2], terminal_initial);
    let deep_initial = snapshot("70", Some(&deep_initial_manifest), "initial nested route");
    let middle_manifest = format!("{} {}\n", aliases[1], deep_initial);
    let middle = snapshot("60", Some(&middle_manifest), "fresh route root");

    let (primary_dir, primary_repository, _) = repository(primary_source);
    let parent_manifest = format!("{} {}\n", aliases[0], middle);
    let parent_commit = commit_snapshot(
        primary_dir.path(),
        primary_source,
        Some(&parent_manifest),
        "primary attachment root",
    );
    let loader = ProjectLoader::default();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), package_repository.clone())),
        loader,
    )
    .unwrap();
    let parent = resolver
        .resolve_for_parent(
            PinnedDatabase::resolve("app", primary_repository, &parent_commit, loader).unwrap(),
        )
        .unwrap();
    let nested_parent = resolver
        .resolve_nested_path(&parent, &[aliases[0], aliases[1]])
        .unwrap();
    let fresh_route = resolver
        .resolve_nested_rebind_path(&nested_parent, &[])
        .unwrap();
    assert!(fresh_route.retained_sessions().is_empty());
    let mut initial_evaluator = AdmittedReplSession::from_attached_database_session(
        fresh_route.final_session(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        initial_evaluator.submit(&format!("use {};", aliases[2])),
        Ok(None)
    );
    assert_eq!(
        initial_evaluator.submit(&format!("{}.package_value()", aliases[2])),
        Ok(Some(Value::int(80.into())))
    );

    let first_pair = [
        PinnedDatabase::resolve(
            aliases[2],
            package_repository.clone(),
            &terminal_middle,
            loader,
        )
        .unwrap(),
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_middle,
            loader,
        )
        .unwrap(),
    ];
    let second_pair = [
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_final,
            loader,
        )
        .unwrap(),
        PinnedDatabase::resolve(
            aliases[4],
            package_repository.clone(),
            &leaf_final,
            loader,
        )
        .unwrap(),
    ];
    let same_depth_second_pair = [
        PinnedDatabase::resolve(
            aliases[2],
            package_repository.clone(),
            &terminal_middle,
            loader,
        )
        .unwrap(),
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_final,
            loader,
        )
        .unwrap(),
    ];
    let first_stage = resolver
        .extend_nested_terminal_pair_chain(&fresh_route, &[first_pair.clone()])
        .unwrap_or_else(|error| panic!("first nested pair failed: {error:?}"));
    assert_eq!(first_stage.retained_wave(0).unwrap().len(), 2);
    let saved_handoff = first_stage.handoff_checkpoint(0, 0).unwrap();
    let saved_middle_handoff = first_stage.handoff_checkpoint(0, 1).unwrap();
    assert_eq!(saved_handoff.depth_label().wave(), 0);
    assert_eq!(saved_handoff.depth_label().depth(), 0);
    assert_eq!(saved_middle_handoff.depth_label().wave(), 0);
    assert_eq!(saved_middle_handoff.depth_label().depth(), 1);
    assert_eq!(
        saved_handoff.depth_label(),
        &first_stage.retained_depth_label(0, 0).unwrap()
    );
    assert_eq!(
        saved_handoff
            .handoff()
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        terminal_initial
    );
    let mut first_evaluator = AdmittedReplSession::from_attached_database_session(
        &first_stage.retained_wave(0).unwrap()[1],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        first_evaluator.submit(&format!("use {};", aliases[3])),
        Ok(None)
    );
    assert_eq!(
        first_evaluator.submit(&format!("{}.package_value()", aliases[3])),
        Ok(Some(Value::int(84.into())))
    );

    let chained = resolver
        .extend_nested_terminal_pair_chain(&first_stage, &[second_pair.clone()])
        .unwrap_or_else(|error| panic!("continued nested pair failed: {error:?}"));

    let mut evaluator = AdmittedReplSession::from_attached_database_session(
        &chained.retained_wave(2).unwrap()[1],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        evaluator.submit(&format!("use {};", aliases[4])),
        Ok(None)
    );
    assert_eq!(
        evaluator.submit(&format!("{}.package_value()", aliases[4])),
        Ok(Some(Value::int(96.into())))
    );

    let historical_handoff = resolver
        .extend_nested_terminal_pair_chain_from_wave(
            &first_stage,
            0,
            0,
            &[first_pair.clone(), second_pair.clone()],
        )
        .unwrap_or_else(|error| panic!("historical handoff chain failed: {error:?}"));
    let mut selected_root = AdmittedReplSession::from_attached_database_session(
        &historical_handoff.retained_wave(2).unwrap()[0],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        selected_root.submit(&format!("use {};", aliases[2])),
        Ok(None)
    );
    assert_eq!(
        selected_root.submit(&format!("{}.package_value()", aliases[2])),
        Ok(Some(Value::int(80.into())))
    );

    let mut handoff = AdmittedReplSession::from_attached_database_session(
        &historical_handoff.retained_wave(2).unwrap()[1],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(handoff.submit(&format!("use {};", aliases[3])), Ok(None));
    assert_eq!(
        handoff.submit(&format!("{}.package_value()", aliases[3])),
        Ok(Some(Value::int(84.into())))
    );

    let mut continued = AdmittedReplSession::from_attached_database_session(
        &historical_handoff.retained_wave(4).unwrap()[1],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(continued.submit(&format!("use {};", aliases[4])), Ok(None));
    assert_eq!(
        continued.submit(&format!("{}.package_value()", aliases[4])),
        Ok(Some(Value::int(96.into())))
    );

    let initial_depth_label = first_stage.retained_depth_label(0, 0).unwrap();
    assert_eq!(initial_depth_label.wave(), 0);
    assert_eq!(initial_depth_label.depth(), 0);
    assert!(matches!(
        resolver.extend_nested_terminal_pair_storm_from_wave(&first_stage, 0, 9, &[]),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    let distinct_depth_route = resolver
        .resolve_nested_rebind_path(
            &first_stage.retained_wave(0).unwrap()[1],
            &[PinnedDatabase::resolve(
                aliases[3],
                package_repository.clone(),
                &route_three_final,
                loader,
            )
            .unwrap()],
        )
        .unwrap();
    let distinct_depth_label = distinct_depth_route.retained_depth_label(0, 0).unwrap();
    assert_ne!(distinct_depth_label, initial_depth_label);
    assert!(matches!(
        resolver.extend_nested_terminal_pair_storm_from_label(
            &first_stage,
            &distinct_depth_label,
            &[],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));

    let fixed_depth_storm = resolver
        .extend_nested_terminal_pair_storm_from_label(
            &first_stage,
            &initial_depth_label,
            &[first_pair.clone(), same_depth_second_pair.clone()],
        )
        .unwrap_or_else(|error| panic!("fixed-depth pair storm failed: {error:?}"));
    assert_eq!(
        fixed_depth_storm.retained_depth_label(0, 0).unwrap(),
        initial_depth_label,
        "the source depth label retains its exact pin route across each fold"
    );
    assert_eq!(
        fixed_depth_storm.final_session().primary().pin().name(),
        aliases[3]
    );
    assert_eq!(
        fixed_depth_storm
            .final_session()
            .primary()
            .pin()
            .commit()
            .as_str(),
        route_three_final
    );
    assert_eq!(fixed_depth_storm.final_session().attached().count(), 1);
    assert_eq!(chained.final_session().primary().pin().name(), aliases[4]);
    let mut fixed_depth_evaluator = AdmittedReplSession::from_attached_database_session(
        fixed_depth_storm.final_session(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        fixed_depth_evaluator.submit(&format!("use {};", aliases[4])),
        Ok(None)
    );
    assert_eq!(
        fixed_depth_evaluator.submit(&format!("{}.package_value()", aliases[4])),
        Ok(Some(Value::int(96.into())))
    );

    let later_cascade = resolver
        .extend_nested_terminal_pair_chain(&first_stage, &[second_pair.clone()])
        .unwrap_or_else(|error| panic!("later nested pair cascade failed: {error:?}"));
    let checkpoint_storm = resolver
        .extend_nested_terminal_pair_storm_from_checkpoint(
            &later_cascade,
            &saved_handoff,
            &[
                first_pair.clone(),
                same_depth_second_pair.clone(),
                first_pair.clone(),
            ],
        )
        .unwrap_or_else(|error| panic!("checkpoint pair storm failed: {error:?}"));
    let pin_route = |session: &AttachedDatabaseSession| {
        (
            (
                session.primary().pin().name().to_owned(),
                session.primary().pin().commit().as_str().to_owned(),
            ),
            session
                .attached()
                .map(|(alias, database)| {
                    (
                        alias.to_owned(),
                        database.pin().name().to_owned(),
                        database.pin().commit().as_str().to_owned(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    for wave in [4, 6, 8] {
        let retained = checkpoint_storm.retained_wave(wave).unwrap();
        assert_eq!(pin_route(&retained[0]), pin_route(saved_handoff.handoff()));
        let label = checkpoint_storm.retained_depth_label(wave, 0).unwrap();
        assert_eq!(label.wave(), wave);
        assert_eq!(label.depth(), 0);

        let mut checkpoint_root = AdmittedReplSession::from_attached_database_session(
            &retained[0],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(checkpoint_root.submit(&format!("use {};", aliases[2])), Ok(None));
        assert_eq!(
            checkpoint_root.submit(&format!("{}.package_value()", aliases[2])),
            Ok(Some(Value::int(80.into())))
        );

        let mut after_first_rebind = AdmittedReplSession::from_attached_database_session(
            &retained[1],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            after_first_rebind.submit(&format!("use {};", aliases[3])),
            Ok(None)
        );
        assert_eq!(
            after_first_rebind.submit(&format!("{}.package_value()", aliases[3])),
            Ok(Some(Value::int(84.into())))
        );
    }

    assert!(matches!(
        resolver.extend_nested_terminal_pair_storm_from_checkpoint(
            &fresh_route,
            &saved_middle_handoff,
            &[],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    let nested_checkpoint_storm = resolver
        .extend_nested_terminal_pair_storm_from_checkpoint(
            &later_cascade,
            &saved_middle_handoff,
            &[
                second_pair.clone(),
                second_pair.clone(),
                second_pair.clone(),
            ],
        )
        .unwrap_or_else(|error| panic!("depth-one checkpoint storm failed: {error:?}"));
    assert_eq!(
        nested_checkpoint_storm.retained_depth_label(0, 1).unwrap(),
        saved_middle_handoff.depth_label().clone(),
        "the source depth-one handoff remains bound to its original pins"
    );
    for wave in [4, 6, 8] {
        let retained = nested_checkpoint_storm.retained_wave(wave).unwrap();
        assert_eq!(pin_route(&retained[0]), pin_route(saved_middle_handoff.handoff()));
        let label = nested_checkpoint_storm.retained_depth_label(wave, 0).unwrap();
        assert_eq!(label.wave(), wave);
        assert_eq!(label.depth(), 0);
    }
    let nested_root = &nested_checkpoint_storm.retained_wave(8).unwrap()[0];
    let mut nested_root_value =
        AdmittedReplSession::from_attached_database_session(nested_root, Limits::default())
            .unwrap();
    assert_eq!(
        nested_root_value.submit(&format!("use {};", aliases[3])),
        Ok(None)
    );
    assert_eq!(
        nested_root_value.submit(&format!("{}.package_value()", aliases[3])),
        Ok(Some(Value::int(84.into())))
    );
    let nested_rebound = &nested_checkpoint_storm.retained_wave(8).unwrap()[1];
    let mut nested_rebound_value =
        AdmittedReplSession::from_attached_database_session(nested_rebound, Limits::default())
            .unwrap();
    assert_eq!(
        nested_rebound_value.submit(&format!("use {};", aliases[4])),
        Ok(None)
    );
    assert_eq!(
        nested_rebound_value.submit(&format!("{}.package_value()", aliases[4])),
        Ok(Some(Value::int(96.into())))
    );

    let anchor_a_waves = [first_pair.clone()];
    let anchor_b_waves = [second_pair.clone()];
    let anchor_a_return_waves = [first_pair.clone()];
    let cyclic_anchor_plans = [
        (&saved_handoff, anchor_a_waves.as_slice()),
        (&saved_middle_handoff, anchor_b_waves.as_slice()),
        (&saved_handoff, anchor_a_return_waves.as_slice()),
    ];
    let cyclic_anchor_storm = resolver
        .extend_nested_terminal_pair_checkpoint_storms(&later_cascade, &cyclic_anchor_plans)
        .unwrap_or_else(|error| panic!("cyclic anchor storm failed: {error:?}"));
    assert_eq!(
        cyclic_anchor_storm.retained_depth_label(0, 0).unwrap(),
        saved_handoff.depth_label().clone()
    );
    assert_eq!(
        cyclic_anchor_storm.retained_depth_label(0, 1).unwrap(),
        saved_middle_handoff.depth_label().clone()
    );
    for (wave, checkpoint) in [
        (4, &saved_handoff),
        (6, &saved_middle_handoff),
        (8, &saved_handoff),
    ] {
        let root = &cyclic_anchor_storm.retained_wave(wave).unwrap()[0];
        assert_eq!(pin_route(root), pin_route(checkpoint.handoff()));
        let label = cyclic_anchor_storm.retained_depth_label(wave, 0).unwrap();
        assert_eq!(label.wave(), wave);
        assert_eq!(label.depth(), 0);
    }
    let checkpoint_replay = resolver
        .extend_nested_terminal_pair_chain_from_checkpoint(
            &later_cascade,
            &saved_handoff,
            &[first_pair.clone(), second_pair.clone()],
        )
        .unwrap_or_else(|error| panic!("checkpoint pair cascade failed: {error:?}"));

    let first_checkpoint_waves = [first_pair.clone(), second_pair.clone()];
    let middle_checkpoint_waves = [second_pair.clone()];
    let checkpoint_plans = [
        (&saved_handoff, first_checkpoint_waves.as_slice()),
        (&saved_middle_handoff, middle_checkpoint_waves.as_slice()),
    ];
    let checkpoint_cascades = resolver
        .extend_nested_terminal_pair_checkpoint_cascades(&later_cascade, &checkpoint_plans)
        .unwrap_or_else(|error| panic!("nested checkpoint cascades failed: {error:?}"));
    let first_cascade_root = &checkpoint_cascades.retained_wave(4).unwrap()[0];
    let first_cascade_label = checkpoint_cascades.retained_depth_label(4, 0).unwrap();
    assert_eq!(first_cascade_label.wave(), 4);
    assert_eq!(first_cascade_label.depth(), 0);
    assert_eq!(pin_route(first_cascade_root), pin_route(saved_handoff.handoff()));

    let middle_cascade_root = &checkpoint_cascades.retained_wave(8).unwrap()[0];
    let middle_cascade_label = checkpoint_cascades.retained_depth_label(8, 0).unwrap();
    assert_eq!(middle_cascade_label.wave(), 8);
    assert_eq!(middle_cascade_label.depth(), 0);
    assert_eq!(
        pin_route(middle_cascade_root),
        pin_route(saved_middle_handoff.handoff())
    );
    let mut middle_cascade_eval = AdmittedReplSession::from_attached_database_session(
        middle_cascade_root,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        middle_cascade_eval.submit(&format!("use {};", aliases[3])),
        Ok(None)
    );
    assert_eq!(
        middle_cascade_eval.submit(&format!("{}.package_value()", aliases[3])),
        Ok(Some(Value::int(84.into())))
    );
    let mut cascade_terminal_eval = AdmittedReplSession::from_attached_database_session(
        &checkpoint_cascades.retained_wave(8).unwrap()[1],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        cascade_terminal_eval.submit(&format!("use {};", aliases[4])),
        Ok(None)
    );
    assert_eq!(
        cascade_terminal_eval.submit(&format!("{}.package_value()", aliases[4])),
        Ok(Some(Value::int(96.into())))
    );

    let replay_handoff = checkpoint_replay.retained_wave(4).unwrap()[0].clone();
    let replay_depth_label = checkpoint_replay.retained_depth_label(4, 0).unwrap();
    assert_eq!(replay_depth_label.wave(), 4);
    assert_eq!(replay_depth_label.depth(), 0);
    assert_eq!(
        pin_route(&replay_handoff),
        pin_route(saved_handoff.handoff()),
        "the checkpoint depth label still identifies every exact pin after later pair folds"
    );
    assert_eq!(
        replay_handoff.primary().pin().name(),
        saved_handoff.handoff().primary().pin().name()
    );
    assert_eq!(
        replay_handoff
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        terminal_initial
    );
    let mut replay_handoff_eval = AdmittedReplSession::from_attached_database_session(
        &replay_handoff,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        replay_handoff_eval.submit(&format!("use {};", aliases[2])),
        Ok(None)
    );
    assert_eq!(
        replay_handoff_eval.submit(&format!("{}.package_value()", aliases[2])),
        Ok(Some(Value::int(80.into())))
    );

    let mut replay_terminal_eval = AdmittedReplSession::from_attached_database_session(
        &checkpoint_replay.retained_wave(6).unwrap()[1],
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        replay_terminal_eval.submit(&format!("use {};", aliases[4])),
        Ok(None)
    );
    assert_eq!(
        replay_terminal_eval.submit(&format!("{}.package_value()", aliases[4])),
        Ok(Some(Value::int(96.into())))
    );
}

#[test]
fn sibling_checkpoint_folds_keep_tabular_anchor_identities() {
    let package_source = include_str!("fixtures/attachment-route-package.orna");
    let primary_source = include_str!("fixtures/attachment-route-primary.orna");
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
        "archive_copy_archive_archive_archive",
    ];
    let (package_dir, package_repository, _) = repository(package_source);
    let snapshot = |value: &str, manifest: Option<&str>, message: &str| {
        commit_snapshot(
            package_dir.path(),
            &package_source.replace("42", value),
            manifest,
            message,
        )
    };

    let leaf_a = snapshot("91", None, "sibling A terminal leaf");
    let leaf_b = snapshot("92", None, "sibling B terminal leaf");
    let route_three_a_manifest = format!("{} {}\n", aliases[4], leaf_a);
    let route_three_a = snapshot(
        "81",
        Some(&route_three_a_manifest),
        "sibling A third-depth route",
    );
    let route_three_b_manifest = format!("{} {}\n", aliases[4], leaf_b);
    let route_three_b = snapshot(
        "82",
        Some(&route_three_b_manifest),
        "sibling B third-depth route",
    );
    let route_two_a_manifest = format!("{} {}\n", aliases[3], route_three_a);
    let route_two_a = snapshot(
        "71",
        Some(&route_two_a_manifest),
        "sibling A second-depth route",
    );
    let route_two_b_manifest = format!("{} {}\n", aliases[3], route_three_b);
    let route_two_b = snapshot(
        "72",
        Some(&route_two_b_manifest),
        "sibling B second-depth route",
    );
    let deep_a_manifest = format!("{} {}\n", aliases[2], route_two_a);
    let deep_a = snapshot("61", Some(&deep_a_manifest), "sibling A first-depth route");
    let deep_b_manifest = format!("{} {}\n", aliases[2], route_two_b);
    let deep_b = snapshot("62", Some(&deep_b_manifest), "sibling B first-depth route");
    let middle_a_manifest = format!("{} {}\n", aliases[1], deep_a);
    let middle_a = snapshot("51", Some(&middle_a_manifest), "sibling A middle route");
    let middle_b_manifest = format!("{} {}\n", aliases[1], deep_b);
    let middle_b = snapshot("52", Some(&middle_b_manifest), "sibling B middle route");

    let (primary_a_dir, primary_a_repository, _) = repository(primary_source);
    let primary_a_manifest = format!("{} {}\n", aliases[0], middle_a);
    let primary_a_commit = commit_snapshot(
        primary_a_dir.path(),
        primary_source,
        Some(&primary_a_manifest),
        "sibling A primary route",
    );
    let (primary_b_dir, primary_b_repository, _) = repository(primary_source);
    let primary_b_manifest = format!("{} {}\n", aliases[0], middle_b);
    let primary_b_commit = commit_snapshot(
        primary_b_dir.path(),
        primary_source,
        Some(&primary_b_manifest),
        "sibling B primary route",
    );

    let loader = ProjectLoader::default();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), package_repository.clone())),
        loader,
    )
    .unwrap();
    let parent_a = resolver
        .resolve_for_parent(
            PinnedDatabase::resolve("app_a", primary_a_repository, &primary_a_commit, loader)
                .unwrap(),
        )
        .unwrap();
    let parent_b = resolver
        .resolve_for_parent(
            PinnedDatabase::resolve("app_b", primary_b_repository, &primary_b_commit, loader)
                .unwrap(),
        )
        .unwrap();
    let nested_a = resolver
        .resolve_nested_path(&parent_a, &[aliases[0], aliases[1]])
        .unwrap();
    let nested_b = resolver
        .resolve_nested_path(&parent_b, &[aliases[0], aliases[1]])
        .unwrap();
    let pair_a = [
        PinnedDatabase::resolve(aliases[2], package_repository.clone(), &route_two_a, loader)
            .unwrap(),
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_a,
            loader,
        )
        .unwrap(),
    ];
    let pair_b = [
        PinnedDatabase::resolve(aliases[2], package_repository.clone(), &route_two_b, loader)
            .unwrap(),
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_b,
            loader,
        )
        .unwrap(),
    ];
    let stage_a = resolver
        .resolve_nested_terminal_pair(&nested_a, pair_a.clone())
        .unwrap();
    let stage_b = resolver
        .resolve_nested_terminal_pair(&nested_b, pair_b.clone())
        .unwrap();
    let independent_stage_a = resolver
        .resolve_nested_terminal_pair(&nested_a, pair_a.clone())
        .unwrap();
    let root_a = stage_a.handoff_checkpoint(0, 0).unwrap();
    let middle_a_checkpoint = stage_a.handoff_checkpoint(0, 1).unwrap();
    let root_b = stage_b.handoff_checkpoint(0, 0).unwrap();
    let middle_b_checkpoint = stage_b.handoff_checkpoint(0, 1).unwrap();
    let independent_root_a = independent_stage_a.handoff_checkpoint(0, 0).unwrap();
    let independent_middle_a_checkpoint = independent_stage_a.handoff_checkpoint(0, 1).unwrap();
    assert_eq!(
        root_a.handoff().primary().pin().commit(),
        independent_root_a.handoff().primary().pin().commit(),
        "independent routes can retain the same exact pins"
    );
    assert_ne!(root_a.depth_label(), independent_root_a.depth_label());

    let terminal_pair_a = [
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_a,
            loader,
        )
        .unwrap(),
        PinnedDatabase::resolve(aliases[4], package_repository.clone(), &leaf_a, loader).unwrap(),
    ];
    let terminal_pair_b = [
        PinnedDatabase::resolve(
            aliases[3],
            package_repository.clone(),
            &route_three_b,
            loader,
        )
        .unwrap(),
        PinnedDatabase::resolve(aliases[4], package_repository.clone(), &leaf_b, loader).unwrap(),
    ];
    let root_waves_a = [pair_a.clone()];
    let middle_waves_a = [terminal_pair_a.clone()];
    let root_return_waves_a = [pair_a.clone()];
    let root_waves_b = [pair_b.clone()];
    let middle_waves_b = [terminal_pair_b.clone()];
    let root_return_waves_b = [pair_b.clone()];
    let storms_a = [
        (&root_a, root_waves_a.as_slice()),
        (&middle_a_checkpoint, middle_waves_a.as_slice()),
        (&root_a, root_return_waves_a.as_slice()),
    ];
    let storms_b = [
        (&root_b, root_waves_b.as_slice()),
        (&middle_b_checkpoint, middle_waves_b.as_slice()),
        (&root_b, root_return_waves_b.as_slice()),
    ];
    let storms_independent_a = [
        (&independent_root_a, root_waves_a.as_slice()),
        (&independent_middle_a_checkpoint, middle_waves_a.as_slice()),
        (&independent_root_a, root_return_waves_a.as_slice()),
    ];
    let paths = [
        (&stage_a, storms_a.as_slice()),
        (&stage_b, storms_b.as_slice()),
        (&independent_stage_a, storms_independent_a.as_slice()),
    ];

    assert!(matches!(
        resolver.extend_sibling_terminal_pair_checkpoint_storms(&[(
            &stage_a,
            [(&independent_root_a, root_waves_a.as_slice())].as_slice()
        ),]),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));

    let one_root_storm = [(&root_a, root_waves_a.as_slice())];
    let folded_branch_a = resolver
        .extend_nested_terminal_pair_checkpoint_storms(&stage_a, &one_root_storm)
        .unwrap();
    let folded_branch_b = resolver
        .extend_nested_terminal_pair_checkpoint_storms(&stage_a, &one_root_storm)
        .unwrap();
    let branch_a_anchor = folded_branch_a.retained_wave(2).unwrap().first().unwrap();
    let branch_b_anchor = folded_branch_b.retained_wave(2).unwrap().first().unwrap();
    assert_eq!(
        branch_a_anchor.primary().pin().commit().as_str(),
        branch_b_anchor.primary().pin().commit().as_str()
    );
    assert_eq!(
        branch_a_anchor
            .attached()
            .map(|(name, database)| (name.to_owned(), database.pin().commit().as_str().to_owned()))
            .collect::<Vec<_>>(),
        branch_b_anchor
            .attached()
            .map(|(name, database)| (name.to_owned(), database.pin().commit().as_str().to_owned()))
            .collect::<Vec<_>>()
    );
    let branch_a_label = folded_branch_a.retained_depth_label(2, 0).unwrap();
    let branch_b_label = folded_branch_b.retained_depth_label(2, 0).unwrap();
    assert_ne!(
        branch_a_label, branch_b_label,
        "separate folds assign distinct identities to matching route snapshots"
    );
    let branch_a_checkpoint = folded_branch_a.handoff_checkpoint(2, 0).unwrap();
    let no_pair_waves: [[PinnedDatabase; 2]; 0] = [];
    assert!(resolver
        .extend_nested_terminal_pair_checkpoint_storms(
            &folded_branch_a,
            &[(
                &branch_a_checkpoint,
                no_pair_waves.as_slice(),
            )],
        )
        .is_ok());
    assert!(matches!(
        resolver.extend_nested_terminal_pair_checkpoint_storms(
            &folded_branch_b,
            &[(
                &branch_a_checkpoint,
                no_pair_waves.as_slice(),
            )],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));

    let folded = resolver
        .extend_sibling_terminal_pair_checkpoint_storms(&paths)
        .unwrap_or_else(|error| panic!("sibling checkpoint folds failed: {error:?}"));
    assert_eq!(folded.routes().len(), 3);
    for (route, expected_anchor, expected_leaf, expected_value, expected_root) in [
        (
            &folded.routes()[0],
            &middle_a_checkpoint,
            &leaf_a,
            91,
            &root_a,
        ),
        (
            &folded.routes()[1],
            &middle_b_checkpoint,
            &leaf_b,
            92,
            &root_b,
        ),
        (
            &folded.routes()[2],
            &independent_middle_a_checkpoint,
            &leaf_a,
            91,
            &independent_root_a,
        ),
    ] {
        let retained_anchor = route.retained_wave(4).unwrap().first().unwrap();
        assert_eq!(
            retained_anchor.primary().pin().commit().as_str(),
            expected_anchor.handoff().primary().pin().commit().as_str(),
            "each table row retains its own middle checkpoint anchor"
        );
        assert_eq!(route.retained_depth_label(4, 0).unwrap().wave(), 4);
        assert_eq!(
            route.retained_depth_label(0, 0).unwrap(),
            expected_root.depth_label().clone(),
            "the first anchor label survives the A-to-B-to-A cycle"
        );
        let returned_anchor = route.retained_wave(6).unwrap().first().unwrap();
        assert_eq!(
            returned_anchor.primary().pin().commit().as_str(),
            expected_root.handoff().primary().pin().commit().as_str()
        );
        assert_eq!(
            route
                .final_session()
                .database(aliases[4])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            expected_leaf.as_str()
        );
        let mut evaluator = AdmittedReplSession::from_attached_database_session(
            route.final_session(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(evaluator.submit(&format!("use {};", aliases[4])), Ok(None));
        assert_eq!(
            evaluator.submit(&format!("{}.package_value()", aliases[4])),
            Ok(Some(Value::int(expected_value.into())))
        );
    }

    let anchor_a = folded.handoff_checkpoint(0, 4, 0).unwrap();
    let anchor_independent_a = folded.handoff_checkpoint(2, 4, 0).unwrap();
    assert_eq!(
        folded.retained_depth_label(0, 4, 0).unwrap(),
        *anchor_a.depth_label(),
        "row-indexed label selection preserves the checkpoint identity"
    );
    assert!(matches!(
        folded.retained_depth_label(3, 4, 0),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    assert_eq!(
        anchor_a.handoff().primary().pin(),
        anchor_independent_a.handoff().primary().pin(),
        "independent parent rows may retain exactly matching anchor pins"
    );
    let attached_pins = |checkpoint: &ReboundPathCheckpoint| {
        checkpoint
            .handoff()
            .attached()
            .map(|(name, database)| {
                (name.to_owned(), database.pin().commit().as_str().to_owned())
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        attached_pins(&anchor_a),
        attached_pins(&anchor_independent_a),
        "independent parent rows may retain exactly matching attached pins"
    );
    assert_ne!(
        anchor_a.depth_label(),
        anchor_independent_a.depth_label(),
        "row-indexed checkpoints preserve distinct anchor identities"
    );
    assert!(folded.validate_handoff_checkpoint(0, &anchor_a).is_ok());
    assert!(matches!(
        folded.validate_handoff_checkpoint(2, &anchor_a),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    assert!(matches!(
        resolver.extend_nested_terminal_pair_checkpoint_storms(
            &folded.routes()[2],
            &[(&anchor_a, no_pair_waves.as_slice())],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));

    let continuation_root_a = folded.handoff_checkpoint(0, 6, 0).unwrap();
    let continuation_root_b = folded.handoff_checkpoint(1, 6, 0).unwrap();
    let continuation_root_independent_a = folded.handoff_checkpoint(2, 6, 0).unwrap();
    let continuation_a = [(&continuation_root_a, root_waves_a.as_slice())];
    let continuation_b = [(&continuation_root_b, root_waves_b.as_slice())];
    let continuation_independent_a = [(
        &continuation_root_independent_a,
        root_return_waves_a.as_slice(),
    )];
    let continuation_plans = [
        continuation_a.as_slice(),
        continuation_b.as_slice(),
        continuation_independent_a.as_slice(),
    ];
    assert!(matches!(
        resolver.extend_sibling_terminal_pair_checkpoint_storms_from_siblings(
            &folded,
            &continuation_plans[..2],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    let nested_folded = resolver
        .extend_sibling_terminal_pair_checkpoint_storms_from_siblings(
            &folded,
            &continuation_plans,
        )
        .unwrap_or_else(|error| panic!("nested sibling checkpoint folds failed: {error:?}"));
    assert_eq!(nested_folded.routes().len(), 3);
    for row in 0..3 {
        assert_eq!(
            nested_folded.retained_depth_label(row, 4, 0).unwrap(),
            folded.retained_depth_label(row, 4, 0).unwrap(),
            "nested folds preserve the parent's earlier anchor identity"
        );
    }
    let nested_anchor_a = nested_folded.handoff_checkpoint(0, 8, 0).unwrap();
    let nested_anchor_independent_a = nested_folded.handoff_checkpoint(2, 8, 0).unwrap();
    assert_eq!(
        nested_anchor_a.handoff().primary().pin(),
        nested_anchor_independent_a.handoff().primary().pin()
    );
    assert_eq!(
        attached_pins(&nested_anchor_a),
        attached_pins(&nested_anchor_independent_a)
    );
    assert_ne!(
        nested_anchor_a.depth_label(),
        nested_anchor_independent_a.depth_label(),
        "new nested anchors keep independent row identities despite equal pins"
    );
    assert!(matches!(
        resolver.extend_nested_terminal_pair_checkpoint_storms(
            &nested_folded.routes()[2],
            &[(&nested_anchor_a, no_pair_waves.as_slice())],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    for (row, expected_value) in [(0, 91), (1, 92), (2, 91)] {
        let final_session = nested_folded.routes()[row].final_session();
        assert_eq!(
            final_session
                .database(aliases[4])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            if expected_value == 91 {
                leaf_a.as_str()
            } else {
                leaf_b.as_str()
            },
            "the nested row keeps the expected terminal pin"
        );
        let mut evaluator = AdmittedReplSession::from_attached_database_session(
            final_session,
            Limits::default(),
        )
        .unwrap();
        assert_eq!(evaluator.submit(&format!("use {};", aliases[4])), Ok(None));
        assert_eq!(
            evaluator.submit(&format!("{}.package_value()", aliases[4])),
            Ok(Some(Value::int(expected_value.into())))
        );
    }

    let depth_label_a = nested_folded.retained_depth_label(0, 6, 1).unwrap();
    let depth_label_b = nested_folded.retained_depth_label(1, 6, 0).unwrap();
    let depth_label_independent_a = nested_folded.retained_depth_label(2, 6, 1).unwrap();
    let evaluate_terminal_pin = |session: &AttachedDatabaseSession, alias: &str| {
        let mut evaluation_session =
            AttachedDatabaseSession::new(parent_a.primary().clone()).unwrap();
        evaluation_session
            .attach_database(session.database(alias).unwrap().clone())
            .unwrap();
        let mut evaluator = AdmittedReplSession::from_attached_database_session(
            &evaluation_session,
            Limits::default(),
        )
        .unwrap();
        assert_eq!(evaluator.submit(&format!("use {alias};")), Ok(None));
        evaluator.submit(&format!("{alias}.package_value()"))
    };
    assert_eq!(depth_label_a.depth(), 1);
    assert_eq!(depth_label_b.depth(), 0);
    assert_ne!(
        depth_label_a, depth_label_independent_a,
        "equal pins at equal coordinates remain distinct across sibling rows"
    );
    let depth_storms_a = [(6, 1, middle_waves_a.as_slice())];
    let depth_storms_b = [(6, 0, root_waves_b.as_slice())];
    let depth_storms_independent_a = [(6, 1, middle_waves_a.as_slice())];
    let depth_plans = [
        depth_storms_a.as_slice(),
        depth_storms_b.as_slice(),
        depth_storms_independent_a.as_slice(),
    ];
    let label_storms_a = [(&depth_label_a, middle_waves_a.as_slice())];
    let label_storms_b = [(&depth_label_b, root_waves_b.as_slice())];
    let label_storms_independent_a =
        [(&depth_label_independent_a, middle_waves_a.as_slice())];
    let label_plans = [
        label_storms_a.as_slice(),
        label_storms_b.as_slice(),
        label_storms_independent_a.as_slice(),
    ];
    let equal_pin_crossed_labels = [
        [(&depth_label_independent_a, middle_waves_a.as_slice())],
        label_storms_b,
        [(&depth_label_a, middle_waves_a.as_slice())],
    ];
    assert!(matches!(
        resolver.extend_sibling_terminal_pair_checkpoint_storms_from_depth_labels(
            &nested_folded,
            &equal_pin_crossed_labels.each_ref().map(|row| row.as_slice()),
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    let label_continued = resolver
        .extend_sibling_terminal_pair_checkpoint_storms_from_depth_labels(
            &nested_folded,
            &label_plans,
        )
        .unwrap_or_else(|error| panic!("label-selected sibling continuation failed: {error:?}"));
    for (row, source_depth, source_label, expected_value) in [
        (0, 1, &depth_label_a, 91),
        (1, 0, &depth_label_b, 92),
        (2, 1, &depth_label_independent_a, 91),
    ] {
        assert_eq!(
            label_continued
                .retained_depth_label(row, 6, source_depth)
                .unwrap(),
            *source_label,
            "label-selected folds retain the exact paired parent anchor"
        );
        let final_session = label_continued.routes()[row].final_session();
        assert_eq!(
            final_session
                .database(aliases[4])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            if expected_value == 91 {
                leaf_a.as_str()
            } else {
                leaf_b.as_str()
            },
            "each exact depth label resolves its row's terminal package"
        );
        assert_eq!(
            evaluate_terminal_pin(final_session, aliases[4]),
            Ok(Some(Value::int(expected_value.into()))),
            "label-selected row {row} evaluates the selected terminal primary"
        );
    }
    assert!(matches!(
        resolver.extend_sibling_terminal_pair_checkpoint_storms_from_depths(
            &nested_folded,
            &depth_plans[..2],
        ),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    let depth_continued = resolver
        .extend_sibling_terminal_pair_checkpoint_storms_from_depths(
            &nested_folded,
            &depth_plans,
        )
        .unwrap_or_else(|error| panic!("depth-selected sibling continuation failed: {error:?}"));
    for (row, depth, original_label, expected_value) in [
        (0, 1, &depth_label_a, 91),
        (1, 0, &depth_label_b, 92),
        (2, 1, &depth_label_independent_a, 91),
    ] {
        assert_eq!(
            depth_continued.retained_depth_label(row, 6, depth).unwrap(),
            *original_label,
            "the selected nonuniform depth identity survives another multi-parent fold"
        );
        let final_session = depth_continued.routes()[row].final_session();
        assert_eq!(
            final_session
                .database(aliases[4])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            if expected_value == 91 {
                leaf_a.as_str()
            } else {
                leaf_b.as_str()
            },
            "continuation follows the pin selected by this row's exact depth"
        );
    }

    let continued_label_a = depth_continued.retained_depth_label(0, 10, 1).unwrap();
    let continued_label_b = depth_continued.retained_depth_label(1, 10, 0).unwrap();
    let continued_label_independent_a = depth_continued.retained_depth_label(2, 10, 1).unwrap();
    let second_depth_storms_a = [(10, 1, no_pair_waves.as_slice())];
    let second_depth_storms_b = [(10, 0, no_pair_waves.as_slice())];
    let second_depth_storms_independent_a = [(10, 1, no_pair_waves.as_slice())];
    let second_depth_plans = [
        second_depth_storms_a.as_slice(),
        second_depth_storms_b.as_slice(),
        second_depth_storms_independent_a.as_slice(),
    ];
    let storm_rounds = [depth_plans.as_slice(), second_depth_plans.as_slice()];
    let label_storm_rounds = [label_plans.as_slice(), label_plans.as_slice()];
    let twice_label_continued = resolver
        .extend_sibling_terminal_pair_checkpoint_storm_rounds_from_depth_labels(
            &nested_folded,
            &label_storm_rounds,
        )
        .unwrap_or_else(|error| panic!("multi-round label storm failed: {error:?}"));
    for (row, source_depth, source_label, expected_value) in [
        (0, 1, &depth_label_a, 91),
        (1, 0, &depth_label_b, 92),
        (2, 1, &depth_label_independent_a, 91),
    ] {
        assert_eq!(
            twice_label_continued
                .retained_depth_label(row, 6, source_depth)
                .unwrap(),
            *source_label,
            "repeated folds keep resolving their captured row anchor identity"
        );
        let final_session = twice_label_continued.routes()[row].final_session();
        assert_eq!(
            evaluate_terminal_pin(final_session, aliases[4]),
            Ok(Some(Value::int(expected_value.into()))),
            "repeated folds return the value computed by each paired parent route"
        );
    }
    let twice_continued = resolver
        .extend_sibling_terminal_pair_checkpoint_storm_rounds_from_depths(
            &nested_folded,
            &storm_rounds,
        )
        .unwrap_or_else(|error| panic!("multi-round sibling depth storm failed: {error:?}"));
    let twice_continued_by_steps = resolver
        .extend_sibling_terminal_pair_checkpoint_storms_from_depths(
            &depth_continued,
            &second_depth_plans,
        )
        .unwrap_or_else(|error| panic!("continued sibling depth replay failed: {error:?}"));
    for (row, source_depth, source_label, next_depth, next_label, expected_value) in [
        (0, 1, &depth_label_a, 1, &continued_label_a, 91),
        (1, 0, &depth_label_b, 0, &continued_label_b, 92),
        (
            2,
            1,
            &depth_label_independent_a,
            1,
            &continued_label_independent_a,
            91,
        ),
    ] {
        assert_eq!(
            twice_continued_by_steps
                .retained_depth_label(row, 6, source_depth)
                .unwrap(),
            *source_label,
            "the first-round source anchor remains bound to its original depth"
        );
        assert_eq!(
            twice_continued_by_steps
                .retained_depth_label(row, 10, next_depth)
                .unwrap(),
            *next_label,
            "the next round retains the checkpoint identity selected from its predecessor"
        );
        assert_eq!(
            twice_continued_by_steps.routes()[row]
                .final_session()
                .database(aliases[4])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            if expected_value == 91 {
                leaf_a.as_str()
            } else {
                leaf_b.as_str()
            }
        );
        let batch_label = twice_continued.retained_depth_label(row, 10, next_depth).unwrap();
        assert_eq!(batch_label.wave(), 10);
        assert_eq!(batch_label.depth(), next_depth);
        assert_eq!(
            twice_continued.routes()[row]
                .final_session()
                .database(aliases[4])
                .unwrap()
                .pin(),
            twice_continued_by_steps.routes()[row]
                .final_session()
                .database(aliases[4])
                .unwrap()
                .pin(),
            "atomic round composition matches the explicitly stepped continuation"
        );
    }
}
