//! `orna query <relation-hex> [--key KEY|0xBYTES] [--from text:KEY|bytes:HEX] [--until text:KEY|bytes:HEX] [--field N] [--limit N] [--format human|json]`
//! lists the payload-free Blob metadata of committed format-3 rows, optionally
//! narrowed by an annotation predicate (`--kind`, `--not-kind`, `--suffix`,
//! `--min-length`, `--max-length`).
//!
//! At a format-1/2 pin (`--at`, or a legacy `HEAD`) the rows come from the
//! versioned legacy reader instead: a compact table's authoritative rows or an
//! editable table's committed loose row units (ORNA-UPGRADE-001,
//! ORNA-UPGRADE-002, ORNA-ROW-013). The report names which of the two answered.
//! The snapshot is admitted through the project loader before either
//! representation is read, so a format-3 snapshot that tracks a loose row file
//! is refused with the format admission code (ORNA-REPO-CONTEXT-004) instead of
//! being answered from the store.
//!
//! The query reads through the same repository-owned native graph the row
//! history walks, so it is measured at the real storage read seam: every row
//! page, index node and descriptor it touches is charged as an object, and
//! `payload_bytes_read` stays at zero because the row's own field tuple
//! carries the length, SHA-256 and MIME-1 annotation. Media bytes are only
//! fetched by an explicit Blob read, never by a listing or summary.
//!
//! The predicate is evaluated against those same stored annotation
//! coordinates, so a filtered listing reports the plan it used and the payload
//! bytes it did not fetch. Coordinates a stored ROV-3 annotation does not bind
//! (a decoded duration or pixel dimension) are not answered here: they are
//! decoder output under ORNA-MEDIA-002, not annotation, and the plan names them
//! as requiring an explicit decode.

use super::cli_history::bare_ref_selector_is_ambiguous;
use super::{Diagnostic, Endpoint, cli_legacy, local_project_path};
use crate::cli_row_key::{
    check_bound_type, check_point_key_type, composite_bound_error, key_candidates,
};
use orna_application::read_legacy_table_rows;
use orna_repository::{
    AdmittedRow, KeyRange, NativeGraphContext, Repository, RepositoryReadScope, TypedKey,
};
use orna_value::BlobMetadataFilter;

/// Most rows one listing reports when `--limit` is not given.
const DEFAULT_QUERY_LIMIT: usize = 64;
/// Largest `--limit` accepted; matches the ORP range bound.
const MAX_QUERY_LIMIT: usize = 256;

/// Output shape for the metadata listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryFormat {
    Human,
    Json,
}

/// Parsed query options: positional `<relation-hex>` plus flags.
#[derive(Debug, Eq, PartialEq)]
struct QueryOptions<'a> {
    relation: &'a str,
    key: Option<&'a str>,
    from: Option<TypedKey>,
    until: Option<TypedKey>,
    field: Option<usize>,
    at: Option<&'a str>,
    limit: usize,
    format: QueryFormat,
    predicate: BlobMetadataFilter,
    has_predicate: bool,
}

#[allow(clippy::too_many_lines)]
fn parse_options(arguments: &[String]) -> Result<QueryOptions<'_>, Diagnostic> {
    let mut positional = Vec::new();
    let mut key = None;
    let mut from = None;
    let mut until = None;
    let mut field = None;
    let mut at = None;
    let mut limit = DEFAULT_QUERY_LIMIT;
    let mut format = QueryFormat::Human;
    let mut predicate = BlobMetadataFilter::new();
    let mut has_predicate = false;
    let mut words = arguments.iter().map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "--key" => {
                let value = words
                    .next()
                    .ok_or_else(|| query_error("--key needs a value", "usage: --key KEY"))?;
                key = Some(value);
            }
            "--field" => {
                let value = words
                    .next()
                    .ok_or_else(|| query_error("--field needs a value", "usage: --field N"))?;
                field = Some(value.parse::<usize>().map_err(|_| {
                    query_error("--field is not an index", format!("got {value:?}"))
                })?);
            }
            "--at" => {
                let value = words
                    .next()
                    .ok_or_else(|| query_error("--at needs a value", "usage: --at <selector>"))?;
                if at.is_some() {
                    return Err(query_error(
                        "Query names one snapshot",
                        "usage: --at <selector>",
                    ));
                }
                at = Some(value);
            }
            "--from" => {
                let value = words.next().ok_or_else(|| {
                    query_error(
                        "--from needs a value",
                        "usage: --from text:KEY|bytes:HEX|int:N|uint:N",
                    )
                })?;
                if from.is_some() {
                    return Err(query_error(
                        "Query names one lower bound",
                        "usage: --from text:KEY|bytes:HEX|int:N|uint:N",
                    ));
                }
                from = Some(parse_bound_key(value)?);
            }
            "--until" => {
                let value = words.next().ok_or_else(|| {
                    query_error(
                        "--until needs a value",
                        "usage: --until text:KEY|bytes:HEX|int:N|uint:N",
                    )
                })?;
                if until.is_some() {
                    return Err(query_error(
                        "Query names one upper bound",
                        "usage: --until text:KEY|bytes:HEX|int:N|uint:N",
                    ));
                }
                until = Some(parse_bound_key(value)?);
            }
            "--kind" => {
                let value = words
                    .next()
                    .ok_or_else(|| query_error("--kind needs a value", "usage: --kind MIME"))?;
                predicate = predicate.with_media_type(value);
                has_predicate = true;
            }
            "--not-kind" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--not-kind needs a value", "usage: --not-kind MIME")
                })?;
                predicate = predicate.without_media_type(value);
                has_predicate = true;
            }
            "--suffix" => {
                let value = words
                    .next()
                    .ok_or_else(|| query_error("--suffix needs a value", "usage: --suffix HINT"))?;
                predicate = predicate.with_suffix(value);
                has_predicate = true;
            }
            "--min-length" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--min-length needs a value", "usage: --min-length BYTES")
                })?;
                predicate = predicate.with_min_length(value.parse::<u64>().map_err(|_| {
                    query_error("--min-length is not a byte count", format!("got {value:?}"))
                })?);
                has_predicate = true;
            }
            "--max-length" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--max-length needs a value", "usage: --max-length BYTES")
                })?;
                predicate = predicate.with_max_length(value.parse::<u64>().map_err(|_| {
                    query_error("--max-length is not a byte count", format!("got {value:?}"))
                })?);
                has_predicate = true;
            }
            "--limit" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--limit needs a value", "usage: --limit <1..=256>")
                })?;
                limit = value
                    .parse::<usize>()
                    .ok()
                    .filter(|limit| (1..=MAX_QUERY_LIMIT).contains(limit))
                    .ok_or_else(|| {
                        query_error("--limit is not in 1..=256", format!("got {value:?}"))
                    })?;
            }
            "--format" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--format needs a value", "usage: --format <human|json>")
                })?;
                format = match value {
                    "human" => QueryFormat::Human,
                    "json" => QueryFormat::Json,
                    _ => {
                        return Err(query_error(
                            "--format is not human or json",
                            format!("got {value:?}"),
                        ));
                    }
                };
            }
            flag if flag.starts_with("--") => {
                return Err(query_error(
                    "Unknown query flag",
                    format!(
                        "got {flag:?}; accepted: --key, --from, --until, --field, --at, --kind, --not-kind, --suffix, \
                         --min-length, --max-length, --limit, --format"
                    ),
                ));
            }
            _ => positional.push(word),
        }
    }
    let [relation] = positional[..] else {
        return Err(query_error(
            "Query expects a relation",
            "usage: orna query <relation-hex> [--key KEY] [--from text:KEY|bytes:HEX|int:N|uint:N] [--until text:KEY|bytes:HEX|int:N|uint:N] [--field N] [--at SELECTOR] [--kind MIME] [--not-kind MIME] [--suffix HINT] [--min-length BYTES] [--max-length BYTES] [--limit N] [--format human|json]",
        ));
    };
    if key.is_some() && (from.is_some() || until.is_some()) {
        return Err(query_error(
            "Point key conflicts with typed bounds",
            "use either --key KEY or --from/--until bounds",
        ));
    }
    Ok(QueryOptions {
        relation,
        key,
        from,
        until,
        field,
        at,
        limit,
        format,
        predicate,
        has_predicate,
    })
}

