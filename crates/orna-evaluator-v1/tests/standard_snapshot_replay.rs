use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use orna_evaluator_v1::{AdmittedReplSession, Limits, SysHostBindingRegistry};
use orna_foundation_v1::CanonicalValue;
use orna_project_v1::{LoadedProject, ProjectLoadError, ProjectLoader};
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
    process_output_with_stderr_value(stdout, &[])
}

fn process_output_with_stderr_value(stdout: &[u8], stderr: &[u8]) -> Raw {
    Raw::Tag(
        60015,
        Box::new(Raw::Array(vec![
            Raw::Tag(
                60013,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), Raw::Int(0.into())])),
            ),
            Raw::Bytes(stdout.to_vec()),
            Raw::Bytes(stderr.to_vec()),
        ])),
    )
}

fn process_boundary_codec_chain_value(bytes: &[u8]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(vec![
        process_output_with_stderr_value(bytes, bytes),
        process_output_value(bytes),
        process_output_value(bytes),
    ]))
    .unwrap()
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

fn base64_codec_depth_process_matrix_value() -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        [
            &[][..],
            &[0][..],
            &[0, 1][..],
            &[0, 1, 2][..],
            &[255][..],
            &[255, 238][..],
            &[0, 1, 2, 3][..],
            &[0, 1, 2, 3, 4][..],
            &[0, 1, 2, 3, 4, 5][..],
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
    let expected = CanonicalValue::new(process_output_value(&bytes)).unwrap();

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
        assert_eq!(output, base64_codec_depth_process_matrix_value());
        folded_outputs.push(output);
    }

    assert_eq!(
        folded_outputs,
        vec![
            base64_codec_depth_process_matrix_value(),
            base64_codec_depth_process_matrix_value(),
            base64_codec_depth_process_matrix_value(),
        ]
    );
}

#[test]
fn captured_codec_snapshots_reprocess_host_output_through_nested_codecs() {
    let fixture = include_str!("fixtures/snapshot-host-codec-process-reprocess-matrix.orna");
    let parsed = orna_syntax_v1::parse_repl(fixture);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
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
    let inputs = [
        ("", &[][..]),
        ("AA==", &[0][..]),
        ("AAE=", &[0, 1][..]),
        ("AAEC", &[0, 1, 2][..]),
        ("/w==", &[255][..]),
        ("AAECAwQF", &[0, 1, 2, 3, 4, 5][..]),
    ];
    let expected = CanonicalValue::new(Raw::Array(
        inputs
            .iter()
            .map(|(_, bytes)| process_output_value(bytes))
            .collect(),
    ))
    .unwrap();
    let mut folded_outputs = Vec::new();
    for snapshot_index in [0, 1, 0] {
        let (session, expected_marker) = match snapshot_index {
            0 => (&mut historical, 1),
            1 => (&mut upgraded, 2),
            _ => unreachable!("the reprocess fold only selects captured snapshots"),
        };
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-host-codec-tagged.orna")),
            Ok(Some(base64_snapshot_marker_value(expected_marker)))
        );
        let mut replay_rows = Vec::new();
        for (encoded, bytes) in &inputs {
            let source = fixture.replace("__BASE64_INPUT__", encoded);
            let output = session
                .submit_with_sys_host_bindings(&source, &mut bindings)
                .unwrap_or_else(|error| {
                    panic!("nested process output replay failed: {}", error.code())
                })
                .expect("nested codecs feed captured stdout into a second host process");
            let expected_row = CanonicalValue::new(process_output_value(bytes)).unwrap();
            assert_eq!(output, expected_row);
            replay_rows.push(output.raw().clone());
        }
        let replay_matrix = CanonicalValue::new(Raw::Array(replay_rows)).unwrap();
        assert_eq!(replay_matrix, expected);
        folded_outputs.push(replay_matrix);
    }

    assert_eq!(
        folded_outputs,
        vec![expected.clone(), expected.clone(), expected]
    );
}

#[test]
fn captured_codec_snapshots_fold_both_process_pipes_through_nested_boundaries() {
    let fixture = include_str!("fixtures/snapshot-host-codec-process-boundary-chain.orna");
    let parsed = orna_syntax_v1::parse_repl(fixture);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
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
        .allow_command("/usr/bin/tee", &working_directory, [])
        .unwrap();
    process
        .allow_command("/usr/bin/cat", &working_directory, [])
        .unwrap();
    let mut bindings =
        SysHostBindingRegistry::new(EnvironmentProvider::default()).with_process_provider(process);
    let inputs = [
        ("", &[][..]),
        ("AA==", &[0][..]),
        ("AAE=", &[0, 1][..]),
        ("AAEC", &[0, 1, 2][..]),
        ("/w==", &[255][..]),
        ("AAECAwQF", &[0, 1, 2, 3, 4, 5][..]),
    ];
    let expected = CanonicalValue::new(Raw::Array(
        inputs
            .iter()
            .map(|(_, bytes)| process_boundary_codec_chain_value(bytes).raw().clone())
            .collect(),
    ))
    .unwrap();

    let mut folded_matrices = Vec::new();
    for snapshot_index in [0, 1, 0] {
        let (session, marker) = match snapshot_index {
            0 => (&mut historical, 1),
            1 => (&mut upgraded, 2),
            _ => unreachable!("the process boundary fold only selects captured snapshots"),
        };
        assert_eq!(
            session.submit(include_str!("fixtures/snapshot-host-codec-tagged.orna")),
            Ok(Some(base64_snapshot_marker_value(marker)))
        );

        let mut rows = Vec::new();
        for (encoded, bytes) in &inputs {
            let source = fixture.replace("__BASE64_INPUT__", encoded);
            let output = session
                .submit_with_sys_host_bindings(&source, &mut bindings)
                .unwrap_or_else(|error| {
                    panic!("captured process codec chain failed: {}", error.code())
                })
                .expect("the two captured output pipes feed real nested process calls");
            let expected_row = process_boundary_codec_chain_value(bytes);
            assert_eq!(output, expected_row, "snapshot {marker}, input {encoded:?}");
            rows.push(output.raw().clone());
        }
        let matrix = CanonicalValue::new(Raw::Array(rows)).unwrap();
        assert_eq!(matrix, expected, "captured snapshot {marker} matrix");
        folded_matrices.push(matrix);
    }

    assert_eq!(
        folded_matrices,
        vec![expected.clone(), expected.clone(), expected]
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

fn divergent_paired_module_projects() -> (TempDir, [LoadedProject; 4], [String; 4]) {
    let (directory, _, _, _, base_project, _, _, snapshots) = module_upgrade_projects();
    let project_path = directory.path().join("project");
    let standard_path = project_path.join("stdlib/std");
    let base_pin = snapshots[3].clone();

    git_output_at(&standard_path, &["checkout", "--detach", base_pin.as_str()]);
    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "diverge math from paired module pin");
    let math_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &["branch", "math-only-pin", math_pin.as_str()],
    );
    let math_parent = capture_standard_gitlink(
        &project_path,
        &math_pin,
        "capture math-only branch from paired module pin",
    );

    git_output_at(&standard_path, &["checkout", "--detach", base_pin.as_str()]);
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "diverge collection from paired module pin");
    let collection_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &["branch", "collection-only-pin", collection_pin.as_str()],
    );
    let collection_parent = capture_standard_gitlink(
        &project_path,
        &collection_pin,
        "capture collection-only branch from paired module pin",
    );

    git_output_at(&standard_path, &["checkout", "--detach", math_pin.as_str()]);
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "fold divergent paired module upgrades");
    let paired_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &["branch", "paired-pin", paired_pin.as_str()],
    );
    let paired_parent = capture_standard_gitlink(
        &project_path,
        &paired_pin,
        "capture combined math and collection divergence fold",
    );

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let load = |parent: &str| {
        let parent = repository.resolve_snapshot(parent).unwrap();
        loader
            .load_committed_snapshot(&repository, &parent)
            .unwrap()
    };
    let math_project = load(&math_parent);
    let collection_project = load(&collection_parent);
    let paired_project = load(&paired_parent);
    (
        directory,
        [
            base_project,
            math_project,
            collection_project,
            paired_project,
        ],
        [base_pin, math_pin, collection_pin, paired_pin],
    )
}

