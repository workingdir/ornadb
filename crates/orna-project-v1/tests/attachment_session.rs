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
fn chained_closure_storm_branches_keep_exact_rebound_precedence() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let variant_count = 2;

    let replacement_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let sibling_marker = |depth: usize, variant: usize, alias_index: usize| {
        300 + depth * 20 + variant * 5 + alias_index
    };

    let mut original_commits = Vec::new();
    for (index, _) in aliases.iter().enumerate() {
        original_commits.push(write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 10 + index),
            None,
            &format!("initial exact alias route {index}"),
        ));
    }

    let mut sibling_commits: Vec<Vec<Vec<String>>> = (0..depth_count)
        .map(|_| {
            (0..variant_count)
                .map(|_| vec![String::new(); aliases.len()])
                .collect()
        })
        .collect();
    for depth in 0..depth_count {
        for variant in 0..variant_count {
            for alias_index in depth + 2..aliases.len() {
                let marker = sibling_marker(depth, variant, alias_index);
                sibling_commits[depth][variant][alias_index] = write_package_snapshot(
                    shared_dir.path(),
                    package_source,
                    &format!("{marker}"),
                    None,
                    &format!("depth {depth} variant {variant} sibling {alias_index}"),
                );
            }
        }
    }

    let mut terminal_commits = Vec::new();
    for variant in 0..variant_count {
        terminal_commits.push(write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 200 + variant),
            None,
            &format!("terminal selected by variant {variant}"),
        ));
    }

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); variant_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..variant_count {
            let child_variant = 1 - variant;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[child_index]);
            for alias_index in depth + 2..aliases.len() {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[alias_index], sibling_commits[depth][variant][alias_index]
                ));
            }
            let replacement = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", replacement_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} rebound branch {variant}"),
            );
            replacement_commits[depth][variant] = replacement;
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
    let root = resolver.resolve_for_parent(primary).unwrap();

    // Each branch starts from the same pinned root and chooses the opposite
    // child variant at every expansion, so either storm cannot rewrite the other.
    for branch_seed in 0..variant_count {
        let mut current = root.clone();
        let mut parent_variant = None;
        for depth in 0..depth_count {
            let selected_variant = (branch_seed + depth) % variant_count;
            let original_current = current.clone();
            let original_marker = if depth == 0 {
                10 + depth
            } else {
                replacement_marker(depth, 1 - parent_variant.unwrap())
            };
            let retained_markers: Vec<_> = (depth + 1..aliases.len())
                .map(|alias_index| {
                    if depth == 0 {
                        10 + alias_index
                    } else {
                        sibling_marker(depth - 1, parent_variant.unwrap(), alias_index)
                    }
                })
                .collect();

            for candidate in [selected_variant, 1 - selected_variant, selected_variant] {
                let replacement = PinnedDatabase::resolve(
                    aliases[depth],
                    shared_repository.clone(),
                    &replacement_commits[depth][candidate],
                    loader,
                )
                .unwrap();
                current.detach_database(aliases[depth]).unwrap();
                current.attach_database(replacement).unwrap();

                assert_module_route(
                    &current,
                    &format!("{}.orna", aliases[depth]),
                    &format!("= {}", replacement_marker(depth, candidate)),
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
                replacement_commits[depth][selected_variant]
            );
            let next = resolver.resolve_for_parent(selected).unwrap();
            assert_module_route(
                &next,
                "main.orna",
                &format!("= {}", replacement_marker(depth, selected_variant)),
            );

            let child_variant = 1 - selected_variant;
            let child_index = depth + 1;
            let expected_child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            assert_eq!(
                next.database(aliases[child_index])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                expected_child_commit
            );
            let child_marker = if child_index < depth_count {
                replacement_marker(child_index, child_variant)
            } else {
                200 + child_variant
            };
            assert_module_route(
                &next,
                &format!("{}.orna", aliases[child_index]),
                &format!("= {child_marker}"),
            );
            for alias_index in depth + 2..aliases.len() {
                assert_module_route(
                    &next,
                    &format!("{}.orna", aliases[alias_index]),
                    &format!("= {}", sibling_marker(depth, selected_variant, alias_index)),
                );
            }

            current = next;
            parent_variant = Some(selected_variant);
        }

        let terminal_variant = 1 - parent_variant.unwrap();
        assert_eq!(
            current
                .database(aliases[depth_count])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[terminal_variant]
        );
        assert_module_route(
            &current,
            &format!("{}.orna", aliases[depth_count]),
            &format!("= {}", 200 + terminal_variant),
        );
    }

    assert_module_route(&root, "archive.orna", "= 10");
    assert_module_route(&root, "archive_copy.orna", "= 11");
}

#[test]
fn storm_level_alias_rebinds_stay_independent_across_chained_closures() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let variant_count = 3;
    let replacement_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let sibling_marker = |depth: usize, variant: usize, alias_index: usize| {
        300 + depth * 20 + variant * 5 + alias_index
    };

    let mut original_commits = Vec::new();
    for (index, _) in aliases.iter().enumerate() {
        original_commits.push(write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 10 + index),
            None,
            &format!("initial storm-level alias {index}"),
        ));
    }

    let mut sibling_commits: Vec<Vec<Vec<String>>> = (0..depth_count)
        .map(|_| {
            (0..variant_count)
                .map(|_| vec![String::new(); aliases.len()])
                .collect()
        })
        .collect();
    for depth in 0..depth_count {
        for variant in 0..variant_count {
            for alias_index in depth + 2..aliases.len() {
                let marker = sibling_marker(depth, variant, alias_index);
                sibling_commits[depth][variant][alias_index] = write_package_snapshot(
                    shared_dir.path(),
                    package_source,
                    &format!("{marker}"),
                    None,
                    &format!("depth {depth} variant {variant} retained sibling {alias_index}"),
                );
            }
        }
    }

    let mut terminal_commits = Vec::new();
    for variant in 0..variant_count {
        terminal_commits.push(write_package_snapshot(
            shared_dir.path(),
            package_source,
            &format!("{}", 200 + variant),
            None,
            &format!("storm terminal variant {variant}"),
        ));
    }

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); variant_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..variant_count {
            let child_variant = (variant + 1) % variant_count;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[child_index]);
            for alias_index in depth + 2..aliases.len() {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[alias_index], sibling_commits[depth][variant][alias_index]
                ));
            }
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", replacement_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} storm-level replacement {variant}"),
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
    let root = resolver.resolve_for_parent(primary).unwrap();
    let mut forward_order = Vec::new();
    for variant in 0..variant_count {
        for depth in 0..depth_count {
            forward_order.push((depth, variant));
        }
    }
    let mut reverse_order = Vec::new();
    for variant in (0..variant_count).rev() {
        for depth in (0..depth_count).rev() {
            reverse_order.push((depth, variant));
        }
    }

    for (storm_index, operations) in [forward_order, reverse_order].into_iter().enumerate() {
        let final_variant = if storm_index == 0 {
            variant_count - 1
        } else {
            0
        };
        let mut storm = root.clone();
        let original_storm = storm.clone();
        let mut current_variants = vec![None; depth_count];
        for (depth, variant) in operations {
            let replacement = PinnedDatabase::resolve(
                aliases[depth],
                shared_repository.clone(),
                &replacement_commits[depth][variant],
                loader,
            )
            .unwrap();
            storm.detach_database(aliases[depth]).unwrap();
            storm.attach_database(replacement).unwrap();
            current_variants[depth] = Some(variant);

            for alias_index in 0..depth_count {
                let marker = current_variants[alias_index]
                    .map(|variant| replacement_marker(alias_index, variant))
                    .unwrap_or(10 + alias_index);
                assert_module_route(
                    &storm,
                    &format!("{}.orna", aliases[alias_index]),
                    &format!("= {marker}"),
                );
            }
            assert_module_route(&storm, "archive_copy_archive_archive.orna", "= 13");
        }
        for depth in 0..depth_count {
            assert_eq!(current_variants[depth], Some(final_variant));
            assert_module_route(
                &original_storm,
                &format!("{}.orna", aliases[depth]),
                &format!("= {}", 10 + depth),
            );
        }

        let selected_root = storm.database(aliases[0]).unwrap().clone();
        let root_variant = current_variants[0].unwrap();
        let mut first_closure = resolver.resolve_for_parent(selected_root).unwrap();
        let first_child_variant = (root_variant + 1) % variant_count;
        assert_module_route(
            &first_closure,
            "main.orna",
            &format!("= {}", replacement_marker(0, root_variant)),
        );
        assert_eq!(
            first_closure
                .database(aliases[1])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[1][first_child_variant]
        );
        for alias_index in 2..aliases.len() {
            assert_module_route(
                &first_closure,
                &format!("{}.orna", aliases[alias_index]),
                &format!("= {}", sibling_marker(0, root_variant, alias_index)),
            );
        }

        let before_middle_storm = first_closure.clone();
        let middle_variant = final_variant;
        for candidate in [
            middle_variant,
            (middle_variant + 1) % variant_count,
            middle_variant,
        ] {
            let replacement = PinnedDatabase::resolve(
                aliases[1],
                shared_repository.clone(),
                &replacement_commits[1][candidate],
                loader,
            )
            .unwrap();
            first_closure.detach_database(aliases[1]).unwrap();
            first_closure.attach_database(replacement).unwrap();
            assert_module_route(
                &first_closure,
                "archive_copy.orna",
                &format!("= {}", replacement_marker(1, candidate)),
            );
            for alias_index in 2..aliases.len() {
                assert_module_route(
                    &first_closure,
                    &format!("{}.orna", aliases[alias_index]),
                    &format!("= {}", sibling_marker(0, root_variant, alias_index)),
                );
            }
            assert_module_route(
                &before_middle_storm,
                "archive_copy.orna",
                &format!("= {}", replacement_marker(1, first_child_variant)),
            );
        }

        let selected_middle = first_closure.database(aliases[1]).unwrap().clone();
        let mut second_closure = resolver.resolve_for_parent(selected_middle).unwrap();
        let second_child_variant = (middle_variant + 1) % variant_count;
        assert_module_route(
            &second_closure,
            "main.orna",
            &format!("= {}", replacement_marker(1, middle_variant)),
        );
        assert_eq!(
            second_closure
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[2][second_child_variant]
        );
        assert_module_route(
            &second_closure,
            "archive_copy_archive_archive.orna",
            &format!("= {}", sibling_marker(1, middle_variant, 3)),
        );

        let before_deep_storm = second_closure.clone();
        let deep_variant = current_variants[2].unwrap();
        for candidate in [
            deep_variant,
            (deep_variant + 1) % variant_count,
            deep_variant,
        ] {
            let replacement = PinnedDatabase::resolve(
                aliases[2],
                shared_repository.clone(),
                &replacement_commits[2][candidate],
                loader,
            )
            .unwrap();
            second_closure.detach_database(aliases[2]).unwrap();
            second_closure.attach_database(replacement).unwrap();
            assert_module_route(
                &second_closure,
                "archive_copy_archive.orna",
                &format!("= {}", replacement_marker(2, candidate)),
            );
            assert_module_route(
                &second_closure,
                "archive_copy_archive_archive.orna",
                &format!("= {}", sibling_marker(1, middle_variant, 3)),
            );
            assert_module_route(
                &before_deep_storm,
                "archive_copy_archive.orna",
                &format!("= {}", replacement_marker(2, second_child_variant)),
            );
        }

        let selected_deep = second_closure.database(aliases[2]).unwrap().clone();
        let terminal = resolver.resolve_for_parent(selected_deep).unwrap();
        let terminal_variant = (deep_variant + 1) % variant_count;
        assert_eq!(
            terminal
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[terminal_variant]
        );
        assert_module_route(
            &terminal,
            "main.orna",
            &format!("= {}", replacement_marker(2, deep_variant)),
        );
        assert_module_route(
            &terminal,
            "archive_copy_archive_archive.orna",
            &format!("= {}", 200 + terminal_variant),
        );
    }

    assert_module_route(&root, "archive.orna", "= 10");
    assert_module_route(&root, "archive_copy.orna", "= 11");
}

#[test]
fn retained_storm_candidates_keep_nested_alias_precedence() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let variant_count = 2;
    let replacement_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let sibling_marker = |depth: usize, variant: usize, alias_index: usize| {
        300 + depth * 20 + variant * 5 + alias_index
    };

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("initial alias {index} before retained candidate storm"),
            )
        })
        .collect::<Vec<_>>();

    let mut sibling_commits: Vec<Vec<Vec<String>>> = (0..depth_count)
        .map(|_| {
            (0..variant_count)
                .map(|_| vec![String::new(); aliases.len()])
                .collect()
        })
        .collect();
    for depth in 0..depth_count {
        for variant in 0..variant_count {
            for alias_index in depth + 2..aliases.len() {
                sibling_commits[depth][variant][alias_index] = write_package_snapshot(
                    shared_dir.path(),
                    package_source,
                    &format!("{}", sibling_marker(depth, variant, alias_index)),
                    None,
                    &format!("depth {depth} variant {variant} sibling {alias_index}"),
                );
            }
        }
    }

    let terminal_commits = (0..variant_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 200 + variant),
                None,
                &format!("retained storm terminal {variant}"),
            )
        })
        .collect::<Vec<_>>();

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); variant_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..variant_count {
            let child_variant = 1 - variant;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[child_index]);
            for alias_index in depth + 2..aliases.len() {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[alias_index], sibling_commits[depth][variant][alias_index]
                ));
            }
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", replacement_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} retained storm candidate {variant}"),
            );
        }
    }

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
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
    let root = resolver.resolve_for_parent(primary).unwrap();

    // Retain both candidates, then finish the storm on the later one.
    let mut storm = root.clone();
    let mut captured_sessions: Vec<Option<AttachedDatabaseSession>> =
        (0..variant_count).map(|_| None).collect();
    let mut captured_pins: Vec<Option<PinnedDatabase>> =
        (0..variant_count).map(|_| None).collect();
    for candidate in [0, 1] {
        let replacement = PinnedDatabase::resolve(
            aliases[0],
            shared_repository.clone(),
            &replacement_commits[0][candidate],
            loader,
        )
        .unwrap();
        storm.detach_database(aliases[0]).unwrap();
        storm.attach_database(replacement).unwrap();
        captured_sessions[candidate] = Some(storm.clone());
        captured_pins[candidate] = Some(storm.database(aliases[0]).unwrap().clone());
    }
    assert_module_route(&storm, "archive.orna", "= 101");
    assert_module_route(&root, "archive.orna", "= 10");

    for root_variant in 0..variant_count {
        let snapshot = captured_sessions[root_variant].as_ref().unwrap();
        assert_module_route(
            snapshot,
            "archive.orna",
            &format!("= {}", replacement_marker(0, root_variant)),
        );
        assert_module_route(snapshot, "archive_copy.orna", "= 11");
        let initial_middle_variant = 1 - root_variant;
        let first_closure = resolver
            .resolve_for_parent(captured_pins[root_variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &first_closure,
            "main.orna",
            &format!("= {}", replacement_marker(0, root_variant)),
        );
        assert_eq!(
            first_closure
                .database(aliases[1])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[1][initial_middle_variant]
        );
        assert_module_route(
            &first_closure,
            "archive_copy_archive.orna",
            &format!("= {}", sibling_marker(0, root_variant, 2)),
        );
        assert_module_route(
            &first_closure,
            "archive_copy_archive_archive.orna",
            &format!("= {}", sibling_marker(0, root_variant, 3)),
        );

        // Capture an earlier and later middle pin; after the storm, expand one
        // according to the root candidate to prove both nested branches.
        let mut middle_storm = first_closure.clone();
        let middle_candidates = [initial_middle_variant, 1 - initial_middle_variant];
        let mut early_middle = None;
        let mut late_middle = None;
        for (position, candidate) in middle_candidates.into_iter().enumerate() {
            let replacement = PinnedDatabase::resolve(
                aliases[1],
                shared_repository.clone(),
                &replacement_commits[1][candidate],
                loader,
            )
            .unwrap();
            middle_storm.detach_database(aliases[1]).unwrap();
            middle_storm.attach_database(replacement).unwrap();
            assert_module_route(
                &middle_storm,
                "archive_copy.orna",
                &format!("= {}", replacement_marker(1, candidate)),
            );
            assert_module_route(
                &middle_storm,
                "archive_copy_archive.orna",
                &format!("= {}", sibling_marker(0, root_variant, 2)),
            );
            assert_module_route(
                &first_closure,
                "archive_copy.orna",
                &format!("= {}", replacement_marker(1, initial_middle_variant)),
            );
            if position == 0 {
                early_middle = Some(middle_storm.database(aliases[1]).unwrap().clone());
            } else {
                late_middle = Some(middle_storm.database(aliases[1]).unwrap().clone());
            }
        }
        let (middle_variant, middle_pin) = if root_variant == 0 {
            (middle_candidates[0], early_middle.unwrap())
        } else {
            (middle_candidates[1], late_middle.unwrap())
        };
        let initial_deep_variant = 1 - middle_variant;
        let second_closure = resolver.resolve_for_parent(middle_pin).unwrap();
        assert_module_route(
            &second_closure,
            "main.orna",
            &format!("= {}", replacement_marker(1, middle_variant)),
        );
        assert_eq!(
            second_closure
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[2][initial_deep_variant]
        );
        assert_module_route(
            &second_closure,
            "archive_copy_archive_archive.orna",
            &format!("= {}", sibling_marker(1, middle_variant, 3)),
        );

        // Rebind the deep alias and later expand the early pin on one branch
        // and the latest pin on the other.
        let mut deep_storm = second_closure.clone();
        let deep_candidates = [initial_deep_variant, 1 - initial_deep_variant];
        let mut early_deep = None;
        let mut late_deep = None;
        for (position, candidate) in deep_candidates.into_iter().enumerate() {
            let replacement = PinnedDatabase::resolve(
                aliases[2],
                shared_repository.clone(),
                &replacement_commits[2][candidate],
                loader,
            )
            .unwrap();
            deep_storm.detach_database(aliases[2]).unwrap();
            deep_storm.attach_database(replacement).unwrap();
            assert_module_route(
                &deep_storm,
                "archive_copy_archive.orna",
                &format!("= {}", replacement_marker(2, candidate)),
            );
            assert_module_route(
                &deep_storm,
                "archive_copy_archive_archive.orna",
                &format!("= {}", sibling_marker(1, middle_variant, 3)),
            );
            assert_module_route(
                &second_closure,
                "archive_copy_archive.orna",
                &format!("= {}", replacement_marker(2, initial_deep_variant)),
            );
            if position == 0 {
                early_deep = Some(deep_storm.database(aliases[2]).unwrap().clone());
            } else {
                late_deep = Some(deep_storm.database(aliases[2]).unwrap().clone());
            }
        }
        let (deep_variant, deep_pin) = if root_variant == 0 {
            (deep_candidates[0], early_deep.unwrap())
        } else {
            (deep_candidates[1], late_deep.unwrap())
        };
        let terminal_variant = 1 - deep_variant;
        let terminal = resolver.resolve_for_parent(deep_pin).unwrap();
        assert_module_route(
            &terminal,
            "main.orna",
            &format!("= {}", replacement_marker(2, deep_variant)),
        );
        assert_eq!(
            terminal
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[terminal_variant]
        );
        assert_module_route(
            &terminal,
            "archive_copy_archive_archive.orna",
            &format!("= {}", 200 + terminal_variant),
        );
    }

    assert_module_route(&root, "archive.orna", "= 10");
    assert_module_route(&root, "archive_copy.orna", "= 11");
}

#[test]
fn repeated_alias_storm_runs_choose_their_own_chained_closure() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = ["archive", "archive_copy", "archive_copy_archive"];
    let variant_count = 2;
    let replacement_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let sibling_marker = |variant: usize| 300 + variant * 5 + aliases.len() - 1;

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("initial alias {index} before repeated storm runs"),
            )
        })
        .collect::<Vec<_>>();
    let sibling_commits = (0..variant_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", sibling_marker(variant)),
                None,
                &format!("root variant {variant} longer prefix sibling"),
            )
        })
        .collect::<Vec<_>>();
    let terminal_commits = (0..variant_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 200 + variant),
                None,
                &format!("run-selected terminal {variant}"),
            )
        })
        .collect::<Vec<_>>();

    let mut replacement_commits: Vec<Vec<String>> = (0..aliases.len() - 1)
        .map(|_| vec![String::new(); variant_count])
        .collect();
    for depth in (0..aliases.len() - 1).rev() {
        for variant in 0..variant_count {
            let child_variant = 1 - variant;
            let child_commit = if depth + 1 < aliases.len() - 1 {
                &replacement_commits[depth + 1][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[depth + 1]);
            if depth == 0 {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[2], sibling_commits[variant]
                ));
            }
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", replacement_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} replacement run candidate {variant}"),
            );
        }
    }
    let replacement_pins: Vec<Vec<PinnedDatabase>> = (0..aliases.len() - 1)
        .map(|depth| {
            (0..variant_count)
                .map(|variant| {
                    PinnedDatabase::resolve(
                        aliases[depth],
                        shared_repository.clone(),
                        &replacement_commits[depth][variant],
                        ProjectLoader::default(),
                    )
                    .unwrap()
                })
                .collect()
        })
        .collect();

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
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
    let root = resolver.resolve_for_parent(primary).unwrap();

    // Apply two complete storms to the same session. Their opposite final
    // candidates must each retain their own exact closure after the next run.
    let runs = [[0, 1, 0, 1], [1, 0, 1, 0]];
    let expected_root_variants = [1, 0];
    let mut root_snapshots = Vec::new();
    let mut root_pins = Vec::new();
    let mut storm = root.clone();
    for (run_index, run) in runs.into_iter().enumerate() {
        for candidate in run {
            storm.detach_database(aliases[0]).unwrap();
            storm
                .attach_database(replacement_pins[0][candidate].clone())
                .unwrap();
            assert_eq!(
                storm
                    .database(aliases[0])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                replacement_commits[0][candidate]
            );
            assert_module_route(
                &storm,
                "archive.orna",
                &format!("= {}", replacement_marker(0, candidate)),
            );
            assert_module_route(&storm, "archive_copy.orna", "= 11");
            assert_module_route(&storm, "archive_copy_archive.orna", "= 12");
        }
        let root_variant = expected_root_variants[run_index];
        root_snapshots.push(storm.clone());
        root_pins.push(storm.database(aliases[0]).unwrap().clone());
        assert_eq!(
            root_pins[run_index].pin().commit().as_str(),
            replacement_commits[0][root_variant]
        );
    }

    let mut middle_snapshots = Vec::new();
    let mut middle_pins = Vec::new();
    let mut middle_variants = Vec::new();
    for (run_index, root_variant) in expected_root_variants.into_iter().enumerate() {
        let root_snapshot = &root_snapshots[run_index];
        assert_module_route(
            root_snapshot,
            "archive.orna",
            &format!("= {}", replacement_marker(0, root_variant)),
        );
        let initial_middle_variant = 1 - root_variant;
        let mut closure = resolver
            .resolve_for_parent(root_pins[run_index].clone())
            .unwrap();
        assert_module_route(
            &closure,
            "main.orna",
            &format!("= {}", replacement_marker(0, root_variant)),
        );
        assert_eq!(
            closure
                .database(aliases[1])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[1][initial_middle_variant]
        );
        assert_eq!(
            closure
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            sibling_commits[root_variant]
        );

        let before_middle_run = closure.clone();
        let middle_run = [
            initial_middle_variant,
            1 - initial_middle_variant,
            initial_middle_variant,
            1 - initial_middle_variant,
        ];
        for candidate in middle_run {
            closure.detach_database(aliases[1]).unwrap();
            closure
                .attach_database(replacement_pins[1][candidate].clone())
                .unwrap();
            assert_eq!(
                closure
                    .database(aliases[1])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                replacement_commits[1][candidate]
            );
            assert_module_route(
                &closure,
                "archive_copy.orna",
                &format!("= {}", replacement_marker(1, candidate)),
            );
            assert_module_route(
                &closure,
                "archive_copy_archive.orna",
                &format!("= {}", sibling_marker(root_variant)),
            );
        }
        assert_module_route(
            &before_middle_run,
            "archive_copy.orna",
            &format!("= {}", replacement_marker(1, initial_middle_variant)),
        );
        let selected_middle_variant = 1 - initial_middle_variant;
        middle_variants.push(selected_middle_variant);
        middle_snapshots.push(closure.clone());
        middle_pins.push(closure.database(aliases[1]).unwrap().clone());
    }

    // The second root run and each nested run leave their prior snapshots
    // pinned, while each final middle pin chooses its own terminal leaf.
    assert_module_route(&root_snapshots[0], "archive.orna", "= 101");
    assert_module_route(&root_snapshots[1], "archive.orna", "= 100");
    for run_index in 0..runs.len() {
        let middle_variant = middle_variants[run_index];
        assert_module_route(
            &middle_snapshots[run_index],
            "archive_copy.orna",
            &format!("= {}", replacement_marker(1, middle_variant)),
        );
        let terminal_variant = 1 - middle_variant;
        let terminal = resolver
            .resolve_for_parent(middle_pins[run_index].clone())
            .unwrap();
        assert_module_route(
            &terminal,
            "main.orna",
            &format!("= {}", replacement_marker(1, middle_variant)),
        );
        assert_eq!(
            terminal
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[terminal_variant]
        );
        assert_module_route(
            &terminal,
            "archive_copy_archive.orna",
            &format!("= {}", 200 + terminal_variant),
        );
    }
    assert_module_route(&root, "archive.orna", "= 10");
}

