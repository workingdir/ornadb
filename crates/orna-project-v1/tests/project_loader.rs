use std::{fs, path::Path, process::Command};

use orna_project_v1::{ProjectLimits, ProjectLoadError, ProjectLoader};
use orna_repository_v1::{ManagedFileChange, ManagedPath, Repository};
use orna_semantic_v1::{Catalogue, StandardDependencyProfile, analyze_with_catalogue};
use tempfile::TempDir;

fn repository(files: &[(&str, &str)]) -> (TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let path = directory.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    let repository = Repository::discover(directory.path()).unwrap();
    (directory, repository)
}

fn commit_all(directory: &TempDir) {
    for (key, value) in [
        ("user.email", "kieran@drewett.dev"),
        ("user.name", "kierandrewett"),
        ("commit.gpgsign", "false"),
    ] {
        let output = Command::new("git")
            .args(["config", key, value])
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git config {key}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(
        Command::new("git")
            .args(["add", "."])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "--quiet", "-m", "snapshot"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
}

fn git_output(directory: &TempDir, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git_output_at(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit_directory(directory: &Path, message: &str) {
    for (key, value) in [
        ("user.email", "kieran@drewett.dev"),
        ("user.name", "kierandrewett"),
        ("commit.gpgsign", "false"),
    ] {
        git_output_at(directory, &["config", key, value]);
    }
    git_output_at(directory, &["add", "-A"]);
    git_output_at(directory, &["commit", "--quiet", "-m", message]);
}

#[test]
fn loads_only_reachable_modules_in_deterministic_logical_order() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/main_with_library_and_nested_module.orna"),
        ),
        ("library.orna", include_str!("fixtures/library_seed.orna")),
        (
            "sensors/greenhouse/main.orna",
            include_str!("fixtures/greenhouse_ingest.orna"),
        ),
        (
            "unused.orna",
            include_str!("fixtures/unreachable_invalid_module.orna"),
        ),
    ]);

    let project = ProjectLoader::default().load(&repository).unwrap();
    let paths = project
        .identities()
        .iter()
        .map(|identity| identity.logical_path())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        ["library.orna", "main.orna", "sensors/greenhouse/main.orna"]
    );
    assert_eq!(
        project.identities()[2].namespace(),
        ["sensors", "greenhouse"]
    );
}

#[test]
fn records_reachable_standard_module_names_without_changing_ordinary_loading() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/main_with_catalogue_import.orna"),
        ),
        (
            "library.orna",
            include_str!("fixtures/library_increment_consumer.orna"),
        ),
        (
            "unreachable.orna",
            include_str!("fixtures/unreachable_standard_import.orna"),
        ),
    ]);

    let project = ProjectLoader::default().load(&repository).unwrap();

    assert_eq!(
        project
            .standard_modules()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["std/math.orna"]
    );
    assert_eq!(
        project
            .identities()
            .iter()
            .map(|identity| identity.logical_path())
            .collect::<Vec<_>>(),
        ["library.orna", "main.orna"]
    );
}

#[test]
fn maps_standard_root_import_to_directory_main_module() {
    let (_directory, repository) = repository(&[(
        "main.orna",
        include_str!("fixtures/main_with_standard_root_import.orna"),
    )]);
    let source = include_str!("fixtures/standard_root_answer.orna");
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/main.orna".into(), source.into())],
    )
    .unwrap();

    let project = ProjectLoader::default()
        .load_with_standard_profile(&repository, Some(profile))
        .unwrap();

    assert_eq!(
        project
            .standard_modules()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["std/main.orna"]
    );
    assert!(
        project
            .standard_catalogue([("std/main.orna".into(), source.into())])
            .unwrap()
            .is_some()
    );
}

