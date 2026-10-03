use std::{fs, path::Path, process::Command};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_project_v1::{LoadedProject, ProjectLoader};
use orna_repository_v1::{Repository, initialize_repository};
use orna_semantic_v1::{Catalogue, StandardDependencyProfile};
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

fn configure_git_repo(directory: &Path) {
    for (key, value) in [
        ("user.email", "kieran@drewett.dev"),
        ("user.name", "kierandrewett"),
        ("commit.gpgsign", "false"),
    ] {
        git_output_at(directory, &["config", key, value]);
    }
}

fn commit_directory(directory: &Path, message: &str) {
    git_output_at(directory, &["add", "-A"]);
    git_output_at(directory, &["commit", "--quiet", "-m", message]);
}

fn strings(values: &[&str]) -> CanonicalValue {
    CanonicalValue::new(Raw::Array(
        values
            .iter()
            .map(|value| Raw::Text((*value).to_owned()))
            .collect(),
    ))
    .unwrap()
}

fn pinned_format_sources() -> Vec<(String, String)> {
    let all_sources = orna_standard::reference_standard_sources_v1();
    let pinned_profile = orna_standard::reference_standard_profile_v1();
    all_sources
        .into_iter()
        .filter(|(path, _)| matches!(path.as_str(), "std/format.orna" | "std/text.orna"))
        .map(|(path, source)| {
            pinned_profile
                .verify_source(&path, &source)
                .unwrap_or_else(|error| panic!("{path} is not from the pinned std DB: {error:?}"));
            (path, source)
        })
        .collect()
}

fn pinned_format_session() -> AdmittedReplSession {
    let sources = pinned_format_sources();
    let profile = StandardDependencyProfile::from_sources(
        "orna.std/format-contracts-wi489",
        sources.clone(),
    )
    .expect("the pinned format and text module bytes form a dependency profile");
    let catalogue = Catalogue::authoritative_core()
        .with_standard_sources(&profile, sources.clone())
        .expect("the pinned std.format and std.text modules resolve against core");
    AdmittedReplSession::from_catalogue(&[], catalogue, sources, Limits::default())
        .unwrap_or_else(|error| panic!("could not admit pinned formatter: {}", error.code()))
}

fn format_db_sources(marker_source: &str) -> Vec<(String, String)> {
    let mut sources = pinned_format_sources();
    sources.push((
        "std/main.orna".to_owned(),
        include_str!("fixtures/stdlib-format-db-main-wi489.orna").to_owned(),
    ));
    sources.push(("std/format_marker.orna".to_owned(), marker_source.to_owned()));
    sources
}

fn write_format_db_sources(standard_path: &Path, sources: &[(String, String)]) {
    for (logical_path, source) in sources {
        let path = logical_path.strip_prefix("std/").unwrap();
        let file_path = standard_path.join(path);
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(file_path, source).unwrap();
    }
}

fn capture_format_db_pin(project_path: &Path, standard_pin: &str, message: &str) -> String {
    git_output_at(project_path, &["add", "main.orna", ".gitmodules"]);
    let gitlink = format!("160000,{standard_pin},stdlib/std");
    git_output_at(
        project_path,
        &["update-index", "--add", "--cacheinfo", &gitlink],
    );
    git_output_at(project_path, &["commit", "--quiet", "-m", message]);
    git_output_at(project_path, &["rev-parse", "HEAD"])
}

