//! `orna import <bundle-dir> --dry-run [--limit N] [--quiet] [--type MEDIA_TYPE] [--format human|table]`:
//! verifies an offline copy bundle in full and reports the rows and history an
//! import would write. Nothing is written to the bundle or to any repository. A
//! committing import is not wired yet, so `--dry-run` is required. `--limit N`
//! reports only the first N rows in bundle order; every row is still verified.
//! `--quiet` suppresses the success report; failures still exit non-zero with a
//! diagnostic. `--type MEDIA_TYPE` reports every row under that media type
//! instead of its recorded one; the bundle's stored types are not changed.
//! Unless `--quiet` is given, one progress line per verified row goes to stderr.
//! Progress covers every row, because `--limit` does not skip verification.
//! `--format table` lists each reported row (key, media type, bytes) above the
//! summary line; `--format human` prints the summary line alone.

use std::path::Path;

use orna_repository_v1::offline_copy::{OfflineCopy, OfflineImportMetadata};
use orna_value_v1::normalize_media_type;

use super::Diagnostic;

const USAGE: &str = "usage: orna import <bundle-dir> --dry-run [--limit N] [--quiet] [--type MEDIA_TYPE] [--format human|table]";

/// Output shape for the dry-run report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportFormat {
    Human,
    Table,
}

/// Parsed import options: the bundle directory, optional row limit, output mode, media type override, and report format.
#[derive(Debug, Eq, PartialEq)]
struct ImportOptions<'a> {
    bundle: &'a str,
    limit: Option<usize>,
    quiet: bool,
    media_type: Option<String>,
    format: ReportFormat,
}

/// Returns the bundle directory, row limit, quiet flag, media type override, and report format when the arguments name one and request a dry run.
fn parse_options(arguments: &[String]) -> Result<ImportOptions<'_>, Diagnostic> {
    let mut bundle = None;
    let mut dry_run = false;
    let mut limit = None;
    let mut quiet = false;
    let mut media_type = None;
    let mut format = ReportFormat::Human;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--dry-run" => dry_run = true,
            "--quiet" => quiet = true,
            "--format" => {
                let value = words.next().ok_or_else(|| {
                    import_error("--format needs a value", "usage: --format <human|table>")
                })?;
                format = match value {
                    "human" => ReportFormat::Human,
                    "table" => ReportFormat::Table,
                    _ => {
                        return Err(import_error(
                            "--format is not human or table",
                            format!("got {value:?}"),
                        ));
                    }
                };
            }
            "--type" => {
                let value = words.next().ok_or_else(|| {
                    import_error("--type needs a value", "usage: --type <MEDIA_TYPE>")
                })?;
                media_type = Some(normalize_media_type(value).map_err(|error| {
                    import_error(
                        "--type is not a valid media type",
                        format!("got {value:?}: {error:?}"),
                    )
                })?);
            }
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
                    format!(
                        "got {flag:?}; accepted: --dry-run, --limit, --quiet, --type, --format"
                    ),
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
        media_type,
        format,
    })
}

pub(super) fn run(arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let summary = dry_run_summary(
        Path::new(options.bundle),
        options.limit,
        options.media_type.as_deref(),
        !options.quiet,
        options.format,
    )?;
    if !options.quiet {
        println!("{summary}");
    }
    Ok(())
}

/// Verifies the bundle and returns the dry-run report. With a limit, only the
/// first rows are counted; history is bundle-wide and always counted. A media
/// type override is named in the report; it does not alter the bundle. The
/// table format lists each reported row above the summary line.
fn dry_run_summary(
    bundle: &Path,
    limit: Option<usize>,
    media_type: Option<&str>,
    progress: bool,
    format: ReportFormat,
) -> Result<String, Diagnostic> {
    let copy = OfflineCopy::open(bundle)
        .map_err(|error| import_error("Bundle could not be verified", format!("{error:?}")))?;
    // Every row is verified even when the report is limited, and the payloads
    // are hashed from a fixed buffer rather than retained (CAPTURE-3 step 2).
    let total = copy.rows().len();
    let summary = copy
        .import_summary_with_progress(total, |step| {
            if progress {
                eprintln!("{step}");
            }
        })
        .map_err(|error| import_error("Bundle could not be verified", format!("{error:?}")))?;
    let reported = limit.map_or(total, |limit| limit.min(total));
    let reported_rows = &summary.rows[..reported];
    let payload_bytes: u64 = reported_rows.iter().map(|row| row.length).sum();
    let typed = media_type.map_or(String::new(), |media_type| format!(" as {media_type}"));
    let summary_line = format!(
        "dry run: {reported} of {total} rows{typed}, {payload_bytes} payload bytes, {} history entries; nothing written",
        summary.history.len()
    );
    Ok(match format {
        ReportFormat::Human => summary_line,
        ReportFormat::Table => format!("{}{summary_line}", render_table(reported_rows, media_type)),
    })
}

