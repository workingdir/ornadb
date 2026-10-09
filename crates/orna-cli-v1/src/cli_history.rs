//! `orna history <relation-hex> <key> [--limit N] [--since <commit-hex>]`:
//! lists the revision history of one committed row through the OGS-1
//! commit-graph walk. Only commit headers and tree listings are read; no blob
//! payload is opened or hydrated.

use super::*;
use orna_repository_v1::{Repository, TypedKey};

/// Most revisions one history listing reports when `--limit` is not given.
const DEFAULT_HISTORY_LIMIT: usize = 64;
/// Largest `--limit` accepted; matches the repository walk bound.
const MAX_HISTORY_LIMIT: usize = 4096;

/// Output shape for the revision listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryFormat {
    Human,
    Json,
}

/// Parsed history options: positional `<relation-hex> <key>` plus flags.
#[derive(Debug, Eq, PartialEq)]
struct HistoryOptions<'a> {
    relation: &'a str,
    key: &'a str,
    at: Option<&'a str>,
    limit: usize,
    since: Option<&'a str>,
    format: HistoryFormat,
    reverse: bool,
    quiet: bool,
    count: bool,
    author: Option<&'a str>,
}

fn parse_options(arguments: &[String]) -> Result<HistoryOptions<'_>, Diagnostic> {
    let mut positional = Vec::new();
    let mut at = None;
    let mut limit = DEFAULT_HISTORY_LIMIT;
    let mut since = None;
    let mut format = HistoryFormat::Human;
    let mut reverse = false;
    let mut quiet = false;
    let mut count = false;
    let mut author = None;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--limit" => {
                let value = words.next().ok_or_else(|| {
                    history_error("--limit needs a value", "usage: --limit <1..=4096>")
                })?;
                limit = value
                    .parse::<usize>()
                    .ok()
                    .filter(|limit| (1..=MAX_HISTORY_LIMIT).contains(limit))
                    .ok_or_else(|| {
                        history_error("--limit is not in 1..=4096", format!("got {value:?}"))
                    })?;
            }
            "--since" => {
                let value = words.next().ok_or_else(|| {
                    history_error("--since needs a commit id", "usage: --since <commit-hex>")
                })?;
                since = Some(value);
            }
            "--at" => {
                let value = words
                    .next()
                    .ok_or_else(|| history_error("--at needs a value", "usage: --at <selector>"))?;
                if at.is_some() {
                    return Err(history_error(
                        "History names one snapshot",
                        "usage: --at <selector>",
                    ));
                }
                at = Some(value);
            }
            "--reverse" => reverse = true,
            "--quiet" => quiet = true,
            "--count" => count = true,
            "--author" => {
                let value = words.next().ok_or_else(|| {
                    history_error("--author needs a value", "usage: --author <substring>")
                })?;
                author = Some(value);
            }
            "--format" => {
                let value = words.next().ok_or_else(|| {
                    history_error("--format needs a value", "usage: --format <human|json>")
                })?;
                format = match value {
                    "human" => HistoryFormat::Human,
                    "json" => HistoryFormat::Json,
                    _ => {
                        return Err(history_error(
                            "--format is not human or json",
                            format!("got {value:?}"),
                        ));
                    }
                };
            }
            flag if flag.starts_with("--") => {
                return Err(history_error(
                    "Unknown history flag",
                    format!("got {flag:?}; accepted: --at, --limit, --since, --format, --reverse, --author, --count, --quiet"),
                ));
            }
            _ => positional.push(word),
        }
    }
    let [relation, key] = positional[..] else {
        return Err(history_error(
            "History expects a relation and a row key",
            "usage: orna history <relation-hex> <key> [--at SELECTOR] [--limit N] [--since <commit-hex>]",
        ));
    };
    Ok(HistoryOptions {
        relation,
        key,
        at,
        limit,
        since,
        format,
        reverse,
        quiet,
        count,
        author,
    })
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let relation = parse_relation_id(options.relation)?;
    let key = options.key;
    let path = local_project_path(endpoint)?;
    let repository = Repository::discover(path)
        .map_err(|error| history_error("Repository could not be opened", format!("{error:?}")))?;
    // `--at` pins one named snapshot. The selector is resolved exactly once
    // here; the row map, the read and the revision walk all come from the
    // commit it named, so a branch that advances during the read never changes
    // the answer. Without `--at` the walk starts at the current HEAD.
    let (format, start) = match options.at {
        Some(selector) => {
            // An abbreviated object id is never ambiguous here: `rev-parse`
            // reports a prefix that names no object or more than one exactly as
            // a failure, which surfaces as a typed diagnostic below.
            if bare_ref_selector_is_ambiguous(path, selector)? {
                return Err(Diagnostic::target_with_detail(
                    "E2000",
                    "Snapshot name is ambiguous",
                    "use the full ref name (`refs/heads/NAME`, `refs/tags/NAME`, \
                     `refs/remotes/ORIGIN/NAME`) or the commit id `orna history` lists",
                    format!("{selector:?} names more than one branch, tag or remote branch"),
                ));
            }
            let commit = repository.resolve_snapshot(selector).map_err(|error| {
                history_error(
                    "Snapshot could not be resolved",
                    format!("{selector:?}: {error:?}"),
                )
            })?;
            let start = commit.as_str().to_owned();
            let format = repository
                .open_pinned_format_context(&start)
                .map_err(|error| {
                    history_error(
                        "Snapshot could not be pinned",
                        format!("{selector:?}: {error:?}"),
                    )
                })?;
            (format, start)
        }
        None => {
            let format = repository.open_format_context().map_err(|error| {
                history_error("Format context could not be opened", format!("{error:?}"))
            })?;
            (format, "HEAD".to_owned())
        }
    };
    // A format-1/2 pin is a read-only compatibility input: it carries no
    // native `.orna/store`, so it has no row map to walk. Say so instead of
    // reporting the format-3 store seam's generic failure.
    if format.is_read_only() {
        return Err(history_error(
            "Snapshot is a legacy format-1/2 input",
            "--at names a read-only compatibility snapshot with no native row store; name a format-3 commit",
        ));
    }
    let row_map = format
        .load_row_map(relation)
        .map_err(|error| history_error("Row map could not be loaded", format!("{error:?}")))?;
    let graph = format
        .open_native_graph(&row_map)
        .map_err(|error| history_error("Native graph could not be opened", format!("{error:?}")))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| history_error("Read scope could not be opened", format!("{error:?}")))?;
    let row = graph
        .lookup_row(&TypedKey::Text(key.to_owned()), &scope)
        .map_err(|error| history_error("Row lookup failed", format!("{error:?}")))?
        .ok_or_else(|| history_error("Row is not committed", format!("no row with key {key:?}")))?;
    // `--since` needs the walk far enough to reach its commit, so walk the
    // full bound and cut afterwards; otherwise walk only what is printed.
    let walk = if options.since.is_some() || options.author.is_some() {
        MAX_HISTORY_LIMIT
    } else {
        options.limit
    };
    let mut revisions = graph
        .list_row_revisions(&row, &start, walk, &scope)
        .map_err(|error| {
            history_error("Revision history could not be listed", format!("{error:?}"))
        })?;
    if let Some(since) = options.since {
        let position = revisions
            .iter()
            .position(|revision| revision.commit().to_hex() == since)
            .ok_or_else(|| {
                history_error(
                    "--since commit is not in the revision history",
                    format!("no revision {since:?} within the {walk} newest commits"),
                )
            })?;
        revisions.truncate(position);
    }
    // `--author` keeps revisions whose `Name <email>` contains the substring.
    if let Some(author) = options.author {
        revisions.retain(|revision| revision.author().contains(author));
    }
    revisions.truncate(options.limit);
    // Revisions arrive newest first; `--reverse` prints oldest first.
    if options.reverse {
        revisions.reverse();
    }
    // `--count` prints only the number of revisions left after every filter.
    if options.count {
        println!("{}", revisions.len());
        return Ok(());
    }
    match options.format {
        HistoryFormat::Human => {
            let entries: Vec<(String, String, bool)> = revisions
                .iter()
                .map(|revision| {
                    (
                        revision.commit().to_hex(),
                        revision.tree().to_hex(),
                        revision.present(),
                    )
                })
                .collect();
            for line in human_lines(&entries, options.quiet) {
                println!("{line}");
            }
        }
        HistoryFormat::Json => {
            let entries: Vec<serde_json::Value> = revisions
                .iter()
                .map(|revision| {
                    revision_json(
                        &revision.commit().to_hex(),
                        &revision.tree().to_hex(),
                        revision.present(),
                        revision.author(),
                    )
                })
                .collect();
            println!("{}", serde_json::Value::Array(entries));
        }
    }
    Ok(())
}

