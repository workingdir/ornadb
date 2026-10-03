use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_project_v1::{LoadedProject, ProjectLoader};
use orna_repository_v1::{Repository, initialize_repository};
use orna_semantic_v1::StandardDependencyProfile;
use orna_sys_v1::{EnvironmentProvider, ProcessProvider};
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

fn host_codec_standard_sources(base64_source: &str) -> Vec<(String, String)> {
    [
        (
            "std/encoding.orna",
            include_str!("fixtures/snapshot-host-codec-encoding-main.orna"),
        ),
        ("std/encoding/base64.orna", base64_source),
        (
            "std/io.orna",
            include_str!("fixtures/snapshot-host-codec-io-main.orna"),
        ),
        (
            "std/io/process.orna",
            include_str!("fixtures/snapshot-host-codec-process.orna"),
        ),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), source.to_owned()))
    .collect()
}

fn host_codec_snapshot_projects() -> (TempDir, LoadedProject, LoadedProject, [String; 2]) {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("project");
    fs::create_dir_all(&project_path).unwrap();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/snapshot-host-codec-project.orna"),
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
    fs::create_dir_all(standard_path.join("encoding")).unwrap();
    fs::create_dir_all(standard_path.join("io")).unwrap();
    initialize_repository(&standard_path).unwrap();
    fs::write(
        standard_path.join("encoding.orna"),
        include_str!("fixtures/snapshot-host-codec-encoding-main.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("io.orna"),
        include_str!("fixtures/snapshot-host-codec-io-main.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("io/process.orna"),
        include_str!("fixtures/snapshot-host-codec-process.orna"),
    )
    .unwrap();
    fs::write(
        standard_path.join("encoding/base64.orna"),
        include_str!("fixtures/snapshot-host-codec-base64-v1.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "captured host codec snapshot v1");
    let snapshot_v1 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v1 = capture_standard_gitlink(&project_path, &snapshot_v1, "capture codec v1");

    fs::write(
        standard_path.join("encoding/base64.orna"),
        include_str!("fixtures/snapshot-host-codec-base64-v2.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "captured host codec snapshot v2");
    let snapshot_v2 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v2 = capture_standard_gitlink(&project_path, &snapshot_v2, "capture codec v2");

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let project_v1_snapshot = repository.resolve_snapshot(&parent_v1).unwrap();
    let project_v2_snapshot = repository.resolve_snapshot(&parent_v2).unwrap();
    assert_eq!(
        repository
            .committed_submodule_commit(&project_v1_snapshot, "stdlib/std")
            .unwrap()
            .as_str(),
        snapshot_v1
    );
    assert_eq!(
        repository
            .committed_submodule_commit(&project_v2_snapshot, "stdlib/std")
            .unwrap()
            .as_str(),
        snapshot_v2
    );
    let profile_v1 = StandardDependencyProfile::from_sources(
        snapshot_v1.clone(),
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna")),
    )
    .unwrap();
    let profile_v2 = StandardDependencyProfile::from_sources(
        snapshot_v2.clone(),
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna")),
    )
    .unwrap();
    let project_v1 = loader
        .load_committed_snapshot_with_standard_profile(
            &repository,
            &project_v1_snapshot,
            Some(profile_v1),
        )
        .unwrap();
    let project_v2 = loader
        .load_committed_snapshot_with_standard_profile(
            &repository,
            &project_v2_snapshot,
            Some(profile_v2),
        )
        .unwrap();
    (
        directory,
        project_v1,
        project_v2,
        [snapshot_v1, snapshot_v2],
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

fn base64_padding_matrix_value() -> CanonicalValue {
    let cases: [(&[u8], &str); 6] = [
        (&[], ""),
        (&[0], "AA=="),
        (&[0, 1], "AAE="),
        (&[0, 1, 2], "AAEC"),
        (&[255], "/w=="),
        (&[255, 238], "/+4="),
    ];
    CanonicalValue::new(Raw::Array(
        cases
            .into_iter()
            .map(|(bytes, encoded)| {
                Raw::Tag(
                    60015,
                    Box::new(Raw::Array(vec![
                        Raw::Bytes(bytes.to_vec()),
                        Raw::Text(encoded.to_owned()),
                    ])),
                )
            })
            .collect(),
    ))
    .unwrap()
}

fn base64_snapshot_marker_value(marker: i64) -> CanonicalValue {
    CanonicalValue::new(Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            Raw::Int(marker.into()),
            Raw::Text("AP8AQQ==".to_owned()),
        ])),
    ))
    .unwrap()
}

fn process_output_value(stdout: &[u8]) -> Raw {
    Raw::Array(vec![
        Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
        ),
        Raw::Bytes(stdout.to_vec()),
        Raw::Bytes(Vec::new()),
    ])
}

fn base64_process_matrix_value() -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        [
            &[][..],
            &[0][..],
            &[0, 1][..],
            &[0, 1, 2][..],
            &[255][..],
            &[255, 238][..],
        ]
        .into_iter()
        .map(process_output_value)
        .collect(),
    ))
    .unwrap()
}

