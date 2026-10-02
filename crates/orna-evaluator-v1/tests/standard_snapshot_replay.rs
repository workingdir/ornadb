use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

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

fn capture_standard_gitlink(project_path: &Path, standard_snapshot: &str, message: &str) -> String {
    git_output_at(project_path, &["add", "main.orna", ".gitmodules"]);
    let gitlink = format!("160000,{standard_snapshot},stdlib/std");
    git_output_at(
        project_path,
        &["update-index", "--add", "--cacheinfo", &gitlink],
    );
    git_output_at(project_path, &["commit", "--quiet", "-m", message]);
    git_output_at(project_path, &["rev-parse", "HEAD"])
}

fn module_upgrade_projects() -> (
    TempDir,
    LoadedProject,
    LoadedProject,
    LoadedProject,
    LoadedProject,
    LoadedProject,
    LoadedProject,
    [String; 6],
) {
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
    let parent_v3 = capture_standard_gitlink(&project_path, &snapshot_v3, "capture std v3");

    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "collection module v4");
    let snapshot_v4 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v4 = capture_standard_gitlink(&project_path, &snapshot_v4, "capture std v4");

    // Each later commit upgrades both imported modules under one gitlink pin.
    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "upgrade math and collection v5");
    let snapshot_v5 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v5 = capture_standard_gitlink(&project_path, &snapshot_v5, "capture std v5");

    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v5.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "upgrade math and collection v6");
    let snapshot_v6 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v6 = capture_standard_gitlink(&project_path, &snapshot_v6, "capture std v6");

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let parent_v1 = repository.resolve_snapshot(&parent_v1).unwrap();
    let parent_v2 = repository.resolve_snapshot(&parent_v2).unwrap();
    let parent_v3 = repository.resolve_snapshot(&parent_v3).unwrap();
    let parent_v4 = repository.resolve_snapshot(&parent_v4).unwrap();
    let parent_v5 = repository.resolve_snapshot(&parent_v5).unwrap();
    let parent_v6 = repository.resolve_snapshot(&parent_v6).unwrap();
    let historical = loader
        .load_committed_snapshot(&repository, &parent_v1)
        .unwrap();
    let intermediate = loader
        .load_committed_snapshot(&repository, &parent_v2)
        .unwrap();
    let latest_math = loader
        .load_committed_snapshot(&repository, &parent_v3)
        .unwrap();
    let upgraded = loader
        .load_committed_snapshot(&repository, &parent_v4)
        .unwrap();
    let upgraded_both = loader
        .load_committed_snapshot(&repository, &parent_v5)
        .unwrap();
    let upgraded_both_again = loader
        .load_committed_snapshot(&repository, &parent_v6)
        .unwrap();
    (
        directory,
        historical,
        intermediate,
        latest_math,
        upgraded,
        upgraded_both,
        upgraded_both_again,
        [
            snapshot_v1,
            snapshot_v2,
            snapshot_v3,
            snapshot_v4,
            snapshot_v5,
            snapshot_v6,
        ],
    )
}