/// Renders the reported rows as an aligned table with a header line. Every
/// line ends in a newline, so the summary line can follow directly.
fn render_table(rows: &[OfflineImportMetadata], media_type: Option<&str>) -> String {
    let header = [
        "key".to_owned(),
        "media_type".to_owned(),
        "bytes".to_owned(),
    ];
    let mut lines = vec![header];
    lines.extend(rows.iter().map(|row| {
        [
            String::from_utf8_lossy(&row.key).into_owned(),
            media_type.unwrap_or(row.media_type.as_str()).to_owned(),
            row.length.to_string(),
        ]
    }));
    let mut widths = [0_usize; 3];
    for line in &lines {
        for (width, cell) in widths.iter_mut().zip(line) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let [key_width, type_width, bytes_width] = widths;
    let mut out = String::new();
    for [key, kind, bytes] in &lines {
        out.push_str(&format!(
            "{key:<key_width$}  {kind:<type_width$}  {bytes:>bytes_width$}\n"
        ));
    }
    out
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
    use super::{ReportFormat, dry_run_summary, parse_options};
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
        let plain_words = words(&["bundle", "--dry-run"]);
        let plain = parse_options(&plain_words).unwrap();
        assert_eq!((plain.bundle, plain.limit), ("bundle", None));
        let reordered_words = words(&["--dry-run", "bundle"]);
        let reordered = parse_options(&reordered_words).unwrap();
        assert_eq!(reordered.bundle, "bundle");
        assert!(parse_options(&words(&["bundle"])).is_err());
        assert!(parse_options(&words(&["--dry-run"])).is_err());
        assert!(parse_options(&words(&["a", "b", "--dry-run"])).is_err());
        assert!(parse_options(&words(&["bundle", "--bogus", "--dry-run"])).is_err());
    }

    #[test]
    fn limit_takes_a_positive_count_in_any_position() {
        let limited_words = words(&["--limit", "2", "bundle", "--dry-run"]);
        let limited = parse_options(&limited_words).unwrap();
        assert_eq!((limited.bundle, limited.limit), ("bundle", Some(2)));
        assert!(parse_options(&words(&["bundle", "--dry-run", "--limit"])).is_err());
        assert!(parse_options(&words(&["bundle", "--dry-run", "--limit", "0"])).is_err());
        assert!(parse_options(&words(&["bundle", "--dry-run", "--limit", "x"])).is_err());
    }

    #[test]
    fn quiet_is_accepted_in_any_position_and_still_requires_dry_run() {
        let quiet_words = words(&["--quiet", "bundle", "--dry-run"]);
        let quiet = parse_options(&quiet_words).unwrap();
        assert_eq!((quiet.bundle, quiet.quiet), ("bundle", true));
        let trailing_words = words(&["bundle", "--dry-run", "--quiet"]);
        let trailing = parse_options(&trailing_words).unwrap();
        assert!(trailing.quiet);
        assert!(parse_options(&words(&["bundle", "--quiet"])).is_err());
    }

    #[test]
    fn format_accepts_human_or_table_only() {
        let table_words = words(&["bundle", "--dry-run", "--format", "table"]);
        assert_eq!(
            parse_options(&table_words).unwrap().format,
            ReportFormat::Table
        );
        let default_words = words(&["bundle", "--dry-run"]);
        assert_eq!(
            parse_options(&default_words).unwrap().format,
            ReportFormat::Human
        );
        assert!(parse_options(&words(&["bundle", "--dry-run", "--format", "json"])).is_err());
        assert!(parse_options(&words(&["bundle", "--dry-run", "--format"])).is_err());
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
            dry_run_summary(&bundle, None, None, false, ReportFormat::Human).unwrap(),
            "dry run: 2 of 2 rows, 16 payload bytes, 1 history entries; nothing written"
        );
        assert_eq!(
            dry_run_summary(&bundle, Some(1), None, false, ReportFormat::Human).unwrap(),
            "dry run: 1 of 2 rows, 6 payload bytes, 1 history entries; nothing written"
        );
        assert!(
            dry_run_summary(&bundle, Some(9), None, false, ReportFormat::Human)
                .unwrap()
                .starts_with("dry run: 2 of 2")
        );
    }

    #[test]
    fn table_lists_only_the_reported_rows_above_the_summary() {
        let directory = TempDir::new().unwrap();
        let bundle = directory.path().join("bundle");
        let rows = [row("image", b"pixels"), row("song", b"tone-bytes")];
        write_offline_copy(&bundle, &rows, &[]).unwrap();

        assert_eq!(
            dry_run_summary(&bundle, Some(1), None, false, ReportFormat::Table).unwrap(),
            "key    media_type  bytes\n\
             image  text/plain      6\n\
             dry run: 1 of 2 rows, 6 payload bytes, 0 history entries; nothing written"
        );
    }

    #[test]
    fn progress_does_not_change_the_report() {
        let directory = TempDir::new().unwrap();
        let bundle = directory.path().join("bundle");
        let rows = [row("image", b"pixels"), row("song", b"tone-bytes")];
        write_offline_copy(&bundle, &rows, &[]).unwrap();

        assert_eq!(
            dry_run_summary(&bundle, Some(1), None, true, ReportFormat::Human).unwrap(),
            dry_run_summary(&bundle, Some(1), None, false, ReportFormat::Human).unwrap(),
        );
    }

    #[test]
    fn type_override_is_validated_and_named_in_the_report() {
        let typed_arguments = words(&["--type", "audio/wav", "bundle", "--dry-run"]);
        let typed = parse_options(&typed_arguments).unwrap();
        assert_eq!(typed.media_type.as_deref(), Some("audio/wav"));
        assert!(parse_options(&words(&["bundle", "--dry-run", "--type"])).is_err());
        assert!(parse_options(&words(&["bundle", "--dry-run", "--type", "bogus"])).is_err());

        let directory = TempDir::new().unwrap();
        let bundle = directory.path().join("bundle");
        write_offline_copy(&bundle, &[row("song", b"tone-bytes")], &[]).unwrap();
        assert_eq!(
            dry_run_summary(&bundle, None, Some("audio/wav"), false, ReportFormat::Human).unwrap(),
            "dry run: 1 of 1 rows as audio/wav, 10 payload bytes, 0 history entries; nothing written"
        );
    }

    #[test]
    fn dry_run_refuses_a_missing_bundle_without_writing() {
        let missing = Path::new("/nonexistent/orna-import-dry-run-bundle");
        assert!(dry_run_summary(missing, None, None, true, ReportFormat::Human).is_err());
    }
}