fn paired_divergence_convergence_projects() -> (TempDir, [LoadedProject; 5], [String; 5]) {
    let (directory, projects, pins) = divergent_paired_module_projects();
    let [base_project, math_project, collection_project, paired_project] = projects;
    let project_path = directory.path().join("project");
    let standard_path = project_path.join("stdlib/std");

    git_output_at(
        &standard_path,
        &["checkout", "--detach", pins[1].as_str()],
    );
    git_output_at(
        &standard_path,
        &["merge", "--no-ff", "--no-commit", pins[2].as_str()],
    );
    commit_directory(&standard_path, "merge paired module divergence branches");
    let convergence_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &["branch", "paired-convergence-pin", convergence_pin.as_str()],
    );
    let convergence_parent = capture_standard_gitlink(
        &project_path,
        &convergence_pin,
        "capture paired module divergence convergence",
    );

    let repository = Repository::discover(&project_path).unwrap();
    let parent = repository
        .resolve_snapshot(&convergence_parent)
        .unwrap();
    let convergence_project = ProjectLoader::default()
        .load_committed_snapshot(&repository, &parent)
        .unwrap();
    (
        directory,
        [
            base_project,
            math_project,
            collection_project,
            paired_project,
            convergence_project,
        ],
        [
            pins[0].clone(),
            pins[1].clone(),
            pins[2].clone(),
            pins[3].clone(),
            convergence_pin,
        ],
    )
}

fn paired_divergence_escalation_projects() -> (TempDir, [LoadedProject; 6], [String; 6]) {
    let (directory, projects, pins) = divergent_paired_module_projects();
    let [
        base_project,
        math_project,
        collection_project,
        paired_project,
    ] = projects;
    let project_path = directory.path().join("project");
    let standard_path = project_path.join("stdlib/std");

    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "escalate paired collection pin to v6");
    let collection_escalation_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &[
            "branch",
            "collection-escalation-pin",
            collection_escalation_pin.as_str(),
        ],
    );
    let collection_escalation_parent = capture_standard_gitlink(
        &project_path,
        &collection_escalation_pin,
        "capture collection escalation from paired pin",
    );

    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v5.orna"),
    )
    .unwrap();
    commit_directory(&standard_path, "escalate paired math pin to v5");
    let math_escalation_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &[
            "branch",
            "math-escalation-pin",
            math_escalation_pin.as_str(),
        ],
    );
    let math_escalation_parent = capture_standard_gitlink(
        &project_path,
        &math_escalation_pin,
        "capture math escalation from collection pin",
    );

    let repository = Repository::discover(&project_path).unwrap();
    let load = |parent: &str| {
        let parent = repository.resolve_snapshot(parent).unwrap();
        ProjectLoader::default()
            .load_committed_snapshot(&repository, &parent)
            .unwrap()
    };
    let collection_escalation_project = load(&collection_escalation_parent);
    let math_escalation_project = load(&math_escalation_parent);
    (
        directory,
        [
            base_project,
            math_project,
            collection_project,
            paired_project,
            collection_escalation_project,
            math_escalation_project,
        ],
        [
            pins[0].clone(),
            pins[1].clone(),
            pins[2].clone(),
            pins[3].clone(),
            collection_escalation_pin,
            math_escalation_pin,
        ],
    )
}