fn write_module_chain_version(
    standard_path: &Path,
    math: &str,
    collection: &str,
    entry: &str,
    bridge: &str,
    leaf: &str,
) {
    for (logical_path, source) in [
        (
            "main.orna",
            include_str!("fixtures/module-chain-std-main.orna"),
        ),
        ("math.orna", math),
        ("collection.orna", collection),
        ("chain/entry.orna", entry),
        ("chain/bridge.orna", bridge),
        ("chain/leaf.orna", leaf),
    ] {
        let path = standard_path.join(logical_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
}

fn module_chain_repository() -> (TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("project");
    fs::create_dir_all(&project_path).unwrap();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/module-chain-project.orna"),
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
    (directory, project_path, standard_path)
}

fn module_chain_projects() -> (TempDir, LoadedProject, LoadedProject, [String; 2]) {
    let (directory, project_path, standard_path) = module_chain_repository();
    write_module_chain_version(
        &standard_path,
        include_str!("fixtures/module-chain-std-math-v1.orna"),
        include_str!("fixtures/module-chain-std-collection-v1.orna"),
        include_str!("fixtures/module-chain-std-entry-v1.orna"),
        include_str!("fixtures/module-chain-std-bridge-v1.orna"),
        include_str!("fixtures/module-chain-std-leaf-v1.orna"),
    );
    commit_directory(&standard_path, "standard module chain v1");
    let snapshot_v1 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v1 = capture_standard_gitlink(&project_path, &snapshot_v1, "capture chain v1");

    write_module_chain_version(
        &standard_path,
        include_str!("fixtures/module-chain-std-math-v2.orna"),
        include_str!("fixtures/module-chain-std-collection-v2.orna"),
        include_str!("fixtures/module-chain-std-entry-v2.orna"),
        include_str!("fixtures/module-chain-std-bridge-v2.orna"),
        include_str!("fixtures/module-chain-std-leaf-v2.orna"),
    );
    commit_directory(&standard_path, "standard module chain v2");
    let snapshot_v2 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v2 = capture_standard_gitlink(&project_path, &snapshot_v2, "capture chain v2");

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let parent_v1 = repository.resolve_snapshot(&parent_v1).unwrap();
    let parent_v2 = repository.resolve_snapshot(&parent_v2).unwrap();
    let historical = loader
        .load_committed_snapshot(&repository, &parent_v1)
        .unwrap();
    let upgraded = loader
        .load_committed_snapshot(&repository, &parent_v2)
        .unwrap();
    (directory, historical, upgraded, [snapshot_v1, snapshot_v2])
}

fn module_chain_upgrade_projects() -> (
    TempDir,
    LoadedProject,
    LoadedProject,
    LoadedProject,
    [String; 3],
) {
    let (directory, project_path, standard_path) = module_chain_repository();
    write_module_chain_version(
        &standard_path,
        include_str!("fixtures/module-chain-std-math-v1.orna"),
        include_str!("fixtures/module-chain-std-collection-v1.orna"),
        include_str!("fixtures/module-chain-std-entry-v1.orna"),
        include_str!("fixtures/module-chain-std-bridge-v1.orna"),
        include_str!("fixtures/module-chain-std-leaf-v1.orna"),
    );
    commit_directory(&standard_path, "standard module chain v1");
    let snapshot_v1 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v1 = capture_standard_gitlink(&project_path, &snapshot_v1, "capture chain v1");

    write_module_chain_version(
        &standard_path,
        include_str!("fixtures/module-chain-std-math-v2.orna"),
        include_str!("fixtures/module-chain-std-collection-v2.orna"),
        include_str!("fixtures/module-chain-std-entry-v2.orna"),
        include_str!("fixtures/module-chain-std-bridge-v2.orna"),
        include_str!("fixtures/module-chain-std-leaf-v2.orna"),
    );
    commit_directory(&standard_path, "standard module chain v2");
    let snapshot_v2 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v2 = capture_standard_gitlink(&project_path, &snapshot_v2, "capture chain v2");

    write_module_chain_version(
        &standard_path,
        include_str!("fixtures/module-chain-std-math-v3.orna"),
        include_str!("fixtures/module-chain-std-collection-v3.orna"),
        include_str!("fixtures/module-chain-std-entry-v3.orna"),
        include_str!("fixtures/module-chain-std-bridge-v3.orna"),
        include_str!("fixtures/module-chain-std-leaf-v3.orna"),
    );
    commit_directory(&standard_path, "standard module chain v3");
    let snapshot_v3 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v3 = capture_standard_gitlink(&project_path, &snapshot_v3, "capture chain v3");

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let historical = loader
        .load_committed_snapshot(
            &repository,
            &repository.resolve_snapshot(&parent_v1).unwrap(),
        )
        .unwrap();
    let intermediate = loader
        .load_committed_snapshot(
            &repository,
            &repository.resolve_snapshot(&parent_v2).unwrap(),
        )
        .unwrap();
    let upgraded = loader
        .load_committed_snapshot(
            &repository,
            &repository.resolve_snapshot(&parent_v3).unwrap(),
        )
        .unwrap();
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
    let (
        _directory,
        historical,
        intermediate,
        latest_math,
        upgraded,
        upgraded_both,
        upgraded_both_again,
        snapshots,
    ) = module_upgrade_projects();

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
        latest_math.standard_profile().unwrap().snapshot(),
        snapshots[2]
    );
    assert_eq!(
        upgraded.standard_profile().unwrap().snapshot(),
        snapshots[3]
    );
    assert_eq!(
        upgraded_both.standard_profile().unwrap().snapshot(),
        snapshots[4]
    );
    assert_eq!(
        upgraded_both_again.standard_profile().unwrap().snapshot(),
        snapshots[5]
    );
    for (project, expected) in [
        (
            &historical,
            include_str!("fixtures/module-upgrade-std-v1.orna"),
        ),
        (
            &intermediate,
            include_str!("fixtures/module-upgrade-std-v2.orna"),
        ),
        (
            &latest_math,
            include_str!("fixtures/module-upgrade-std-v3.orna"),
        ),
        (
            &upgraded,
            include_str!("fixtures/module-upgrade-std-v3.orna"),
        ),
        (
            &upgraded_both,
            include_str!("fixtures/module-upgrade-std-v4.orna"),
        ),
        (
            &upgraded_both_again,
            include_str!("fixtures/module-upgrade-std-v5.orna"),
        ),
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
    for (project, expected) in [
        (
            &historical,
            include_str!("fixtures/module-upgrade-std-collection.orna"),
        ),
        (
            &intermediate,
            include_str!("fixtures/module-upgrade-std-collection.orna"),
        ),
        (
            &latest_math,
            include_str!("fixtures/module-upgrade-std-collection.orna"),
        ),
        (
            &upgraded,
            include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        ),
        (
            &upgraded_both,
            include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        ),
        (
            &upgraded_both_again,
            include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
        ),
    ] {
        assert_eq!(
            project
                .standard_sources()
                .iter()
                .find(|(path, _)| path == "std/collection.orna")
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

fn standard_source<'a>(project: &'a LoadedProject, path: &str) -> &'a str {
    project
        .standard_sources()
        .iter()
        .find(|(logical_path, _)| logical_path == path)
        .unwrap()
        .1
        .as_str()
}

#[test]
fn module_upgrade_sessions_replay_with_their_captured_standard_snapshot() {
    let (
        _directory,
        project_v1,
        project_v2,
        project_v3,
        project_v4,
        project_v5,
        project_v6,
        snapshots,
    ) = module_upgrade_projects();
    let mut session_v1 = admitted_upgrade_session(&project_v1);
    let mut session_v2 = admitted_upgrade_session(&project_v2);
    let mut session_v3 = admitted_upgrade_session(&project_v3);
    let mut session_v4 = admitted_upgrade_session(&project_v4);
    let mut session_v5 = admitted_upgrade_session(&project_v5);
    let mut session_v6 = admitted_upgrade_session(&project_v6);

    for (session, direct, callback) in [
        (&mut session_v1, 8, &[11][..]),
        (&mut session_v2, 107, &[110][..]),
        (&mut session_v3, 1007, &[1010][..]),
        (&mut session_v4, 1007, &[1010][..]),
        (&mut session_v5, 10007, &[10010][..]),
        (&mut session_v6, 100007, &[100010][..]),
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

    assert_eq!(
        project_v1.standard_profile().unwrap().snapshot(),
        snapshots[0]
    );
    assert_eq!(
        project_v2.standard_profile().unwrap().snapshot(),
        snapshots[1]
    );
    assert_eq!(
        project_v3.standard_profile().unwrap().snapshot(),
        snapshots[2]
    );
    assert_eq!(
        project_v4.standard_profile().unwrap().snapshot(),
        snapshots[3]
    );
    assert_eq!(
        project_v5.standard_profile().unwrap().snapshot(),
        snapshots[4]
    );
    assert_eq!(
        project_v6.standard_profile().unwrap().snapshot(),
        snapshots[5]
    );
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
    assert_eq!(
        session_v4.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[1012])))
    );
    assert_eq!(
        session_v5.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[10012])))
    );
    assert_eq!(
        session_v6.submit(include_str!("fixtures/snapshot-replay-callback-nine.orna")),
        Ok(Some(ints(&[100012])))
    );
}

