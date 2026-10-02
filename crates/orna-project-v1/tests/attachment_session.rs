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

fn write_package_snapshot(
    directory: &Path,
    package_source: &str,
    marker: &str,
    manifest: Option<&str>,
    message: &str,
) -> String {
    let source = package_source.replace("42", marker);
    match manifest {
        Some(manifest) => write_commit(
            directory,
            &[
                ("main.orna", &source),
                (PACKAGE_PIN_MANIFEST_PATH, manifest),
            ],
            message,
        ),
        None => write_commit(directory, &[("main.orna", &source)], message),
    }
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
fn repeated_rebinds_preserve_exact_alias_precedence_at_each_closure_depth() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let old_short_source = package_source.replace("42", "40");
    let old_short_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_short_source)],
        "original short route",
    );
    let stable_long_source = package_source.replace("42", "42");
    let stable_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &stable_long_source)],
        "stable long sibling route",
    );

    let replacement_terminal_tail_source = package_source.replace("42", "62");
    let replacement_terminal_tail_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &replacement_terminal_tail_source)],
        "terminal child after the second rebind",
    );
    let replacement_terminal_source = package_source.replace("42", "61");
    let replacement_terminal_manifest = format!(
        "archive_copy_archive_archive {replacement_terminal_tail_commit}\n"
    );
    let replacement_terminal_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_terminal_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_terminal_manifest),
        ],
        "terminal route after the second rebind",
    );

    let second_rebound_source = package_source.replace("42", "60");
    let second_rebound_manifest =
        format!("archive_copy_archive {replacement_terminal_commit}\n");
    let second_rebound_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &second_rebound_source),
            (PACKAGE_PIN_MANIFEST_PATH, &second_rebound_manifest),
        ],
        "second rebound alias with its own nested closure",
    );

    let first_middle_source = package_source.replace("42", "51");
    let first_middle_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &first_middle_source)],
        "first rebound middle route",
    );
    let first_long_source = package_source.replace("42", "52");
    let first_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &first_long_source)],
        "first rebound longer sibling route",
    );
    let first_rebound_source = package_source.replace("42", "50");
    let first_rebound_manifest = format!(
        "archive_copy {first_middle_commit}\narchive_copy_archive {first_long_commit}\n"
    );
    let first_rebound_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &first_rebound_source),
            (PACKAGE_PIN_MANIFEST_PATH, &first_rebound_manifest),
        ],
        "first rebound alias with prefix-related child routes",
    );

    let selected_parent_source = package_source.replace("42", "43");
    let selected_parent_manifest = format!(
        "archive {old_short_commit}\narchive_copy_archive {stable_long_commit}\n"
    );
    let selected_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &selected_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &selected_parent_manifest),
        ],
        "selected nested parent before either rebind",
    );
    let outer_parent_source = package_source.replace("42", "44");
    let outer_parent_manifest = format!("archive_copy {selected_parent_commit}\n");
    let outer_parent_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_parent_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_parent_manifest),
        ],
        "outer closure leading to the selected nested parent",
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
            (
                "archive_copy_archive_archive".to_owned(),
                shared_repository.clone(),
            ),
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
    let selected_parent = outer_closure.database("archive_copy").unwrap().clone();
    assert_eq!(
        selected_parent.pin().commit().as_str(),
        selected_parent_commit
    );
    let mut selected_closure = resolver.resolve_for_parent(selected_parent).unwrap();

    let first_rebound = PinnedDatabase::resolve(
        "archive",
        shared_repository.clone(),
        &first_rebound_commit,
        loader,
    )
    .unwrap();
    selected_closure.detach_database("archive").unwrap();
    selected_closure
        .attach_database(first_rebound.clone())
        .unwrap();
    assert_module_route(&selected_closure, "archive.orna", "= 50");
    assert_module_route(&selected_closure, "archive_copy_archive.orna", "= 42");

    let mut first_rebound_closure = resolver.resolve_for_parent(first_rebound).unwrap();
    assert_module_route(&first_rebound_closure, "main.orna", "= 50");
    assert_module_route(&first_rebound_closure, "archive_copy.orna", "= 51");
    assert_module_route(&first_rebound_closure, "archive_copy_archive.orna", "= 52");

    let second_rebound = PinnedDatabase::resolve(
        "archive_copy",
        shared_repository,
        &second_rebound_commit,
        loader,
    )
    .unwrap();
    first_rebound_closure
        .detach_database("archive_copy")
        .unwrap();
    first_rebound_closure
        .attach_database(second_rebound.clone())
        .unwrap();
    assert_module_route(&first_rebound_closure, "archive_copy.orna", "= 60");
    assert_module_route(
        &first_rebound_closure,
        "archive_copy_archive.orna",
        "= 52",
    );
    assert_eq!(
        first_rebound_closure
            .database("archive_copy")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        second_rebound_commit
    );
    assert_eq!(
        first_rebound_closure
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        first_long_commit
    );

    let second_rebound_closure = resolver.resolve_for_parent(second_rebound).unwrap();
    assert_module_route(&second_rebound_closure, "main.orna", "= 60");
    assert_module_route(
        &second_rebound_closure,
        "archive_copy_archive.orna",
        "= 61",
    );
    let terminal = second_rebound_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    let terminal_closure = resolver.resolve_for_parent(terminal).unwrap();
    assert_module_route(&terminal_closure, "main.orna", "= 61");
    assert_module_route(
        &terminal_closure,
        "archive_copy_archive_archive.orna",
        "= 62",
    );
    assert_eq!(
        terminal_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_terminal_tail_commit
    );
}

