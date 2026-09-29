use std::{fs, path::Path};

use orna_project_v1::{ProjectLoadError, ProjectLoader};
use orna_repository_v1::Repository;
use tempfile::TempDir;

fn repository(files: &[(&str, &str)]) -> (TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let path = directory.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(directory.path())
        .status()
        .unwrap();
    assert!(status.success());
    let repository = Repository::discover(directory.path()).unwrap();
    (directory, repository)
}

fn write_source(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}

// Specified: ORNA-NS-001/002/003, source/03-source-modules.md:22,24,26.
// Exists: checked-in root, module, and directory-main source fixtures.
// Passed: these three logical-path to namespace assignments only.
#[test]
fn root_module_file_module_and_directory_main_define_their_namespaces() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/requirement-tail-ns/root-imports.orna"),
        ),
        (
            "report.orna",
            include_str!("fixtures/requirement-tail-ns/report.orna"),
        ),
        (
            "warehouse/main.orna",
            include_str!("fixtures/requirement-tail-ns/warehouse-main.orna"),
        ),
    ]);

    let project = ProjectLoader::default().load(&repository).unwrap();
    let identities = project
        .identities()
        .iter()
        .map(|identity| {
            (
                identity.logical_path(),
                identity
                    .namespace()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        identities,
        [
            ("main.orna", vec![]),
            ("report.orna", vec!["report"]),
            ("warehouse/main.orna", vec!["warehouse"]),
        ]
    );
}

// Specified: ORNA-NS-004, source/03-source-modules.md:28.
// Exists: checked-in colliding file and directory-main source fixtures.
// Passed: rejecting the duplicate logical module namespace.
#[test]
fn rejects_file_and_directory_main_that_own_the_same_namespace() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/requirement-tail-ns/root.orna"),
        ),
        (
            "reports.orna",
            include_str!("fixtures/requirement-tail-ns/module.orna"),
        ),
        (
            "reports/main.orna",
            include_str!("fixtures/requirement-tail-ns/module.orna"),
        ),
    ]);

    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::DuplicateModuleNamespace)
    ));
}

// Specified: ORNA-NS-005, source/03-source-modules.md:32.
// Exists: checked-in reserved-name fixtures and the project loader.
// Passed is bounded to rejecting source-owned sys and std namespaces.
#[test]
fn rejects_source_owned_reserved_namespaces() {
    for module in ["sys.orna", "std.orna"] {
        let (_directory, repository) = repository(&[
            (
                "main.orna",
                include_str!("fixtures/requirement-tail-ns/root.orna"),
            ),
            (
                module,
                include_str!("fixtures/requirement-tail-ns/module.orna"),
            ),
        ]);
        assert!(matches!(
            ProjectLoader::default().load(&repository),
            Err(ProjectLoadError::ReservedNamespace)
        ));
    }

    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/requirement-tail-ns/root.orna"),
        ),
        (
            "custom.orna",
            include_str!("fixtures/requirement-tail-ns/module.orna"),
        ),
    ]);
    assert!(ProjectLoader::default().load(&repository).is_ok());
}

// Specified: ORNA-NS-008, source/03-source-modules.md:38.
// Exists: checked-in Orna source fixture with Unicode-colliding sibling paths.
// Passed: host-independent Unicode 16 case-fold collision rejection on load.
#[test]
fn rejects_unicode_16_casefold_colliding_sibling_paths() {
    let (directory, repository) = repository(&[(
        "main.orna",
        include_str!("fixtures/requirement-tail-ns/root.orna"),
    )]);
    write_source(
        directory.path(),
        "\u{10d50}.orna",
        include_str!("fixtures/requirement-tail-ns/module.orna"),
    );
    write_source(
        directory.path(),
        "\u{10d70}.orna",
        include_str!("fixtures/requirement-tail-ns/module.orna"),
    );

    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::SiblingCollision)
    ));
}
