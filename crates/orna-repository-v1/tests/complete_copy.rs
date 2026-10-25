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
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use orna_repository_v1::{
    Repository,
    complete_copy::{
        CompleteCopyObjectKind, DependencySource, export_complete_copy, export_complete_copy_of,
        materialize_complete_copy, read_complete_copy_manifest, restore_complete_copy,
    },
};
use sha2::{Digest, Sha256};
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

/// Copies an archive tree, including the per-member bundle directories.
fn copy_archive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create archive copy");
    for entry in fs::read_dir(source).expect("read archive") {
        let entry = entry.expect("archive entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("archive entry type").is_dir() {
            copy_archive(&entry.path(), &target);
        } else if entry.file_name() != std::ffi::OsStr::new("manifest.tsv") {
            fs::copy(entry.path(), &target).expect("copy archive member");
        }
    }
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

/// A dependency repository that pins a second repository in its own tree.
fn nested_dependency_repositories(root: &Path) -> (PathBuf, PathBuf) {
    let leaf = root.join("leaf-dependency");
    fs::create_dir_all(&leaf).expect("create nested dependency");
    init_repository(&leaf);
    fs::write(leaf.join("payload.bin"), b"nested dependency payload\n")
        .expect("write nested dependency payload");
    git(&leaf, &["add", "--all"]);
    git(
        &leaf,
        &["commit", "--quiet", "-m", "nested dependency content"],
    );

    let dependency = dependency_repository(root);
    git(
        &dependency,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            leaf.to_str().expect("nested dependency path is UTF-8"),
            "tools/leaf",
        ],
    );
    git(&dependency, &["add", "--all"]);
    git(
        &dependency,
        &["commit", "--quiet", "-m", "pin nested dependency"],
    );
    git(
        &dependency,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--recursive",
        ],
    );
    (dependency, leaf)
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
fn complete_copy_carries_nested_pins_and_restores_them_offline() {
    let root = TempDir::new().expect("create fixture root");
    let (dependency, leaf) = nested_dependency_repositories(root.path());
    let project = superproject(root.path(), &dependency);
    git(
        &project,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--recursive",
        ],
    );

    let repository = Repository::discover(&project).expect("discover superproject");
    let snapshot = git_stdout(&project, &["rev-parse", "HEAD"]);
    let dependency_commit = git_stdout(&dependency, &["rev-parse", "HEAD"]);
    let nested_commit = git_stdout(&leaf, &["rev-parse", "HEAD"]);
    let archive = root.path().join("nested-archive");
    let manifest = export_complete_copy(&repository, "HEAD", &archive, &[])
        .expect("export recursive dependency closure");

    assert_eq!(manifest.snapshot, snapshot);
    assert_eq!(manifest.dependencies.len(), 2);
    assert_eq!(manifest.dependencies[0].path, "deps/std");
    assert_eq!(manifest.dependencies[0].commit, dependency_commit);
    assert_eq!(manifest.dependencies[1].path, "deps/std/tools/leaf");
    assert_eq!(manifest.dependencies[1].commit, nested_commit);
    for dependency in &manifest.dependencies {
        assert!(archive.join(&dependency.bundle).is_file());
    }
    let explicit_archive = root.path().join("nested-explicit-archive");
    let sources = [DependencySource {
        path: "deps/std".to_owned(),
        directory: dependency.clone(),
    }];
    let explicit_manifest = export_complete_copy(&repository, "HEAD", &explicit_archive, &sources)
        .expect("export nested dependencies from the explicit parent source");
    assert_eq!(explicit_manifest, manifest);

    drop(repository);
    fs::remove_dir_all(&project).expect("remove source superproject");
    fs::remove_dir_all(&dependency).expect("remove first dependency source");
    fs::remove_dir_all(&leaf).expect("remove nested dependency source");
    let restored = root.path().join("restored-nested-copy");
    let reconstructed =
        restore_complete_copy(&archive, &restored).expect("restore only from archived bundles");
    assert_eq!(reconstructed, manifest);

    let restored_dependency = restored.join("deps/std");
    let restored_nested = restored.join("deps/std/tools/leaf");
    assert_eq!(
        git_stdout(&restored_dependency, &["rev-parse", "HEAD"]),
        dependency_commit
    );
    assert_eq!(
        git_stdout(&restored_nested, &["rev-parse", "HEAD"]),
        nested_commit
    );
    assert_eq!(git_stdout(&restored_dependency, &["remote"]), "");
    assert_eq!(git_stdout(&restored_nested, &["remote"]), "");

    materialize_complete_copy(&restored, &manifest).expect("materialise all pinned worktrees");
    assert_eq!(
        fs::read(restored_nested.join("payload.bin"))
            .expect("read nested payload from restored worktree"),
        b"nested dependency payload\n"
    );
}