#[test]
fn captured_codec_snapshots_replay_real_base64_values_without_retargeting() {
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    assert_ne!(snapshots[0], snapshots[1]);
    assert_eq!(
        project_v1
            .standard_profile()
            .expect("captured v1 std profile")
            .snapshot(),
        &snapshots[0]
    );
    assert_eq!(
        project_v2
            .standard_profile()
            .expect("captured v2 std profile")
            .snapshot(),
        &snapshots[1]
    );
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    assert_eq!(
        sources_v1
            .iter()
            .find(|(path, _)| path == "std/encoding/base64.orna")
            .unwrap()
            .1,
        include_str!("fixtures/snapshot-host-codec-base64-v1.orna")
    );
    assert_eq!(
        sources_v2
            .iter()
            .find(|(path, _)| path == "std/encoding/base64.orna")
            .unwrap()
            .1,
        include_str!("fixtures/snapshot-host-codec-base64-v2.orna")
    );

    // The version marker is proof-only because section 9 specifies Base64
    // values but no source-level snapshot marker API. The codec bodies remain
    // identical and real so the result identifies the captured dependency.
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    let bytes = CanonicalValue::new(Raw::Bytes(vec![0, 255, 0, b'A'])).unwrap();

    for (session, expected_marker) in [(&mut historical, 1), (&mut upgraded, 2)] {
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-host-codec-decode.orna")),
            Ok(Some(bytes.clone()))
        );
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-host-codec-tagged.orna")),
            Ok(Some(base64_snapshot_marker_value(expected_marker)))
        );
    }
    assert_eq!(
        historical.submit(include_str!("fixtures/snapshot-host-codec-tagged.orna")),
        Ok(Some(base64_snapshot_marker_value(1)))
    );
}

#[test]
fn captured_codec_snapshots_feed_real_process_stdin_after_interleaved_upgrade() {
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_eq!(
        project_v1.standard_profile().unwrap().snapshot(),
        &snapshots[0]
    );
    assert_eq!(
        project_v2.standard_profile().unwrap().snapshot(),
        &snapshots[1]
    );

    let working_directory = env::current_dir().unwrap();
    let mut process = ProcessProvider::new(Duration::from_secs(2), 64).unwrap();
    process
        .allow_command("/usr/bin/cat", &working_directory, [])
        .unwrap();
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);
    let bytes = vec![0, 255, 0, b'A'];
    let expected = CanonicalValue::new(Raw::Array(vec![
        Raw::Tag(
            60013,
            Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
        ),
        Raw::Bytes(bytes),
        Raw::Bytes(Vec::new()),
    ]))
    .unwrap();

    let mut assert_process_value = |session: &mut AdmittedReplSession| {
        let output = session.submit_with_sys_host_bindings(
            include_str!("fixtures/snapshot-host-codec-process-call.orna"),
            &mut bindings,
        );
        assert!(
            output.is_ok(),
            "captured process call failed: {}",
            output.as_ref().unwrap_err().code()
        );
        assert_eq!(output, Ok(Some(expected.clone())));
    };
    assert_process_value(&mut historical);
    assert_process_value(&mut upgraded);
    assert_process_value(&mut historical);
}

