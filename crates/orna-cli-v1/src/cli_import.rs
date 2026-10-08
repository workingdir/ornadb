//! `orna import <bundle-dir> --dry-run [--limit N] [--quiet]`: verifies an
//! offline copy bundle in full and reports the rows and history an import would
//! write. Nothing is written to the bundle or to any repository. A committing
//! import is not wired yet, so `--dry-run` is required. `--limit N` reports only
//! the first N rows in bundle order; every row is still verified. `--quiet`
//! suppresses the success report; failures still exit non-zero with a diagnostic.

use std::path::Path;

use orna_repository_v1::offline_copy::OfflineCopy;

use super::Diagnostic;

const USAGE: &str = "usage: orna import <bundle-dir> --dry-run [--limit N] [--quiet]";

/// Parsed import options: the bundle directory, optional row limit, and output mode.
#[derive(Debug, Eq, PartialEq)]
struct ImportOptions<'a> {
    bundle: &'a str,
    limit: Option<usize>,
    quiet: bool,
}

/// Returns the bundle directory, row limit, and quiet flag when the arguments name one and request a dry run.
fn parse_options(arguments: &[String]) -> Result<ImportOptions<'_>, Diagnostic> {
    let mut bundle = None;
    let mut dry_run = false;
    let mut limit = None;
    let mut quiet = false;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--dry-run" => dry_run = true,
            "--quiet" => quiet = true,
            "--limit" => {
                let value = words.next().ok_or_else(|| {
                    import_error("--limit needs a value", "usage: --limit <N>, N >= 1")
                })?;
                limit = Some(
                    value
                        .parse::<usize>()
                        .ok()
                        .filter(|limit| *limit >= 1)
                        .ok_or_else(|| {
                            import_error(
                                "--limit is not a positive number",
                                format!("got {value:?}"),
                            )
                        })?,
                );
            }
            flag if flag.starts_with("--") => {
                return Err(import_error(
                    "Unknown import flag",
                    format!("got {flag:?}; accepted: --dry-run, --limit, --quiet"),
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
    Ok(ImportOptions {
        bundle,
        limit,
        quiet,
    })
}

pub(super) fn run(arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let summary = dry_run_summary(Path::new(options.bundle), options.limit)?;
    if !options.quiet {
        println!("{summary}");
    }
    Ok(())
}

/// Verifies the bundle and returns the one-line dry-run report. With a limit,
/// only the first rows are counted; history is bundle-wide and always counted.
fn dry_run_summary(bundle: &Path, limit: Option<usize>) -> Result<String, Diagnostic> {
    let plan = OfflineCopy::open(bundle)
        .and_then(|copy| copy.import_plan())
        .map_err(|error| import_error("Bundle could not be verified", format!("{error:?}")))?;
    let total = plan.rows.len();
    let reported = limit.map_or(total, |limit| limit.min(total));
    let payload_bytes: u64 = plan.rows[..reported].iter().map(|row| row.length).sum();
    Ok(format!(
        "dry run: {reported} of {total} rows, {payload_bytes} payload bytes, {} history entries; nothing written",
        plan.history.len()
    ))
}

fn import_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna import <bundle-dir> --dry-run [--limit N]` on a bundle written by an offline copy",
        detail.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{dry_run_summary, parse_options};
    use orna_repository_v1::offline_copy::{OfflineHistoryEntry, OfflineRow, write_offline_copy};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use tempfile::TempDir;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn row(key: &str, payload: &[u8]) -> OfflineRow {
        OfflineRow {
            key: key.as_bytes().to_vec(),
            media_type: "text/plain".to_owned(),
            suffix: None,
            length: payload.len() as u64,
            sha256: Sha256::digest(payload).into(),
            payload: Some(payload.to_vec()),
        }
    }

    #[test]
    fn options_take_one_bundle_and_require_dry_run() {
        let plain_arguments = words(&["bundle", "--dry-run"]);
        let plain = parse_options(&plain_arguments).unwrap();
        assert_eq!((plain.bundle, plain.limit), ("bundle", None));
        let reordered_arguments = words(&["--dry-run", "bundle"]);
        let reordered = parse_options(&reordered_arguments).unwrap();
        assert_eq!(reordered.bundle, "bundle");
        assert!(parse_options(&words(&["bundle"])).is_err());
        assert!(parse_options(&words(&["--dry-run"])).is_err());
        assert!(parse_options(&words(&["a", "b", "--dry-run"])).is_err());
        assert!(parse_options(&words(&["bundle", "--bogus", "--dry-run"])).is_err());
    }

    #[test]
    fn limit_takes_a_positive_count_in_any_position() {
        let limited_arguments = words(&["--limit", "2", "bundle", "--dry-run"]);
        let limited = parse_options(&limited_arguments).unwrap();
        assert_eq!((limited.bundle, limited.limit), ("bundle", Some(2)));
        assert!(parse_options(&words(&["bundle", "--dry-run", "--limit"])).is_err());
        assert!(parse_options(&words(&["bundle", "--dry-run", "--limit", "0"])).is_err());
        assert!(parse_options(&words(&["bundle", "--dry-run", "--limit", "x"])).is_err());
    }

    #[test]
    fn quiet_is_accepted_in_any_position_and_still_requires_dry_run() {
        let quiet_arguments = words(&["--quiet", "bundle", "--dry-run"]);
        let quiet = parse_options(&quiet_arguments).unwrap();
        assert_eq!((quiet.bundle, quiet.quiet), ("bundle", true));
        let trailing_arguments = words(&["bundle", "--dry-run", "--quiet"]);
        let trailing = parse_options(&trailing_arguments).unwrap();
        assert!(trailing.quiet);
        assert!(parse_options(&words(&["bundle", "--quiet"])).is_err());
    }

    #[test]
    fn dry_run_reports_only_the_limited_rows() {
        let directory = TempDir::new().unwrap();
        let bundle = directory.path().join("bundle");
        let rows = [row("image", b"pixels"), row("song", b"tone-bytes")];
        let history = [OfflineHistoryEntry {
            sequence: 1,
            commit: [0x41; 32],
        }];
        write_offline_copy(&bundle, &rows, &history).unwrap();

        assert_eq!(
            dry_run_summary(&bundle, None).unwrap(),
            "dry run: 2 of 2 rows, 16 payload bytes, 1 history entries; nothing written"
        );
        assert_eq!(
            dry_run_summary(&bundle, Some(1)).unwrap(),
            "dry run: 1 of 2 rows, 6 payload bytes, 1 history entries; nothing written"
        );
        assert!(
            dry_run_summary(&bundle, Some(9))
                .unwrap()
                .starts_with("dry run: 2 of 2")
        );
    }

    #[test]
    fn dry_run_refuses_a_missing_bundle_without_writing() {
        let missing = Path::new("/nonexistent/orna-import-dry-run-bundle");
        assert!(dry_run_summary(missing, None).is_err());
    }
}
