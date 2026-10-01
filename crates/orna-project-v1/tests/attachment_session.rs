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

fn write_commit(directory: &Path, files: &[(&str, &str)], message: &str) -> String {
    for (path, contents) in files {
        let path = directory.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    git(directory, &["add", "--all"]);
    git(directory, &["commit", "--quiet", "-m", message]);
    git(directory, &["rev-parse", "HEAD"])
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

#[test]
fn nested_short_and_long_alias_routes_keep_each_historical_parent_pin() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, base_commit) =
        repository(&[("main.orna", package_source)]);
    let short_child_manifest = format!("archive {base_commit}\n");
    let short_child_commit = write_commit(
        shared_dir.path(),
        &[(PACKAGE_PIN_MANIFEST_PATH, &short_child_manifest)],
        "nested short-alias child pin",
    );

    let short_source = package_source.replace("42", "43");
    let short_parent_manifest = format!("archive_copy {short_child_commit}\n");
    let short_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &short_source),
            (PACKAGE_PIN_MANIFEST_PATH, &short_parent_manifest),
        ],
        "nested short-alias parent pin",
    );

    let long_source = package_source.replace("42", "44");
    let long_parent_manifest = format!("archive {base_commit}\n");
    let long_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &long_source),
            (PACKAGE_PIN_MANIFEST_PATH, &long_parent_manifest),
        ],
        "nested long-alias parent pin",
    );

    // Advancing the repository's HEAD must not retarget either root pin.
    let latest_manifest = format!(
        "archive_copy {long_parent_commit}\narchive {short_child_commit}\n"
    );
    let latest_commit = write_commit(
        shared_dir.path(),
        &[(PACKAGE_PIN_MANIFEST_PATH, &latest_manifest)],
        "move aliases after historical parents",
    );

    let root_manifest = format!(
        "archive {short_parent_commit}\narchive_copy {long_parent_commit}\n"
    );
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    let short = root.database("archive").unwrap().clone();
    let long = root.database("archive_copy").unwrap().clone();
    assert_eq!(short.pin().name(), "archive");
    assert_eq!(short.pin().commit().as_str(), short_parent_commit);
    assert_eq!(long.pin().name(), "archive_copy");
    assert_eq!(long.pin().commit().as_str(), long_parent_commit);
    assert_ne!(short.pin().commit().as_str(), latest_commit);

    let short_closure = resolver.resolve_for_parent(short).unwrap();
    let short_child = short_closure.database("archive_copy").unwrap().clone();
    assert_eq!(short_child.pin().commit().as_str(), short_child_commit);
    let short_leaf_closure = resolver.resolve_for_parent(short_child).unwrap();

    let long_closure = resolver.resolve_for_parent(long).unwrap();
    let long_leaf = long_closure.database("archive").unwrap();
    assert_eq!(long_leaf.pin().commit().as_str(), base_commit);
    assert_eq!(
        short_leaf_closure.database("archive").unwrap().pin(),
        long_leaf.pin()
    );
    assert_eq!(
        root.database("archive_copy").unwrap().pin().commit().as_str(),
        long_parent_commit
    );
}

#[test]
fn nested_equal_oid_aliases_remain_distinct_through_the_public_resolver() {
    let (_shared_dir, shared_repository, package_commit) = repository(&[(
        "main.orna",
        include_str!("fixtures/attach-package.orna"),
    )]);
    let root_manifest = format!(
        "archive {package_commit}\narchive_copy {package_commit}\n"
    );
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    let short = root.database("archive").unwrap().clone();
    let long = root.database("archive_copy").unwrap().clone();
    assert_eq!(short.pin().commit(), long.pin().commit());
    assert_eq!(short.pin().commit().as_str(), package_commit);
    assert_eq!(short.pin().name(), "archive");
    assert_eq!(long.pin().name(), "archive_copy");
    assert_ne!(short.pin(), long.pin());
    assert_eq!(root.attached().count(), 2);

    let short_closure = resolver.resolve_for_parent(short).unwrap();
    let long_closure = resolver.resolve_for_parent(long).unwrap();
    assert_eq!(short_closure.primary().pin().name(), "archive");
    assert_eq!(long_closure.primary().pin().name(), "archive_copy");
    assert_eq!(short_closure.attached().count(), 0);
    assert_eq!(long_closure.attached().count(), 0);
    assert_eq!(root.database("archive").unwrap().pin().name(), "archive");
    assert_eq!(root.database("archive_copy").unwrap().pin().name(), "archive_copy");
}

