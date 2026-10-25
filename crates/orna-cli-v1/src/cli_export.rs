//! `orna export DEST --at SELECTOR [--dependency PATH=DIR]... [--check]`:
//! freezes, verifies, inspects, and reconstructs one complete offline copy.
//!
//! Three modes share one verb so a copy is always described by one archive
//! format:
//!
//! - `orna export DEST --at SELECTOR` writes an archive of the selected
//!   snapshot and every recursively pinned dependency, reading each nested
//!   repository at the commit recorded by its parent. `--dependency PATH=DIR`
//!   supplies a local dependency repository and may also supply its populated
//!   descendants. Exporting never moves the user's branch or contacts a remote
//!   (ORNA-GIT-003, ORNA-GIT-007, ORNA-GIT-008).
//! - `orna export ARCHIVE --check` reads an archive back and reports exactly
//!   what it records, without a repository.
//! - `orna export ARCHIVE --restore DEST [--at COMMIT] [--worktree]`
//!   reconstructs the archive, optionally selecting one recorded commit,
//!   into a directory that holds no object of the source, verifies every
//!   recorded object and every blob's bytes with this build's own extractor,
//!   and optionally materialises the pinned snapshot and its dependencies as a
//!   usable worktree. No remote is configured and no source path is consulted.

use std::path::{Path, PathBuf};

use orna_repository_v1::Repository;
use orna_repository_v1::complete_copy::{
    CompleteCopyError, CompleteCopyManifest, DependencySource, export_complete_copy_of,
    materialize_complete_copy, read_complete_copy_manifest, restore_complete_copy,
    restore_complete_copy_at,
};

use super::Diagnostic;

const USAGE: &str = "usage:\n  \
     orna export <DEST> --at <SELECTOR> [--at <SELECTOR>]... [--dependency PATH=DIR]...\n  \
     orna export <ARCHIVE> --check\n  \
     orna export <ARCHIVE> --restore <DEST> [--at <COMMIT>] [--worktree]";

/// What one invocation of the export verb was asked to do.
#[derive(Debug, Eq, PartialEq)]
enum Mode {
    /// Freeze one resolved snapshot into a new archive.
    Export {
        destination: PathBuf,
        selectors: Vec<String>,
        dependencies: Vec<DependencySource>,
    },
    /// Report what an existing archive records.
    Inspect { archive: PathBuf },
    /// Reconstruct and verify an archive into a new directory.
    Restore {
        archive: PathBuf,
        destination: PathBuf,
        snapshot: Option<String>,
        worktree: bool,
    },
}