#[test]
fn equal_oid_rebound_aliases_keep_independent_nested_route_identity() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let leaf_source = package_source.replace("42", "82");
    let leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &leaf_source)],
        "equal-OID rebound route leaf",
    );
    let shared_source = package_source.replace("42", "80");
    let shared_manifest = format!("archive_copy_archive_archive {leaf_commit}\n");
    let shared_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &shared_source),
            (PACKAGE_PIN_MANIFEST_PATH, &shared_manifest),
        ],
        "shared snapshot for short and long aliases",
    );
    let old_source = package_source.replace("42", "50");
    let old_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_source)],
        "old short alias before equal-OID rebind",
    );
    let selected_source = package_source.replace("42", "70");
    let selected_manifest = format!(
        "archive {old_commit}\narchive_copy_archive {shared_commit}\n"
    );
    let selected_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &selected_source),
            (PACKAGE_PIN_MANIFEST_PATH, &selected_manifest),
        ],
        "nested parent with prefix-related aliases at one OID",
    );
    let outer_source = package_source.replace("42", "71");
    let outer_manifest = format!("archive_copy {selected_commit}\n");
    let outer_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_manifest),
        ],
        "outer closure before equal-OID rebind",
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
            (
                "archive_copy_archive_archive".to_owned(),
                shared_repository.clone(),
            ),
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
    let selected = outer_closure.database("archive_copy").unwrap().clone();
    let mut rebound = resolver.resolve_for_parent(selected).unwrap();
    assert_module_route(&rebound, "archive.orna", "= 50");
    assert_module_route(&rebound, "archive_copy_archive.orna", "= 80");

    let replacement = PinnedDatabase::resolve(
        "archive",
        shared_repository,
        &shared_commit,
        loader,
    )
    .unwrap();
    rebound.detach_database("archive").unwrap();
    rebound.attach_database(replacement).unwrap();

    let short_alias = rebound.database("archive").unwrap().clone();
    let long_alias = rebound.database("archive_copy_archive").unwrap().clone();
    assert_eq!(short_alias.pin().commit().as_str(), shared_commit);
    assert_eq!(long_alias.pin().commit().as_str(), shared_commit);
    assert_ne!(short_alias.pin().name(), long_alias.pin().name());
    assert_module_route(&rebound, "archive.orna", "= 80");
    assert_module_route(&rebound, "archive_copy_archive.orna", "= 80");

    let short_closure = resolver.resolve_for_parent(short_alias).unwrap();
    let long_closure = resolver.resolve_for_parent(long_alias).unwrap();
    assert_eq!(short_closure.primary().pin().name(), "archive");
    assert_eq!(long_closure.primary().pin().name(), "archive_copy_archive");
    assert_eq!(
        short_closure.primary().pin().commit(),
        long_closure.primary().pin().commit()
    );
    assert_module_route(
        &short_closure,
        "archive_copy_archive_archive.orna",
        "= 82",
    );
    assert_module_route(
        &long_closure,
        "archive_copy_archive_archive.orna",
        "= 82",
    );
    assert_eq!(
        short_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        leaf_commit
    );
    assert_eq!(
        long_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        leaf_commit
    );
}