#[test]
fn nested_closure_module_routes_follow_exact_parent_aliases() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, base_commit) =
        repository(&[("main.orna", package_source)]);
    let short_source = package_source.replace("42", "43");
    let short_manifest = format!(
        "archive_copy {base_commit}\narchive_copy_archive {base_commit}\n"
    );
    let short_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &short_source),
            (PACKAGE_PIN_MANIFEST_PATH, &short_manifest),
        ],
        "short alias closure routes",
    );
    let long_source = package_source.replace("42", "44");
    let long_manifest = format!("archive {base_commit}\narchive_copy_archive {base_commit}\n");
    let long_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &long_source),
            (PACKAGE_PIN_MANIFEST_PATH, &long_manifest),
        ],
        "long alias closure routes",
    );

    let root_manifest = format!(
        "archive {short_parent_commit}\narchive_copy {long_parent_commit}\n"
    );
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository.clone()),
            ("archive_copy_archive".to_owned(), shared_repository),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    assert_module_route(&root, "archive.orna", "= 43");
    assert_module_route(&root, "archive_copy.orna", "= 44");

    let short = root.database("archive").unwrap().clone();
    let short_closure = resolver.resolve_for_parent(short).unwrap();
    assert_eq!(short_closure.primary().pin().name(), "archive");
    assert_eq!(short_closure.primary().pin().commit().as_str(), short_parent_commit);
    assert_module_route(&short_closure, "main.orna", "= 43");
    assert_module_route(&short_closure, "archive_copy.orna", "= 42");
    assert_module_route(&short_closure, "archive_copy_archive.orna", "= 42");

    let short_child = short_closure.database("archive_copy").unwrap().clone();
    let short_leaf = resolver.resolve_for_parent(short_child).unwrap();
    assert_eq!(short_leaf.primary().pin().name(), "archive_copy");
    assert_eq!(short_leaf.primary().pin().commit().as_str(), base_commit);
    assert_eq!(short_leaf.attached().count(), 0);

    let long = root.database("archive_copy").unwrap().clone();
    let long_closure = resolver.resolve_for_parent(long).unwrap();
    assert_eq!(long_closure.primary().pin().name(), "archive_copy");
    assert_eq!(long_closure.primary().pin().commit().as_str(), long_parent_commit);
    assert_module_route(&long_closure, "main.orna", "= 44");
    assert_module_route(&long_closure, "archive.orna", "= 42");
    assert_module_route(&long_closure, "archive_copy_archive.orna", "= 42");

    // The reference fixes each exact pin but does not specify prefix-related
    // alias precedence between closure levels. V1 resolves the selected
    // parent's complete alias before routing its direct child pins.
    assert_eq!(root.database("archive").unwrap().pin().commit().as_str(), short_parent_commit);
    assert_eq!(root.database("archive_copy").unwrap().pin().commit().as_str(), long_parent_commit);
}