#[test]
fn carries_only_an_explicit_standard_dependency_profile() {
    let (_directory, repository) =
        repository(&[("main.orna", include_str!("fixtures/main_run.orna"))]);
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [(
            "std/math.orna".into(),
            include_str!("fixtures/standard_increment.orna").into(),
        )],
    )
    .unwrap();

    let project = ProjectLoader::default()
        .load_with_standard_profile(&repository, Some(profile.clone()))
        .unwrap();
    assert_eq!(project.standard_profile(), Some(&profile));
    assert_eq!(project.modules().len(), 1);
    assert!(
        ProjectLoader::default()
            .load(&repository)
            .unwrap()
            .standard_profile()
            .is_none()
    );
}

#[test]
fn derives_standard_catalogue_only_from_the_pinned_profile_source_bundle() {
    let (_directory, repository) = repository(&[(
        "main.orna",
        include_str!("fixtures/main_increment_consumer.orna"),
    )]);
    let source = include_str!("fixtures/standard_public_increment.orna");
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [("std/math.orna".into(), source.into())],
    )
    .unwrap();
    let project = ProjectLoader::default()
        .load_with_standard_profile(&repository, Some(profile))
        .unwrap();

    let catalogue = project
        .standard_catalogue([("std/math.orna".into(), source.into())])
        .unwrap()
        .unwrap();
    let analysis = analyze_with_catalogue(project.modules(), &catalogue);
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    assert_eq!(
        ProjectLoader::default()
            .load(&repository)
            .unwrap()
            .standard_catalogue([])
            .unwrap(),
        None
    );
}

#[test]
fn loads_the_captured_standard_gitlink_and_keeps_historical_imports_pinned() {
    let (_directory, repository) = repository(&[(
        "main.orna",
        include_str!("fixtures/captured-standard-parent.orna"),
    )]);
    let module_path = repository.worktree().join("stdlib/std");
    fs::create_dir_all(&module_path).unwrap();
    orna_repository_v1::initialize_repository(&module_path).unwrap();
    fs::write(
        module_path.join("main.orna"),
        include_str!("fixtures/captured-standard-main.orna"),
    )
    .unwrap();
    fs::write(
        module_path.join("collection.orna"),
        include_str!("fixtures/captured-standard-collection-v1.orna"),
    )
    .unwrap();
    fs::write(
        module_path.join("unreachable.orna"),
        include_str!("fixtures/unreachable-standard-module.orna"),
    )
    .unwrap();
    commit_directory(&module_path, "standard v1");
    let first_module_commit = git_output_at(&module_path, &["rev-parse", "HEAD"]);

    fs::write(
        repository.worktree().join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/ornadb-std.git\n",
    )
    .unwrap();
    git_output_at(repository.worktree(), &["config", "user.email", "kieran@drewett.dev"]);
    git_output_at(repository.worktree(), &["config", "user.name", "kierandrewett"]);
    git_output_at(repository.worktree(), &["add", "main.orna", ".gitmodules"]);
    let first_link = format!("160000,{first_module_commit},stdlib/std");
    git_output_at(
        repository.worktree(),
        &["update-index", "--add", "--cacheinfo", &first_link],
    );
    git_output_at(repository.worktree(), &["commit", "--quiet", "-m", "capture std v1"]);
    let first_parent_commit = repository.head().unwrap().unwrap();

    fs::write(
        module_path.join("collection.orna"),
        include_str!("fixtures/captured-standard-collection-v2.orna"),
    )
    .unwrap();
    commit_directory(&module_path, "standard v2");
    let second_module_commit = git_output_at(&module_path, &["rev-parse", "HEAD"]);

    let loader = ProjectLoader::default();
    let current = loader.load(&repository).unwrap();
    let historical = loader
        .load_committed_snapshot(&repository, &first_parent_commit)
        .unwrap();
    for project in [&current, &historical] {
        let profile = project.standard_profile().expect("captured std profile");
        assert_eq!(profile.snapshot(), first_module_commit);
        assert_eq!(
            project
                .standard_sources()
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["std/collection.orna", "std/main.orna"]
        );
        let collection = project
            .standard_sources()
            .iter()
            .find(|(path, _)| path == "std/collection.orna")
            .unwrap();
        assert_eq!(collection.1, include_str!("fixtures/captured-standard-collection-v1.orna"));
        let catalogue = Catalogue::authoritative_core()
            .with_standard_sources(profile, project.standard_sources().iter().cloned())
            .unwrap();
        let analysis = analyze_with_catalogue(project.modules(), &catalogue);
        assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
    }

    let second_link = format!("160000,{second_module_commit},stdlib/std");
    git_output_at(
        repository.worktree(),
        &["update-index", "--add", "--cacheinfo", &second_link],
    );
    git_output_at(repository.worktree(), &["commit", "--quiet", "-m", "capture std v2"]);
    let second_parent_commit = repository.head().unwrap().unwrap();
    let latest = loader
        .load_committed_snapshot(&repository, &second_parent_commit)
        .unwrap();
    assert_eq!(
        latest.standard_profile().unwrap().snapshot(),
        second_module_commit
    );
    assert_eq!(
        latest
            .standard_sources()
            .iter()
            .find(|(path, _)| path == "std/collection.orna")
            .unwrap()
            .1,
        include_str!("fixtures/captured-standard-collection-v2.orna")
    );
}