/// HIGH-1 regression at the real entry point: the process working directory is
/// the parent of a relative dependency directory. Run in a child process so the
/// process-global working directory cannot race sibling tests.
#[test]
fn complete_copy_relative_dependency_does_not_fall_through_to_the_parent() {
    const CHILD: &str = "ORNA_COMPLETE_COPY_RELATIVE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = PathBuf::from(std::env::var_os(CHILD).expect("child root"));
        let project = root.join("library");
        let dependency = root.join("std-dependency");
        let archive = root.join("relative-archive");
        let repository = Repository::discover(&project).expect("discover superproject");
        let sources = [DependencySource {
            path: "deps/std".to_owned(),
            directory: PathBuf::from("std-dependency"),
        }];
        std::env::set_current_dir(&root).expect("enter parent directory");
        let manifest = export_complete_copy(&repository, "HEAD", &archive, &sources)
            .expect("export with a relative dependency directory");
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].path, "deps/std");
        assert_eq!(
            manifest.dependencies[0].commit,
            git_stdout(&dependency, &["rev-parse", "HEAD"])
        );
        return;
    }

    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    git(
        &project,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
            "--recursive",
        ],
    );
    let status = Command::new(std::env::current_exe().expect("test binary"))
        .arg("complete_copy_relative_dependency_does_not_fall_through_to_the_parent")
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD, root.path())
        .status()
        .expect("run child export");
    assert!(status.success(), "relative dependency export failed: {status}");
}

/// Relative-path fall-through at the real entry point: the working directory is a
/// healthy repository and the relative dependency directory is a child whose
/// `HEAD` is corrupt. Discovery must fail closed rather than adopt the parent.
#[test]
fn complete_copy_relative_corrupt_dependency_fails_closed_under_healthy_parent() {
    const CHILD: &str = "ORNA_COMPLETE_COPY_CORRUPT_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = PathBuf::from(std::env::var_os(CHILD).expect("child root"));
        let project = root.join("library");
        let archive = root.join("corrupt-archive");
        let repository = Repository::discover(&project).expect("discover superproject");
        let sources = [DependencySource {
            path: "deps/std".to_owned(),
            directory: PathBuf::from("std-dependency"),
        }];
        std::env::set_current_dir(&root).expect("enter healthy parent");
        let result = export_complete_copy(&repository, "HEAD", &archive, &sources);
        assert!(result.is_err(), "corrupt relative child must not adopt parent");
        return;
    }

    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    git(&project, &["-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive"]);
    git(root.path(), &["init", "-q"]);
    fs::write(dependency.join(".git").join("HEAD"), b"garbage\n").expect("corrupt child HEAD");
    let status = Command::new(std::env::current_exe().expect("test binary"))
        .arg("complete_copy_relative_corrupt_dependency_fails_closed_under_healthy_parent")
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD, root.path())
        .status()
        .expect("run child export");
    assert!(status.success(), "corrupt relative dependency export did not fail closed: {status}");
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
    git(
        &project,
        &["commit", "--quiet", "-m", "remaster the library"],
    );
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

