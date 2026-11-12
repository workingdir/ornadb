//! `orna history <relation-hex> <key> [--limit N] [--since <commit-hex>]`:
//! lists the revision history of one committed row through the OGS-1
//! commit-graph walk. Only commit headers and tree listings are read; no blob
//! payload is opened or hydrated.
//!
//! At a format-1/2 pin (`--at`, or a legacy `HEAD`) the same listing comes from
//! the versioned legacy reader: a loose row's revisions are the commits whose
//! tree does or does not carry its row unit (ORNA-UPGRADE-001,
//! ORNA-UPGRADE-002, ORNA-ROW-013), and a compact row, which lives inside
//! committed segments rather than in one file, has no history this walk can
//! report.
//!
//! `<key>` is one canonical Orna key expression. A row committed from imported
//! media is keyed by the bytes the capture was given, so the spelling a
//! `orna query` listing printed resolves to that row here too, whether the
//! caller spells it as text or as `0x`-prefixed hex.
//!
//! `orna history <relation-hex> --diff <a> <b>` reports what changed in one
//! relation between two named commits instead. Both endpoints are pinned
//! independently through the same repository-owned historical snapshot
//! context, so the comparison is one pair of immutable reads taken from the
//! local repository alone: no fetch, no remote, and no media payload.

use super::{Diagnostic, Endpoint, cli_legacy, cli_row_key, local_project_path};
use crate::cli_row_key::render_key;
use orna_base::hex;

use orna_repository::{
    AdmittedRow, FormatContextError, GitCommitRef, KeyRange, NativeGraphContext, Repository,
    RepositoryFormatContext, RepositoryReadScope, TypedKey,
};
use orna_runtime::TableObjectId;
use orna_value::{BlobMetadata, ROV3_BLOB_TAG, Raw, decode_canonical_storage_value};

use orna_repository::relation_admission::CommittedRekey;
use std::collections::{BTreeMap, BTreeSet};

/// Most revisions one history listing reports when `--limit` is not given.
const DEFAULT_HISTORY_LIMIT: usize = 64;
/// Largest `--limit` accepted; matches the repository walk bound.
const MAX_HISTORY_LIMIT: usize = 4096;
/// Most rows one diff page reads; matches the repository range bound
/// (`MAX_ROW_RANGE_LIMIT`). A relation larger than one page is walked page by
/// page so the comparison never materialises an unbounded range.
const MAX_DIFF_PAGE: usize = 256;

/// How a legacy `history` invocation names the table of a loose row.
const LEGACY_HELP: &str = "name the table of a legacy loose row by its key-path directory, such as `contacts/Contact`, with `--at` naming the format-1/2 snapshot";

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
    key: Option<&'a str>,
    at: Option<&'a str>,
    limit: usize,
    since: Option<&'a str>,
    format: HistoryFormat,
    reverse: bool,
    quiet: bool,
    count: bool,
    author: Option<&'a str>,
    /// `--diff <a> <b>`: the two snapshots to compare instead of listing one
    /// row's revisions.
    diff: Option<(&'a str, &'a str)>,
}

use crate::cli_options::Words;

/// The value one option consumes, or the option's own usage diagnostic.
fn option_value<'a>(
    words: &mut Words<'a>,
    message: &'static str,
    usage: &'static str,
) -> Result<&'a str, Diagnostic> {
    crate::cli_options::value(words, history_error, message, usage)
}

/// `--limit <1..=4096>`.
fn limit_option(words: &mut Words<'_>) -> Result<usize, Diagnostic> {
    let value = option_value(words, "--limit needs a value", "usage: --limit <1..=4096>")?;
    value
        .parse::<usize>()
        .ok()
        .filter(|limit| (1..=MAX_HISTORY_LIMIT).contains(limit))
        .ok_or_else(|| history_error("--limit is not in 1..=4096", format!("got {value:?}")))
}

/// `--since <commit-hex>`.
fn since_option<'a>(words: &mut Words<'a>) -> Result<&'a str, Diagnostic> {
    option_value(
        words,
        "--since needs a commit id",
        "usage: --since <commit-hex>",
    )
}

/// `--at <selector>`: a listing names one snapshot.
fn at_option<'a>(words: &mut Words<'a>, current: Option<&'a str>) -> Result<&'a str, Diagnostic> {
    let value = option_value(words, "--at needs a value", "usage: --at <selector>")?;
    if current.is_some() {
        return Err(history_error(
            "History names one snapshot",
            "usage: --at <selector>",
        ));
    }
    Ok(value)
}

/// `--diff <from-selector> <to-selector>`: a comparison names one pair.
fn diff_option<'a>(
    words: &mut Words<'a>,
    current: Option<(&'a str, &'a str)>,
) -> Result<(&'a str, &'a str), Diagnostic> {
    let from = option_value(
        words,
        "--diff needs two snapshots",
        "usage: --diff <from-selector> <to-selector>",
    )?;
    let to = option_value(
        words,
        "--diff needs two snapshots",
        "usage: --diff <from-selector> <to-selector>",
    )?;
    if current.is_some() {
        return Err(history_error(
            "History compares one pair of snapshots",
            "usage: --diff <from-selector> <to-selector>",
        ));
    }
    Ok((from, to))
}

/// `--author <substring>`.
fn author_option<'a>(words: &mut Words<'a>) -> Result<&'a str, Diagnostic> {
    option_value(
        words,
        "--author needs a value",
        "usage: --author <substring>",
    )
}

/// `--format <human|json>`.
fn format_option(words: &mut Words<'_>) -> Result<HistoryFormat, Diagnostic> {
    let value = option_value(
        words,
        "--format needs a value",
        "usage: --format <human|json>",
    )?;
    match value {
        "human" => Ok(HistoryFormat::Human),
        "json" => Ok(HistoryFormat::Json),
        _ => Err(history_error(
            "--format is not human or json",
            format!("got {value:?}"),
        )),
    }
}