#[test]
fn unchanged_reference_bundle_loads_and_reaches_v1_semantic_analysis() {
    let directory = tempfile::tempdir().unwrap();
    for (name, source) in [
        ("main.orna", include_str!("fixtures/reference-project/main.orna")),
        ("library.orna", include_str!("fixtures/reference-project/library.orna")),
        ("warehouse.orna", include_str!("fixtures/reference-project/warehouse.orna")),
        ("sensors.orna", include_str!("fixtures/reference-project/sensors.orna")),
        ("values.orna", include_str!("fixtures/reference-project/values.orna")),
    ] {
        fs::write(directory.path().join(name), source).unwrap();
    }
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    let repository = Repository::discover(directory.path()).unwrap();

    let project = ProjectLoader::default().load(&repository).unwrap();
    assert_eq!(project.modules().len(), 5);
    // The frozen bundle uses only the authoritative intrinsic catalogue today;
    // `sys` and `std` imports remain catalogue dependencies, never files here.
    let analysis = analyze_with_catalogue(project.modules(), &Catalogue::authoritative_core());
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}

#[test]
fn rejects_conflicting_and_unavailable_imported_modules_before_loading_them() {
    let (_directory, first_repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/main_import_library.orna"),
        ),
        ("library.orna", include_str!("fixtures/library_one.orna")),
        (
            "library/main.orna",
            include_str!("fixtures/library_two.orna"),
        ),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&first_repository),
        Err(ProjectLoadError::DuplicateModuleNamespace)
    ));

    let (_directory, second_repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/main_import_missing.orna"),
        ),
        ("unused.orna", include_str!("fixtures/unused_nope.orna")),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&second_repository),
        Err(ProjectLoadError::ImportUnavailable)
    ));
}

#[test]
fn applies_limits_before_reading_an_unbounded_project() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/main_import_library.orna"),
        ),
        ("library.orna", include_str!("fixtures/library_one.orna")),
    ]);
    let loader = ProjectLoader::new(ProjectLimits {
        max_modules: 1,
        max_source_bytes: 1024,
        max_repository_entries: 16,
    });
    assert!(matches!(
        loader.load(&repository),
        Err(ProjectLoadError::ModuleLimit)
    ));
}

#[test]
fn applies_total_source_limit_with_a_bounded_read() {
    let (_directory, repository) =
        repository(&[("main.orna", include_str!("fixtures/library_one.orna"))]);
    let loader = ProjectLoader::new(ProjectLimits {
        max_modules: 1,
        max_source_bytes: 1,
        max_repository_entries: 16,
    });
    assert!(matches!(
        loader.load(&repository),
        Err(ProjectLoadError::SourceTooLarge)
    ));
}

#[test]
fn bounds_repository_metadata_before_reachable_source_processing() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            "unreachable.orna",
            include_str!("fixtures/unreachable_unparsed_body.orna"),
        ),
    ]);
    let loader = ProjectLoader::new(ProjectLimits {
        max_modules: 1,
        max_source_bytes: 1024,
        max_repository_entries: 1,
    });
    assert!(matches!(
        loader.load(&repository),
        Err(ProjectLoadError::RepositoryLimit)
    ));
}

