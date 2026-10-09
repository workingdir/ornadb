//! Offline complete-copy export and reconstruction for pinned snapshots with a
//! real pinned dependency.
//!
//! One superproject with a gitlink dependency is exported, the original
//! remotes are disabled, and the archive is reconstructed into a fresh
//! directory. The reconstruction is then materialised and read back, so this
//! proves the exported copy carries the snapshot closure, the dependency
//! closure, and the raw blob bytes without the source repository.
//!
//! A second case exports two snapshots of one repository — the current commit
//! and the commit it replaced — into one archive, then reads both back offline
//! and checks every recorded blob's raw bytes with an extractor that shares no
//! code with the crate under test (`support/independent_extractor.rs`).

#[path = "support/independent_extractor.rs"]
mod extractor;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_repository_v1::{
    complete_copy::{
        export_complete_copy, export_complete_copy_of, materialize_complete_copy,
        read_complete_copy_manifest, restore_complete_copy, CompleteCopyObjectKind,
        DependencySource,
    },
    Repository,
};
use tempfile::TempDir;

const DATABASE: &str = include_str!("fixtures/format-context/database-final.orna");
const MAIN_SOURCE: &str = include_str!("fixtures/git-repository-main.orna");
const DEPENDENCY_SOURCE: &str = include_str!("fixtures/offline-complete-copy/dependency.orna");

fn git(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn init_repository(directory: &Path) {
    git(
        directory,
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    git(directory, &["config", "user.name", "kierandrewett"]);
    git(directory, &["config", "user.email", "kieran@drewett.dev"]);
    git(directory, &["config", "commit.gpgsign", "false"]);
}

/// A dependency repository with its own committed Orna content and store.
fn dependency_repository(root: &Path) -> PathBuf {
    let dependency = root.join("std-dependency");
    fs::create_dir_all(&dependency).expect("create dependency directory");
    init_repository(&dependency);
    fs::create_dir_all(dependency.join(".orna/store")).expect("create dependency store");
    fs::write(dependency.join(".orna/store/data"), "dependency store root")
        .expect("write dependency store root");
    fs::write(dependency.join("main.orna"), DEPENDENCY_SOURCE).expect("write dependency source");
    git(&dependency, &["add", "--all"]);
    git(
        &dependency,
        &["commit", "--quiet", "-m", "dependency content"],
    );
    dependency
}

/// A format-3 superproject that pins `dependency` at `deps/std`.
fn superproject(root: &Path, dependency: &Path) -> PathBuf {
    let project = root.join("library");
    fs::create_dir_all(&project).expect("create superproject directory");
    init_repository(&project);
    fs::create_dir_all(project.join(".orna/store")).expect("create store root");
    fs::write(project.join(".orna/store/data"), "store root").expect("write store root");
    fs::write(project.join(".orna/database.orna"), DATABASE).expect("write database metadata");
    fs::write(project.join("main.orna"), MAIN_SOURCE).expect("write source root");
    git(
        &project,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            dependency.to_str().expect("dependency path is UTF-8"),
            "deps/std",
        ],
    );
    git(&project, &["add", "--all"]);
    git(
        &project,
        &["commit", "--quiet", "-m", "library with dependency"],
    );
    project
}

#[test]
fn complete_copy_reconstructs_snapshot_and_dependency_without_the_source() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);

    let repository = Repository::discover(&project).expect("discover superproject");
    let snapshot = git_stdout(&project, &["rev-parse", "HEAD"]);
    let dependency_commit = git_stdout(&dependency, &["rev-parse", "HEAD"]);
    let payload = git_stdout(&project, &["rev-parse", "HEAD:main.orna"]);

    // The exported closure is the snapshot plus its pinned dependency, not a
    // pointer-only clone.
    let archive = root.path().join("archive");
    let manifest =
        export_complete_copy(&repository, "HEAD", &archive, &[]).expect("export complete copy");
    assert_eq!(manifest.snapshot, snapshot);
    assert!(
        manifest
            .objects
            .iter()
            .any(|object| object.kind == CompleteCopyObjectKind::Blob && object.oid == payload),
        "the closure records the pinned source blob: {:?}",
        manifest.objects
    );
    assert_eq!(manifest.dependencies.len(), 1);
    let recorded = &manifest.dependencies[0];
    assert_eq!(recorded.path, "deps/std");
    assert_eq!(recorded.commit, dependency_commit);
    assert_eq!(recorded.origin, dependency.to_string_lossy());
    assert!(archive.join(&recorded.bundle).is_file());
    assert!(archive.join(&manifest.members[0].bundle).is_file());
    assert_eq!(
        manifest.members[0].bundle,
        format!("snapshots/{snapshot}.bundle")
    );

    // Reading the manifest back without the source yields the same record.
    let reread = read_complete_copy_manifest(&archive).expect("read manifest");
    assert_eq!(reread, manifest);

    // Disable the original remotes: the recorded dependency origin no longer
    // resolves and the dependency worktree is gone.
    let disabled = root.path().join("std-dependency-unavailable");
    fs::rename(&dependency, &disabled).expect("move dependency away");

    let restored = root.path().join("restored");
    let reconstructed =
        restore_complete_copy(&archive, &restored).expect("reconstruct complete copy");
    assert_eq!(reconstructed, manifest);

    // The pinned commit and its bytes are present in the reconstruction.
    assert_eq!(git_stdout(&restored, &["rev-parse", "HEAD"]), snapshot);
    assert_eq!(
        git_stdout(&restored, &["rev-parse", "HEAD:main.orna"]),
        payload
    );
    assert_eq!(git_stdout(&restored, &["remote"]), "");

    let restored_dependency = restored.join("deps/std");
    assert_eq!(
        git_stdout(&restored_dependency, &["rev-parse", "HEAD"]),
        dependency_commit
    );
    assert_eq!(git_stdout(&restored_dependency, &["remote"]), "");

    // Materialising needs no source remote: code and rows come from the copy.
    materialize_complete_copy(&restored, &manifest).expect("materialise the copy");
    assert_eq!(
        fs::read_to_string(restored.join("main.orna")).expect("read restored source"),
        MAIN_SOURCE
    );
    assert_eq!(
        fs::read_to_string(restored_dependency.join("main.orna"))
            .expect("read restored dependency source"),
        DEPENDENCY_SOURCE
    );

    // A caller may also name the dependency source explicitly.
    let explicit = root.path().join("explicit-archive");
    let sources = [DependencySource {
        path: "deps/std".to_owned(),
        directory: disabled.clone(),
    }];
    let explicit_manifest = export_complete_copy(&repository, "HEAD", &explicit, &sources)
        .expect("export with an explicit dependency source");
    assert_eq!(explicit_manifest, manifest);
}