fn paired_divergent_history_projects() -> (TempDir, [LoadedProject; 5], [String; 5]) {
    let (directory, projects, pins) = divergent_paired_module_projects();
    let [
        base_project,
        math_project,
        collection_project,
        math_first_project,
    ] = projects;
    let project_path = directory.path().join("project");
    let standard_path = project_path.join("stdlib/std");
    let base_pin = pins[0].clone();

    git_output_at(&standard_path, &["checkout", "--detach", base_pin.as_str()]);
    fs::write(
        standard_path.join("collection.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    )
    .unwrap();
    commit_directory(
        &standard_path,
        "upgrade collection before math on sibling fold",
    );
    let collection_first_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &[
            "branch",
            "collection-first-pin",
            collection_first_pin.as_str(),
        ],
    );

    fs::write(
        standard_path.join("math.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
    )
    .unwrap();
    commit_directory(
        &standard_path,
        "fold math after collection on sibling branch",
    );
    let collection_first_pair_pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    git_output_at(
        &standard_path,
        &[
            "branch",
            "collection-first-paired-pin",
            collection_first_pair_pin.as_str(),
        ],
    );
    let collection_first_parent = capture_standard_gitlink(
        &project_path,
        &collection_first_pair_pin,
        "capture collection-first paired fold",
    );

    let repository = Repository::discover(&project_path).unwrap();
    let parent = repository
        .resolve_snapshot(&collection_first_parent)
        .unwrap();
    let collection_first_project = ProjectLoader::default()
        .load_committed_snapshot(&repository, &parent)
        .unwrap();
    (
        directory,
        [
            base_project,
            math_project,
            math_first_project,
            collection_project,
            collection_first_project,
        ],
        [
            pins[0].clone(),
            pins[1].clone(),
            pins[3].clone(),
            pins[2].clone(),
            collection_first_pair_pin,
        ],
    )
}

fn paired_resolution_fold_projects() -> (TempDir, [LoadedProject; 4], [String; 4]) {
    let (directory, _, _, _, _, _, project_v6, snapshots) = module_upgrade_projects();
    let project_path = directory.path().join("project");
    let standard_path = project_path.join("stdlib/std");
    let mut projects = vec![project_v6];
    let mut pins = vec![snapshots[5].clone()];

    for (module, source, message) in [
        (
            "math.orna",
            include_str!("fixtures/module-upgrade-std-v4.orna"),
            "fold math back while collection remains v6",
        ),
        (
            "collection.orna",
            include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
            "fold collection back while math remains v4",
        ),
        (
            "math.orna",
            include_str!("fixtures/module-upgrade-std-v3.orna"),
            "fold math back while collection remains v5",
        ),
    ] {
        fs::write(standard_path.join(module), source).unwrap();
        commit_directory(&standard_path, message);
        let pin = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
        let parent = capture_standard_gitlink(&project_path, &pin, message);
        let repository = Repository::discover(&project_path).unwrap();
        let parent = repository.resolve_snapshot(&parent).unwrap();
        projects.push(
            ProjectLoader::default()
                .load_committed_snapshot(&repository, &parent)
                .unwrap(),
        );
        pins.push(pin);
    }

    (
        directory,
        projects.try_into().unwrap(),
        pins.try_into().unwrap(),
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

fn write_dependency_graph_sources(
    standard_path: &Path,
    shared: &str,
    left: &str,
    right: &str,
    aggregate: &str,
) {
    for (path, source) in [
        (
            "main.orna",
            include_str!("fixtures/snapshot-dependency-graph-main.orna"),
        ),
        ("graph/shared.orna", shared),
        ("graph/left.orna", left),
        ("graph/right.orna", right),
        ("graph/aggregate.orna", aggregate),
    ] {
        let path = standard_path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
}

fn dependency_graph_sources(
    shared: &str,
    left: &str,
    right: &str,
    aggregate: &str,
) -> Vec<(String, String)> {
    [
        (
            "std/main.orna",
            include_str!("fixtures/snapshot-dependency-graph-main.orna"),
        ),
        ("std/graph/shared.orna", shared),
        ("std/graph/left.orna", left),
        ("std/graph/right.orna", right),
        ("std/graph/aggregate.orna", aggregate),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), source.to_owned()))
    .collect()
}

fn dependency_graph_projects() -> (
    TempDir,
    Vec<LoadedProject>,
    Vec<String>,
    Vec<Vec<(String, String)>>,
) {
    let (directory, project_path, standard_path) = module_chain_repository();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/snapshot-dependency-graph-project.orna"),
    )
    .unwrap();
    write_dependency_graph_sources(
        &standard_path,
        include_str!("fixtures/snapshot-dependency-graph-shared-v1.orna"),
        include_str!("fixtures/snapshot-dependency-graph-left-v1.orna"),
        include_str!("fixtures/snapshot-dependency-graph-right-v1.orna"),
        include_str!("fixtures/snapshot-dependency-graph-aggregate-v1.orna"),
    );

    let mut standard_snapshots = Vec::with_capacity(5);
    let mut parent_snapshots = Vec::with_capacity(5);
    let mut capture = |message: &str| {
        commit_directory(&standard_path, message);
        let standard_snapshot = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
        parent_snapshots.push(capture_standard_gitlink(
            &project_path,
            &standard_snapshot,
            message,
        ));
        standard_snapshots.push(standard_snapshot);
    };
    capture("dependency graph snapshot v1");

    fs::write(
        standard_path.join("graph/shared.orna"),
        include_str!("fixtures/snapshot-dependency-graph-shared-v2.orna"),
    )
    .unwrap();
    capture("upgrade shared graph dependency");

    fs::write(
        standard_path.join("graph/left.orna"),
        include_str!("fixtures/snapshot-dependency-graph-left-v2.orna"),
    )
    .unwrap();
    capture("upgrade left graph branch");

    fs::write(
        standard_path.join("graph/right.orna"),
        include_str!("fixtures/snapshot-dependency-graph-right-v2.orna"),
    )
    .unwrap();
    capture("upgrade right graph branch");

    fs::write(
        standard_path.join("graph/aggregate.orna"),
        include_str!("fixtures/snapshot-dependency-graph-aggregate-v2.orna"),
    )
    .unwrap();
    capture("upgrade graph aggregator");
    drop(capture);

    let source_bundles = vec![
        dependency_graph_sources(
            include_str!("fixtures/snapshot-dependency-graph-shared-v1.orna"),
            include_str!("fixtures/snapshot-dependency-graph-left-v1.orna"),
            include_str!("fixtures/snapshot-dependency-graph-right-v1.orna"),
            include_str!("fixtures/snapshot-dependency-graph-aggregate-v1.orna"),
        ),
        dependency_graph_sources(
            include_str!("fixtures/snapshot-dependency-graph-shared-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-left-v1.orna"),
            include_str!("fixtures/snapshot-dependency-graph-right-v1.orna"),
            include_str!("fixtures/snapshot-dependency-graph-aggregate-v1.orna"),
        ),
        dependency_graph_sources(
            include_str!("fixtures/snapshot-dependency-graph-shared-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-left-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-right-v1.orna"),
            include_str!("fixtures/snapshot-dependency-graph-aggregate-v1.orna"),
        ),
        dependency_graph_sources(
            include_str!("fixtures/snapshot-dependency-graph-shared-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-left-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-right-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-aggregate-v1.orna"),
        ),
        dependency_graph_sources(
            include_str!("fixtures/snapshot-dependency-graph-shared-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-left-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-right-v2.orna"),
            include_str!("fixtures/snapshot-dependency-graph-aggregate-v2.orna"),
        ),
    ];

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let projects = parent_snapshots
        .iter()
        .zip(&standard_snapshots)
        .zip(&source_bundles)
        .map(|((parent_snapshot, standard_snapshot), sources)| {
            let parent_snapshot = repository.resolve_snapshot(parent_snapshot).unwrap();
            assert_eq!(
                repository
                    .committed_submodule_commit(&parent_snapshot, "stdlib/std")
                    .unwrap()
                    .as_str(),
                standard_snapshot
            );
            let profile =
                StandardDependencyProfile::from_sources(standard_snapshot.clone(), sources.clone())
                    .unwrap();
            loader
                .load_committed_snapshot_with_standard_profile(
                    &repository,
                    &parent_snapshot,
                    Some(profile),
                )
                .unwrap()
        })
        .collect();
    (directory, projects, standard_snapshots, source_bundles)
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

fn module_chain_sources(leaf: &str) -> Vec<(String, String)> {
    [
        (
            "std/main.orna",
            include_str!("fixtures/module-chain-std-main.orna"),
        ),
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
        ("std/chain/leaf.orna", leaf),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), source.to_owned()))
    .collect()
}

fn module_chain_repin_projects() -> (
    TempDir,
    Vec<LoadedProject>,
    [String; 3],
    [Vec<(String, String)>; 3],
) {
    let (directory, project_path, standard_path) = module_chain_repository();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/module-chain-repin-project.orna"),
    )
    .unwrap();
    let leaves = [
        include_str!("fixtures/module-chain-std-leaf-v1.orna"),
        include_str!("fixtures/module-chain-std-leaf-repin-v2.orna"),
        include_str!("fixtures/module-chain-std-leaf-repin-v3.orna"),
    ];
    let source_bundles = leaves.map(module_chain_sources);
    let mut snapshots = Vec::with_capacity(3);
    let mut parent_snapshots = Vec::with_capacity(3);

    for (index, leaf) in leaves.iter().enumerate() {
        write_module_chain_version(
            &standard_path,
            include_str!("fixtures/module-chain-std-math-v1.orna"),
            include_str!("fixtures/module-chain-std-collection-v1.orna"),
            include_str!("fixtures/module-chain-std-entry-v1.orna"),
            include_str!("fixtures/module-chain-std-bridge-v1.orna"),
            leaf,
        );
        commit_directory(
            &standard_path,
            &format!("repin stable module chain {}", index + 1),
        );
        let standard_snapshot = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
        parent_snapshots.push(capture_standard_gitlink(
            &project_path,
            &standard_snapshot,
            &format!("capture stable module repin {}", index + 1),
        ));
        snapshots.push(standard_snapshot);
    }

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let projects = parent_snapshots
        .iter()
        .zip(&snapshots)
        .zip(&source_bundles)
        .map(|((parent_snapshot, standard_snapshot), sources)| {
            let parent_snapshot = repository.resolve_snapshot(parent_snapshot).unwrap();
            assert_eq!(
                repository
                    .committed_submodule_commit(&parent_snapshot, "stdlib/std")
                    .unwrap()
                    .as_str(),
                standard_snapshot
            );
            let profile =
                StandardDependencyProfile::from_sources(standard_snapshot.clone(), sources.clone())
                    .unwrap();
            loader
                .load_committed_snapshot_with_standard_profile(
                    &repository,
                    &parent_snapshot,
                    Some(profile),
                )
                .unwrap()
        })
        .collect();

    (
        directory,
        projects,
        [
            snapshots.remove(0),
            snapshots.remove(0),
            snapshots.remove(0),
        ],
        source_bundles,
    )
}

fn nested_module_pin_sources(collection: &str, leaf: &str) -> Vec<(String, String)> {
    [
        (
            "std/main.orna",
            include_str!("fixtures/module-chain-std-main.orna"),
        ),
        (
            "std/math.orna",
            include_str!("fixtures/module-chain-std-math-v1.orna"),
        ),
        ("std/collection.orna", collection),
        (
            "std/chain/entry.orna",
            include_str!("fixtures/nested-module-pin-entry.orna"),
        ),
        (
            "std/chain/bridge.orna",
            include_str!("fixtures/nested-module-pin-bridge.orna"),
        ),
        ("std/chain/leaf.orna", leaf),
    ]
    .into_iter()
    .map(|(path, source)| (path.to_owned(), source.to_owned()))
    .collect()
}