#[test]
fn alternating_aliases_follow_each_selected_historical_closure_parent() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let short_leaf_source = package_source.replace("42", "50");
    let short_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &short_leaf_source)],
        "short route leaf snapshot",
    );
    let short_middle_source = package_source.replace("42", "51");
    let short_middle_manifest = format!("archive {short_leaf_commit}\n");
    let short_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &short_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &short_middle_manifest),
        ],
        "short route middle snapshot",
    );
    let prefixed_alias_source = package_source.replace("42", "52");
    let prefixed_alias_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &prefixed_alias_source)],
        "longer alias route snapshot",
    );
    let short_parent_source = package_source.replace("42", "43");
    let short_parent_manifest = format!(
        "archive_copy {short_middle_commit}\narchive_copy_archive {prefixed_alias_commit}\n"
    );
    let short_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &short_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &short_parent_manifest),
        ],
        "short route parent snapshot",
    );

    let long_leaf_source = package_source.replace("42", "60");
    let long_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &long_leaf_source)],
        "long route leaf snapshot",
    );
    let long_middle_source = package_source.replace("42", "61");
    let long_middle_manifest = format!("archive_copy {long_leaf_commit}\n");
    let long_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &long_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &long_middle_manifest),
        ],
        "long route middle snapshot",
    );
    let long_parent_source = package_source.replace("42", "44");
    let long_parent_manifest = format!("archive {long_middle_commit}\n");
    let long_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &long_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &long_parent_manifest),
        ],
        "long route parent snapshot",
    );

    let root_manifest = format!(
        "archive {short_parent_commit}\narchive_copy {long_parent_commit}\n"
    );
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository.clone()),
            ("archive_copy_archive".to_owned(), shared_repository),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    assert_module_route(&root, "archive.orna", "= 43");
    assert_module_route(&root, "archive_copy.orna", "= 44");

    let short_parent = root.database("archive").unwrap().clone();
    let short_closure = resolver.resolve_for_parent(short_parent).unwrap();
    assert_module_route(&short_closure, "main.orna", "= 43");
    assert_module_route(&short_closure, "archive_copy.orna", "= 51");
    assert_module_route(&short_closure, "archive_copy_archive.orna", "= 52");
    assert_eq!(
        short_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        short_middle_commit
    );
    assert_eq!(
        short_closure
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        prefixed_alias_commit
    );
    let short_middle = short_closure.database("archive_copy").unwrap().clone();
    let short_middle_closure = resolver.resolve_for_parent(short_middle).unwrap();
    assert_module_route(&short_middle_closure, "main.orna", "= 51");
    assert_module_route(&short_middle_closure, "archive.orna", "= 50");
    assert_eq!(
        short_middle_closure.primary().pin().commit().as_str(),
        short_middle_commit
    );
    assert_eq!(
        short_middle_closure.database("archive").unwrap().pin().commit().as_str(),
        short_leaf_commit
    );

    let long_parent = root.database("archive_copy").unwrap().clone();
    let long_closure = resolver.resolve_for_parent(long_parent).unwrap();
    assert_module_route(&long_closure, "main.orna", "= 44");
    assert_module_route(&long_closure, "archive.orna", "= 61");
    let long_middle = long_closure.database("archive").unwrap().clone();
    let long_middle_closure = resolver.resolve_for_parent(long_middle).unwrap();
    assert_module_route(&long_middle_closure, "main.orna", "= 61");
    assert_module_route(&long_middle_closure, "archive_copy.orna", "= 60");
    assert_eq!(
        long_middle_closure.primary().pin().commit().as_str(),
        long_middle_commit
    );
    assert_eq!(
        long_middle_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        long_leaf_commit
    );
}