#[test]
fn rebound_depth_chains_keep_old_and_replacement_branches_independent() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let old_short_source = package_source.replace("42", "30");
    let old_short_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_short_source)],
        "old short route before the chain rebind",
    );
    let old_long_source = package_source.replace("42", "32");
    let old_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_long_source)],
        "old long sibling before the chain rebind",
    );

    let old_leaf_source = package_source.replace("42", "45");
    let old_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &old_leaf_source)],
        "leaf from the original descendant chain",
    );
    let old_terminal_source = package_source.replace("42", "43");
    let old_terminal_manifest = format!("archive_copy_archive_archive {old_leaf_commit}\n");
    let old_terminal_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &old_terminal_source),
            (PACKAGE_PIN_MANIFEST_PATH, &old_terminal_manifest),
        ],
        "terminal from the original descendant chain",
    );
    let old_middle_source = package_source.replace("42", "41");
    let old_middle_manifest = format!("archive_copy_archive {old_terminal_commit}\n");
    let old_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &old_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &old_middle_manifest),
        ],
        "middle from the original descendant chain",
    );

    let replacement_leaf_source = package_source.replace("42", "95");
    let replacement_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &replacement_leaf_source)],
        "leaf from the rebound descendant chain",
    );
    let replacement_terminal_source = package_source.replace("42", "93");
    let replacement_terminal_manifest =
        format!("archive_copy_archive_archive {replacement_leaf_commit}\n");
    let replacement_terminal_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_terminal_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_terminal_manifest),
        ],
        "terminal from the rebound descendant chain",
    );
    let replacement_middle_source = package_source.replace("42", "91");
    let replacement_middle_manifest =
        format!("archive_copy_archive {replacement_terminal_commit}\n");
    let replacement_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_middle_manifest),
        ],
        "middle from the rebound descendant chain",
    );

    let replacement_source = package_source.replace("42", "50");
    let replacement_manifest = format!("archive_copy {old_middle_commit}\n");
    let replacement_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_manifest),
        ],
        "rebound ancestor with an original descendant branch",
    );
    let selected_source = package_source.replace("42", "70");
    let selected_manifest = format!(
        "archive {old_short_commit}\narchive_copy_archive {old_long_commit}\n"
    );
    let selected_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &selected_source),
            (PACKAGE_PIN_MANIFEST_PATH, &selected_manifest),
        ],
        "selected parent before the ancestor rebind",
    );
    let outer_source = package_source.replace("42", "71");
    let outer_manifest = format!("archive_copy {selected_commit}\n");
    let outer_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_manifest),
        ],
        "outer parent leading into the selected chain",
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
            (
                "archive_copy_archive_archive".to_owned(),
                shared_repository.clone(),
            ),
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
    let selected = outer_closure.database("archive_copy").unwrap().clone();
    let mut selected_closure = resolver.resolve_for_parent(selected).unwrap();
    let old_selected_closure = selected_closure.clone();

    let replacement = PinnedDatabase::resolve(
        "archive",
        shared_repository.clone(),
        &replacement_commit,
        loader,
    )
    .unwrap();
    selected_closure.detach_database("archive").unwrap();
    selected_closure
        .attach_database(replacement.clone())
        .unwrap();
    assert_module_route(&selected_closure, "main.orna", "= 70");
    assert_module_route(&selected_closure, "archive.orna", "= 50");
    assert_module_route(&selected_closure, "archive_copy_archive.orna", "= 32");
    assert_module_route(&old_selected_closure, "archive.orna", "= 30");
    assert_module_route(
        &old_selected_closure,
        "archive_copy_archive.orna",
        "= 32",
    );

    let mut original_chain = resolver.resolve_for_parent(replacement).unwrap();
    assert_module_route(&original_chain, "main.orna", "= 50");
    assert_module_route(&original_chain, "archive_copy.orna", "= 41");
    let original_middle = original_chain.database("archive_copy").unwrap().clone();
    let original_middle_snapshot = original_chain.clone();

    let replacement_middle = PinnedDatabase::resolve(
        "archive_copy",
        shared_repository.clone(),
        &replacement_middle_commit,
        loader,
    )
    .unwrap();
    original_chain.detach_database("archive_copy").unwrap();
    original_chain
        .attach_database(replacement_middle.clone())
        .unwrap();
    assert_module_route(&original_chain, "main.orna", "= 50");
    assert_module_route(&original_chain, "archive_copy.orna", "= 91");
    assert_module_route(&original_middle_snapshot, "archive_copy.orna", "= 41");

    let replacement_middle_closure =
        resolver.resolve_for_parent(replacement_middle).unwrap();
    assert_module_route(&replacement_middle_closure, "main.orna", "= 91");
    assert_module_route(
        &replacement_middle_closure,
        "archive_copy_archive.orna",
        "= 93",
    );
    let replacement_terminal = replacement_middle_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    let replacement_terminal_closure =
        resolver.resolve_for_parent(replacement_terminal).unwrap();
    assert_module_route(&replacement_terminal_closure, "main.orna", "= 93");
    assert_module_route(
        &replacement_terminal_closure,
        "archive_copy_archive_archive.orna",
        "= 95",
    );
    assert_eq!(
        replacement_terminal_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_leaf_commit
    );

    let original_middle_closure = resolver.resolve_for_parent(original_middle).unwrap();
    assert_module_route(&original_middle_closure, "main.orna", "= 41");
    assert_module_route(
        &original_middle_closure,
        "archive_copy_archive.orna",
        "= 43",
    );
    let original_terminal = original_middle_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    let original_terminal_closure = resolver.resolve_for_parent(original_terminal).unwrap();
    assert_module_route(&original_terminal_closure, "main.orna", "= 43");
    assert_module_route(
        &original_terminal_closure,
        "archive_copy_archive_archive.orna",
        "= 45",
    );
    assert_eq!(
        original_terminal_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        old_leaf_commit
    );
}