/// Parses a typed index bound without applying the point-key spelling fallback.
fn parse_bound_key(value: &str) -> Result<TypedKey, Diagnostic> {
    if let Some(text) = value.strip_prefix("text:") {
        return Ok(TypedKey::Text(text.to_owned()));
    }
    if let Some(number) = value.strip_prefix("int:") {
        return number.parse::<i64>().map(TypedKey::Int).map_err(|_| {
            query_error(
                "Range integer key is not an i64",
                format!(
                    "got {value:?}; int: takes -9223372036854775808..=9223372036854775807, \
                     use uint:N for 0..=18446744073709551615"
                ),
            )
        });
    }
    if let Some(number) = value.strip_prefix("uint:") {
        return number.parse::<u64>().map(TypedKey::UInt).map_err(|_| {
            query_error(
                "Range integer key is not a u64",
                format!(
                    "got {value:?}; uint: takes 0..=18446744073709551615, \
                     use int:N for negative values"
                ),
            )
        });
    }
    if let Some(hex) = value.strip_prefix("bytes:") {
        if !hex.len().is_multiple_of(2) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(query_error(
                "Range byte key is not hexadecimal",
                format!("got {value:?}; usage: bytes:HEX with an even number of digits"),
            ));
        }
        let bytes = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).expect("hex digits are ASCII"), 16)
                    .expect("hexadecimal digits were checked above")
            })
            .collect();
        return Ok(TypedKey::Bytes(bytes));
    }
    Err(query_error(
        "Range key is not typed",
        format!("got {value:?}; use text:KEY, bytes:HEX, int:N or uint:N"),
    ))
}

