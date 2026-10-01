use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_project_v1::{LoadedProject, ProjectLoader};
use orna_repository_v1::{Repository, initialize_repository};
use orna_semantic_v1::StandardDependencyProfile;
use orna_value_v1::{Raw, Value};
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
        StandardDependencyProfile::from_sources(snapshot_v2.clone(), sources_v2.clone()).unwrap();
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

fn int(value: i64) -> CanonicalValue {
    CanonicalValue::new(Value::int(value.into()).raw().clone()).unwrap()
}

fn capture_standard_gitlink(
    project_path: &Path,
    standard_snapshot: &str,
    message: &str,
) -> String {
    git_output_at(project_path, &["add", "main.orna", ".gitmodules"]);
    let gitlink = format!("160000,{standard_snapshot},stdlib/std");
    git_output_at(
        project_path,
        &["update-index", "--add", "--cacheinfo", &gitlink],
    );
    git_output_at(project_path, &["commit", "--quiet", "-m", message]);
    git_output_at(project_path, &["rev-parse", "HEAD"])
}

fn module_upgrade_projects() -> (TempDir, LoadedProject, LoadedProject, LoadedProject, [String; 3]) {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("project");
    fs::create_dir_all(&project_path).unwrap();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/module-upgrade-project.orna"),
    )
    .unwrap();
    git_output_at(&project_path, &["init", "--quiet"]);
    for (key, value) in [
        ("user.email", "kieran@drewett.dev"),
        ("user.name", "kierandrewett"),
        ("commit.gpgsign", "false"),
    ] {
        git_output_at(&project_path, &["config", key, value]);
    }
    fs::write(
        project_path.join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/ornadb-std.git\n",
    )
    .unwrap();

    let standard_path = project_path.join("stdlib/std");
    fs::create_dir_all(&standard_path).unwrap();
    initialize_repository(&standard_path).unwrap();
    fs::write(
        standard_path.join("main.orna"),
        include_str!("fixtures/module-upgrade-std-main.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v1.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "standard module v1");
    let snapshot_v1 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v1 = capture_standard_gitlink(&project_path, &snapshot_v1, "capture std v1");

    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v2.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "standard module v2");
    let snapshot_v2 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v2 = capture_standard_gitlink(&project_path, &snapshot_v2, "capture std v2");

    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v3.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "standard module v3");
    let snapshot_v3 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    capture_standard_gitlink(&project_path, &snapshot_v3, "capture std v3");

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let parent_v1 = repository.resolve_snapshot(&parent_v1).unwrap();
    let parent_v2 = repository.resolve_snapshot(&parent_v2).unwrap();
    let historical = loader
        .load_committed_snapshot(&repository, &parent_v1)
        .unwrap();
    let intermediate = loader
        .load_committed_snapshot(&repository, &parent_v2)
        .unwrap();
    let upgraded = loader.load(&repository).unwrap();
    (
        directory,
        historical,
        intermediate,
        upgraded,
        [snapshot_v1, snapshot_v2, snapshot_v3],
    )
}

#[test]
fn project_gitlink_upgrade_keeps_historical_standard_sources_available() {
    let (_directory, historical, intermediate, upgraded, snapshots) = module_upgrade_projects();

    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    assert_eq!(
        historical.standard_profile().unwrap().snapshot(),
        snapshots[0]
    );
    assert_eq!(
        intermediate.standard_profile().unwrap().snapshot(),
        snapshots[1]
    );
    assert_eq!(
        upgraded.standard_profile().unwrap().snapshot(),
        snapshots[2]
    );
    for (project, expected) in [
        (&historical, include_str!("fixtures/module-upgrade-std-v1.orna")),
        (
            &intermediate,
            include_str!("fixtures/module-upgrade-std-v2.orna"),
        ),
        (&upgraded, include_str!("fixtures/module-upgrade-std-v3.orna")),
    ] {
        assert_eq!(
            project
                .standard_sources()
                .iter()
                .find(|(path, _)| path == "std/math.orna")
                .unwrap()
                .1,
            expected
        );
    }
}

fn admitted_upgrade_session(project: &LoadedProject) -> AdmittedReplSession {
    let mut session = AdmittedReplSession::from_loaded_project(
        project,
        project.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("failed to admit upgraded project: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/snapshot-replay-use-math.orna")),
        Ok(None)
    );
    session
}

#[test]
fn module_upgrade_sessions_replay_with_their_captured_standard_snapshot() {
    let (_directory, project_v1, project_v2, project_v3, snapshots) = module_upgrade_projects();
    let mut session_v1 = admitted_upgrade_session(&project_v1);
    let mut session_v2 = admitted_upgrade_session(&project_v2);
    let mut session_v3 = admitted_upgrade_session(&project_v3);

    for (session, direct, callback) in [
        (&mut session_v1, 8, &[11][..]),
        (&mut session_v2, 107, &[110][..]),
        (&mut session_v3, 1007, &[1010][..]),
    ] {
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-replay-direct-seven.orna")),
            Ok(Some(int(direct)))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-replay-callback-seven.orna")),
            Ok(Some(ints(callback)))
        );
    }

    assert_eq!(project_v1.standard_profile().unwrap().snapshot(), snapshots[0]);
    assert_eq!(project_v2.standard_profile().unwrap().snapshot(), snapshots[1]);
    assert_eq!(project_v3.standard_profile().unwrap().snapshot(), snapshots[2]);
    assert_eq!(
        session_v1.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[13])))
    );
    assert_eq!(
        session_v2.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[112])))
    );
    assert_eq!(
        session_v3.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[1012])))
    );
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
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &project_v1,
            sources_v2.clone(),
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );

    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    assert_eq!(
        historical.submit(include_str!("fixtures/snapshot-replay-use-math.orna")),
        Ok(None)
    );
    let historical_direct = historical
        .submit(include_str!("fixtures/snapshot-replay-direct-seven.orna"))
        .unwrap_or_else(|error| panic!("historical source export call failed: {}", error.code()));
    assert_eq!(historical_direct, Some(int(8)));
    let source = include_str!("fixtures/snapshot-replay-callback-seven.orna");
    let parsed = orna_syntax_v1::parse_repl(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let historical_callback = historical
        .submit(source)
        .unwrap_or_else(|error| panic!("historical callback failed: {}", error.code()));
    assert_eq!(historical_callback, Some(ints(&[11])));
    let mut replay = historical.clone();

    let mut current =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_eq!(
        current.submit(include_str!("fixtures/snapshot-replay-use-math.orna")),
        Ok(None)
    );
    assert_eq!(
        current.submit(include_str!("fixtures/snapshot-replay-direct-seven.orna")),
        Ok(Some(int(107)))
    );
    assert_eq!(current.submit(source), Ok(Some(ints(&[110]))));
    assert_eq!(
        replay.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[13])))
    );
}