/// The relation and optional row key; `--diff` names the relation and both
/// snapshots, so no row key is read and the comparison covers every row.
fn relation_and_key<'a>(
    diff: Option<(&'a str, &'a str)>,
    positional: &[&'a str],
) -> Result<(&'a str, Option<&'a str>), Diagnostic> {
    match (diff, positional.split_first()) {
        (Some(_), Some((relation, []))) => Ok((*relation, None)),
        (Some((_, _)), _) => Err(history_error(
            "History expects a relation for --diff",
            "usage: orna history <relation-hex> --diff <from-selector> <to-selector>",
        )),
        (None, Some((relation, [key]))) => Ok((*relation, Some(*key))),
        (None, _) => Err(history_error(
            "History expects a relation and a row key",
            "usage: orna history <relation-hex> <key> [--at SELECTOR] [--limit N] [--since <commit-hex>]",
        )),
    }
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
    let mut diff = None;
    let mut words = crate::cli_options::words(arguments);
    while let Some(word) = words.next() {
        match word {
            "--limit" => limit = limit_option(&mut words)?,
            "--since" => since = Some(since_option(&mut words)?),
            "--at" => at = Some(at_option(&mut words, at)?),
            "--reverse" => reverse = true,
            "--quiet" => quiet = true,
            "--count" => count = true,
            "--diff" => diff = Some(diff_option(&mut words, diff)?),
            "--author" => author = Some(author_option(&mut words)?),
            "--format" => format = format_option(&mut words)?,
            flag if flag.starts_with("--") => {
                return Err(history_error(
                    "Unknown history flag",
                    format!(
                        "got {flag:?}; accepted: --at, --diff, --limit, --since, --format, --reverse, --author, --count, --quiet"
                    ),
                ));
            }
            _ => positional.push(word),
        }
    }
    let (relation, key) = relation_and_key(diff, &positional)?;
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
        diff,
    })
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let path = local_project_path(endpoint)?;
    let repository = Repository::discover(path)
        .map_err(|error| history_error("Repository could not be opened", format!("{error:?}")))?;
    if let Some((from, to)) = options.diff {
        let relation = parse_relation_id(options.relation)?;
        return run_diff(&repository, path, relation, from, to, options.format);
    }
    // `parse_options` yields a key for every invocation without `--diff`.
    let key = options
        .key
        .expect("a history read without --diff names one row key");
    // `--at` pins one named snapshot. The selector is resolved exactly once
    // here; the row map, the read and the revision walk all come from the
    // commit it named, so a branch that advances during the read never changes
    // the answer. Without `--at` the walk starts at the current HEAD.
    let (format, start, commit) = resolve_walk(&repository, path, options.at)?;
    // The snapshot is admitted before any revision is listed: a format-3
    // snapshot that tracks a loose row file is the mixed layout project
    // admission refuses (ORNA-META-001, ORNA-META-002), and a history answered
    // from the store would describe rows whose row files the same snapshot
    // carries.
    cli_legacy::admit_snapshot(&repository, commit.as_ref())?;
    // A format-1/2 pin is a read-only compatibility input: it carries no native
    // `.orna/store`, so it has no row map to walk. A loose row's revisions are
    // its committed units, read through the versioned legacy reader
    // (ORNA-UPGRADE-001, ORNA-UPGRADE-002, ORNA-ROW-013).
    if format.is_read_only() {
        let Some(commit) = commit.as_ref() else {
            return Err(history_error(
                "Snapshot could not be admitted",
                "--at names a read-only compatibility snapshot with no committed project source",
            ));
        };
        return run_legacy(&options, &repository, commit, &start, key);
    }
    let relation = parse_relation_id(options.relation)?;
    let row_map = format
        .load_row_map(relation)
        .map_err(|error| history_error("Row map could not be loaded", format!("{error:?}")))?;
    let graph = format
        .open_native_graph(&row_map)
        .map_err(|error| history_error("Native graph could not be opened", format!("{error:?}")))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| history_error("Read scope could not be opened", format!("{error:?}")))?;
    // The key is one canonical Orna key expression (ORNA-CLI-005), and the
    // same spelling names both a text key and the bytes that spell it: a row
    // committed from imported media is keyed by the bytes the capture was
    // given, while a row written by hand may be keyed by canonical text. Both
    // are bounded point reads of the committed row map, so the row that answers
    // is a real committed row rather than a reinterpretation of the argument.
    let row = committed_row(&graph, &scope, key)?;
    let walk = walk_bound(&options);
    let mut revisions = native_revisions(&graph, &scope, &row, &start, walk)?;
    finish(&options, &mut revisions, walk)
}

/// The format context, walk start and pinned commit a listing reads.
///
/// One guard, one resolution: `pin_snapshot` refuses an ambiguous `--at` name
/// before it resolves, so `--at` and `--diff` cannot disagree.
fn resolve_walk<'a>(
    repository: &Repository,
    path: &'a str,
    at: Option<&'a str>,
) -> Result<(RepositoryFormatContext, String, Option<GitCommitRef>), Diagnostic> {
    let Some(selector) = at else {
        let commit = repository.head().map_err(|error| {
            history_error("Snapshot could not be resolved", format!("{error:?}"))
        })?;
        let format = repository.open_format_context().map_err(|error| {
            history_error("Format context could not be opened", format!("{error:?}"))
        })?;
        return Ok((format, "HEAD".to_owned(), commit));
    };
    let (commit, format) = pin_snapshot(repository, path, selector)?;
    let start = commit.as_str().to_owned();
    Ok((format, start, Some(commit)))
}

/// The one committed row `key` names, trying the text spelling and then the
/// bytes that spell it.
fn committed_row(
    graph: &NativeGraphContext,
    scope: &RepositoryReadScope,
    key: &str,
) -> Result<AdmittedRow, Diagnostic> {
    let mut row = None;
    for candidate in cli_row_key::key_candidates(key, None)? {
        row = graph
            .lookup_row(&candidate, scope)
            .map_err(|error| history_error("Row lookup failed", format!("{error:?}")))?;
        if row.is_some() {
            break;
        }
    }
    row.ok_or_else(|| history_error("Row is not committed", format!("no row with key {key:?}")))
}

/// How far the walk reads: `--since` and `--author` need the whole bound so
/// their commit and author can be reached, otherwise only what is printed.
fn walk_bound(options: &HistoryOptions<'_>) -> usize {
    if options.since.is_some() || options.author.is_some() {
        MAX_HISTORY_LIMIT
    } else {
        options.limit
    }
}