#[test]
fn rejects_unreachable_nfkc_casefold_sibling_collisions_without_loading_them() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        ("cafe.orna", include_str!("fixtures/cafe_lower.orna")),
        ("Cafe.orna", include_str!("fixtures/cafe_upper.orna")),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}

#[test]
fn rejects_full_unicode_casefold_sibling_collisions() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            "Straße.orna",
            include_str!("fixtures/unparsed_invalid.orna"),
        ),
        (
            "STRASSE.orna",
            include_str!("fixtures/unparsed_also_invalid.orna"),
        ),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}

#[test]
fn rejects_a_unicode_16_casefold_sibling_collision() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            "\u{10d50}.orna",
            include_str!("fixtures/unparsed_invalid.orna"),
        ),
        (
            "\u{10d70}.orna",
            include_str!("fixtures/unparsed_also_invalid.orna"),
        ),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}

#[test]
fn rejects_unreferenced_file_and_directory_module_ownership_conflicts() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        ("x.orna", include_str!("fixtures/unparsed_invalid.orna")),
        (
            "x/main.orna",
            include_str!("fixtures/unparsed_also_invalid.orna"),
        ),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::DuplicateModuleNamespace)
    ));
}

#[test]
fn rejects_source_modules_that_shadow_reserved_namespaces() {
    for module in ["sys.orna", "std.orna"] {
        let (_directory, repository) = repository(&[
            ("main.orna", include_str!("fixtures/main_run.orna")),
            (module, include_str!("fixtures/unparsed_invalid.orna")),
        ]);
        assert!(matches!(
            ProjectLoader::default().load(&repository),
            Err(ProjectLoadError::ReservedNamespace)
        ));
    }
}

#[test]
fn accepts_nfc_paths_and_skips_git_administration_during_portability_validation() {
    let (directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_cafe_import.orna")),
        ("café.orna", include_str!("fixtures/main_run.orna")),
    ]);
    let git_admin = directory.path().join(".git/orna");
    fs::create_dir_all(&git_admin).unwrap();
    fs::write(git_admin.join("cache"), "unread admin data").unwrap();
    let project = ProjectLoader::default().load(&repository).unwrap();
    assert_eq!(
        project
            .identities()
            .iter()
            .map(|identity| identity.logical_path())
            .collect::<Vec<_>>(),
        ["café.orna", "main.orna"]
    );
}

#[test]
fn rejects_non_nfc_paths_even_when_the_file_is_unreachable() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            "cafe\u{301}.orna",
            include_str!("fixtures/unparsed_invalid.orna"),
        ),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::NonPortablePath)
    ));
}

#[test]
fn accepts_committed_orna_metadata_without_treating_it_as_source() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            ".orna/format.orna",
            include_str!("fixtures/metadata_format.orna"),
        ),
    ]);
    let project = ProjectLoader::default().load(&repository).unwrap();
    assert_eq!(project.modules().len(), 1);
}