/// One revision as a JSON object: commit, root tree, presence and author.
fn revision_json(commit: &str, tree: &str, present: bool, author: &str) -> serde_json::Value {
    serde_json::json!({
        "commit": commit,
        "tree": tree,
        "present": present,
        "author": author,
    })
}

/// Human listing: one `commit tree state` line per revision, then the summary
/// unless `quiet`. `--quiet` drops only the summary; the listing is unchanged.
fn human_lines(entries: &[(String, String, bool)], quiet: bool) -> Vec<String> {
    let mut lines: Vec<String> = entries
        .iter()
        .map(|(commit, tree, present)| {
            let state = if *present { "present" } else { "absent" };
            format!("{commit} {tree} {state}")
        })
        .collect();
    if !quiet {
        let present = entries.iter().filter(|(_, _, present)| *present).count();
        lines.push(summary_line(entries.len(), present));
    }
    lines
}

/// Final human line: total revisions and how many carry the row.
fn summary_line(total: usize, present: usize) -> String {
    let noun = if total == 1 { "revision" } else { "revisions" };
    format!(
        "{total} {noun} ({present} present, {} absent)",
        total - present
    )
}

/// Parses a 32-digit hexadecimal relation id into its 16 raw bytes.
fn parse_relation_id(value: &str) -> Result<[u8; 16], Diagnostic> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(history_error(
            "Relation id is not 32 hexadecimal digits",
            format!("got {value:?}"),
        ));
    }
    let mut bytes = [0_u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .expect("hexadecimal digits were checked above");
    }
    Ok(bytes)
}