#[test]
fn complete_copy_fails_closed_when_a_dependency_is_unavailable() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let repository = Repository::discover(&project).expect("discover superproject");

    // An unpopulated dependency: neither the worktree path nor the module
    // object store that a populated checkout keeps holds its pinned commit.
    fs::remove_dir_all(project.join("deps/std")).expect("remove the dependency checkout");
    fs::remove_dir_all(project.join(".git/modules/deps/std")).expect("remove the module objects");
    fs::remove_dir_all(&dependency).expect("remove the dependency repository");

    let archive = root.path().join("archive");
    let error = export_complete_copy(&repository, "HEAD", &archive, &[]).expect_err("export fails");
    assert!(
        format!("{error}").contains("deps/std"),
        "the failure names the unavailable dependency: {error}"
    );
    // Nothing claims to be a complete copy.
    assert!(!archive.join("manifest.tsv").exists());
}

/// A third committed snapshot: the same schema source with one new row file, so
/// the second snapshot has its own distinct closure and its own blob.
const MAIN_SOURCE_UPDATED: &str = "module main;\n// remastered\n";

#[test]
fn complete_copy_reconstructs_both_snapshots_offline_with_an_independent_extractor() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let repository = Repository::discover(&project).expect("discover superproject");

    let previous = git_stdout(&project, &["rev-parse", "HEAD"]);
    let previous_payload = git_stdout(&project, &["rev-parse", "HEAD:main.orna"]);
    // A second commit that replaces the source blob, so both members carry a
    // different closure and the archive must hold both.
    fs::write(project.join("main.orna"), MAIN_SOURCE_UPDATED).expect("write updated source");
    git(&project, &["add", "--all"]);
    git(&project, &["commit", "--quiet", "-m", "remaster the library"]);
    let current = git_stdout(&project, &["rev-parse", "HEAD"]);
    let current_payload = git_stdout(&project, &["rev-parse", "HEAD:main.orna"]);
    assert_ne!(previous, current);
    assert_ne!(previous_payload, current_payload);

    // Export both snapshots into one archive: current first, then the commit it
    // replaced.
    let archive = root.path().join("both");
    let selectors = ["HEAD", previous.as_str()];
    let manifest = export_complete_copy_of(&repository, &selectors, &archive, &[])
        .expect("export both snapshots");
    assert_eq!(manifest.snapshot, current);
    assert_eq!(
        manifest.commits().collect::<Vec<_>>(),
        vec![current.as_str(), previous.as_str()]
    );
    // Each member records its own source blob, and the archive records both.
    let current_member = manifest.member(&current).expect("current member");
    let previous_member = manifest.member(&previous).expect("previous member");
    for (member, payload) in [
        (current_member, &current_payload),
        (previous_member, &previous_payload),
    ] {
        assert!(
            member
                .objects
                .iter()
                .any(|object| object.kind == CompleteCopyObjectKind::Blob && object.oid == *payload),
            "{} records its own source blob",
            member.commit
        );
    }
    assert_ne!(current_member.bundle, previous_member.bundle);

    // The extractor reads the archive itself: same members, same closures.
    let recorded = extractor::read_manifest(&archive);
    assert_eq!(recorded.snapshot, current);
    assert_eq!(recorded.members.len(), 2);
    assert_eq!(recorded.members[0].commit, current);
    assert_eq!(recorded.members[1].commit, previous);
    // The dependency is carried once and pinned by the member that names it.
    assert_eq!(recorded.dependencies.len(), 1);
    assert_eq!(recorded.dependencies[0].0, "deps/std");

    // Disable the original remote entirely: only the archive remains.
    fs::rename(&dependency, root.path().join("dependency-disabled")).expect("disable dependency");
    fs::remove_dir_all(&project).expect("remove the source repository");

    // Each member is extractable from its own bundle alone.
    for member in &recorded.members {
        let scratch = extractor::scratch(&member.commit[..8]);
        extractor::extract_member(&archive, member, &scratch);
        let blobs = extractor::verify_blobs(&scratch, member);
        assert_eq!(
            blobs,
            member
                .objects
                .iter()
                .filter(|object| object.kind == "blob")
                .count()
        );
        fs::remove_dir_all(&scratch).expect("clean scratch");
    }

    let restored = root.path().join("restored");
    let reconstructed = restore_complete_copy(&archive, &restored).expect("reconstruct the copy");
    assert_eq!(reconstructed, manifest);
    assert_eq!(git_stdout(&restored, &["rev-parse", "HEAD"]), current);
    assert_eq!(git_stdout(&restored, &["remote"]), "");
    // Both snapshots are readable from the one reconstruction.
    assert_eq!(
        extractor::read_path(&restored, &current, "main.orna"),
        MAIN_SOURCE_UPDATED.as_bytes()
    );
    assert_eq!(
        extractor::read_path(&restored, &previous, "main.orna"),
        MAIN_SOURCE.as_bytes()
    );
    let paths = extractor::tree_paths(&restored, &previous);
    assert!(paths.iter().any(|(path, _)| path == "main.orna"));

    // Independent blob-hash verification over every member of the copy, driven
    // by the extractor's own reading of the archive.
    for member in &recorded.members {
        assert!(extractor::verify_blobs(&restored, member) > 0);
    }

    // The primary member is materialised by --worktree, with its dependency.
    materialize_complete_copy(&restored, &reconstructed).expect("materialise the copy");
    assert_eq!(
        fs::read_to_string(restored.join("main.orna")).expect("read materialised source"),
        MAIN_SOURCE_UPDATED
    );
    assert_eq!(
        fs::read_to_string(restored.join("deps/std/main.orna")).expect("read dependency source"),
        DEPENDENCY_SOURCE
    );
}

#[test]
fn complete_copy_exports_one_snapshot_at_a_time_and_refuses_a_repeat() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let repository = Repository::discover(&project).expect("discover superproject");

    // The single-selector spelling and the repeated spelling agree.
    let single = root.path().join("single");
    let one = export_complete_copy(&repository, "HEAD", &single, &[]).expect("export one snapshot");
    assert_eq!(one.members.len(), 1);

    let repeated = root.path().join("repeated");
    let error = export_complete_copy_of(&repository, &["HEAD", "HEAD"], &repeated, &[])
        .expect_err("a repeated selector is refused");
    assert!(
        format!("{error}").contains("repeated snapshot"),
        "the failure names the repeated member: {error}"
    );
    assert!(!repeated.join("manifest.tsv").exists());

    let none = root.path().join("none");
    assert!(export_complete_copy_of(&repository, &[], &none, &[]).is_err());
}