#[test]
fn loads_reachable_modules_from_a_committed_snapshot_without_touching_git_state() {
    let (directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_library_seed.orna")),
        (
            "library.orna",
            include_str!("fixtures/library_seed_value.orna"),
        ),
        (
            "unused.orna",
            include_str!("fixtures/unreachable_snapshot_module.orna"),
        ),
        ("README.txt", "ordinary non-source content"),
        (
            ".orna/format.orna",
            include_str!("fixtures/metadata_not_source.orna"),
        ),
    ]);
    commit_all(&directory);
    let commit = repository.resolve_snapshot("HEAD").unwrap();
    let before_head = git_output(&directory, &["rev-parse", "HEAD"]);
    let before_index = git_output(&directory, &["ls-files", "-s"]);
    let before_status = git_output(&directory, &["status", "--porcelain=v1"]);

    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/worktree_ignored_invalid.orna"),
    )
    .unwrap();
    let project = ProjectLoader::default()
        .load_committed_snapshot(&repository, &commit)
        .unwrap();

    assert_eq!(
        project
            .identities()
            .iter()
            .map(|identity| identity.logical_path())
            .collect::<Vec<_>>(),
        ["library.orna", "main.orna"]
    );
    assert_eq!(git_output(&directory, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(git_output(&directory, &["ls-files", "-s"]), before_index);
    assert_eq!(
        git_output(&directory, &["status", "--porcelain=v1"]),
        format!(" M main.orna\n{before_status}")
    );
}

#[test]
fn loads_private_candidate_source_without_reading_human_edits_or_changing_head() {
    const CANDIDATE_SOURCE: &str =
        include_str!("fixtures/semantic_consumer_gap.orna");

    let (directory, candidate_repository) =
        repository(&[("main.orna", include_str!("fixtures/main_from_head.orna"))]);
    commit_all(&directory);
    let base = candidate_repository.resolve_snapshot("HEAD").unwrap();
    let candidate = candidate_repository
        .build_private_commit(
            &base,
            &[ManagedFileChange::new(
                ManagedPath::new("main.orna").unwrap(),
                Some(CANDIDATE_SOURCE.as_bytes().to_vec()),
            )],
            "candidate source",
        )
        .unwrap();

    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/staged_human_edit.orna"),
    )
    .unwrap();
    assert!(
        Command::new("git")
            .args(["add", "main.orna"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/unstaged_human_edit.orna"),
    )
    .unwrap();
    let before_head = git_output(&directory, &["rev-parse", "HEAD"]);
    let before_index = git_output(&directory, &["ls-files", "-s"]);
    let before_status = git_output(&directory, &["status", "--porcelain=v1"]);

    assert_ne!(candidate.commit(), &base);
    let candidate_project = ProjectLoader::default()
        .load_private_candidate(&candidate_repository, &candidate)
        .unwrap();
    assert_eq!(candidate_project.modules().len(), 1);
    assert_eq!(candidate_project.modules()[0].source, CANDIDATE_SOURCE);

    let head_project = ProjectLoader::default()
        .load_committed_snapshot(&candidate_repository, &base)
        .unwrap();
    assert_eq!(head_project.modules().len(), 1);
    assert_eq!(
        head_project.modules()[0].source,
        include_str!("fixtures/main_from_head.orna")
    );
    assert_eq!(git_output(&directory, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(git_output(&directory, &["ls-files", "-s"]), before_index);
    assert_eq!(
        git_output(&directory, &["status", "--porcelain=v1"]),
        before_status
    );
}

#[test]
fn committed_snapshot_loader_enforces_repository_entry_limit_before_reads() {
    let (directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            "unreachable.orna",
            include_str!("fixtures/unreachable_snapshot_invalid.orna"),
        ),
    ]);
    commit_all(&directory);
    let commit = repository.resolve_snapshot("HEAD").unwrap();
    let loader = ProjectLoader::new(ProjectLimits {
        max_modules: 1,
        max_source_bytes: 1024,
        max_repository_entries: 1,
    });

    assert!(matches!(
        loader.load_committed_snapshot(&repository, &commit),
        Err(ProjectLoadError::RepositoryLimit)
    ));
}

#[test]
fn rejects_invalid_non_metadata_module_paths() {
    let (_directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_run.orna")),
        (
            "invalid.name.orna",
            include_str!("fixtures/unparsed_invalid.orna"),
        ),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::UnsafePath)
    ));
}

#[cfg(unix)]
#[test]
fn committed_snapshot_loader_rejects_symlink_entries() {
    use std::os::unix::fs::symlink;

    let (directory, repository) =
        repository(&[("main.orna", include_str!("fixtures/main_run.orna"))]);
    symlink("main.orna", directory.path().join("linked.orna")).unwrap();
    commit_all(&directory);
    let commit = repository.resolve_snapshot("HEAD").unwrap();

    assert!(matches!(
        ProjectLoader::default().load_committed_snapshot(&repository, &commit),
        Err(ProjectLoadError::Symlink)
    ));
}