#[test]
fn repeated_closure_rebinds_select_each_storm_chain_edge() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let variant_count = 2;
    let replacement_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let sibling_marker = |depth: usize, alias_index: usize| 300 + depth * 10 + alias_index;

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("initial chain alias {index}"),
            )
        })
        .collect::<Vec<_>>();
    let mut sibling_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); aliases.len()])
        .collect();
    for depth in 0..depth_count {
        for alias_index in depth + 2..aliases.len() {
            sibling_commits[depth][alias_index] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", sibling_marker(depth, alias_index)),
                None,
                &format!("depth {depth} retained sibling {alias_index}"),
            );
        }
    }
    let terminal_commits = (0..variant_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 200 + variant),
                None,
                &format!("storm chain terminal {variant}"),
            )
        })
        .collect::<Vec<_>>();

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); variant_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..variant_count {
            let child_variant = 1 - variant;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[child_index]);
            for alias_index in depth + 2..aliases.len() {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[alias_index], sibling_commits[depth][alias_index]
                ));
            }
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", replacement_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} storm chain candidate {variant}"),
            );
        }
    }
    let loader = ProjectLoader::default();
    let replacement_pins: Vec<Vec<PinnedDatabase>> = (0..depth_count)
        .map(|depth| {
            (0..variant_count)
                .map(|variant| {
                    PinnedDatabase::resolve(
                        aliases[depth],
                        shared_repository.clone(),
                        &replacement_commits[depth][variant],
                        loader,
                    )
                    .unwrap()
                })
                .collect()
        })
        .collect();

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/attach-primary.orna"),
        ),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let root = resolver.resolve_for_parent(primary).unwrap();

    let chain_finals = [[0, 1, 0], [1, 0, 1]];
    let mut pre_storm_snapshots = Vec::new();
    let mut terminals = Vec::new();
    let mut terminal_choices = Vec::new();
    for (chain_index, finals) in chain_finals.into_iter().enumerate() {
        let mut session = root.clone();
        let mut parent_variant = None;
        for depth in 0..depth_count {
            let final_variant = finals[depth];
            let initial_variant = if depth == 0 {
                1 - final_variant
            } else {
                1 - parent_variant.unwrap()
            };
            let prior_marker = if depth == 0 {
                10 + depth
            } else {
                replacement_marker(depth, initial_variant)
            };
            let before_storm = session.clone();
            pre_storm_snapshots.push((depth, prior_marker, before_storm.clone()));

            let retained_siblings = depth + 1..aliases.len();
            for alias_index in retained_siblings.clone() {
                let expected_commit = if depth == 0 {
                    &original_commits[alias_index]
                } else {
                    &sibling_commits[depth - 1][alias_index]
                };
                assert_eq!(
                    session
                        .database(aliases[alias_index])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    expected_commit
                );
            }

            let candidates = [
                initial_variant,
                1 - initial_variant,
                initial_variant,
                final_variant,
            ];
            for candidate in candidates {
                session.detach_database(aliases[depth]).unwrap();
                session
                    .attach_database(replacement_pins[depth][candidate].clone())
                    .unwrap();
                assert_eq!(
                    session
                        .database(aliases[depth])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    replacement_commits[depth][candidate]
                );
                assert_module_route(
                    &session,
                    &format!("{}.orna", aliases[depth]),
                    &format!("= {}", replacement_marker(depth, candidate)),
                );
                for alias_index in retained_siblings.clone() {
                    let (expected_commit, expected_marker) = if depth == 0 {
                        (&original_commits[alias_index], 10 + alias_index)
                    } else {
                        (
                            &sibling_commits[depth - 1][alias_index],
                            sibling_marker(depth - 1, alias_index),
                        )
                    };
                    assert_eq!(
                        session
                            .database(aliases[alias_index])
                            .unwrap()
                            .pin()
                            .commit()
                            .as_str(),
                        expected_commit
                    );
                    assert_module_route(
                        &session,
                        &format!("{}.orna", aliases[alias_index]),
                        &format!("= {expected_marker}"),
                    );
                }
            }
            assert_module_route(
                &before_storm,
                &format!("{}.orna", aliases[depth]),
                &format!("= {prior_marker}"),
            );

            let selected_pin = session.database(aliases[depth]).unwrap().clone();
            if depth + 1 == depth_count {
                let terminal_variant = 1 - final_variant;
                let terminal = resolver.resolve_for_parent(selected_pin).unwrap();
                assert_module_route(
                    &terminal,
                    "main.orna",
                    &format!("= {}", replacement_marker(depth, final_variant)),
                );
                assert_eq!(
                    terminal
                        .database(aliases[depth + 1])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    terminal_commits[terminal_variant]
                );
                assert_module_route(
                    &terminal,
                    &format!("{}.orna", aliases[depth + 1]),
                    &format!("= {}", 200 + terminal_variant),
                );
                terminals.push(terminal);
                terminal_choices.push((final_variant, terminal_variant));
            } else {
                let next = resolver.resolve_for_parent(selected_pin).unwrap();
                let child_variant = 1 - final_variant;
                assert_module_route(
                    &next,
                    "main.orna",
                    &format!("= {}", replacement_marker(depth, final_variant)),
                );
                assert_eq!(
                    next.database(aliases[depth + 1])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    replacement_commits[depth + 1][child_variant]
                );
                for alias_index in depth + 2..aliases.len() {
                    assert_eq!(
                        next.database(aliases[alias_index])
                            .unwrap()
                            .pin()
                            .commit()
                            .as_str(),
                        sibling_commits[depth][alias_index]
                    );
                }
                session = next;
            }
            parent_variant = Some(final_variant);
        }
        assert_eq!(terminals.len(), chain_index + 1);
    }

    for (depth, marker, snapshot) in pre_storm_snapshots {
        assert_module_route(
            &snapshot,
            &format!("{}.orna", aliases[depth]),
            &format!("= {marker}"),
        );
    }
    for (chain_index, (deep_variant, terminal_variant)) in
        terminal_choices.into_iter().enumerate()
    {
        assert_module_route(
            &terminals[chain_index],
            "main.orna",
            &format!("= {}", replacement_marker(2, deep_variant)),
        );
        assert_module_route(
            &terminals[chain_index],
            "archive_copy_archive_archive.orna",
            &format!("= {}", 200 + terminal_variant),
        );
    }
    assert_module_route(&root, "archive.orna", "= 10");
}

#[test]
fn retained_closure_branches_keep_precedence_across_repeated_storm_chains() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let candidate_count = 2;
    let loader = ProjectLoader::default();
    let root_marker = |root: usize| 100 + root;
    let middle_marker = |root: usize, middle: usize| 200 + root * 10 + middle;
    let middle_sibling_marker = |root: usize, middle: usize| 300 + root * 10 + middle;
    let terminal_marker = |root: usize, middle: usize| 400 + root * 10 + middle;
    let outer_sibling_marker = |root: usize, sibling: usize| 500 + root * 10 + sibling;

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("original storm-chain alias {index}"),
            )
        })
        .collect::<Vec<_>>();

    let terminal_commits: Vec<Vec<String>> = (0..candidate_count)
        .map(|root| {
            (0..candidate_count)
                .map(|middle| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", terminal_marker(root, middle)),
                        None,
                        &format!("root {root} middle {middle} terminal"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_sibling_commits: Vec<Vec<String>> = (0..candidate_count)
        .map(|root| {
            (0..candidate_count)
                .map(|middle| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", middle_sibling_marker(root, middle)),
                        None,
                        &format!("root {root} middle {middle} retained sibling"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..candidate_count)
        .map(|root| {
            (0..candidate_count)
                .map(|middle| {
                    let manifest = format!(
                        "{} {}\n{} {}\n",
                        aliases[2],
                        terminal_commits[root][middle],
                        aliases[3],
                        middle_sibling_commits[root][middle],
                    );
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", middle_marker(root, middle)),
                        Some(&manifest),
                        &format!("root {root} middle candidate {middle}"),
                    )
                })
                .collect()
        })
        .collect();
    let outer_sibling_commits: Vec<Vec<String>> = (0..candidate_count)
        .map(|root| {
            (0..aliases.len() - 1)
                .map(|sibling| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", outer_sibling_marker(root, sibling)),
                        None,
                        &format!("root {root} outer sibling {sibling}"),
                    )
                })
                .collect()
        })
        .collect();
    let root_commits: Vec<String> = (0..candidate_count)
        .map(|root| {
            let manifest = format!(
                "{} {}\n{} {}\n{} {}\n",
                aliases[1],
                middle_commits[root][0],
                aliases[2],
                outer_sibling_commits[root][0],
                aliases[3],
                outer_sibling_commits[root][1],
            );
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", root_marker(root)),
                Some(&manifest),
                &format!("outer storm-chain candidate {root}"),
            )
        })
        .collect();
    let root_pins: Vec<PinnedDatabase> = root_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[0], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let middle_pins: Vec<Vec<PinnedDatabase>> = (0..candidate_count)
        .map(|root| {
            (0..candidate_count)
                .map(|middle| {
                    PinnedDatabase::resolve(
                        aliases[1],
                        shared_repository.clone(),
                        &middle_commits[root][middle],
                        loader,
                    )
                    .unwrap()
                })
                .collect()
        })
        .collect();

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let mut root_session = resolver.resolve_for_parent(primary).unwrap();
    let original_root_session = root_session.clone();
    let mut retained_root_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();

    for root in [0, 1, 0, 1] {
        root_session.detach_database(aliases[0]).unwrap();
        root_session
            .attach_database(root_pins[root].clone())
            .unwrap();
        retained_root_pins[root]
            .get_or_insert_with(|| root_session.database(aliases[0]).unwrap().clone());

        assert_module_route(
            &root_session,
            "archive.orna",
            &format!("= {}", root_marker(root)),
        );
        for (index, alias) in aliases[1..].iter().enumerate() {
            assert_eq!(
                root_session
                    .database(alias)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                original_commits[index + 1]
            );
            assert_module_route(
                &root_session,
                &format!("{alias}.orna"),
                &format!("= {}", 11 + index),
            );
        }
    }
    assert_module_route(&original_root_session, "archive.orna", "= 10");
    assert_module_route(&root_session, "archive.orna", "= 101");

    for root in 0..candidate_count {
        let outer = resolver
            .resolve_for_parent(retained_root_pins[root].take().unwrap())
            .unwrap();
        assert_module_route(&outer, "main.orna", &format!("= {}", root_marker(root)));
        assert_eq!(
            outer.database(aliases[1]).unwrap().pin().commit().as_str(),
            middle_commits[root][0]
        );
        assert_module_route(
            &outer,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", middle_marker(root, 0)),
        );
        for sibling in 0..aliases.len() - 2 {
            assert_eq!(
                outer
                    .database(aliases[sibling + 2])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                outer_sibling_commits[root][sibling]
            );
            assert_module_route(
                &outer,
                &format!("{}.orna", aliases[sibling + 2]),
                &format!("= {}", outer_sibling_marker(root, sibling)),
            );
        }

        let original_outer = outer.clone();
        let mut nested = outer;
        let mut retained_middle_pins: Vec<Option<PinnedDatabase>> =
            (0..candidate_count).map(|_| None).collect();
        for middle in [1, 0, 1, 0] {
            nested.detach_database(aliases[1]).unwrap();
            nested
                .attach_database(middle_pins[root][middle].clone())
                .unwrap();
            retained_middle_pins[middle]
                .get_or_insert_with(|| nested.database(aliases[1]).unwrap().clone());

            assert_module_route(
                &nested,
                &format!("{}.orna", aliases[1]),
                &format!("= {}", middle_marker(root, middle)),
            );
            for sibling in 0..aliases.len() - 2 {
                assert_eq!(
                    nested
                        .database(aliases[sibling + 2])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    outer_sibling_commits[root][sibling]
                );
                assert_module_route(
                    &nested,
                    &format!("{}.orna", aliases[sibling + 2]),
                    &format!("= {}", outer_sibling_marker(root, sibling)),
                );
            }
        }
        assert_eq!(
            original_outer
                .database(aliases[1])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            middle_commits[root][0]
        );
        assert_module_route(
            &original_outer,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", middle_marker(root, 0)),
        );

        for middle in 0..candidate_count {
            let descendant = resolver
                .resolve_for_parent(retained_middle_pins[middle].take().unwrap())
                .unwrap();
            assert_module_route(
                &descendant,
                "main.orna",
                &format!("= {}", middle_marker(root, middle)),
            );
            for (alias_index, commit, marker) in [
                (
                    2,
                    &terminal_commits[root][middle],
                    terminal_marker(root, middle),
                ),
                (
                    3,
                    &middle_sibling_commits[root][middle],
                    middle_sibling_marker(root, middle),
                ),
            ] {
                assert_eq!(
                    descendant
                        .database(aliases[alias_index])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    *commit
                );
                assert_module_route(
                    &descendant,
                    &format!("{}.orna", aliases[alias_index]),
                    &format!("= {marker}"),
                );
            }
        }
    }
}

#[test]
fn later_ancestor_storm_keeps_chained_closure_rebinds_on_their_own_pins() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let candidate_count = 2;
    let candidate_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let sibling_marker = |depth: usize, variant: usize, alias_index: usize| {
        300 + depth * 20 + variant * 5 + alias_index
    };
    let terminal_marker = |variant: usize| 200 + variant;

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("original chained alias {index}"),
            )
        })
        .collect::<Vec<_>>();
    let terminal_commits: Vec<String> = (0..candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", terminal_marker(variant)),
                None,
                &format!("terminal candidate {variant}"),
            )
        })
        .collect();

    let mut sibling_commits: Vec<Vec<Vec<String>>> = (0..depth_count)
        .map(|_| {
            (0..candidate_count)
                .map(|_| vec![String::new(); aliases.len()])
                .collect()
        })
        .collect();
    for depth in 0..depth_count {
        for variant in 0..candidate_count {
            for alias_index in depth + 2..aliases.len() {
                sibling_commits[depth][variant][alias_index] = write_package_snapshot(
                    shared_dir.path(),
                    package_source,
                    &format!("{}", sibling_marker(depth, variant, alias_index)),
                    None,
                    &format!("depth {depth} variant {variant} sibling {alias_index}"),
                );
            }
        }
    }

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); candidate_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..candidate_count {
            let child_variant = 1 - variant;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let mut manifest = format!("{} {child_commit}\n", aliases[child_index]);
            for alias_index in depth + 2..aliases.len() {
                manifest.push_str(&format!(
                    "{} {}\n",
                    aliases[alias_index], sibling_commits[depth][variant][alias_index]
                ));
            }
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", candidate_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} chained candidate {variant}"),
            );
        }
    }

    let loader = ProjectLoader::default();
    let replacement_pins: Vec<Vec<PinnedDatabase>> = (0..depth_count)
        .map(|depth| {
            (0..candidate_count)
                .map(|variant| {
                    PinnedDatabase::resolve(
                        aliases[depth],
                        shared_repository.clone(),
                        &replacement_commits[depth][variant],
                        loader,
                    )
                    .unwrap()
                })
                .collect()
        })
        .collect();
    let assert_candidate_siblings =
        |session: &AttachedDatabaseSession, depth: usize, variant: usize| {
            for alias_index in depth + 2..aliases.len() {
                assert_eq!(
                    session
                        .database(aliases[alias_index])
                        .unwrap()
                        .pin()
                        .commit()
                        .as_str(),
                    sibling_commits[depth][variant][alias_index]
                );
                assert_module_route(
                    session,
                    &format!("{}.orna", aliases[alias_index]),
                    &format!("= {}", sibling_marker(depth, variant, alias_index)),
                );
            }
        };

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let mut root_session = resolver.resolve_for_parent(primary).unwrap();
    let original_root = root_session.clone();
    let mut retained_root_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();

    for variant in [0, 1, 0] {
        root_session.detach_database(aliases[0]).unwrap();
        root_session
            .attach_database(replacement_pins[0][variant].clone())
            .unwrap();
        retained_root_pins[variant]
            .get_or_insert_with(|| root_session.database(aliases[0]).unwrap().clone());
        assert_module_route(
            &root_session,
            "archive.orna",
            &format!("= {}", candidate_marker(0, variant)),
        );
        for (index, alias) in aliases[1..].iter().enumerate() {
            assert_eq!(
                root_session
                    .database(alias)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                original_commits[index + 1]
            );
        }
    }
    assert_module_route(&original_root, "archive.orna", "= 10");

    let mut old_outer = resolver
        .resolve_for_parent(retained_root_pins[0].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &old_outer,
        "main.orna",
        &format!("= {}", candidate_marker(0, 0)),
    );
    assert_eq!(
        old_outer
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][1]
    );
    assert_candidate_siblings(&old_outer, 0, 0);
    let old_outer_snapshot = old_outer.clone();
    let mut retained_middle_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [0, 1, 0] {
        old_outer.detach_database(aliases[1]).unwrap();
        old_outer
            .attach_database(replacement_pins[1][variant].clone())
            .unwrap();
        retained_middle_pins[variant]
            .get_or_insert_with(|| old_outer.database(aliases[1]).unwrap().clone());
        assert_module_route(
            &old_outer,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", candidate_marker(1, variant)),
        );
        for alias_index in 2..aliases.len() {
            assert_eq!(
                old_outer
                    .database(aliases[alias_index])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                sibling_commits[0][0][alias_index]
            );
        }
    }
    assert_eq!(
        old_outer_snapshot
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][1]
    );

    let mut middle_closure = resolver
        .resolve_for_parent(retained_middle_pins[0].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &middle_closure,
        "main.orna",
        &format!("= {}", candidate_marker(1, 0)),
    );
    assert_eq!(
        middle_closure
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[2][1]
    );
    assert_candidate_siblings(&middle_closure, 1, 0);
    let middle_snapshot = middle_closure.clone();
    let mut retained_deep_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [0, 1, 0] {
        middle_closure.detach_database(aliases[2]).unwrap();
        middle_closure
            .attach_database(replacement_pins[2][variant].clone())
            .unwrap();
        retained_deep_pins[variant]
            .get_or_insert_with(|| middle_closure.database(aliases[2]).unwrap().clone());
        assert_module_route(
            &middle_closure,
            &format!("{}.orna", aliases[2]),
            &format!("= {}", candidate_marker(2, variant)),
        );
        assert_eq!(
            middle_closure
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            sibling_commits[1][0][3]
        );
    }
    assert_eq!(
        middle_snapshot
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[2][1]
    );
    let terminal_closure = resolver
        .resolve_for_parent(retained_deep_pins[0].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &terminal_closure,
        "main.orna",
        &format!("= {}", candidate_marker(2, 0)),
    );
    assert_eq!(
        terminal_closure
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        terminal_commits[1]
    );
    assert_module_route(
        &terminal_closure,
        &format!("{}.orna", aliases[3]),
        &format!("= {}", terminal_marker(1)),
    );

    for variant in [1, 0, 1] {
        root_session.detach_database(aliases[0]).unwrap();
        root_session
            .attach_database(replacement_pins[0][variant].clone())
            .unwrap();
        assert_module_route(
            &root_session,
            "archive.orna",
            &format!("= {}", candidate_marker(0, variant)),
        );
        for (index, alias) in aliases[1..].iter().enumerate() {
            assert_eq!(
                root_session
                    .database(alias)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                original_commits[index + 1]
            );
        }
    }
    assert_module_route(&root_session, "archive.orna", "= 101");
    assert_module_route(&original_root, "archive.orna", "= 10");

    let late_outer = resolver
        .resolve_for_parent(retained_root_pins[0].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &late_outer,
        "main.orna",
        &format!("= {}", candidate_marker(0, 0)),
    );
    assert_eq!(
        late_outer
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][1]
    );
    assert_candidate_siblings(&late_outer, 0, 0);

    let mut latest_outer = resolver
        .resolve_for_parent(retained_root_pins[1].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &latest_outer,
        "main.orna",
        &format!("= {}", candidate_marker(0, 1)),
    );
    assert_eq!(
        latest_outer
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][0]
    );
    assert_candidate_siblings(&latest_outer, 0, 1);
    let latest_outer_snapshot = latest_outer.clone();
    let mut newest_middle_pin = None;
    for variant in [1, 0, 1] {
        latest_outer.detach_database(aliases[1]).unwrap();
        latest_outer
            .attach_database(replacement_pins[1][variant].clone())
            .unwrap();
        newest_middle_pin = Some(latest_outer.database(aliases[1]).unwrap().clone());
        assert_module_route(
            &latest_outer,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", candidate_marker(1, variant)),
        );
        assert_candidate_siblings(&latest_outer, 0, 1);
    }
    assert_eq!(
        latest_outer_snapshot
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][0]
    );
    let newest_middle = resolver
        .resolve_for_parent(newest_middle_pin.unwrap())
        .unwrap();
    assert_module_route(
        &newest_middle,
        "main.orna",
        &format!("= {}", candidate_marker(1, 1)),
    );
    assert_eq!(
        newest_middle
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[2][0]
    );

    for variant in 0..candidate_count {
        let retained_middle = resolver
            .resolve_for_parent(retained_middle_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &retained_middle,
            "main.orna",
            &format!("= {}", candidate_marker(1, variant)),
        );
        assert_eq!(
            retained_middle
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[2][1 - variant]
        );
        assert_candidate_siblings(&retained_middle, 1, variant);

        let retained_deep = resolver
            .resolve_for_parent(retained_deep_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &retained_deep,
            "main.orna",
            &format!("= {}", candidate_marker(2, variant)),
        );
        assert_eq!(
            retained_deep
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[1 - variant]
        );
        assert_module_route(
            &retained_deep,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", terminal_marker(1 - variant)),
        );
    }
}

#[test]
fn repeated_storm_cascade_selects_each_terminal_alias_from_its_chain() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let candidate_count = 2;
    let candidate_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let terminal_marker = |variant: usize| 200 + variant;

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("original cascade alias {index}"),
            )
        })
        .collect::<Vec<_>>();
    let terminal_commits: Vec<String> = (0..candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", terminal_marker(variant)),
                None,
                &format!("storm cascade terminal {variant}"),
            )
        })
        .collect();

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); candidate_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..candidate_count {
            let child_variant = 1 - variant;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let manifest = format!("{} {child_commit}\n", aliases[child_index]);
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", candidate_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} cascade candidate {variant}"),
            );
        }
    }

    let loader = ProjectLoader::default();
    let replacement_pins: Vec<Vec<PinnedDatabase>> = (0..depth_count)
        .map(|depth| {
            (0..candidate_count)
                .map(|variant| {
                    PinnedDatabase::resolve(
                        aliases[depth],
                        shared_repository.clone(),
                        &replacement_commits[depth][variant],
                        loader,
                    )
                    .unwrap()
                })
                .collect()
        })
        .collect();
    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let mut root_session = resolver.resolve_for_parent(primary).unwrap();
    let original_root = root_session.clone();
    let mut retained_root_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();

    for variant in [0, 1, 0] {
        root_session.detach_database(aliases[0]).unwrap();
        root_session
            .attach_database(replacement_pins[0][variant].clone())
            .unwrap();
        retained_root_pins[variant]
            .get_or_insert_with(|| root_session.database(aliases[0]).unwrap().clone());
        assert_module_route(
            &root_session,
            "archive.orna",
            &format!("= {}", candidate_marker(0, variant)),
        );
        for (index, alias) in aliases[1..].iter().enumerate() {
            assert_eq!(
                root_session
                    .database(alias)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                original_commits[index + 1]
            );
        }
    }

    let mut old_outer = resolver
        .resolve_for_parent(retained_root_pins[0].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &old_outer,
        "main.orna",
        &format!("= {}", candidate_marker(0, 0)),
    );
    assert_eq!(
        old_outer
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][1]
    );
    let mut retained_middle_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [0, 1, 0] {
        old_outer.detach_database(aliases[1]).unwrap();
        old_outer
            .attach_database(replacement_pins[1][variant].clone())
            .unwrap();
        retained_middle_pins[variant]
            .get_or_insert_with(|| old_outer.database(aliases[1]).unwrap().clone());
        assert_module_route(
            &old_outer,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", candidate_marker(1, variant)),
        );
    }
    let mut old_middle = resolver
        .resolve_for_parent(retained_middle_pins[0].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &old_middle,
        "main.orna",
        &format!("= {}", candidate_marker(1, 0)),
    );
    assert_eq!(
        old_middle
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[2][1]
    );
    let mut retained_old_deep_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [1, 0, 1] {
        old_middle.detach_database(aliases[2]).unwrap();
        old_middle
            .attach_database(replacement_pins[2][variant].clone())
            .unwrap();
        retained_old_deep_pins[variant]
            .get_or_insert_with(|| old_middle.database(aliases[2]).unwrap().clone());
    }
    let old_terminal = resolver
        .resolve_for_parent(retained_old_deep_pins[1].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &old_terminal,
        "main.orna",
        &format!("= {}", candidate_marker(2, 1)),
    );
    assert_eq!(
        old_terminal
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        terminal_commits[0]
    );
    assert_module_route(
        &old_terminal,
        &format!("{}.orna", aliases[3]),
        &format!("= {}", terminal_marker(0)),
    );

    for variant in [1, 0, 1] {
        root_session.detach_database(aliases[0]).unwrap();
        root_session
            .attach_database(replacement_pins[0][variant].clone())
            .unwrap();
        assert_module_route(
            &root_session,
            "archive.orna",
            &format!("= {}", candidate_marker(0, variant)),
        );
        for (index, alias) in aliases[1..].iter().enumerate() {
            assert_eq!(
                root_session
                    .database(alias)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                original_commits[index + 1]
            );
        }
    }
    assert_module_route(&root_session, "archive.orna", "= 101");
    assert_module_route(&original_root, "archive.orna", "= 10");

    let mut latest_outer = resolver
        .resolve_for_parent(retained_root_pins[1].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(
        &latest_outer,
        "main.orna",
        &format!("= {}", candidate_marker(0, 1)),
    );
    assert_eq!(
        latest_outer
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][0]
    );
    let latest_outer_snapshot = latest_outer.clone();
    let mut latest_middle_pin = None;
    for variant in [1, 0, 1] {
        latest_outer.detach_database(aliases[1]).unwrap();
        latest_outer
            .attach_database(replacement_pins[1][variant].clone())
            .unwrap();
        latest_middle_pin = Some(latest_outer.database(aliases[1]).unwrap().clone());
        assert_module_route(
            &latest_outer,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", candidate_marker(1, variant)),
        );
    }
    assert_eq!(
        latest_outer_snapshot
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[1][0]
    );
    let mut latest_middle = resolver
        .resolve_for_parent(latest_middle_pin.unwrap())
        .unwrap();
    assert_module_route(
        &latest_middle,
        "main.orna",
        &format!("= {}", candidate_marker(1, 1)),
    );
    assert_eq!(
        latest_middle
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        replacement_commits[2][0]
    );
    let mut latest_deep_pin = None;
    for variant in [0, 1, 0] {
        latest_middle.detach_database(aliases[2]).unwrap();
        latest_middle
            .attach_database(replacement_pins[2][variant].clone())
            .unwrap();
        latest_deep_pin = Some(latest_middle.database(aliases[2]).unwrap().clone());
        assert_module_route(
            &latest_middle,
            &format!("{}.orna", aliases[2]),
            &format!("= {}", candidate_marker(2, variant)),
        );
    }
    let latest_terminal = resolver
        .resolve_for_parent(latest_deep_pin.unwrap())
        .unwrap();
    assert_module_route(
        &latest_terminal,
        "main.orna",
        &format!("= {}", candidate_marker(2, 0)),
    );
    assert_eq!(
        latest_terminal
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        terminal_commits[1]
    );
    assert_module_route(
        &latest_terminal,
        &format!("{}.orna", aliases[3]),
        &format!("= {}", terminal_marker(1)),
    );

    for variant in 0..candidate_count {
        let retained_middle = resolver
            .resolve_for_parent(retained_middle_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &retained_middle,
            "main.orna",
            &format!("= {}", candidate_marker(1, variant)),
        );
        assert_eq!(
            retained_middle
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            replacement_commits[2][1 - variant]
        );

        let retained_deep = resolver
            .resolve_for_parent(retained_old_deep_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &retained_deep,
            "main.orna",
            &format!("= {}", candidate_marker(2, variant)),
        );
        assert_eq!(
            retained_deep
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[1 - variant]
        );
    }
}

