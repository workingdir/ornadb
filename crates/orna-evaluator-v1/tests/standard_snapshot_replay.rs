use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_project_v1::{LoadedProject, ProjectLoader};
use orna_repository_v1::{Repository, initialize_repository};
use orna_semantic_v1::StandardDependencyProfile;
use orna_value_v1::Raw;
use tempfile::TempDir;

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

fn standard_sources(math_source: &str) -> Vec<(String, String)> {
    [
        (
            "std/main.orna",
            include_str!("fixtures/snapshot-replay-std-main.orna"),
        ),
        (
            "std/collection.orna",
            include_str!("fixtures/snapshot-replay-std-collection.orna"),
        ),
        ("std/math.orna", math_source),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), source.to_owned()))
    .collect()
}

fn snapshot_projects() -> (
    TempDir,
    LoadedProject,
    LoadedProject,
    String,
    String,
    Vec<(String, String)>,
    Vec<(String, String)>,
) {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("project");
    fs::create_dir_all(&project_path).unwrap();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/snapshot-replay-project.orna"),
    )
    .unwrap();
    git_output_at(&project_path, &["init", "--quiet"]);
    let repository = Repository::discover(&project_path).unwrap();

    let standard_path = directory.path().join("standard");
    fs::create_dir_all(&standard_path).unwrap();
    initialize_repository(&standard_path).unwrap();
    fs::write(
        standard_path.join("main.orna"),
        include_str!("fixtures/snapshot-replay-std-main.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/snapshot-replay-std-collection.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/snapshot-replay-std-v1.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "std snapshot v1");
    let snapshot_v1 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let sources_v1 = standard_sources(include_str!("fixtures/snapshot-replay-std-v1.orna"));
    let profile_v1 =
        StandardDependencyProfile::from_sources(snapshot_v1.clone(), sources_v1.clone()).unwrap();
    let loader = ProjectLoader::default();
    let project_v1 = loader
        .load_with_standard_profile(&repository, Some(profile_v1))
        .unwrap();

    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/snapshot-replay-std-v2.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "std snapshot v2");
    let snapshot_v2 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let sources_v2 = standard_sources(include_str!("fixtures/snapshot-replay-std-v2.orna"));
    let profile_v2 =
        StandardDependencyProfile::from_sources(snapshot_v2.clone(), sources_v2).unwrap();
    let project_v2 = loader
        .load_with_standard_profile(&repository, Some(profile_v2))
        .unwrap();

    (
        directory,
        project_v1,
        project_v2,
        snapshot_v1,
        snapshot_v2,
        sources_v1,
        sources_v2,
    )
}

fn ints(values: &[i64]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values
            .iter()
            .map(|value| Raw::Int((*value).into()))
            .collect(),
    ))
    .unwrap()
}

#[test]
fn replayed_closure_uses_its_captured_dependency_snapshot_after_snapshot_changes() {
    let (_directory, project_v1, project_v2, snapshot_v1, snapshot_v2, sources_v1, sources_v2) =
        snapshot_projects();
    assert_eq!(
        project_v1.standard_profile().unwrap().snapshot(),
        snapshot_v1
    );
    assert_eq!(
        project_v2.standard_profile().unwrap().snapshot(),
        snapshot_v2
    );
    assert_ne!(snapshot_v1, snapshot_v2);
    assert_eq!(
        sources_v1
            .iter()
            .find(|(path, _)| path == "std/math.orna")
            .unwrap()
            .1,
        include_str!("fixtures/snapshot-replay-std-v1.orna")
    );
    assert_eq!(
        sources_v2
            .iter()
            .find(|(path, _)| path == "std/math.orna")
            .unwrap()
            .1,
        include_str!("fixtures/snapshot-replay-std-v2.orna")
    );

    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let source = include_str!("fixtures/snapshot-replay-callback-seven.orna");
    assert_eq!(historical.submit(source), Ok(Some(ints(&[8]))));
    let mut replay = historical.clone();

    let mut current =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_eq!(current.submit(source), Ok(Some(ints(&[107]))));
    assert_eq!(
        replay.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[10])))
    );
}
