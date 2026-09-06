use std::{fs, path::Path, process::Command};

use orna_project_v1::{ProjectLimits, ProjectLoadError, ProjectLoader};
use orna_repository_v1::Repository;
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

#[test]
fn loads_only_reachable_modules_in_deterministic_logical_order() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            "use library; use sensors.greenhouse; pub fn run() {}",
        ),
        ("library.orna", "pub fn seed() {}"),
        ("sensors/greenhouse/main.orna", "pub fn ingest() {}"),
        ("unused.orna", "@@ not a module body"),
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
fn carries_only_an_explicit_standard_dependency_profile() {
    let (_directory, repository) = repository(&[("main.orna", "pub fn run() {}")]);
    let profile = StandardDependencyProfile::from_sources(
        "std-snapshot-1",
        [(
            "std/math.orna".into(),
            "fn increment(value: Int) = value + 1;".into(),
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
fn unchanged_reference_bundle_loads_and_reaches_v1_semantic_analysis() {
    let reference = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../reference/Orna-1.0.0/examples/reference");
    let directory = tempfile::tempdir().unwrap();
    for name in [
        "main.orna",
        "library.orna",
        "warehouse.orna",
        "sensors.orna",
        "values.orna",
    ] {
        fs::copy(reference.join(name), directory.path().join(name)).unwrap();
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
        ("main.orna", "use library;"),
        ("library.orna", "pub fn one() {}"),
        ("library/main.orna", "pub fn two() {}"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&first_repository),
        Err(ProjectLoadError::DuplicateModuleNamespace)
    ));

    let (_directory, second_repository) = repository(&[
        ("main.orna", "use missing;"),
        ("unused.orna", "pub fn nope() {}"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&second_repository),
        Err(ProjectLoadError::ImportUnavailable)
    ));
}

#[test]
fn applies_limits_before_reading_an_unbounded_project() {
    let (_directory, repository) = repository(&[
        ("main.orna", "use library;"),
        ("library.orna", "pub fn one() {}"),
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
    let (_directory, repository) = repository(&[("main.orna", "pub fn one() {}")]);
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
        ("main.orna", "pub fn run() {}"),
        ("unreachable.orna", "this body must never be parsed"),
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
        ("main.orna", "pub fn run() {}"),
        ("cafe.orna", "pub fn lower() {}"),
        ("Cafe.orna", "pub fn upper() {}"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}

#[test]
fn rejects_full_unicode_casefold_sibling_collisions() {
    let (_directory, repository) = repository(&[
        ("main.orna", "pub fn run() {}"),
        ("Straße.orna", "not parsed"),
        ("STRASSE.orna", "also not parsed"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}

#[test]
fn rejects_a_unicode_16_casefold_sibling_collision() {
    let (_directory, repository) = repository(&[
        ("main.orna", "pub fn run() {}"),
        ("\u{10d50}.orna", "not parsed"),
        ("\u{10d70}.orna", "also not parsed"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}

#[test]
fn rejects_unreferenced_file_and_directory_module_ownership_conflicts() {
    let (_directory, repository) = repository(&[
        ("main.orna", "pub fn run() {}"),
        ("x.orna", "not parsed"),
        ("x/main.orna", "also not parsed"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::DuplicateModuleNamespace)
    ));
}

#[test]
fn rejects_source_modules_that_shadow_reserved_namespaces() {
    for module in ["sys.orna", "std.orna"] {
        let (_directory, repository) =
            repository(&[("main.orna", "pub fn run() {}"), (module, "not parsed")]);
        assert!(matches!(
            ProjectLoader::default().load(&repository),
            Err(ProjectLoadError::ReservedNamespace)
        ));
    }
}

#[test]
fn accepts_nfc_paths_and_skips_git_administration_during_portability_validation() {
    let (directory, repository) =
        repository(&[("main.orna", "use café;"), ("café.orna", "pub fn run() {}")]);
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
        ("main.orna", "pub fn run() {}"),
        ("cafe\u{301}.orna", "not parsed"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::NonPortablePath)
    ));
}

#[test]
fn accepts_committed_orna_metadata_without_treating_it_as_source() {
    let (_directory, repository) = repository(&[
        ("main.orna", "pub fn run() {}"),
        (".orna/format.orna", "metadata is not a module"),
    ]);
    let project = ProjectLoader::default().load(&repository).unwrap();
    assert_eq!(project.modules().len(), 1);
}

#[test]
fn rejects_invalid_non_metadata_module_paths() {
    let (_directory, repository) = repository(&[
        ("main.orna", "pub fn run() {}"),
        ("invalid.name.orna", "not parsed"),
    ]);
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::UnsafePath)
    ));
}

#[cfg(unix)]
#[test]
fn rejects_unreachable_symlinks_during_metadata_preflight() {
    use std::os::unix::fs::symlink;

    let (directory, repository) = repository(&[("main.orna", "pub fn run() {}")]);
    symlink("main.orna", directory.path().join("unreachable.orna")).unwrap();
    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::Symlink)
    ));
}