fn nested_module_pin_projects() -> (
    TempDir,
    PathBuf,
    Vec<String>,
    Vec<LoadedProject>,
    [String; 3],
    [Vec<(String, String)>; 3],
) {
    let (directory, project_path, standard_path) = module_chain_repository();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/nested-module-pin-project.orna"),
    )
    .unwrap();
    let collections = [
        include_str!("fixtures/module-chain-std-collection-v1.orna"),
        include_str!("fixtures/module-chain-std-collection-v1.orna"),
        include_str!("fixtures/nested-module-pin-collection-v2.orna"),
    ];
    let leaves = [
        include_str!("fixtures/nested-module-pin-leaf-v1.orna"),
        include_str!("fixtures/nested-module-pin-leaf-v2.orna"),
        include_str!("fixtures/nested-module-pin-leaf-v2.orna"),
    ];
    let source_bundles = [
        nested_module_pin_sources(collections[0], leaves[0]),
        nested_module_pin_sources(collections[1], leaves[1]),
        nested_module_pin_sources(collections[2], leaves[2]),
    ];
    let mut snapshots = Vec::with_capacity(3);
    let mut parent_snapshots = Vec::with_capacity(3);

    for index in 0..source_bundles.len() {
        write_module_chain_version(
            &standard_path,
            include_str!("fixtures/module-chain-std-math-v1.orna"),
            collections[index],
            include_str!("fixtures/nested-module-pin-entry.orna"),
            include_str!("fixtures/nested-module-pin-bridge.orna"),
            leaves[index],
        );
        commit_directory(
            &standard_path,
            &format!("nested module repin snapshot {}", index + 1),
        );
        let standard_snapshot = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
        parent_snapshots.push(capture_standard_gitlink(
            &project_path,
            &standard_snapshot,
            &format!("capture nested repin {}", index + 1),
        ));
        snapshots.push(standard_snapshot);
    }

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let projects = parent_snapshots
        .iter()
        .zip(&snapshots)
        .zip(&source_bundles)
        .map(|((parent_snapshot, standard_snapshot), sources)| {
            let parent_snapshot = repository.resolve_snapshot(parent_snapshot).unwrap();
            assert_eq!(
                repository
                    .committed_submodule_commit(&parent_snapshot, "stdlib/std")
                    .unwrap()
                    .as_str(),
                standard_snapshot
            );
            let profile =
                StandardDependencyProfile::from_sources(standard_snapshot.clone(), sources.clone())
                    .unwrap();
            loader
                .load_committed_snapshot_with_standard_profile(
                    &repository,
                    &parent_snapshot,
                    Some(profile),
                )
                .unwrap()
        })
        .collect();

    (
        directory,
        project_path,
        parent_snapshots,
        projects,
        [
            snapshots.remove(0),
            snapshots.remove(0),
            snapshots.remove(0),
        ],
        source_bundles,
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
fn paired_module_pin_upgrades_replay_computed_values_deterministically() {
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
    let projects = [&project_v4, &project_v5, &project_v6];
    let pins = [&snapshots[3], &snapshots[4], &snapshots[5]];
    // The reference specifies captured dependency behavior, not a snapshot-token function.
    let expected_values = [1_011, 10_012, 100_013];
    let expected_math = [
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v5.orna"),
    ];
    let expected_collection = [
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
    ];
    let replay = include_str!("fixtures/module-upgrade-paired-call.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(project.standard_profile().unwrap().snapshot(), pins[index]);
        assert_eq!(
            standard_source(project, "std/math.orna"),
            expected_math[index]
        );
        assert_eq!(
            standard_source(project, "std/collection.orna"),
            expected_collection[index]
        );
        if index > 0 {
            assert_ne!(
                standard_source(projects[index - 1], "std/math.orna"),
                standard_source(project, "std/math.orna"),
                "each paired upgrade changes std.math under its new pin"
            );
            assert_ne!(
                standard_source(projects[index - 1], "std/collection.orna"),
                standard_source(project, "std/collection.orna"),
                "each paired upgrade changes std.collection under its new pin"
            );
        }
    }

    for index in 0..projects.len() - 1 {
        for path in ["std/math.orna", "std/collection.orna"] {
            let mut mixed_sources = projects[index].standard_sources().to_vec();
            let upgraded_source = standard_source(projects[index + 1], path).to_owned();
            mixed_sources
                .iter_mut()
                .find(|(candidate, _)| candidate == path)
                .unwrap()
                .1 = upgraded_source;
            assert_eq!(
                AdmittedReplSession::from_loaded_project(
                    projects[index],
                    mixed_sources,
                    Limits::default(),
                )
                .unwrap_err()
                .code(),
                "ORNA-REPL-STANDARD",
                "snapshot {} must reject upgraded source {path}",
                pins[index]
            );
        }
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
    for (session, expected) in sessions.iter_mut().zip(expected_values) {
        assert_eq!(session.submit(replay), Ok(Some(int(expected))));
    }

    for index in [2, 0, 1, 2, 1, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(int(expected_values[index]))),
            "replaying the paired modules must retain snapshot {}'s value",
            pins[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in (0..cloned_sessions.len()).rev() {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(int(expected_values[index]))),
            "cloned snapshot {} must replay the same paired-module value",
            pins[index]
        );
    }
}

#[test]
fn captured_pair_resolution_folds_keep_each_pinned_snapshot_identity() {
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
    let projects = [&project_v4, &project_v5, &project_v6];
    let pins = [&snapshots[3], &snapshots[4], &snapshots[5]];
    let expected_values = [1_011, 10_012, 100_013];
    let use_pair = include_str!("fixtures/module-upgrade-paired-use.orna");
    let fold_pair = include_str!("fixtures/module-upgrade-paired-fold.orna");

    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(project.standard_profile().unwrap().snapshot(), pins[index]);
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(
            session.submit(fold_pair),
            Ok(Some(int(expected_values[index]))),
            "snapshot {} must resolve the paired imports from its captured sources",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [2, 0, 1, 2, 1, 0] {
        assert_eq!(
            sessions[index].submit(fold_pair),
            Ok(Some(int(expected_values[index]))),
            "interleaved resolution fold must retain snapshot {}'s imported functions",
            pins[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in (0..cloned_sessions.len()).rev() {
        assert_eq!(
            cloned_sessions[index].submit(fold_pair),
            Ok(Some(int(expected_values[index]))),
            "cloned snapshot {} must preserve both resolved function identities",
            pins[index]
        );
    }
}

#[test]
fn nested_paired_resolution_keeps_import_and_qualified_paths_on_each_pin() {
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
    let projects = [&project_v4, &project_v5, &project_v6];
    let pins = [&snapshots[3], &snapshots[4], &snapshots[5]];
    let expected = [[1_011, 1_012], [10_012, 10_013], [100_013, 100_014]];
    let use_pair = include_str!("fixtures/module-upgrade-paired-use.orna");
    let replay = include_str!("fixtures/module-upgrade-paired-resolution-depth.orna");

    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(project.standard_profile().unwrap().snapshot(), pins[index]);
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        let output = session.submit(replay).unwrap_or_else(|error| {
            panic!(
                "nested imported and qualified calls failed on snapshot {}: {}",
                pins[index],
                error.code()
            )
        });
        assert_eq!(
            output,
            Some(ints(&expected[index])),
            "nested imported and qualified calls must resolve from snapshot {}",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [2, 0, 1, 2, 1, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "interleaved nested resolution must retain snapshot {}'s paired functions",
            pins[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in (0..cloned_sessions.len()).rev() {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned nested resolution must retain snapshot {}'s paired functions",
            pins[index]
        );
    }
}

#[test]
fn divergent_paired_pins_keep_same_named_exports_with_their_module_identity() {
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
    let projects = [&project_v4, &project_v5, &project_v6];
    let pins = [&snapshots[3], &snapshots[4], &snapshots[5]];
    let expected = [[40, 4, 1_007], [50, 5, 10_007], [60, 6, 100_007]];
    let expected_math = [
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v5.orna"),
    ];
    let expected_collection = [
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
    ];
    let use_pair = include_str!("fixtures/module-upgrade-paired-divergence-use.orna");
    let fold_pair = include_str!("fixtures/module-upgrade-paired-divergence-fold.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(project.standard_profile().unwrap().snapshot(), pins[index]);
        assert_eq!(
            standard_source(project, "std/math.orna"),
            expected_math[index]
        );
        assert_eq!(
            standard_source(project, "std/collection.orna"),
            expected_collection[index]
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(
            session.submit(fold_pair),
            Ok(Some(ints(&expected[index]))),
            "snapshot {} must keep each marker bound to its module and pin",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [2, 0, 1, 2, 1, 0] {
        assert_eq!(
            sessions[index].submit(fold_pair),
            Ok(Some(ints(&expected[index]))),
            "divergent paired fold must retain snapshot {}'s module identities",
            pins[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in (0..cloned_sessions.len()).rev() {
        assert_eq!(
            cloned_sessions[index].submit(fold_pair),
            Ok(Some(ints(&expected[index]))),
            "cloned fold must retain snapshot {}'s module identities",
            pins[index]
        );
    }

    for index in 0..projects.len() - 1 {
        let mut crossed_sources = projects[index].standard_sources().to_vec();
        for path in ["std/math.orna", "std/collection.orna"] {
            let next_source = standard_source(projects[index + 1], path).to_owned();
            crossed_sources
                .iter_mut()
                .find(|(candidate, _)| candidate == path)
                .unwrap()
                .1 = next_source;
        }
        assert_eq!(
            AdmittedReplSession::from_loaded_project(
                projects[index],
                crossed_sources,
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-REPL-STANDARD",
            "snapshot {} must reject the paired sources from snapshot {}",
            pins[index],
            pins[index + 1]
        );
    }
}

#[test]
fn paired_dependency_upgrade_replay_keeps_each_original_snapshot_pin() {
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
    let expected = [
        [8, 3, 11],
        [107, 3, 110],
        [1_007, 3, 1_010],
        [1_007, 4, 1_011],
        [10_007, 5, 10_012],
        [100_007, 6, 100_013],
    ];
    let use_pair = include_str!("fixtures/module-upgrade-paired-use.orna");
    let replay = include_str!("fixtures/module-upgrade-snapshot-paired-identity-fold.orna");

    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    assert_eq!(
        standard_source(&project_v3, "std/math.orna"),
        standard_source(&project_v4, "std/math.orna"),
        "v3 to v4 changes collection while math remains the same"
    );
    assert_ne!(
        standard_source(&project_v3, "std/collection.orna"),
        standard_source(&project_v4, "std/collection.orna")
    );
    assert_ne!(
        standard_source(&project_v4, "std/math.orna"),
        standard_source(&project_v5, "std/math.orna")
    );
    assert_ne!(
        standard_source(&project_v4, "std/collection.orna"),
        standard_source(&project_v5, "std/collection.orna")
    );

    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &snapshots[index],
            "project v{} must retain its captured std pin",
            index + 1
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "snapshot {} must compute its own math, collection, and paired values",
            snapshots[index]
        );
        sessions.push(session);
    }

    for index in [5, 0, 3, 1, 4, 2, 5, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "replay after later dependency upgrades must preserve snapshot {}",
            snapshots[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in [2, 4, 1, 5, 0, 3] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned replay must preserve snapshot {} after the full upgrade chain",
            snapshots[index]
        );
    }
}

#[test]
fn paired_snapshot_pin_replay_matches_alias_and_qualified_values_across_upgrades() {
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
    let expected = [
        [8, 3, 11, 9, 3, 12],
        [107, 3, 110, 108, 3, 111],
        [1_007, 3, 1_010, 1_008, 3, 1_011],
        [1_007, 4, 1_011, 1_008, 4, 1_012],
        [10_007, 5, 10_012, 10_008, 5, 10_013],
        [100_007, 6, 100_013, 100_008, 6, 100_014],
    ];
    let use_modules = include_str!("fixtures/module-upgrade-module-pin-replay-use.orna");
    let replay = include_str!("fixtures/module-upgrade-module-pin-replay-fold.orna");

    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &snapshots[index],
            "project v{} must retain its captured paired-module pin",
            index + 1
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_modules.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "snapshot {} must give aliases and qualified calls the same captured module pair",
            snapshots[index]
        );
        sessions.push(session);
    }

    for index in [5, 0, 3, 1, 4, 2, 5, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "interleaved replay must preserve alias and qualified values for snapshot {}",
            snapshots[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in [2, 4, 1, 5, 0, 3] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned replay must preserve paired resolution for snapshot {}",
            snapshots[index]
        );
    }
}

#[test]
fn captured_dependency_snapshots_survive_paired_pin_replay_folds() {
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
    let expected = [
        [8, 3, 11, 9, 3, 12],
        [107, 3, 110, 108, 3, 111],
        [1_007, 3, 1_010, 1_008, 3, 1_011],
        [1_007, 4, 1_011, 1_008, 4, 1_012],
        [10_007, 5, 10_012, 10_008, 5, 10_013],
        [100_007, 6, 100_013, 100_008, 6, 100_014],
    ];
    let use_modules = include_str!("fixtures/module-upgrade-module-pin-replay-use.orna");
    let capture = include_str!("fixtures/module-upgrade-captured-paired-pin-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-captured-paired-pin-replay.orna");

    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &snapshots[index],
            "project v{} must keep the dependency pin captured by its closure",
            index + 1
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_modules.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        let captured = session.submit(capture);
        assert!(
            captured.is_ok(),
            "snapshot {} rejected its captured paired function with {}",
            snapshots[index],
            captured.as_ref().unwrap_err().code()
        );
        assert_eq!(captured, Ok(None));
        sessions.push(session);
    }

    for index in [5, 0, 3, 1, 4, 2, 5, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "captured closure must replay its own paired dependencies at snapshot {}",
            snapshots[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in [2, 4, 1, 5, 0, 3] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned captured closure must retain paired pin {}",
            snapshots[index]
        );
    }
}

#[test]
fn captured_same_named_exports_keep_paired_resolution_pin_identity() {
    let (
        _directory,
        _project_v1,
        _project_v2,
        project_v3,
        project_v4,
        project_v5,
        project_v6,
        snapshots,
    ) = module_upgrade_projects();
    let projects = [&project_v3, &project_v4, &project_v5, &project_v6];
    let pins = [&snapshots[2], &snapshots[3], &snapshots[4], &snapshots[5]];
    let expected = [
        [40, 3, 43, 40, 3, 43, 1_007, 1_008],
        [40, 4, 44, 40, 4, 44, 1_007, 1_008],
        [50, 5, 55, 50, 5, 55, 10_007, 10_008],
        [60, 6, 66, 60, 6, 66, 100_007, 100_008],
    ];
    let use_pair = include_str!("fixtures/module-upgrade-paired-divergence-use.orna");
    let capture = include_str!("fixtures/module-upgrade-paired-resolution-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-paired-resolution-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            pins[index],
            "project v{} must retain its paired module snapshot",
            index + 3
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(session.submit(capture), Ok(None));
        sessions.push(session);
    }

    for index in [3, 0, 2, 1, 3, 1, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "same-named math and collection exports must remain paired at pin {}",
            pins[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in [1, 3, 0, 2] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned resolution replay must preserve both module identities at pin {}",
            pins[index]
        );
    }
}

#[test]
fn captured_replays_preserve_divergent_paired_dependency_pins() {
    let (_directory, projects, pins) = divergent_paired_module_projects();
    let expected = [
        [40, 4, 44, 40, 4, 44, 1_007, 1_008],
        [50, 4, 54, 50, 4, 54, 10_007, 10_008],
        [40, 5, 45, 40, 5, 45, 1_007, 1_008],
        [50, 5, 55, 50, 5, 55, 10_007, 10_008],
    ];
    let expected_math = [
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
    ];
    let expected_collection = [
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    ];
    let use_pair = include_str!("fixtures/module-upgrade-paired-divergence-use.orna");
    let capture = include_str!("fixtures/module-upgrade-divergent-pair-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-divergent-pair-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    for (index, project) in projects.iter().enumerate() {
        let profile = project.standard_profile().unwrap_or_else(|| {
            panic!(
                "divergent project {index} has no captured standard profile; modules: {:?}",
                project.standard_modules()
            )
        });
        assert_eq!(profile.snapshot(), &pins[index]);
        assert_eq!(
            standard_source(project, "std/math.orna"),
            expected_math[index]
        );
        assert_eq!(
            standard_source(project, "std/collection.orna"),
            expected_collection[index]
        );
    }
    assert_eq!(
        standard_source(&projects[0], "std/collection.orna"),
        standard_source(&projects[1], "std/collection.orna"),
        "the math-only fork retains the base collection module"
    );
    assert_eq!(
        standard_source(&projects[0], "std/math.orna"),
        standard_source(&projects[2], "std/math.orna"),
        "the collection-only fork retains the base math module"
    );

    for (pinned_index, replacement_index, path) in
        [(1, 2, "std/collection.orna"), (2, 1, "std/math.orna")]
    {
        let mut crossed_sources = projects[pinned_index].standard_sources().to_vec();
        let replacement = standard_source(&projects[replacement_index], path).to_owned();
        crossed_sources
            .iter_mut()
            .find(|(candidate, _)| candidate == path)
            .unwrap()
            .1 = replacement;
        assert_eq!(
            AdmittedReplSession::from_loaded_project(
                &projects[pinned_index],
                crossed_sources,
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-REPL-STANDARD",
            "snapshot {} must reject the crossed {path} from snapshot {}",
            pins[pinned_index],
            pins[replacement_index]
        );
    }

    let mut sessions = Vec::with_capacity(projects.len());
    for project in &projects {
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(session.submit(capture), Ok(None));
        sessions.push(session);
    }

    for index in [3, 0, 2, 1, 3, 1, 0, 2] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "captured fold must keep both divergent module identities at pin {}",
            pins[index]
        );
    }

    let mut cloned_sessions = sessions.clone();
    for index in [2, 3, 0, 1] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned captured fold must preserve divergent pin {}",
            pins[index]
        );
    }
}

#[test]
fn captured_paired_pin_identity_survives_opposite_divergence_folds() {
    let (_directory, projects, pins) = paired_divergent_history_projects();
    let expected = [
        [40, 4, 44, 40, 4, 44, 1_007, 1_008],
        [50, 4, 54, 50, 4, 54, 10_007, 10_008],
        [50, 5, 55, 50, 5, 55, 10_007, 10_008],
        [40, 5, 45, 40, 5, 45, 1_007, 1_008],
        [50, 5, 55, 50, 5, 55, 10_007, 10_008],
    ];
    let expected_math = [
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
    ];
    let expected_collection = [
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    ];
    let use_pair = include_str!("fixtures/module-upgrade-paired-divergence-use.orna");
    let capture = include_str!("fixtures/module-upgrade-divergent-history-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-divergent-history-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    assert_ne!(
        pins[2], pins[4],
        "opposite fold orders have distinct snapshot identities"
    );
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &pins[index],
            "history project {index} must retain its own paired module pin"
        );
        assert_eq!(
            standard_source(project, "std/math.orna"),
            expected_math[index]
        );
        assert_eq!(
            standard_source(project, "std/collection.orna"),
            expected_collection[index]
        );
    }
    for path in ["std/math.orna", "std/collection.orna"] {
        assert_eq!(
            standard_source(&projects[2], path),
            standard_source(&projects[4], path),
            "opposite fold orders resolve the same final source for {path}"
        );
    }

    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in use_pair.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(session.submit(capture), Ok(None));
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "captured behavior must be computed from history pin {}",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [4, 2, 0, 3, 1, 4, 2, 1] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "interleaved replay must retain paired module pin {}",
            pins[index]
        );
    }
    let mut cloned_sessions = sessions.clone();
    for index in [2, 4, 1, 3, 0] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned replay must retain paired module pin {}",
            pins[index]
        );
    }
}