#[test]
fn nested_alias_rebind_changes_only_its_route_and_uses_the_replacement_closure() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let old_short_source = package_source.replace("42", "50");
    let old_short_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_short_source)],
        "historical short alias snapshot",
    );
    let old_middle_source = package_source.replace("42", "51");
    let old_middle_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_middle_source)],
        "historical middle alias snapshot",
    );
    let old_long_source = package_source.replace("42", "52");
    let old_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_long_source)],
        "historical long alias snapshot",
    );

    let replacement_middle_source = package_source.replace("42", "61");
    let replacement_middle_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &replacement_middle_source)],
        "replacement middle closure snapshot",
    );
    let replacement_long_source = package_source.replace("42", "62");
    let replacement_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &replacement_long_source)],
        "replacement long closure snapshot",
    );
    let replacement_source = package_source.replace("42", "60");
    let replacement_manifest = format!(
        "archive_copy {replacement_middle_commit}\narchive_copy_archive {replacement_long_commit}\n"
    );
    let replacement_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_manifest),
        ],
        "replacement short alias with a new closure",
    );

    let selected_parent_source = package_source.replace("42", "44");
    let selected_parent_manifest = format!(
        "archive {old_short_commit}\narchive_copy {old_middle_commit}\narchive_copy_archive {old_long_commit}\n"
    );
    let selected_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &selected_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &selected_parent_manifest),
        ],
        "nested alias parent with three prefix-related pins",
    );

    let root_manifest = format!("archive_copy_archive_archive {selected_parent_commit}\n");
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository.clone()),
            ("archive_copy_archive".to_owned(), shared_repository.clone()),
            ("archive_copy_archive_archive".to_owned(), shared_repository.clone()),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    let selected = root
        .database("archive_copy_archive_archive")
        .unwrap()
        .clone();
    let mut rebound_session = resolver.resolve_for_parent(selected.clone()).unwrap();
    let sibling_session = resolver.resolve_for_parent(selected).unwrap();
    assert_eq!(
        rebound_session.primary().pin().commit().as_str(),
        selected_parent_commit
    );

    let replacement = PinnedDatabase::resolve(
        "archive",
        shared_repository,
        &replacement_commit,
        loader,
    )
    .unwrap();
    rebound_session.detach_database("archive").unwrap();
    rebound_session
        .attach_database(replacement.clone())
        .unwrap();

    assert_module_route(&rebound_session, "main.orna", "= 44");
    assert_module_route(&rebound_session, "archive.orna", "= 60");
    assert_module_route(&rebound_session, "archive_copy.orna", "= 51");
    assert_module_route(&rebound_session, "archive_copy_archive.orna", "= 52");
    assert_eq!(
        rebound_session.database("archive").unwrap().pin().commit().as_str(),
        replacement_commit
    );
    assert_eq!(
        rebound_session
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        old_middle_commit
    );
    assert_eq!(
        rebound_session
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        old_long_commit
    );

    assert_module_route(&sibling_session, "archive.orna", "= 50");
    assert_module_route(&sibling_session, "archive_copy.orna", "= 51");
    assert_module_route(&sibling_session, "archive_copy_archive.orna", "= 52");
    assert_eq!(
        sibling_session.database("archive").unwrap().pin().commit().as_str(),
        old_short_commit
    );

    let replacement_closure = resolver.resolve_for_parent(replacement).unwrap();
    assert_module_route(&replacement_closure, "main.orna", "= 60");
    assert_module_route(&replacement_closure, "archive_copy.orna", "= 61");
    assert_module_route(&replacement_closure, "archive_copy_archive.orna", "= 62");
    assert_eq!(
        replacement_closure.primary().pin().commit().as_str(),
        replacement_commit
    );
    assert_eq!(
        replacement_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_middle_commit
    );
    assert_eq!(
        replacement_closure
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_long_commit
    );
    assert_eq!(
        root.database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        selected_parent_commit
    );
}