#[test]
fn stable_module_replays_across_distinct_snapshot_pins() {
    let (
        _directory,
        _project_v1,
        _project_v2,
        project_v3,
        project_v4,
        _project_v5,
        _project_v6,
        snapshots,
    ) = module_upgrade_projects();
    assert_ne!(snapshots[2], snapshots[3]);
    assert_eq!(
        standard_source(&project_v3, "std/math.orna"),
        standard_source(&project_v4, "std/math.orna")
    );
    assert_ne!(
        standard_source(&project_v3, "std/collection.orna"),
        standard_source(&project_v4, "std/collection.orna")
    );

    let mut session_v3 = admitted_upgrade_session(&project_v3);
    let mut session_v4 = admitted_upgrade_session(&project_v4);
    for session in [&mut session_v3, &mut session_v4] {
        assert_eq!(
            session.submit(include_str!(
                "fixtures/module-upgrade-use-collection-marker.orna"
            )),
            Ok(None)
        );
    }
    for (session, expected_marker) in [(&mut session_v3, 3), (&mut session_v4, 4)] {
        assert_eq!(
            session.submit(include_str!(
                "fixtures/module-upgrade-call-collection-marker.orna"
            )),
            Ok(Some(int(expected_marker)))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-replay-callback-seven.orna")),
            Ok(Some(ints(&[1010])))
        );
    }
    let mut replay_v3 = session_v3.clone();
    let mut replay_v4 = session_v4.clone();
    for (replay, expected_marker) in [(&mut replay_v3, 3), (&mut replay_v4, 4)] {
        assert_eq!(
            replay.submit(include_str!(
                "fixtures/module-upgrade-call-collection-marker.orna"
            )),
            Ok(Some(int(expected_marker)))
        );
    }
}