#[test]
fn captured_snapshot_identity_survives_paired_divergence_escalations() {
    let (_directory, projects, pins) = paired_divergence_escalation_projects();
    let expected = [
        [40, 4, 44, 40, 4, 1_007, 1_008],
        [50, 4, 54, 50, 4, 10_007, 10_008],
        [40, 5, 45, 40, 5, 1_007, 1_008],
        [50, 5, 55, 50, 5, 10_007, 10_008],
        [50, 6, 56, 50, 6, 10_007, 10_008],
        [60, 6, 66, 60, 6, 100_007, 100_008],
    ];
    let expected_math = [
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v3.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v5.orna"),
    ];
    let expected_collection = [
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v4.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
    ];
    let imports = include_str!("fixtures/module-upgrade-paired-divergence-use.orna");
    let capture = include_str!("fixtures/module-upgrade-divergence-escalation-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-divergence-escalation-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &pins[index],
            "divergence escalation {index} must preserve its captured snapshot pin"
        );
        assert_eq!(
            standard_source(project, "std/math.orna"),
            expected_math[index]
        );
        assert_eq!(
            standard_source(project, "std/collection.orna"),
            expected_collection[index]
        );
    }
    for (before, after, unchanged, changed) in [
        (0, 1, "std/collection.orna", "std/math.orna"),
        (0, 2, "std/math.orna", "std/collection.orna"),
        (1, 3, "std/math.orna", "std/collection.orna"),
        (3, 4, "std/math.orna", "std/collection.orna"),
        (4, 5, "std/collection.orna", "std/math.orna"),
    ] {
        assert_eq!(
            standard_source(&projects[before], unchanged),
            standard_source(&projects[after], unchanged),
            "escalation {before} to {after} must retain {unchanged}"
        );
        assert_ne!(
            standard_source(&projects[before], changed),
            standard_source(&projects[after], changed),
            "escalation {before} to {after} must change {changed}"
        );
    }

    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in imports.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(session.submit(capture), Ok(None));
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "captured math and collection values must come from pin {}",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [5, 0, 3, 2, 4, 1, 5, 2, 3, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "interleaved replay must preserve escalation pin {}",
            pins[index]
        );
    }
    let mut cloned_sessions = sessions.clone();
    for index in [2, 5, 1, 4, 0, 3] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned replay must preserve escalation pin {}",
            pins[index]
        );
    }
}