/// One row's revisions through the committed row map walk.
fn native_revisions(
    graph: &NativeGraphContext,
    scope: &RepositoryReadScope,
    row: &AdmittedRow,
    start: &str,
    walk: usize,
) -> Result<Vec<HistoryRevision>, Diagnostic> {
    Ok(graph
        .list_row_revisions(row, start, walk, scope)
        .map_err(|error| {
            history_error("Revision history could not be listed", format!("{error:?}"))
        })?
        .into_iter()
        .map(|revision| HistoryRevision {
            commit: revision.commit().to_hex(),
            tree: revision.tree().to_hex(),
            present: revision.present(),
            author: revision.author().to_owned(),
            committed_at: revision.committed_at(),
            migration: revision.migration().map(str::to_owned),
        })
        .collect())
}

/// One revision as either row walk reports it.
///
/// The native walk reports a row's presence as the reachability of its
/// descriptor tree, and the legacy walk as the commit's own row unit; both
/// become this record so filtering and printing happen once.
struct HistoryRevision {
    commit: String,
    tree: String,
    present: bool,
    author: String,
    committed_at: u64,
    migration: Option<String>,
}

/// Applies `--since`, `--author`, `--limit`, `--reverse` and `--count` to a
/// walk's revisions, then prints them in the requested shape.
fn finish(
    options: &HistoryOptions<'_>,
    revisions: &mut Vec<HistoryRevision>,
    walk: usize,
) -> Result<(), Diagnostic> {
    if let Some(since) = options.since {
        let position = revisions
            .iter()
            .position(|revision| revision.commit == since)
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
        revisions.retain(|revision| revision.author.contains(author));
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
                        revision.commit.clone(),
                        revision.tree.clone(),
                        revision.present,
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
                        &revision.commit,
                        &revision.tree,
                        revision.present,
                        &revision.author,
                        revision.committed_at,
                        revision.migration.as_deref(),
                    )
                })
                .collect();
            println!("{}", serde_json::Value::Array(entries));
        }
    }
    Ok(())
}

/// Lists one row's revisions at a format-1/2 pin through the versioned legacy
/// reader (ORNA-UPGRADE-001, ORNA-UPGRADE-002, ORNA-ROW-013).
///
/// A loose row unit is a committed file, so its revisions are the commits whose
/// tree does or does not carry that file, which is the same coordinate the
/// native walk reports for a format-3 row. A compact row has no such file: its
/// value lives inside committed segments, so it has no revision the commit
/// graph alone can show, and the command says so instead of reporting an empty
/// history.
fn run_legacy(
    options: &HistoryOptions<'_>,
    repository: &Repository,
    commit: &GitCommitRef,
    start: &str,
    key: &str,
) -> Result<(), Diagnostic> {
    let table_path = match cli_legacy::legacy_address(options.relation, LEGACY_HELP)? {
        orna_application::LegacyTableAddress::TablePath(path) => path,
        orna_application::LegacyTableAddress::Relation(relation) => {
            // A compact table the snapshot does not hold is reported as a
            // missing table; one it does hold has no revison list this walk can
            // follow, because its rows live inside committed segments.
            if !repository
                .read_compact_manifest_inventory(commit)
                .map_err(|error| {
                    cli_legacy::legacy_read_error(
                        &orna_application::LegacyReadError::Repository(error),
                        LEGACY_HELP,
                    )
                })?
                .contains_key(&orna_repository::Uuid::from_bytes(relation))
            {
                return Err(cli_legacy::legacy_read_error(
                    &orna_application::LegacyReadError::UnknownRelation(relation),
                    LEGACY_HELP,
                ));
            }
            return Err(history_error(
                "Legacy compact rows have no file history",
                "a compact row is stored inside committed segments, not in a row unit the commit graph can follow; read its value with `orna query`, or migrate the snapshot to format 3 (`orna migrate copy`) and read its history there",
            ));
        }
    };
    let row_path = legacy_row_path(repository, commit, &table_path, key)?;
    // `--since` needs the walk far enough to reach its commit, so walk the
    // full bound and cut afterwards; otherwise walk only what is printed.
    let walk = walk_bound(options);
    let mut revisions =
        orna_application::read_legacy_row_revisions(repository, start, &row_path, walk)
            .map_err(|error| cli_legacy::legacy_read_error(&error, LEGACY_HELP))?
            .into_iter()
            .map(|revision| HistoryRevision {
                commit: revision.snapshot().commit().as_str().to_owned(),
                tree: revision.snapshot().tree().to_owned(),
                present: revision.present(),
                author: revision.snapshot().author().to_owned(),
                committed_at: revision.snapshot().committed_at(),
                migration: revision.snapshot().migration().map(str::to_owned),
            })
            .collect::<Vec<_>>();
    finish(options, &mut revisions, walk)
}

/// The loose row unit `key` names under `table_path`. The key is tried as text
/// and then as the bytes that spell it, the same two spellings the native point
/// read uses.
fn legacy_row_path(
    repository: &Repository,
    commit: &GitCommitRef,
    table_path: &str,
    key: &str,
) -> Result<String, Diagnostic> {
    let mut row_path = None;
    for candidate in cli_row_key::key_candidates(key, None)? {
        row_path =
            orna_application::legacy_loose_row_path(repository, commit, table_path, &candidate)
                .map_err(|error| cli_legacy::legacy_read_error(&error, LEGACY_HELP))?;
        if row_path.is_some() {
            break;
        }
    }
    row_path
        .ok_or_else(|| history_error("Row is not committed", format!("no row with key {key:?}")))
}