#[test]
fn nested_rebind_chain_keeps_longer_sibling_and_uses_each_replacement_manifest() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let original_leaf_source = package_source.replace("42", "45");
    let original_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &original_leaf_source)],
        "original chain leaf",
    );
    let original_terminal_source = package_source.replace("42", "43");
    let original_terminal_manifest =
        format!("archive_copy_archive_archive {original_leaf_commit}\n");
    let original_terminal_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &original_terminal_source),
            (PACKAGE_PIN_MANIFEST_PATH, &original_terminal_manifest),
        ],
        "original chain terminal",
    );
    let original_long_sibling_source = package_source.replace("42", "46");
    let original_long_sibling_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &original_long_sibling_source)],
        "original chain long sibling",
    );
    let original_middle_source = package_source.replace("42", "41");
    let original_middle_manifest = format!(
        "archive_copy_archive {original_terminal_commit}\narchive_copy_archive_archive {original_long_sibling_commit}\n"
    );
    let original_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &original_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &original_middle_manifest),
        ],
        "original chain middle",
    );

    let rebound_leaf_source = package_source.replace("42", "99");
    let rebound_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &rebound_leaf_source)],
        "rebound chain leaf",
    );
    let rebound_terminal_source = package_source.replace("42", "97");
    let rebound_terminal_manifest =
        format!("archive_copy_archive_archive {rebound_leaf_commit}\n");
    let rebound_terminal_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &rebound_terminal_source),
            (PACKAGE_PIN_MANIFEST_PATH, &rebound_terminal_manifest),
        ],
        "rebound chain terminal",
    );
    let alternate_leaf_source = package_source.replace("42", "94");
    let alternate_leaf_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &alternate_leaf_source)],
        "alternate terminal leaf before rebind",
    );
    let alternate_terminal_source = package_source.replace("42", "93");
    let alternate_terminal_manifest =
        format!("archive_copy_archive_archive {alternate_leaf_commit}\n");
    let alternate_terminal_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &alternate_terminal_source),
            (PACKAGE_PIN_MANIFEST_PATH, &alternate_terminal_manifest),
        ],
        "alternate terminal before rebind",
    );
    let rebound_long_sibling_source = package_source.replace("42", "95");
    let rebound_long_sibling_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &rebound_long_sibling_source)],
        "rebound middle long sibling",
    );
    let rebound_middle_source = package_source.replace("42", "91");
    let rebound_middle_manifest = format!(
        "archive_copy_archive {alternate_terminal_commit}\narchive_copy_archive_archive {rebound_long_sibling_commit}\n"
    );
    let rebound_middle_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &rebound_middle_source),
            (PACKAGE_PIN_MANIFEST_PATH, &rebound_middle_manifest),
        ],
        "rebound middle with a prefix-related sibling",
    );

    let replacement_source = package_source.replace("42", "50");
    let replacement_manifest = format!("archive_copy {original_middle_commit}\n");
    let replacement_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &replacement_source),
            (PACKAGE_PIN_MANIFEST_PATH, &replacement_manifest),
        ],
        "rebound ancestor before nested rebinds",
    );
    let selected_short_source = package_source.replace("42", "30");
    let selected_short_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &selected_short_source)],
        "old selected short alias",
    );
    let selected_long_source = package_source.replace("42", "32");
    let selected_long_commit = write_commit(
        shared_dir.path(),
        &[("main.orna", &selected_long_source)],
        "old selected longer sibling",
    );
    let selected_source = package_source.replace("42", "70");
    let selected_manifest = format!(
        "archive {selected_short_commit}\narchive_copy_archive {selected_long_commit}\n"
    );
    let selected_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &selected_source),
            (PACKAGE_PIN_MANIFEST_PATH, &selected_manifest),
        ],
        "selected nested parent before rebind",
    );
    let outer_source = package_source.replace("42", "71");
    let outer_manifest = format!("archive_copy {selected_commit}\n");
    let outer_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_manifest),
        ],
        "outer parent before nested rebind chain",
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
            (
                "archive_copy_archive_archive".to_owned(),
                shared_repository.clone(),
            ),
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
    let selected = outer_closure.database("archive_copy").unwrap().clone();
    let mut selected_closure = resolver.resolve_for_parent(selected).unwrap();

    let replacement = PinnedDatabase::resolve(
        "archive",
        shared_repository.clone(),
        &replacement_commit,
        loader,
    )
    .unwrap();
    selected_closure.detach_database("archive").unwrap();
    selected_closure
        .attach_database(replacement.clone())
        .unwrap();
    assert_module_route(&selected_closure, "archive.orna", "= 50");
    assert_module_route(&selected_closure, "archive_copy_archive.orna", "= 32");

    let mut replacement_ancestor_closure = resolver.resolve_for_parent(replacement).unwrap();
    let original_middle = replacement_ancestor_closure
        .database("archive_copy")
        .unwrap()
        .clone();
    let replacement_middle = PinnedDatabase::resolve(
        "archive_copy",
        shared_repository.clone(),
        &rebound_middle_commit,
        loader,
    )
    .unwrap();
    replacement_ancestor_closure
        .detach_database("archive_copy")
        .unwrap();
    replacement_ancestor_closure
        .attach_database(replacement_middle.clone())
        .unwrap();
    assert_module_route(&replacement_ancestor_closure, "archive_copy.orna", "= 91");

    let mut replacement_middle_closure =
        resolver.resolve_for_parent(replacement_middle).unwrap();
    let alternate_terminal = replacement_middle_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    assert_module_route(
        &replacement_middle_closure,
        "archive_copy_archive_archive.orna",
        "= 95",
    );
    let rebound_terminal = PinnedDatabase::resolve(
        "archive_copy_archive",
        shared_repository.clone(),
        &rebound_terminal_commit,
        loader,
    )
    .unwrap();
    replacement_middle_closure
        .detach_database("archive_copy_archive")
        .unwrap();
    replacement_middle_closure
        .attach_database(rebound_terminal.clone())
        .unwrap();
    assert_module_route(
        &replacement_middle_closure,
        "archive_copy_archive.orna",
        "= 97",
    );
    assert_module_route(
        &replacement_middle_closure,
        "archive_copy_archive_archive.orna",
        "= 95",
    );
    assert_eq!(
        replacement_middle_closure
            .database("archive_copy_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        rebound_terminal_commit
    );
    assert_eq!(
        replacement_middle_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        rebound_long_sibling_commit
    );

    let rebound_terminal_closure = resolver.resolve_for_parent(rebound_terminal).unwrap();
    assert_module_route(&rebound_terminal_closure, "main.orna", "= 97");
    assert_module_route(
        &rebound_terminal_closure,
        "archive_copy_archive_archive.orna",
        "= 99",
    );
    assert_eq!(
        rebound_terminal_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        rebound_leaf_commit
    );

    let alternate_terminal_closure =
        resolver.resolve_for_parent(alternate_terminal).unwrap();
    assert_module_route(&alternate_terminal_closure, "main.orna", "= 93");
    assert_module_route(
        &alternate_terminal_closure,
        "archive_copy_archive_archive.orna",
        "= 94",
    );
    assert_eq!(
        alternate_terminal_closure
            .database("archive_copy_archive_archive")
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        alternate_leaf_commit
    );

    let original_middle_closure = resolver.resolve_for_parent(original_middle).unwrap();
    assert_module_route(&original_middle_closure, "main.orna", "= 41");
    assert_module_route(
        &original_middle_closure,
        "archive_copy_archive.orna",
        "= 43",
    );
    assert_module_route(
        &original_middle_closure,
        "archive_copy_archive_archive.orna",
        "= 46",
    );
    let original_terminal = original_middle_closure
        .database("archive_copy_archive")
        .unwrap()
        .clone();
    let original_terminal_closure = resolver.resolve_for_parent(original_terminal).unwrap();
    assert_module_route(&original_terminal_closure, "main.orna", "= 43");
    assert_module_route(
        &original_terminal_closure,
        "archive_copy_archive_archive.orna",
        "= 45",
    );
}