/// Whether a `--at` selector names one branch, tag or remote branch but is not
/// spelled as a full ref, so Git would resolve it by ref lookup precedence.
///
/// `git rev-parse` *succeeds* on such a name and only warns, so a read pinned
/// with `--at amb` would silently return whichever of the colliding refs it
/// preferred. Nothing in the walk could report that choice, so the read is
/// refused instead: the selector is compared against the branch, tag and
/// remote-branch namespaces, and two exact matches mean the name does not
/// identify one snapshot.
///
/// Only exact matches count. `git for-each-ref <pattern>` matches by prefix, so
/// `refs/remotes/origin` also lists `refs/remotes/origin/main`; those are
/// different names and must not be read as a collision.
fn bare_ref_selector_is_ambiguous(directory: &str, selector: &str) -> Result<bool, Diagnostic> {
    // Only a name that could collide is checked. A commit id, a `HEAD~2`-style
    // expression, a revision `@`/`:` syntax, a `a..b` range or a full `refs/`
    // path names one object by construction. Slashed names such as
    // `origin/main` are *not* skipped: a branch and a remote-tracking branch
    // can share one.
    if selector.is_empty()
        || selector.starts_with('-')
        || selector.starts_with("refs/")
        || selector == "HEAD"
        || selector.contains([':', '^', '~', '@'])
        || selector.contains("..")
    {
        return Ok(false);
    }
    let output = std::process::Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/heads/{selector}"),
            &format!("refs/tags/{selector}"),
            &format!("refs/remotes/{selector}"),
            &format!("refs/remotes/{selector}/HEAD"),
        ])
        .current_dir(directory)
        .output()
        .map_err(|error| {
            history_error(
                "Git could not be started",
                format!("check that Git is installed and available on PATH: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(history_error(
            "Git could not list the snapshot names",
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let wanted = [
        format!("refs/heads/{selector}"),
        format!("refs/tags/{selector}"),
        format!("refs/remotes/{selector}"),
        format!("refs/remotes/{selector}/HEAD"),
    ];
    let matches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|name| wanted.iter().any(|candidate| candidate == name))
        .count();
    Ok(matches > 1)
}

fn history_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna history <relation-hex> <key>` inside an initialized repository",
        detail.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_options, parse_relation_id, HistoryFormat, DEFAULT_HISTORY_LIMIT};
    use crate::Exit;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn relation_id_parses_sixteen_bytes_and_rejects_bad_input() {
        let parsed = parse_relation_id("000102030405060708090a0b0c0d0e0f").unwrap();
        assert_eq!(parsed[0], 0x00);
        assert_eq!(parsed[15], 0x0f);
        assert!(parse_relation_id("0123").is_err());
        assert!(parse_relation_id(&"zz".repeat(16)).is_err());
    }

    #[test]
    fn options_default_limit_and_accept_flags_in_any_position() {
        let default_arguments = words(&["0102", "song"]);
        let parsed = parse_options(&default_arguments).unwrap();
        assert_eq!((parsed.limit, parsed.since), (DEFAULT_HISTORY_LIMIT, None));
        assert_eq!(parsed.at, None);
        let flagged_arguments = words(&["--limit", "3", "0102", "--since", "abc", "song"]);
        let parsed = parse_options(&flagged_arguments).unwrap();
        assert_eq!((parsed.relation, parsed.key), ("0102", "song"));
        assert_eq!((parsed.limit, parsed.since), (3, Some("abc")));
        assert_eq!(parsed.format, HistoryFormat::Human);
        let json = words(&["--format", "json", "0102", "song"]);
        assert_eq!(parse_options(&json).unwrap().format, HistoryFormat::Json);
        // `--at` takes one selector, wherever it appears.
        let pinned_arguments = words(&["--at", "HEAD~1", "0102", "song"]);
        let pinned = parse_options(&pinned_arguments).unwrap();
        assert_eq!(pinned.at, Some("HEAD~1"));
    }

    #[test]
    fn options_reject_bad_limits_unknown_flags_and_missing_values() {
        assert!(parse_options(&words(&["r", "k", "--limit", "0"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--limit", "4097"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--limit", "x"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--since"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--at"])).is_err());
        assert!(
            parse_options(&words(&["r", "k", "--at", "a", "--at", "b"])).is_err(),
            "history names one snapshot"
        );
        assert!(parse_options(&words(&["r", "k", "--bogus"])).is_err());
        assert!(parse_options(&words(&["r"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--format", "xml"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--format"])).is_err());
    }

    #[test]
    fn summary_line_counts_total_present_and_absent() {
        assert_eq!(
            super::summary_line(0, 0),
            "0 revisions (0 present, 0 absent)"
        );
        assert_eq!(
            super::summary_line(1, 1),
            "1 revision (1 present, 0 absent)"
        );
        assert_eq!(
            super::summary_line(4, 3),
            "4 revisions (3 present, 1 absent)"
        );
    }

    #[test]
    fn history_errors_exit_with_target_code_one() {
        // Every history failure is a target diagnostic, so the documented
        // exit status for `orna history` errors is 1.
        let bad = [
            words(&["r"]),
            words(&["r", "k", "--limit", "0"]),
            words(&["r", "k", "--bogus"]),
        ];
        for arguments in &bad {
            assert_eq!(parse_options(arguments).unwrap_err().exit, Exit::Target);
        }
    }

    #[test]
    fn revision_json_has_exactly_the_documented_keys() {
        let value = super::revision_json("c0ffee", "7ree", true, "Ada <ada@example.test>");
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["author", "commit", "present", "tree"]);
        assert_eq!(object["present"], serde_json::Value::Bool(true));
        assert_eq!(object["author"], "Ada <ada@example.test>");
    }

    #[test]
    fn revision_listing_round_trips_in_order_as_a_json_array() {
        let listing = serde_json::Value::Array(vec![
            super::revision_json("aaa", "t1", true, "Ada <ada@example.test>"),
            super::revision_json("bbb", "t2", false, "Ada <ada@example.test>"),
        ]);
        let parsed: serde_json::Value = serde_json::from_str(&listing.to_string()).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["commit"], "aaa");
        assert_eq!(entries[1]["commit"], "bbb");
        assert_eq!(entries[1]["present"], serde_json::Value::Bool(false));
        assert_eq!(serde_json::Value::Array(vec![]).to_string(), "[]");
    }

    #[test]
    fn quiet_keeps_the_two_revision_listing_and_drops_only_the_summary() {
        let entries = vec![
            ("c2".to_owned(), "t2".to_owned(), true),
            ("c1".to_owned(), "t1".to_owned(), false),
        ];
        let loud = super::human_lines(&entries, false);
        let quiet = super::human_lines(&entries, true);
        assert_eq!(
            loud,
            [
                "c2 t2 present",
                "c1 t1 absent",
                "2 revisions (1 present, 1 absent)",
            ]
        );
        // Quiet output is exactly the loud listing without its final summary line.
        assert_eq!(quiet, loud[..loud.len() - 1]);
    }

    #[test]
    fn quiet_combines_with_format_and_flags_in_any_position() {
        let flagged = words(&[
            "--quiet", "--format", "json", "0102", "--limit", "2", "song",
        ]);
        let parsed = parse_options(&flagged).unwrap();
        assert!(parsed.quiet);
        assert_eq!(parsed.format, HistoryFormat::Json);
        assert_eq!(
            (parsed.limit, parsed.relation, parsed.key),
            (2, "0102", "song")
        );
    }

    fn quiet_is_a_bare_flag_and_defaults_off() {
        let plain = words(&["0102", "song"]);
        assert!(!parse_options(&plain).unwrap().quiet);
        let flagged = words(&["0102", "--quiet", "song"]);
        assert!(parse_options(&flagged).unwrap().quiet);
    }

    fn count_is_a_bare_flag_and_defaults_off() {
        let plain = words(&["0102", "song"]);
        assert!(!parse_options(&plain).unwrap().count);
        let flagged = words(&["0102", "--count", "song"]);
        assert!(parse_options(&flagged).unwrap().count);
    }

    #[test]
    fn author_takes_a_value_and_defaults_off() {
        let plain = words(&["0102", "song"]);
        assert_eq!(parse_options(&plain).unwrap().author, None);
        let flagged = words(&["0102", "--author", "Ada", "song"]);
        assert_eq!(parse_options(&flagged).unwrap().author, Some("Ada"));
        assert!(parse_options(&words(&["0102", "song", "--author"])).is_err());
    }

    #[test]
    fn reverse_is_a_bare_flag_and_defaults_off() {
        let plain = words(&["0102", "song"]);
        assert!(!parse_options(&plain).unwrap().reverse);
        let flagged = words(&["0102", "--reverse", "song"]);
        assert!(parse_options(&flagged).unwrap().reverse);
    }
}