/// One revision as a JSON object: commit, root tree, presence, author and the
/// commit time in seconds since the Unix epoch, plus the migration coordinate
/// when the commit is the journalled migration commit.
///
/// `migration` is present only on commits that carry one, so the documented
/// shape is unchanged for every ordinary revision.
fn revision_json(
    commit: &str,
    tree: &str,
    present: bool,
    author: &str,
    committed_at: u64,
    migration: Option<&str>,
) -> serde_json::Value {
    let mut object = serde_json::json!({
        "commit": commit,
        "tree": tree,
        "present": present,
        "author": author,
        "committed": committed_at,
    });
    if let Some(migration) = migration {
        object["migration"] = serde_json::Value::String(migration.to_owned());
    }
    object
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

fn parse_relation_id(value: &str) -> Result<[u8; 16], Diagnostic> {
    cli_row_key::parse_relation_id(value, history_error)
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
/// Resolves and pins one named snapshot exactly once, refusing a bare name that
/// more than one branch, tag or remote branch carries. `--at` and both `--diff`
/// endpoints pin through here, so an identical-prefix name is refused wherever
/// it appears rather than resolved to whichever ref Git lists first.
fn pin_snapshot(
    repository: &Repository,
    directory: &str,
    selector: &str,
) -> Result<(GitCommitRef, RepositoryFormatContext), Diagnostic> {
    if bare_ref_selector_is_ambiguous(
        directory,
        selector,
        "run `orna history <relation-hex> <key>` inside an initialized repository",
    )? {
        return Err(Diagnostic::target_with_detail(
            "E2000",
            "Snapshot name is ambiguous",
            "use the full ref name (`refs/heads/NAME`, `refs/tags/NAME`, \
             `refs/remotes/ORIGIN/NAME`) or the commit id `orna history` lists",
            format!("{selector:?} names more than one branch, tag or remote branch"),
        ));
    }
    let (commit, format) = repository
        .open_pinned_format_context_with_commit(selector)
        .map_err(|error| {
            let message = match error {
                FormatContextError::SnapshotUnavailable | FormatContextError::SnapshotInvalid => {
                    "Snapshot could not be resolved"
                }
                _ => "Snapshot could not be pinned",
            };
            history_error(message, format!("{selector:?}: {error:?}"))
        })?;
    Ok((commit, format))
}

/// Only exact matches count. `git for-each-ref <pattern>` matches by prefix, so
/// `refs/remotes/origin` also lists `refs/remotes/origin/main`; those are
/// different names and must not be read as a collision.
pub(super) fn bare_ref_selector_is_ambiguous(
    directory: &str,
    selector: &str,
    help: &'static str,
) -> Result<bool, Diagnostic> {
    // Only a name that could collide is checked. A commit id, a `HEAD~2`-style
    // expression, an `@{...}` or `:` revision syntax, a `a..b` range or a full `refs/`
    // path names one object by construction. Slashed names such as
    // `origin/main` are *not* skipped: a branch and a remote-tracking branch
    // can share one.
    if selector.is_empty()
        || selector.starts_with('-')
        || selector.starts_with("refs/")
        || selector == "HEAD"
        || selector == "@"
        || selector.contains([':', '^', '~'])
        || selector.contains("@{")
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
            Diagnostic::target_with_detail(
                "E2000",
                "Git could not be started",
                help,
                format!("check that Git is installed and available on PATH: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(Diagnostic::target_with_detail(
            "E2000",
            "Git could not list the snapshot names",
            help,
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

/// The source-qualified value coordinate of one field.
#[derive(Clone, Debug, Eq, PartialEq)]
enum FieldContent {
    Value(Raw),
    Blob { length: u64, sha256: [u8; 32] },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BlobAnnotation {
    media_type: String,
    suffix: Option<String>,
    length: u64,
    sha256: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HistoryField {
    id: [u8; 16],
    index: usize,
    content: FieldContent,
    annotation: Option<BlobAnnotation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HistoryRow {
    key: TypedKey,
    fields: Vec<HistoryField>,
}

/// One row's change between two pinned snapshots.
enum RowChange {
    Added(HistoryRow),
    Removed(HistoryRow),
    Changed {
        before: HistoryRow,
        after: HistoryRow,
    },
    Rekeyed {
        before: HistoryRow,
        after: HistoryRow,
    },
}

fn history_row(
    row: &AdmittedRow,
    fields: &orna_repository::AdmittedRowFields,
) -> Result<HistoryRow, Diagnostic> {
    let decoded = decode_canonical_storage_value(fields.canonical_fields()).map_err(|error| {
        history_error(
            "Canonical row fields could not be decoded",
            format!("{error:?}"),
        )
    })?;
    let Raw::Array(values) = &decoded else {
        return Err(history_error(
            "Canonical row fields are not a tuple",
            "the admitted source decoder returned a non-array value",
        ));
    };
    if values.len() != fields.fields().len() {
        return Err(history_error(
            "Canonical row field count does not match schema",
            format!(
                "{} values, {} source fields",
                values.len(),
                fields.fields().len()
            ),
        ));
    }
    let mut result = Vec::with_capacity(values.len());
    for (index, (value, field)) in values.iter().zip(fields.fields()).enumerate() {
        let metadata = field.blob_metadata();
        let content = match metadata {
            Some(metadata) => {
                let value = match value {
                    Raw::Tag(60013, payload) => {
                        let Raw::Array(parts) = payload.as_ref() else {
                            return Err(history_error(
                                "Decoded Blob field is not canonical",
                                format!(
                                    "source field {} has an invalid Option encoding",
                                    hex(field.field_id())
                                ),
                            ));
                        };
                        match parts.as_slice() {
                            [Raw::Int(flag), value] if *flag == 1.into() => value,
                            _ => {
                                return Err(history_error(
                                    "Decoded Blob field is not canonical",
                                    format!(
                                        "source field {} does not contain Some(Blob)",
                                        hex(field.field_id())
                                    ),
                                ));
                            }
                        }
                    }
                    value => value,
                };
                if !matches!(value, Raw::Tag(ROV3_BLOB_TAG, _)) {
                    return Err(history_error(
                        "Decoded Blob field is not canonical",
                        format!(
                            "source field {} does not contain an ROV-3 value",
                            hex(field.field_id())
                        ),
                    ));
                }
                FieldContent::Blob {
                    length: metadata.length(),
                    sha256: metadata.sha256(),
                }
            }
            None => FieldContent::Value(value.clone()),
        };
        let annotation = metadata.map(|metadata: &BlobMetadata| BlobAnnotation {
            media_type: metadata.media_type().to_owned(),
            suffix: metadata.suffix().map(str::to_owned),
            length: metadata.length(),
            sha256: metadata.sha256(),
        });
        result.push(HistoryField {
            id: *field.field_id(),
            index,
            content,
            annotation,
        });
    }
    result.sort_by_key(|field| field.id);
    Ok(HistoryRow {
        key: row.key().clone(),
        fields: result,
    })
}

fn annotations_of(row: &HistoryRow) -> Vec<serde_json::Value> {
    row.fields
        .iter()
        .filter_map(|field| {
            field.annotation.as_ref().map(|annotation| {
                serde_json::json!({
                    "field": field.index,
                    "field_id": hex(&field.id),
                    "media_type": annotation.media_type,
                    "suffix": annotation.suffix,
                    "length": annotation.length,
                    "sha256": hex(&annotation.sha256),
                })
            })
        })
        .collect()
}

fn content_changed(before: &HistoryRow, after: &HistoryRow) -> bool {
    before
        .fields
        .iter()
        .map(|field| (field.id, &field.content))
        .ne(after.fields.iter().map(|field| (field.id, &field.content)))
}

fn annotations_changed(before: &HistoryRow, after: &HistoryRow) -> bool {
    before
        .fields
        .iter()
        .filter_map(|field| {
            field
                .annotation
                .as_ref()
                .map(|annotation| (field.id, &annotation.media_type, &annotation.suffix))
        })
        .ne(after.fields.iter().filter_map(|field| {
            field
                .annotation
                .as_ref()
                .map(|annotation| (field.id, &annotation.media_type, &annotation.suffix))
        }))
}

fn row_changed(before: &HistoryRow, after: &HistoryRow) -> bool {
    content_changed(before, after) || annotations_changed(before, after)
}

/// Both endpoints of one `--diff`: each pinned once, with the table schema of
/// its own snapshot and the labels the report prints.
struct DiffEndpoints {
    before_format: RepositoryFormatContext,
    after_format: RepositoryFormatContext,
    before_schema: orna_project::ResolvedTableSchema,
    after_schema: orna_project::ResolvedTableSchema,
    from_commit: GitCommitRef,
    to_commit: GitCommitRef,
    from_label: String,
    to_label: String,
}

/// Reports what changed in one relation between two named snapshots.
///
/// Both endpoints are pinned through the same repository-owned historical
/// snapshot context used by `--at`, so each side is one immutable read of the
/// local store. Nothing here contacts a remote, fetches an object, or opens a
/// media payload: the comparison is over row keys, the row's own field tuple,
/// and the Blob annotation descriptors that tuple carries. An annotation that
/// moved between the two sides is therefore reported as its own coordinate
/// rather than as a change to the row's data.
fn run_diff(
    repository: &Repository,
    directory: &str,
    relation: [u8; 16],
    from: &str,
    to: &str,
    format: HistoryFormat,
) -> Result<(), Diagnostic> {
    let endpoints = diff_endpoints(repository, directory, relation, from, to)?;
    let (before, before_read) = read_relation_rows(
        &endpoints.before_format,
        relation,
        endpoints.before_schema.binding(),
    )?;
    let (after, after_read) = read_relation_rows(
        &endpoints.after_format,
        relation,
        endpoints.after_schema.binding(),
    )?;
    let cost = ReadCost {
        payload_bytes_read: before_read.payload_bytes_read + after_read.payload_bytes_read,
        objects_read: before_read.objects_read + after_read.objects_read,
    };
    let window = rekey_window(
        repository,
        &endpoints.from_commit,
        &endpoints.to_commit,
        relation,
    )?;
    let (before, after, paired) = pair_rekeys(before, after, &window);
    let mut changes = compare_rows(before, after);
    changes.extend(
        paired
            .into_iter()
            .map(|(before, after)| RowChange::Rekeyed { before, after }),
    );
    report_diff(
        format,
        &endpoints.from_label,
        &endpoints.to_label,
        &changes,
        cost,
    );
    Ok(())
}

/// Pins both `--diff` endpoints and refuses a pair this comparison cannot join:
/// a legacy input, two databases, or a schema that is not from the exact
/// endpoint.
fn diff_endpoints(
    repository: &Repository,
    directory: &str,
    relation: [u8; 16],
    from: &str,
    to: &str,
) -> Result<DiffEndpoints, Diagnostic> {
    // Each endpoint is pinned once, through the same ambiguity guard `--at`
    // uses, so an identical-prefix name is refused rather than resolved to
    // whichever ref `git for-each-ref` lists first.
    let (from_commit, before_format) = pin_snapshot(repository, directory, from)?;
    let (to_commit, after_format) = pin_snapshot(repository, directory, to)?;
    // A format-1/2 endpoint is a read-only compatibility input with no native
    // `.orna/store`, so it has no row map to compare. The legacy reader answers
    // one table's rows and one loose row's revisions, not a two-ended row diff,
    // so `--diff` names that limit instead of reporting the format-3 store
    // seam's generic failure.
    for (selector, format) in [(from, &before_format), (to, &after_format)] {
        if format.is_read_only() {
            return Err(history_error(
                "Snapshot is a legacy format-1/2 input",
                format!(
                    "{selector:?} names a read-only compatibility snapshot with no native row map; `--diff` compares two format-3 row maps, so read either side with `orna query` or migrate the snapshot to format 3 (`orna migrate copy`)"
                ),
            ));
        }
    }
    // Both endpoints are resolved inside this repository, but a commit can
    // record a database identity other than the current one: a reinitialized
    // repository keeps its older commits reachable, so one repository can hold
    // snapshots of two databases. The format-3 row map is keyed by that
    // recorded identity, so comparing across two identities would join rows
    // that belong to different databases. Refuse instead of reporting
    // unrelated rows as changes.
    if before_format.database_id() != after_format.database_id() {
        return Err(history_error(
            "Snapshots belong to different repositories",
            format!(
                "{from:?} and {to:?} record different database identities; \
                 compare two snapshots of one repository"
            ),
        ));
    }
    let loader = orna_project::ProjectLoader::default();
    let before_schema = loader
        .resolve_committed_source_table_schema(
            repository,
            &from_commit,
            TableObjectId::new(relation),
        )
        .map_err(|error| {
            history_error(
                "Source table schema could not be resolved",
                format!("{from:?}: {error:?}"),
            )
        })?;
    let after_schema = loader
        .resolve_committed_source_table_schema(repository, &to_commit, TableObjectId::new(relation))
        .map_err(|error| {
            history_error(
                "Source table schema could not be resolved",
                format!("{to:?}: {error:?}"),
            )
        })?;
    if before_schema.source_commit() != &from_commit
        || after_schema.source_commit() != &to_commit
        || before_schema.table_object_id() != TableObjectId::new(relation)
        || after_schema.table_object_id() != TableObjectId::new(relation)
        || before_format
            .database_id()
            .is_none_or(|id| before_schema.database_id() != id.as_bytes())
        || after_format
            .database_id()
            .is_none_or(|id| after_schema.database_id() != id.as_bytes())
        || before_schema.snapshot_id() != &before_format.snapshot_pin().snapshot_id()
        || after_schema.snapshot_id() != &after_format.snapshot_pin().snapshot_id()
    {
        return Err(history_error(
            "Source table schema snapshot mismatch",
            "source table identity and schema must come from the exact history endpoint",
        ));
    }
    let from_label = format!("{from:?} ({})", from_commit.as_str());
    let to_label = format!("{to:?} ({})", to_commit.as_str());
    Ok(DiffEndpoints {
        before_format,
        after_format,
        before_schema,
        after_schema,
        from_commit,
        to_commit,
        from_label,
        to_label,
    })
}

/// Prints one comparison in the requested shape with its measured read cost.
fn report_diff(
    format: HistoryFormat,
    from_label: &str,
    to_label: &str,
    changes: &[RowChange],
    cost: ReadCost,
) {
    let (added, updated, removed, rekeyed) = counts(changes);
    let payload_bytes_read = cost.payload_bytes_read;
    let native_objects_read = cost.objects_read;
    match format {
        HistoryFormat::Human => {
            for change in changes {
                println!("{}", human_change(change));
            }
            println!(
                "{added} added; {updated} changed; {removed} removed; {rekeyed} rekeyed; {} rows in {to_label}",
                changes
                    .iter()
                    .filter(|change| !matches!(change, RowChange::Removed(_)))
                    .count(),
            );
            println!(
                "media payload bytes read: {payload_bytes_read}; native objects read: {native_objects_read}"
            );
        }
        HistoryFormat::Json => {
            let entries: Vec<serde_json::Value> = changes.iter().map(change_json).collect();
            println!(
                "{}",
                serde_json::json!({
                    "from": from_label,
                    "to": to_label,
                    "added": added,
                    "changed": updated,
                    "removed": removed,
                    "rekeyed": rekeyed,
                    "media_payload_bytes_read": payload_bytes_read,
                    "native_objects_read": native_objects_read,
                    "changes": entries,
                })
            );
        }
    }
}

/// The measured cost of one pinned relation read: the media payload bytes and
/// native objects the read touched.
#[derive(Clone, Copy)]
struct ReadCost {
    payload_bytes_read: u64,
    objects_read: u64,
}

/// Reads every committed row of one relation at one already-pinned snapshot,
/// keyed by canonical key bytes so the two sides join on the committed row
/// identity.
///
/// The relation is walked one bounded page at a time; each page resumes at the
/// last key it read, and the map keys by canonical bytes so a boundary row seen
/// twice is stored once. The cost returned is the read scope's own accounting,
/// so a caller can report the payload bytes the comparison did not fetch.
fn read_relation_rows(
    format: &RepositoryFormatContext,
    relation: [u8; 16],
    binding: &orna_repository::CommittedTableBinding,
) -> Result<(BTreeMap<Vec<u8>, HistoryRow>, ReadCost), Diagnostic> {
    let row_map = format
        .load_row_map(relation)
        .map_err(|error| history_error("Row map could not be loaded", format!("{error:?}")))?;
    let graph = format
        .open_native_graph(&row_map)
        .map_err(|error| history_error("Native graph could not be opened", format!("{error:?}")))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| history_error("Read scope could not be opened", format!("{error:?}")))?;
    let mut rows: BTreeMap<Vec<u8>, HistoryRow> = BTreeMap::new();
    let mut cursor: Option<TypedKey> = None;
    loop {
        let range = KeyRange::new(cursor.clone(), None, MAX_DIFF_PAGE)
            .map_err(|error| history_error("Row range is invalid", format!("{error:?}")))?;
        let page = graph
            .range_rows(&range, &scope)
            .map_err(|error| history_error("Row range could not be read", format!("{error:?}")))?;
        let read = page.len();
        let mut last = None;
        for row in page {
            last = Some(row.key().clone());
            let key = row.key().canonical_bytes().map_err(|error| {
                history_error("Row key could not be encoded", format!("{error:?}"))
            })?;
            let fields = graph
                .decode_admitted_row_fields(&row, binding, &scope)
                .map_err(|error| {
                    history_error(
                        "Source-qualified row fields could not be decoded",
                        format!("{error:?}"),
                    )
                })?;
            rows.insert(key, history_row(&row, &fields)?);
        }
        if read < MAX_DIFF_PAGE {
            return Ok((
                rows,
                ReadCost {
                    payload_bytes_read: scope.payload_bytes_read(),
                    objects_read: scope.objects_read(),
                },
            ));
        }
        match last {
            Some(last) => cursor = Some(last),
            None => {
                return Ok((
                    rows,
                    ReadCost {
                        payload_bytes_read: scope.payload_bytes_read(),
                        objects_read: scope.objects_read(),
                    },
                ));
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
/// Joins the two pinned reads by canonical key into one ordered change list.
fn compare_rows(
    before: BTreeMap<Vec<u8>, HistoryRow>,
    after: BTreeMap<Vec<u8>, HistoryRow>,
) -> Vec<RowChange> {
    let mut changes = Vec::new();
    for (key, old) in &before {
        match after.get(key) {
            None => changes.push(RowChange::Removed(old.clone())),
            Some(new) if row_changed(old, new) => changes.push(RowChange::Changed {
                before: old.clone(),
                after: new.clone(),
            }),
            Some(_) => {}
        }
    }
    for (key, new) in &after {
        if !before.contains_key(key) {
            changes.push(RowChange::Added(new.clone()));
        }
    }
    changes
}

#[allow(clippy::needless_pass_by_value)]
/// One change as a JSON object. A `changed` entry separates the two
/// coordinates so a content move and an annotation move are told apart.
fn change_json(change: &RowChange) -> serde_json::Value {
    match change {
        RowChange::Added(row) => serde_json::json!({
            "change": "added",
            "key": render_key(&row.key),
            "annotations": annotations_of(row),
        }),
        RowChange::Removed(row) => serde_json::json!({
            "change": "removed",
            "key": render_key(&row.key),
            "annotations": annotations_of(row),
        }),
        RowChange::Changed { before, after } => serde_json::json!({
            "change": "changed",
            "key": render_key(&after.key),
            "content_changed": content_changed(before, after),
            "before": { "annotations": annotations_of(before) },
            "after": { "annotations": annotations_of(after) },
        }),
        RowChange::Rekeyed { before, after } => serde_json::json!({
            "change": "rekeyed",
            "key": render_key(&after.key),
            "from_key": render_key(&before.key),
            "content_changed": content_changed(before, after),
            "before": { "annotations": annotations_of(before) },
            "after": { "annotations": annotations_of(after) },
        }),
    }
}

/// One change as a human line. A changed row names which coordinate moved so a
/// re-annotation is not read as a data change.
fn human_change(change: &RowChange) -> String {
    match change {
        RowChange::Added(row) => format!("added   {}", render_key(&row.key)),
        RowChange::Removed(row) => format!("removed {}", render_key(&row.key)),
        RowChange::Changed { before, after } => format!(
            "changed {} (content={}, annotation={})",
            render_key(&after.key),
            content_changed(before, after),
            annotations_changed(before, after),
        ),
        RowChange::Rekeyed { before, after } => format!(
            "rekeyed {} -> {} (content={}, annotation={})",
            render_key(&before.key),
            render_key(&after.key),
            content_changed(before, after),
            annotations_changed(before, after),
        ),
    }
}

/// Counts the changes as `(added, changed, removed, rekeyed)`.
fn counts(changes: &[RowChange]) -> (usize, usize, usize, usize) {
    let mut added = 0;
    let mut updated = 0;
    let mut removed = 0;
    let mut rekeyed = 0;
    for change in changes {
        match change {
            RowChange::Added(_) => added += 1,
            RowChange::Changed { .. } => updated += 1,
            RowChange::Removed(_) => removed += 1,
            RowChange::Rekeyed { .. } => rekeyed += 1,
        }
    }
    (added, updated, removed, rekeyed)
}

#[allow(clippy::type_complexity)]
/// The row moves the committed rekeys of `relation` make between the two
/// pinned snapshots, as `(old, new)` key pairs in the order that leads from
/// `from` to `to`. Each snapshot's log holds every rekey of the history it
/// descends from. When `to`'s log extends `from`'s, the window is the rekeys
/// appended since; when `from`'s extends `to`'s (a reverse diff), it is those
/// rekeys undone, newest first. Otherwise the two histories prove no shared
/// identity, so the window is empty and the diff reports conservative
/// removals and additions (ORNA-SYS-051).
fn rekey_window(
    repository: &Repository,
    from: &GitCommitRef,
    to: &GitCommitRef,
    relation: [u8; 16],
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, Diagnostic> {
    let from_log = repository
        .read_committed_rekeys(from)
        .map_err(|error| history_error("Rekey log could not be read", format!("{error:?}")))?;
    let to_log = repository
        .read_committed_rekeys(to)
        .map_err(|error| history_error("Rekey log could not be read", format!("{error:?}")))?;
    let in_relation = |rekey: &&CommittedRekey| rekey.relation_id() == &relation;
    if to_log.starts_with(&from_log) {
        return Ok(to_log[from_log.len()..]
            .iter()
            .filter(in_relation)
            .map(|rekey| (rekey.old_key().to_vec(), rekey.new_key().to_vec()))
            .collect());
    }
    if from_log.starts_with(&to_log) {
        return Ok(from_log[to_log.len()..]
            .iter()
            .rev()
            .filter(in_relation)
            .map(|rekey| (rekey.new_key().to_vec(), rekey.old_key().to_vec()))
            .collect());
    }
    Ok(Vec::new())
}

#[allow(clippy::type_complexity)]
/// Pairs each row the window moved, so the move reads as one rekeyed change
/// instead of a removal plus an addition. The moves are folded first: each
/// key a chain ends on is traced back to the key it started from, so `a -> b`
/// then `b -> c` pairs `before[a]` with `after[c]`, and a swap through a
/// temporary key pairs both rows crosswise. A row whose start or end key is
/// absent from its side stays in the comparison as the plain removal or
/// addition it is. A start key reused by a later insert proves no identity,
/// so every chain tracing back to it stays a removal plus an addition
/// (ORNA-SYS-051). The log records rekeys only, so a key a later commit
/// deleted and inserted again still reads as the end of its chain.
fn pair_rekeys(
    mut before: BTreeMap<Vec<u8>, HistoryRow>,
    mut after: BTreeMap<Vec<u8>, HistoryRow>,
    moves: &[(Vec<u8>, Vec<u8>)],
) -> (
    BTreeMap<Vec<u8>, HistoryRow>,
    BTreeMap<Vec<u8>, HistoryRow>,
    Vec<(HistoryRow, HistoryRow)>,
) {
    // The key each moved row started the window at, by the key it ends on.
    let mut origins = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    // Keys a rekey moved a row away from. A later move from one of them that
    // continues no chain moves a row inserted there afterwards, so that start
    // key is reused; this also covers two ends claiming one start key.
    let mut vacated = BTreeSet::<Vec<u8>>::new();
    let mut reused = BTreeSet::<Vec<u8>>::new();
    for (old, new) in moves {
        let origin = if let Some(origin) = origins.remove(old) {
            origin
        } else {
            if vacated.contains(old) {
                reused.insert(old.clone());
            }
            old.clone()
        };
        vacated.insert(old.clone());
        origins.insert(new.clone(), origin);
    }
    let mut pairs = Vec::new();
    for (end, origin) in origins {
        // A row moved away and back again has no net rekey to report, and a
        // reused start key cannot say which row an end continues.
        if end == origin || reused.contains(&origin) {
            continue;
        }
        let (Some(old), Some(new)) = (before.get(&origin), after.get(&end)) else {
            continue;
        };
        pairs.push((old.clone(), new.clone()));
        before.remove(&origin);
        after.remove(&end);
    }
    (before, after, pairs)
}

fn history_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    // The human renderer shows the title and help but not `detail`, so the
    // detail is carried in the title as well.
    let detail = detail.into();
    let mut diagnostic = Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna history <relation-hex> <key>` inside an initialized repository",
        detail.clone(),
    );
    diagnostic.title = std::borrow::Cow::Owned(format!("{title}: {detail}"));
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_HISTORY_LIMIT, HistoryFormat, HistoryRow, pair_rekeys, parse_options,
        parse_relation_id, render_key,
    };
    use crate::Exit;
    use orna_repository::TypedKey;
    use std::collections::BTreeMap;

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
        assert_eq!((parsed.relation, parsed.key), ("0102", Some("song")));
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
        let value = super::revision_json(
            "c0ffee",
            "7ree",
            true,
            "Ada <ada@example.test>",
            1_700_000_000,
            None,
        );
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["author", "commit", "committed", "present", "tree"]);
        assert_eq!(object["present"], serde_json::Value::Bool(true));
        assert_eq!(object["author"], "Ada <ada@example.test>");
        assert_eq!(object["committed"], 1_700_000_000);
    }

    #[test]
    fn revision_json_reports_the_migration_coordinate_only_on_the_migration_commit() {
        let value = super::revision_json(
            "c0ffee",
            "7ree",
            true,
            "Ada <ada@example.test>",
            1_700_000_000,
            Some("format-1-to-3"),
        );
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "author",
                "commit",
                "committed",
                "migration",
                "present",
                "tree"
            ],
            "the migration coordinate is an additional key, not a replaced one"
        );
        assert_eq!(object["migration"], "format-1-to-3");
        assert_eq!(object["commit"], "c0ffee");
    }

    #[test]
    fn revision_listing_round_trips_in_order_as_a_json_array() {
        let listing = serde_json::Value::Array(vec![
            super::revision_json(
                "aaa",
                "t1",
                true,
                "Ada <ada@example.test>",
                1_700_000_000,
                None,
            ),
            super::revision_json(
                "bbb",
                "t2",
                false,
                "Ada <ada@example.test>",
                1_700_000_001,
                None,
            ),
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
            (2, "0102", Some("song"))
        );
    }

    #[test]
    fn quiet_is_a_bare_flag_and_defaults_off() {
        let plain = words(&["0102", "song"]);
        assert!(!parse_options(&plain).unwrap().quiet);
        let flagged = words(&["0102", "--quiet", "song"]);
        assert!(parse_options(&flagged).unwrap().quiet);
    }

    #[test]
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

    #[test]
    fn rekey_pairing_folds_chains_and_swaps() {
        let key = |name: &str| TypedKey::Text(name.to_owned()).canonical_bytes().unwrap();
        let rows = |names: &[&str]| {
            names
                .iter()
                .map(|name| {
                    let row = HistoryRow {
                        key: TypedKey::Text((*name).to_owned()),
                        fields: Vec::new(),
                    };
                    (key(name), row)
                })
                .collect::<BTreeMap<_, _>>()
        };
        let moves = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(old, new)| (key(old), key(new)))
                .collect::<Vec<_>>()
        };
        let rekeyed = |paired: &[(HistoryRow, HistoryRow)]| {
            paired
                .iter()
                .map(|(old, new)| format!("{}->{}", render_key(&old.key), render_key(&new.key)))
                .collect::<Vec<_>>()
        };

        // A chain across publications reads as one rekey from start to end.
        let (before, after, paired) = pair_rekeys(
            rows(&["a"]),
            rows(&["c"]),
            &moves(&[("a", "b"), ("b", "c")]),
        );
        assert!(before.is_empty() && after.is_empty());
        assert_eq!(rekeyed(&paired), ["a->c"]);

        // A swap through a temporary key pairs both rows crosswise.
        let (before, after, paired) = pair_rekeys(
            rows(&["a", "b"]),
            rows(&["a", "b"]),
            &moves(&[("a", "t"), ("b", "a"), ("t", "b")]),
        );
        assert!(before.is_empty() && after.is_empty());
        assert_eq!(rekeyed(&paired), ["b->a", "a->b"]);

        // A row moved away and back again has no net rekey.
        let (before, after, paired) = pair_rekeys(
            rows(&["a"]),
            rows(&["a"]),
            &moves(&[("a", "t"), ("t", "a")]),
        );
        assert!(paired.is_empty());
        assert_eq!((before.len(), after.len()), (1, 1));

        // A chain whose end row is gone stays a plain removal.
        let (before, after, paired) = pair_rekeys(rows(&["a"]), rows(&[]), &moves(&[("a", "b")]));
        assert!(paired.is_empty());
        assert_eq!((before.len(), after.len()), (1, 0));

        // `m -> z`, a new row inserted at `m`, then `m -> c` with `c < z`:
        // the reused start key proves neither end's identity, so both stay
        // additions and the original row a removal.
        let (before, after, paired) = pair_rekeys(
            rows(&["m"]),
            rows(&["c", "z"]),
            &moves(&[("m", "z"), ("m", "c")]),
        );
        assert!(paired.is_empty());
        assert_eq!((before.len(), after.len()), (1, 2));

        // A row that returns to its start key and moves on is still tracked.
        let (before, after, paired) = pair_rekeys(
            rows(&["m"]),
            rows(&["c"]),
            &moves(&[("m", "z"), ("z", "m"), ("m", "c")]),
        );
        assert!(before.is_empty() && after.is_empty());
        assert_eq!(rekeyed(&paired), ["m->c"]);
    }
}