#[test]
fn rebound_alias_precedence_survives_multiple_closure_depths() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let replacement_leaf_source = package_source.replace("42", "70");
    let replacement_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &replacement_leaf_source)],
        "replacement short terminal",
    );
    let deep_terminal_source = package_source.replace("42", "72");
    let deep_terminal_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &deep_terminal_source)],
        "replacement long terminal",
    );
    let replacement_middle_source = package_source.replace("42", "71");
    let replacement_middle_manifest =
        format!("archive_copy_archive {deep_terminal_commit}\n");
    let replacement_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_middle_manifest),
        ],
        "replacement closure middle",
    );

    let replacement_source = package_source.replace("42", "50");
    let replacement_manifest = format!(
        "archive {replacement_leaf_commit}\narchive_copy {replacement_middle_commit}\n"
    );
    let replacement_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_manifest),
        ],
        "rebound alias snapshot with two child routes",
    );
    let old_long_source = package_source.replace("42", "60");
    let old_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_long_source)],
        "historical long alias snapshot",
    );

    let nested_parent_source = package_source.replace("42", "53");
    let nested_parent_manifest = format!(
        "archive {replacement_commit}\narchive_copy_archive {old_long_commit}\n"
    );
    let nested_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &nested_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &nested_parent_manifest),
        ],
        "nested parent with short and long aliases",
    );
    let outer_parent_source = package_source.replace("42", "44");
    let outer_parent_manifest = format!(
        "archive_copy {nested_parent_commit}\narchive_copy_archive {nested_parent_commit}\n"
    );
    let outer_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_parent_manifest),
        ],
        "outer closure with equal prefix-related pins",
    );

    let root_manifest = format!("archive_copy_archive_archive {outer_parent_commit}\n");
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository.clone()),
            ("archive_copy_archive".to_owned(), shared_repository.clone()),
            ("archive_copy_archive_archive".to_owned(), shared_repository.clone()),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    let outer = root
        .database("archive_copy_archive_archive")
        .unwrap()
        .clone();
    let outer_closure = resolver.resolve_for_parent(outer).unwrap();
    let short_nested = outer_closure.database("archive_copy").unwrap().clone();
    let short_nested_closure = resolver.resolve_for_parent(short_nested).unwrap();
    assert_eq!(
        short_nested_closure.primary().pin().commit().as_str(),
        nested_parent_commit
    );
    assert_eq!(
        short_nested_closure
            .database("archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commit
    );
    let old_long_pin = short_nested_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    assert_eq!(old_long_pin.pin().commit().as_str(), old_long_commit);

    let replacement = PinnedDatabase::resolve(
        "archive_copy_archive",
        shared_repository,
        &replacement_commit,
        loader,
    )
    .unwrap();
    let mut rebound = short_nested_closure;
    rebound
        .detach_database("archive_copy_archive")
        .unwrap();
    rebound.attach_database(replacement.clone()).unwrap();
    assert_module_route(&rebound, "archive.orna", "= 50");
    assert_module_route(&rebound, "archive_copy_archive.orna", "= 50");
    assert_eq!(
        rebound.database("archive").unwrap().pin().commit(),
        replacement.pin().commit()
    );
    assert_eq!(
        rebound
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit(),
        replacement.pin().commit()
    );
    assert_ne!(
        rebound.database("archive").unwrap().pin(),
        rebound
            .database("archive_copy_archive")
            .unwrap()
            .pin()
    );

    let replacement_closure = resolver.resolve_for_parent(replacement).unwrap();
    assert_module_route(&replacement_closure, "main.orna", "= 50");
    assert_module_route(&replacement_closure, "archive.orna", "= 70");
    assert_module_route(&replacement_closure, "archive_copy.orna", "= 71");
    assert_eq!(
        replacement_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_middle_commit
    );
    let deep_middle = replacement_closure
        .database("archive_copy")
        .unwrap()
        .clone();
    let deep_closure = resolver.resolve_for_parent(deep_middle).unwrap();
    assert_module_route(&deep_closure, "main.orna", "= 71");
    assert_module_route(&deep_closure, "archive_copy_archive.orna", "= 72");
    assert_eq!(
        deep_closure
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        deep_terminal_commit
    );
    assert_eq!(old_long_pin.pin().commit().as_str(), old_long_commit);
}

