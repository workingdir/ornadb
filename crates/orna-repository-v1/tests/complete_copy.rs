//! Offline complete-copy export and reconstruction for one pinned snapshot
//! with a real pinned dependency.
//!
//! One superproject with a gitlink dependency is exported, the original
//! remotes are disabled, and the archive is reconstructed into a fresh
//! directory. The reconstruction is then materialised and read back, so this
//! proves the exported copy carries the snapshot closure, the dependency
//! closure, and the raw blob bytes without the source repository.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use orna_repository_v1::{
    complete_copy::{
        export_complete_copy, materialize_complete_copy, read_complete_copy_manifest,
        restore_complete_copy, CompleteCopyObjectKind, DependencySource,
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
    assert!(archive.join("superproject.bundle").is_file());

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
