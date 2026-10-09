//! `orna history <relation-hex> <key> [--limit N] [--since <commit-hex>]`:
//! lists the revision history of one committed row through the OGS-1
//! commit-graph walk. Only commit headers and tree listings are read; no blob
//! payload is opened or hydrated.
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

use super::*;
use orna_repository_v1::{AdmittedRow, KeyRange, Repository, RepositoryFormatContext, TypedKey};

use std::collections::BTreeMap;

/// Most revisions one history listing reports when `--limit` is not given.
const DEFAULT_HISTORY_LIMIT: usize = 64;
/// Largest `--limit` accepted; matches the repository walk bound.
const MAX_HISTORY_LIMIT: usize = 4096;
/// Most rows one diff page reads; matches the repository range bound
/// (`MAX_ROW_RANGE_LIMIT`). A relation larger than one page is walked page by
/// page so the comparison never materialises an unbounded range.
const MAX_DIFF_PAGE: usize = 256;

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
            "--diff" => {
                let from = words.next().ok_or_else(|| {
                    history_error(
                        "--diff needs two snapshots",
                        "usage: --diff <from-selector> <to-selector>",
                    )
                })?;
                let to = words.next().ok_or_else(|| {
                    history_error(
                        "--diff needs two snapshots",
                        "usage: --diff <from-selector> <to-selector>",
                    )
                })?;
                if diff.is_some() {
                    return Err(history_error(
                        "History compares one pair of snapshots",
                        "usage: --diff <from-selector> <to-selector>",
                    ));
                }
                diff = Some((from, to));
            }
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
                    format!(
                        "got {flag:?}; accepted: --at, --diff, --limit, --since, --format, --reverse, --author, --count, --quiet"
                    ),
                ));
            }
            _ => positional.push(word),
        }
    }
    let (relation, key) = match (diff, positional[..].split_first()) {
        // `--diff` names the relation and both snapshots, so no row key is
        // read: the comparison is over every row of the relation.
        (Some(_), Some((relation, []))) => (*relation, None),
        (Some((_, _)), _) => {
            return Err(history_error(
                "History expects a relation for --diff",
                "usage: orna history <relation-hex> --diff <from-selector> <to-selector>",
            ));
        }
        (None, Some((relation, [key]))) => (*relation, Some(*key)),
        (None, _) => {
            return Err(history_error(
                "History expects a relation and a row key",
                "usage: orna history <relation-hex> <key> [--at SELECTOR] [--limit N] [--since <commit-hex>]",
            ));
        }
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
        diff,
    })
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let relation = parse_relation_id(options.relation)?;
    let path = local_project_path(endpoint)?;
    let repository = Repository::discover(path)
        .map_err(|error| history_error("Repository could not be opened", format!("{error:?}")))?;
    if let Some((from, to)) = options.diff {
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
    let (format, start) = match options.at {
        Some(selector) => {
            // One guard, one resolution: `pin_snapshot` refuses an ambiguous
            // name before it resolves, so `--at` and `--diff` cannot disagree.
            let (start, format) = pin_snapshot(&repository, path, selector)?;
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
    // The key is one canonical Orna key expression (ORNA-CLI-005), and the
    // same spelling names both a text key and the bytes that spell it: a row
    // committed from imported media is keyed by the bytes the capture was
    // given, while a row written by hand may be keyed by canonical text. Both
    // are bounded point reads of the committed row map, so the row that answers
    // is a real committed row rather than a reinterpretation of the argument.
    let mut row = None;
    for candidate in cli_row_key::key_candidates(key)? {
        row = graph
            .lookup_row(&candidate, &scope)
            .map_err(|error| history_error("Row lookup failed", format!("{error:?}")))?;
        if row.is_some() {
            break;
        }
    }
    let row = row
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
                        revision.committed_at(),
                        revision.migration(),
                    )
                })
                .collect();
            println!("{}", serde_json::Value::Array(entries));
        }
    }
    Ok(())
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
/// Resolves and pins one named snapshot exactly once, refusing a bare name that
/// more than one branch, tag or remote branch carries. `--at` and both `--diff`
/// endpoints pin through here, so an identical-prefix name is refused wherever
/// it appears rather than resolved to whichever ref Git lists first.
fn pin_snapshot(
    repository: &Repository,
    directory: &str,
    selector: &str,
) -> Result<(String, RepositoryFormatContext), Diagnostic> {
    if bare_ref_selector_is_ambiguous(directory, selector)? {
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
    Ok((start, format))
}

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

/// One row's change between the two compared snapshots.
enum RowChange {
    Added(AdmittedRow),
    Removed(AdmittedRow),
    Changed {
        before: AdmittedRow,
        after: AdmittedRow,
    },
}

/// The payload-free annotation coordinate of one row: the Blob field
/// descriptors its own stored tuple carries, separately from the row's value.
fn annotations_of(row: &AdmittedRow) -> Vec<(usize, serde_json::Value)> {
    row.blob_fields()
        .unwrap_or_default()
        .into_iter()
        .map(|(field, metadata)| {
            (
                field,
                serde_json::json!({
                    "field": field,
                    "media_type": metadata.media_type(),
                    "suffix": metadata.suffix(),
                    "length": metadata.length(),
                    "sha256": hex(&metadata.sha256()),
                }),
            )
        })
        .collect()
}

/// The stored value the row's own tuple holds, in its canonical encoded form.
fn encoded_value(row: &AdmittedRow) -> Vec<u8> {
    row.value().encoded_fields().unwrap_or_default().to_vec()
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
    // Each endpoint is pinned once, through the same ambiguity guard `--at`
    // uses, so an identical-prefix name is refused rather than resolved to
    // whichever ref `git for-each-ref` lists first.
    let (from_commit, before_format) = pin_snapshot(repository, directory, from)?;
    let (to_commit, after_format) = pin_snapshot(repository, directory, to)?;
    // A format-1/2 endpoint is a read-only compatibility input with no native
    // `.orna/store`, so it has no row map to compare. Say so instead of
    // reporting the format-3 store seam's generic failure, the same refusal
    // `--at` gives.
    for (selector, format) in [(from, &before_format), (to, &after_format)] {
        if format.is_read_only() {
            return Err(history_error(
                "Snapshot is a legacy format-1/2 input",
                format!(
                    "{selector:?} names a read-only compatibility snapshot with no native row store; name a format-3 commit"
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
    let from_label = format!("{from:?} ({from_commit})");
    let to_label = format!("{to:?} ({to_commit})");
    let (before, before_read) = read_relation_rows(&before_format, relation)?;
    let (after, after_read) = read_relation_rows(&after_format, relation)?;
    let payload_bytes_read = before_read.payload_bytes_read + after_read.payload_bytes_read;
    let native_objects_read = before_read.objects_read + after_read.objects_read;
    let changes = compare_rows(before, after);
    let (added, changed, removed) = counts(&changes);
    match format {
        HistoryFormat::Human => {
            for change in &changes {
                println!("{}", human_change(change));
            }
            println!(
                "{added} added; {changed} changed; {removed} removed; {} rows in {to_label}",
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
                    "changed": changed,
                    "removed": removed,
                    "media_payload_bytes_read": payload_bytes_read,
                    "native_objects_read": native_objects_read,
                    "changes": entries,
                })
            );
        }
    }
    Ok(())
}

/// The measured cost of one pinned relation read: the media payload bytes and
/// native objects the read touched.
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
) -> Result<(BTreeMap<Vec<u8>, AdmittedRow>, ReadCost), Diagnostic> {
    let row_map = format
        .load_row_map(relation)
        .map_err(|error| history_error("Row map could not be loaded", format!("{error:?}")))?;
    let graph = format
        .open_native_graph(&row_map)
        .map_err(|error| history_error("Native graph could not be opened", format!("{error:?}")))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| history_error("Read scope could not be opened", format!("{error:?}")))?;
    let mut rows: BTreeMap<Vec<u8>, AdmittedRow> = BTreeMap::new();
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
            // `KeyRange`'s lower bound is inclusive, so each page re-reads the
            // boundary row it resumes at; the map insert keys by canonical
            // bytes and dedupes it, and the cursor still advances a full page.
            last = Some(row.key().clone());
            let key = row.key().canonical_bytes().map_err(|error| {
                history_error("Row key could not be encoded", format!("{error:?}"))
            })?;
            rows.insert(key, row);
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
            // A full page with no advance would re-read the same page.
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

/// Joins the two pinned reads by canonical key into one ordered change list.
fn compare_rows(
    before: BTreeMap<Vec<u8>, AdmittedRow>,
    after: BTreeMap<Vec<u8>, AdmittedRow>,
) -> Vec<RowChange> {
    let mut changes = Vec::new();
    for (key, old) in &before {
        match after.get(key) {
            None => changes.push(RowChange::Removed(old.clone())),
            Some(new) if row_identity(old) != row_identity(new) => {
                changes.push(RowChange::Changed {
                    before: old.clone(),
                    after: new.clone(),
                });
            }
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

/// What makes two readings of one committed row the same row: the stored value
/// beside the Blob annotation coordinate its tuple carries.
fn row_identity(row: &AdmittedRow) -> (Vec<u8>, Vec<(usize, serde_json::Value)>) {
    (encoded_value(row), annotations_of(row))
}

/// One change as a JSON object. A `changed` entry separates the two
/// coordinates so a content move and an annotation move are told apart.
fn change_json(change: &RowChange) -> serde_json::Value {
    match change {
        RowChange::Added(row) => serde_json::json!({
            "change": "added",
            "key": render_key(row.key()),
            "annotations": annotations_of(row),
        }),
        RowChange::Removed(row) => serde_json::json!({
            "change": "removed",
            "key": render_key(row.key()),
            "annotations": annotations_of(row),
        }),
        RowChange::Changed { before, after } => serde_json::json!({
            "change": "changed",
            "key": render_key(after.key()),
            "content_changed": encoded_value(before) != encoded_value(after),
            "before": { "annotations": annotations_of(before) },
            "after": { "annotations": annotations_of(after) },
        }),
    }
}

/// One change as a human line. A changed row names which coordinate moved so a
/// re-annotation is not read as a data change.
fn human_change(change: &RowChange) -> String {
    match change {
        RowChange::Added(row) => format!("added   {}", render_key(row.key())),
        RowChange::Removed(row) => format!("removed {}", render_key(row.key())),
        RowChange::Changed { before, after } => {
            let content = encoded_value(before) != encoded_value(after);
            let annotation = annotations_of(before) != annotations_of(after);
            format!(
                "changed {} (content={}, annotation={})",
                render_key(after.key()),
                content,
                annotation,
            )
        }
    }
}

/// Counts the changes as `(added, changed, removed)`.
fn counts(changes: &[RowChange]) -> (usize, usize, usize) {
    let mut added = 0;
    let mut changed = 0;
    let mut removed = 0;
    for change in changes {
        match change {
            RowChange::Added(_) => added += 1,
            RowChange::Changed { .. } => changed += 1,
            RowChange::Removed(_) => removed += 1,
        }
    }
    (added, changed, removed)
}

/// Renders a canonical row key the way `orna query` prints one: a text key as
/// its text, and a byte key as that same text when its bytes are valid UTF-8,
/// so a key reported by either verb is greppable the same way.
fn render_key(key: &TypedKey) -> String {
    match key {
        TypedKey::Text(text) => text.clone(),
        TypedKey::UInt(value) => value.to_string(),
        TypedKey::Int(value) => value.to_string(),
        TypedKey::Bool(value) => value.to_string(),
        TypedKey::Null => "-".to_owned(),
        TypedKey::Bytes(bytes) => render_bytes(bytes),
        TypedKey::Tuple(values) => values.iter().map(render_key).collect::<Vec<_>>().join(","),
    }
}

fn render_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.is_empty() && text.chars().all(|character| !character.is_control()) => {
            text.to_owned()
        }
        _ => format!("0x{}", hex(bytes)),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
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