#[test]
fn committed_snapshot_loader_rejects_submodule_entries() {
    let (directory, repository) =
        repository(&[("main.orna", include_str!("fixtures/main_run.orna"))]);
    commit_all(&directory);
    let object = git_output(&directory, &["rev-parse", "HEAD"]);
    let cache_info = format!("160000,{},vendor", object.trim());
    assert!(
        Command::new("git")
            .args(["update-index", "--add", "--cacheinfo", &cache_info])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "--quiet", "-m", "gitlink"])
            .current_dir(directory.path())
            .status()
            .unwrap()
            .success()
    );
    let commit = repository.resolve_snapshot("HEAD").unwrap();

    assert!(matches!(
        ProjectLoader::default().load_committed_snapshot(&repository, &commit),
        Err(ProjectLoadError::UnsafePath)
    ));
}

#[cfg(unix)]
#[test]
fn rejects_unreachable_symlinks_during_metadata_preflight() {
    use std::os::unix::fs::symlink;

    let (directory, repository) =
        repository(&[("main.orna", include_str!("fixtures/main_run.orna"))]);
    symlink("main.orna", directory.path().join("unreachable.orna")).unwrap();
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::Symlink)
    ));
}

#[test]
fn discovers_only_reachable_table_rows_with_opaque_path_metadata() {
    let (directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_contacts.orna")),
        (
            "contacts.orna",
            include_str!("fixtures/contacts_table.orna"),
        ),
        (
            "contacts/Contact/42.orna",
            include_str!("fixtures/contact_42.orna"),
        ),
        (
            "contacts/Contact/malformed.orna",
            include_str!("fixtures/contact_malformed.orna"),
        ),
        (
            "unused/Contact/1.orna",
            include_str!("fixtures/unreachable_contact_row.orna"),
        ),
    ]);
    commit_all(&directory);

    let worktree = ProjectLoader::default().load(&repository).unwrap();
    assert_eq!(worktree.loose_rows().len(), 2);
    assert_eq!(
        worktree.loose_rows()[0].logical_path(),
        "contacts/Contact/42.orna"
    );
    assert_eq!(worktree.loose_rows()[0].table_path(), "contacts/Contact");
    assert_eq!(worktree.loose_rows()[0].key_path(), ["42.orna"]);
    assert_eq!(worktree.loose_rows()[0].parse_as(), "row_unit");
    assert_eq!(
        worktree.loose_rows()[1].source(),
        include_str!("fixtures/contact_malformed.orna")
    );
    assert!(
        !worktree
            .loose_rows()
            .iter()
            .any(|row| row.logical_path() == "unused/Contact/1.orna")
    );

    let commit = repository.resolve_snapshot("HEAD").unwrap();
    let committed = ProjectLoader::default()
        .load_committed_snapshot(&repository, &commit)
        .unwrap();
    assert_eq!(
        committed
            .loose_rows()
            .iter()
            .map(|row| row.logical_path())
            .collect::<Vec<_>>(),
        [
            "contacts/Contact/42.orna",
            "contacts/Contact/malformed.orna"
        ]
    );
}

#[cfg(unix)]
#[test]
fn worktree_row_discovery_skips_unreadable_git_administration() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, repository) = repository(&[
        ("main.orna", include_str!("fixtures/main_contacts.orna")),
        (
            "contacts.orna",
            include_str!("fixtures/contacts_table.orna"),
        ),
        (
            "contacts/Contact/42.orna",
            include_str!("fixtures/contact_42.orna"),
        ),
    ]);

    let unreadable = directory.path().join(".git/unreadable/nested");
    fs::create_dir_all(&unreadable).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0)).unwrap();

    let result = ProjectLoader::default().load(&repository);
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();

    let loaded = result.unwrap();
    assert_eq!(
        loaded
            .loose_rows()
            .iter()
            .map(|row| row.logical_path())
            .collect::<Vec<_>>(),
        ["contacts/Contact/42.orna"]
    );
    assert_eq!(
        loaded.loose_rows()[0].source(),
        include_str!("fixtures/contact_42.orna")
    );
}