fn export_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna export DEST --at SELECTOR` inside an admitted repository",
        detail.into(),
    )
}

fn parse_options(arguments: &[String]) -> Result<Mode, Diagnostic> {
    let mut positional = Vec::new();
    let mut selectors: Vec<String> = Vec::new();
    let mut dependencies = Vec::new();
    let mut check = false;
    let mut restore = None;
    let mut worktree = false;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--at" => {
                let value = words
                    .next()
                    .ok_or_else(|| export_error("--at needs a value", USAGE))?;
                if value.is_empty() {
                    return Err(export_error("--at needs a value", USAGE));
                }
                selectors.push(value.to_owned());
            }
            "--dependency" => {
                let value = words
                    .next()
                    .ok_or_else(|| export_error("--dependency needs a value", USAGE))?;
                let (path, directory) = value.split_once('=').ok_or_else(|| {
                    export_error("--dependency is not PATH=DIR", format!("got {value:?}"))
                })?;
                if path.is_empty() || directory.is_empty() {
                    return Err(export_error(
                        "--dependency is not PATH=DIR",
                        format!("got {value:?}"),
                    ));
                }
                dependencies.push(DependencySource {
                    path: path.to_owned(),
                    directory: PathBuf::from(directory),
                });
            }
            "--check" => check = true,
            "--restore" => {
                let value = words
                    .next()
                    .ok_or_else(|| export_error("--restore needs a destination", USAGE))?;
                if restore.is_some() {
                    return Err(export_error("Export restores one archive", USAGE));
                }
                restore = Some(PathBuf::from(value));
            }
            "--worktree" => worktree = true,
            flag if flag.starts_with("--") => {
                return Err(export_error(
                    "Unknown export flag",
                    format!(
                        "got {flag:?}; accepted: --at, --dependency, --check, --restore, --worktree"
                    ),
                ));
            }
            path => positional.push(PathBuf::from(path)),
        }
    }
    match (positional.as_slice(), selectors.is_empty(), restore, check) {
        ([destination], false, None, false) if !worktree => Ok(Mode::Export {
            destination: destination.clone(),
            selectors,
            dependencies,
        }),
        ([archive], true, None, true) if dependencies.is_empty() && !worktree => {
            Ok(Mode::Inspect {
                archive: archive.clone(),
            })
        }
        ([archive], _, Some(destination), false)
            if selectors.len() <= 1 && dependencies.is_empty() =>
        {
            Ok(Mode::Restore {
                archive: archive.clone(),
                destination,
                snapshot: selectors.into_iter().next(),
                worktree,
            })
        }
        _ => Err(export_error("Export options do not name one mode", USAGE)),
    }
}

pub(super) fn run(project: &Path, arguments: &[String]) -> Result<(), Diagnostic> {
    match parse_options(arguments)? {
        Mode::Export {
            destination,
            selectors,
            dependencies,
        } => run_export(project, &destination, &selectors, &dependencies),
        Mode::Inspect { archive } => report_archive(&archive),
        Mode::Restore {
            archive,
            destination,
            snapshot,
            worktree,
        } => run_restore(&archive, &destination, snapshot.as_deref(), worktree),
    }
}

fn run_export(
    project: &Path,
    destination: &Path,
    selectors: &[String],
    dependencies: &[DependencySource],
) -> Result<(), Diagnostic> {
    let repository = Repository::discover(project).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let selectors: Vec<&str> = selectors.iter().map(String::as_str).collect();
    let manifest = export_complete_copy_of(&repository, &selectors, destination, dependencies)
        .map_err(|error| describe_export_failure(&error))?;
    let (objects, dependencies) = manifest.counts();
    println!(
        "exported {} ({} snapshots): {objects} objects, {dependencies} pinned dependencies",
        manifest.snapshot,
        manifest.members.len()
    );
    for member in manifest.members.iter().skip(1) {
        println!("  with {}", member.commit);
    }
    report_dependencies(&manifest);
    Ok(())
}

/// Reconstructs one archive into a fresh directory and verifies it there.
fn run_restore(
    archive: &Path,
    destination: &Path,
    snapshot: Option<&str>,
    worktree: bool,
) -> Result<(), Diagnostic> {
    let manifest = match snapshot {
        Some(commit) => restore_complete_copy_at(archive, destination, commit),
        None => restore_complete_copy(archive, destination),
    }
    .map_err(|error| describe_export_failure(&error))?;
    let (objects, dependencies) = manifest.counts();
    println!(
        "restored {} ({} snapshots): {objects} objects, {dependencies} pinned dependencies verified",
        manifest.snapshot,
        manifest.members.len()
    );
    if worktree {
        materialize_complete_copy(destination, &manifest)
            .map_err(|error| describe_export_failure(&error))?;
        println!("materialised the pinned snapshot and its dependencies");
    }
    report_dependencies(&manifest);
    Ok(())
}

/// Reads one archive back and reports what it records.
fn report_archive(archive: &Path) -> Result<(), Diagnostic> {
    let manifest =
        read_complete_copy_manifest(archive).map_err(|error| describe_export_failure(&error))?;
    let (objects, dependencies) = manifest.counts();
    println!(
        "complete copy of {} ({} snapshots): {objects} objects, {dependencies} pinned dependencies",
        manifest.snapshot,
        manifest.members.len()
    );
    for member in manifest.members.iter().skip(1) {
        println!("  with {}", member.commit);
    }
    report_dependencies(&manifest);
    Ok(())
}

fn report_dependencies(manifest: &CompleteCopyManifest) {
    for dependency in &manifest.dependencies {
        println!("  {} at {}", dependency.path, dependency.commit);
    }
}

/// ORNA-GIT-012: name the unavailable scope and keep the remedy actionable.
fn describe_export_failure(error: &CompleteCopyError) -> Diagnostic {
    let title: &'static str = match error {
        CompleteCopyError::TargetNotEmpty => "Destination is not empty",
        CompleteCopyError::UnresolvedSnapshot => "--at did not resolve to a commit",
        CompleteCopyError::NotAFormat3Snapshot => "Snapshot is not an admitted repository snapshot",
        CompleteCopyError::MissingDependencyOrigin { .. } => {
            "A pinned dependency has no recorded origin"
        }
        CompleteCopyError::DependencyUnavailable { .. } => "A pinned dependency is unavailable",
        CompleteCopyError::InvalidDependencyPath { .. } => "A pinned dependency path is invalid",
        CompleteCopyError::DependencyCycle { .. } => "Pinned dependencies contain a cycle",
        CompleteCopyError::ClosureTooLarge => "Snapshot closure exceeds the export bound",
        CompleteCopyError::MissingObject { .. } => "A required Git object is unavailable offline",
        CompleteCopyError::IntegrityMismatch { .. } => "A recorded object failed verification",
        CompleteCopyError::GitUnavailable => "Git is unavailable or refused the export",
        CompleteCopyError::InvalidArchive(_) => "Archive is not a complete copy",
        CompleteCopyError::Io(_) => "Archive could not be written",
    };
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "export from an admitted repository snapshot and keep every pinned dependency available",
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Mode, parse_options, run};

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_destination_selectors_and_dependencies() {
        let mode = parse_options(&words(&[
            "out",
            "--at",
            "main",
            "--at",
            "@~1",
            "--dependency",
            "deps/std=/srv/std",
        ]))
        .expect("parse export arguments");
        let Mode::Export {
            destination,
            selectors,
            dependencies,
        } = mode
        else {
            panic!("expected an export mode");
        };
        assert_eq!(destination.to_str(), Some("out"));
        assert_eq!(selectors, vec!["main".to_owned(), "@~1".to_owned()]);
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].path, "deps/std");
        assert_eq!(dependencies[0].directory.to_str(), Some("/srv/std"));
    }

    #[test]
    fn parses_check_and_restore_modes_separately() {
        assert_eq!(
            parse_options(&words(&["archive", "--check"])).expect("parse check"),
            Mode::Inspect {
                archive: "archive".into()
            }
        );
        assert_eq!(
            parse_options(&words(&["archive", "--restore", "out", "--worktree"]))
                .expect("parse restore"),
            Mode::Restore {
                archive: "archive".into(),
                destination: "out".into(),
                snapshot: None,
                worktree: true
            }
        );
        assert_eq!(
            parse_options(&words(&["archive", "--restore", "out", "--at", "past"]))
                .expect("parse selected restore"),
            Mode::Restore {
                archive: "archive".into(),
                destination: "out".into(),
                snapshot: Some("past".into()),
                worktree: false
            }
        );
    }

    #[test]
    fn rejects_mixed_or_incomplete_modes() {
        assert!(parse_options(&words(&["out"])).is_err());
        assert!(parse_options(&words(&["out", "--at", "main", "--dependency", "deps"])).is_err());
        assert!(parse_options(&words(&["out", "--at", "main", "--unknown"])).is_err());
        assert!(parse_options(&words(&[])).is_err());
        // `--check` describes an archive, not a snapshot.
        assert!(parse_options(&words(&["out", "--at", "main", "--check"])).is_err());
        // `--worktree` only means something for a reconstruction.
        assert!(parse_options(&words(&["out", "--at", "main", "--worktree"])).is_err());
        assert!(parse_options(&words(&["a", "b", "--restore", "c"])).is_err());
        // `--dependency` only applies while exporting a snapshot.
        assert!(
            parse_options(&words(&[
                "archive",
                "--restore",
                "out",
                "--dependency",
                "deps/std=/srv/std",
            ]))
            .is_err()
        );
        // A selector needs a value, and one flag cannot carry two.
        assert!(parse_options(&words(&["out", "--at"])).is_err());
        assert!(parse_options(&words(&["out", "--at", ""])).is_err());
        assert!(
            parse_options(&words(&[
                "archive",
                "--restore",
                "out",
                "--at",
                "a",
                "--at",
                "b"
            ]))
            .is_err()
        );
    }

    #[test]
    fn check_reports_an_absent_archive_without_a_repository() {
        // A read-only check must fail on the archive, not silently succeed.
        let missing = tempfile::tempdir().expect("create temporary directory");
        assert!(run(missing.path(), &words(&["./absent", "--check"])).is_err());
    }
}