#[test]
fn terminal_alias_storm_resolves_latest_route_after_nested_rebinds() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let depth_count = aliases.len() - 1;
    let candidate_count = 2;
    let candidate_marker = |depth: usize, variant: usize| 100 + depth * 10 + variant;
    let terminal_marker = |variant: usize| 200 + variant;

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("original terminal storm alias {index}"),
            )
        })
        .collect::<Vec<_>>();
    let terminal_commits: Vec<String> = (0..candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", terminal_marker(variant)),
                None,
                &format!("rebound terminal route {variant}"),
            )
        })
        .collect();

    let mut replacement_commits: Vec<Vec<String>> = (0..depth_count)
        .map(|_| vec![String::new(); candidate_count])
        .collect();
    for depth in (0..depth_count).rev() {
        for variant in 0..candidate_count {
            let child_variant = 1 - variant;
            let child_index = depth + 1;
            let child_commit = if child_index < depth_count {
                &replacement_commits[child_index][child_variant]
            } else {
                &terminal_commits[child_variant]
            };
            let manifest = format!("{} {child_commit}\n", aliases[child_index]);
            replacement_commits[depth][variant] = write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", candidate_marker(depth, variant)),
                Some(&manifest),
                &format!("depth {depth} terminal-chain candidate {variant}"),
            );
        }
    }

    let loader = ProjectLoader::default();
    let replacement_pins: Vec<Vec<PinnedDatabase>> = (0..depth_count)
        .map(|depth| {
            (0..candidate_count)
                .map(|variant| {
                    PinnedDatabase::resolve(
                        aliases[depth],
                        shared_repository.clone(),
                        &replacement_commits[depth][variant],
                        loader,
                    )
                    .unwrap()
                })
                .collect()
        })
        .collect();
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let assert_replacement_route =
        |session: &AttachedDatabaseSession, depth: usize, variant: usize| {
            assert_eq!(
                session
                    .database(aliases[depth])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                replacement_commits[depth][variant]
            );
            assert_module_route(
                session,
                &format!("{}.orna", aliases[depth]),
                &format!("= {}", candidate_marker(depth, variant)),
            );
        };
    let assert_terminal_route = |session: &AttachedDatabaseSession, variant: usize| {
        assert_eq!(
            session
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[variant]
        );
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", terminal_marker(variant)),
        );
    };

    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let mut root_session = resolver.resolve_for_parent(primary).unwrap();
    let original_root = root_session.clone();
    for variant in [0, 1, 0] {
        root_session.detach_database(aliases[0]).unwrap();
        root_session
            .attach_database(replacement_pins[0][variant].clone())
            .unwrap();
        assert_replacement_route(&root_session, 0, variant);
        for (index, alias) in aliases[1..].iter().enumerate() {
            assert_eq!(
                root_session
                    .database(alias)
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                original_commits[index + 1]
            );
        }
    }
    assert_module_route(&original_root, "archive.orna", "= 10");

    let selected_root = root_session.database(aliases[0]).unwrap().clone();
    let mut outer = resolver.resolve_for_parent(selected_root).unwrap();
    assert_module_route(
        &outer,
        "main.orna",
        &format!("= {}", candidate_marker(0, 0)),
    );
    assert_replacement_route(&outer, 1, 1);
    let mut retained_middle_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [0, 1, 0] {
        outer.detach_database(aliases[1]).unwrap();
        outer
            .attach_database(replacement_pins[1][variant].clone())
            .unwrap();
        retained_middle_pins[variant]
            .get_or_insert_with(|| outer.database(aliases[1]).unwrap().clone());
        assert_replacement_route(&outer, 1, variant);
    }
    let selected_middle = outer.database(aliases[1]).unwrap().clone();
    let mut middle = resolver.resolve_for_parent(selected_middle).unwrap();
    assert_module_route(
        &middle,
        "main.orna",
        &format!("= {}", candidate_marker(1, 0)),
    );
    assert_replacement_route(&middle, 2, 1);
    let mut retained_deep_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [1, 0, 1] {
        middle.detach_database(aliases[2]).unwrap();
        middle
            .attach_database(replacement_pins[2][variant].clone())
            .unwrap();
        retained_deep_pins[variant]
            .get_or_insert_with(|| middle.database(aliases[2]).unwrap().clone());
        assert_replacement_route(&middle, 2, variant);
    }
    let selected_deep = middle.database(aliases[2]).unwrap().clone();
    let mut terminal = resolver.resolve_for_parent(selected_deep).unwrap();
    assert_module_route(
        &terminal,
        "main.orna",
        &format!("= {}", candidate_marker(2, 1)),
    );
    assert_terminal_route(&terminal, 0);
    let manifest_selected_terminal = terminal.clone();

    let mut retained_terminal_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [1, 0, 1] {
        terminal.detach_database(aliases[3]).unwrap();
        terminal
            .attach_database(terminal_pins[variant].clone())
            .unwrap();
        retained_terminal_pins[variant]
            .get_or_insert_with(|| terminal.database(aliases[3]).unwrap().clone());
        assert_terminal_route(&terminal, variant);
    }
    assert_terminal_route(&terminal, 1);
    assert_terminal_route(&manifest_selected_terminal, 0);

    for variant in 0..candidate_count {
        let retained_terminal = resolver
            .resolve_for_parent(retained_terminal_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_eq!(retained_terminal.primary().pin().name(), aliases[3]);
        assert_module_route(
            &retained_terminal,
            "main.orna",
            &format!("= {}", terminal_marker(variant)),
        );
    }
    for variant in 0..candidate_count {
        let retained_middle = resolver
            .resolve_for_parent(retained_middle_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &retained_middle,
            "main.orna",
            &format!("= {}", candidate_marker(1, variant)),
        );
        assert_replacement_route(&retained_middle, 2, 1 - variant);

        let retained_deep = resolver
            .resolve_for_parent(retained_deep_pins[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &retained_deep,
            "main.orna",
            &format!("= {}", candidate_marker(2, variant)),
        );
        assert_terminal_route(&retained_deep, 1 - variant);
    }
}

#[test]
fn terminal_routes_stay_branch_local_across_chained_rebind_storms() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let branch_count = 2;
    let candidate_count = 2;
    let marker =
        |depth: usize, branch: usize, variant: usize| (depth + 1) * 100 + branch * 10 + variant;

    let terminal_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(3, branch, variant)),
                        None,
                        &format!("branch {branch} terminal route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[3], terminal_commits[branch][1 - variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} deep route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[2], deep_commits[branch][1 - variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} middle route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let root_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[1], middle_commits[branch][1 - variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(0, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} root route {variant}"),
                    )
                })
                .collect()
        })
        .collect();

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|branch| {
                branch
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let root_pins = resolve_pin_grid(aliases[0], &root_commits);
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins = resolve_pin_grid(aliases[3], &terminal_commits);

    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("initial attached route {index}"),
            )
        })
        .collect::<Vec<_>>();
    let root_manifest = aliases
        .iter()
        .zip(&original_commits)
        .map(|(alias, commit)| format!("{alias} {commit}\n"))
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let mut root = resolver.resolve_for_parent(primary).unwrap();
    let original_root = root.clone();
    let mut retained_root_pins: Vec<Option<PinnedDatabase>> =
        (0..branch_count).map(|_| None).collect();
    for branch in [0, 1, 0] {
        root.detach_database(aliases[0]).unwrap();
        root.attach_database(root_pins[branch][branch].clone())
            .unwrap();
        retained_root_pins[branch]
            .get_or_insert_with(|| root.database(aliases[0]).unwrap().clone());
        assert_module_route(
            &root,
            &format!("{}.orna", aliases[0]),
            &format!("= {}", marker(0, branch, branch)),
        );
    }
    assert_module_route(&original_root, "archive.orna", "= 10");

    let mut finished_branches = Vec::new();
    for branch in 0..branch_count {
        let mut outer = resolver
            .resolve_for_parent(retained_root_pins[branch].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &outer,
            "main.orna",
            &format!("= {}", marker(0, branch, branch)),
        );
        assert_eq!(
            outer.database(aliases[1]).unwrap().pin().commit().as_str(),
            middle_commits[branch][1 - branch]
        );
        let manifest_selected_middle = outer.clone();

        let middle_order = if branch == 0 { [0, 1, 0] } else { [1, 0, 1] };
        let mut retained_middle_pins: Vec<Option<PinnedDatabase>> =
            (0..candidate_count).map(|_| None).collect();
        for variant in middle_order {
            outer.detach_database(aliases[1]).unwrap();
            outer
                .attach_database(middle_pins[branch][variant].clone())
                .unwrap();
            retained_middle_pins[variant]
                .get_or_insert_with(|| outer.database(aliases[1]).unwrap().clone());
            assert_module_route(
                &outer,
                &format!("{}.orna", aliases[1]),
                &format!("= {}", marker(1, branch, variant)),
            );
        }
        assert_module_route(
            &manifest_selected_middle,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, branch, 1 - branch)),
        );

        let selected_middle = outer.database(aliases[1]).unwrap().clone();
        let mut middle = resolver.resolve_for_parent(selected_middle).unwrap();
        assert_module_route(
            &middle,
            "main.orna",
            &format!("= {}", marker(1, branch, branch)),
        );
        let manifest_selected_deep = middle.clone();
        let deep_order = [1 - branch, branch, branch];
        let mut retained_deep_pins: Vec<Option<PinnedDatabase>> =
            (0..candidate_count).map(|_| None).collect();
        for variant in deep_order {
            middle.detach_database(aliases[2]).unwrap();
            middle
                .attach_database(deep_pins[branch][variant].clone())
                .unwrap();
            retained_deep_pins[variant]
                .get_or_insert_with(|| middle.database(aliases[2]).unwrap().clone());
            assert_module_route(
                &middle,
                &format!("{}.orna", aliases[2]),
                &format!("= {}", marker(2, branch, variant)),
            );
        }
        assert_module_route(
            &manifest_selected_deep,
            &format!("{}.orna", aliases[2]),
            &format!("= {}", marker(2, branch, 1 - branch)),
        );

        let selected_deep = middle.database(aliases[2]).unwrap().clone();
        let mut terminal = resolver.resolve_for_parent(selected_deep).unwrap();
        assert_module_route(
            &terminal,
            "main.orna",
            &format!("= {}", marker(2, branch, branch)),
        );
        let manifest_selected_terminal = terminal.clone();
        let mut retained_terminal_pins: Vec<Option<PinnedDatabase>> =
            (0..candidate_count).map(|_| None).collect();
        for variant in if branch == 0 { [0, 1, 0] } else { [1, 0, 1] } {
            terminal.detach_database(aliases[3]).unwrap();
            terminal
                .attach_database(terminal_pins[branch][variant].clone())
                .unwrap();
            retained_terminal_pins[variant]
                .get_or_insert_with(|| terminal.database(aliases[3]).unwrap().clone());
            assert_module_route(
                &terminal,
                &format!("{}.orna", aliases[3]),
                &format!("= {}", marker(3, branch, variant)),
            );
        }
        assert_module_route(
            &manifest_selected_terminal,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, branch, 1 - branch)),
        );
        finished_branches.push((terminal, retained_terminal_pins));
    }

    for branch in 0..branch_count {
        let (terminal, retained_pins) = &finished_branches[branch];
        assert_module_route(
            terminal,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, branch, branch)),
        );
        for variant in 0..candidate_count {
            let retained = resolver
                .resolve_for_parent(retained_pins[variant].as_ref().unwrap().clone())
                .unwrap();
            assert_eq!(retained.primary().pin().name(), aliases[3]);
            assert_module_route(
                &retained,
                "main.orna",
                &format!("= {}", marker(3, branch, variant)),
            );
        }
    }
}

#[test]
fn paired_depth_storm_closures_keep_terminal_routes_isolated() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let branch_count = 2;
    let candidate_count = 2;
    let marker =
        |depth: usize, branch: usize, variant: usize| (depth + 1) * 100 + branch * 10 + variant;

    let terminal_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(3, branch, variant)),
                        None,
                        &format!("paired branch {branch} terminal {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[3], terminal_commits[branch][1 - variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, branch, variant)),
                        Some(&manifest),
                        &format!("paired branch {branch} deep {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let loader = ProjectLoader::default();
    let middle_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], deep_commits[branch][variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, branch, variant)),
                        Some(&manifest),
                        &format!("paired branch {branch} middle {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|branch| {
                branch
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins = resolve_pin_grid(aliases[3], &terminal_commits);

    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "shared parent for paired closures",
    );
    let original_commits = aliases
        .iter()
        .enumerate()
        .map(|(index, _)| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", 10 + index),
                None,
                &format!("initial paired closure attachment {index}"),
            )
        })
        .collect::<Vec<_>>();
    let root_manifest = aliases
        .iter()
        .enumerate()
        .map(|(index, alias)| {
            let commit = if index == 0 {
                &parent_commit
            } else {
                &original_commits[index]
            };
            format!("{alias} {commit}\n")
        })
        .collect::<String>();
    let (_root_dir, root_repository, root_commit) = repository(&[
        ("main.orna", include_str!("fixtures/attach-primary.orna")),
        (PACKAGE_PIN_MANIFEST_PATH, &root_manifest),
    ]);
    let primary = PinnedDatabase::resolve("app", root_repository, &root_commit, loader).unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let root = resolver.resolve_for_parent(primary).unwrap();
    let shared_parent = root.database(aliases[0]).unwrap().clone();
    let mut left = resolver.resolve_for_parent(shared_parent.clone()).unwrap();
    let mut right = resolver.resolve_for_parent(shared_parent).unwrap();
    assert_eq!(
        left.database(aliases[1]).unwrap().pin().commit().as_str(),
        right.database(aliases[1]).unwrap().pin().commit().as_str()
    );
    assert_module_route(&left, "main.orna", "= 50");
    assert_module_route(&right, "main.orna", "= 50");

    let mut retained_middle = [
        (0..candidate_count)
            .map(|_| None)
            .collect::<Vec<Option<PinnedDatabase>>>(),
        (0..candidate_count)
            .map(|_| None)
            .collect::<Vec<Option<PinnedDatabase>>>(),
    ];
    for variant in [0, 1, 0] {
        left.detach_database(aliases[1]).unwrap();
        left.attach_database(middle_pins[0][variant].clone())
            .unwrap();
        retained_middle[0][variant]
            .get_or_insert_with(|| left.database(aliases[1]).unwrap().clone());
        assert_module_route(
            &left,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, 0, variant)),
        );
    }
    assert_module_route(&right, &format!("{}.orna", aliases[1]), "= 200");
    for variant in [1, 0, 1] {
        right.detach_database(aliases[1]).unwrap();
        right
            .attach_database(middle_pins[1][variant].clone())
            .unwrap();
        retained_middle[1][variant]
            .get_or_insert_with(|| right.database(aliases[1]).unwrap().clone());
        assert_module_route(
            &right,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, 1, variant)),
        );
    }
    assert_module_route(&left, &format!("{}.orna", aliases[1]), "= 200");

    let mut left_deep = resolver
        .resolve_for_parent(left.database(aliases[1]).unwrap().clone())
        .unwrap();
    let mut right_deep = resolver
        .resolve_for_parent(right.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_module_route(&left_deep, "main.orna", "= 200");
    assert_module_route(&right_deep, "main.orna", "= 211");
    let mut retained_deep = [
        (0..candidate_count)
            .map(|_| None)
            .collect::<Vec<Option<PinnedDatabase>>>(),
        (0..candidate_count)
            .map(|_| None)
            .collect::<Vec<Option<PinnedDatabase>>>(),
    ];
    for variant in [1, 0, 1] {
        left_deep.detach_database(aliases[2]).unwrap();
        left_deep
            .attach_database(deep_pins[0][variant].clone())
            .unwrap();
        retained_deep[0][variant]
            .get_or_insert_with(|| left_deep.database(aliases[2]).unwrap().clone());
        assert_module_route(
            &left_deep,
            &format!("{}.orna", aliases[2]),
            &format!("= {}", marker(2, 0, variant)),
        );
    }
    assert_module_route(&right_deep, &format!("{}.orna", aliases[2]), "= 311");
    for variant in [0, 1, 0] {
        right_deep.detach_database(aliases[2]).unwrap();
        right_deep
            .attach_database(deep_pins[1][variant].clone())
            .unwrap();
        retained_deep[1][variant]
            .get_or_insert_with(|| right_deep.database(aliases[2]).unwrap().clone());
        assert_module_route(
            &right_deep,
            &format!("{}.orna", aliases[2]),
            &format!("= {}", marker(2, 1, variant)),
        );
    }
    assert_module_route(&left_deep, &format!("{}.orna", aliases[2]), "= 301");

    let mut left_terminal = resolver
        .resolve_for_parent(left_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    let mut right_terminal = resolver
        .resolve_for_parent(right_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&left_terminal, &format!("{}.orna", aliases[3]), "= 400");
    assert_module_route(&right_terminal, &format!("{}.orna", aliases[3]), "= 411");
    let left_manifest_route = left_terminal.clone();
    let right_manifest_route = right_terminal.clone();
    let mut retained_terminal = [
        (0..candidate_count)
            .map(|_| None)
            .collect::<Vec<Option<PinnedDatabase>>>(),
        (0..candidate_count)
            .map(|_| None)
            .collect::<Vec<Option<PinnedDatabase>>>(),
    ];
    for variant in [0, 1] {
        left_terminal.detach_database(aliases[3]).unwrap();
        left_terminal
            .attach_database(terminal_pins[0][variant].clone())
            .unwrap();
        retained_terminal[0][variant]
            .get_or_insert_with(|| left_terminal.database(aliases[3]).unwrap().clone());
        assert_module_route(
            &left_terminal,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, 0, variant)),
        );
    }
    assert_module_route(&right_terminal, &format!("{}.orna", aliases[3]), "= 411");
    for variant in [1, 0] {
        right_terminal.detach_database(aliases[3]).unwrap();
        right_terminal
            .attach_database(terminal_pins[1][variant].clone())
            .unwrap();
        retained_terminal[1][variant]
            .get_or_insert_with(|| right_terminal.database(aliases[3]).unwrap().clone());
        assert_module_route(
            &right_terminal,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, 1, variant)),
        );
    }

    assert_module_route(&left_terminal, &format!("{}.orna", aliases[3]), "= 401");
    assert_module_route(&right_terminal, &format!("{}.orna", aliases[3]), "= 410");
    assert_module_route(
        &left_manifest_route,
        &format!("{}.orna", aliases[3]),
        "= 400",
    );
    assert_module_route(
        &right_manifest_route,
        &format!("{}.orna", aliases[3]),
        "= 411",
    );
    for branch in 0..branch_count {
        for variant in 0..candidate_count {
            let retained = resolver
                .resolve_for_parent(retained_terminal[branch][variant].as_ref().unwrap().clone())
                .unwrap();
            assert_module_route(
                &retained,
                "main.orna",
                &format!("= {}", marker(3, branch, variant)),
            );
        }
    }
    for branch in 0..branch_count {
        for variant in 0..candidate_count {
            let retained_middle = resolver
                .resolve_for_parent(retained_middle[branch][variant].as_ref().unwrap().clone())
                .unwrap();
            assert_module_route(
                &retained_middle,
                "main.orna",
                &format!("= {}", marker(1, branch, variant)),
            );
            assert_module_route(
                &retained_middle,
                &format!("{}.orna", aliases[2]),
                &format!("= {}", marker(2, branch, variant)),
            );

            let retained_deep = resolver
                .resolve_for_parent(retained_deep[branch][variant].as_ref().unwrap().clone())
                .unwrap();
            assert_module_route(
                &retained_deep,
                "main.orna",
                &format!("= {}", marker(2, branch, variant)),
            );
            assert_module_route(
                &retained_deep,
                &format!("{}.orna", aliases[3]),
                &format!("= {}", marker(3, branch, 1 - variant)),
            );
        }
    }
}