#[test]
fn alias_rebind_storm_keeps_latest_exact_manifest_and_prior_route_snapshots() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);

    let original_short = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "20",
        None,
        "original short route before rebound storm",
    );
    let original_long = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "30",
        None,
        "original longer prefix route before rebound storm",
    );
    let original_deep = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "40",
        None,
        "original deepest prefix route before rebound storm",
    );

    let mut replacements = Vec::new();
    for revision in 0..3 {
        let child = write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 60 + revision),
            None,
            &format!("storm child revision {revision}"),
        );
        let deep_sibling = write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 70 + revision),
            None,
            &format!("storm deep sibling revision {revision}"),
        );
        let manifest = format!(
            "archive_copy {child}\narchive_copy_archive {deep_sibling}\n"
        );
        let replacement = write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 50 + revision),
            Some(&manifest),
            &format!("short alias rebound revision {revision}"),
        );
        replacements.push(replacement);
    }

    let root_manifest = format!(
        "archive {original_short}\narchive_copy {original_long}\narchive_copy_archive {original_deep}\n"
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
            ("archive_copy_archive".to_owned(), shared_repository.clone()),
        ],
        loader,
    )
    .unwrap();

    let mut session = resolver.resolve_for_parent(primary).unwrap();
    let original_session = session.clone();
    for revision in [0, 1, 2, 0, 2, 1, 2] {
        let replacement = PinnedDatabase::resolve(
            "archive",
            shared_repository.clone(),
            &replacements[revision],
            loader,
        )
        .unwrap();
        session.detach_database("archive").unwrap();
        session.attach_database(replacement).unwrap();

        assert_module_route(
            &session,
            "archive.orna",
            &format!("= {}", 50 + revision),
        );
        assert_module_route(&session, "archive_copy.orna", "= 30");
        assert_module_route(&session, "archive_copy_archive.orna", "= 40");
        assert_module_route(&original_session, "archive.orna", "= 20");
        assert_module_route(&original_session, "archive_copy.orna", "= 30");
    }

    let latest = PinnedDatabase::resolve(
        "archive",
        shared_repository,
        &replacements[2],
        loader,
    )
    .unwrap();
    let latest_closure = resolver.resolve_for_parent(latest).unwrap();
    assert_module_route(&latest_closure, "main.orna", "= 52");
    assert_module_route(&latest_closure, "archive_copy.orna", "= 62");
    assert_module_route(&latest_closure, "archive_copy_archive.orna", "= 72");
}

