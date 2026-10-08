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
    limit: usize,
    since: Option<&'a str>,
    format: HistoryFormat,
    reverse: bool,
    author: Option<&'a str>,
}

fn parse_options(arguments: &[String]) -> Result<HistoryOptions<'_>, Diagnostic> {
    let mut positional = Vec::new();
    let mut limit = DEFAULT_HISTORY_LIMIT;
    let mut since = None;
    let mut format = HistoryFormat::Human;
    let mut reverse = false;
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
                        history_error(
                            "--limit is not in 1..=4096",
                            format!("got {value:?}"),
                        )
                    })?;
            }
            "--since" => {
                let value = words.next().ok_or_else(|| {
                    history_error("--since needs a commit id", "usage: --since <commit-hex>")
                })?;
                since = Some(value);
            }
            "--reverse" => reverse = true,
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
                    format!("got {flag:?}; accepted: --limit, --since, --format, --reverse, --author"),
                ));
            }
            _ => positional.push(word),
        }
    }
    let [relation, key] = positional[..] else {
        return Err(history_error(
            "History expects a relation and a row key",
            "usage: orna history <relation-hex> <key> [--limit N] [--since <commit-hex>]",
        ));
    };
    Ok(HistoryOptions {
        relation,
        key,
        limit,
        since,
        format,
        reverse,
        author,
    })
}

pub(super) fn run(arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let relation = parse_relation_id(options.relation)?;
    let key = options.key;
    let repository = Repository::discover(
        std::env::current_dir()
            .map_err(|error| history_error("Current directory is unavailable", error.to_string()))?,
    )
    .map_err(|error| history_error("Repository could not be opened", format!("{error:?}")))?;
    let format = repository
        .open_format_context()
        .map_err(|error| history_error("Format context could not be opened", format!("{error:?}")))?;
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
        .ok_or_else(|| {
            history_error("Row is not committed", format!("no row with key {key:?}"))
        })?;
    // `--since` needs the walk far enough to reach its commit, so walk the
    // full bound and cut afterwards; otherwise walk only what is printed.
    let walk = if options.since.is_some() || options.author.is_some() {
        MAX_HISTORY_LIMIT
    } else {
        options.limit
    };
    let mut revisions = graph
        .list_row_revisions(&row, walk, &scope)
        .map_err(|error| history_error("Revision history could not be listed", format!("{error:?}")))?;
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
    match options.format {
        HistoryFormat::Human => {
            let present = revisions.iter().filter(|revision| revision.present()).count();
            for revision in &revisions {
                let state = if revision.present() { "present" } else { "absent" };
                println!(
                    "{} {} {state}",
                    revision.commit().to_hex(),
                    revision.tree().to_hex()
                );
            }
            println!("{}", summary_line(revisions.len(), present));
        }
        HistoryFormat::Json => {
            let entries: Vec<serde_json::Value> = revisions
                .iter()
                .map(|revision| {
                    serde_json::json!({
                        "commit": revision.commit().to_hex(),
                        "tree": revision.tree().to_hex(),
                        "present": revision.present(),
                        "author": revision.author(),
                    })
                })
                .collect();
            println!("{}", serde_json::Value::Array(entries));
        }
    }
    Ok(())
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
    use super::{DEFAULT_HISTORY_LIMIT, HistoryFormat, parse_options, parse_relation_id};

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
        let flagged_arguments = words(&["--limit", "3", "0102", "--since", "abc", "song"]);
        let parsed = parse_options(&flagged_arguments).unwrap();
        assert_eq!((parsed.relation, parsed.key), ("0102", "song"));
        assert_eq!((parsed.limit, parsed.since), (3, Some("abc")));
        assert_eq!(parsed.format, HistoryFormat::Human);
        let json = words(&["--format", "json", "0102", "song"]);
        assert_eq!(parse_options(&json).unwrap().format, HistoryFormat::Json);
    }

    #[test]
    fn options_reject_bad_limits_unknown_flags_and_missing_values() {
        assert!(parse_options(&words(&["r", "k", "--limit", "0"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--limit", "4097"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--limit", "x"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--since"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--bogus"])).is_err());
        assert!(parse_options(&words(&["r"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--format", "xml"])).is_err());
        assert!(parse_options(&words(&["r", "k", "--format"])).is_err());
    }

    #[test]
    fn summary_line_counts_total_present_and_absent() {
        assert_eq!(super::summary_line(0, 0), "0 revisions (0 present, 0 absent)");
        assert_eq!(super::summary_line(1, 1), "1 revision (1 present, 0 absent)");
        assert_eq!(super::summary_line(4, 3), "4 revisions (3 present, 1 absent)");
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