#[test]
fn late_sibling_expansion_keeps_terminal_storm_routes_isolated() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = ["archive", "archive_copy", "archive_copy_archive"];
    let branch_count = 2;
    let candidate_count = 2;
    let marker =
        |depth: usize, branch: usize, variant: usize| (depth + 1) * 100 + branch * 10 + variant;

    let terminal_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, branch, variant)),
                        None,
                        &format!("late sibling terminal {branch}-{variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<String> = (0..branch_count)
        .map(|branch| {
            let manifest = format!("{} {}\n", aliases[2], terminal_commits[branch][0]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, branch, 0)),
                Some(&manifest),
                &format!("late sibling middle route {branch}"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[1]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "retained ancestor for late sibling expansion",
    );

    let loader = ProjectLoader::default();
    let middle_pins: Vec<PinnedDatabase> = middle_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[1], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let terminal_pins: Vec<Vec<PinnedDatabase>> = terminal_commits
        .iter()
        .map(|branch| {
            branch
                .iter()
                .map(|commit| {
                    PinnedDatabase::resolve(aliases[2], shared_repository.clone(), commit, loader)
                        .unwrap()
                })
                .collect()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_middle_route = |session: &AttachedDatabaseSession, branch: usize| {
        assert_eq!(
            session
                .database(aliases[1])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            middle_commits[branch]
        );
        assert_module_route(
            session,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, branch, 0)),
        );
    };
    let assert_terminal_route =
        |session: &AttachedDatabaseSession, branch: usize, variant: usize| {
            assert_eq!(
                session
                    .database(aliases[2])
                    .unwrap()
                    .pin()
                    .commit()
                    .as_str(),
                terminal_commits[branch][variant]
            );
            assert_module_route(
                session,
                &format!("{}.orna", aliases[2]),
                &format!("= {}", marker(2, branch, variant)),
            );
        };

    let mut early = resolver.resolve_for_parent(parent_pin.clone()).unwrap();
    assert_middle_route(&early, 1);
    let manifest_selected_early = early.clone();
    early.detach_database(aliases[1]).unwrap();
    early.attach_database(middle_pins[0].clone()).unwrap();
    assert_middle_route(&early, 0);
    assert_middle_route(&manifest_selected_early, 1);

    let mut early_terminal = resolver
        .resolve_for_parent(early.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_module_route(&early_terminal, "main.orna", "= 200");
    assert_terminal_route(&early_terminal, 0, 0);
    let early_manifest_terminal = early_terminal.clone();
    early_terminal.detach_database(aliases[2]).unwrap();
    early_terminal
        .attach_database(terminal_pins[0][1].clone())
        .unwrap();
    assert_terminal_route(&early_terminal, 0, 1);
    assert_terminal_route(&early_manifest_terminal, 0, 0);

    // Expand the same retained ancestor only after its earlier sibling has
    // rebound a terminal route; this closure must still start from branch 1.
    let late = resolver.resolve_for_parent(parent_pin).unwrap();
    assert_module_route(&late, "main.orna", "= 50");
    assert_middle_route(&late, 1);
    let mut late_terminal = resolver
        .resolve_for_parent(late.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_module_route(&late_terminal, "main.orna", "= 210");
    assert_terminal_route(&late_terminal, 1, 0);
    let late_manifest_terminal = late_terminal.clone();
    late_terminal.detach_database(aliases[2]).unwrap();
    late_terminal
        .attach_database(terminal_pins[1][1].clone())
        .unwrap();
    assert_terminal_route(&late_terminal, 1, 1);

    assert_terminal_route(&early_terminal, 0, 1);
    assert_terminal_route(&late_manifest_terminal, 1, 0);
    for (branch, variant) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        let retained = resolver
            .resolve_for_parent(terminal_pins[branch][variant].clone())
            .unwrap();
        assert_module_route(
            &retained,
            "main.orna",
            &format!("= {}", marker(2, branch, variant)),
        );
    }
}
#[test]
fn late_sibling_terminal_routes_survive_paired_depth_storms() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let branch_count = 2;
    let candidate_count = 2;
    let marker =
        |depth: usize, branch: usize, variant: usize| (depth + 1) * 100 + branch * 10 + variant;

    let terminal_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(3, branch, variant)),
                        None,
                        &format!("deep late branch terminal {branch}-{variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[3], terminal_commits[branch][1 - variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, branch, variant)),
                        Some(&manifest),
                        &format!("deep late branch deep {branch}-{variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[2], deep_commits[branch][1 - variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, branch, variant)),
                        Some(&manifest),
                        &format!("deep late branch middle {branch}-{variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[1][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "ancestor for paired deep closure storms",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|branch| {
                branch
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins = resolve_pin_grid(aliases[3], &terminal_commits);
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_child_route =
        |session: &AttachedDatabaseSession, depth: usize, branch: usize, variant: usize| {
            let alias = aliases[depth];
            let commits = match depth {
                1 => &middle_commits,
                2 => &deep_commits,
                3 => &terminal_commits,
                _ => unreachable!(),
            };
            assert_eq!(
                session.database(alias).unwrap().pin().commit().as_str(),
                commits[branch][variant]
            );
            assert_module_route(
                session,
                &format!("{alias}.orna"),
                &format!("= {}", marker(depth, branch, variant)),
            );
        };

    let mut early = resolver.resolve_for_parent(parent_pin.clone()).unwrap();
    assert_child_route(&early, 1, 1, 0);
    let early_manifest_middle = early.clone();
    for variant in [0, 1, 0] {
        early.detach_database(aliases[1]).unwrap();
        early
            .attach_database(middle_pins[0][variant].clone())
            .unwrap();
        assert_child_route(&early, 1, 0, variant);
    }
    assert_child_route(&early_manifest_middle, 1, 1, 0);

    let mut early_deep = resolver
        .resolve_for_parent(early.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_module_route(&early_deep, "main.orna", "= 200");
    assert_child_route(&early_deep, 2, 0, 1);
    let early_manifest_deep = early_deep.clone();
    for variant in [1, 0, 0] {
        early_deep.detach_database(aliases[2]).unwrap();
        early_deep
            .attach_database(deep_pins[0][variant].clone())
            .unwrap();
        assert_child_route(&early_deep, 2, 0, variant);
    }
    assert_child_route(&early_manifest_deep, 2, 0, 1);

    let mut early_terminal = resolver
        .resolve_for_parent(early_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&early_terminal, "main.orna", "= 300");
    assert_child_route(&early_terminal, 3, 0, 1);
    let early_manifest_terminal = early_terminal.clone();
    let mut retained_terminals: Vec<Vec<Option<PinnedDatabase>>> = (0..branch_count)
        .map(|_| (0..candidate_count).map(|_| None).collect())
        .collect();
    for variant in [0, 1, 0] {
        early_terminal.detach_database(aliases[3]).unwrap();
        early_terminal
            .attach_database(terminal_pins[0][variant].clone())
            .unwrap();
        retained_terminals[0][variant]
            .get_or_insert_with(|| early_terminal.database(aliases[3]).unwrap().clone());
        assert_child_route(&early_terminal, 3, 0, variant);
    }

    // Only now expand the retained ancestor again. The earlier branch has
    // already changed its middle, deep, and terminal aliases.
    let late = resolver.resolve_for_parent(parent_pin).unwrap();
    assert_module_route(&late, "main.orna", "= 50");
    assert_child_route(&late, 1, 1, 0);
    let mut late_deep = resolver
        .resolve_for_parent(late.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_module_route(&late_deep, "main.orna", "= 210");
    assert_child_route(&late_deep, 2, 1, 1);
    let late_manifest_deep = late_deep.clone();
    for variant in [0, 1, 0] {
        late_deep.detach_database(aliases[2]).unwrap();
        late_deep
            .attach_database(deep_pins[1][variant].clone())
            .unwrap();
        assert_child_route(&late_deep, 2, 1, variant);
    }
    assert_child_route(&late_manifest_deep, 2, 1, 1);

    let mut late_terminal = resolver
        .resolve_for_parent(late_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&late_terminal, "main.orna", "= 310");
    assert_child_route(&late_terminal, 3, 1, 1);
    let late_manifest_terminal = late_terminal.clone();
    for variant in [1, 0, 1] {
        late_terminal.detach_database(aliases[3]).unwrap();
        late_terminal
            .attach_database(terminal_pins[1][variant].clone())
            .unwrap();
        retained_terminals[1][variant]
            .get_or_insert_with(|| late_terminal.database(aliases[3]).unwrap().clone());
        assert_child_route(&late_terminal, 3, 1, variant);
    }

    assert_child_route(&early_terminal, 3, 0, 0);
    assert_child_route(&early_manifest_terminal, 3, 0, 1);
    assert_child_route(&late_terminal, 3, 1, 1);
    assert_child_route(&late_manifest_terminal, 3, 1, 1);
    for (branch, variant) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
        let retained = resolver
            .resolve_for_parent(
                retained_terminals[branch][variant]
                    .as_ref()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_module_route(
            &retained,
            "main.orna",
            &format!("= {}", marker(3, branch, variant)),
        );
    }
}
#[test]
fn retained_middle_pin_keeps_late_route_across_sibling_depth_storms() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let candidate_count = 2;
    let marker = |depth: usize, variant: usize| (depth + 1) * 100 + variant;

    let terminal_commits: Vec<String> = (0..candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, variant)),
                None,
                &format!("retained middle terminal {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<String> = (0..candidate_count)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[3], terminal_commits[variant]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(2, variant)),
                Some(&manifest),
                &format!("retained middle deep {variant}"),
            )
        })
        .collect();
    let middle_commits: Vec<String> = (0..candidate_count)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[2], deep_commits[1 - variant]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, variant)),
                Some(&manifest),
                &format!("retained sibling middle {variant}"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent retaining initial middle route",
    );

    let loader = ProjectLoader::default();
    let resolve_pins = |alias: &str, commits: &[String]| {
        commits
            .iter()
            .map(|commit| {
                PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pins(aliases[1], &middle_commits);
    let deep_pins = resolve_pins(aliases[2], &deep_commits);
    let terminal_pins = resolve_pins(aliases[3], &terminal_commits);
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_route = |session: &AttachedDatabaseSession, depth: usize, variant: usize| {
        let alias = aliases[depth];
        let commits = match depth {
            1 => &middle_commits,
            2 => &deep_commits,
            3 => &terminal_commits,
            _ => unreachable!(),
        };
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commits[variant]
        );
        assert_module_route(
            session,
            &format!("{alias}.orna"),
            &format!("= {}", marker(depth, variant)),
        );
    };

    let mut early = resolver.resolve_for_parent(parent_pin).unwrap();
    assert_route(&early, 1, 0);
    let retained_middle = early.database(aliases[1]).unwrap().clone();
    let manifest_middle_snapshot = early.clone();
    for variant in [0, 1] {
        early.detach_database(aliases[1]).unwrap();
        early.attach_database(middle_pins[variant].clone()).unwrap();
        assert_route(&early, 1, variant);
    }
    assert_route(&manifest_middle_snapshot, 1, 0);

    let mut early_deep = resolver
        .resolve_for_parent(early.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_module_route(&early_deep, "main.orna", "= 201");
    assert_route(&early_deep, 2, 0);
    let manifest_deep_snapshot = early_deep.clone();
    let mut early_deep_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [0, 1] {
        early_deep.detach_database(aliases[2]).unwrap();
        early_deep
            .attach_database(deep_pins[variant].clone())
            .unwrap();
        early_deep_pins[variant]
            .get_or_insert_with(|| early_deep.database(aliases[2]).unwrap().clone());
        assert_route(&early_deep, 2, variant);
    }
    assert_route(&manifest_deep_snapshot, 2, 0);

    let mut early_terminal = resolver
        .resolve_for_parent(early_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&early_terminal, "main.orna", "= 301");
    assert_route(&early_terminal, 3, 1);
    let early_manifest_terminal = early_terminal.clone();
    let mut early_terminal_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [1, 0] {
        early_terminal.detach_database(aliases[3]).unwrap();
        early_terminal
            .attach_database(terminal_pins[variant].clone())
            .unwrap();
        early_terminal_pins[variant]
            .get_or_insert_with(|| early_terminal.database(aliases[3]).unwrap().clone());
        assert_route(&early_terminal, 3, variant);
    }
    assert_route(&early_manifest_terminal, 3, 1);

    // Reopen the middle pin retained before the sibling selected variant 1.
    // Its manifest must still choose deep pin 1 after the sibling's deep and
    // terminal storms have already completed.
    let mut late_deep = resolver.resolve_for_parent(retained_middle).unwrap();
    assert_module_route(&late_deep, "main.orna", "= 200");
    assert_route(&late_deep, 2, 1);
    let late_manifest_deep = late_deep.clone();
    let mut late_deep_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [1, 0] {
        late_deep.detach_database(aliases[2]).unwrap();
        late_deep
            .attach_database(deep_pins[variant].clone())
            .unwrap();
        late_deep_pins[variant]
            .get_or_insert_with(|| late_deep.database(aliases[2]).unwrap().clone());
        assert_route(&late_deep, 2, variant);
    }
    assert_route(&late_manifest_deep, 2, 1);

    let mut late_terminal = resolver
        .resolve_for_parent(late_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&late_terminal, "main.orna", "= 300");
    assert_route(&late_terminal, 3, 0);
    let late_manifest_terminal = late_terminal.clone();
    let mut late_terminal_pins: Vec<Option<PinnedDatabase>> =
        (0..candidate_count).map(|_| None).collect();
    for variant in [0, 1] {
        late_terminal.detach_database(aliases[3]).unwrap();
        late_terminal
            .attach_database(terminal_pins[variant].clone())
            .unwrap();
        late_terminal_pins[variant]
            .get_or_insert_with(|| late_terminal.database(aliases[3]).unwrap().clone());
        assert_route(&late_terminal, 3, variant);
    }

    assert_route(&early_terminal, 3, 0);
    assert_route(&early_manifest_terminal, 3, 1);
    assert_route(&late_terminal, 3, 1);
    assert_route(&late_manifest_terminal, 3, 0);
    for variant in 0..candidate_count {
        for pin in [
            early_deep_pins[variant].as_ref().unwrap(),
            late_deep_pins[variant].as_ref().unwrap(),
        ] {
            let retained = resolver.resolve_for_parent(pin.clone()).unwrap();
            assert_module_route(
                &retained,
                &format!("{}.orna", aliases[3]),
                &format!("= {}", marker(3, variant)),
            );
        }
        for pin in [
            early_terminal_pins[variant].as_ref().unwrap(),
            late_terminal_pins[variant].as_ref().unwrap(),
        ] {
            let retained = resolver.resolve_for_parent(pin.clone()).unwrap();
            assert_module_route(&retained, "main.orna", &format!("= {}", marker(3, variant)));
        }
    }
}
#[test]
fn shared_terminal_pin_survives_sibling_closure_rebind_storms() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let terminal_candidate_count = 3;
    let marker = |depth: usize, variant: usize| (depth + 1) * 100 + variant;

    let terminal_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, variant)),
                None,
                &format!("shared terminal candidate {variant}"),
            )
        })
        .collect();
    let deep_manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
    let deep_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        &format!("{}", marker(2, 0)),
        Some(&deep_manifest),
        "shared deep manifest route",
    );
    let middle_commits: Vec<String> = (0..2)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[2], deep_commit);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, variant)),
                Some(&manifest),
                &format!("sibling middle route {variant} shares deep pin"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "ancestor selecting first sibling middle pin",
    );

    let loader = ProjectLoader::default();
    let middle_pins: Vec<PinnedDatabase> = middle_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[1], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_terminal_route = |session: &AttachedDatabaseSession, variant: usize| {
        assert_eq!(
            session
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[variant]
        );
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, variant)),
        );
    };

    let mut sibling = resolver.resolve_for_parent(parent_pin).unwrap();
    assert_eq!(
        sibling
            .database(aliases[1])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        middle_commits[0]
    );
    let retained_middle = sibling.database(aliases[1]).unwrap().clone();
    let original_sibling = sibling.clone();
    sibling.detach_database(aliases[1]).unwrap();
    sibling.attach_database(middle_pins[1].clone()).unwrap();
    assert_module_route(&sibling, &format!("{}.orna", aliases[1]), "= 201");
    assert_module_route(&original_sibling, &format!("{}.orna", aliases[1]), "= 200");

    let selected_middle = sibling.database(aliases[1]).unwrap().clone();
    let early_middle = resolver.resolve_for_parent(selected_middle).unwrap();
    assert_module_route(&early_middle, "main.orna", "= 201");
    assert_eq!(
        early_middle
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        deep_commit
    );
    let mut early_terminal = resolver
        .resolve_for_parent(early_middle.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&early_terminal, "main.orna", "= 300");
    assert_terminal_route(&early_terminal, 0);
    let retained_shared_terminal = early_terminal.database(aliases[3]).unwrap().clone();
    let manifest_selected_terminal = early_terminal.clone();
    let mut retained_replacements: Vec<Option<PinnedDatabase>> =
        (0..terminal_candidate_count).map(|_| None).collect();
    for variant in [1, 2] {
        early_terminal.detach_database(aliases[3]).unwrap();
        early_terminal
            .attach_database(terminal_pins[variant].clone())
            .unwrap();
        retained_replacements[variant]
            .get_or_insert_with(|| early_terminal.database(aliases[3]).unwrap().clone());
        assert_terminal_route(&early_terminal, variant);
    }
    assert_terminal_route(&manifest_selected_terminal, 0);

    // This sibling was retained before the first branch changed the shared
    // terminal alias, so its expansion must recover the same exact deep pin.
    let late_deep = resolver.resolve_for_parent(retained_middle).unwrap();
    assert_module_route(&late_deep, "main.orna", "= 200");
    assert_eq!(
        late_deep
            .database(aliases[2])
            .unwrap()
            .pin()
            .commit()
            .as_str(),
        deep_commit
    );
    let mut late_terminal = resolver
        .resolve_for_parent(late_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_module_route(&late_terminal, "main.orna", "= 300");
    assert_terminal_route(&late_terminal, 0);
    let late_manifest_terminal = late_terminal.clone();
    late_terminal.detach_database(aliases[3]).unwrap();
    late_terminal
        .attach_database(terminal_pins[1].clone())
        .unwrap();
    assert_terminal_route(&late_terminal, 1);

    assert_terminal_route(&early_terminal, 2);
    assert_terminal_route(&manifest_selected_terminal, 0);
    assert_terminal_route(&late_manifest_terminal, 0);
    let retained_shared = resolver
        .resolve_for_parent(retained_shared_terminal)
        .unwrap();
    assert_module_route(&retained_shared, "main.orna", "= 400");
    for variant in 1..terminal_candidate_count {
        let retained = resolver
            .resolve_for_parent(retained_replacements[variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(&retained, "main.orna", &format!("= {}", marker(3, variant)));
    }
}

#[test]
fn paired_depth_storms_preserve_shared_terminal_route_stability() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let branch_count = 2;
    let candidate_count = 2;
    let terminal_candidate_count = 3;
    let marker = |depth: usize, branch: usize, variant: usize| {
        (depth + 1) * 100 + branch * 10 + variant
    };

    let terminal_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, 0, variant)),
                None,
                &format!("shared terminal route candidate {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} deep candidate {variant} converges"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest =
                        format!("{} {}\n", aliases[2], deep_commits[branch][variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} middle candidate {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for paired routes sharing terminal pin",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|branch| {
                branch
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, expected: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            expected
        );
    };
    let assert_terminal = |session: &AttachedDatabaseSession, variant: usize| {
        assert_pin(session, aliases[3], &terminal_commits[variant]);
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, 0, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut paired_middle = [parent.clone(), parent.clone()];
    let mut retained_middle: Vec<Vec<Option<PinnedDatabase>>> =
        vec![vec![None; candidate_count]; branch_count];
    for (branch, variant) in [(0, 1), (1, 0), (0, 0), (1, 1), (0, 1), (1, 0)] {
        paired_middle[branch]
            .detach_database(aliases[1])
            .unwrap();
        paired_middle[branch]
            .attach_database(middle_pins[branch][variant].clone())
            .unwrap();
        retained_middle[branch][variant]
            .get_or_insert_with(|| paired_middle[branch].database(aliases[1]).unwrap().clone());
        assert_module_route(
            &paired_middle[branch],
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, branch, variant)),
        );
    }
    assert_module_route(&parent, &format!("{}.orna", aliases[1]), "= 200");

    let mut paired_deep = [
        resolver
            .resolve_for_parent(paired_middle[0].database(aliases[1]).unwrap().clone())
            .unwrap(),
        resolver
            .resolve_for_parent(paired_middle[1].database(aliases[1]).unwrap().clone())
            .unwrap(),
    ];
    let mut retained_deep: Vec<Vec<Option<PinnedDatabase>>> =
        vec![vec![None; candidate_count]; branch_count];
    for (branch, variant) in [(0, 0), (1, 1), (0, 1), (1, 0), (0, 0), (1, 1)] {
        paired_deep[branch]
            .detach_database(aliases[2])
            .unwrap();
        paired_deep[branch]
            .attach_database(deep_pins[branch][variant].clone())
            .unwrap();
        retained_deep[branch][variant]
            .get_or_insert_with(|| paired_deep[branch].database(aliases[2]).unwrap().clone());
        assert_pin(&paired_deep[branch], aliases[2], &deep_commits[branch][variant]);
        assert_module_route(
            &paired_deep[branch],
            &format!("{}.orna", aliases[2]),
            &format!("= {}", marker(2, branch, variant)),
        );
    }

    let selected_deep: Vec<PinnedDatabase> = (0..branch_count)
        .map(|branch| paired_deep[branch].database(aliases[2]).unwrap().clone())
        .collect();
    let mut paired_terminal = [
        resolver.resolve_for_parent(selected_deep[0].clone()).unwrap(),
        resolver.resolve_for_parent(selected_deep[1].clone()).unwrap(),
    ];
    assert_terminal(&paired_terminal[0], 0);
    assert_terminal(&paired_terminal[1], 0);
    let manifest_routes = paired_terminal.clone();
    let mut retained_terminal: Vec<Vec<Option<PinnedDatabase>>> =
        vec![vec![None; terminal_candidate_count]; branch_count];

    for (branch, candidate) in [(0, 1), (1, 2), (0, 2), (1, 1)] {
        paired_terminal[branch]
            .detach_database(aliases[3])
            .unwrap();
        paired_terminal[branch]
            .attach_database(terminal_pins[candidate].clone())
            .unwrap();
        retained_terminal[branch][candidate]
            .get_or_insert_with(|| paired_terminal[branch].database(aliases[3]).unwrap().clone());
        assert_terminal(&paired_terminal[branch], candidate);
    }
    assert_terminal(&paired_terminal[0], 2);
    assert_terminal(&paired_terminal[1], 1);
    assert_terminal(&manifest_routes[0], 0);
    assert_terminal(&manifest_routes[1], 0);

    for deep_pin in selected_deep {
        let reopened = resolver.resolve_for_parent(deep_pin).unwrap();
        assert_terminal(&reopened, 0);
    }
    for (branch, variant) in [(0, 0), (1, 1)] {
        let reopened_middle = resolver
            .resolve_for_parent(retained_middle[branch][variant].as_ref().unwrap().clone())
            .unwrap();
        assert_module_route(
            &reopened_middle,
            "main.orna",
            &format!("= {}", marker(1, branch, variant)),
        );
        let reopened_terminal = resolver
            .resolve_for_parent(reopened_middle.database(aliases[2]).unwrap().clone())
            .unwrap();
        assert_terminal(&reopened_terminal, 0);
    }
    for (branch, variant) in [(0, 1), (1, 0)] {
        let reopened = resolver
            .resolve_for_parent(retained_deep[branch][variant].as_ref().unwrap().clone())
            .unwrap();
        assert_terminal(&reopened, 0);
    }
    let retained_terminal_pin = retained_terminal[0][1].as_ref().unwrap().clone();
    let reopened_terminal = resolver.resolve_for_parent(retained_terminal_pin).unwrap();
    assert_module_route(&reopened_terminal, "main.orna", "= 401");
    assert_module_route(&parent, &format!("{}.orna", aliases[1]), "= 200");
}

#[test]
fn sibling_depth_storms_preserve_convergent_terminal_routes() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let marker = |depth: usize, variant: usize| (depth + 1) * 100 + variant;
    let terminal_candidate_count = 3;

    let terminal_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, variant)),
                None,
                &format!("convergent terminal candidate {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(2, variant)),
                Some(&manifest),
                &format!("sibling deep storm candidate {variant} converges"),
            )
        })
        .collect();
    let shared_deep_commit = deep_commits[0].clone();
    let middle_commits: Vec<String> = (0..2)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[2], shared_deep_commit);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, variant)),
                Some(&manifest),
                &format!("sibling middle pin {variant} shares exact deep pin"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent retaining the first sibling route",
    );

    let loader = ProjectLoader::default();
    let middle_pins: Vec<PinnedDatabase> = middle_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[1], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let deep_pins: Vec<PinnedDatabase> = deep_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[2], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(session.database(alias).unwrap().pin().commit().as_str(), commit);
    };
    let assert_terminal = |session: &AttachedDatabaseSession, variant: usize| {
        assert_pin(session, aliases[3], &terminal_commits[variant]);
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut siblings = [parent.clone(), parent.clone()];
    let mut retained_middle: Vec<Option<PinnedDatabase>> = vec![None, None];
    for (sibling, variant) in [(0, 1), (1, 0), (0, 0), (1, 1)] {
        siblings[sibling].detach_database(aliases[1]).unwrap();
        siblings[sibling]
            .attach_database(middle_pins[variant].clone())
            .unwrap();
        retained_middle[sibling]
            .get_or_insert_with(|| siblings[sibling].database(aliases[1]).unwrap().clone());
        assert_module_route(
            &siblings[sibling],
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, variant)),
        );
    }
    assert_module_route(&parent, &format!("{}.orna", aliases[1]), "= 200");

    let mut deep_branches = [
        resolver
            .resolve_for_parent(siblings[0].database(aliases[1]).unwrap().clone())
            .unwrap(),
        resolver
            .resolve_for_parent(siblings[1].database(aliases[1]).unwrap().clone())
            .unwrap(),
    ];
    let mut retained_deep: Vec<Vec<Option<PinnedDatabase>>> = vec![vec![None; 3]; 2];
    for (sibling, candidate) in [(0, 1), (1, 2), (0, 2), (1, 1)] {
        deep_branches[sibling]
            .detach_database(aliases[2])
            .unwrap();
        deep_branches[sibling]
            .attach_database(deep_pins[candidate].clone())
            .unwrap();
        retained_deep[sibling][candidate]
            .get_or_insert_with(|| deep_branches[sibling].database(aliases[2]).unwrap().clone());
        assert_pin(&deep_branches[sibling], aliases[2], &deep_commits[candidate]);
        assert_module_route(
            &deep_branches[sibling],
            &format!("{}.orna", aliases[2]),
            &format!("= {}", marker(2, candidate)),
        );
    }
    let shared_deep_pin = resolver
        .resolve_for_parent(retained_middle[0].as_ref().unwrap().clone())
        .unwrap()
        .database(aliases[2])
        .unwrap()
        .clone();
    assert_pin(&deep_branches[0], aliases[2], &deep_commits[2]);
    assert_pin(&deep_branches[1], aliases[2], &deep_commits[1]);

    let selected_deep: Vec<PinnedDatabase> = (0..2)
        .map(|sibling| deep_branches[sibling].database(aliases[2]).unwrap().clone())
        .collect();
    let mut terminal_branches = [
        resolver.resolve_for_parent(selected_deep[0].clone()).unwrap(),
        resolver.resolve_for_parent(selected_deep[1].clone()).unwrap(),
    ];
    assert_terminal(&terminal_branches[0], 0);
    assert_terminal(&terminal_branches[1], 0);
    let manifest_routes = terminal_branches.clone();
    let mut retained_terminal: Vec<Vec<Option<PinnedDatabase>>> = vec![vec![None; 3]; 2];
    for (sibling, candidate) in [(0, 1), (1, 2), (0, 2), (1, 1)] {
        terminal_branches[sibling]
            .detach_database(aliases[3])
            .unwrap();
        terminal_branches[sibling]
            .attach_database(terminal_pins[candidate].clone())
            .unwrap();
        retained_terminal[sibling][candidate]
            .get_or_insert_with(|| terminal_branches[sibling].database(aliases[3]).unwrap().clone());
        assert_terminal(&terminal_branches[sibling], candidate);
    }
    assert_terminal(&terminal_branches[0], 2);
    assert_terminal(&terminal_branches[1], 1);
    assert_terminal(&manifest_routes[0], 0);
    assert_terminal(&manifest_routes[1], 0);

    for middle_pin in retained_middle.into_iter().flatten() {
        let middle = resolver.resolve_for_parent(middle_pin).unwrap();
        assert_pin(&middle, aliases[2], &shared_deep_commit);
        let terminal = resolver
            .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
            .unwrap();
        assert_terminal(&terminal, 0);
    }
    for pin in selected_deep.into_iter().chain([shared_deep_pin]) {
        let terminal = resolver.resolve_for_parent(pin).unwrap();
        assert_terminal(&terminal, 0);
    }
    for pin in retained_deep.into_iter().flatten().flatten() {
        let terminal = resolver.resolve_for_parent(pin).unwrap();
        assert_terminal(&terminal, 0);
    }
    let retained_terminal_pin = retained_terminal[0][1].as_ref().unwrap().clone();
    let retained = resolver.resolve_for_parent(retained_terminal_pin).unwrap();
    assert_module_route(&retained, "main.orna", "= 401");
}