fn pinned_format_db_projects() -> (
    TempDir,
    [LoadedProject; 3],
    [String; 3],
    [Vec<(String, String)>; 3],
) {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("project");
    fs::create_dir_all(&project_path).unwrap();
    fs::write(
        project_path.join("main.orna"),
        include_str!("fixtures/stdlib-format-db-project-wi489.orna"),
    )
    .unwrap();
    fs::write(
        project_path.join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/ornadb-std.git\n",
    )
    .unwrap();
    git_output_at(&project_path, &["init", "--quiet"]);
    configure_git_repo(&project_path);

    let standard_path = project_path.join("stdlib/std");
    fs::create_dir_all(&standard_path).unwrap();
    initialize_repository(&standard_path).unwrap();
    configure_git_repo(&standard_path);
    let sources_v1 =
        format_db_sources(include_str!("fixtures/stdlib-format-db-marker-v1-wi489.orna"));
    write_format_db_sources(&standard_path, &sources_v1);
    commit_directory(&standard_path, "capture pinned format module v1");
    let pin_v1 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v1 = capture_format_db_pin(&project_path, &pin_v1, "capture format DB v1");

    let sources_v2 =
        format_db_sources(include_str!("fixtures/stdlib-format-db-marker-v2-wi489.orna"));
    fs::write(
        standard_path.join("format_marker.orna"),
        sources_v2
            .iter()
            .find(|(path, _)| path == "std/format_marker.orna")
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    commit_directory(&standard_path, "capture pinned format module v2");
    let pin_v2 = git_output_at(&standard_path, &["rev-parse", "HEAD"]);
    let parent_v2 = capture_format_db_pin(&project_path, &pin_v2, "capture format DB v2");
    let parent_v1_restored = capture_format_db_pin(
        &project_path,
        &pin_v1,
        "restore captured format DB v1",
    );

    let repository = Repository::discover(&project_path).unwrap();
    let loader = ProjectLoader::default();
    let parent_snapshots = [&parent_v1, &parent_v2, &parent_v1_restored]
        .map(|parent| repository.resolve_snapshot(parent).unwrap());
    let expected_pins = [pin_v1.clone(), pin_v2, pin_v1];
    let sources = [sources_v1.clone(), sources_v2, sources_v1];
    let projects = parent_snapshots
        .iter()
        .zip(&expected_pins)
        .zip(&sources)
        .map(|((snapshot, pin), sources)| {
            let profile = StandardDependencyProfile::from_sources(pin.clone(), sources.clone())
                .expect("the selected formatter source snapshot forms a profile");
            loader
                .load_committed_snapshot_with_standard_profile(
                    &repository,
                    snapshot,
                    Some(profile),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let projects: [LoadedProject; 3] = projects.try_into().unwrap();
    for (index, (snapshot, project)) in parent_snapshots.iter().zip(&projects).enumerate() {
        assert_eq!(
            repository
                .committed_submodule_commit(snapshot, "stdlib/std")
                .unwrap()
                .as_str(),
            expected_pins[index],
            "format DB snapshot {index} must select the captured gitlink"
        );
        assert_eq!(
            project.standard_profile().unwrap().snapshot(),
            expected_pins[index],
            "format DB snapshot {index} must load its captured profile"
        );
    }
    assert_ne!(expected_pins[0], expected_pins[1]);
    assert_eq!(expected_pins[0], expected_pins[2]);
    (directory, projects, expected_pins, sources)
}

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(Raw::Text(value.to_owned())).unwrap()
}

#[test]
fn pinned_format_module_emits_minimal_integer_and_boolean_spellings() {
    let mut session = pinned_format_session();
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-format-contract-use-wi489.orna")),
        Ok(None)
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-format-contract-integers-wi489.orna")),
        Ok(Some(strings(&[
            "0",
            "1",
            "-1",
            "9",
            "10",
            "-10",
            "99",
            "100",
            "-100",
            "90071992547409931234567890",
            "-90071992547409931234567890",
        ])))
    );
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-format-contract-booleans-wi489.orna")),
        Ok(Some(strings(&["true", "false"])))
    );
}

#[test]
fn format_contract_outputs_follow_selected_and_restored_std_db_pins() {
    let (_directory, projects, pins, sources) = pinned_format_db_projects();
    let mut sessions = projects
        .iter()
        .zip(&sources)
        .map(|(project, sources)| {
            AdmittedReplSession::from_loaded_project(
                project,
                sources.clone(),
                Limits::default(),
            )
            .unwrap_or_else(|error| {
                panic!("could not admit captured format DB: {}", error.code())
            })
        })
        .collect::<Vec<_>>();
    let imports = [
        include_str!("fixtures/stdlib-format-contract-use-wi489.orna"),
        include_str!("fixtures/stdlib-format-contract-pinned-use-wi489.orna"),
    ];
    for session in &mut sessions {
        for source in imports {
            for import in source.lines() {
                assert_eq!(session.submit(import), Ok(None));
            }
        }
    }

    let markers = ["format-db-v1", "format-db-v2", "format-db-v1"];
    for (index, session) in sessions.iter_mut().enumerate() {
        assert_eq!(
            session.submit(include_str!("fixtures/stdlib-format-contract-marker-wi489.orna")),
            Ok(Some(text_value(markers[index]))),
            "captured format DB marker at pin {}",
            pins[index]
        );
        assert_eq!(
            session.submit(include_str!("fixtures/stdlib-format-contract-integers-wi489.orna")),
            Ok(Some(strings(&[
                "0",
                "1",
                "-1",
                "9",
                "10",
                "-10",
                "99",
                "100",
                "-100",
                "90071992547409931234567890",
                "-90071992547409931234567890",
            ]))),
            "integer display values at captured format DB pin {}",
            pins[index]
        );
        assert_eq!(
            session.submit(include_str!("fixtures/stdlib-format-contract-booleans-wi489.orna")),
            Ok(Some(strings(&["true", "false"]))),
            "Boolean display values at captured format DB pin {}",
            pins[index]
        );
    }

    for index in [2, 0, 1, 2, 1] {
        assert_eq!(
            sessions[index]
                .submit(include_str!("fixtures/stdlib-format-contract-marker-wi489.orna")),
            Ok(Some(text_value(markers[index]))),
            "replayed format DB marker at captured pin {}",
            pins[index]
        );
    }
}