#[test]
fn mixed_module_sources_are_rejected_under_a_different_snapshot_pin() {
    let (
        _directory,
        _project_v1,
        _project_v2,
        project_v3,
        project_v4,
        project_v5,
        project_v6,
        _snapshots,
    ) = module_upgrade_projects();

    let mut v3_with_v4_collection = project_v3.standard_sources().to_vec();
    v3_with_v4_collection
        .iter_mut()
        .find(|(path, _)| path == "std/collection.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-upgrade-std-collection-v4.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &project_v3,
            v3_with_v4_collection,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );

    let mut v4_with_v3_collection = project_v4.standard_sources().to_vec();
    v4_with_v3_collection
        .iter_mut()
        .find(|(path, _)| path == "std/collection.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-upgrade-std-collection.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &project_v4,
            v4_with_v3_collection,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );

    let mut v5_with_v4_math = project_v5.standard_sources().to_vec();
    v5_with_v4_math
        .iter_mut()
        .find(|(path, _)| path == "std/math.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-upgrade-std-v3.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(&project_v5, v5_with_v4_math, Limits::default())
            .unwrap_err()
            .code(),
        "ORNA-REPL-STANDARD"
    );

    let mut v6_with_v5_collection = project_v6.standard_sources().to_vec();
    v6_with_v5_collection
        .iter_mut()
        .find(|(path, _)| path == "std/collection.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-upgrade-std-collection-v5.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &project_v6,
            v6_with_v5_collection,
            Limits::default()
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );
}

#[test]
fn replay_keeps_each_multi_module_upgrade_bound_to_its_snapshot_pin() {
    let (
        _directory,
        _project_v1,
        _project_v2,
        _project_v3,
        project_v4,
        project_v5,
        project_v6,
        snapshots,
    ) = module_upgrade_projects();

    for (older, newer) in [(&project_v4, &project_v5), (&project_v5, &project_v6)] {
        assert_ne!(
            older.standard_profile().unwrap().snapshot(),
            newer.standard_profile().unwrap().snapshot()
        );
        assert_ne!(
            standard_source(older, "std/math.orna"),
            standard_source(newer, "std/math.orna")
        );
        assert_ne!(
            standard_source(older, "std/collection.orna"),
            standard_source(newer, "std/collection.orna")
        );
    }

    for (project, pin, expected_marker, expected_direct, expected_callback) in [
        (&project_v4, &snapshots[3], 4, 1007, 1010),
        (&project_v5, &snapshots[4], 5, 10007, 10010),
        (&project_v6, &snapshots[5], 6, 100007, 100010),
    ] {
        assert_eq!(project.standard_profile().unwrap().snapshot(), pin);
        let mut session = admitted_upgrade_session(project);
        assert_eq!(
            session.submit(include_str!(
                "fixtures/module-upgrade-use-collection-marker.orna"
            )),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!(
                "fixtures/module-upgrade-call-collection-marker.orna"
            )),
            Ok(Some(int(expected_marker)))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-replay-direct-seven.orna")),
            Ok(Some(int(expected_direct)))
        );
        let mut replay = session.clone();
        assert_eq!(
            replay.submit(include_str!("fixtures/snapshot-replay-callback-seven.orna")),
            Ok(Some(ints(&[expected_callback])))
        );
    }
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