#[test]
fn captured_snapshot_identity_survives_paired_divergence_suppression_folds() {
    let (_directory, projects, pins) = paired_divergence_escalation_projects();
    let expected = [
        [99, 40, 4, 40, 4, 1_007, 1_008],
        [99, 50, 4, 50, 4, 10_007, 10_008],
        [99, 40, 5, 40, 5, 1_007, 1_008],
        [99, 50, 5, 50, 5, 10_007, 10_008],
        [99, 50, 6, 50, 6, 10_007, 10_008],
        [99, 60, 6, 60, 6, 100_007, 100_008],
    ];
    let shadow = include_str!("fixtures/module-upgrade-divergence-suppression-shadow.orna");
    let imports = include_str!("fixtures/module-upgrade-divergence-suppression-use.orna");
    let capture = include_str!("fixtures/module-upgrade-divergence-suppression-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-divergence-suppression-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &pins[index],
            "suppressed paired imports must retain dependency pin {}",
            pins[index]
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(session.submit(shadow), Ok(None));
        for import in imports.lines() {
            assert_eq!(
                session.submit(import),
                Ok(None),
                "wildcard imports must accept local marker suppression at pin {}",
                pins[index]
            );
        }
        assert_eq!(session.submit(capture), Ok(None));
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "local marker suppression must preserve qualified paired values at pin {}",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [5, 0, 3, 2, 4, 1, 5, 2, 3, 0] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "interleaved suppressed replay must retain pin {}",
            pins[index]
        );
    }
    let mut cloned_sessions = sessions.clone();
    for index in [2, 5, 1, 4, 0, 3] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned suppressed replay must retain pin {}",
            pins[index]
        );
    }
}