#[test]
fn deep_alias_rebind_storm_uses_each_latest_parent_manifest() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
        "archive_copy_archive_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let cycle_count = 3;

    let mut original_commits = Vec::new();
    for (index, _) in aliases.iter().enumerate() {
        original_commits.push(write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 10 + index),
            None,
            &format!("original alias route {index}"),
        ));
    }

    let mut sibling_commits: Vec<Vec<Vec<String>>> = (0..depth_count)
        .map(|_| {
            (0..cycle_count)
                .map(|_| vec![String::new(); aliases.len()])
                .collect()
        })
        .collect();
    for depth in 0..depth_count {
        for cycle in 0..cycle_count {
            for alias_index in depth + 2..aliases.len() {
                let marker = 150 + depth * 20 + cycle * 5 + alias_index;
                sibling_commits[depth][cycle][alias_index] = write_package_snapshot(
                    shared_dir.path(),
                    package_source,
                    &format!("{marker}"),
                    None,
                    &format!("depth {depth} cycle {cycle} sibling {alias_index}"),
                );
            }
        }
    }

    let mut terminal_commits = Vec::new();
    for cycle in 0..cycle_count {
        terminal_commits.push(write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 240 + cycle),
            None,
            &format!("terminal route selected by cycle {cycle}"),
        ));
    }

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); cycle_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for cycle in 0..cycle_count {
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][cycle]
            } else {
                &terminal_commits[cycle]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[child_index]);
            for alias_index in depth + 2..aliases.len() {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[alias_index], sibling_commits[depth][cycle][alias_index]
                ));
            }
            let marker = 80 + depth * 10 + cycle;
            replacement_commits[depth][cycle] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{marker}"),
                Some(&manifest),
                &format!("depth {depth} replacement cycle {cycle}"),
            );
        }
    }

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();

    let mut current = resolver.resolve_for_parent(primary).unwrap();
    for depth in 0..depth_count {
        let original_current = current.clone();
        let original_marker = if depth == 0 {
            10 + depth
        } else {
            80 + depth * 10 + cycle_count - 1
        };
        let retained_markers: Vec<_> = (depth + 1..aliases.len())
            .map(|alias_index| {
                if depth == 0 {
                    10 + alias_index
                } else {
                    150 + (depth - 1) * 20 + (cycle_count - 1) * 5 + alias_index
                }
            })
            .collect();

        if depth > 0 {
            assert_module_route(
                &current,
                "main.orna",
                &format!("= {}", 80 + (depth - 1) * 10 + cycle_count - 1),
            );
        }
        for cycle in 0..cycle_count {
            let replacement = PinnedDatabase::resolve(
                aliases[depth],
                shared_repository.clone(),
                &replacement_commits[depth][cycle],
                loader,
            )
            .unwrap();
            current.detach_database(aliases[depth]).unwrap();
            current.attach_database(replacement).unwrap();

            assert_module_route(
                &current,
                &format!("{}.orna", aliases[depth]),
                &format!("= {}", 80 + depth * 10 + cycle),
            );
            for (offset, alias_index) in (depth + 1..aliases.len()).enumerate() {
                assert_module_route(
                    &current,
                    &format!("{}.orna", aliases[alias_index]),
                    &format!("= {}", retained_markers[offset]),
                );
            }
            assert_module_route(
                &original_current,
                &format!("{}.orna", aliases[depth]),
                &format!("= {original_marker}"),
            );
        }

        let selected = current.database(aliases[depth]).unwrap().clone();
        assert_eq!(
            selected.pin().commit().as_str(),
            replacement_commits[depth][cycle_count - 1]
        );
        let next = resolver.resolve_for_parent(selected).unwrap();
        assert_module_route(
            &next,
            "main.orna",
            &format!("= {}", 80 + depth * 10 + cycle_count - 1),
        );
        for alias_index in depth + 1..aliases.len() {
            let marker = if alias_index == depth + 1 {
                if alias_index < depth_count {
                    80 + alias_index * 10 + cycle_count - 1
                } else {
                    240 + cycle_count - 1
                }
            } else {
                150 + depth * 20 + (cycle_count - 1) * 5 + alias_index
            };
            assert_module_route(
                &next,
                &format!("{}.orna", aliases[alias_index]),
                &format!("= {marker}"),
            );
        }
        current = next;
    }

    assert_eq!(
        current
            .database(aliases[depth_count])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        terminal_commits[cycle_count - 1]
    );
    assert_module_route(&current, "main.orna", "= 112");
    assert_module_route(
        &current,
        &format!("{}.orna", aliases[depth_count]),
        "= 242",
    );
}