#[test]
fn captured_codec_snapshots_retain_rfc4648_padding_values() {
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_ne!(snapshots[0], snapshots[1]);
    assert_eq!(
        historical.submit(include_str!(
            "fixtures/snapshot-host-codec-base64-padding-matrix.orna"
        )),
        Ok(Some(base64_padding_matrix_value()))
    );
    assert_eq!(
        upgraded.submit(include_str!(
            "fixtures/snapshot-host-codec-base64-padding-matrix.orna"
        )),
        Ok(Some(base64_padding_matrix_value()))
    );
}

#[test]
fn captured_codec_snapshots_reject_noncanonical_base64_without_losing_the_pin() {
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_ne!(snapshots[0], snapshots[1]);

    // RFC 4648 section 9's standard profile rejects malformed padding,
    // nonstandard alphabets, whitespace, and nonzero unused tail bits. Keep
    // these error assertions pinned to both captured dependency snapshots.
    for (session, snapshot, marker) in [
        (&mut historical, &snapshots[0], 1),
        (&mut upgraded, &snapshots[1], 2),
    ] {
        assert_eq!(
            session
                .submit(include_str!("fixtures/snapshot-host-codec-tagged.orna"))
                .unwrap()
                .unwrap(),
            base64_snapshot_marker_value(marker)
        );
        for invalid in ["A===", "AAA", "-w==", "AA==\n", "AA=A", "AB==", "AAB="] {
            let source = include_str!("fixtures/snapshot-host-codec-decode-probe.orna")
                .replace("__BASE64_INPUT__", &format!("{invalid:?}"));
            assert_eq!(
                session.submit(&source).unwrap_err().code(),
                "ORNA-EVAL-VALUE",
                "snapshot {snapshot} must reject noncanonical Base64 {invalid:?}"
            );
        }
        assert_eq!(
            session.submit(include_str!(
                "fixtures/snapshot-host-codec-base64-padding-matrix.orna"
            )),
            Ok(Some(base64_padding_matrix_value()))
        );
    }
}

#[test]
fn captured_codec_snapshots_send_padding_shapes_through_real_process_stdin() {
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_ne!(snapshots[0], snapshots[1]);
    let working_directory = env::current_dir().unwrap();
    let mut process = ProcessProvider::new(Duration::from_secs(2), 64).unwrap();
    process
        .allow_command("/usr/bin/cat", &working_directory, [])
        .unwrap();
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);
    let mut assert_process_matrix = |session: &mut AdmittedReplSession| {
        let output = session.submit_with_sys_host_bindings(
            include_str!("fixtures/snapshot-host-codec-process-padding-matrix.orna"),
            &mut bindings,
        );
        assert!(
            output.is_ok(),
            "captured process matrix failed: {}",
            output.as_ref().unwrap_err().code()
        );
        assert_eq!(output, Ok(Some(base64_process_matrix_value())));
    };
    assert_process_matrix(&mut historical);
    assert_process_matrix(&mut upgraded);
    assert_process_matrix(&mut historical);
}

