//! `orna import <bundle-dir> --dry-run`: verifies an offline copy bundle in full
//! and reports the rows and history an import would write. Nothing is written
//! to the bundle or to any repository. A committing import is not wired yet,
//! so `--dry-run` is required.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineCopy;

use super::Diagnostic;

const USAGE: &str = "usage: orna import <bundle-dir> --dry-run";

/// Returns the bundle directory when the arguments name one and request a dry run.
fn parse_options(arguments: &[String]) -> Result<&str, Diagnostic> {
    let mut bundle = None;
    let mut dry_run = false;
    for word in arguments.iter().map(String::as_str) {
        match word {
            "--dry-run" => dry_run = true,
            flag if flag.starts_with("--") => {
                return Err(import_error(
                    "Unknown import flag",
                    format!("got {flag:?}; accepted: --dry-run"),
                ));
            }
            path if bundle.is_none() => bundle = Some(path),
            extra => {
                return Err(import_error(
                    "Import takes one bundle directory",
                    format!("unexpected {extra:?}"),
                ));
            }
        }
    }
    let bundle = bundle.ok_or_else(|| import_error("Import expects a bundle directory", USAGE))?;
    if !dry_run {
        return Err(import_error(
            "Import without --dry-run is not available",
            USAGE,
        ));
    }
    Ok(bundle)
}

pub(super) fn run(arguments: &[String]) -> Result<(), Diagnostic> {
    let bundle = parse_options(arguments)?;
    println!("{}", dry_run_summary(Path::new(bundle))?);
    Ok(())
}

/// Verifies the bundle and returns the one-line dry-run report.
fn dry_run_summary(bundle: &Path) -> Result<String, Diagnostic> {
    let report = OfflineCopy::open(bundle)
        .and_then(|copy| copy.dry_run_import())
        .map_err(|error| import_error("Bundle could not be verified", format!("{error:?}")))?;
    Ok(format!(
        "dry run: {} rows, {} payload bytes, {} history entries; nothing written",
        report.rows, report.payload_bytes, report.history
    ))
}

fn import_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna import <bundle-dir> --dry-run` on a bundle written by an offline copy",
        detail.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{dry_run_summary, parse_options};
    use std::path::Path;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn options_take_one_bundle_and_require_dry_run() {
        assert_eq!(
            parse_options(&words(&["bundle", "--dry-run"])).unwrap(),
            "bundle"
        );
        assert_eq!(
            parse_options(&words(&["--dry-run", "bundle"])).unwrap(),
            "bundle"
        );
        assert!(parse_options(&words(&["bundle"])).is_err());
        assert!(parse_options(&words(&["--dry-run"])).is_err());
        assert!(parse_options(&words(&["a", "b", "--dry-run"])).is_err());
        assert!(parse_options(&words(&["bundle", "--bogus", "--dry-run"])).is_err());
    }

    #[test]
    fn dry_run_refuses_a_missing_bundle_without_writing() {
        let missing = Path::new("/nonexistent/orna-import-dry-run-bundle");
        assert!(dry_run_summary(missing).is_err());
    }
}