#[test]
fn captured_paired_resolution_identity_survives_stepwise_repins() {
    let (_directory, projects, pins) = paired_resolution_fold_projects();
    let expected = [
        [60, 6, 66, 60, 6, 66, 100_007, 100_008],
        [50, 6, 56, 50, 6, 56, 10_007, 10_008],
        [50, 5, 55, 50, 5, 55, 10_007, 10_008],
        [40, 5, 45, 40, 5, 45, 1_007, 1_008],
    ];
    let expected_math = [
        include_str!("fixtures/module-upgrade-std-v5.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v4.orna"),
        include_str!("fixtures/module-upgrade-std-v3.orna"),
    ];
    let expected_collection = [
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v6.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
        include_str!("fixtures/module-upgrade-std-collection-v5.orna"),
    ];
    let imports = include_str!("fixtures/module-upgrade-module-pin-replay-use.orna");
    let capture = include_str!("fixtures/module-upgrade-paired-fold-capture.orna");
    let replay = include_str!("fixtures/module-upgrade-paired-fold-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            &pins[index],
            "stepwise fold {index} must retain its own standard pin"
        );
        assert_eq!(
            standard_source(project, "std/math.orna"),
            expected_math[index]
        );
        assert_eq!(
            standard_source(project, "std/collection.orna"),
            expected_collection[index]
        );
    }
    for (before, after, unchanged, changed) in [
        (0, 1, "std/collection.orna", "std/math.orna"),
        (1, 2, "std/math.orna", "std/collection.orna"),
        (2, 3, "std/collection.orna", "std/math.orna"),
    ] {
        assert_eq!(
            standard_source(&projects[before], unchanged),
            standard_source(&projects[after], unchanged),
            "fold {before} to {after} must preserve {unchanged}"
        );
        assert_ne!(
            standard_source(&projects[before], changed),
            standard_source(&projects[after], changed),
            "fold {before} to {after} must update {changed}"
        );
    }

    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in imports.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        assert_eq!(session.submit(capture), Ok(None));
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "captured pair must compute the values for standard pin {}",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [3, 0, 2, 1, 3, 1, 0, 2] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "interleaved replay must retain the paired resolution at pin {}",
            pins[index]
        );
    }
    let mut cloned_sessions = sessions.clone();
    for index in [2, 0, 3, 1] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned replay must retain the paired resolution at pin {}",
            pins[index]
        );
    }
}

#[test]
fn captured_dependency_pin_identity_survives_nested_replay_chains() {
    let (
        _directory,
        _project_v1,
        _project_v2,
        project_v3,
        project_v4,
        project_v5,
        project_v6,
        snapshots,
    ) = module_upgrade_projects();
    let projects = [&project_v3, &project_v4, &project_v5, &project_v6];
    let pins = [&snapshots[2], &snapshots[3], &snapshots[4], &snapshots[5]];
    let expected = [
        [1_010, 2_018, 2_021],
        [1_011, 2_019, 2_023],
        [10_012, 20_020, 20_025],
        [100_013, 200_021, 200_027],
    ];
    let imports = include_str!("fixtures/module-upgrade-module-pin-replay-use.orna");
    let leaf = include_str!("fixtures/module-upgrade-pin-chain-leaf.orna");
    let middle = include_str!("fixtures/module-upgrade-pin-chain-middle.orna");
    let root = include_str!("fixtures/module-upgrade-pin-chain-root.orna");
    let replay = include_str!("fixtures/module-upgrade-pin-chain-replay.orna");

    assert!(pins.windows(2).all(|pair| pair[0] != pair[1]));
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            pins[index],
            "project at chain depth {index} must retain its paired dependency pin"
        );
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .unwrap();
        for import in imports.lines() {
            assert_eq!(session.submit(import), Ok(None));
        }
        for (layer, definition) in [("leaf", leaf), ("middle", middle), ("root", root)] {
            assert_eq!(
                session.submit(definition),
                Ok(None),
                "capturing {layer} must succeed at paired pin {}",
                pins[index]
            );
        }
        assert_eq!(
            session.submit(replay),
            Ok(Some(ints(&expected[index]))),
            "each captured layer must compute the values for paired pin {}",
            pins[index]
        );
        sessions.push(session);
    }

    for index in [3, 0, 2, 1, 3, 1, 0, 2] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "nested dependency replay must retain pin {} after other pins run",
            pins[index]
        );
    }
    let mut cloned_sessions = sessions.clone();
    for index in [2, 0, 3, 1] {
        assert_eq!(
            cloned_sessions[index].submit(replay),
            Ok(Some(ints(&expected[index]))),
            "cloned nested dependency replay must retain pin {}",
            pins[index]
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

#[test]
fn diamond_dependency_snapshots_replay_deterministically_when_interleaved() {
    let (_directory, projects, snapshots, source_bundles) = dependency_graph_projects();
    assert_eq!(projects.len(), 5);
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));

    let expected_results = [1112, 2122, 2322, 2324, 23024];
    let mut sessions = Vec::with_capacity(projects.len());
    for (index, project) in projects.iter().enumerate() {
        let profile = project
            .standard_profile()
            .expect("each gitlink pin carries an explicit profile");
        assert_eq!(profile.snapshot(), snapshots[index]);
        assert_eq!(
            project
                .standard_modules()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["std/graph/aggregate.orna"]
        );
        assert_eq!(
            source_bundles[index]
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            [
                "std/main.orna",
                "std/graph/shared.orna",
                "std/graph/left.orna",
                "std/graph/right.orna",
                "std/graph/aggregate.orna",
            ]
        );
        for (path, source) in &source_bundles[index] {
            let parsed = orna_syntax_v1::parse_module_with_file(source, path);
            assert!(
                parsed.is_ok(),
                "fixture {path} did not parse: {:?}",
                parsed.diagnostics
            );
        }
        project
            .standard_catalogue(source_bundles[index].clone())
            .unwrap_or_else(|error| {
                panic!(
                    "snapshot {} rejected its source catalogue: {error:?}",
                    snapshots[index]
                )
            })
            .expect("the explicit graph profile creates a catalogue");
        sessions.push(
            AdmittedReplSession::from_loaded_project(
                project,
                source_bundles[index].clone(),
                Limits::default(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "snapshot {} rejected its graph sources: {}",
                    snapshots[index],
                    error.code()
                )
            }),
        );
    }

    let use_graph = include_str!("fixtures/snapshot-dependency-graph-use.orna");
    for session in &mut sessions {
        assert_eq!(
            session.submit(use_graph),
            Ok(None),
            "a captured aggregate dependency should be importable"
        );
    }
    let replay = include_str!("fixtures/snapshot-dependency-graph-call.orna");
    for index in [4, 2, 0, 3, 1, 4, 0, 1, 3, 2] {
        let actual = sessions[index].submit(replay).unwrap_or_else(|error| {
            panic!(
                "snapshot {} rejected graph replay: {}",
                snapshots[index],
                error.code()
            )
        });
        assert_eq!(
            actual,
            Some(int(expected_results[index])),
            "snapshot {} changed its computed graph value during interleaved replay",
            snapshots[index]
        );
    }
}

#[test]
fn dependency_graph_replays_reject_divergence_without_retargeting_values() {
    let (_directory, projects, snapshots, source_bundles) = dependency_graph_projects();
    let expected_results = [1112, 2122, 2322, 2324, 23024];
    let use_graph = include_str!("fixtures/snapshot-dependency-graph-use.orna");
    let replay = include_str!("fixtures/snapshot-dependency-graph-call.orna");
    let mut sessions = Vec::with_capacity(projects.len());

    for (index, project) in projects.iter().enumerate() {
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            source_bundles[index].clone(),
            Limits::default(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "pinned program {} could not be replayed: {}",
                snapshots[index],
                error.code()
            )
        });
        assert_eq!(session.submit(use_graph), Ok(None));
        assert_eq!(
            session.submit(replay),
            Ok(Some(int(expected_results[index]))),
            "program {} must compute with its own captured dependency graph",
            snapshots[index]
        );
        sessions.push(session);
    }

    let mut divergent_edges = 0;

    for index in 0..projects.len() - 1 {
        assert_ne!(snapshots[index], snapshots[index + 1]);
        assert_eq!(
            projects[index].standard_profile().unwrap().snapshot(),
            snapshots[index]
        );
        assert_eq!(
            projects[index + 1].standard_profile().unwrap().snapshot(),
            snapshots[index + 1]
        );

        for (path, older_source) in &source_bundles[index] {
            let newer_source = source_bundles[index + 1]
                .iter()
                .find(|(candidate, _)| candidate == path)
                .unwrap()
                .1
                .as_str();
            if older_source == newer_source {
                continue;
            }
            divergent_edges += 1;

            let mut older_program_with_newer_edge = source_bundles[index].clone();
            older_program_with_newer_edge
                .iter_mut()
                .find(|(candidate, _)| candidate == path)
                .unwrap()
                .1 = newer_source.to_owned();
            assert_eq!(
                AdmittedReplSession::from_loaded_project(
                    &projects[index],
                    older_program_with_newer_edge,
                    Limits::default(),
                )
                .unwrap_err()
                .code(),
                "ORNA-REPL-STANDARD",
                "older program must reject the newer {path} source"
            );

            let mut newer_program_with_older_edge = source_bundles[index + 1].clone();
            newer_program_with_older_edge
                .iter_mut()
                .find(|(candidate, _)| candidate == path)
                .unwrap()
                .1 = older_source.clone();
            assert_eq!(
                AdmittedReplSession::from_loaded_project(
                    &projects[index + 1],
                    newer_program_with_older_edge,
                    Limits::default(),
                )
                .unwrap_err()
                .code(),
                "ORNA-REPL-STANDARD",
                "newer program must reject the older {path} source"
            );
        }
    }

    assert_eq!(
        divergent_edges, 4,
        "each graph layer changes in one upgrade"
    );

    for index in [4, 2, 0, 3, 1, 4, 0, 1, 3, 2] {
        assert_eq!(
            sessions[index].submit(replay),
            Ok(Some(int(expected_results[index]))),
            "rejected source drift must not retarget replayed program {}",
            snapshots[index]
        );
    }
}

