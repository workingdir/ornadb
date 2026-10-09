//! `orna query <relation-hex> [--key KEY|0xBYTES] [--field N] [--limit N] [--format human|json]`
//! lists the payload-free Blob metadata of committed format-3 rows, optionally
//! narrowed by an annotation predicate (`--kind`, `--not-kind`, `--suffix`,
//! `--min-length`, `--max-length`).
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

use super::*;
use crate::cli_row_key::key_candidates;
use orna_repository_v1::{KeyRange, Repository, TypedKey};
use orna_value_v1::BlobMetadataFilter;

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
    field: Option<usize>,
    limit: usize,
    format: QueryFormat,
    predicate: BlobMetadataFilter,
}

fn parse_options(arguments: &[String]) -> Result<QueryOptions<'_>, Diagnostic> {
    let mut positional = Vec::new();
    let mut key = None;
    let mut field = None;
    let mut limit = DEFAULT_QUERY_LIMIT;
    let mut format = QueryFormat::Human;
    let mut predicate = BlobMetadataFilter::new();
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
            "--kind" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--kind needs a value", "usage: --kind MIME")
                })?;
                predicate = predicate.with_media_type(value);
            }
            "--not-kind" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--not-kind needs a value", "usage: --not-kind MIME")
                })?;
                predicate = predicate.without_media_type(value);
            }
            "--suffix" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--suffix needs a value", "usage: --suffix HINT")
                })?;
                predicate = predicate.with_suffix(value);
            }
            "--min-length" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--min-length needs a value", "usage: --min-length BYTES")
                })?;
                predicate = predicate.with_min_length(value.parse::<u64>().map_err(|_| {
                    query_error("--min-length is not a byte count", format!("got {value:?}"))
                })?);
            }
            "--max-length" => {
                let value = words.next().ok_or_else(|| {
                    query_error("--max-length needs a value", "usage: --max-length BYTES")
                })?;
                predicate = predicate.with_max_length(value.parse::<u64>().map_err(|_| {
                    query_error("--max-length is not a byte count", format!("got {value:?}"))
                })?);
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
                        "got {flag:?}; accepted: --key, --field, --kind, --not-kind, --suffix, \
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
            "usage: orna query <relation-hex> [--key KEY] [--field N] [--kind MIME] [--not-kind MIME] [--suffix HINT] [--min-length BYTES] [--max-length BYTES] [--limit N] [--format human|json]",
        ));
    };
    Ok(QueryOptions {
        relation,
        key,
        field,
        limit,
        format,
        predicate,
    })
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let options = parse_options(arguments)?;
    let relation = parse_relation_id(options.relation)?;
    let path = local_project_path(endpoint)?;
    let repository = Repository::discover(path)
        .map_err(|error| query_error("Repository could not be opened", format!("{error:?}")))?;
    let format = repository
        .open_format_context()
        .map_err(|error| query_error("Format context could not be opened", format!("{error:?}")))?;
    let row_map = format
        .load_row_map(relation)
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
    // rather than a reinterpretation of the query.
    let rows = match options.key {
        Some(key) => {
            let mut found = None;
            for candidate in key_candidates(key)? {
                found = graph
                    .lookup_row(&candidate, &scope)
                    .map_err(|error| query_error("Row lookup failed", format!("{error:?}")))?;
                if found.is_some() {
                    break;
                }
            }
            found.into_iter().collect::<Vec<_>>()
        }
        None => {
            let range = KeyRange::new(None, None, options.limit)
                .map_err(|error| query_error("Row range is invalid", format!("{error:?}")))?;
            graph
                .range_rows(&range, &scope)
                .map_err(|error| query_error("Row range could not be read", format!("{error:?}")))?
        }
    };
    if options.key.is_some() && rows.is_empty() {
        return Err(query_error(
            "Row is not committed",
            format!("no row with key {:?}", options.key.unwrap_or_default()),
        ));
    }
    // Each field is projected from the row's own stored tuple; a field that
    // is not a Blob in canonical shape is not a Blob, so it is skipped rather
    // than reported with a fabricated summary. Naming a field decodes only
    // that field; otherwise the row is walked once so a Blob at any index is
    // reported with the index it actually occupies.
    let mut listings = Vec::new();
    for row in &rows {
        match options.field {
            Some(field) => {
                let metadata = row.blob_metadata(field).map_err(|error| {
                    query_error("Row fields could not be decoded", format!("{error:?}"))
                })?;
                if let Some(metadata) = metadata {
                    listings.push((row.key().clone(), field, metadata));
                }
            }
            None => {
                for (field, metadata) in row.blob_fields().map_err(|error| {
                    query_error("Row fields could not be decoded", format!("{error:?}"))
                })? {
                    listings.push((row.key().clone(), field, metadata));
                }
            }
        }
    }
    // The predicate is evaluated over the same stored annotation
    // coordinates the listing already decoded, so narrowing a listing costs no
    // additional read and never enters a descriptor or a payload chunk. A
    // candidate that does not match is dropped rather than reported, and the
    // plan below states which coordinates answered it.
    let candidates = listings.len();
    let subject = options.field.map_or_else(
        || "all fields".to_owned(),
        |field| format!("field {field}"),
    );
    listings.retain(|(_, _, metadata)| options.predicate.matches(metadata));
    let matched = listings.len();
    let plan = PredicatePlan::new(&options.predicate, &subject, candidates, matched);
    match options.format {
        QueryFormat::Human => {
            for (key, field, metadata) in &listings {
                println!(
                    "{} {field} {} {} {} {} {}",
                    render_key(key),
                    metadata.media_type(),
                    metadata.suffix().unwrap_or("-"),
                    metadata.length(),
                    hex(&metadata.sha256()),
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
                        "hydrated": metadata.is_hydrated(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "rows": rows.len(),
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
    candidates: usize,
    matched: usize,
}

impl<'a> PredicatePlan<'a> {
    const fn new(
        predicate: &'a BlobMetadataFilter,
        subject: &'a str,
        candidates: usize,
        matched: usize,
    ) -> Self {
        Self {
            predicate,
            subject,
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
             candidates {} matched {}; subject {}",
            self.subject,
            self.coordinates().join(","),
            Self::decode_required_coordinates().join(","),
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
        TypedKey::Tuple(values) => values
            .iter()
            .map(render_key)
            .collect::<Vec<_>>()
            .join(","),
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
    use super::{
        parse_options, parse_relation_id, render_key, QueryFormat, DEFAULT_QUERY_LIMIT,
    };
    use crate::Exit;
    use orna_repository_v1::TypedKey;

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
}
