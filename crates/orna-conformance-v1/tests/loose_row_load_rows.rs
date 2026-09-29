use std::{fs, process::Command};

use orna_project_v1::ProjectLoader;
use orna_repository_v1::Repository;
use orna_semantic_v1::{Catalogue, RowUnitInput, admit_row_unit, analyze_with_catalogue};
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
fn complete_project_loads_and_validates_every_loose_row() {
    let (_directory, repository) = repository(&[
        (
            "main.orna",
            include_str!("fixtures/loose-rows/main.orna"),
        ),
        (
            "Contact/north/42.orna",
            include_str!("fixtures/loose-rows/valid.orna"),
        ),
        (
            "Contact/south/7.orna",
            include_str!("fixtures/loose-rows/invalid-field-unit.orna"),
        ),
        (
            "Contact/east/8.orna",
            include_str!("fixtures/loose-rows/unknown-field.orna"),
        ),
        (
            "Contact/west/9.orna",
            include_str!("fixtures/loose-rows/missing-field.orna"),
        ),
        (
            "Contact/central/not-an-int.orna",
            include_str!("fixtures/loose-rows/valid.orna"),
        ),
    ]);

    let project = ProjectLoader::default().load(&repository).unwrap();
    assert_eq!(project.modules().len(), 1);
    assert_eq!(project.loose_rows().len(), 5);
    let analysis = analyze_with_catalogue(project.modules(), &Catalogue::authoritative_fixture());
    assert!(analysis.is_ok(), "table declaration failed: {:?}", analysis.diagnostics);

    let mut passed = Vec::new();
    let mut rejected = Vec::new();
    for row in project.loose_rows() {
        let input = RowUnitInput {
            logical_path: row.logical_path(),
            table_path: row.table_path(),
            key_path: row.key_path(),
            source: row.source(),
            source_bytes: row.source_bytes(),
            parse_as: row.parse_as(),
        };
        let admission = admit_row_unit(&analysis, &input);
        if admission.is_ok() {
            passed.push((row.logical_path(), admission));
        } else {
            rejected.push(row.logical_path());
        }
    }

    assert_eq!(passed.len(), 1);
    assert_eq!(passed[0].0, "Contact/north/42.orna");
    assert_eq!(passed[0].1.owner.as_deref(), Some("Contact"));
    assert_eq!(passed[0].1.key, ["north", "42"]);
    assert_eq!(
        rejected,
        [
            "Contact/central/not-an-int.orna",
            "Contact/east/8.orna",
            "Contact/south/7.orna",
            "Contact/west/9.orna",
        ]
    );
}
