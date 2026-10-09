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

#[test]
fn complete_copy_refuses_a_dependency_that_does_not_reach_its_recorded_closure() {
    let root = TempDir::new().expect("create fixture root");
    let dependency = dependency_repository(root.path());
    let project = superproject(root.path(), &dependency);
    let repository = Repository::discover(&project).expect("discover superproject");
    let dependency_commit = git_stdout(&dependency, &["rev-parse", "HEAD"]);
    let dependency_payload = git_stdout(&dependency, &["rev-parse", "HEAD:main.orna"]);

    let archive = root.path().join("archive");
    let manifest =
        export_complete_copy(&repository, "HEAD", &archive, &[]).expect("export the complete copy");
    let recorded = manifest.dependencies[0].clone();
    assert_eq!(recorded.path, "deps/std");
    assert!(
        recorded
            .objects
            .iter()
            .any(|object| object.kind == CompleteCopyObjectKind::Blob
                && object.oid == dependency_payload),
        "the dependency closure records the pinned payload blob: {:?}",
        recorded.objects
    );

    // Disable the source repository: only the archive remains.
    fs::rename(&dependency, root.path().join("dependency-disabled")).expect("disable dependency");

    // The archive alone reconstructs, and the dependency keeps its payload.
    let intact = root.path().join("copy-intact");
    restore_complete_copy(&archive, &intact).expect("the recorded copy reconstructs");
    assert_eq!(
        git_stdout(&intact.join("deps/std"), &["rev-parse", "HEAD:main.orna"]),
        dependency_payload,
        "the dependency's payload blob is readable from the copy alone"
    );

    // Drop the payload blob from the dependency's recorded closure and lower
    // the count with it, so the manifest stays internally coherent: the only
    // thing wrong with it is that the recorded closure no longer describes the
    // bytes the dependency reaches.
    let document = fs::read_to_string(archive.join("manifest.tsv")).expect("read the manifest");
    let dropped = format!("object\tblob\t{dependency_payload}\t");
    let mut truncated_lines: Vec<String> = Vec::new();
    let mut dropped_once = false;
    let mut lowered_once = false;
    for line in document.lines() {
        if line.starts_with("dependency\t") {
            let count = line
                .rsplit('\t')
                .next()
                .expect("the count column")
                .parse::<usize>()
                .expect("the count is a number");
            let lowered = line
                .rsplit_once('\t')
                .expect("the count is the last column")
                .0
                .to_owned();
            truncated_lines.push(format!("{lowered}\t{}", count - 1));
            lowered_once = true;
            continue;
        }
        if !dropped_once && line.starts_with(&dropped) {
            dropped_once = true;
            continue;
        }
        truncated_lines.push(line.to_owned());
    }
    assert!(dropped_once, "the dependency records its payload blob");
    assert!(lowered_once, "the dependency line carries a count");
    let mut truncated_document = truncated_lines.join("\n");
    truncated_document.push('\n');
    assert_ne!(truncated_document, document, "the manifest changed");
    assert!(
        truncated_document.contains("dependency\tdeps/std\t"),
        "the dependency line survives"
    );

    let truncated = root.path().join("truncated");
    copy_archive(&archive, &truncated);
    fs::write(truncated.join("manifest.tsv"), &truncated_document).expect("write the manifest");
    let error = restore_complete_copy(&truncated, &root.path().join("copy-truncated"))
        .expect_err("a dependency that reaches more than the recording is refused");
    assert!(
        matches!(
            error,
            orna_repository_v1::complete_copy::CompleteCopyError::IntegrityMismatch { .. }
        ),
        "the failure is a typed integrity diagnostic: {error}"
    );
    assert!(
        format!("{error}").contains("deps/std"),
        "the failure names the dependency: {error}"
    );
    assert!(
        !root
            .path()
            .join("copy-truncated/deps/std/main.orna")
            .exists(),
        "a refused dependency is not materialised: {error}"
    );
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
    // The dependency is carried once and pinned by the member that names it,
    // and its closure is recorded rather than left to whichever objects its
    // bundle happens to reach.
    assert_eq!(recorded.dependencies.len(), 1);
    assert_eq!(recorded.dependencies[0].path, "deps/std");
    assert!(
        !recorded.dependencies[0].objects.is_empty(),
        "the archive records the dependency closure"
    );
    for dependency in &recorded.dependencies {
        let scratch = extractor::scratch(&format!("dep-{}", &dependency.commit[..8]));
        extractor::extract_commit(&archive, &dependency.bundle, &dependency.commit, &scratch);
        // The recorded closure is exactly what the dependency's bundle reaches,
        // and it is read back from that bundle alone: payload blobs included,
        // so a descriptor-only copy fails here.
        let reached = extractor::closure_objects(&scratch, &dependency.commit);
        let mut recorded_objects = dependency.objects.clone();
        recorded_objects.sort_by(|left, right| left.oid.cmp(&right.oid));
        assert_eq!(
            reached, recorded_objects,
            "dependency {} reaches exactly its recorded closure",
            dependency.path
        );
        // Re-hashing the payload bytes proves the objects are carried, not just
        // named: a closure of descriptor trees alone verifies here and fails.
        assert!(
            extractor::verify_recorded_blobs(&scratch, &dependency.objects) > 0,
            "dependency {} carries its payload blobs",
            dependency.path
        );
        fs::remove_dir_all(&scratch).expect("clean scratch");
    }

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
    let error = restore_complete_copy(&truncated, &root.path().join("copy-truncated"))
        .expect_err("a truncated recording is refused");
    assert!(
        matches!(
            error,
            orna_repository_v1::complete_copy::CompleteCopyError::IntegrityMismatch { .. }
        ),
        "the failure is a typed integrity diagnostic: {error}"
    );
    // The destination was created by the fetch and then refused: a partial
    // object database may remain, but no snapshot tree may be materialised.
    let refused = root.path().join("copy-truncated");
    assert!(
        !refused.join("main.orna").exists(),
        "a refused reconstruction writes no snapshot tree: {error}"
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