#[allow(clippy::too_many_lines)]
pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let path = local_project_path(endpoint)?;
    let repository = Repository::discover(path)
        .map_err(|error| query_error("Repository could not be opened", format!("{error:?}")))?;
    // `--at` pins one named snapshot before any row is read. The selector is
    // resolved exactly once here; the row map, the graph and every descriptor
    // below come from the commit it named, so a branch that advances during
    // the read never changes the answer, and the workspace `HEAD` is never
    // consulted instead.
    let (commit, format) = if let Some(selector) = options.at {
        // `rev-parse` accepts a bare name that two namespaces both define
        // and only warns about the choice, so a name that identifies more
        // than one snapshot is refused rather than silently answered from
        // whichever one it preferred.
        if bare_ref_selector_is_ambiguous(
            path,
            selector,
            "run `orna query <relation-hex>` inside an initialized repository",
        )? {
            return Err(Diagnostic::target_with_detail(
                "E2000",
                "Snapshot name is ambiguous",
                "use the full ref name (`refs/heads/NAME`, `refs/tags/NAME`, \
                     `refs/remotes/ORIGIN/NAME`) or the commit id `orna history` lists",
                format!("{selector:?} names more than one branch, tag or remote branch"),
            ));
        }
        let commit = repository.resolve_snapshot(selector).map_err(|error| {
            query_error(
                "Snapshot could not be resolved",
                format!("{selector:?}: {error:?}"),
            )
        })?;
        let format = repository
            .open_pinned_format_context(commit.as_str())
            .map_err(|error| {
                query_error(
                    "Snapshot could not be pinned",
                    format!("{selector:?}: {error:?}"),
                )
            })?;
        (Some(commit), format)
    } else {
        let commit = repository
            .head()
            .map_err(|error| query_error("Snapshot could not be resolved", format!("{error:?}")))?;
        let format = repository.open_format_context().map_err(|error| {
            query_error("Format context could not be opened", format!("{error:?}"))
        })?;
        (commit, format)
    };
    // The snapshot is admitted before any row is read from either
    // representation: a format-3 snapshot that tracks a loose row file is the
    // mixed layout project admission refuses (ORNA-META-001, ORNA-META-002), and
    // answering it from the store would report rows whose row files the same
    // snapshot carries.
    cli_legacy::admit_snapshot(&repository, commit.as_ref())?;
    // A format-1/2 pin is a read-only compatibility input: it carries no native
    // `.orna/store`, so it has no row map. Its rows are read through the
    // versioned legacy reader instead (ORNA-UPGRADE-001, ORNA-UPGRADE-002,
    // ORNA-ROW-013).
    if format.is_read_only() {
        let Some(commit) = commit.as_ref() else {
            return Err(query_error(
                "Snapshot could not be admitted",
                "--at names a read-only compatibility snapshot with no committed project source",
            ));
        };
        return run_legacy(&options, &repository, commit);
    }
    let relation = parse_relation_id(options.relation)?;
    let table = format
        .resolve_committed_table_by_relation_id(relation)
        .map_err(|error| {
            query_error(
                "Table descriptor could not be resolved",
                format!("{error:?}"),
            )
        })?;
    // The committed descriptor declares each primary-key component's type. A
    // single component is the only key type a `--key` spelling or a typed
    // bound can name; a composite key is untyped here and keeps the untyped
    // spellings it has always had, so only a predicate scan over it (which must
    // page) is refused.
    let key_types = table
        .descriptor()
        .primary_key_primitive_types(*table.relation_id())
        .map_err(|error| {
            query_error("Table primary key could not be read", format!("{error:?}"))
        })?;
    let single_key = match key_types.as_slice() {
        [key_type] => Some(key_type.as_deref()),
        _ => None,
    };
    if let Some(key_type) = single_key {
        for bound in options.from.iter().chain(options.until.iter()) {
            check_bound_type(bound, key_type)?;
        }
    } else if options.from.is_some() || options.until.is_some() {
        return Err(composite_bound_error());
    }
    let row_map = format
        .load_row_map(*table.relation_id())
        .map_err(|error| query_error("Row map could not be loaded", format!("{error:?}")))?;
    let graph = format
        .open_native_graph(&row_map)
        .map_err(|error| query_error("Native graph could not be opened", format!("{error:?}")))?;
    let scope = graph
        .open_read_scope()
        .map_err(|error| query_error("Read scope could not be opened", format!("{error:?}")))?;
    // `--key` asks for exactly one row. A row committed from imported media is
    // keyed by the raw bytes a caller passed, and a row written by hand may be
    // keyed by canonical text, so the spelling printed by the listing is tried
    // as text and then as those same bytes. Both are bounded point reads of
    // the committed row map, and the row that answers is a real committed row
    // rather than a reinterpretation of the query. A single-component key
    // type that `--key` cannot spell is refused rather than missed.
    let point_key_type = match (single_key, options.key) {
        (Some(key_type), Some(_)) => {
            check_point_key_type(key_type)?;
            key_type
        }
        _ => None,
    };
    // A point lookup reads one row. A bounded range without a predicate reads
    // one page, because `--limit` bounds it. A bounded range with a predicate
    // is walked in ordered pages: every candidate in the range must be
    // evaluated so the counters are exact, but no page is retained after its
    // rows are evaluated, so the walk costs one page of memory whatever the
    // range holds (ORNA-ROW-009).
    let mut rows_read = 0u64;
    let mut listings: Vec<(TypedKey, usize, orna_value::BlobMetadata)> = Vec::new();
    let mut candidates = 0usize;
    let mut matched = 0usize;
    match options.key {
        Some(key) => {
            let mut found = None;
            for candidate in key_candidates(key, point_key_type)? {
                found = graph
                    .lookup_row(&candidate, &scope)
                    .map_err(|error| query_error("Row lookup failed", format!("{error:?}")))?;
                if found.is_some() {
                    break;
                }
            }
            let Some(row) = found else {
                return Err(query_error(
                    "Row is not committed",
                    format!("no row with key {:?}", options.key.unwrap_or_default()),
                ));
            };
            rows_read = 1;
            collect_row_blob_listings(
                &graph,
                &scope,
                &row,
                &options,
                &mut listings,
                &mut candidates,
                &mut matched,
            )?;
        }
        None if options.has_predicate => {
            visit_row_pages(
                MAX_QUERY_LIMIT,
                options.from.clone(),
                options.until.clone(),
                |range| {
                    graph.range_rows(range, &scope).map_err(|error| {
                        query_error("Row range could not be read", format!("{error:?}"))
                    })
                },
                |row| row.key().clone(),
                |row| {
                    rows_read += 1;
                    // A decode failure ends the walk with its own diagnostic.
                    let failed = collect_row_blob_listings(
                        &graph,
                        &scope,
                        &row,
                        &options,
                        &mut listings,
                        &mut candidates,
                        &mut matched,
                    )
                    .is_err();
                    if failed {
                        std::ops::ControlFlow::Break(())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                },
            )?;
        }
        None => {
            let range =
                KeyRange::new(options.from.clone(), options.until.clone(), options.limit)
                    .map_err(|error| query_error("Row range is invalid", format!("{error:?}")))?;
            let page = graph.range_rows(&range, &scope).map_err(|error| {
                query_error("Row range could not be read", format!("{error:?}"))
            })?;
            rows_read = u64::try_from(page.len()).unwrap_or(u64::MAX);
            for row in &page {
                collect_row_blob_listings(
                    &graph,
                    &scope,
                    row,
                    &options,
                    &mut listings,
                    &mut candidates,
                    &mut matched,
                )?;
            }
        }
    }
    // The predicate and the field filter ran per row inside the walk, so what
    // is left is the caller's own bound.
    let subject = options
        .field
        .map_or_else(|| "all fields".to_owned(), |field| format!("field {field}"));
    listings.truncate(options.limit);
    let estimated_rows = match options.key {
        Some(_) => 1,
        None => row_map.version().row_count().unwrap_or(rows_read),
    };
    let plan = PredicatePlan::new(
        &options.predicate,
        &subject,
        estimated_rows,
        rows_read,
        candidates,
        matched,
    );
    match options.format {
        QueryFormat::Human => {
            for (key, field, metadata) in &listings {
                // The descriptor is the object a caller needs to read the
                // payload the listing did not fetch, so it is stated here
                // rather than left to a second lookup. The listing decoded it
                // from the row's own stored reference, so printing it reads no
                // additional object.
                println!(
                    "{} {field} {} {} {} {} descriptor {} {}",
                    render_key(key),
                    metadata.media_type(),
                    metadata.suffix().unwrap_or("-"),
                    metadata.length(),
                    hex(&metadata.sha256()),
                    metadata
                        .descriptor_oid()
                        .map_or_else(|| "-".to_owned(), |oid| hex(oid.as_bytes())),
                    metadata.is_hydrated(),
                );
            }
            println!(
                "{} blobs; media payload bytes read: {}; native objects read: {}",
                listings.len(),
                scope.payload_bytes_read(),
                scope.objects_read(),
            );
            println!("{}", plan.human_line());
        }
        QueryFormat::Json => {
            let entries: Vec<serde_json::Value> = listings
                .iter()
                .map(|(key, field, metadata)| {
                    serde_json::json!({
                        "key": render_key(key),
                        "field": field,
                        "media_type": metadata.media_type(),
                        "suffix": metadata.suffix(),
                        "length": metadata.length(),
                        "sha256": hex(&metadata.sha256()),
                        "descriptor": metadata
                            .descriptor_oid()
                            .map(|oid| hex(oid.as_bytes())),
                        "hydrated": metadata.is_hydrated(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "rows": rows_read,
                    "blobs": entries.len(),
                    "media_payload_bytes_read": scope.payload_bytes_read(),
                    "native_objects_read": scope.objects_read(),
                    "plan": plan.to_json(),
                    "listings": entries,
                })
            );
        }
    }
    Ok(())
}

/// Answers one `orna query` from a format-1/2 pin through the versioned legacy
/// reader (ORNA-UPGRADE-001, ORNA-UPGRADE-002, ORNA-ROW-013).
///
/// The table is named the way the 1.0.0 layout names it: a compact table by its
/// storage table id, an editable table by its key-path directory. `--key`,
/// `--from`/`--until` and `--limit` bound the answer the same way the format-3
/// walk bounds it, over the reconstructed key order (ORNA-ROW-002). Nothing here
/// reads a payload byte: a legacy listing is the annotation the stored value
/// already carries, so the payload counters stay at zero.
#[allow(clippy::too_many_lines)]
fn run_legacy(
    options: &QueryOptions<'_>,
    repository: &Repository,
    commit: &orna_repository::GitCommitRef,
) -> Result<(), Diagnostic> {
    const LEGACY_HELP: &str = "name a legacy table by its storage table id (32 hex digits) or by its key-path directory, such as `contacts/Contact`";
    let address = cli_legacy::legacy_address(options.relation, LEGACY_HELP)?;
    let table = read_legacy_table_rows(repository, commit, &address)
        .map_err(|error| cli_legacy::legacy_read_error(&error, LEGACY_HELP))?;
    let mut rows_read = 0u64;
    let mut listings: Vec<(TypedKey, usize, orna_value::BlobMetadata)> = Vec::new();
    let mut candidates = 0usize;
    let mut matched = 0usize;
    match options.key {
        Some(key) => {
            // A legacy row's key is reconstructed from its path text (or, for a
            // compact row, decoded from its stored key), so the spelling a
            // listing printed names the row here as well.
            let spelled = key_candidates(key, None)?;
            let found = table.rows().iter().find(|row| {
                spelled.iter().any(|candidate| {
                    row.key() == candidate
                        || row.key().component_text() == candidate.component_text()
                })
            });
            let Some(row) = found else {
                return Err(query_error(
                    "Row is not committed",
                    format!("no row with key {key:?}"),
                ));
            };
            rows_read = 1;
            collect_legacy_listings(row, options, &mut listings, &mut candidates, &mut matched);
        }
        None => {
            for row in table.rows() {
                if options.from.as_ref().is_some_and(|lower| row.key() < lower)
                    || options
                        .until
                        .as_ref()
                        .is_some_and(|upper| row.key() >= upper)
                {
                    continue;
                }
                // `--limit` bounds the rows one answer reads, exactly as it
                // bounds the format-3 page.
                if usize::try_from(rows_read).unwrap_or(usize::MAX) >= options.limit {
                    break;
                }
                rows_read += 1;
                collect_legacy_listings(row, options, &mut listings, &mut candidates, &mut matched);
            }
        }
    }
    let subject = options
        .field
        .map_or_else(|| "all fields".to_owned(), |field| format!("field {field}"));
    listings.truncate(options.limit);
    let estimated_rows = u64::try_from(table.rows().len()).unwrap_or(u64::MAX);
    let plan = PredicatePlan::new(
        &options.predicate,
        &subject,
        estimated_rows,
        rows_read,
        candidates,
        matched,
    );
    let source = legacy_source_name(table.source());
    match options.format {
        QueryFormat::Human => {
            for (key, field, metadata) in &listings {
                println!(
                    "{} {field} {} {} {} {} descriptor {} {}",
                    render_key(key),
                    metadata.media_type(),
                    metadata.suffix().unwrap_or("-"),
                    metadata.length(),
                    hex(&metadata.sha256()),
                    metadata
                        .descriptor_oid()
                        .map_or_else(|| "-".to_owned(), |oid| hex(oid.as_bytes())),
                    metadata.is_hydrated(),
                );
            }
            println!(
                "{} blobs; media payload bytes read: 0; native objects read: 0; legacy source: {source}",
                listings.len()
            );
            println!("{}", plan.human_line());
        }
        QueryFormat::Json => {
            let entries: Vec<serde_json::Value> = listings
                .iter()
                .map(|(key, field, metadata)| {
                    serde_json::json!({
                        "key": render_key(key),
                        "field": field,
                        "media_type": metadata.media_type(),
                        "suffix": metadata.suffix(),
                        "length": metadata.length(),
                        "sha256": hex(&metadata.sha256()),
                        "descriptor": metadata
                            .descriptor_oid()
                            .map(|oid| hex(oid.as_bytes())),
                        "hydrated": metadata.is_hydrated(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "rows": rows_read,
                    "blobs": entries.len(),
                    "media_payload_bytes_read": 0,
                    "native_objects_read": 0,
                    "legacy_source": source,
                    "plan": plan.to_json(),
                    "listings": entries,
                })
            );
        }
    }
    Ok(())
}

/// `compact` or `loose-row-unit`: which representation of the 1.0.0 layout
/// answered this read.
const fn legacy_source_name(source: orna_application::LegacyRowSource) -> &'static str {
    match source {
        orna_application::LegacyRowSource::Compact => "compact",
        orna_application::LegacyRowSource::LooseRowUnit => "loose-row-unit",
    }
}

/// Projects one legacy row's Blob-valued stored fields into the listing.
///
/// The predicate, the field filter and the counters are the format-3 walk's
/// own: a candidate is a Blob field the filter evaluated and a match is one it
/// accepted. A legacy row carries a Blob only as an annotated content reference
/// the row already stores, so evaluating the predicate reads no payload.
fn collect_legacy_listings(
    row: &orna_application::LegacyRow,
    options: &QueryOptions<'_>,
    listings: &mut Vec<(TypedKey, usize, orna_value::BlobMetadata)>,
    candidates: &mut usize,
    matched: &mut usize,
) {
    for (field, stored) in row.fields().iter().enumerate() {
        let Some(metadata) = legacy_field_blob_metadata(stored.value()) else {
            continue;
        };
        if options.field.is_none_or(|wanted| wanted == field) {
            *candidates += 1;
            if options.predicate.matches(&metadata) {
                *matched += 1;
                if listings.len() < options.limit {
                    listings.push((row.key().clone(), field, metadata));
                }
            }
        }
    }
}

/// The Blob annotation of one legacy stored value, when the value carries one.
///
/// A Blob field is stored as the ROV-3 annotated content reference, optionally
/// inside the canonical present `Option` wrapper, which is the shape the
/// format-3 field projection decodes.
fn legacy_field_blob_metadata(value: &orna_value::Raw) -> Option<orna_value::BlobMetadata> {
    let value = match value {
        orna_value::Raw::Tag(60013, payload) => match payload.as_ref() {
            orna_value::Raw::Array(parts) => match parts.as_slice() {
                [orna_value::Raw::Int(flag), value] if *flag == 1.into() => value,
                _ => return None,
            },
            _ => return None,
        },
        value => value,
    };
    if !matches!(value, orna_value::Raw::Tag(orna_value::ROV3_BLOB_TAG, _)) {
        return None;
    }
    orna_value::blob_metadata_from_raw(value).ok()
}

/// Pages `[from, until)` in ordered pages of `page_rows` keys. Each page
/// resumes after the last key it returned, through the range's exclusive
/// resume cursor, so no row is read twice and none is skipped. The cursor is
/// a comparison under the logical key order, so every key type pages,
/// composite keys included, and no successor of a key is ever computed.
fn scan_pages<T>(
    page_rows: usize,
    from: Option<TypedKey>,
    until: Option<TypedKey>,
    mut read_page: impl FnMut(&KeyRange) -> Result<Vec<T>, Diagnostic>,
    key_of: impl Fn(&T) -> TypedKey,
) -> Result<Vec<T>, Diagnostic> {
    let base = KeyRange::new(from, until, page_rows)
        .map_err(|error| query_error("Row range is invalid", format!("{error:?}")))?;
    let mut rows = Vec::new();
    let mut range = base.clone();
    loop {
        let page = read_page(&range)?;
        let short = page.len() < page_rows;
        let Some(last) = page.last().map(&key_of) else {
            break;
        };
        rows.extend(page);
        if short {
            break;
        }
        range = base
            .clone()
            .with_resume_after(last)
            .map_err(|error| query_error("Row range is invalid", format!("{error:?}")))?;
    }
    Ok(rows)
}

/// Projects one row's Blob fields into the listing, applying the field filter
/// and the annotation predicate at the row the walk just read.
///
/// The predicate is evaluated over the annotation the listing already decoded,
/// so narrowing costs no extra read and never enters a descriptor or a payload
/// chunk. `candidates` counts the fields the predicate evaluated and `matched`
/// counts those it accepted, over the whole bounded range; only the first
/// `--limit` matches are kept, so the listing's memory does not grow with the
/// range (ORNA-ROW-009).
fn collect_row_blob_listings(
    graph: &NativeGraphContext,
    scope: &RepositoryReadScope,
    row: &AdmittedRow,
    options: &QueryOptions<'_>,
    listings: &mut Vec<(TypedKey, usize, orna_value::BlobMetadata)>,
    candidates: &mut usize,
    matched: &mut usize,
) -> Result<(), Diagnostic> {
    let fields = graph
        .row_blob_fields(row, scope)
        .map_err(|error| query_error("Row fields could not be decoded", format!("{error:?}")))?;
    for (field, metadata) in fields {
        if options.field.is_none_or(|wanted| wanted == field) {
            *candidates += 1;
            if options.predicate.matches(&metadata) {
                *matched += 1;
                if listings.len() < options.limit {
                    listings.push((row.key().clone(), field, metadata));
                }
            }
        }
    }
    Ok(())
}

/// Visits `[from, until)` in ordered pages, stopping when `visit` asks it to.
///
/// The paging rules are the ones [`scan_pages`] states: each page resumes after
/// the last key it returned, and the walk ends on a short page or the upper
/// bound. Unlike [`scan_pages`], no page is retained, so a caller that stops on
/// its own limit never materialises the rest of the range (ORNA-ROW-009
/// separates the estimated work of the range from the actual work of the walk).
fn visit_row_pages<T>(
    page_rows: usize,
    from: Option<TypedKey>,
    until: Option<TypedKey>,
    mut read_page: impl FnMut(&KeyRange) -> Result<Vec<T>, Diagnostic>,
    key_of: impl Fn(&T) -> TypedKey,
    mut visit: impl FnMut(T) -> std::ops::ControlFlow<()>,
) -> Result<(), Diagnostic> {
    let base = KeyRange::new(from, until, page_rows)
        .map_err(|error| query_error("Row range is invalid", format!("{error:?}")))?;
    let mut range = base.clone();
    loop {
        let page = read_page(&range)?;
        let short = page.len() < page_rows;
        let Some(last) = page.last().map(&key_of) else {
            break;
        };
        for item in page {
            if visit(item).is_break() {
                return Ok(());
            }
        }
        if short {
            break;
        }
        range = base
            .clone()
            .with_resume_after(last)
            .map_err(|error| query_error("Row range is invalid", format!("{error:?}")))?;
    }
    Ok(())
}

/// The measured plan of one annotation predicate evaluation.
///
/// This is the plan the query reports so a caller can see which annotation
/// coordinates answered the predicate, whether the answer needed any payload
/// byte, and which coordinates the stored annotation does not bind. It is
/// evidence about the read that happened, not a statement of intent: the
/// payload figure is the read seam's own count for this process.
struct PredicatePlan<'a> {
    predicate: &'a BlobMetadataFilter,
    subject: &'a str,
    /// Rows the row map says the bounded range holds, before any read.
    estimated_rows: u64,
    /// Rows the walk actually read, which stops at `--limit` matches.
    scanned_rows: u64,
    candidates: usize,
    matched: usize,
}

impl<'a> PredicatePlan<'a> {
    const fn new(
        predicate: &'a BlobMetadataFilter,
        subject: &'a str,
        estimated_rows: u64,
        scanned_rows: u64,
        candidates: usize,
        matched: usize,
    ) -> Self {
        Self {
            predicate,
            subject,
            estimated_rows,
            scanned_rows,
            candidates,
            matched,
        }
    }

    /// The coordinates this predicate evaluated, as the planner names them.
    fn coordinates(&self) -> &'static [&'static str] {
        self.predicate.coordinates()
    }

    /// Coordinates a decoded duration or pixel dimension would name. They are
    /// not part of the stored annotation, so no predicate over them can be
    /// answered without an explicit decode that reads payload bytes.
    const fn decode_required_coordinates() -> &'static [&'static str] {
        &["duration", "dimensions"]
    }

    fn human_line(&self) -> String {
        format!(
            "plan: annotation predicate over {}; coordinates {}; decoded-payload coordinates {}; \
             estimated rows {} scanned rows {}; candidates {} matched {}; subject {}",
            self.subject,
            self.coordinates().join(","),
            Self::decode_required_coordinates().join(","),
            self.estimated_rows,
            self.scanned_rows,
            self.candidates,
            self.matched,
            self.subject,
        )
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": "annotation-predicate",
            "subject": self.subject,
            "coordinates": self.coordinates(),
            "decode_required_coordinates": Self::decode_required_coordinates(),
            "estimated_rows": self.estimated_rows,
            "scanned_rows": self.scanned_rows,
            "candidates": self.candidates,
            "matched": self.matched,
            "payload_fetch": "none",
        })
    }
}