#[test]
fn complete_copy_verifies_every_object_id_and_refuses_a_truncated_or_substituted_closure() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let repository = Repository::discover(&project).expect("discover superproject");

    let previous = git_stdout(&project, &["rev-parse", "HEAD"]);
    fs::write(project.join("main.orna"), MAIN_SOURCE_UPDATED).expect("write updated source");
    git(&project, &["add", "--all"]);
    git(
        &project,
        &["commit", "--quiet", "-m", "remaster the library"],
    );
    let current = git_stdout(&project, &["rev-parse", "HEAD"]);

    let archive = root.path().join("archive");
    let selectors = ["HEAD", previous.as_str()];
    let manifest = export_complete_copy_of(&repository, &selectors, &archive, &[])
        .expect("export both snapshots");
    let member = manifest.member(&current).expect("current member");
    assert!(
        member.objects.len() > 1,
        "the current member carries more than the pinned commit: {:?}",
        member.objects
    );

    // Both snapshots and their dependency must survive a manifest that was
    // edited rather than a copy that lost objects: the case a truncated or
    // substituted archive presents.
    fs::rename(&dependency, root.path().join("dependency-disabled")).expect("disable dependency");

    let document =
        fs::read_to_string(archive.join("manifest.tsv")).expect("read the archive manifest");
    let (member_block, other_records): (String, usize) = {
        let mut block = String::new();
        let mut others = 0;
        let mut inside = false;
        for line in document.lines() {
            if let Some(rest) = line.strip_prefix("archive\t") {
                inside = rest.split('\t').next() == Some(current.as_str());
            }
            if line.starts_with("object\t") {
                if inside {
                    block.push_str(line);
                    block.push('\n');
                } else {
                    others += 1;
                }
            }
        }
        (block, others)
    };
    assert!(
        !member_block.is_empty() && other_records > 0,
        "each member records its own closure: {other_records} elsewhere"
    );

    // 1. Truncated: the member's object records are dropped but for one, so the
    // commit survives and the recorded closure is non-empty yet incomplete.
    let truncated = root.path().join("truncated");
    copy_archive(&archive, &truncated);
    let retained = member_block
        .lines()
        .next()
        .expect("the member records at least its own commit")
        .to_owned();
    let truncated_document = document.replace(&member_block, &format!("{retained}\n"));
    assert_ne!(truncated_document, document, "the manifest changed");
    assert!(
        truncated_document
            .lines()
            .filter(|line| line.starts_with("object\t"))
            .count()
            < document
                .lines()
                .filter(|line| line.starts_with("object\t"))
                .count(),
        "the recorded closure shrank"
    );
    fs::write(truncated.join("manifest.tsv"), &truncated_document).expect("write manifest");
    let refused = root.path().join("copy-truncated");
    fs::create_dir(&refused).expect("make the empty restore destination");
    let error =
        restore_complete_copy(&truncated, &refused).expect_err("a truncated recording is refused");
    assert!(
        matches!(
            error,
            orna_repository_v1::complete_copy::CompleteCopyError::IntegrityMismatch { .. }
        ),
        "the failure is a typed integrity diagnostic: {error}"
    );
    assert!(
        fs::read_dir(&refused)
            .expect("the original destination remains readable")
            .next()
            .is_none(),
        "a refused reconstruction preserves the existing empty destination: {error}"
    );

    // 2. Substituted: re-point one object record at another real object of the
    // archive, keeping the record count and every field valid.
    let updated_blob = git_stdout(&project, &["rev-parse", "HEAD:main.orna"]);
    let previous_blob = git_stdout(&project, &["rev-parse", &format!("{previous}:main.orna")]);
    assert_ne!(updated_blob, previous_blob);
    let lines: Vec<&str> = document.lines().collect();
    let previous_size = lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("object\tblob\t{previous_blob}\t")))
        .expect("the archive records the previous payload blob");
    let mut substituted_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut substituted_once = false;
    for line in &lines {
        if !substituted_once && line.starts_with(&format!("object\tblob\t{updated_blob}\t")) {
            substituted_lines.push(format!("object\tblob\t{previous_blob}\t{previous_size}"));
            substituted_once = true;
        } else {
            substituted_lines.push((*line).to_owned());
        }
    }
    assert!(
        substituted_once,
        "the archive records the updated payload blob"
    );
    let mut substituted_document = substituted_lines.join("\n");
    substituted_document.push('\n');
    assert_eq!(
        substituted_document
            .lines()
            .filter(|line| line.starts_with("object\t"))
            .count(),
        member_block.lines().count() + other_records,
        "the substituted manifest keeps every object record"
    );
    let substituted = root.path().join("substituted");
    copy_archive(&archive, &substituted);
    fs::write(substituted.join("manifest.tsv"), &substituted_document).expect("write manifest");
    let error = restore_complete_copy(&substituted, &root.path().join("copy-substituted"))
        .expect_err("a substituted object record is refused");
    assert!(
        matches!(
            error,
            orna_repository_v1::complete_copy::CompleteCopyError::IntegrityMismatch { .. }
        ),
        "the failure is a typed integrity diagnostic: {error}"
    );
}