#[test]
fn captured_codec_snapshots_fold_host_process_values_across_interleaved_pins() {
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_ne!(snapshots[0], snapshots[1]);

    let working_directory = env::current_dir().unwrap();
    let mut process = ProcessProvider::new(Duration::from_secs(2), 64).unwrap();
    process
        .allow_command("/usr/bin/cat", &working_directory, [])
        .unwrap();
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);
    let mut folded_outputs = Vec::new();
    for snapshot_index in [0, 1, 0] {
        let (session, expected_marker) = match snapshot_index {
            0 => (&mut historical, 1),
            1 => (&mut upgraded, 2),
            _ => unreachable!("the process fold only selects captured snapshots"),
        };
        let tagged = session
            .submit(include_str!("fixtures/snapshot-host-codec-tagged.orna"))
            .unwrap_or_else(|error| panic!("snapshot marker failed: {}", error.code()))
            .expect("the captured snapshot marker returns a value");
        let expected_tagged = base64_snapshot_marker_value(expected_marker);
        assert_eq!(tagged, expected_tagged);

        let process_values = session
            .submit_with_sys_host_bindings(
                include_str!("fixtures/snapshot-host-codec-process-padding-matrix.orna"),
                &mut bindings,
            )
            .unwrap_or_else(|error| panic!("captured process fold failed: {}", error.code()))
            .expect("the process fold returns its six captured values");
        assert_eq!(process_values, base64_process_matrix_value());
        folded_outputs.push(
            CanonicalValue::new(Raw::Array(vec![
                tagged.raw().clone(),
                process_values.raw().clone(),
            ]))
            .unwrap(),
        );
    }
    let process_values = base64_process_matrix_value();
    let expected = [1, 2, 1]
        .into_iter()
        .map(|marker| {
            let tagged = base64_snapshot_marker_value(marker);
            CanonicalValue::new(Raw::Array(vec![
                tagged.raw().clone(),
                process_values.raw().clone(),
            ]))
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(folded_outputs, expected);
}

#[test]
fn captured_codec_snapshots_preserve_nested_codec_bytes_through_host_process() {
    let fixture = include_str!("fixtures/snapshot-host-codec-process-nested-matrix.orna");
    let (_directory, project_v1, project_v2, snapshots) = host_codec_snapshot_projects();
    let sources_v1 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v1.orna"));
    let sources_v2 =
        host_codec_standard_sources(include_str!("fixtures/snapshot-host-codec-base64-v2.orna"));
    let mut historical =
        AdmittedReplSession::from_loaded_project(&project_v1, sources_v1, Limits::default())
            .unwrap();
    let mut upgraded =
        AdmittedReplSession::from_loaded_project(&project_v2, sources_v2, Limits::default())
            .unwrap();
    assert_ne!(snapshots[0], snapshots[1]);

    let working_directory = env::current_dir().unwrap();
    let mut process = ProcessProvider::new(Duration::from_secs(2), 64).unwrap();
    process
        .allow_command("/usr/bin/cat", &working_directory, [])
        .unwrap();
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);
    let mut folded_outputs = Vec::new();
    for snapshot_index in [0, 1, 0] {
        let (session, expected_marker) = match snapshot_index {
            0 => (&mut historical, 1),
            1 => (&mut upgraded, 2),
            _ => unreachable!("the codec process fold only selects captured snapshots"),
        };
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-host-codec-tagged.orna")),
            Ok(Some(base64_snapshot_marker_value(expected_marker)))
        );
        let output = session
            .submit_with_sys_host_bindings(fixture, &mut bindings)
            .unwrap_or_else(|error| panic!("nested codec process fold failed: {}", error.code()))
            .expect("the nested codec process fold returns all six values");
        assert_eq!(output, base64_process_matrix_value());
        folded_outputs.push(output);
    }

    assert_eq!(
        folded_outputs,
        vec![
            base64_process_matrix_value(),
            base64_process_matrix_value(),
            base64_process_matrix_value(),
        ]
    );
}