#[test]
fn short_alias_rebound_to_long_pin_keeps_longer_nested_routes() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let deep_leaf_source = package_source.replace("42", "83");
    let deep_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &deep_leaf_source)],
        "short-to-long route terminal",
    );
    let nested_short_source = package_source.replace("42", "81");
    let nested_short_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &nested_short_source)],
        "short-to-long middle route",
    );
    let nested_long_source = package_source.replace("42", "82");
    let nested_long_manifest = format!("archive_copy {deep_leaf_commit}\n");
    let nested_long_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &nested_long_source),
            (PACKAGE_PIN_MANIFEST_PATH, &nested_long_manifest),
        ],
        "short-to-long nested route",
    );

    let replacement_source = package_source.replace("42", "80");
    let replacement_manifest = format!(
        "archive_copy {nested_short_commit}\narchive_copy_archive {nested_long_commit}\n"
    );
    let replacement_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_manifest),
        ],
        "long alias replacement snapshot",
    );
    let old_short_source = package_source.replace("42", "50");
    let old_short_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_short_source)],
        "old short alias snapshot",
    );
    let nested_parent_source = package_source.replace("42", "53");
    let nested_parent_manifest = format!(
        "archive {old_short_commit}\narchive_copy_archive {replacement_commit}\n"
    );
    let nested_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &nested_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &nested_parent_manifest),
        ],
        "nested parent before short alias rebind",
    );
    let outer_source = package_source.replace("42", "44");
    let outer_manifest = format!("archive_copy {nested_parent_commit}\n");
    let outer_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_manifest),
        ],
        "outer parent before short alias rebind",
    );

    let root_manifest = format!("archive_copy_archive_archive {outer_commit}\n");
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            ("archive".to_owned(), shared_repository.clone()),
            ("archive_copy".to_owned(), shared_repository.clone()),
            ("archive_copy_archive".to_owned(), shared_repository.clone()),
            ("archive_copy_archive_archive".to_owned(), shared_repository.clone()),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    let outer = root
        .database("archive_copy_archive_archive")
        .unwrap()
        .clone();
    let outer_closure = resolver.resolve_for_parent(outer).unwrap();
    let nested_parent = outer_closure.database("archive_copy").unwrap().clone();
    let mut nested_closure = resolver.resolve_for_parent(nested_parent).unwrap();
    let long_pin = nested_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    assert_eq!(long_pin.pin().commit().as_str(), replacement_commit);
    assert_eq!(
        nested_closure.database("archive").unwrap().pin().commit().as_str(),
        old_short_commit
    );

    let replacement = PinnedDatabase::resolve(
        "archive",
        shared_repository,
        &replacement_commit,
        loader,
    )
    .unwrap();
    nested_closure.detach_database("archive").unwrap();
    nested_closure
        .attach_database(replacement.clone())
        .unwrap();
    assert_module_route(&nested_closure, "main.orna", "= 53");
    assert_module_route(&nested_closure, "archive.orna", "= 80");
    assert_module_route(&nested_closure, "archive_copy_archive.orna", "= 80");
    assert_eq!(
        nested_closure.database("archive").unwrap().pin().commit(),
        long_pin.pin().commit()
    );
    assert_ne!(
        nested_closure.database("archive").unwrap().pin(),
        nested_closure
            .database("archive_copy_archive")
            .unwrap()
            .pin()
    );

    let replacement_closure = resolver.resolve_for_parent(replacement).unwrap();
    assert_module_route(&replacement_closure, "main.orna", "= 80");
    assert_module_route(&replacement_closure, "archive_copy.orna", "= 81");
    assert_module_route(&replacement_closure, "archive_copy_archive.orna", "= 82");
    assert_eq!(
        replacement_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        nested_short_commit
    );
    let long_nested = replacement_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    let deep_closure = resolver.resolve_for_parent(long_nested).unwrap();
    assert_module_route(&deep_closure, "main.orna", "= 82");
    assert_module_route(&deep_closure, "archive_copy.orna", "= 83");
    assert_eq!(
        deep_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        deep_leaf_commit
    );
    assert_eq!(long_pin.pin().commit().as_str(), replacement_commit);
}

fn assert_module_route(session: &AttachedDatabaseSession, path: &str, source_marker: &str) {
    assert!(
        session
            .module_inputs()
            .iter()
            .any(|module| module.logical_path == path && module.source.contains(source_marker)),
        "expected {path} to contain {source_marker}"
    );
}