#[test]
fn complete_copy_reconstructs_large_blob_after_source_and_origin_are_gone() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let unavailable_origin = root.path().join("unavailable-origin");
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            unavailable_origin.to_str().expect("origin path is UTF-8"),
        ],
    );

    let payload_length = 8 * 1024 * 1024;
    let payload: Vec<u8> = (0..payload_length)
        .map(|index| ((index * 31 + (index >> 8)) & 0xff) as u8)
        .collect();
    let expected_sha256: [u8; 32] = Sha256::digest(&payload).into();
    fs::write(project.join("large-payload.bin"), &payload).expect("write large payload");
    git(&project, &["add", "large-payload.bin"]);
    git(
        &project,
        &["commit", "--quiet", "-m", "large offline payload"],
    );
    let snapshot = git_stdout(&project, &["rev-parse", "HEAD"]);
    let source_oid = git_stdout(&project, &["rev-parse", "HEAD:large-payload.bin"]);
    let repository = Repository::discover(&project).expect("discover source repository");
    let archive = root.path().join("large-archive");
    let manifest = export_complete_copy(&repository, "HEAD", &archive, &[])
        .expect("export while the configured remote is unavailable");
    assert_eq!(manifest.snapshot, snapshot);
    assert_eq!(
        git_stdout(&project, &["remote", "get-url", "origin"]),
        unavailable_origin.to_string_lossy().into_owned()
    );
    assert!(
        !unavailable_origin.exists(),
        "the source remote is unavailable"
    );

    drop(repository);
    fs::remove_dir_all(&project).expect("remove the source repository");
    fs::remove_dir_all(&dependency).expect("remove the dependency source");
    let restored = root.path().join("restored-large-copy");
    fs::create_dir(&restored).expect("create existing empty destination");
    let reconstructed = restore_complete_copy(&archive, &restored)
        .expect("restore and verify from the archive alone");
    assert_eq!(reconstructed, manifest);
    assert_eq!(git_stdout(&restored, &["remote"]), "");
    assert_eq!(
        git_stdout(
            &restored,
            &["rev-parse", &format!("{snapshot}:large-payload.bin")]
        ),
        source_oid,
        "the restored snapshot preserves the large blob object"
    );
    materialize_complete_copy(&restored, &manifest).expect("materialise verified snapshot");

    let mut payload_file = fs::File::open(restored.join("large-payload.bin"))
        .expect("open materialised large payload");
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut restored_length = 0_u64;
    loop {
        let count = payload_file
            .read(&mut buffer)
            .expect("stream restored payload");
        if count == 0 {
            break;
        }
        restored_length += count as u64;
        hasher.update(&buffer[..count]);
    }
    assert_eq!(restored_length, payload_length as u64);
    let restored_sha256: [u8; 32] = hasher.finalize().into();
    assert_eq!(restored_sha256, expected_sha256, "restored raw blob hash");
}

#[test]
fn complete_copy_refuses_a_dependency_path_that_names_git_metadata() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let repository = Repository::discover(&project).expect("discover superproject");
    let archive = root.path().join("archive");
    export_complete_copy(&repository, "HEAD", &archive, &[]).expect("export complete copy");
    let document = fs::read_to_string(archive.join("manifest.tsv")).expect("read manifest");

    // Restore fetches each dependency into its path inside the copy and
    // materialising checks it out there, so a `.git` path would write the
    // dependency's files, hooks included, into a Git directory.
    for (index, hostile) in [".git", "deps/.git", "deps/.GIT", "deps/git~1"]
        .into_iter()
        .enumerate()
    {
        let tampered = root.path().join(format!("tampered-{index}"));
        copy_archive(&archive, &tampered);
        let tampered_document =
            document.replace("dependency\tdeps/std\t", &format!("dependency\t{hostile}\t"));
        assert_ne!(tampered_document, document, "the dependency path changed");
        fs::write(tampered.join("manifest.tsv"), &tampered_document).expect("write manifest");
        let destination = root.path().join(format!("copy-{index}"));
        let error = restore_complete_copy(&tampered, &destination)
            .expect_err("a dependency path naming git metadata is refused");
        assert!(
            matches!(
                error,
                orna_repository_v1::complete_copy::CompleteCopyError::InvalidArchive(
                    "dependency line"
                )
            ),
            "{hostile:?} is refused while the manifest is read: {error}"
        );
        assert!(!destination.exists(), "nothing is restored for {hostile:?}");
    }
}