#[test]
fn paired_snapshot_pins_reload_transitive_standard_module_sources() {
    let (_directory, historical, upgraded, snapshots) = module_chain_projects();
    assert_ne!(snapshots[0], snapshots[1]);

    let expected_v1_sources = [
        (
            "std/math.orna",
            include_str!("fixtures/module-chain-std-math-v1.orna"),
        ),
        (
            "std/collection.orna",
            include_str!("fixtures/module-chain-std-collection-v1.orna"),
        ),
        (
            "std/chain/entry.orna",
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
        ),
        (
            "std/chain/bridge.orna",
            include_str!("fixtures/module-chain-std-bridge-v1.orna"),
        ),
        (
            "std/chain/leaf.orna",
            include_str!("fixtures/module-chain-std-leaf-v1.orna"),
        ),
    ];
    let expected_v2_sources = [
        (
            "std/math.orna",
            include_str!("fixtures/module-chain-std-math-v2.orna"),
        ),
        (
            "std/collection.orna",
            include_str!("fixtures/module-chain-std-collection-v2.orna"),
        ),
        (
            "std/chain/entry.orna",
            include_str!("fixtures/module-chain-std-entry-v2.orna"),
        ),
        (
            "std/chain/bridge.orna",
            include_str!("fixtures/module-chain-std-bridge-v2.orna"),
        ),
        (
            "std/chain/leaf.orna",
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
        ),
    ];
    let expected_runtime_modules = [
        "std/chain/bridge.orna",
        "std/chain/entry.orna",
        "std/chain/leaf.orna",
        "std/collection.orna",
        "std/math.orna",
    ];

    for (project, snapshot, expected_sources) in [
        (&historical, &snapshots[0], &expected_v1_sources),
        (&upgraded, &snapshots[1], &expected_v2_sources),
    ] {
        assert_eq!(project.standard_profile().unwrap().snapshot(), snapshot);
        for (path, expected) in expected_sources {
            assert_eq!(standard_source(project, path), *expected);
        }
        let runtime_modules = project
            .standard_runtime_modules()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(runtime_modules, expected_runtime_modules);
    }
}

#[test]
fn repl_replays_and_rejects_mixed_transitive_module_pins() {
    let (_directory, historical, upgraded, _snapshots) = module_chain_projects();
    let mut historical_session = AdmittedReplSession::from_loaded_project(
        &historical,
        historical.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        historical_session.submit(include_str!("fixtures/module-chain-use-replay.orna")),
        Ok(None)
    );
    assert_eq!(
        historical_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );

    let mut upgraded_session = AdmittedReplSession::from_loaded_project(
        &upgraded,
        upgraded.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(
        upgraded_session.submit(include_str!("fixtures/module-chain-use-replay.orna")),
        Ok(None)
    );
    assert_eq!(
        upgraded_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[155])))
    );

    let mut replay = historical_session.clone();
    assert_eq!(
        replay.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );

    let mut historical_with_new_leaf = historical.standard_sources().to_vec();
    historical_with_new_leaf
        .iter_mut()
        .find(|(path, _)| path == "std/chain/leaf.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-chain-std-leaf-v2.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &historical,
            historical_with_new_leaf,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );

    let mut upgraded_with_old_bridge = upgraded.standard_sources().to_vec();
    upgraded_with_old_bridge
        .iter_mut()
        .find(|(path, _)| path == "std/chain/bridge.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-chain-std-bridge-v1.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &upgraded,
            upgraded_with_old_bridge,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );
}