/// Renders a canonical row key the way the caller spelled it: a text key as
/// its text, and a byte key as that same text when the key's own bytes are
/// valid UTF-8, so a listing over an imported catalogue stays greppable. A
/// byte key that is not UTF-8 is printed as `0x` hex rather than being
/// lossily decoded into text it does not contain.
fn render_key(key: &TypedKey) -> String {
    match key {
        TypedKey::Text(text) => text.clone(),
        TypedKey::UInt(value) => value.to_string(),
        TypedKey::Int(value) => value.to_string(),
        TypedKey::Bool(value) => value.to_string(),
        TypedKey::Null => "-".to_owned(),
        TypedKey::Bytes(bytes) => render_bytes(bytes),
        TypedKey::Tuple(values) => values.iter().map(render_key).collect::<Vec<_>>().join(","),
        TypedKey::Value(value) => value
            .canonical_text()
            .unwrap_or_else(|| format!("cbor:{}", hex(value.canonical_bytes()))),
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

/// The canonical keys one `--key` spelling names, in the order a lookup tries
/// them: the text spelling, then the same spelling's own bytes, then the bytes
/// a `0x` hex spelling names.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Parses a 32-digit hexadecimal relation id into its 16 raw bytes.
fn parse_relation_id(value: &str) -> Result<[u8; 16], Diagnostic> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(query_error(
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

fn query_error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2000",
        title,
        "run `orna query <relation-hex>` inside an initialized repository",
        detail.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_QUERY_LIMIT, QueryFormat, parse_options, parse_relation_id, render_key};
    use crate::Exit;
    use orna_repository::TypedKey;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn query_options_default_to_one_human_page_of_every_field() {
        let arguments = words(&["000102030405060708090a0b0c0d0e0f"]);
        let parsed = parse_options(&arguments).unwrap();
        assert_eq!(parsed.field, None);
        assert_eq!(parsed.limit, DEFAULT_QUERY_LIMIT);
        assert_eq!(parsed.format, QueryFormat::Human);
    }

    #[test]
    fn query_options_take_one_snapshot_selector_wherever_it_appears() {
        let plain = words(&["000102030405060708090a0b0c0d0e0f"]);
        assert_eq!(parse_options(&plain).unwrap().at, None);
        let pinned = words(&["--at", "HEAD~1", "000102030405060708090a0b0c0d0e0f"]);
        assert_eq!(parse_options(&pinned).unwrap().at, Some("HEAD~1"));
        let trailing = words(&["000102030405060708090a0b0c0d0e0f", "--at", "abc"]);
        assert_eq!(parse_options(&trailing).unwrap().at, Some("abc"));
        // A query names at most one snapshot, and needs a value for it.
        assert!(parse_options(&words(&["a", "--at"])).is_err());
        assert!(parse_options(&words(&["a", "--at", "x", "--at", "y"])).is_err());
    }

    #[test]
    fn query_options_accept_a_field_and_json_format() {
        let arguments = words(&[
            "000102030405060708090a0b0c0d0e0f",
            "--field",
            "2",
            "--limit",
            "5",
            "--format",
            "json",
        ]);
        let parsed = parse_options(&arguments).unwrap();
        assert_eq!(parsed.field, Some(2));
        assert_eq!(parsed.limit, 5);
        assert_eq!(parsed.format, QueryFormat::Json);
    }

    #[test]
    fn query_options_accept_a_key_and_a_field() {
        let arguments = words(&[
            "000102030405060708090a0b0c0d0e0f",
            "--key",
            "song",
            "--field",
            "0",
        ]);
        let parsed = parse_options(&arguments).unwrap();
        assert_eq!(parsed.key, Some("song"));
        assert_eq!(parsed.field, Some(0));
    }

    #[test]
    fn rendered_keys_follow_the_spelling_the_row_carries() {
        assert_eq!(render_key(&TypedKey::Text("song".to_owned())), "song");
        assert_eq!(render_key(&TypedKey::Bytes(b"song".to_vec())), "song");
        assert_eq!(render_key(&TypedKey::Bytes(vec![0xff, 0x00])), "0xff00");
        assert_eq!(render_key(&TypedKey::UInt(7)), "7");
    }

    #[test]
    fn query_rejects_a_limit_outside_the_orp_range_bound() {
        for limit in ["0", "257"] {
            assert!(parse_options(&words(&["00".repeat(16).as_str(), "--limit", limit])).is_err());
        }
        assert!(parse_options(&words(&["00".repeat(16).as_str(), "--limit"])).is_err());
    }

    #[test]
    fn query_requires_exactly_one_relation() {
        assert!(parse_options(&[]).is_err());
        assert!(parse_options(&words(&["aa", "bb"])).is_err());
    }

    #[test]
    fn relation_id_parses_sixteen_bytes_and_rejects_bad_input() {
        let parsed = parse_relation_id("000102030405060708090a0b0c0d0e0f").unwrap();
        assert_eq!(parsed[0], 0);
        assert_eq!(parsed[15], 0x0f);
        assert!(parse_relation_id("0123").is_err());
        assert!(parse_relation_id(&"zz".repeat(16)).is_err());
    }

    #[test]
    fn query_errors_are_target_exit_status() {
        let error = parse_relation_id("nope").unwrap_err();
        assert_eq!(error.exit, Exit::Target);
    }

    #[test]
    fn typed_bounds_parse_integers_and_refuse_bad_spellings() {
        use super::parse_bound_key;
        assert_eq!(parse_bound_key("int:-3").unwrap(), TypedKey::Int(-3));
        assert_eq!(parse_bound_key("uint:7").unwrap(), TypedKey::UInt(7));
        assert!(parse_bound_key("int:x").is_err());
        assert!(parse_bound_key("uint:-1").is_err());
        assert!(parse_bound_key("plain").is_err());
    }

    /// Pages a sorted key set the way the graph does: each read serves the
    /// keys in `[lower, upper)` strictly above the resume cursor, at most the
    /// range's limit of them.
    fn scan_fixture(
        keys: &[TypedKey],
        from: Option<TypedKey>,
        until: Option<TypedKey>,
        page_rows: usize,
    ) -> Result<Vec<TypedKey>, super::Diagnostic> {
        super::scan_pages(
            page_rows,
            from,
            until,
            |range| {
                Ok(keys
                    .iter()
                    .filter(|key| range.lower_inclusive().is_none_or(|lower| *key >= lower))
                    .filter(|key| range.resume_after().is_none_or(|after| *key > after))
                    .filter(|key| range.upper_exclusive().is_none_or(|upper| *key < upper))
                    .take(range.limit())
                    .cloned()
                    .collect())
            },
            std::clone::Clone::clone,
        )
    }

    #[test]
    fn scan_pages_returns_every_key_at_each_production_page_boundary() {
        for count in [0_i64, 1, 255, 256, 257, 512] {
            let keys: Vec<TypedKey> = (0..count).map(TypedKey::Int).collect();
            let scanned = scan_fixture(&keys, None, None, super::MAX_QUERY_LIMIT).unwrap();
            assert_eq!(scanned, keys, "count {count}");
        }
    }

    #[test]
    fn scan_pages_matches_a_single_read_at_small_page_sizes() {
        let keys: Vec<TypedKey> = (0..8_i64).map(TypedKey::Int).collect();
        for page_rows in [1_usize, 2, 3, 4, 5, 8] {
            let scanned = scan_fixture(&keys, None, None, page_rows).unwrap();
            assert_eq!(scanned, keys, "page {page_rows}");
        }
    }

    #[test]
    fn scan_pages_stops_at_an_upper_bound_that_falls_on_a_page_boundary() {
        let keys: Vec<TypedKey> = (0..512_i64).map(TypedKey::Int).collect();
        let scanned = scan_fixture(
            &keys,
            Some(TypedKey::Int(0)),
            Some(TypedKey::Int(256)),
            super::MAX_QUERY_LIMIT,
        )
        .unwrap();
        assert_eq!(scanned, keys[..256].to_vec());
    }

    #[test]
    fn scan_pages_reaches_a_match_held_only_by_a_later_page() {
        let keys: Vec<TypedKey> = (0..512_i64).map(TypedKey::Int).collect();
        let scanned =
            scan_fixture(&keys, Some(TypedKey::Int(250)), Some(TypedKey::Int(260)), 4).unwrap();
        assert_eq!(scanned, (250..260).map(TypedKey::Int).collect::<Vec<_>>());
    }

    #[test]
    fn scan_pages_yields_nothing_for_missing_keys() {
        let keys: Vec<TypedKey> = (0..512_i64).map(TypedKey::Int).collect();
        let past_end = scan_fixture(&keys, Some(TypedKey::Int(1000)), None, 256).unwrap();
        assert!(past_end.is_empty());
        let between =
            scan_fixture(&keys, Some(TypedKey::Int(-5)), Some(TypedKey::Int(-1)), 256).unwrap();
        assert!(between.is_empty());
    }

    #[test]
    fn scan_pages_pages_a_uint_range_across_production_pages() {
        let keys: Vec<TypedKey> = (0..600_u64).map(TypedKey::UInt).collect();
        let scanned = scan_fixture(&keys, None, None, super::MAX_QUERY_LIMIT).unwrap();
        assert_eq!(scanned, keys);
    }

    #[test]
    fn scan_pages_pages_composite_keys_component_wise() {
        let mut keys: Vec<TypedKey> = Vec::new();
        for first in 0..4_i64 {
            for second in ["a", "b", "c"] {
                keys.push(TypedKey::Tuple(vec![
                    TypedKey::Int(first),
                    TypedKey::Text(second.to_owned()),
                ]));
            }
        }
        keys.sort();
        for page_rows in [1_usize, 2, 5, super::MAX_QUERY_LIMIT] {
            let scanned = scan_fixture(&keys, None, None, page_rows).unwrap();
            assert_eq!(scanned, keys, "page {page_rows}");
        }
        let from = TypedKey::Tuple(vec![TypedKey::Int(1), TypedKey::Text("b".to_owned())]);
        let until = TypedKey::Tuple(vec![TypedKey::Int(2), TypedKey::Text("b".to_owned())]);
        let expected: Vec<TypedKey> = keys
            .iter()
            .filter(|key| *key >= &from && *key < &until)
            .cloned()
            .collect();
        assert!(!expected.is_empty());
        let scanned = scan_fixture(&keys, Some(from), Some(until), 2).unwrap();
        assert_eq!(scanned, expected);
    }

    #[test]
    fn scan_pages_reaches_a_uint_key_held_only_by_a_later_page() {
        let keys: Vec<TypedKey> = (0..600_u64).map(TypedKey::UInt).collect();
        let scanned = scan_fixture(
            &keys,
            Some(TypedKey::UInt(300)),
            Some(TypedKey::UInt(600)),
            super::MAX_QUERY_LIMIT,
        )
        .unwrap();
        assert_eq!(scanned, keys[300..].to_vec());
        assert!(
            scanned.len() > super::MAX_QUERY_LIMIT,
            "range spans a second page"
        );
    }

    #[test]
    fn scan_pages_continues_past_a_full_page_at_the_greatest_unsigned_key() {
        let keys: Vec<TypedKey> = (u64::MAX - 255..=u64::MAX).map(TypedKey::UInt).collect();
        assert_eq!(
            scan_fixture(&keys, None, None, super::MAX_QUERY_LIMIT).unwrap(),
            keys
        );
        let short: Vec<TypedKey> = (u64::MAX - 2..=u64::MAX).map(TypedKey::UInt).collect();
        assert_eq!(
            scan_fixture(&short, None, None, super::MAX_QUERY_LIMIT).unwrap(),
            short
        );
    }

    #[test]
    fn typed_bounds_name_the_remedy_for_out_of_range_integers() {
        use super::parse_bound_key;
        let wide = parse_bound_key("int:18446744073709551615").unwrap_err();
        assert!(format!("{wide:?}").contains("uint:"), "{wide:?}");
        let negative = parse_bound_key("uint:-1").unwrap_err();
        assert!(format!("{negative:?}").contains("int:"), "{negative:?}");
    }
}