#[test]
fn deep_rebind_chain_uses_exact_parent_aliases_at_each_depth() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let alias0 = "archive";
    let alias1 = "archive_copy";
    let alias2 = "archive_copy_archive";
    let alias3 = "archive_copy_archive_archive";
    let alias4 = "archive_copy_archive_archive_archive";
    let alias5 = "archive_copy_archive_archive_archive_archive";

    let old_short = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "30",
        None,
        "old short alias before deep rebinds",
    );
    let old_selected_long = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "33",
        None,
        "selected parent's longer sibling",
    );
    let old_selected_deep = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "34",
        None,
        "selected parent's deepest sibling",
    );
    let old_depth1 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "31",
        None,
        "first retained route in original chain",
    );
    let old_depth2 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "41",
        None,
        "second retained route in original chain",
    );
    let old_depth3 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "51",
        None,
        "third retained route in original chain",
    );
    let old_depth4 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "61",
        None,
        "fourth retained route in original chain",
    );
    let sibling1 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "35",
        None,
        "first retained prefix sibling",
    );
    let sibling2 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "55",
        None,
        "second retained prefix sibling",
    );
    let sibling3 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "65",
        None,
        "third retained prefix sibling",
    );
    let replacement_leaf = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "99",
        None,
        "leaf in final rebound chain",
    );

    let manifest4 = format!("{alias5} {replacement_leaf}\n");
    let replacement4 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "90",
        Some(&manifest4),
        "fourth replacement owns the final child",
    );
    let manifest3 = format!("{alias4} {old_depth4}\n{alias5} {sibling3}\n");
    let replacement3 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "80",
        Some(&manifest3),
        "third replacement with longer sibling",
    );
    let manifest2 = format!("{alias3} {old_depth3}\n{alias4} {sibling2}\n");
    let replacement2 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "70",
        Some(&manifest2),
        "second replacement with longer sibling",
    );
    let manifest1 = format!("{alias2} {old_depth2}\n{alias3} {sibling1}\n");
    let replacement1 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "60",
        Some(&manifest1),
        "first replacement with longer sibling",
    );
    let manifest0 = format!("{alias1} {old_depth1}\n");
    let replacement0 = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&manifest0),
        "ancestor replacement starts the new chain",
    );

    let selected_source = package_source.replace("42", "25");
    let selected_manifest = format!(
        "{alias0} {old_short}\n{alias3} {old_selected_long}\n{alias4} {old_selected_deep}\n"
    );
    let selected_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &selected_source),
            (PACKAGE_PIN_MANIFEST_PATH, &selected_manifest),
        ],
        "selected parent before the deepest alias chain rebinds",
    );
    let outer_source = package_source.replace("42", "24");
    let outer_manifest = format!("{alias2} {selected_commit}\n");
    let outer_commit = write_commit(
        shared_dir.path(),
        &[
            ("main.orna", &outer_source),
            (PACKAGE_PIN_MANIFEST_PATH, &outer_manifest),
        ],
        "outer parent for deep alias chain",
    );
    let root_manifest = format!("{alias4} {outer_commit}\n");
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let loader = ProjectLoader::default();
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        [
            (alias0.to_owned(), shared_repository.clone()),
            (alias1.to_owned(), shared_repository.clone()),
            (alias2.to_owned(), shared_repository.clone()),
            (alias3.to_owned(), shared_repository.clone()),
            (alias4.to_owned(), shared_repository.clone()),
            (alias5.to_owned(), shared_repository.clone()),
        ],
        loader,
    )
    .unwrap();

    let root = resolver.resolve_for_parent(primary).unwrap();
    let outer = root.database(alias4).unwrap().clone();
    let outer_closure = resolver.resolve_for_parent(outer).unwrap();
    let selected = outer_closure.database(alias2).unwrap().clone();
    let mut selected_closure = resolver.resolve_for_parent(selected).unwrap();
    let original_selected_closure = selected_closure.clone();
    let rebound0 = PinnedDatabase::resolve(
        alias0,
        shared_repository.clone(),
        &replacement0,
        loader,
    )
    .unwrap();
    selected_closure.detach_database(alias0).unwrap();
    selected_closure.attach_database(rebound0.clone()).unwrap();
    assert_module_route(&selected_closure, "archive.orna", "= 50");
    assert_module_route(
        &selected_closure,
        "archive_copy_archive_archive.orna",
        "= 33",
    );
    assert_module_route(
        &selected_closure,
        "archive_copy_archive_archive_archive.orna",
        "= 34",
    );
    assert_module_route(&original_selected_closure, "archive.orna", "= 30");

    let mut closure0 = resolver.resolve_for_parent(rebound0).unwrap();
    let original0 = closure0.clone();
    let rebound1 = PinnedDatabase::resolve(
        alias1,
        shared_repository.clone(),
        &replacement1,
        loader,
    )
    .unwrap();
    closure0.detach_database(alias1).unwrap();
    closure0.attach_database(rebound1.clone()).unwrap();
    assert_module_route(&closure0, "archive_copy.orna", "= 60");
    assert_module_route(&original0, "archive_copy.orna", "= 31");

    let mut closure1 = resolver.resolve_for_parent(rebound1).unwrap();
    let original1 = closure1.clone();
    let rebound2 = PinnedDatabase::resolve(
        alias2,
        shared_repository.clone(),
        &replacement2,
        loader,
    )
    .unwrap();
    closure1.detach_database(alias2).unwrap();
    closure1.attach_database(rebound2.clone()).unwrap();
    assert_module_route(&closure1, "archive_copy_archive.orna", "= 70");
    assert_module_route(&closure1, "archive_copy_archive_archive.orna", "= 35");
    assert_module_route(&original1, "archive_copy_archive.orna", "= 41");

    let mut closure2 = resolver.resolve_for_parent(rebound2).unwrap();
    let original2 = closure2.clone();
    let rebound3 = PinnedDatabase::resolve(
        alias3,
        shared_repository.clone(),
        &replacement3,
        loader,
    )
    .unwrap();
    closure2.detach_database(alias3).unwrap();
    closure2.attach_database(rebound3.clone()).unwrap();
    assert_module_route(&closure2, "archive_copy_archive_archive.orna", "= 80");
    assert_module_route(
        &closure2,
        "archive_copy_archive_archive_archive.orna",
        "= 55",
    );
    assert_module_route(&original2, "archive_copy_archive_archive.orna", "= 51");

    let mut closure3 = resolver.resolve_for_parent(rebound3).unwrap();
    let original3 = closure3.clone();
    let rebound4 = PinnedDatabase::resolve(
        alias4,
        shared_repository,
        &replacement4,
        loader,
    )
    .unwrap();
    closure3.detach_database(alias4).unwrap();
    closure3.attach_database(rebound4.clone()).unwrap();
    assert_module_route(
        &closure3,
        "archive_copy_archive_archive_archive.orna",
        "= 90",
    );
    assert_module_route(
        &closure3,
        "archive_copy_archive_archive_archive_archive.orna",
        "= 65",
    );
    assert_module_route(&original3, "archive_copy_archive_archive_archive.orna", "= 61");

    let closure4 = resolver.resolve_for_parent(rebound4).unwrap();
    assert_module_route(&closure4, "main.orna", "= 90");
    assert_module_route(
        &closure4,
        "archive_copy_archive_archive_archive_archive.orna",
        "= 99",
    );
    assert_eq!(
        closure4
            .database(alias5)
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_leaf
    );
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