#[test]
fn paired_transitive_sessions_keep_snapshot_pins_when_replayed_interleaved() {
    let (_directory, historical, upgraded, snapshots) = module_chain_projects();
    assert_ne!(snapshots[0], snapshots[1]);

    let mut historical_session = AdmittedReplSession::from_loaded_project(
        &historical,
        historical.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    let mut upgraded_session = AdmittedReplSession::from_loaded_project(
        &upgraded,
        upgraded.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    for session in [&mut historical_session, &mut upgraded_session] {
        assert_eq!(
            session.submit(include_str!("fixtures/module-chain-use-replay.orna")),
            Ok(None)
        );
    }

    assert_eq!(
        historical_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        upgraded_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[155])))
    );
    assert_eq!(
        historical_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        upgraded_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[155])))
    );
}

#[test]
fn cloned_transitive_sessions_replay_under_their_original_snapshot_pins() {
    let (_directory, historical, upgraded, snapshots) = module_chain_projects();
    assert_ne!(snapshots[0], snapshots[1]);

    let mut historical_session = AdmittedReplSession::from_loaded_project(
        &historical,
        historical.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    let mut upgraded_session = AdmittedReplSession::from_loaded_project(
        &upgraded,
        upgraded.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    for session in [&mut historical_session, &mut upgraded_session] {
        assert_eq!(
            session.submit(include_str!("fixtures/module-chain-use-replay.orna")),
            Ok(None)
        );
    }
    assert_eq!(
        historical_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        upgraded_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[155])))
    );

    let mut historical_replay = historical_session.clone();
    let mut upgraded_replay = upgraded_session.clone();
    assert_eq!(
        upgraded_replay.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[155])))
    );
    assert_eq!(
        historical_replay.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        historical_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        upgraded_session.submit(include_str!("fixtures/module-chain-call-replay.orna")),
        Ok(Some(ints(&[155])))
    );
}

#[test]
fn transitive_module_upgrade_chain_loads_each_captured_snapshot() {
    let (_directory, historical, intermediate, upgraded, snapshots) =
        module_chain_upgrade_projects();
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));

    let expected_modules = [
        "std/chain/bridge.orna",
        "std/chain/entry.orna",
        "std/chain/leaf.orna",
        "std/collection.orna",
        "std/math.orna",
    ];
    let versions = [
        (
            &historical,
            &snapshots[0],
            [
                include_str!("fixtures/module-chain-std-math-v1.orna"),
                include_str!("fixtures/module-chain-std-collection-v1.orna"),
                include_str!("fixtures/module-chain-std-entry-v1.orna"),
                include_str!("fixtures/module-chain-std-bridge-v1.orna"),
                include_str!("fixtures/module-chain-std-leaf-v1.orna"),
            ],
        ),
        (
            &intermediate,
            &snapshots[1],
            [
                include_str!("fixtures/module-chain-std-math-v2.orna"),
                include_str!("fixtures/module-chain-std-collection-v2.orna"),
                include_str!("fixtures/module-chain-std-entry-v2.orna"),
                include_str!("fixtures/module-chain-std-bridge-v2.orna"),
                include_str!("fixtures/module-chain-std-leaf-v2.orna"),
            ],
        ),
        (
            &upgraded,
            &snapshots[2],
            [
                include_str!("fixtures/module-chain-std-math-v3.orna"),
                include_str!("fixtures/module-chain-std-collection-v3.orna"),
                include_str!("fixtures/module-chain-std-entry-v3.orna"),
                include_str!("fixtures/module-chain-std-bridge-v3.orna"),
                include_str!("fixtures/module-chain-std-leaf-v3.orna"),
            ],
        ),
    ];

    for (project, snapshot, expected_sources) in versions {
        assert_eq!(project.standard_profile().unwrap().snapshot(), snapshot);
        for (path, expected) in [
            "std/math.orna",
            "std/collection.orna",
            "std/chain/entry.orna",
            "std/chain/bridge.orna",
            "std/chain/leaf.orna",
        ]
        .into_iter()
        .zip(expected_sources)
        {
            assert_eq!(standard_source(project, path), expected);
        }
        assert_eq!(
            project
                .standard_runtime_modules()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected_modules
        );
    }
}