#[test]
fn dependency_graph_upgrade_programs_compute_values_from_each_pinned_snapshot() {
    let (_directory, projects, snapshots, source_bundles) = dependency_graph_projects();
    let expected_results = [1112, 2122, 2322, 2324, 23024];
    let use_graph = include_str!("fixtures/snapshot-dependency-graph-use.orna");
    let replay = include_str!("fixtures/snapshot-dependency-graph-call.orna");
    let mut retained_sessions = Vec::with_capacity(projects.len());

    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    for (upgrade, ((project, snapshot), (sources, expected))) in projects
        .iter()
        .zip(&snapshots)
        .zip(source_bundles.iter().zip(expected_results))
        .enumerate()
    {
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            snapshot,
            "upgrade {} must retain its captured standard dependency pin",
            upgrade + 1
        );
        let mut session =
            AdmittedReplSession::from_loaded_project(project, sources.clone(), Limits::default())
                .unwrap_or_else(|error| {
                    panic!(
                        "upgrade {} could not load its pinned program: {}",
                        upgrade + 1,
                        error.code()
                    )
                });
        assert_eq!(session.submit(use_graph), Ok(None));
        assert_eq!(
            session.submit(replay),
            Ok(Some(int(expected))),
            "upgrade {} must compute from its own dependency snapshot",
            upgrade + 1
        );
        retained_sessions.push(session);

        for retained_index in (0..retained_sessions.len()).rev() {
            assert_eq!(
                retained_sessions[retained_index].submit(replay),
                Ok(Some(int(expected_results[retained_index]))),
                "program pinned at upgrade {} must keep its value through upgrade {}",
                retained_index + 1,
                upgrade + 1
            );
        }
    }
}

#[test]
fn replay_results_are_stable_across_semantics_preserving_module_repins() {
    let (_directory, projects, snapshots, source_bundles) = module_chain_repin_projects();
    let leaf_sources = [
        include_str!("fixtures/module-chain-std-leaf-v1.orna"),
        include_str!("fixtures/module-chain-std-leaf-repin-v2.orna"),
        include_str!("fixtures/module-chain-std-leaf-repin-v3.orna"),
    ];
    let expected_body =
        "pub fn finish(value: Int): Int = std.math.shift(value + std.collection.marker() + 3);";
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    let mut retained_sessions = Vec::with_capacity(projects.len());

    for (((project, snapshot), sources), expected_leaf) in projects
        .iter()
        .zip(&snapshots)
        .zip(&source_bundles)
        .zip(leaf_sources)
    {
        assert_eq!(project.standard_profile().unwrap().snapshot(), snapshot);
        let pinned_leaf = sources
            .iter()
            .find(|(path, _)| path == "std/chain/leaf.orna")
            .unwrap()
            .1
            .as_str();
        assert_eq!(pinned_leaf, expected_leaf);
        assert!(pinned_leaf.lines().any(|line| line == expected_body));

        let mut session =
            AdmittedReplSession::from_loaded_project(project, sources.clone(), Limits::default())
                .unwrap();
        assert_eq!(
            session.submit(include_str!("fixtures/module-chain-repin-use.orna")),
            Ok(None)
        );
        let output = session
            .submit(include_str!("fixtures/module-chain-repin-call.orna"))
            .unwrap_or_else(|error| {
                panic!(
                    "snapshot {snapshot} failed to compute the stable replay value: {}",
                    error.code()
                )
            });
        assert_eq!(output, Some(int(20)));
        retained_sessions.push(session);
    }

    let replay = include_str!("fixtures/module-chain-repin-call.orna");
    for (index, session) in retained_sessions.iter_mut().enumerate() {
        assert_eq!(
            session.submit(replay),
            Ok(Some(int(20))),
            "retained session {index} must keep its behavior after later repins"
        );
    }

    let mut cloned_replays = retained_sessions.iter().cloned().collect::<Vec<_>>();
    for index in (0..cloned_replays.len()).rev() {
        assert_eq!(
            cloned_replays[index].submit(replay),
            Ok(Some(int(20))),
            "cloned session {index} must replay deterministically in reverse pin order"
        );
    }
}

#[test]
fn nested_module_imports_replay_from_each_captured_snapshot_pin() {
    let (_directory, _project_path, _parent_snapshots, projects, snapshots, source_bundles) =
        nested_module_pin_projects();
    let expected_values = [20, 24, 60];
    assert!(snapshots.windows(2).all(|pair| pair[0] != pair[1]));
    let mut retained_sessions = Vec::with_capacity(projects.len());

    for (((project, snapshot), sources), expected) in projects
        .iter()
        .zip(&snapshots)
        .zip(&source_bundles)
        .zip(expected_values)
    {
        assert_eq!(project.standard_profile().unwrap().snapshot(), snapshot);
        let mut session =
            AdmittedReplSession::from_loaded_project(project, sources.clone(), Limits::default())
                .unwrap();
        assert_eq!(
            session.submit(include_str!("fixtures/nested-module-pin-use.orna")),
            Ok(None)
        );
        let output = session
            .submit(include_str!("fixtures/nested-module-pin-call.orna"))
            .unwrap_or_else(|error| {
                panic!(
                    "snapshot {snapshot} failed to resolve its nested imports: {}",
                    error.code()
                )
            });
        assert_eq!(output, Some(int(expected)));
        retained_sessions.push(session);
    }
}

#[test]
fn committed_snapshot_rejects_divergent_dependency_pin_profiles() {
    let (_directory, project_path, parent_snapshots, projects, snapshots, source_bundles) =
        nested_module_pin_projects();
    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let expected_values = [20, 24, 60];

    for index in 0..projects.len() {
        let next = (index + 1) % projects.len();
        assert_ne!(snapshots[index], snapshots[next]);
        let committed_project = repository
            .resolve_snapshot(&parent_snapshots[index])
            .unwrap();
        assert_eq!(
            repository
                .committed_submodule_commit(&committed_project, "stdlib/std")
                .unwrap()
                .as_str(),
            snapshots[index]
        );

        let project = &projects[index];
        let mut session = AdmittedReplSession::from_loaded_project(
            project,
            source_bundles[index].clone(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            session.submit(include_str!("fixtures/nested-module-pin-use.orna")),
            Ok(None)
        );
        assert_eq!(
            session.submit(include_str!("fixtures/nested-module-pin-call.orna")),
            Ok(Some(int(expected_values[index]))),
            "pin {} must compute from its own nested dependency bodies",
            snapshots[index]
        );

        let divergent_profile = StandardDependencyProfile::from_sources(
            snapshots[next].clone(),
            source_bundles[next].clone(),
        )
        .unwrap();
        assert!(matches!(
            loader.load_committed_snapshot_with_standard_profile(
                &repository,
                &committed_project,
                Some(divergent_profile),
            ),
            Err(ProjectLoadError::StandardSnapshotMismatch)
        ));
    }
}
