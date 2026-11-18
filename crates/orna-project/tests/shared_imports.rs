//! A module that many modules import resolves to the same file for every
//! importer, and is loaded once.

use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use orna_project::{ProjectLoadError, ProjectLoader};
use orna_repository::Repository;
use tempfile::TempDir;

fn repository(files: &[(String, String)]) -> (TempDir, Repository) {
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

fn file(path: &str, source: &str) -> (String, String) {
    (path.to_owned(), source.to_owned())
}

fn logical_paths(project: &orna_project::LoadedProject) -> Vec<&str> {
    project
        .identities()
        .iter()
        .map(|identity| identity.logical_path())
        .collect()
}

/// Both spellings of a module, `shared.orna` and `pkg/main.orna`, are imported
/// by several modules, and each importer reaches the same file.
#[test]
fn modules_that_import_one_module_each_resolve_it() {
    let (_directory, repository) = repository(&[
        file("main.orna", "use first;\nuse second;\nuse third;\n"),
        file("first.orna", "use shared;\nuse pkg;\n"),
        file("second.orna", "use shared;\nuse pkg;\n"),
        file("third.orna", "use pkg;\nuse shared;\n"),
        file("shared.orna", "pub fn value(): Int = 1;\n"),
        file("pkg/main.orna", "pub fn value(): Int = 2;\n"),
        file("unused.orna", "pub fn unused(): Int = 3;\n"),
    ]);

    let project = ProjectLoader::default().load(&repository).unwrap();

    assert_eq!(
        logical_paths(&project),
        [
            "first.orna",
            "main.orna",
            "pkg/main.orna",
            "second.orna",
            "shared.orna",
            "third.orna",
        ]
    );
}

/// An overlay-only module has no file to find, and still resolves for every
/// module that imports it.
#[test]
fn an_overlay_only_module_resolves_for_each_importer() {
    let (_directory, repository) = repository(&[
        file("main.orna", "use first;\nuse second;\n"),
        file("first.orna", "use helpers;\n"),
        file("second.orna", "use helpers;\n"),
    ]);
    let overlays = BTreeMap::from([(
        "helpers.orna".to_owned(),
        "pub fn value(): Int = 3;\n".to_owned(),
    )]);

    let project = ProjectLoader::default()
        .load_with_overlays(&repository, &overlays)
        .unwrap();

    assert_eq!(
        logical_paths(&project),
        ["first.orna", "helpers.orna", "main.orna", "second.orna"]
    );
}

/// A path that resolved for one importer is not assumed to resolve for the
/// next: a path that names nothing is refused whichever module reaches it, even
/// after other imports succeeded.
#[test]
fn a_missing_import_is_refused_after_other_imports_resolved() {
    let (_directory, repository) = repository(&[
        file("main.orna", "use first;\nuse second;\n"),
        file("first.orna", "use shared;\n"),
        file("second.orna", "use shared;\nuse absent;\n"),
        file("shared.orna", "pub fn value(): Int = 1;\n"),
    ]);

    assert!(matches!(
        ProjectLoader::default().load(&repository),
        Err(ProjectLoadError::ImportUnavailable)
    ));
}

/// Every module imports every other module, which is the shape that made the
/// loader check each import path once per importer.
#[test]
fn modules_that_import_each_other_load_once_in_logical_order() {
    const COUNT: usize = 40;
    let name = |index: usize| format!("m{index:02}");
    let mut files = vec![file(
        "main.orna",
        &(0..COUNT)
            .map(|index| format!("use {};\n", name(index)))
            .collect::<String>(),
    )];
    for index in 0..COUNT {
        let imports = (0..COUNT)
            .filter(|other| *other != index)
            .map(|other| format!("use {};\n", name(other)))
            .collect::<String>();
        files.push((
            format!("{}.orna", name(index)),
            format!("{imports}pub fn value(): Int = {index};\n"),
        ));
    }
    let (_directory, repository) = repository(&files);

    let project = ProjectLoader::default().load(&repository).unwrap();

    let mut expected = (0..COUNT)
        .map(|index| format!("{}.orna", name(index)))
        .collect::<Vec<_>>();
    expected.push("main.orna".to_owned());
    expected.sort();
    assert_eq!(logical_paths(&project), expected);
    assert_eq!(project.modules().len(), COUNT + 1);
}