fn capture_standard_gitlink(project_path: &Path, standard_snapshot: &str, message: &str) -> String {
    let mut staged_paths = vec!["main.orna", ".gitmodules"];
    if project_path.join("snapshot_app.orna").is_file() {
        staged_paths.push("snapshot_app.orna");
    }
    let mut add_arguments = vec!["add"];
    add_arguments.extend(staged_paths);
    git_output_at(project_path, &add_arguments);
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
    fs::write(
        project_path.join("snapshot_app.orna"),
        include_str!("fixtures/module-upgrade-app.orna"),
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

fn snapshot_matrix_projects() -> (TempDir, Vec<LoadedProject>, Vec<String>) {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("project");
    fs::create_dir_all(&project_path).unwrap();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/snapshot-matrix-project.orna"),
    )
    .unwrap();
    fs::write(
        project_path.join("snapshot_app.orna"),
        include_str!("fixtures/module-upgrade-app.orna"),
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
        include_str!("fixtures/snapshot-matrix-std-main.orna"),
    )
    .unwrap();
    let math_versions = [
        include_str!("fixtures/module-upgrade-std-v1.orna"),
        include_str!("fixtures/module-upgrade-std-v2.orna"),
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v5.orna"),
        include_str!("fixtures/module-upgrade-std-v6.orna"),
    ];

    let mut parent_snapshots = Vec::with_capacity(math_versions.len());
    let mut standard_snapshots = Vec::with_capacity(math_versions.len());
    for (index, math_source) in math_versions.into_iter().enumerate() {
        fs::write(standard_path.join("math.orna"), math_source).unwrap();
        commit_directory(
            &standard_path,
            &format!("real math body snapshot v{}", index + 1),
        );
        let standard_snapshot = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
        parent_snapshots.push(capture_standard_gitlink(
            &project_path,
            &standard_snapshot,
            &format!("capture real math snapshot v{}", index + 1),
        ));
        standard_snapshots.push(standard_snapshot);
    }

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let projects = parent_snapshots
        .iter()
        .map(|snapshot| {
            let snapshot = repository.resolve_snapshot(snapshot).unwrap();
            loader
                .load_committed_snapshot(&repository, &snapshot)
                .unwrap()
        })
        .collect();
    (directory, projects, standard_snapshots)
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

fn incremental_transitive_upgrade_projects() -> (TempDir, Vec<LoadedProject>, Vec<String>) {
    let (directory, project_path, standard_path) = module_chain_repository();
    write_module_chain_version(
        &standard_path,
        include_str!("fixtures/module-chain-std-math-v1.orna"),
        include_str!("fixtures/module-chain-std-collection-v1.orna"),
        include_str!("fixtures/module-chain-std-entry-v1.orna"),
        include_str!("fixtures/module-chain-std-bridge-v1.orna"),
        include_str!("fixtures/module-chain-std-leaf-v1.orna"),
    );
    commit_directory(&standard_path, "transitive standard v1");
    let mut standard_snapshots = vec![git_output_at(&standard_path, &["rev-parse", "HEAD"])];
    let mut parent_snapshots = vec![capture_standard_gitlink(
        &project_path,
        &standard_snapshots[0],
        "capture transitive v1",
    )];

    for (path, source, message) in [
        (
            "chain/leaf.orna",
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
            "upgrade transitive leaf",
        ),
        (
            "math.orna",
            include_str!("fixtures/module-chain-std-math-v2.orna"),
            "upgrade transitive math",
        ),
        (
            "collection.orna",
            include_str!("fixtures/module-chain-std-collection-v2.orna"),
            "upgrade transitive collection",
        ),
        (
            "chain/bridge.orna",
            include_str!("fixtures/module-chain-std-bridge-v2.orna"),
            "upgrade transitive bridge",
        ),
        (
            "chain/entry.orna",
            include_str!("fixtures/module-chain-std-entry-v2.orna"),
            "upgrade transitive entry",
        ),
    ] {
        fs::write(standard_path.join(path), source).unwrap();
        commit_directory(&standard_path, message);
        standard_snapshots.push(git_output_at(&standard_path, &["rev-parse", "HEAD"]));
        parent_snapshots.push(capture_standard_gitlink(
            &project_path,
            standard_snapshots.last().unwrap(),
            message,
        ));
    }

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let projects = parent_snapshots
        .iter()
        .map(|parent| {
            let snapshot = repository.resolve_snapshot(parent).unwrap();
            loader
                .load_committed_snapshot(&repository, &snapshot)
                .unwrap()
        })
        .collect();
    (directory, projects, standard_snapshots)
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

fn admitted_snapshot_app_session(project: &LoadedProject) -> AdmittedReplSession {
    let mut session = AdmittedReplSession::from_loaded_project(
        project,
        project.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap_or_else(|error| panic!("failed to admit snapshot app project: {}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/module-upgrade-use-app.orna")),
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
fn project_module_executes_a_real_function_from_its_gitlink_pinned_std_snapshot() {
    let (
        _directory,
        project_v1,
        _project_v2,
        _project_v3,
        _project_v4,
        _project_v5,
        project_v6,
        snapshots,
    ) = module_upgrade_projects();
    assert_ne!(snapshots[0], snapshots[5]);

    let mut historical = AdmittedReplSession::from_loaded_project(
        &project_v1,
        project_v1.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .expect("the historical project's exact std gitlink snapshot admits");
    let mut current = AdmittedReplSession::from_loaded_project(
        &project_v6,
        project_v6.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .expect("the current project's exact std gitlink snapshot admits");

    assert_eq!(
        project_v1.standard_profile().unwrap().snapshot(),
        snapshots[0]
    );
    assert_eq!(
        project_v6.standard_profile().unwrap().snapshot(),
        snapshots[5]
    );
    assert_eq!(historical.submit("use snapshot_app;"), Ok(None));
    assert_eq!(current.submit("use snapshot_app;"), Ok(None));
    assert_eq!(
        historical
            .submit("snapshot_app.value()")
            .unwrap_or_else(|error| panic!(
                "historical snapshot_app.value failed: {}",
                error.code()
            )),
        Some(int(8))
    );
    assert_eq!(
        current
            .submit("snapshot_app.value()")
            .unwrap_or_else(|error| panic!("current snapshot_app.value failed: {}", error.code())),
        Some(int(100_007))
    );
}

#[test]
fn imported_project_module_executes_under_each_captured_standard_snapshot() {
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
    let projects = [
        &project_v1,
        &project_v2,
        &project_v3,
        &project_v4,
        &project_v5,
        &project_v6,
    ];
    let expected_values = [8, 107, 1007, 1007, 10007, 100007];

    let mut sessions = projects
        .iter()
        .map(|project| admitted_snapshot_app_session(project))
        .collect::<Vec<_>>();

    for (index, ((project, snapshot), (session, expected))) in projects
        .iter()
        .zip(&snapshots)
        .zip(sessions.iter_mut().zip(expected_values))
        .enumerate()
    {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            snapshot,
            "project v{} must retain its captured std gitlink",
            index + 1
        );
        assert_eq!(
            session.submit(include_str!("fixtures/module-upgrade-call-app.orna")),
            Ok(Some(int(expected))),
            "project v{} must execute snapshot_app against its pinned std body",
            index + 1
        );
    }

    let mut replays = sessions.clone();
    for index in [5, 0, 4, 1, 3, 2] {
        assert_eq!(
            replays[index].submit(include_str!("fixtures/module-upgrade-call-app.orna")),
            Ok(Some(int(expected_values[index]))),
            "interleaved replay for project v{} must keep its original dependency pin",
            index + 1
        );
    }
}

#[test]
fn stepwise_snapshot_replay_matrix_retains_values_after_each_upgrade() {
    let (_directory, projects, snapshots) = snapshot_matrix_projects();
    let expected_values = [8, 107, 1007, 10007, 100007, 1000007];
    let mut retained_sessions = Vec::new();

    assert_eq!(projects.len(), expected_values.len());
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    for (upgrade_index, (project, expected)) in projects.iter().zip(expected_values).enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &snapshots[upgrade_index],
            "upgrade v{} must use its committed std gitlink",
            upgrade_index + 1
        );
        assert_eq!(
            project
                .standard_sources()
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["std/main.orna", "std/math.orna"],
            "matrix snapshots contain only the real imported math body"
        );
        let mut session = admitted_snapshot_app_session(project);
        assert_eq!(
            session.submit(include_str!("fixtures/module-upgrade-call-app.orna")),
            Ok(Some(int(expected))),
            "new project v{} must execute its captured std math body",
            upgrade_index + 1
        );
        retained_sessions.push(session);

        for retained_index in (0..retained_sessions.len()).rev() {
            assert_eq!(
                retained_sessions[retained_index]
                    .submit(include_str!("fixtures/module-upgrade-call-app.orna")),
                Ok(Some(int(expected_values[retained_index]))),
                "replay of project v{} must stay pinned after upgrade v{}",
                retained_index + 1,
                upgrade_index + 1
            );
        }
    }
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

#[test]
fn transitive_replay_keeps_each_pin_across_three_upgrade_generations() {
    let (_directory, historical, intermediate, upgraded, snapshots) =
        module_chain_upgrade_projects();
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));

    let mut historical_session = AdmittedReplSession::from_loaded_project(
        &historical,
        historical.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    let mut intermediate_session = AdmittedReplSession::from_loaded_project(
        &intermediate,
        intermediate.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    let mut upgraded_session = AdmittedReplSession::from_loaded_project(
        &upgraded,
        upgraded.standard_sources().iter().cloned(),
        Limits::default(),
    )
    .unwrap();
    for session in [
        &mut historical_session,
        &mut intermediate_session,
        &mut upgraded_session,
    ] {
        assert_eq!(
            session.submit(include_str!("fixtures/module-chain-use-replay.orna")),
            Ok(None)
        );
    }

    let replay_source = include_str!("fixtures/module-chain-call-replay.orna");
    assert_eq!(
        historical_session.submit(replay_source),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        intermediate_session.submit(replay_source),
        Ok(Some(ints(&[155])))
    );
    assert_eq!(
        upgraded_session.submit(replay_source),
        Ok(Some(ints(&[1505])))
    );
    assert_eq!(
        intermediate_session.submit(replay_source),
        Ok(Some(ints(&[155])))
    );
    assert_eq!(
        historical_session.submit(replay_source),
        Ok(Some(ints(&[20])))
    );
    assert_eq!(
        upgraded_session.submit(replay_source),
        Ok(Some(ints(&[1505])))
    );

    let mut historical_replay = historical_session.clone();
    let mut intermediate_replay = intermediate_session.clone();
    let mut upgraded_replay = upgraded_session.clone();
    for (session, expected) in [
        (&mut upgraded_replay, 1505),
        (&mut intermediate_replay, 155),
        (&mut historical_replay, 20),
    ] {
        assert_eq!(session.submit(replay_source), Ok(Some(ints(&[expected]))));
    }

    let mut historical_with_new_leaf = historical.standard_sources().to_vec();
    historical_with_new_leaf
        .iter_mut()
        .find(|(path, _)| path == "std/chain/leaf.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-chain-std-leaf-v3.orna").into();
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
}

#[test]
fn incremental_transitive_upgrade_pins_capture_cumulative_module_sources() {
    let (_directory, projects, snapshots) = incremental_transitive_upgrade_projects();
    assert_eq!(projects.len(), 6);
    assert_eq!(snapshots.len(), projects.len());
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));

    let expected_modules = [
        "std/chain/bridge.orna",
        "std/chain/entry.orna",
        "std/chain/leaf.orna",
        "std/collection.orna",
        "std/math.orna",
    ];
    let expected_sources = [
        [
            include_str!("fixtures/module-chain-std-math-v1.orna"),
            include_str!("fixtures/module-chain-std-collection-v1.orna"),
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
            include_str!("fixtures/module-chain-std-bridge-v1.orna"),
            include_str!("fixtures/module-chain-std-leaf-v1.orna"),
        ],
        [
            include_str!("fixtures/module-chain-std-math-v1.orna"),
            include_str!("fixtures/module-chain-std-collection-v1.orna"),
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
            include_str!("fixtures/module-chain-std-bridge-v1.orna"),
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
        ],
        [
            include_str!("fixtures/module-chain-std-math-v2.orna"),
            include_str!("fixtures/module-chain-std-collection-v1.orna"),
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
            include_str!("fixtures/module-chain-std-bridge-v1.orna"),
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
        ],
        [
            include_str!("fixtures/module-chain-std-math-v2.orna"),
            include_str!("fixtures/module-chain-std-collection-v2.orna"),
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
            include_str!("fixtures/module-chain-std-bridge-v1.orna"),
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
        ],
        [
            include_str!("fixtures/module-chain-std-math-v2.orna"),
            include_str!("fixtures/module-chain-std-collection-v2.orna"),
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
            include_str!("fixtures/module-chain-std-bridge-v2.orna"),
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
        ],
        [
            include_str!("fixtures/module-chain-std-math-v2.orna"),
            include_str!("fixtures/module-chain-std-collection-v2.orna"),
            include_str!("fixtures/module-chain-std-entry-v2.orna"),
            include_str!("fixtures/module-chain-std-bridge-v2.orna"),
            include_str!("fixtures/module-chain-std-leaf-v2.orna"),
        ],
    ];

    for ((project, snapshot), sources) in projects.iter().zip(&snapshots).zip(expected_sources) {
        assert_eq!(project.standard_profile().unwrap().snapshot(), snapshot);
        for (path, expected) in [
            "std/math.orna",
            "std/collection.orna",
            "std/chain/entry.orna",
            "std/chain/bridge.orna",
            "std/chain/leaf.orna",
        ]
        .into_iter()
        .zip(sources)
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

    let mut sessions = projects
        .iter()
        .map(|project| {
            AdmittedReplSession::from_loaded_project(
                project,
                project.standard_sources().iter().cloned(),
                Limits::default(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    for session in &mut sessions {
        assert_eq!(
            session.submit(include_str!("fixtures/module-chain-use-replay.orna")),
            Ok(None)
        );
    }

    let replay_source = include_str!("fixtures/module-chain-call-replay.orna");
    let expected_results = [20, 47, 92, 128, 146, 155];
    for (session, expected) in sessions.iter_mut().zip(expected_results) {
        assert_eq!(session.submit(replay_source), Ok(Some(ints(&[expected]))));
    }
    for (index, expected) in (0..sessions.len())
        .rev()
        .zip(expected_results.into_iter().rev())
    {
        let mut replay = sessions[index].clone();
        assert_eq!(replay.submit(replay_source), Ok(Some(ints(&[expected]))));
    }

    let mut historical_with_current_leaf = projects[0].standard_sources().to_vec();
    historical_with_current_leaf
        .iter_mut()
        .find(|(path, _)| path == "std/chain/leaf.orna")
        .unwrap()
        .1 = include_str!("fixtures/module-chain-std-leaf-v2.orna").into();
    assert_eq!(
        AdmittedReplSession::from_loaded_project(
            &projects[0],
            historical_with_current_leaf,
            Limits::default(),
        )
        .unwrap_err()
        .code(),
        "ORNA-REPL-STANDARD"
    );
}

#[test]
fn stepwise_transitive_replay_retains_prior_pins_after_each_upgrade() {
    let (_directory, projects, snapshots) = incremental_transitive_upgrade_projects();
    let expected_results = [20, 47, 92, 128, 146, 155];
    let replay_source = include_str!("fixtures/module-chain-call-replay.orna");
    let mut retained_sessions = Vec::new();

    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &snapshots[index]
        );
        let mut current = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            current.submit(include_str!("fixtures/module-chain-use-replay.orna")),
            Ok(None)
        );
        assert_eq!(
            current.submit(replay_source),
            Ok(Some(ints(&[expected_results[index]])))
        );
        retained_sessions.push(current);

        for (session, expected) in retained_sessions.iter_mut().zip(&expected_results) {
            assert_eq!(session.submit(replay_source), Ok(Some(ints(&[*expected]))));
        }
    }

    let mut clones = retained_sessions.iter().cloned().collect::<Vec<_>>();
    for index in (0..clones.len()).rev() {
        assert_eq!(
            clones[index].submit(replay_source),
            Ok(Some(ints(&[expected_results[index]])))
        );
    }

    for (index, path) in [
        "std/chain/leaf.orna",
        "std/math.orna",
        "std/collection.orna",
        "std/chain/bridge.orna",
        "std/chain/entry.orna",
    ]
    .into_iter()
    .enumerate()
    {
        let mut mixed_sources = projects[index].standard_sources().to_vec();
        let next_source = standard_source(&projects[index + 1], path).to_owned();
        mixed_sources
            .iter_mut()
            .find(|(source_path, _)| source_path == path)
            .unwrap()
            .1 = next_source;
        assert_eq!(
            AdmittedReplSession::from_loaded_project(
                &projects[index],
                mixed_sources,
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-REPL-STANDARD"
        );
    }
}