#[test]
fn reversed_sibling_storm_order_preserves_convergent_terminal_routes() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let branch_count = 2;
    let depth_candidate_count = 2;
    let terminal_candidate_count = 3;
    let marker = |depth: usize, branch: usize, variant: usize| {
        (depth + 1) * 100 + branch * 10 + variant
    };

    let terminal_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, 0, variant)),
                None,
                &format!("order-stable terminal {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..depth_candidate_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} deep {variant} shared terminal"),
                    )
                })
                .collect()
        })
        .collect();
    let shared_deep_commit = deep_commits[0][0].clone();
    let middle_commits: Vec<Vec<String>> = (0..branch_count)
        .map(|branch| {
            (0..depth_candidate_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], shared_deep_commit);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, branch, variant)),
                        Some(&manifest),
                        &format!("branch {branch} middle {variant} shared deep pin"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for reversed sibling storm schedules",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|branch| {
                branch
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let storm_alias = |session: &mut AttachedDatabaseSession,
                       alias: &str,
                       candidates: &[PinnedDatabase],
                       sequence: &[usize]| {
        for candidate in sequence {
            session.detach_database(alias).unwrap();
            session
                .attach_database(candidates[*candidate].clone())
                .unwrap();
        }
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(session.database(alias).unwrap().pin().commit().as_str(), commit);
    };
    let assert_terminal = |session: &AttachedDatabaseSession, variant: usize| {
        assert_pin(session, aliases[3], &terminal_commits[variant]);
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, 0, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut forward_middle = vec![parent.clone(); branch_count];
    let mut reverse_middle = vec![parent.clone(); branch_count];
    let middle_sequences = [[1, 0, 1], [0, 1, 0]];
    for branch in [0, 1] {
        storm_alias(
            &mut forward_middle[branch],
            aliases[1],
            &middle_pins[branch],
            &middle_sequences[branch],
        );
    }
    for branch in [1, 0] {
        storm_alias(
            &mut reverse_middle[branch],
            aliases[1],
            &middle_pins[branch],
            &middle_sequences[branch],
        );
    }
    let final_middle: Vec<PinnedDatabase> = (0..branch_count)
        .map(|branch| forward_middle[branch].database(aliases[1]).unwrap().clone())
        .collect();
    for branch in 0..branch_count {
        assert_pin(
            &reverse_middle[branch],
            aliases[1],
            final_middle[branch].pin().commit().as_str(),
        );
        assert_module_route(&parent, &format!("{}.orna", aliases[1]), "= 200");
    }

    let mut forward_deep: Vec<AttachedDatabaseSession> = final_middle
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let reverse_middle_pins: Vec<PinnedDatabase> = (0..branch_count)
        .map(|branch| reverse_middle[branch].database(aliases[1]).unwrap().clone())
        .collect();
    let mut reverse_deep: Vec<AttachedDatabaseSession> = reverse_middle_pins
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let shared_deep_pin = forward_deep[0].database(aliases[2]).unwrap().clone();
    let forward_deep_snapshots = forward_deep.clone();
    let reverse_deep_snapshots = reverse_deep.clone();
    let deep_sequences = [[1, 0, 1], [0, 1, 0]];
    for branch in [0, 1] {
        storm_alias(
            &mut forward_deep[branch],
            aliases[2],
            &deep_pins[branch],
            &deep_sequences[branch],
        );
    }
    for branch in [1, 0] {
        storm_alias(
            &mut reverse_deep[branch],
            aliases[2],
            &deep_pins[branch],
            &deep_sequences[branch],
        );
    }
    let final_deep: Vec<PinnedDatabase> = (0..branch_count)
        .map(|branch| forward_deep[branch].database(aliases[2]).unwrap().clone())
        .collect();
    for branch in 0..branch_count {
        assert_pin(
            &reverse_deep[branch],
            aliases[2],
            final_deep[branch].pin().commit().as_str(),
        );
        assert_module_route(
            &forward_deep_snapshots[branch],
            &format!("{}.orna", aliases[2]),
            "= 300",
        );
        assert_module_route(
            &reverse_deep_snapshots[branch],
            &format!("{}.orna", aliases[2]),
            "= 300",
        );
    }

    let mut forward_terminal: Vec<AttachedDatabaseSession> = final_deep
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let reverse_deep_pins: Vec<PinnedDatabase> = (0..branch_count)
        .map(|branch| reverse_deep[branch].database(aliases[2]).unwrap().clone())
        .collect();
    let mut reverse_terminal: Vec<AttachedDatabaseSession> = reverse_deep_pins
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    for terminal in forward_terminal.iter().chain(reverse_terminal.iter()) {
        assert_terminal(terminal, 0);
    }
    let forward_manifest_routes = forward_terminal.clone();
    let reverse_manifest_routes = reverse_terminal.clone();
    let terminal_sequences = [[1, 2, 1], [2, 1, 2]];
    for branch in [0, 1] {
        storm_alias(
            &mut forward_terminal[branch],
            aliases[3],
            &terminal_pins,
            &terminal_sequences[branch],
        );
    }
    for branch in [1, 0] {
        storm_alias(
            &mut reverse_terminal[branch],
            aliases[3],
            &terminal_pins,
            &terminal_sequences[branch],
        );
    }
    for (branch, final_variant) in [(0, 1), (1, 2)] {
        assert_terminal(&forward_terminal[branch], final_variant);
        assert_terminal(&reverse_terminal[branch], final_variant);
        assert_terminal(&forward_manifest_routes[branch], 0);
        assert_terminal(&reverse_manifest_routes[branch], 0);
    }
    let reopened_shared_route = resolver.resolve_for_parent(shared_deep_pin).unwrap();
    assert_terminal(&reopened_shared_route, 0);
}

#[test]
fn post_storm_sibling_routes_survive_paired_depth_rebinds() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let marker = |depth: usize, variant: usize| (depth + 1) * 100 + variant;

    let terminal_commits: Vec<String> = (0..3)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, variant)),
                None,
                &format!("convergent terminal {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<String> = (0..3)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(2, variant)),
                Some(&manifest),
                &format!("paired-depth candidate {variant}"),
            )
        })
        .collect();
    let shared_deep_commit = deep_commits[0].clone();
    let middle_commits: Vec<String> = (0..3)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[2], shared_deep_commit);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, variant)),
                Some(&manifest),
                &format!("sibling middle candidate {variant}"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for convergent sibling terminal storms",
    );

    let loader = ProjectLoader::default();
    let resolve_pins = |alias: &str, commits: &[String]| {
        commits
            .iter()
            .map(|commit| {
                PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pins(aliases[1], &middle_commits);
    let deep_pins = resolve_pins(aliases[2], &deep_commits);
    let terminal_pins = resolve_pins(aliases[3], &terminal_commits);
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(session.database(alias).unwrap().pin().commit().as_str(), commit);
    };
    let assert_route = |session: &AttachedDatabaseSession, alias: &str, depth, variant| {
        assert_module_route(
            session,
            &format!("{}.orna", alias),
            &format!("= {}", marker(depth, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut siblings = [parent.clone(), parent.clone()];
    let mut retained_middle = [None, None];
    for (sibling, candidate) in [(0, 1), (1, 2), (1, 1), (0, 2), (1, 2), (0, 1)] {
        siblings[sibling].detach_database(aliases[1]).unwrap();
        siblings[sibling]
            .attach_database(middle_pins[candidate].clone())
            .unwrap();
        retained_middle[sibling]
            .get_or_insert_with(|| siblings[sibling].database(aliases[1]).unwrap().clone());
        assert_pin(
            &siblings[sibling],
            aliases[1],
            &middle_commits[candidate],
        );
        assert_route(&siblings[sibling], aliases[1], 1, candidate);
    }
    assert_route(&parent, aliases[1], 1, 0);

    let selected_middle: Vec<PinnedDatabase> = siblings
        .iter()
        .map(|sibling| sibling.database(aliases[1]).unwrap().clone())
        .collect();
    let mut deep_branches: Vec<AttachedDatabaseSession> = selected_middle
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let retained_shared_deep = deep_branches[0].database(aliases[2]).unwrap().clone();
    let manifest_deep_routes = deep_branches.clone();
    let mut retained_deep = [None, None];
    for (sibling, candidate) in [(0, 1), (1, 2), (1, 1), (0, 2), (1, 0), (0, 0)] {
        deep_branches[sibling]
            .detach_database(aliases[2])
            .unwrap();
        deep_branches[sibling]
            .attach_database(deep_pins[candidate].clone())
            .unwrap();
        retained_deep[sibling]
            .get_or_insert_with(|| deep_branches[sibling].database(aliases[2]).unwrap().clone());
        assert_pin(
            &deep_branches[sibling],
            aliases[2],
            &deep_commits[candidate],
        );
        assert_route(&deep_branches[sibling], aliases[2], 2, candidate);
    }
    for sibling in 0..2 {
        assert_pin(&deep_branches[sibling], aliases[2], &shared_deep_commit);
        assert_route(&deep_branches[sibling], aliases[2], 2, 0);
        assert_pin(&manifest_deep_routes[sibling], aliases[2], &shared_deep_commit);
        assert_route(&manifest_deep_routes[sibling], aliases[2], 2, 0);
    }

    let shared_deep_pins: Vec<PinnedDatabase> = deep_branches
        .iter()
        .map(|branch| branch.database(aliases[2]).unwrap().clone())
        .collect();
    assert_eq!(
        shared_deep_pins[0].pin().commit(),
        shared_deep_pins[1].pin().commit()
    );
    let mut terminal_branches: Vec<AttachedDatabaseSession> = shared_deep_pins
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let manifest_terminal_routes = terminal_branches.clone();
    let mut retained_terminal = [None, None];
    for (sibling, candidate) in [(0, 1), (1, 2), (1, 1), (0, 2), (1, 2)] {
        terminal_branches[sibling]
            .detach_database(aliases[3])
            .unwrap();
        terminal_branches[sibling]
            .attach_database(terminal_pins[candidate].clone())
            .unwrap();
        retained_terminal[sibling]
            .get_or_insert_with(|| terminal_branches[sibling].database(aliases[3]).unwrap().clone());
        assert_pin(
            &terminal_branches[sibling],
            aliases[3],
            &terminal_commits[candidate],
        );
        assert_route(&terminal_branches[sibling], aliases[3], 3, candidate);
    }
    for sibling in 0..2 {
        assert_pin(
            &terminal_branches[sibling],
            aliases[3],
            &terminal_commits[2],
        );
        assert_route(&terminal_branches[sibling], aliases[3], 3, 2);
        assert_route(&manifest_terminal_routes[sibling], aliases[3], 3, 0);
    }

    let fresh_shared_route = resolver
        .resolve_for_parent(retained_shared_deep)
        .unwrap();
    assert_route(&fresh_shared_route, aliases[3], 3, 0);
    for sibling in 0..2 {
        let middle_route = resolver
            .resolve_for_parent(retained_middle[sibling].take().unwrap())
            .unwrap();
        assert_route(&middle_route, aliases[2], 2, 0);
        let retained_deep_route = resolver
            .resolve_for_parent(retained_deep[sibling].take().unwrap())
            .unwrap();
        assert_route(&retained_deep_route, aliases[3], 3, 0);
        let retained_terminal_route = resolver
            .resolve_for_parent(retained_terminal[sibling].take().unwrap())
            .unwrap();
        assert_eq!(retained_terminal_route.attached().count(), 0);
    }

    let mut reopened_terminal_routes: Vec<AttachedDatabaseSession> = selected_middle
        .iter()
        .map(|middle_pin| {
            let reopened_deep = resolver.resolve_for_parent(middle_pin.clone()).unwrap();
            assert_pin(&reopened_deep, aliases[2], &shared_deep_commit);
            assert_route(&reopened_deep, aliases[2], 2, 0);
            resolver
                .resolve_for_parent(reopened_deep.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let post_storm_manifest_routes = reopened_terminal_routes.clone();
    for sibling in 0..2 {
        assert_pin(
            &reopened_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[0],
        );
        assert_route(&reopened_terminal_routes[sibling], aliases[3], 3, 0);
    }

    // A second storm wave on the newly reopened siblings converges on one
    // terminal pin without moving either manifest snapshot or the old wave.
    for (sibling, candidate) in [(0, 2), (1, 1), (1, 2), (0, 1), (1, 1)] {
        reopened_terminal_routes[sibling]
            .detach_database(aliases[3])
            .unwrap();
        reopened_terminal_routes[sibling]
            .attach_database(terminal_pins[candidate].clone())
            .unwrap();
        assert_pin(
            &reopened_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[candidate],
        );
        assert_route(&reopened_terminal_routes[sibling], aliases[3], 3, candidate);
    }
    for sibling in 0..2 {
        assert_pin(
            &reopened_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[1],
        );
        assert_route(&reopened_terminal_routes[sibling], aliases[3], 3, 1);
        assert_route(&post_storm_manifest_routes[sibling], aliases[3], 3, 0);
        assert_pin(
            &terminal_branches[sibling],
            aliases[3],
            &terminal_commits[2],
        );
        assert_route(&terminal_branches[sibling], aliases[3], 3, 2);
    }
    assert_eq!(
        reopened_terminal_routes[0]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        reopened_terminal_routes[1]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
    );

    let previous_middle_routes = siblings.clone();
    for (sibling, candidate) in [(0, 2), (1, 1), (1, 2), (0, 1), (1, 1), (0, 2)] {
        siblings[sibling].detach_database(aliases[1]).unwrap();
        siblings[sibling]
            .attach_database(middle_pins[candidate].clone())
            .unwrap();
        assert_pin(
            &siblings[sibling],
            aliases[1],
            &middle_commits[candidate],
        );
        assert_route(&siblings[sibling], aliases[1], 1, candidate);
    }
    assert_route(&previous_middle_routes[0], aliases[1], 1, 1);
    assert_route(&previous_middle_routes[1], aliases[1], 1, 2);

    let second_wave_middle_pins: Vec<PinnedDatabase> = siblings
        .iter()
        .map(|sibling| sibling.database(aliases[1]).unwrap().clone())
        .collect();
    let mut second_wave_deep_routes: Vec<AttachedDatabaseSession> = second_wave_middle_pins
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let first_wave_deep_routes = second_wave_deep_routes.clone();
    for (sibling, candidate) in [(0, 2), (1, 1), (0, 1), (1, 2)] {
        second_wave_deep_routes[sibling]
            .detach_database(aliases[2])
            .unwrap();
        second_wave_deep_routes[sibling]
            .attach_database(deep_pins[candidate].clone())
            .unwrap();
        assert_pin(
            &second_wave_deep_routes[sibling],
            aliases[2],
            &deep_commits[candidate],
        );
        assert_route(&second_wave_deep_routes[sibling], aliases[2], 2, candidate);
    }
    assert_pin(
        &second_wave_deep_routes[0],
        aliases[2],
        &deep_commits[1],
    );
    assert_pin(
        &second_wave_deep_routes[1],
        aliases[2],
        &deep_commits[2],
    );
    for route in &first_wave_deep_routes {
        assert_pin(route, aliases[2], &shared_deep_commit);
        assert_route(route, aliases[2], 2, 0);
    }

    let second_wave_deep_pins: Vec<PinnedDatabase> = second_wave_deep_routes
        .iter()
        .map(|route| route.database(aliases[2]).unwrap().clone())
        .collect();
    let mut second_wave_terminal_routes: Vec<AttachedDatabaseSession> = second_wave_deep_pins
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let second_wave_manifest_routes = second_wave_terminal_routes.clone();
    for sibling in 0..2 {
        assert_pin(
            &second_wave_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[0],
        );
        assert_route(&second_wave_terminal_routes[sibling], aliases[3], 3, 0);
    }

    // Both new deep pins select the same terminal manifest, so the later
    // terminal rebinds converge again without changing either earlier wave.
    for (sibling, candidate) in [(0, 1), (1, 2), (1, 1), (0, 2), (1, 2)] {
        second_wave_terminal_routes[sibling]
            .detach_database(aliases[3])
            .unwrap();
        second_wave_terminal_routes[sibling]
            .attach_database(terminal_pins[candidate].clone())
            .unwrap();
        assert_pin(
            &second_wave_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[candidate],
        );
        assert_route(&second_wave_terminal_routes[sibling], aliases[3], 3, candidate);
    }
    for sibling in 0..2 {
        assert_pin(
            &second_wave_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[2],
        );
        assert_route(&second_wave_terminal_routes[sibling], aliases[3], 3, 2);
        assert_route(&second_wave_manifest_routes[sibling], aliases[3], 3, 0);
        assert_pin(
            &terminal_branches[sibling],
            aliases[3],
            &terminal_commits[2],
        );
        assert_route(&terminal_branches[sibling], aliases[3], 3, 2);
        assert_pin(
            &reopened_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[1],
        );
        assert_route(&reopened_terminal_routes[sibling], aliases[3], 3, 1);
    }
    assert_eq!(
        second_wave_terminal_routes[0]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        second_wave_terminal_routes[1]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
    );
}

#[test]
fn sibling_terminal_rebinds_survive_later_paired_depth_storms() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let marker = |depth: usize, variant: usize| (depth + 1) * 100 + variant;

    let terminal_commits: Vec<String> = (0..3)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, variant)),
                None,
                &format!("cross-depth terminal {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<String> = (0..3)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(2, variant)),
                Some(&manifest),
                &format!("cross-depth deep candidate {variant}"),
            )
        })
        .collect();
    let shared_deep_commit = deep_commits[0].clone();
    let middle_commits: Vec<String> = (0..3)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[2], shared_deep_commit);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, variant)),
                Some(&manifest),
                &format!("cross-depth middle candidate {variant}"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for cross-depth sibling storms",
    );

    let loader = ProjectLoader::default();
    let resolve_pins = |alias: &str, commits: &[String]| {
        commits
            .iter()
            .map(|commit| {
                PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pins(aliases[1], &middle_commits);
    let deep_pins = resolve_pins(aliases[2], &deep_commits);
    let terminal_pins = resolve_pins(aliases[3], &terminal_commits);
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let rebind = |session: &mut AttachedDatabaseSession, alias: &str, pin: &PinnedDatabase| {
        session.detach_database(alias).unwrap();
        session.attach_database(pin.clone()).unwrap();
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };
    let assert_route = |session: &AttachedDatabaseSession, alias: &str, depth, variant| {
        assert_module_route(
            session,
            &format!("{}.orna", alias),
            &format!("= {}", marker(depth, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut siblings = [parent.clone(), parent.clone()];
    rebind(&mut siblings[0], aliases[1], &middle_pins[1]);
    rebind(&mut siblings[1], aliases[1], &middle_pins[2]);
    let first_middle_snapshots = siblings.clone();
    let mut deep_routes: Vec<AttachedDatabaseSession> = siblings
        .iter()
        .map(|sibling| {
            resolver
                .resolve_for_parent(sibling.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let first_deep_snapshots = deep_routes.clone();
    for (sibling, candidate) in [(0, 1), (1, 2), (0, 2), (1, 1)] {
        rebind(&mut deep_routes[sibling], aliases[2], &deep_pins[candidate]);
        assert_pin(&deep_routes[sibling], aliases[2], &deep_commits[candidate]);
        assert_route(&deep_routes[sibling], aliases[2], 2, candidate);
    }
    assert_pin(&deep_routes[0], aliases[2], &deep_commits[2]);
    assert_pin(&deep_routes[1], aliases[2], &deep_commits[1]);
    for snapshot in &first_deep_snapshots {
        assert_pin(snapshot, aliases[2], &shared_deep_commit);
        assert_route(snapshot, aliases[2], 2, 0);
    }

    let selected_deep: Vec<PinnedDatabase> = deep_routes
        .iter()
        .map(|route| route.database(aliases[2]).unwrap().clone())
        .collect();
    let mut terminal_routes: Vec<AttachedDatabaseSession> = selected_deep
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let first_terminal_snapshots = terminal_routes.clone();
    for (sibling, candidate) in [(0, 1), (1, 2), (1, 1), (0, 2)] {
        rebind(
            &mut terminal_routes[sibling],
            aliases[3],
            &terminal_pins[candidate],
        );
        assert_pin(
            &terminal_routes[sibling],
            aliases[3],
            &terminal_commits[candidate],
        );
        assert_route(&terminal_routes[sibling], aliases[3], 3, candidate);
    }
    assert_pin(&terminal_routes[0], aliases[3], &terminal_commits[2]);
    assert_pin(&terminal_routes[1], aliases[3], &terminal_commits[1]);
    let first_terminal_wave_routes = terminal_routes.clone();
    for snapshot in &first_terminal_snapshots {
        assert_pin(snapshot, aliases[3], &terminal_commits[0]);
        assert_route(snapshot, aliases[3], 3, 0);
    }

    // A's terminal storm brackets B's later paired-depth rebind wave.
    rebind(&mut terminal_routes[0], aliases[3], &terminal_pins[0]);
    let mut later_middle = siblings[1].clone();
    rebind(&mut later_middle, aliases[1], &middle_pins[0]);
    rebind(&mut terminal_routes[0], aliases[3], &terminal_pins[2]);
    rebind(&mut later_middle, aliases[1], &middle_pins[1]);
    assert_pin(&siblings[1], aliases[1], &middle_commits[2]);
    assert_route(&first_middle_snapshots[1], aliases[1], 1, 2);

    let mut later_deep = resolver
        .resolve_for_parent(later_middle.database(aliases[1]).unwrap().clone())
        .unwrap();
    assert_pin(&later_deep, aliases[2], &shared_deep_commit);
    let post_storm_deep_snapshot = later_deep.clone();
    rebind(&mut later_deep, aliases[2], &deep_pins[2]);
    assert_pin(&later_deep, aliases[2], &deep_commits[2]);
    assert_pin(&post_storm_deep_snapshot, aliases[2], &shared_deep_commit);
    assert_route(&post_storm_deep_snapshot, aliases[2], 2, 0);

    rebind(&mut terminal_routes[0], aliases[3], &terminal_pins[1]);
    let mut later_terminal = resolver
        .resolve_for_parent(later_deep.database(aliases[2]).unwrap().clone())
        .unwrap();
    assert_pin(&later_terminal, aliases[3], &terminal_commits[0]);
    let post_storm_terminal_snapshot = later_terminal.clone();
    rebind(&mut later_terminal, aliases[3], &terminal_pins[2]);
    rebind(&mut terminal_routes[0], aliases[3], &terminal_pins[2]);
    rebind(&mut later_terminal, aliases[3], &terminal_pins[1]);
    rebind(&mut terminal_routes[0], aliases[3], &terminal_pins[1]);

    assert_pin(&terminal_routes[0], aliases[3], &terminal_commits[1]);
    assert_pin(&later_terminal, aliases[3], &terminal_commits[1]);
    assert_route(&terminal_routes[0], aliases[3], 3, 1);
    assert_route(&later_terminal, aliases[3], 3, 1);
    assert_eq!(
        terminal_routes[0]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        later_terminal.database(aliases[3]).unwrap().pin().commit()
    );
    assert_pin(&terminal_routes[1], aliases[3], &terminal_commits[1]);
    assert_route(&terminal_routes[1], aliases[3], 3, 1);
    assert_pin(
        &first_terminal_wave_routes[0],
        aliases[3],
        &terminal_commits[2],
    );
    assert_route(&first_terminal_wave_routes[0], aliases[3], 3, 2);
    assert_pin(
        &first_terminal_wave_routes[1],
        aliases[3],
        &terminal_commits[1],
    );
    assert_route(&first_terminal_wave_routes[1], aliases[3], 3, 1);
    assert_pin(
        &first_terminal_snapshots[0],
        aliases[3],
        &terminal_commits[0],
    );
    assert_pin(
        &first_terminal_snapshots[1],
        aliases[3],
        &terminal_commits[0],
    );
    assert_route(&post_storm_terminal_snapshot, aliases[3], 3, 0);
    assert_pin(&parent, aliases[1], &middle_commits[0]);
    assert_route(&parent, aliases[1], 1, 0);
}

#[test]
fn post_storm_paired_closures_preserve_three_sibling_terminal_routes() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 3;
    let variants = 3;
    let marker =
        |depth: usize, sibling: usize, variant: usize| (depth + 1) * 100 + sibling * 10 + variant;

    let terminal_commits: Vec<String> = (0..variants)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, 0, variant)),
                None,
                &format!("post-storm shared terminal {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..variants)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, sibling, variant)),
                        Some(&manifest),
                        &format!("post-storm deep sibling {sibling} variant {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..variants)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], deep_commits[sibling][0]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, sibling, variant)),
                        Some(&manifest),
                        &format!("post-storm middle sibling {sibling} variant {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for post-storm sibling closure wave",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|sibling| {
                sibling
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let rebind = |session: &mut AttachedDatabaseSession, alias: &str, pin: &PinnedDatabase| {
        session.detach_database(alias).unwrap();
        session.attach_database(pin.clone()).unwrap();
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };
    let assert_route =
        |session: &AttachedDatabaseSession, depth: usize, sibling: usize, variant: usize| {
            assert_module_route(
                session,
                &format!("{}.orna", aliases[depth]),
                &format!("= {}", marker(depth, sibling, variant)),
            );
        };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut sibling_roots = vec![parent.clone(); sibling_count];
    let initial_middle = [1, 2, 0];
    for sibling in 0..sibling_count {
        rebind(
            &mut sibling_roots[sibling],
            aliases[1],
            &middle_pins[sibling][initial_middle[sibling]],
        );
        assert_route(&sibling_roots[sibling], 1, sibling, initial_middle[sibling]);
    }
    let pre_paired_roots = sibling_roots.clone();

    let mut initial_terminal_closures: Vec<AttachedDatabaseSession> = sibling_roots
        .iter()
        .map(|root| {
            resolver
                .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .map(|middle| {
            resolver
                .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let retained_initial_terminals = initial_terminal_closures.clone();
    for (sibling, variant) in [(0, 2), (1, 1), (2, 2), (0, 1), (1, 2), (2, 1)] {
        rebind(
            &mut initial_terminal_closures[sibling],
            aliases[3],
            &terminal_pins[variant],
        );
        assert_pin(
            &initial_terminal_closures[sibling],
            aliases[3],
            &terminal_commits[variant],
        );
        assert_route(&initial_terminal_closures[sibling], 3, 0, variant);
    }
    let retained_storm_terminals = initial_terminal_closures.clone();

    // Reopen each middle closure after terminal storms, then rebind its deep
    // edge before resolving and rebinding the terminal route again.
    let paired_middle = [2, 0, 1];
    for sibling in 0..sibling_count {
        rebind(
            &mut sibling_roots[sibling],
            aliases[1],
            &middle_pins[sibling][paired_middle[sibling]],
        );
        assert_pin(
            &sibling_roots[sibling],
            aliases[1],
            &middle_commits[sibling][paired_middle[sibling]],
        );
        assert_route(&sibling_roots[sibling], 1, sibling, paired_middle[sibling]);
    }

    let mut reopened_deep: Vec<AttachedDatabaseSession> = sibling_roots
        .iter()
        .map(|root| {
            resolver
                .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let reopened_deep_snapshots = reopened_deep.clone();
    let paired_deep = [2, 1, 2];
    for sibling in 0..sibling_count {
        assert_pin(
            &reopened_deep[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        rebind(
            &mut reopened_deep[sibling],
            aliases[2],
            &deep_pins[sibling][paired_deep[sibling]],
        );
        assert_pin(
            &reopened_deep[sibling],
            aliases[2],
            &deep_commits[sibling][paired_deep[sibling]],
        );
        assert_route(&reopened_deep[sibling], 2, sibling, paired_deep[sibling]);
    }

    let mut reopened_terminals: Vec<AttachedDatabaseSession> = reopened_deep
        .iter()
        .map(|deep| {
            resolver
                .resolve_for_parent(deep.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let reopened_terminal_snapshots = reopened_terminals.clone();
    for terminal in &reopened_terminals {
        assert_pin(terminal, aliases[3], &terminal_commits[0]);
        assert_route(terminal, 3, 0, 0);
    }
    for (sibling, variant) in [(2, 2), (0, 1), (1, 2), (2, 1), (0, 1), (1, 1)] {
        rebind(
            &mut reopened_terminals[sibling],
            aliases[3],
            &terminal_pins[variant],
        );
        assert_pin(
            &reopened_terminals[sibling],
            aliases[3],
            &terminal_commits[variant],
        );
        assert_route(&reopened_terminals[sibling], 3, 0, variant);
    }

    for sibling in 0..sibling_count {
        // Each reopened sibling converges on terminal 1, independently of
        // the earlier storm's last selection.
        assert_pin(
            &reopened_terminals[sibling],
            aliases[3],
            &terminal_commits[1],
        );
        assert_route(&reopened_terminals[sibling], 3, 0, 1);
        assert_pin(
            &retained_storm_terminals[sibling],
            aliases[3],
            &terminal_commits[[1, 2, 1][sibling]],
        );
        assert_route(&retained_storm_terminals[sibling], 3, 0, [1, 2, 1][sibling]);
        assert_pin(
            &retained_initial_terminals[sibling],
            aliases[3],
            &terminal_commits[0],
        );
        assert_pin(
            &pre_paired_roots[sibling],
            aliases[1],
            &middle_commits[sibling][initial_middle[sibling]],
        );
        assert_pin(
            &reopened_deep_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        assert_pin(
            &reopened_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[0],
        );
    }
    assert_eq!(
        reopened_terminals[0]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        reopened_terminals[1]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
    );
    assert_eq!(
        reopened_terminals[1]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        reopened_terminals[2]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
    );
    assert_pin(&parent, aliases[1], &middle_commits[0][0]);
}

#[test]
fn post_storm_paired_depth_rebinds_follow_each_sibling_manifest() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 2;
    let marker =
        |depth: usize, sibling: usize, variant: usize| (depth + 1) * 100 + sibling * 10 + variant;

    let terminal_commits: Vec<String> = (0..sibling_count)
        .map(|sibling| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, sibling, sibling)),
                None,
                &format!("sibling-specific terminal {sibling}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..2)
                .map(|variant| {
                    let terminal = if variant == 0 { sibling } else { 1 - sibling };
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[terminal]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, sibling, variant)),
                        Some(&manifest),
                        &format!("sibling {sibling} deep route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..2)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], deep_commits[sibling][variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, sibling, variant)),
                        Some(&manifest),
                        &format!("sibling {sibling} middle route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for diverging post-storm sibling routes",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|sibling| {
                sibling
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let rebind = |session: &mut AttachedDatabaseSession, alias: &str, pin: &PinnedDatabase| {
        session.detach_database(alias).unwrap();
        session.attach_database(pin.clone()).unwrap();
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };
    let assert_route =
        |session: &AttachedDatabaseSession, depth: usize, sibling: usize, variant: usize| {
            assert_module_route(
                session,
                &format!("{}.orna", aliases[depth]),
                &format!("= {}", marker(depth, sibling, variant)),
            );
        };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut sibling_roots = vec![parent.clone(); sibling_count];
    rebind(&mut sibling_roots[1], aliases[1], &middle_pins[1][0]);
    for sibling in 0..sibling_count {
        assert_pin(
            &sibling_roots[sibling],
            aliases[1],
            &middle_commits[sibling][0],
        );
        assert_route(&sibling_roots[sibling], 1, sibling, 0);
    }
    let pre_storm_roots = sibling_roots.clone();

    let old_deep_closures: Vec<AttachedDatabaseSession> = sibling_roots
        .iter()
        .map(|root| {
            resolver
                .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let old_deep_snapshots = old_deep_closures.clone();
    let mut old_terminal_routes: Vec<AttachedDatabaseSession> = old_deep_closures
        .iter()
        .map(|deep| {
            resolver
                .resolve_for_parent(deep.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let pre_storm_terminal_snapshots = old_terminal_routes.clone();
    for sibling in 0..sibling_count {
        assert_pin(
            &old_deep_closures[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        assert_route(&old_deep_closures[sibling], 2, sibling, 0);
        assert_pin(
            &old_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
    }

    // Earlier terminal storms choose routes opposite their sibling manifests.
    rebind(&mut old_terminal_routes[0], aliases[3], &terminal_pins[1]);
    rebind(&mut old_terminal_routes[1], aliases[3], &terminal_pins[0]);
    assert_pin(&old_terminal_routes[0], aliases[3], &terminal_commits[1]);
    assert_pin(&old_terminal_routes[1], aliases[3], &terminal_commits[0]);
    assert_route(&old_terminal_routes[0], 3, 1, 1);
    assert_route(&old_terminal_routes[1], 3, 0, 0);

    // The later middle rebinds select different deep pins by manifest.
    rebind(&mut sibling_roots[1], aliases[1], &middle_pins[1][1]);
    rebind(&mut sibling_roots[0], aliases[1], &middle_pins[0][1]);
    for sibling in 0..sibling_count {
        assert_pin(
            &sibling_roots[sibling],
            aliases[1],
            &middle_commits[sibling][1],
        );
        assert_route(&sibling_roots[sibling], 1, sibling, 1);
    }
    let mut reopened_deep_closures: Vec<AttachedDatabaseSession> = sibling_roots
        .iter()
        .map(|root| {
            resolver
                .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let selected_manifest_deep_snapshots = reopened_deep_closures.clone();
    for sibling in 0..sibling_count {
        assert_pin(
            &reopened_deep_closures[sibling],
            aliases[2],
            &deep_commits[sibling][1],
        );
        assert_route(&reopened_deep_closures[sibling], 2, sibling, 1);
    }

    // Rebinding the selected deep edge changes only that reopened closure.
    rebind(&mut reopened_deep_closures[0], aliases[2], &deep_pins[0][0]);
    rebind(&mut reopened_deep_closures[1], aliases[2], &deep_pins[1][0]);
    let final_terminal_routes: Vec<AttachedDatabaseSession> = reopened_deep_closures
        .iter()
        .map(|deep| {
            resolver
                .resolve_for_parent(deep.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();

    for sibling in 0..sibling_count {
        assert_pin(
            &reopened_deep_closures[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        assert_route(&reopened_deep_closures[sibling], 2, sibling, 0);
        assert_pin(
            &selected_manifest_deep_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][1],
        );
        assert_pin(
            &final_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
        assert_route(&final_terminal_routes[sibling], 3, sibling, sibling);
        assert_pin(
            &pre_storm_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
        assert_pin(
            &old_terminal_routes[sibling],
            aliases[3],
            &terminal_commits[1 - sibling],
        );
        assert_pin(
            &old_deep_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        assert_pin(
            &pre_storm_roots[sibling],
            aliases[1],
            &middle_commits[sibling][0],
        );
    }
    assert_ne!(
        final_terminal_routes[0]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        final_terminal_routes[1]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
    );
    assert_pin(&parent, aliases[1], &middle_commits[0][0]);
}

#[test]
fn post_storm_sibling_sessions_keep_pin_and_module_routes_consistent() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 2;
    let marker =
        |depth: usize, sibling: usize, variant: usize| (depth + 1) * 100 + sibling * 10 + variant;

    let terminal_commits: Vec<String> = (0..sibling_count)
        .map(|sibling| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, sibling, sibling)),
                None,
                &format!("consistent sibling terminal {sibling}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..2)
                .map(|variant| {
                    let terminal = if variant == 0 { sibling } else { 1 - sibling };
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[terminal]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, sibling, variant)),
                        Some(&manifest),
                        &format!("consistent sibling {sibling} deep {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..2)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], deep_commits[sibling][variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, sibling, variant)),
                        Some(&manifest),
                        &format!("consistent sibling {sibling} middle {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for route-consistent sibling sessions",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|sibling| {
                sibling
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let rebind = |session: &mut AttachedDatabaseSession, alias: &str, pin: &PinnedDatabase| {
        session.detach_database(alias).unwrap();
        session.attach_database(pin.clone()).unwrap();
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };
    let assert_route = |session: &AttachedDatabaseSession,
                        path: &str,
                        depth: usize,
                        sibling: usize,
                        variant: usize| {
        assert_module_route(
            session,
            path,
            &format!("= {}", marker(depth, sibling, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut sibling_roots = vec![parent.clone(); sibling_count];
    rebind(&mut sibling_roots[1], aliases[1], &middle_pins[1][0]);
    let pre_storm_roots = sibling_roots.clone();
    for sibling in 0..sibling_count {
        assert_pin(
            &sibling_roots[sibling],
            aliases[1],
            &middle_commits[sibling][0],
        );
        assert_route(
            &sibling_roots[sibling],
            &format!("{}.orna", aliases[1]),
            1,
            sibling,
            0,
        );
    }

    let old_middle_closures: Vec<AttachedDatabaseSession> = sibling_roots
        .iter()
        .map(|root| {
            resolver
                .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let old_middle_snapshots = old_middle_closures.clone();
    let mut old_terminal_closures: Vec<AttachedDatabaseSession> = old_middle_closures
        .iter()
        .map(|middle| {
            resolver
                .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let old_terminal_snapshots = old_terminal_closures.clone();

    for (sibling, candidate) in [(0, 1), (1, 0), (0, 0), (1, 1), (1, 0), (0, 1)] {
        rebind(
            &mut old_terminal_closures[sibling],
            aliases[3],
            &terminal_pins[candidate],
        );
    }
    for sibling in 0..sibling_count {
        assert_pin(
            &old_terminal_closures[sibling],
            aliases[3],
            &terminal_commits[1 - sibling],
        );
        assert_route(
            &old_terminal_closures[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            1 - sibling,
            1 - sibling,
        );
    }

    for sibling in [1, 0] {
        rebind(
            &mut sibling_roots[sibling],
            aliases[1],
            &middle_pins[sibling][1],
        );
    }
    for sibling in 0..sibling_count {
        assert_pin(
            &sibling_roots[sibling],
            aliases[1],
            &middle_commits[sibling][1],
        );
        assert_route(
            &sibling_roots[sibling],
            &format!("{}.orna", aliases[1]),
            1,
            sibling,
            1,
        );
    }
    let mut reopened_middle_closures: Vec<AttachedDatabaseSession> = sibling_roots
        .iter()
        .map(|root| {
            resolver
                .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                .unwrap()
        })
        .collect();
    let manifest_deep_snapshots = reopened_middle_closures.clone();
    for sibling in 0..sibling_count {
        assert_pin(
            &reopened_middle_closures[sibling],
            aliases[2],
            &deep_commits[sibling][1],
        );
        assert_route(
            &reopened_middle_closures[sibling],
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            1,
        );
    }
    for sibling in [1, 0] {
        rebind(
            &mut reopened_middle_closures[sibling],
            aliases[2],
            &deep_pins[sibling][0],
        );
    }
    let new_terminal_closures: Vec<AttachedDatabaseSession> = reopened_middle_closures
        .iter()
        .map(|middle| {
            resolver
                .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
                .unwrap()
        })
        .collect();

    for sibling in 0..sibling_count {
        let middle_route = format!("{}.orna", aliases[1]);
        let deep_route = format!("{}.orna", aliases[2]);
        let terminal_route = format!("{}.orna", aliases[3]);
        assert_pin(
            &pre_storm_roots[sibling],
            aliases[1],
            &middle_commits[sibling][0],
        );
        assert_route(&pre_storm_roots[sibling], &middle_route, 1, sibling, 0);
        assert_pin(
            &old_middle_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        assert_route(&old_middle_snapshots[sibling], "main.orna", 1, sibling, 0);
        assert_route(&old_middle_snapshots[sibling], &deep_route, 2, sibling, 0);
        assert_pin(
            &old_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
        assert_route(&old_terminal_snapshots[sibling], "main.orna", 2, sibling, 0);
        assert_route(
            &old_terminal_snapshots[sibling],
            &terminal_route,
            3,
            sibling,
            sibling,
        );
        assert_pin(
            &reopened_middle_closures[sibling],
            aliases[2],
            &deep_commits[sibling][0],
        );
        assert_route(
            &reopened_middle_closures[sibling],
            &deep_route,
            2,
            sibling,
            0,
        );
        assert_pin(
            &manifest_deep_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][1],
        );
        assert_route(
            &manifest_deep_snapshots[sibling],
            &deep_route,
            2,
            sibling,
            1,
        );
        assert_pin(
            &new_terminal_closures[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
        assert_route(&new_terminal_closures[sibling], "main.orna", 2, sibling, 0);
        assert_route(
            &new_terminal_closures[sibling],
            &terminal_route,
            3,
            sibling,
            sibling,
        );
    }
    assert_pin(&parent, aliases[1], &middle_commits[0][0]);
}

#[test]
fn interleaved_paired_depth_rebinds_keep_sibling_routes_consistent() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 2;
    let marker =
        |depth: usize, sibling: usize, variant: usize| (depth + 1) * 100 + sibling * 10 + variant;

    let terminal_commits: Vec<String> = (0..sibling_count)
        .map(|sibling| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, sibling, sibling)),
                None,
                &format!("ordered sibling terminal {sibling}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..2)
                .map(|variant| {
                    let terminal = if variant == 0 { sibling } else { 1 - sibling };
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[terminal]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, sibling, variant)),
                        Some(&manifest),
                        &format!("ordered sibling {sibling} deep {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..2)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], deep_commits[sibling][variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, sibling, variant)),
                        Some(&manifest),
                        &format!("ordered sibling {sibling} middle {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for interleaved sibling depth waves",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|sibling| {
                sibling
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let rebind = |session: &mut AttachedDatabaseSession, alias: &str, pin: &PinnedDatabase| {
        session.detach_database(alias).unwrap();
        session.attach_database(pin.clone()).unwrap();
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };
    let assert_route = |session: &AttachedDatabaseSession,
                        path: &str,
                        depth: usize,
                        sibling: usize,
                        variant: usize| {
        assert_module_route(
            session,
            path,
            &format!("= {}", marker(depth, sibling, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let make_roots = || {
        let mut roots = vec![parent.clone(); sibling_count];
        rebind(&mut roots[1], aliases[1], &middle_pins[1][0]);
        roots
    };
    let open_terminal_routes = |roots: &[AttachedDatabaseSession]| {
        roots
            .iter()
            .map(|root| {
                let middle = resolver
                    .resolve_for_parent(root.database(aliases[1]).unwrap().clone())
                    .unwrap();
                resolver
                    .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let storm_terminals = |terminals: &mut [AttachedDatabaseSession]| {
        for (sibling, candidate) in [(0, 1), (1, 0), (0, 0), (1, 1), (0, 1), (1, 0)] {
            rebind(
                &mut terminals[sibling],
                aliases[3],
                &terminal_pins[candidate],
            );
            assert_pin(
                &terminals[sibling],
                aliases[3],
                &terminal_commits[candidate],
            );
            assert_route(
                &terminals[sibling],
                &format!("{}.orna", aliases[3]),
                3,
                candidate,
                candidate,
            );
        }
    };
    let apply_paired_wave = |roots: &mut [AttachedDatabaseSession], order: [usize; 2]| {
        let mut terminal_routes: Vec<Option<AttachedDatabaseSession>> = vec![None; sibling_count];
        let mut manifest_deep_snapshots: Vec<Option<AttachedDatabaseSession>> =
            vec![None; sibling_count];
        for sibling in order {
            rebind(&mut roots[sibling], aliases[1], &middle_pins[sibling][1]);
            let mut deep = resolver
                .resolve_for_parent(roots[sibling].database(aliases[1]).unwrap().clone())
                .unwrap();
            assert_pin(&deep, aliases[2], &deep_commits[sibling][1]);
            assert_route(&deep, &format!("{}.orna", aliases[2]), 2, sibling, 1);
            manifest_deep_snapshots[sibling] = Some(deep.clone());
            rebind(&mut deep, aliases[2], &deep_pins[sibling][0]);
            assert_pin(&deep, aliases[2], &deep_commits[sibling][0]);
            assert_route(&deep, &format!("{}.orna", aliases[2]), 2, sibling, 0);
            let terminal = resolver
                .resolve_for_parent(deep.database(aliases[2]).unwrap().clone())
                .unwrap();
            assert_pin(&terminal, aliases[3], &terminal_commits[sibling]);
            assert_route(&terminal, "main.orna", 2, sibling, 0);
            assert_route(
                &terminal,
                &format!("{}.orna", aliases[3]),
                3,
                sibling,
                sibling,
            );
            terminal_routes[sibling] = Some(terminal);
        }
        (
            terminal_routes
                .into_iter()
                .map(Option::unwrap)
                .collect::<Vec<_>>(),
            manifest_deep_snapshots
                .into_iter()
                .map(Option::unwrap)
                .collect::<Vec<_>>(),
        )
    };

    let mut forward_roots = make_roots();
    let mut reverse_roots = make_roots();
    let forward_root_snapshots = forward_roots.clone();
    let reverse_root_snapshots = reverse_roots.clone();
    let mut forward_old_terminals = open_terminal_routes(&forward_roots);
    let mut reverse_old_terminals = open_terminal_routes(&reverse_roots);
    let forward_terminal_snapshots = forward_old_terminals.clone();
    let reverse_terminal_snapshots = reverse_old_terminals.clone();
    storm_terminals(&mut forward_old_terminals);
    storm_terminals(&mut reverse_old_terminals);

    let (forward_final, forward_manifest_snapshots) = apply_paired_wave(&mut forward_roots, [0, 1]);
    let (reverse_final, reverse_manifest_snapshots) = apply_paired_wave(&mut reverse_roots, [1, 0]);

    for sibling in 0..sibling_count {
        let root_route = format!("{}.orna", aliases[1]);
        let deep_route = format!("{}.orna", aliases[2]);
        let terminal_route = format!("{}.orna", aliases[3]);
        let selected_middle = &middle_commits[sibling][1];
        let selected_terminal = &terminal_commits[sibling];

        assert_pin(&forward_roots[sibling], aliases[1], selected_middle);
        assert_pin(&reverse_roots[sibling], aliases[1], selected_middle);
        assert_route(&forward_roots[sibling], &root_route, 1, sibling, 1);
        assert_route(&reverse_roots[sibling], &root_route, 1, sibling, 1);
        assert_pin(
            &forward_root_snapshots[sibling],
            aliases[1],
            &middle_commits[sibling][0],
        );
        assert_pin(
            &reverse_root_snapshots[sibling],
            aliases[1],
            &middle_commits[sibling][0],
        );
        assert_route(&forward_root_snapshots[sibling], &root_route, 1, sibling, 0);
        assert_route(&reverse_root_snapshots[sibling], &root_route, 1, sibling, 0);
        assert_pin(&forward_final[sibling], aliases[3], selected_terminal);
        assert_pin(&reverse_final[sibling], aliases[3], selected_terminal);
        assert_eq!(
            forward_final[sibling]
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit(),
            reverse_final[sibling]
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
        );
        assert_route(
            &forward_final[sibling],
            &terminal_route,
            3,
            sibling,
            sibling,
        );
        assert_route(
            &reverse_final[sibling],
            &terminal_route,
            3,
            sibling,
            sibling,
        );
        assert_pin(
            &forward_manifest_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][1],
        );
        assert_pin(
            &reverse_manifest_snapshots[sibling],
            aliases[2],
            &deep_commits[sibling][1],
        );
        assert_route(
            &forward_manifest_snapshots[sibling],
            &deep_route,
            2,
            sibling,
            1,
        );
        assert_route(
            &reverse_manifest_snapshots[sibling],
            &deep_route,
            2,
            sibling,
            1,
        );
        assert_pin(
            &forward_old_terminals[sibling],
            aliases[3],
            &terminal_commits[1 - sibling],
        );
        assert_route(
            &forward_old_terminals[sibling],
            &terminal_route,
            3,
            1 - sibling,
            1 - sibling,
        );
        assert_pin(
            &reverse_old_terminals[sibling],
            aliases[3],
            &terminal_commits[1 - sibling],
        );
        assert_route(
            &reverse_old_terminals[sibling],
            &terminal_route,
            3,
            1 - sibling,
            1 - sibling,
        );
        assert_pin(
            &forward_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
        assert_route(
            &forward_terminal_snapshots[sibling],
            &terminal_route,
            3,
            sibling,
            sibling,
        );
        assert_pin(
            &reverse_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[sibling],
        );
        assert_route(
            &reverse_terminal_snapshots[sibling],
            &terminal_route,
            3,
            sibling,
            sibling,
        );
    }
    assert_ne!(
        forward_final[0]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit(),
        forward_final[1]
            .database(aliases[3])
            .unwrap()
            .pin()
            .commit()
    );
    assert_pin(&parent, aliases[1], &middle_commits[0][0]);
}

#[test]
fn three_sibling_depth_storms_keep_convergent_routes_stable() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 3;
    let candidate_count = 3;
    let terminal_candidate_count = 4;
    let marker = |depth: usize, sibling: usize, variant: usize| {
        (depth + 1) * 100 + sibling * 10 + variant
    };

    let terminal_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, 0, variant)),
                None,
                &format!("three sibling terminal {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..candidate_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, sibling, variant)),
                        Some(&manifest),
                        &format!("sibling {sibling} deep candidate {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let shared_deep_commit = deep_commits[0][0].clone();
    let middle_commits: Vec<String> = (0..sibling_count)
        .map(|sibling| {
            let manifest = format!("{} {}\n", aliases[2], shared_deep_commit);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, sibling, sibling)),
                Some(&manifest),
                &format!("sibling middle {sibling} converges on shared deep pin"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for three convergent sibling routes",
    );

    let loader = ProjectLoader::default();
    let middle_pins: Vec<PinnedDatabase> = middle_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[1], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|sibling| {
                sibling
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(session.database(alias).unwrap().pin().commit().as_str(), commit);
    };
    let assert_terminal = |session: &AttachedDatabaseSession, variant: usize| {
        assert_pin(session, aliases[3], &terminal_commits[variant]);
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, 0, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut siblings = vec![parent.clone(); sibling_count];
    let first_middle_variant = [1, 2, 1];
    let mut retained_middle: Vec<Option<PinnedDatabase>> = vec![None; sibling_count];
    for (sibling, variant) in [(0, 1), (1, 2), (2, 1), (0, 0), (1, 1), (2, 2)] {
        siblings[sibling].detach_database(aliases[1]).unwrap();
        siblings[sibling]
            .attach_database(middle_pins[variant].clone())
            .unwrap();
        retained_middle[sibling]
            .get_or_insert_with(|| siblings[sibling].database(aliases[1]).unwrap().clone());
        assert_module_route(
            &siblings[sibling],
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, variant, variant)),
        );
    }
    assert_module_route(&parent, &format!("{}.orna", aliases[1]), "= 200");

    let selected_middle: Vec<PinnedDatabase> = (0..sibling_count)
        .map(|sibling| siblings[sibling].database(aliases[1]).unwrap().clone())
        .collect();
    let mut deep_branches: Vec<AttachedDatabaseSession> = selected_middle
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    let retained_shared_deep = deep_branches[0].database(aliases[2]).unwrap().clone();
    let mut retained_deep: Vec<Vec<Option<PinnedDatabase>>> =
        vec![vec![None; candidate_count]; sibling_count];
    let deep_storms = [(0, 2), (1, 0), (2, 1), (0, 1), (1, 2), (2, 0)];
    for (sibling, variant) in deep_storms {
        deep_branches[sibling]
            .detach_database(aliases[2])
            .unwrap();
        deep_branches[sibling]
            .attach_database(deep_pins[sibling][variant].clone())
            .unwrap();
        retained_deep[sibling][variant]
            .get_or_insert_with(|| deep_branches[sibling].database(aliases[2]).unwrap().clone());
        assert_pin(
            &deep_branches[sibling],
            aliases[2],
            &deep_commits[sibling][variant],
        );
        assert_module_route(
            &deep_branches[sibling],
            &format!("{}.orna", aliases[2]),
            &format!("= {}", marker(2, sibling, variant)),
        );
    }
    assert_pin(&deep_branches[0], aliases[2], &deep_commits[0][1]);
    assert_pin(&deep_branches[1], aliases[2], &deep_commits[1][2]);
    assert_pin(&deep_branches[2], aliases[2], &deep_commits[2][0]);

    let selected_deep: Vec<PinnedDatabase> = (0..sibling_count)
        .map(|sibling| deep_branches[sibling].database(aliases[2]).unwrap().clone())
        .collect();
    let mut terminal_branches: Vec<AttachedDatabaseSession> = selected_deep
        .iter()
        .map(|pin| resolver.resolve_for_parent(pin.clone()).unwrap())
        .collect();
    for terminal in &terminal_branches {
        assert_terminal(terminal, 0);
    }
    let manifest_routes = terminal_branches.clone();
    let terminal_storms = [(0, 1), (1, 2), (2, 3), (1, 1), (0, 3), (2, 2)];
    for (sibling, variant) in terminal_storms {
        terminal_branches[sibling]
            .detach_database(aliases[3])
            .unwrap();
        terminal_branches[sibling]
            .attach_database(terminal_pins[variant].clone())
            .unwrap();
        assert_terminal(&terminal_branches[sibling], variant);
    }
    for (sibling, final_variant) in [(0, 3), (1, 1), (2, 2)] {
        assert_terminal(&terminal_branches[sibling], final_variant);
        assert_terminal(&manifest_routes[sibling], 0);
    }

    for (sibling, pin) in retained_middle.into_iter().enumerate() {
        let reopened = resolver.resolve_for_parent(pin.unwrap()).unwrap();
        assert_module_route(
            &reopened,
            "main.orna",
            &format!(
                "= {}",
                marker(
                    1,
                    first_middle_variant[sibling],
                    first_middle_variant[sibling]
                )
            ),
        );
        assert_pin(&reopened, aliases[2], &shared_deep_commit);
    }
    let reopened_shared_deep = resolver.resolve_for_parent(retained_shared_deep).unwrap();
    assert_terminal(&reopened_shared_deep, 0);
    for (sibling, variant) in [(0, 2), (1, 0), (2, 1)] {
        let reopened = resolver
            .resolve_for_parent(retained_deep[sibling][variant].as_ref().unwrap().clone())
            .unwrap();
        assert_terminal(&reopened, 0);
    }
}

#[test]
fn interleaved_sibling_storms_keep_shared_terminal_routes_independent() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 3;
    let terminal_candidate_count = 3;
    let marker = |depth: usize, variant: usize| (depth + 1) * 100 + variant;

    let terminal_commits: Vec<String> = (0..terminal_candidate_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, variant)),
                None,
                &format!("interleaved shared terminal {variant}"),
            )
        })
        .collect();
    let deep_manifest = format!("{} {}\n", aliases[3], terminal_commits[0]);
    let deep_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        &format!("{}", marker(2, 0)),
        Some(&deep_manifest),
        "deep pin shared by every sibling route",
    );
    let middle_commits: Vec<String> = (0..sibling_count)
        .map(|variant| {
            let manifest = format!("{} {}\n", aliases[2], deep_commit);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(1, variant)),
                Some(&manifest),
                &format!("sibling {variant} converges on shared deep pin"),
            )
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent selecting initial sibling route",
    );

    let loader = ProjectLoader::default();
    let middle_pins: Vec<PinnedDatabase> = middle_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[1], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_terminal_route = |session: &AttachedDatabaseSession, variant: usize| {
        assert_eq!(
            session
                .database(aliases[3])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            terminal_commits[variant]
        );
        assert_module_route(
            session,
            &format!("{}.orna", aliases[3]),
            &format!("= {}", marker(3, variant)),
        );
    };

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut sibling_terminals = Vec::with_capacity(sibling_count);
    let mut shared_deep_pins = Vec::with_capacity(sibling_count);
    for variant in 0..sibling_count {
        let mut sibling = parent.clone();
        sibling.detach_database(aliases[1]).unwrap();
        sibling
            .attach_database(middle_pins[variant].clone())
            .unwrap();
        assert_module_route(
            &sibling,
            &format!("{}.orna", aliases[1]),
            &format!("= {}", marker(1, variant)),
        );

        let middle = resolver
            .resolve_for_parent(sibling.database(aliases[1]).unwrap().clone())
            .unwrap();
        assert_eq!(
            middle
                .database(aliases[2])
                .unwrap()
                .pin()
                .commit()
                .as_str(),
            deep_commit
        );
        let deep_pin = middle.database(aliases[2]).unwrap().clone();
        let terminal = resolver.resolve_for_parent(deep_pin.clone()).unwrap();
        assert_terminal_route(&terminal, 0);
        shared_deep_pins.push(deep_pin);
        sibling_terminals.push(terminal);
    }
    let manifest_routes = sibling_terminals.clone();
    let mut retained_candidates: Vec<Vec<Option<PinnedDatabase>>> =
        vec![vec![None; terminal_candidate_count]; sibling_count];

    // Interleave rebinds only after every sibling closure has captured the
    // same manifest-selected terminal pin.
    for (sibling, candidate) in [(0, 1), (1, 2), (2, 1), (1, 0), (0, 2), (2, 0), (1, 1)] {
        sibling_terminals[sibling]
            .detach_database(aliases[3])
            .unwrap();
        sibling_terminals[sibling]
            .attach_database(terminal_pins[candidate].clone())
            .unwrap();
        retained_candidates[sibling][candidate]
            .get_or_insert_with(|| sibling_terminals[sibling].database(aliases[3]).unwrap().clone());
        assert_terminal_route(&sibling_terminals[sibling], candidate);
    }

    for (sibling, final_candidate) in [(0, 2), (1, 1), (2, 0)] {
        assert_terminal_route(&sibling_terminals[sibling], final_candidate);
        assert_terminal_route(&manifest_routes[sibling], 0);
    }

    // A fresh closure from the shared deep pin reads its manifest after the
    // sibling storms; a retained intermediate terminal candidate also keeps
    // its own source snapshot.
    let fresh_shared_route = resolver
        .resolve_for_parent(shared_deep_pins[0].clone())
        .unwrap();
    assert_terminal_route(&fresh_shared_route, 0);
    let retained_terminal = resolver
        .resolve_for_parent(retained_candidates[0][1].as_ref().unwrap().clone())
        .unwrap();
    assert_module_route(&retained_terminal, "main.orna", "= 401");
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

#[test]
fn paired_depth_storm_rebinds_keep_sibling_terminal_routes_consistent() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let sibling_count = 2;
    let variant_count = 2;
    let marker = |depth: usize, sibling: usize, variant: usize| {
        (depth + 1) * 100 + sibling * 10 + variant
    };

    let terminal_commits: Vec<String> = (0..variant_count)
        .map(|variant| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                &format!("{}", marker(3, 0, variant)),
                None,
                &format!("shared terminal route {variant}"),
            )
        })
        .collect();
    let deep_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..variant_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[3], terminal_commits[variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(2, sibling, variant)),
                        Some(&manifest),
                        &format!("sibling {sibling} deep route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let middle_commits: Vec<Vec<String>> = (0..sibling_count)
        .map(|sibling| {
            (0..variant_count)
                .map(|variant| {
                    let manifest = format!("{} {}\n", aliases[2], deep_commits[sibling][variant]);
                    write_package_snapshot(
                        shared_dir.path(),
                        package_source,
                        &format!("{}", marker(1, sibling, variant)),
                        Some(&manifest),
                        &format!("sibling {sibling} middle route {variant}"),
                    )
                })
                .collect()
        })
        .collect();
    let parent_manifest = format!("{} {}\n", aliases[1], middle_commits[0][0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "50",
        Some(&parent_manifest),
        "parent for paired depth sibling storms",
    );

    let loader = ProjectLoader::default();
    let resolve_pin_grid = |alias: &str, commits: &[Vec<String>]| {
        commits
            .iter()
            .map(|sibling| {
                sibling
                    .iter()
                    .map(|commit| {
                        PinnedDatabase::resolve(alias, shared_repository.clone(), commit, loader)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let middle_pins = resolve_pin_grid(aliases[1], &middle_commits);
    let deep_pins = resolve_pin_grid(aliases[2], &deep_commits);
    let terminal_pins: Vec<PinnedDatabase> = terminal_commits
        .iter()
        .map(|commit| {
            PinnedDatabase::resolve(aliases[3], shared_repository.clone(), commit, loader).unwrap()
        })
        .collect();
    let parent_pin = PinnedDatabase::resolve(
        aliases[0],
        shared_repository.clone(),
        &parent_commit,
        loader,
    )
    .unwrap();
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let rebind = |session: &mut AttachedDatabaseSession, alias: &str, pin: &PinnedDatabase| {
        session.detach_database(alias).unwrap();
        session.attach_database(pin.clone()).unwrap();
    };
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };
    let assert_route = |session: &AttachedDatabaseSession,
                        path: &str,
                        depth: usize,
                        sibling: usize,
                        variant: usize| {
        assert_module_route(
            session,
            path,
            &format!("= {}", marker(depth, sibling, variant)),
        );
    };

    struct RetainedWave {
        sibling: usize,
        middle_variant: usize,
        deep_variant: usize,
        root: AttachedDatabaseSession,
        manifest_closure: AttachedDatabaseSession,
        rebound_closure: AttachedDatabaseSession,
        terminal_closure: AttachedDatabaseSession,
    }

    let parent = resolver.resolve_for_parent(parent_pin).unwrap();
    let mut roots = vec![parent.clone(); sibling_count];
    let mut waves = Vec::new();
    let mut final_terminals: Vec<Option<AttachedDatabaseSession>> =
        vec![None; sibling_count];
    let mut final_deep_variants = vec![0; sibling_count];
    // Each sibling sees multiple paired depth waves in an interleaved order.
    // The last wave leaves distinct middle/deep routes on a shared terminal.
    for (sibling, middle_variant, deep_variant) in
        [(0, 1, 0), (1, 0, 1), (0, 0, 1), (1, 1, 0), (1, 0, 0), (0, 1, 0)]
    {
        rebind(
            &mut roots[sibling],
            aliases[1],
            &middle_pins[sibling][middle_variant],
        );
        assert_pin(
            &roots[sibling],
            aliases[1],
            &middle_commits[sibling][middle_variant],
        );
        assert_route(
            &roots[sibling],
            &format!("{}.orna", aliases[1]),
            1,
            sibling,
            middle_variant,
        );

        let mut middle = resolver
            .resolve_for_parent(roots[sibling].database(aliases[1]).unwrap().clone())
            .unwrap();
        assert_pin(
            &middle,
            aliases[2],
            &deep_commits[sibling][middle_variant],
        );
        assert_route(&middle, "main.orna", 1, sibling, middle_variant);
        assert_route(
            &middle,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            middle_variant,
        );
        let manifest_closure = middle.clone();

        rebind(
            &mut middle,
            aliases[2],
            &deep_pins[sibling][deep_variant],
        );
        assert_pin(
            &middle,
            aliases[2],
            &deep_commits[sibling][deep_variant],
        );
        assert_route(&middle, "main.orna", 1, sibling, middle_variant);
        assert_route(
            &middle,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            deep_variant,
        );
        let rebound_closure = middle.clone();

        let terminal = resolver
            .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
            .unwrap();
        assert_pin(
            &terminal,
            aliases[3],
            &terminal_commits[deep_variant],
        );
        assert_route(&terminal, "main.orna", 2, sibling, deep_variant);
        assert_route(
            &terminal,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            deep_variant,
        );
        final_terminals[sibling] = Some(terminal.clone());
        final_deep_variants[sibling] = deep_variant;
        waves.push(RetainedWave {
            sibling,
            middle_variant,
            deep_variant,
            root: roots[sibling].clone(),
            manifest_closure,
            rebound_closure,
            terminal_closure: terminal,
        });
    }

    for wave in waves {
        let sibling = wave.sibling;
        assert_pin(
            &wave.root,
            aliases[1],
            &middle_commits[sibling][wave.middle_variant],
        );
        assert_route(
            &wave.root,
            &format!("{}.orna", aliases[1]),
            1,
            sibling,
            wave.middle_variant,
        );
        assert_pin(
            &wave.manifest_closure,
            aliases[2],
            &deep_commits[sibling][wave.middle_variant],
        );
        assert_route(
            &wave.manifest_closure,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            wave.middle_variant,
        );
        assert_pin(
            &wave.rebound_closure,
            aliases[2],
            &deep_commits[sibling][wave.deep_variant],
        );
        assert_route(
            &wave.rebound_closure,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            wave.deep_variant,
        );
        assert_pin(
            &wave.terminal_closure,
            aliases[3],
            &terminal_commits[wave.deep_variant],
        );
        assert_route(
            &wave.terminal_closure,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            wave.deep_variant,
        );
    }

    let mut terminal_closures: Vec<AttachedDatabaseSession> = final_terminals
        .into_iter()
        .map(Option::unwrap)
        .collect();
    let manifest_terminal_snapshots = terminal_closures.clone();
    let mut terminal_storm_snapshots = Vec::new();
    let mut selected_terminal_variants = vec![0; sibling_count];
    for (sibling, variant) in [(0, 1), (1, 0), (0, 0), (1, 1), (1, 0), (0, 1)] {
        rebind(
            &mut terminal_closures[sibling],
            aliases[3],
            &terminal_pins[variant],
        );
        selected_terminal_variants[sibling] = variant;
        terminal_storm_snapshots.push((sibling, variant, terminal_closures[sibling].clone()));

        for branch in 0..sibling_count {
            let selected = selected_terminal_variants[branch];
            assert_pin(
                &terminal_closures[branch],
                aliases[3],
                &terminal_commits[selected],
            );
            assert_route(
                &terminal_closures[branch],
                &format!("{}.orna", aliases[3]),
                3,
                0,
                selected,
            );
            assert_route(
                &terminal_closures[branch],
                "main.orna",
                2,
                branch,
                final_deep_variants[branch],
            );
        }
    }

    for sibling in 0..sibling_count {
        assert_pin(
            &manifest_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[0],
        );
        assert_route(
            &manifest_terminal_snapshots[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            0,
        );
        assert_route(
            &manifest_terminal_snapshots[sibling],
            "main.orna",
            2,
            sibling,
            final_deep_variants[sibling],
        );
    }
    for (sibling, variant, snapshot) in terminal_storm_snapshots {
        assert_pin(&snapshot, aliases[3], &terminal_commits[variant]);
        assert_route(&snapshot, &format!("{}.orna", aliases[3]), 3, 0, variant);
        assert_route(
            &snapshot,
            "main.orna",
            2,
            sibling,
            final_deep_variants[sibling],
        );
    }

    let left_terminal = &terminal_closures[0];
    let right_terminal = &terminal_closures[1];
    assert_pin(
        left_terminal,
        aliases[3],
        &terminal_commits[selected_terminal_variants[0]],
    );
    assert_pin(
        right_terminal,
        aliases[3],
        &terminal_commits[selected_terminal_variants[1]],
    );
    assert_route(
        left_terminal,
        &format!("{}.orna", aliases[3]),
        3,
        0,
        selected_terminal_variants[0],
    );
    assert_route(
        right_terminal,
        &format!("{}.orna", aliases[3]),
        3,
        0,
        selected_terminal_variants[1],
    );
    assert_route(
        left_terminal,
        "main.orna",
        2,
        0,
        final_deep_variants[0],
    );
    assert_route(
        right_terminal,
        "main.orna",
        2,
        1,
        final_deep_variants[1],
    );

    let mut later_terminal_closures: Vec<Option<AttachedDatabaseSession>> =
        vec![None; sibling_count];
    let mut later_middle_variants = vec![0; sibling_count];
    let mut later_deep_variants = vec![0; sibling_count];
    for (sibling, middle_variant, deep_variant) in
        [(1, 1, 0), (0, 0, 1), (1, 0, 1), (0, 0, 0), (1, 1, 1)]
    {
        rebind(
            &mut roots[sibling],
            aliases[1],
            &middle_pins[sibling][middle_variant],
        );
        let mut middle = resolver
            .resolve_for_parent(roots[sibling].database(aliases[1]).unwrap().clone())
            .unwrap();
        assert_pin(
            &middle,
            aliases[2],
            &deep_commits[sibling][middle_variant],
        );
        assert_route(&middle, "main.orna", 1, sibling, middle_variant);
        assert_route(
            &middle,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            middle_variant,
        );

        rebind(
            &mut middle,
            aliases[2],
            &deep_pins[sibling][deep_variant],
        );
        assert_pin(
            &middle,
            aliases[2],
            &deep_commits[sibling][deep_variant],
        );
        assert_route(
            &middle,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            deep_variant,
        );
        let terminal = resolver
            .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
            .unwrap();
        assert_pin(
            &terminal,
            aliases[3],
            &terminal_commits[deep_variant],
        );
        assert_route(&terminal, "main.orna", 2, sibling, deep_variant);
        assert_route(
            &terminal,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            deep_variant,
        );
        later_terminal_closures[sibling] = Some(terminal);
        later_middle_variants[sibling] = middle_variant;
        later_deep_variants[sibling] = deep_variant;
    }

    for sibling in 0..sibling_count {
        assert_pin(
            &terminal_closures[sibling],
            aliases[3],
            &terminal_commits[selected_terminal_variants[sibling]],
        );
        assert_route(
            &terminal_closures[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            selected_terminal_variants[sibling],
        );
        assert_route(
            &terminal_closures[sibling],
            "main.orna",
            2,
            sibling,
            final_deep_variants[sibling],
        );
        assert_pin(
            &roots[sibling],
            aliases[1],
            &middle_commits[sibling][later_middle_variants[sibling]],
        );
        assert_route(
            &roots[sibling],
            &format!("{}.orna", aliases[1]),
            1,
            sibling,
            later_middle_variants[sibling],
        );
        let later_terminal = later_terminal_closures[sibling].as_ref().unwrap();
        assert_pin(
            later_terminal,
            aliases[3],
            &terminal_commits[later_deep_variants[sibling]],
        );
        assert_route(
            later_terminal,
            "main.orna",
            2,
            sibling,
            later_deep_variants[sibling],
        );
        assert_route(
            later_terminal,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            later_deep_variants[sibling],
        );
    }
    for sibling in 0..sibling_count {
        assert_pin(
            &manifest_terminal_snapshots[sibling],
            aliases[3],
            &terminal_commits[0],
        );
        assert_route(
            &manifest_terminal_snapshots[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            0,
        );
        assert_route(
            &manifest_terminal_snapshots[sibling],
            "main.orna",
            2,
            sibling,
            final_deep_variants[sibling],
        );
    }

    let mut reopened_terminals: Vec<AttachedDatabaseSession> = later_terminal_closures
        .into_iter()
        .map(Option::unwrap)
        .collect();
    let reopened_manifest_snapshots = reopened_terminals.clone();
    let mut selected_reopened_variants = later_deep_variants.clone();
    let mut reopened_terminal_snapshots = Vec::new();
    for (sibling, variant) in [(0, 1), (1, 0), (0, 0), (1, 1), (0, 1)] {
        rebind(
            &mut reopened_terminals[sibling],
            aliases[3],
            &terminal_pins[variant],
        );
        selected_reopened_variants[sibling] = variant;
        reopened_terminal_snapshots.push((sibling, variant, reopened_terminals[sibling].clone()));

        for branch in 0..sibling_count {
            let selected = selected_reopened_variants[branch];
            assert_pin(
                &reopened_terminals[branch],
                aliases[3],
                &terminal_commits[selected],
            );
            assert_route(
                &reopened_terminals[branch],
                &format!("{}.orna", aliases[3]),
                3,
                0,
                selected,
            );
            assert_route(
                &reopened_terminals[branch],
                "main.orna",
                2,
                branch,
                later_deep_variants[branch],
            );
        }
    }

    for sibling in 0..sibling_count {
        assert_pin(
            &reopened_manifest_snapshots[sibling],
            aliases[3],
            &terminal_commits[later_deep_variants[sibling]],
        );
        assert_route(
            &reopened_manifest_snapshots[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            later_deep_variants[sibling],
        );
        assert_route(
            &reopened_manifest_snapshots[sibling],
            "main.orna",
            2,
            sibling,
            later_deep_variants[sibling],
        );
        assert_pin(
            &terminal_closures[sibling],
            aliases[3],
            &terminal_commits[selected_terminal_variants[sibling]],
        );
        assert_route(
            &terminal_closures[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            selected_terminal_variants[sibling],
        );
    }
    for (sibling, variant, snapshot) in reopened_terminal_snapshots {
        assert_pin(&snapshot, aliases[3], &terminal_commits[variant]);
        assert_route(&snapshot, &format!("{}.orna", aliases[3]), 3, 0, variant);
        assert_route(
            &snapshot,
            "main.orna",
            2,
            sibling,
            later_deep_variants[sibling],
        );
    }
    assert_pin(
        &reopened_terminals[0],
        aliases[3],
        &terminal_commits[selected_reopened_variants[0]],
    );
    assert_pin(
        &reopened_terminals[1],
        aliases[3],
        &terminal_commits[selected_reopened_variants[1]],
    );
    assert_route(&reopened_terminals[0], &format!("{}.orna", aliases[3]), 3, 0, 1);
    assert_route(&reopened_terminals[1], &format!("{}.orna", aliases[3]), 3, 0, 1);

    let forward_interleaving = [(0, 1), (1, 0), (0, 0), (1, 1)];
    let reordered_interleaving = [(1, 0), (0, 1), (1, 1), (0, 0)];
    let mut forward_terminals = reopened_manifest_snapshots.clone();
    let mut reordered_terminals = reopened_manifest_snapshots.clone();
    for (sibling, variant) in forward_interleaving {
        rebind(
            &mut forward_terminals[sibling],
            aliases[3],
            &terminal_pins[variant],
        );
    }
    for (sibling, variant) in reordered_interleaving {
        rebind(
            &mut reordered_terminals[sibling],
            aliases[3],
            &terminal_pins[variant],
        );
    }
    for (sibling, expected_variant) in [0, 1].into_iter().enumerate() {
        for session in [&forward_terminals[sibling], &reordered_terminals[sibling]] {
            assert_pin(
                session,
                aliases[3],
                &terminal_commits[expected_variant],
            );
            assert_route(
                session,
                &format!("{}.orna", aliases[3]),
                3,
                0,
                expected_variant,
            );
            assert_route(
                session,
                "main.orna",
                2,
                sibling,
                later_deep_variants[sibling],
            );
        }
        assert_pin(
            &reopened_manifest_snapshots[sibling],
            aliases[3],
            &terminal_commits[later_deep_variants[sibling]],
        );
        assert_route(
            &reopened_manifest_snapshots[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            later_deep_variants[sibling],
        );
    }

    let retained_post_storm_terminals = forward_terminals.clone();
    let mut repeated_post_storm_terminals = forward_terminals.clone();
    let post_storm_events = [(0, 1), (1, 0), (0, 0), (1, 1)];
    let mut selected_post_storm_variants = vec![0, 1];
    let mut repeated_post_storm_snapshots = Vec::new();
    for wave in 0..2 {
        for (sibling, variant) in post_storm_events {
            rebind(
                &mut repeated_post_storm_terminals[sibling],
                aliases[3],
                &terminal_pins[variant],
            );
            selected_post_storm_variants[sibling] = variant;
            repeated_post_storm_snapshots.push((
                wave,
                sibling,
                variant,
                repeated_post_storm_terminals[sibling].clone(),
            ));

            for branch in 0..sibling_count {
                let selected = selected_post_storm_variants[branch];
                assert_pin(
                    &repeated_post_storm_terminals[branch],
                    aliases[3],
                    &terminal_commits[selected],
                );
                assert_route(
                    &repeated_post_storm_terminals[branch],
                    &format!("{}.orna", aliases[3]),
                    3,
                    0,
                    selected,
                );
                assert_route(
                    &repeated_post_storm_terminals[branch],
                    "main.orna",
                    2,
                    branch,
                    later_deep_variants[branch],
                );
            }
        }
        assert_eq!(selected_post_storm_variants, vec![0, 1]);
    }
    for event_index in 0..post_storm_events.len() {
        let (_, first_sibling, first_variant, first_snapshot) =
            &repeated_post_storm_snapshots[event_index];
        let (_, second_sibling, second_variant, second_snapshot) =
            &repeated_post_storm_snapshots[event_index + post_storm_events.len()];
        assert_eq!(first_sibling, second_sibling);
        assert_eq!(first_variant, second_variant);
        assert_pin(
            first_snapshot,
            aliases[3],
            &terminal_commits[*first_variant],
        );
        assert_pin(
            second_snapshot,
            aliases[3],
            &terminal_commits[*second_variant],
        );
        assert_route(
            first_snapshot,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            *first_variant,
        );
        assert_route(
            second_snapshot,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            *second_variant,
        );
    }
    for (_, sibling, variant, snapshot) in repeated_post_storm_snapshots {
        assert_pin(&snapshot, aliases[3], &terminal_commits[variant]);
        assert_route(&snapshot, &format!("{}.orna", aliases[3]), 3, 0, variant);
        assert_route(
            &snapshot,
            "main.orna",
            2,
            sibling,
            later_deep_variants[sibling],
        );
    }
    let stable_post_storm_variants = [0, 1];
    for sibling in 0..sibling_count {
        assert_pin(
            &retained_post_storm_terminals[sibling],
            aliases[3],
            &terminal_commits[stable_post_storm_variants[sibling]],
        );
        assert_route(
            &retained_post_storm_terminals[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            stable_post_storm_variants[sibling],
        );
        assert_pin(
            &reopened_manifest_snapshots[sibling],
            aliases[3],
            &terminal_commits[later_deep_variants[sibling]],
        );
        assert_route(
            &reopened_manifest_snapshots[sibling],
            &format!("{}.orna", aliases[3]),
            3,
            0,
            later_deep_variants[sibling],
        );
    }

    let paired_depth_waves = [
        [(0, 1, 1), (1, 0, 0), (0, 0, 0), (1, 1, 1)],
        [(1, 0, 0), (0, 1, 1), (1, 1, 1), (0, 0, 0)],
    ];
    let mut post_storm_roots = roots.clone();
    let mut selected_post_storm_middle = later_middle_variants.clone();
    let mut selected_post_storm_deep = later_deep_variants.clone();
    let mut latest_post_storm_terminals: Vec<Option<AttachedDatabaseSession>> =
        vec![None; sibling_count];
    let mut post_storm_depth_snapshots = Vec::new();
    for (wave, events) in paired_depth_waves.into_iter().enumerate() {
        for (sibling, middle_variant, deep_variant) in events {
            rebind(
                &mut post_storm_roots[sibling],
                aliases[1],
                &middle_pins[sibling][middle_variant],
            );
            selected_post_storm_middle[sibling] = middle_variant;
            assert_pin(
                &post_storm_roots[sibling],
                aliases[1],
                &middle_commits[sibling][middle_variant],
            );
            assert_route(
                &post_storm_roots[sibling],
                &format!("{}.orna", aliases[1]),
                1,
                sibling,
                middle_variant,
            );

            let mut middle = resolver
                .resolve_for_parent(
                    post_storm_roots[sibling]
                        .database(aliases[1])
                        .unwrap()
                        .clone(),
                )
                .unwrap();
            assert_pin(
                &middle,
                aliases[2],
                &deep_commits[sibling][middle_variant],
            );
            assert_route(&middle, "main.orna", 1, sibling, middle_variant);
            assert_route(
                &middle,
                &format!("{}.orna", aliases[2]),
                2,
                sibling,
                middle_variant,
            );
            let manifest_middle_snapshot = middle.clone();

            rebind(
                &mut middle,
                aliases[2],
                &deep_pins[sibling][deep_variant],
            );
            selected_post_storm_deep[sibling] = deep_variant;
            assert_pin(
                &middle,
                aliases[2],
                &deep_commits[sibling][deep_variant],
            );
            assert_route(&middle, "main.orna", 1, sibling, middle_variant);
            assert_route(
                &middle,
                &format!("{}.orna", aliases[2]),
                2,
                sibling,
                deep_variant,
            );
            let rebound_middle_snapshot = middle.clone();

            let terminal = resolver
                .resolve_for_parent(middle.database(aliases[2]).unwrap().clone())
                .unwrap();
            assert_pin(
                &terminal,
                aliases[3],
                &terminal_commits[deep_variant],
            );
            assert_route(&terminal, "main.orna", 2, sibling, deep_variant);
            assert_route(
                &terminal,
                &format!("{}.orna", aliases[3]),
                3,
                0,
                deep_variant,
            );
            latest_post_storm_terminals[sibling] = Some(terminal.clone());
            post_storm_depth_snapshots.push((
                wave,
                sibling,
                middle_variant,
                deep_variant,
                post_storm_roots[sibling].clone(),
                manifest_middle_snapshot,
                rebound_middle_snapshot,
                terminal,
            ));

            for branch in 0..sibling_count {
                assert_pin(
                    &post_storm_roots[branch],
                    aliases[1],
                    &middle_commits[branch][selected_post_storm_middle[branch]],
                );
                assert_route(
                    &post_storm_roots[branch],
                    &format!("{}.orna", aliases[1]),
                    1,
                    branch,
                    selected_post_storm_middle[branch],
                );
            }
        }
        assert_eq!(selected_post_storm_middle, vec![0, 1]);
        assert_eq!(selected_post_storm_deep, vec![0, 1]);
    }
    for (wave, sibling, middle_variant, deep_variant, root, manifest_middle, rebound_middle, terminal) in
        post_storm_depth_snapshots
    {
        assert!(wave < paired_depth_waves.len());
        assert_pin(
            &root,
            aliases[1],
            &middle_commits[sibling][middle_variant],
        );
        assert_route(
            &root,
            &format!("{}.orna", aliases[1]),
            1,
            sibling,
            middle_variant,
        );
        assert_pin(
            &manifest_middle,
            aliases[2],
            &deep_commits[sibling][middle_variant],
        );
        assert_route(
            &manifest_middle,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            middle_variant,
        );
        let reopened_manifest_terminal = resolver
            .resolve_for_parent(manifest_middle.database(aliases[2]).unwrap().clone())
            .unwrap();
        assert_pin(
            &reopened_manifest_terminal,
            aliases[3],
            &terminal_commits[middle_variant],
        );
        assert_route(
            &reopened_manifest_terminal,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            middle_variant,
        );
        assert_route(
            &reopened_manifest_terminal,
            "main.orna",
            2,
            sibling,
            middle_variant,
        );
        assert_pin(
            &rebound_middle,
            aliases[2],
            &deep_commits[sibling][deep_variant],
        );
        assert_route(
            &rebound_middle,
            &format!("{}.orna", aliases[2]),
            2,
            sibling,
            deep_variant,
        );
        assert_pin(
            &terminal,
            aliases[3],
            &terminal_commits[deep_variant],
        );
        assert_route(
            &terminal,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            deep_variant,
        );
        assert_route(&terminal, "main.orna", 2, sibling, deep_variant);
    }
    for sibling in 0..sibling_count {
        let terminal = latest_post_storm_terminals[sibling].as_ref().unwrap();
        assert_pin(
            terminal,
            aliases[3],
            &terminal_commits[selected_post_storm_deep[sibling]],
        );
        assert_route(
            terminal,
            &format!("{}.orna", aliases[3]),
            3,
            0,
            selected_post_storm_deep[sibling],
        );
        for (retained, terminal_variant) in [
            (
                &reopened_manifest_snapshots[sibling],
                later_deep_variants[sibling],
            ),
            (
                &reopened_terminals[sibling],
                selected_reopened_variants[sibling],
            ),
            (
                &retained_post_storm_terminals[sibling],
                stable_post_storm_variants[sibling],
            ),
        ] {
            assert_pin(
                retained,
                aliases[3],
                &terminal_commits[terminal_variant],
            );
            assert_route(
                retained,
                &format!("{}.orna", aliases[3]),
                3,
                0,
                terminal_variant,
            );
            assert_route(
                retained,
                "main.orna",
                2,
                sibling,
                later_deep_variants[sibling],
            );
        }
    }

    assert_pin(&parent, aliases[1], &middle_commits[0][0]);
    assert_route(&parent, &format!("{}.orna", aliases[1]), 1, 0, 0);
}

#[test]
fn atomic_rebind_preserves_retained_routes_through_paired_depths() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = ["archive", "archive_copy", "archive_copy_archive"];
    let terminal_before_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "701",
        None,
        "terminal before paired rebind",
    );
    let terminal_after_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "702",
        None,
        "terminal after paired rebind",
    );
    let deep_before_manifest = format!("{} {}\n", aliases[2], terminal_before_commit);
    let deep_before_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "601",
        Some(&deep_before_manifest),
        "deep before paired rebind",
    );
    let deep_after_manifest = format!("{} {}\n", aliases[2], terminal_after_commit);
    let deep_after_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "602",
        Some(&deep_after_manifest),
        "deep after paired rebind",
    );
    let middle_before_manifest = format!("{} {}\n", aliases[1], deep_before_commit);
    let middle_before_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "501",
        Some(&middle_before_manifest),
        "middle before paired rebind",
    );
    let middle_after_manifest = format!("{} {}\n", aliases[1], deep_after_commit);
    let middle_after_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "502",
        Some(&middle_after_manifest),
        "middle after paired rebind",
    );
    let parent_manifest = format!("{} {}\n", aliases[0], middle_before_commit);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "401",
        Some(&parent_manifest),
        "parent for atomic paired rebind",
    );

    let loader = ProjectLoader::default();
    let resolve_pin = |name: &str, commit: &str| {
        PinnedDatabase::resolve(
            name.to_owned(),
            shared_repository.clone(),
            commit,
            loader,
        )
        .unwrap()
    };
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };

    let mut parent = resolver
        .resolve_for_parent(resolve_pin("app", &parent_commit))
        .unwrap();
    let retained_parent = parent.clone();
    let previous_middle = parent
        .rebind_database(resolve_pin(aliases[0], &middle_after_commit))
        .unwrap();
    assert_pin(&parent, aliases[0], &middle_after_commit);
    assert_module_route(&parent, "archive.orna", "= 502");
    assert_pin(&retained_parent, aliases[0], &middle_before_commit);
    assert_module_route(&retained_parent, "archive.orna", "= 501");
    assert_eq!(previous_middle.pin().name(), aliases[0]);
    assert_eq!(
        previous_middle.pin().commit().as_str(),
        middle_before_commit.as_str()
    );

    let mut retained_middle = resolver
        .resolve_nested_for_alias(&retained_parent, aliases[0])
        .unwrap();
    assert_pin(&retained_middle, aliases[0], &middle_before_commit);
    assert_pin(&retained_middle, aliases[1], &deep_before_commit);
    assert_module_route(&retained_middle, "main.orna", "= 501");
    assert_module_route(&retained_middle, "archive_copy.orna", "= 601");
    let retained_deep = retained_middle
        .rebind_database(resolve_pin(aliases[1], &deep_after_commit))
        .unwrap();
    assert_pin(&retained_middle, aliases[1], &deep_after_commit);
    assert_module_route(&retained_middle, "archive_copy.orna", "= 602");
    assert_eq!(retained_deep.pin().name(), aliases[1]);
    assert_eq!(
        retained_deep.pin().commit().as_str(),
        deep_before_commit.as_str()
    );

    let retained_terminal = resolver.resolve_for_parent(retained_deep).unwrap();
    assert_pin(
        &retained_terminal,
        aliases[2],
        &terminal_before_commit,
    );
    assert_module_route(&retained_terminal, "main.orna", "= 601");
    assert_module_route(&retained_terminal, "archive_copy_archive.orna", "= 701");
    let fresh_middle = resolver
        .resolve_nested_for_alias(&parent, aliases[0])
        .unwrap();
    assert_pin(&fresh_middle, aliases[0], &middle_after_commit);
    assert_pin(&fresh_middle, aliases[1], &deep_after_commit);
    assert_module_route(&fresh_middle, "main.orna", "= 502");
    assert_module_route(&fresh_middle, "archive_copy.orna", "= 602");
    let fresh_terminal = resolver
        .resolve_nested_for_alias(&retained_middle, aliases[1])
        .unwrap();
    assert_pin(&fresh_terminal, aliases[2], &terminal_after_commit);
    assert_module_route(&fresh_terminal, "main.orna", "= 602");
    assert_module_route(&fresh_terminal, "archive_copy_archive.orna", "= 702");

    let retained_path_terminal = resolver
        .resolve_nested_path(&retained_parent, &aliases[..2])
        .unwrap();
    assert_pin(
        &retained_path_terminal,
        aliases[2],
        &terminal_before_commit,
    );
    assert_module_route(&retained_path_terminal, "main.orna", "= 601");
    assert_module_route(
        &retained_path_terminal,
        "archive_copy_archive.orna",
        "= 701",
    );
    let current_path_terminal = resolver
        .resolve_nested_path(&parent, &aliases[..2])
        .unwrap();
    assert_pin(
        &current_path_terminal,
        aliases[2],
        &terminal_after_commit,
    );
    assert_module_route(&current_path_terminal, "main.orna", "= 602");
    assert_module_route(
        &current_path_terminal,
        "archive_copy_archive.orna",
        "= 702",
    );

    assert!(matches!(
        resolver.resolve_nested_for_alias(&parent, aliases[1]),
        Err(AttachmentError::DatabaseUnavailable)
    ));
    assert!(matches!(
        resolver.resolve_nested_path(&parent, &["missing"]),
        Err(AttachmentError::DatabaseUnavailable)
    ));

    assert!(matches!(
        parent.rebind_database(resolve_pin(aliases[2], &terminal_after_commit)),
        Err(AttachmentError::AttachmentNotFound)
    ));
    assert_pin(&parent, aliases[0], &middle_after_commit);
    assert_module_route(&parent, "archive.orna", "= 502");
}

#[test]
fn paired_post_storm_rebinds_preserve_each_route_snapshot_atomically() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = ["archive", "archive_copy", "archive_copy_archive"];
    let terminal_commits = ["700", "701", "702", "703", "704", "705"]
        .map(|marker| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                marker,
                None,
                &format!("terminal {marker}"),
            )
        });
    let deep_commits = ["600", "601", "602", "603", "604"]
        .into_iter()
        .enumerate()
        .map(|(index, marker)| {
            let manifest = format!("{} {}\n", aliases[2], terminal_commits[index]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                marker,
                Some(&manifest),
                &format!("deep {marker}"),
            )
        })
        .collect::<Vec<_>>();
    let middle_commits = ["500", "501", "502", "503"]
        .into_iter()
        .enumerate()
        .map(|(index, marker)| {
            let manifest = format!("{} {}\n", aliases[1], deep_commits[index]);
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                marker,
                Some(&manifest),
                &format!("middle {marker}"),
            )
        })
        .collect::<Vec<_>>();
    let parent_manifest = format!("{} {}\n", aliases[0], middle_commits[0]);
    let parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "400",
        Some(&parent_manifest),
        "root before paired rebind storms",
    );

    let loader = ProjectLoader::default();
    let resolve_pin = |name: &str, commit: &str| {
        PinnedDatabase::resolve(
            name.to_owned(),
            shared_repository.clone(),
            commit,
            loader,
        )
        .unwrap()
    };
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };

    let mut post_storm_parent = resolver
        .resolve_for_parent(resolve_pin("app", &parent_commit))
        .unwrap();
    post_storm_parent
        .rebind_database(resolve_pin(aliases[0], &middle_commits[1]))
        .unwrap();
    post_storm_parent
        .rebind_database(resolve_pin(aliases[0], &middle_commits[2]))
        .unwrap();
    let after_prior_storm = post_storm_parent.clone();

    let first_wave = resolver
        .resolve_nested_rebind_path(
            &post_storm_parent,
            &[
                resolve_pin(aliases[0], &middle_commits[3]),
                resolve_pin(aliases[1], &deep_commits[4]),
            ],
        )
        .unwrap();
    assert_eq!(first_wave.retained_sessions().len(), 2);
    assert_pin(
        &first_wave.retained_sessions()[0],
        aliases[0],
        &middle_commits[2],
    );
    assert_module_route(
        &first_wave.retained_sessions()[0],
        "archive.orna",
        "= 502",
    );
    assert_pin(
        &first_wave.retained_sessions()[1],
        aliases[1],
        &deep_commits[3],
    );
    assert_module_route(
        &first_wave.retained_sessions()[1],
        "main.orna",
        "= 503",
    );
    assert_module_route(
        &first_wave.retained_sessions()[1],
        "archive_copy.orna",
        "= 603",
    );
    assert_pin(
        first_wave.final_session(),
        aliases[2],
        &terminal_commits[4],
    );
    assert_module_route(first_wave.final_session(), "main.orna", "= 604");
    assert_module_route(
        first_wave.final_session(),
        "archive_copy_archive.orna",
        "= 704",
    );

    let reopened_prior_route = resolver
        .resolve_nested_path(&after_prior_storm, &aliases[..2])
        .unwrap();
    assert_pin(
        &reopened_prior_route,
        aliases[2],
        &terminal_commits[2],
    );
    assert_module_route(&reopened_prior_route, "main.orna", "= 602");
    assert_module_route(
        &reopened_prior_route,
        "archive_copy_archive.orna",
        "= 702",
    );
    assert_pin(
        &post_storm_parent,
        aliases[0],
        &middle_commits[2],
    );

    let extended = resolver
        .extend_nested_rebind_path(
            &first_wave,
            &[resolve_pin(aliases[2], &terminal_commits[5])],
        )
        .unwrap();
    assert_eq!(extended.retained_sessions().len(), 3);
    assert_pin(
        &extended.retained_sessions()[2],
        aliases[2],
        &terminal_commits[4],
    );
    assert_pin(
        first_wave.final_session(),
        aliases[2],
        &terminal_commits[4],
    );
    assert_pin(extended.final_session(), aliases[2], &terminal_commits[5]);

    let failed_extension = resolver.extend_nested_rebind_path(
        &first_wave,
        &[
            resolve_pin(aliases[2], &terminal_commits[5]),
            resolve_pin("unavailable", &terminal_commits[0]),
        ],
    );
    assert!(matches!(
        failed_extension,
        Err(AttachmentError::AttachmentNotFound)
    ));
    assert_pin(
        first_wave.final_session(),
        aliases[2],
        &terminal_commits[4],
    );
}

#[test]
fn sibling_terminal_routes_stabilize_across_repeated_post_storm_waves() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let leaf_commits = ["700", "701", "702", "703"]
        .into_iter()
        .map(|marker| {
            write_package_snapshot(
                shared_dir.path(),
                package_source,
                marker,
                None,
                &format!("leaf {marker}"),
            )
        })
        .collect::<Vec<_>>();
    let terminal_left_manifest = format!("{} {}\n", aliases[3], leaf_commits[0]);
    let terminal_left = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "710",
        Some(&terminal_left_manifest),
        "left terminal route",
    );
    let terminal_right_manifest = format!("{} {}\n", aliases[3], leaf_commits[1]);
    let terminal_right = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "711",
        Some(&terminal_right_manifest),
        "right terminal route",
    );
    let shared_terminal_manifest = format!("{} {}\n", aliases[3], leaf_commits[2]);
    let shared_terminal = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "712",
        Some(&shared_terminal_manifest),
        "shared terminal route",
    );
    let deep_left_manifest = format!("{} {}\n", aliases[2], terminal_left);
    let deep_left = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "602",
        Some(&deep_left_manifest),
        "left deep route",
    );
    let deep_right_manifest = format!("{} {}\n", aliases[2], terminal_right);
    let deep_right = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "603",
        Some(&deep_right_manifest),
        "right deep route",
    );
    let deep_base_manifest = format!("{} {}\n", aliases[2], terminal_left);
    let deep_base = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "600",
        Some(&deep_base_manifest),
        "shared middle closure base",
    );
    let middle_left_manifest = format!("{} {}\n", aliases[1], deep_base);
    let middle_left = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "500",
        Some(&middle_left_manifest),
        "left initial middle route",
    );
    let middle_right_manifest = format!("{} {}\n", aliases[1], deep_base);
    let middle_right = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "501",
        Some(&middle_right_manifest),
        "right initial middle route",
    );
    let middle_post_left_manifest = format!("{} {}\n", aliases[1], deep_base);
    let middle_post_left = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "502",
        Some(&middle_post_left_manifest),
        "left post-storm middle route",
    );
    let middle_post_right_manifest = format!("{} {}\n", aliases[1], deep_base);
    let middle_post_right = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "503",
        Some(&middle_post_right_manifest),
        "right post-storm middle route",
    );
    let middle_shared_manifest = format!("{} {}\n", aliases[1], deep_base);
    let middle_shared = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "504",
        Some(&middle_shared_manifest),
        "shared rebind wave middle route",
    );
    let left_parent_manifest = format!("{} {}\n", aliases[0], middle_left);
    let left_parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "400",
        Some(&left_parent_manifest),
        "left sibling root",
    );
    let right_parent_manifest = format!("{} {}\n", aliases[0], middle_right);
    let right_parent_commit = write_package_snapshot(
        shared_dir.path(),
        package_source,
        "401",
        Some(&right_parent_manifest),
        "right sibling root",
    );

    let loader = ProjectLoader::default();
    let resolve_pin = |name: &str, commit: &str| {
        PinnedDatabase::resolve(
            name.to_owned(),
            shared_repository.clone(),
            commit,
            loader,
        )
        .unwrap()
    };
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };

    let mut parents = vec![
        resolver
            .resolve_for_parent(resolve_pin("app_left", &left_parent_commit))
            .unwrap(),
        resolver
            .resolve_for_parent(resolve_pin("app_right", &right_parent_commit))
            .unwrap(),
    ];
    let initial_parents = parents.clone();
    parents[0]
        .rebind_database(resolve_pin(aliases[0], &middle_post_left))
        .unwrap();
    parents[1]
        .rebind_database(resolve_pin(aliases[0], &middle_post_right))
        .unwrap();
    let post_storm = resolver
        .resolve_sibling_rebind_wave(&parents, &[resolve_pin(aliases[0], &middle_shared)])
        .unwrap();

    assert_eq!(post_storm.routes().len(), 2);
    assert_pin(
        &post_storm.routes()[0].retained_sessions()[0],
        aliases[0],
        &middle_post_left,
    );
    assert_pin(
        &post_storm.routes()[1].retained_sessions()[0],
        aliases[0],
        &middle_post_right,
    );
    assert_pin(
        &initial_parents[0],
        aliases[0],
        &middle_left,
    );
    assert_pin(
        &initial_parents[1],
        aliases[0],
        &middle_right,
    );

    let left_deep_wave = [resolve_pin(aliases[1], &deep_left)];
    let right_deep_wave = [resolve_pin(aliases[1], &deep_right)];
    let diverged = resolver
        .extend_sibling_rebind_paths(&[
            (&post_storm.routes()[0], &left_deep_wave),
            (&post_storm.routes()[1], &right_deep_wave),
        ])
        .unwrap();
    assert_eq!(diverged.routes().len(), 2);
    assert_pin(
        diverged.routes()[0].final_session(),
        aliases[2],
        &terminal_left,
    );
    assert_pin(
        diverged.routes()[1].final_session(),
        aliases[2],
        &terminal_right,
    );

    let shared_terminal_wave = [resolve_pin(aliases[2], &shared_terminal)];
    let terminal_convergence = resolver
        .extend_sibling_rebind_wave(&diverged, &shared_terminal_wave)
        .unwrap();
    assert_eq!(terminal_convergence.routes().len(), 2);
    assert_pin(
        terminal_convergence.routes()[0].final_session(),
        aliases[3],
        &leaf_commits[2],
    );
    assert_pin(
        terminal_convergence.routes()[1].final_session(),
        aliases[3],
        &leaf_commits[2],
    );
    assert_eq!(terminal_convergence.routes()[0].retained_sessions().len(), 3);
    assert_eq!(terminal_convergence.routes()[1].retained_sessions().len(), 3);
    assert_pin(
        &terminal_convergence.routes()[0].retained_sessions()[2],
        aliases[2],
        &terminal_left,
    );
    assert_pin(
        &terminal_convergence.routes()[1].retained_sessions()[2],
        aliases[2],
        &terminal_right,
    );
    assert_module_route(
        &terminal_convergence.routes()[0].retained_sessions()[2],
        "archive_copy_archive.orna",
        "= 710",
    );
    assert_module_route(
        &terminal_convergence.routes()[1].retained_sessions()[2],
        "archive_copy_archive.orna",
        "= 711",
    );

    let shared_leaf_wave = [resolve_pin(aliases[3], &leaf_commits[3])];
    let stabilized = resolver
        .extend_sibling_rebind_wave(&terminal_convergence, &shared_leaf_wave)
        .unwrap();
    assert_eq!(stabilized.routes().len(), 2);
    for route in stabilized.routes() {
        assert_eq!(route.retained_sessions().len(), 4);
        assert_pin(route.final_session(), aliases[3], &leaf_commits[3]);
        assert_module_route(route.final_session(), "main.orna", "= 703");
        assert_pin(
            &route.retained_sessions()[3],
            aliases[3],
            &leaf_commits[2],
        );
        assert_module_route(
            &route.retained_sessions()[3],
            "archive_copy_archive_archive.orna",
            "= 702",
        );
    }

    let valid_plan = [resolve_pin(aliases[0], &middle_shared)];
    let late_failure = [
        resolve_pin(aliases[0], &middle_shared),
        resolve_pin("unavailable", &leaf_commits[0]),
    ];
    let failed_batch = resolver.resolve_sibling_rebind_paths(&[
        (&parents[0], &valid_plan),
        (&parents[1], &late_failure),
    ]);
    assert!(matches!(
        failed_batch,
        Err(AttachmentError::AttachmentNotFound)
    ));
    assert_pin(&parents[0], aliases[0], &middle_post_left);
    assert_pin(&parents[1], aliases[0], &middle_post_right);
    assert_pin(
        diverged.routes()[0].final_session(),
        aliases[2],
        &terminal_left,
    );
    assert_pin(
        diverged.routes()[1].final_session(),
        aliases[2],
        &terminal_right,
    );
}

#[test]
fn paired_terminal_depth_waves_retain_pre_and_between_routes() {
    let package_source = include_str!("fixtures/attach-package.orna");
    let (shared_dir, shared_repository, _) = repository(&[("main.orna", package_source)]);
    let aliases = [
        "archive",
        "archive_copy",
        "archive_copy_archive",
        "archive_copy_archive_archive",
    ];
    let write = |marker: &str, manifest: Option<&str>, message: &str| {
        write_package_snapshot(
            shared_dir.path(),
            package_source,
            marker,
            manifest,
            message,
        )
    };
    let leaves = ["700", "701", "702", "703", "704"]
        .into_iter()
        .map(|marker| write(marker, None, &format!("leaf {marker}")))
        .collect::<Vec<_>>();
    let terminal_base_manifest = format!("{} {}\n", aliases[3], leaves[0]);
    let terminal_base = write(
        "710",
        Some(&terminal_base_manifest),
        "terminal route before pair",
    );
    let terminal_left_manifest = format!("{} {}\n", aliases[3], leaves[1]);
    let terminal_left = write(
        "711",
        Some(&terminal_left_manifest),
        "left terminal pair route",
    );
    let terminal_right_manifest = format!("{} {}\n", aliases[3], leaves[2]);
    let terminal_right = write(
        "712",
        Some(&terminal_right_manifest),
        "right terminal pair route",
    );
    let deep_base_manifest = format!("{} {}\n", aliases[2], terminal_base);
    let deep_base = write("600", Some(&deep_base_manifest), "deep route before storm");
    let deep_left_manifest = format!("{} {}\n", aliases[2], terminal_base);
    let deep_left = write("601", Some(&deep_left_manifest), "left deep pair route");
    let deep_right_manifest = format!("{} {}\n", aliases[2], terminal_base);
    let deep_right = write("602", Some(&deep_right_manifest), "right deep pair route");
    let middle_commits = [
        ("500", "initial left middle"),
        ("501", "initial right middle"),
        ("502", "post-storm left middle"),
        ("503", "post-storm right middle"),
        ("504", "paired left middle"),
        ("505", "paired right middle"),
    ]
    .into_iter()
    .map(|(marker, message)| {
        let manifest = format!("{} {}\n", aliases[1], deep_base);
        write(marker, Some(&manifest), message)
    })
    .collect::<Vec<_>>();
    let left_parent_manifest = format!("{} {}\n", aliases[0], middle_commits[0]);
    let left_parent_commit = write("400", Some(&left_parent_manifest), "left sibling root");
    let right_parent_manifest = format!("{} {}\n", aliases[0], middle_commits[1]);
    let right_parent_commit = write("401", Some(&right_parent_manifest), "right sibling root");

    let loader = ProjectLoader::default();
    let resolve_pin = |name: &str, commit: &str| {
        PinnedDatabase::resolve(
            name.to_owned(),
            shared_repository.clone(),
            commit,
            loader,
        )
        .unwrap()
    };
    let resolver = PackageResolver::new(
        aliases
            .iter()
            .map(|alias| ((*alias).to_owned(), shared_repository.clone())),
        loader,
    )
    .unwrap();
    let assert_pin = |session: &AttachedDatabaseSession, alias: &str, commit: &str| {
        assert_eq!(
            session.database(alias).unwrap().pin().commit().as_str(),
            commit
        );
    };

    let mut left_parent = resolver
        .resolve_for_parent(resolve_pin("app_left", &left_parent_commit))
        .unwrap();
    let mut right_parent = resolver
        .resolve_for_parent(resolve_pin("app_right", &right_parent_commit))
        .unwrap();
    left_parent
        .rebind_database(resolve_pin(aliases[0], &middle_commits[2]))
        .unwrap();
    right_parent
        .rebind_database(resolve_pin(aliases[0], &middle_commits[3]))
        .unwrap();

    let left_history = resolver
        .resolve_nested_terminal_pair(
            &left_parent,
            [
                resolve_pin(aliases[0], &middle_commits[4]),
                resolve_pin(aliases[1], &deep_left),
            ],
        )
        .unwrap();
    let right_history = resolver
        .resolve_nested_terminal_pair(
            &right_parent,
            [
                resolve_pin(aliases[0], &middle_commits[5]),
                resolve_pin(aliases[1], &deep_right),
            ],
        )
        .unwrap();
    for (history, parent_middle, paired_middle, deep) in [
        (
            &left_history,
            &middle_commits[2],
            &middle_commits[4],
            &deep_left,
        ),
        (
            &right_history,
            &middle_commits[3],
            &middle_commits[5],
            &deep_right,
        ),
    ] {
        let initial_wave = history.retained_wave(0).unwrap();
        assert_eq!(initial_wave.len(), 2);
        assert_pin(&initial_wave[0], aliases[0], parent_middle);
        assert_pin(&initial_wave[1], aliases[0], paired_middle);
        assert_pin(&initial_wave[1], aliases[1], &deep_base);
        assert_pin(history.final_session(), aliases[1], deep);
        assert_pin(history.final_session(), aliases[2], &terminal_base);
    }

    let left_terminal_pair = [
        resolve_pin(aliases[2], &terminal_left),
        resolve_pin(aliases[3], &leaves[3]),
    ];
    let right_terminal_pair = [
        resolve_pin(aliases[2], &terminal_right),
        resolve_pin(aliases[3], &leaves[4]),
    ];
    let terminal_pair_wave = resolver
        .extend_sibling_terminal_pair_paths(&[
            (&left_history, &left_terminal_pair),
            (&right_history, &right_terminal_pair),
        ])
        .unwrap();
    assert_eq!(terminal_pair_wave.routes().len(), 2);
    for (route, terminal, prior_leaf, final_leaf, terminal_marker, leaf_marker) in [
        (
            &terminal_pair_wave.routes()[0],
            &terminal_left,
            &leaves[1],
            &leaves[3],
            "711",
            "701",
        ),
        (
            &terminal_pair_wave.routes()[1],
            &terminal_right,
            &leaves[2],
            &leaves[4],
            "712",
            "702",
        ),
    ] {
        let terminal_wave = route.retained_wave(1).unwrap();
        assert_eq!(terminal_wave.len(), 2);
        assert_pin(&terminal_wave[0], aliases[2], &terminal_base);
        assert_module_route(&terminal_wave[0], "archive_copy_archive.orna", "= 710");
        assert_pin(&terminal_wave[1], aliases[3], prior_leaf);
        assert_pin(&terminal_wave[1], aliases[2], terminal);
        assert_module_route(&terminal_wave[1], "main.orna", &format!("= {terminal_marker}"));
        assert_module_route(
            &terminal_wave[1],
            "archive_copy_archive_archive.orna",
            &format!("= {leaf_marker}"),
        );
        assert_pin(route.final_session(), aliases[3], final_leaf);
    }
    assert_pin(left_history.final_session(), aliases[2], &terminal_base);
    assert_pin(right_history.final_session(), aliases[2], &terminal_base);

    let failed_pair = [
        resolve_pin(aliases[2], &terminal_right),
        resolve_pin("unavailable", &leaves[4]),
    ];
    let failed_batch = resolver.extend_sibling_terminal_pair_paths(&[
        (&left_history, &left_terminal_pair),
        (&right_history, &failed_pair),
    ]);
    assert!(matches!(
        failed_batch,
        Err(AttachmentError::AttachmentNotFound)
    ));
    assert_pin(left_history.final_session(), aliases[2], &terminal_base);
    assert_pin(right_history.final_session(), aliases[2], &terminal_base);

    let left_retained_terminal_pair = [
        resolve_pin(aliases[1], &deep_left),
        resolve_pin(aliases[2], &terminal_left),
    ];
    let right_retained_terminal_pair = [
        resolve_pin(aliases[1], &deep_right),
        resolve_pin(aliases[2], &terminal_right),
    ];
    let reopened = resolver
        .extend_sibling_terminal_pair_paths_from_retained(&[
            (&left_history, 1, &left_retained_terminal_pair),
            (&right_history, 1, &right_retained_terminal_pair),
        ])
        .unwrap();
    assert_eq!(reopened.routes().len(), 2);
    for (
        route,
        middle,
        deep,
        terminal,
        terminal_leaf,
        deep_marker,
        terminal_marker,
        terminal_leaf_marker,
    ) in [
        (
            &reopened.routes()[0],
            &middle_commits[4],
            &deep_left,
            &terminal_left,
            &leaves[1],
            "601",
            "711",
            "701",
        ),
        (
            &reopened.routes()[1],
            &middle_commits[5],
            &deep_right,
            &terminal_right,
            &leaves[2],
            "602",
            "712",
            "702",
        ),
    ] {
        let retained_pre_storm = route.retained_wave(0).unwrap();
        assert_eq!(retained_pre_storm.len(), 2);
        assert_pin(&retained_pre_storm[1], aliases[0], middle);

        let reopened_wave = route.retained_wave(1).unwrap();
        assert_eq!(reopened_wave.len(), 2);
        assert_pin(&reopened_wave[0], aliases[0], middle);
        assert_pin(&reopened_wave[0], aliases[1], &deep_base);
        assert_pin(&reopened_wave[1], aliases[1], deep);
        assert_pin(&reopened_wave[1], aliases[2], &terminal_base);
        assert_module_route(
            &reopened_wave[1],
            "main.orna",
            &format!("= {deep_marker}"),
        );
        assert_module_route(
            &reopened_wave[1],
            "archive_copy_archive.orna",
            "= 710",
        );
        assert_pin(route.final_session(), aliases[2], terminal);
        assert_pin(route.final_session(), aliases[3], terminal_leaf);
        assert_module_route(
            route.final_session(),
            "main.orna",
            &format!("= {terminal_marker}"),
        );
        assert_module_route(
            route.final_session(),
            "archive_copy_archive_archive.orna",
            &format!("= {terminal_leaf_marker}"),
        );
    }
    assert!(matches!(
        resolver.extend_sibling_terminal_pair_paths_from_retained(&[
            (&left_history, usize::MAX, &left_retained_terminal_pair),
            (&right_history, 1, &right_retained_terminal_pair),
        ]),
        Err(AttachmentError::RetainedSnapshotUnavailable)
    ));
    assert_pin(left_history.final_session(), aliases[2], &terminal_base);
    assert_pin(right_history.final_session(), aliases[2], &terminal_base);
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
