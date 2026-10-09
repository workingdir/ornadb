//! `orna export DEST --at SELECTOR [--dependency PATH=DIR]...`: freezes and
//! verifies one complete offline copy before claiming one.
//!
//! The selected snapshot's full object closure and every recursively pinned
//! dependency are written as a portable archive (ORNA-GIT-003, ORNA-GIT-007).
//! Exporting never moves the user's branch, never creates a pin, and never
//! contacts a remote: the source repository is read only (ORNA-GIT-008).
//!
//! `orna export DEST --at SELECTOR --check` reads an archive back instead of
//! writing one and reports what it records, so a copy can be inspected without
//! touching a repository.

use std::path::{Path, PathBuf};

use orna_repository_v1::complete_copy::{
    CompleteCopyError, DependencySource, export_complete_copy, read_complete_copy_manifest,
};
use orna_repository_v1::Repository;

use super::Diagnostic;

const USAGE: &str = "usage: orna export <DEST> --at <SELECTOR> [--dependency PATH=DIR]... [--check]";

/// One parsed export request.
#[derive(Debug, Eq, PartialEq)]
struct ExportOptions {
    destination: PathBuf,
    selector: String,
    dependencies: Vec<DependencySource>,
    check_only: bool,
}

fn export_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna export DEST --at SELECTOR` inside a format-3 repository",
        detail.into(),
    )
}

fn parse_options(arguments: &[String]) -> Result<ExportOptions, Diagnostic> {
    let mut destination = None;
    let mut selector = None;
    let mut dependencies = Vec::new();
    let mut check_only = false;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--at" => {
                let value = words
                    .next()
                    .ok_or_else(|| export_error("--at needs a value", USAGE))?;
                if selector.is_some() {
                    return Err(export_error("Export names one snapshot", USAGE));
                }
                selector = Some(value.to_owned());
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
            "--check" => check_only = true,
            flag if flag.starts_with("--") => {
                return Err(export_error(
                    "Unknown export flag",
                    format!("got {flag:?}; accepted: --at, --dependency, --check"),
                ));
            }
            path if destination.is_none() => destination = Some(PathBuf::from(path)),
            extra => {
                return Err(export_error(
                    "Export takes one destination directory",
                    format!("unexpected {extra:?}"),
                ));
            }
        }
    }
    Ok(ExportOptions {
        destination: destination
            .ok_or_else(|| export_error("Export expects a destination directory", USAGE))?,
        selector: selector.ok_or_else(|| export_error("Export expects --at SELECTOR", USAGE))?,
        dependencies,
        check_only,
    })
}

pub(super) fn run(project: &Path, arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    if options.check_only {
        return report_archive(&options.destination);
    }
    let repository = Repository::discover(project).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let manifest = export_complete_copy(
        &repository,
        &options.selector,
        &options.destination,
        &options.dependencies,
    )
    .map_err(|error| describe_export_failure(&error))?;
    let (objects, dependencies) = manifest.counts();
    println!(
        "exported {}: {objects} objects, {dependencies} pinned dependencies",
        manifest.snapshot
    );
    for dependency in &manifest.dependencies {
        println!("  {} at {}\n", dependency.path, dependency.commit);
    }
    Ok(())
}

/// Reads one archive back and reports what it records.
fn report_archive(destination: &Path) -> Result<(), Diagnostic> {
    let manifest = read_complete_copy_manifest(destination)
        .map_err(|error| describe_export_failure(&error))?;
    let (objects, dependencies) = manifest.counts();
    println!(
        "complete copy of {}: {objects} objects, {dependencies} pinned dependencies",
        manifest.snapshot
    );
    for dependency in &manifest.dependencies {
        println!("  {} at {}\n", dependency.path, dependency.commit);
    }
    Ok(())
}

/// ORNA-GIT-012: name the unavailable scope and keep the remedy actionable.
fn describe_export_failure(error: &CompleteCopyError) -> Diagnostic {
    let title: &'static str = match error {
        CompleteCopyError::TargetNotEmpty => "Destination is not empty",
        CompleteCopyError::UnresolvedSnapshot => "--at did not resolve to a commit",
        CompleteCopyError::NotAFormat3Snapshot => {
            "Snapshot is not a format-3 repository snapshot"
        }
        CompleteCopyError::MissingDependencyOrigin { .. } => {
            "A pinned dependency has no recorded origin"
        }
        CompleteCopyError::DependencyUnavailable { .. } => "A pinned dependency is unavailable",
        CompleteCopyError::ClosureTooLarge => "Snapshot closure exceeds the export bound",
        CompleteCopyError::IntegrityMismatch { .. } => "A recorded object failed verification",
        CompleteCopyError::GitUnavailable => "Git is unavailable or refused the export",
        CompleteCopyError::InvalidArchive(_) => "Archive is not a complete copy",
        CompleteCopyError::Io(_) => "Archive could not be written",
    };
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "export from a format-3 repository and keep every pinned dependency available",
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_options, run};
    use std::path::Path;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_destination_selector_and_dependencies() {
        let options = parse_options(&words(&[
            "out",
            "--at",
            "main",
            "--dependency",
            "deps/std=/srv/std",
        ]))
        .expect("parse export arguments");
        assert_eq!(options.destination.to_str(), Some("out"));
        assert_eq!(options.selector, "main");
        assert_eq!(options.dependencies.len(), 1);
        assert_eq!(options.dependencies[0].path, "deps/std");
        assert_eq!(options.dependencies[0].directory.to_str(), Some("/srv/std"));
        assert!(!options.check_only);
    }

    #[test]
    fn rejects_missing_selector_and_malformed_dependency() {
        assert!(parse_options(&words(&["out"])).is_err());
        assert!(parse_options(&words(&["out", "--at", "main", "--dependency", "deps"])).is_err());
        assert!(parse_options(&words(&["out", "--at", "main", "--unknown"])).is_err());
        assert!(parse_options(&words(&[])).is_err());
    }

    #[test]
    fn check_reports_an_absent_archive_without_a_repository() {
        // A read-only check must fail on the archive, not silently succeed.
        let missing = tempfile::tempdir().expect("create temporary directory");
        assert!(run(missing.path(), &words(&["./absent", "--at", "main", "--check"])).is_err());
        assert!(Path::new(".").exists());
    }
}
