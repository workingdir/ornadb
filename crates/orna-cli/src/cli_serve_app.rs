//! The selected source-hosted serving entry (ORNA-APP-010, DESIGN S1).
//!
//! `orna serve` admits the project's `main.orna` module and invokes its
//! `serve` function for `GET /`. The invocation is staged and read-only: a
//! `serve` that attempts a table write is refused, and nothing is committed.
//! Each request serves this clone's HEAD and CWD (ORNA-SERVE-003): the module
//! and the committed table identities are read from the committed `main.orna`
//! at that request's HEAD commit, and the committed rows are overlaid with the
//! CWD generation the runtime captures for that one activation (ORNA-TXN-005).
//! Durable CWD changes are visible; the partial writes of an uncommitted
//! activation are not (ORNA-TXN-005). Without that entry the server keeps its
//! Git transport, installs no page at `/`, and reports the absence once at
//! startup.

use super::apply_payload_policy;
use super::{
    CommittedRowsError, CommittedTableIdentity, Diagnostic, Limits, Mutex, PinnedCommittedRows,
    PinnedCommittedTables, Request, Response, RuntimeState, admitted_table_schemas,
    committed_rows_at, committed_table_identities, evaluate_over_committed_tables,
    legacy_bare_module_candidates, legacy_bare_module_table, runtime_identity,
    unavailable_response,
};
use crate::media_range::{MediaMethod, MediaReply, media_reply, plan_media_response};
use orna_application::{ApplicationAuthority, ApplicationError, StagedActivation};
use orna_evaluator::Environment;
use orna_repository::{GitHashAlgorithm, KeyRange, NativeOid};
use orna_serving::media_preview::{
    MAX_PREVIEW_INPUT_BYTES, MAX_WAVE_INPUT_BYTES, PreviewError, WaveInspectionError,
    decode_image_preview, inspect_pcm_wave,
};

/// Repository-relative module that hosts the serving entry.
pub(super) const SERVE_MODULE_PATH: &str = "main.orna";
/// The function invoked for each `GET /`.
pub(super) const SERVE_ENTRY: &str = "serve";
/// Path prefix of the explicit media route (ORNA-APP-013, ORNA-APP-014).
const MEDIA_PREFIX: &str = "/media/";
const PREVIEW_PREFIX: &str = "/preview/";
/// Rows read per page while locating a media descriptor at HEAD.
const MEDIA_ROW_PAGE: usize = 64;
/// Bytes read from the repository per verified range read.
const MEDIA_READ_CHUNK: u64 = 1024 * 1024;

/// The admitted serving entry's authority. Each request re-admits the module
/// from its committed HEAD source, so the startup admission only proves that
/// the working tree declares a public `serve`.
pub(super) struct ServeApp {
    authority: ApplicationAuthority,
    /// Project root whose committed `HEAD` supplies the module and rows.
    root: String,
}

impl ServeApp {
    /// Admits the selected entry. Returns `Ok(None)` when the project has no
    /// `main.orna` module or that module declares no `serve` function.
    pub(super) fn admit(
        root: &str,
        project: &orna_project::LoadedProject,
        catalogue: &orna_semantic::Catalogue,
    ) -> Result<Option<Self>, Diagnostic> {
        let Some(index) = project
            .identities()
            .iter()
            .position(|identity| identity.logical_path() == SERVE_MODULE_PATH)
        else {
            return Ok(None);
        };
        // Admission analyses the entry module alone, so the standard modules
        // the project pins must reach the authority through the catalogue's
        // standard declarations; the project's own profile and sources are
        // used, never the host library (ORNA-APP-005, ORNA-APP-007).
        let mut authority = ApplicationAuthority::new(catalogue.clone(), Limits::default());
        if let Some(profile) = project.standard_profile() {
            authority = authority
                .with_standard_sources(profile, project.standard_sources().iter().cloned())
                .unwrap_or_else(|_| {
                    ApplicationAuthority::new(catalogue.clone(), Limits::default())
                });
        }
        match authority.admit_module(
            SERVE_MODULE_PATH,
            project.modules()[index].source.clone(),
            SERVE_ENTRY,
        ) {
            Ok(_) if serve_is_public(&project.modules()[index].source) => Ok(Some(Self {
                authority,
                root: root.to_owned(),
            })),
            // A private `serve` is not the serving entry (ORNA-APP-001 explicit root `pub fn serve()`).
            Ok(_) | Err(ApplicationError::MissingEntry(_)) => Ok(None),
            Err(error) => {
                let detail = match &error {
                    ApplicationError::Semantic(codes) => format!("{error}: {}", codes.join(", ")),
                    _ => error.to_string(),
                };
                Err(Diagnostic::target_with_detail(
                    "E2101",
                    "serving module failed semantic analysis",
                    "fix the first reported source contract error in main.orna, then retry `serve`",
                    detail,
                ))
            }
        }
    }

    /// Invokes `serve` read-only and renders its value as the `GET /` body.
    /// A `serve` that stages any table mutation is refused.
    fn respond(&self) -> Response {
        let (staged, commit) = match self.stage_at_head() {
            Ok(staged) => staged,
            Err(code) => return serve_error(code),
        };
        if !staged.mutations().is_empty() {
            return serve_error("serve.write_refused");
        }
        // A `std.ui` node tree renders as semantic HTML; every other value
        // keeps the Inspect text fallback.
        match crate::ui_render::render_value_html(staged.value().raw(), Some(commit.as_str())) {
            Some(html) => Response::new(200, "text/html; charset=utf-8", html.into_bytes()),
            None => Response::new(
                200,
                "text/plain; charset=utf-8",
                crate::repl::inspect(staged.value()).into_bytes(),
            ),
        }
    }

    #[allow(clippy::too_many_lines)]
    /// Stages `serve` for one request at this clone's HEAD and CWD.
    ///
    /// The request takes no per-request lock. The argument that one read cannot
    /// mix generations has three steps, in this order:
    /// 1. The published pin is read before the HEAD commit is captured. A
    ///    publication after the pin moves the published mutation sequence away
    ///    from that pin.
    /// 2. Every committed row is read from that one captured commit, an
    ///    immutable Git object, so no row can change under the read.
    /// 3. The activation's transaction (`load_tx` in the runtime) captures the
    ///    CWD generation and re-reads the published sequence in one immediate
    ///    transaction. A sequence that differs from the pin fails closed as
    ///    `StaleCapture`, which this request reports as the coded 500
    ///    `serve.evaluation_failed` with no value.
    ///
    /// The served process holds the runtime owner lock, so an ordinary write
    /// cannot change the CWD while it serves. A publish that does not take the
    /// lease is the remaining writer path, and step 3 covers it.
    ///
    /// The committed `main.orna` at HEAD is admitted, its tables are bound to
    /// their committed identities, and the committed rows are overlaid with the
    /// CWD generation captured for this activation. A missing or unreadable HEAD
    /// entry is `serve.entry_unavailable`; a table whose committed identity is
    /// missing, or present for only some tables, is refused before any read.
    fn stage_at_head(
        &self,
    ) -> Result<(StagedActivation, orna_repository::GitCommitRef), &'static str> {
        let repository = orna_repository::Repository::discover(&self.root)
            .map_err(|_| "serve.entry_unavailable")?;
        // An unborn HEAD is reported before any runtime state opens.
        let _ = Self::head_commit(&repository)?;
        let (identity, initial_digest) =
            runtime_identity(&repository).map_err(|_| "serve.table_rows_unavailable")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "serve.table_rows_unavailable")?;
        // The served process holds the runtime owner lock, so this per-request
        // handle opens as a secondary reader: it reads the CWD and the published
        // pin but never writes or fences the owner.
        let state = runtime
            .block_on(RuntimeState::open(&repository, identity, initial_digest))
            .map_err(|_| "serve.table_rows_unavailable")?;
        // Step 1: the pin is read before the HEAD capture below.
        let pin = runtime
            .block_on(state.published_pin())
            .map_err(|_| "serve.table_rows_unavailable")?;
        // Step 2: every committed read below uses this one captured commit.
        let commit = Self::head_commit(&repository)?;
        let bytes = repository
            .read_committed_file(
                &commit,
                SERVE_MODULE_PATH,
                orna_project::ProjectLimits::default().max_source_bytes,
            )
            .map_err(|_| "serve.entry_unavailable")?;
        let source = String::from_utf8(bytes).map_err(|_| "serve.entry_unavailable")?;
        if !serve_is_public(&source) {
            return Err("serve.entry_unavailable");
        }
        let base = self
            .authority
            .admit_module(SERVE_MODULE_PATH, source.clone(), SERVE_ENTRY)
            .map_err(|_| "serve.entry_unavailable")?;
        let names = base.table_schemas().keys().cloned().collect::<Vec<_>>();
        if names.is_empty() {
            return self
                .authority
                .evaluate_staged(&base, &Environment::new())
                .map(|staged| (staged, commit))
                .map_err(|_| "serve.evaluation_failed");
        }
        let Ok(tables) = committed_table_identities(&repository, &commit, names.clone()) else {
            let bound = names
                .iter()
                .filter(|name| {
                    committed_table_identities(&repository, &commit, vec![(*name).clone()]).is_ok()
                })
                .count();
            return Err(if bound == 0 || bound == names.len() {
                "serve.table_identity_unavailable"
            } else {
                "serve.mixed_table_identity"
            });
        };
        let table_names = names.iter().map(String::as_str).collect::<Vec<_>>();
        let candidates = legacy_bare_module_candidates(&repository, Some(&commit), &table_names)
            .map_err(|_| "serve.table_rows_unavailable")?;
        if legacy_bare_module_table(&state, &runtime, &candidates)
            .map_err(|_| "serve.table_rows_unavailable")?
            .is_some()
        {
            return Err("serve.table_rows_unavailable");
        }
        let committed = tables
            .iter()
            .map(|table| CommittedTableIdentity::new(table.table(), table.object_id()));
        let admitted = self
            .authority
            .admit_module_with_committed_table_metadata(
                SERVE_MODULE_PATH,
                &source,
                SERVE_ENTRY.to_owned(),
                committed,
            )
            .map_err(|_| "serve.evaluation_failed")?;
        if admitted.requires_publication_metadata() {
            return Err("serve.table_read_unsupported");
        }
        let schemas = admitted_table_schemas(&admitted);
        let (rows, storage_orders) = committed_rows_at(&repository, &commit, &schemas, &tables)
            .map_err(|error| match error {
                CommittedRowsError::Limit => "serve.table_read_limit",
                CommittedRowsError::Unavailable => "serve.table_rows_unavailable",
            })?;
        // A stored Blob reference has no portable value to serve; refuse rather than
        // expose its placeholder as committed data.
        for (name, table) in admitted.table_schemas() {
            let stored = rows.get(name).map(Vec::as_slice).unwrap_or_default();
            let order = storage_orders
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if orna_project::stored_rows_hold_blob(table.schema(), order, stored)
                .map_err(|_| "serve.table_rows_unavailable")?
            {
                return Err("serve.table_read_unsupported");
            }
        }
        // Step 3 is the activation's `load_tx` fence over this pin.
        let committed = PinnedCommittedTables {
            rows: PinnedCommittedRows::new(pin, rows)
                .map_err(|_| "serve.table_rows_unavailable")?,
            storage_orders,
        };
        let (_snapshot, staged) = evaluate_over_committed_tables(
            &runtime,
            &state,
            &self.authority,
            &admitted,
            &tables,
            committed,
            None,
        )
        .map_err(|_| "serve.evaluation_failed")?;
        Ok((staged, commit))
    }

    /// The HEAD commit of `repository`. An unborn HEAD has no commit to serve
    /// (ORNA-SERVE-003) and is `serve.entry_unavailable`.
    fn head_commit(
        repository: &orna_repository::Repository,
    ) -> Result<orna_repository::GitCommitRef, &'static str> {
        repository
            .head()
            .ok()
            .flatten()
            .ok_or("serve.entry_unavailable")
    }

    /// Serves one committed media descriptor at HEAD (ORNA-APP-013, ORNA-APP-014).
    ///
    /// The descriptor is authorised only when a committed row of the HEAD
    /// `main.orna` tables stores a blob reference to it: the row scan is the
    /// authority check, and the response never names a path or a CWD object.
    /// Bytes are read as verified ranges from the immutable HEAD graph, and the
    /// descriptor OID is the validator (`ETag`), so the validator names exactly
    /// the bytes served.
    #[allow(clippy::too_many_lines)]
    fn media_at_head(
        &self,
        use_: MediaUse<'_>,
        commit: &str,
        descriptor: &str,
    ) -> Result<Response, (u16, &'static str)> {
        let not_found = (404, "media.not_found");
        let unavailable = (500, "media.read_unavailable");
        let repository = orna_repository::Repository::discover(&self.root)
            .map_err(|_| (500, "media.entry_unavailable"))?;
        let head = Self::head_commit(&repository).map_err(|_| (500, "media.entry_unavailable"))?;
        if head.as_str() != commit {
            return Err(not_found);
        }
        let algorithm = match descriptor.len() {
            40 => GitHashAlgorithm::Sha1,
            64 => GitHashAlgorithm::Sha256,
            _ => return Err(not_found),
        };
        let descriptor = NativeOid::from_hex(algorithm, descriptor).map_err(|_| not_found)?;
        // Blob metadata carries the value-layer OID; the same bytes name the object.
        let value_descriptor =
            orna_value::NativeOid::from_bytes(descriptor.as_bytes()).map_err(|_| not_found)?;
        let bytes = repository
            .read_committed_file(
                &head,
                SERVE_MODULE_PATH,
                orna_project::ProjectLimits::default().max_source_bytes,
            )
            .map_err(|_| (500, "media.entry_unavailable"))?;
        let source = String::from_utf8(bytes).map_err(|_| (500, "media.entry_unavailable"))?;
        if !serve_is_public(&source) {
            return Err((500, "media.entry_unavailable"));
        }
        let base = self
            .authority
            .admit_module(SERVE_MODULE_PATH, source, SERVE_ENTRY)
            .map_err(|_| (500, "media.entry_unavailable"))?;
        let names = base.table_schemas().keys().cloned().collect::<Vec<_>>();
        if names.is_empty() {
            return Err(not_found);
        }
        let tables = committed_table_identities(&repository, &head, names)
            .map_err(|_| (500, "media.table_identity_unavailable"))?;
        let format = repository
            .open_pinned_format_context(head.as_str())
            .map_err(|_| unavailable)?;
        for table in &tables {
            let row_map = format
                .load_row_map(table.object_id().bytes())
                .map_err(|_| unavailable)?;
            let graph = format
                .open_native_graph(&row_map)
                .map_err(|_| unavailable)?;
            let scope = graph.open_read_scope().map_err(|_| unavailable)?;
            let mut lower = None;
            loop {
                let page_range =
                    KeyRange::new(lower.clone(), None, MEDIA_ROW_PAGE).map_err(|_| unavailable)?;
                let page = graph
                    .range_rows(&page_range, &scope)
                    .map_err(|_| unavailable)?;
                if page.is_empty() {
                    break;
                }
                let next_lower = page.last().map(|row| row.key().clone());
                for row in page.iter().skip(usize::from(lower.is_some())) {
                    let fields = row.blob_fields().map_err(|_| unavailable)?;
                    let Some((_, metadata)) = fields
                        .iter()
                        .find(|(_, metadata)| metadata.descriptor_oid() == Some(&value_descriptor))
                    else {
                        continue;
                    };
                    let reference = graph
                        .admit_blob_reference(row, &descriptor, &scope)
                        .map_err(|_| unavailable)?;
                    let length = metadata.length();
                    let media_type = metadata.media_type().to_owned();
                    let suffix = metadata.suffix().map(str::to_owned);
                    let read_span = |start: u64, end: u64| -> Result<Vec<u8>, ()> {
                        let mut out = Vec::new();
                        let mut cursor = start;
                        while cursor <= end {
                            let last = end.min(cursor.saturating_add(MEDIA_READ_CHUNK - 1));
                            let expected = last - cursor + 1;
                            let verified = graph
                                .read_blob_range(&reference, cursor..last + 1, &scope)
                                .map_err(|_| ())?;
                            // A chunk that is not the requested length is a truncated stream.
                            if u64::try_from(verified.bytes().len()) != Ok(expected) {
                                return Err(());
                            }
                            out.extend_from_slice(verified.bytes());
                            cursor = last + 1;
                        }
                        Ok(out)
                    };
                    let (method, range) = match use_ {
                        MediaUse::Bytes { method, range } => (method, range),
                        MediaUse::Preview => {
                            return preview_representation(
                                &media_type,
                                suffix.as_deref(),
                                length,
                                read_span,
                            );
                        }
                    };
                    check_media_span(method, range, length, MEDIA_MAX_BODY_BYTES)?;
                    let reply =
                        media_reply(method, range, length, read_span).map_err(|_| unavailable)?;
                    return Ok(media_response(reply, &media_type, &descriptor));
                }
                if next_lower == lower {
                    break;
                }
                lower = next_lower;
            }
        }
        Err(not_found)
    }
}

/// The `serve` error response: a JSON body naming a stable code, never a value.
fn serve_error(code: &str) -> Response {
    Response::new(
        500,
        "application/json",
        format!(r#"{{"error":"{code}"}}"#).into_bytes(),
    )
}

/// Answers `/` through the serving entry when one is installed. Returns
/// `None` for every other request, which the Git transport then handles.
/// Every response this route produces (including 405 and 500) carries
/// `X-Content-Type-Options: nosniff`.
pub(super) fn serving_entry_route(
    app: Option<&Mutex<ServeApp>>,
    request: &Request,
) -> Option<Response> {
    let mut response = serving_entry_route_inner(app, request)?;
    super::ensure_header(&mut response, "X-Content-Type-Options", "nosniff");
    Some(response)
}

fn serving_entry_route_inner(app: Option<&Mutex<ServeApp>>, request: &Request) -> Option<Response> {
    let app = app?;
    if let Some(suffix) = request.path.strip_prefix(MEDIA_PREFIX) {
        return Some(media_route(app, request, suffix));
    }
    if let Some(suffix) = request.path.strip_prefix(PREVIEW_PREFIX) {
        return Some(preview_route(app, request, suffix));
    }
    if request.path != "/" {
        return None;
    }
    if request.method != "GET" && request.method != "HEAD" {
        return Some(Response::new(
            405,
            "application/json",
            br#"{"error":"method_not_allowed"}"#.to_vec(),
        ));
    }
    // RFC 9110 9.3.2: HEAD answers exactly as GET but sends no content.
    let head = request.method == "HEAD";
    Some(match app.lock() {
        Ok(app) => {
            let mut response = app.respond();
            if head {
                response.body.clear();
            }
            response
        }
        Err(_) => unavailable_response(),
    })
}

/// Answers `/media/<commit>/<descriptor>` at HEAD (ORNA-APP-013, ORNA-APP-014).
/// Only GET and HEAD are served; every other method is refused.
fn media_route(app: &Mutex<ServeApp>, request: &Request, suffix: &str) -> Response {
    if request.method != "GET" && request.method != "HEAD" {
        return Response::new(
            405,
            "application/json",
            br#"{"error":"method_not_allowed"}"#.to_vec(),
        );
    }
    let method = if request.method == "HEAD" {
        MediaMethod::Head
    } else {
        MediaMethod::Get
    };
    let range = request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("range"))
        .map(|(_, value)| value.as_str());
    let Some((commit, descriptor)) = suffix.split_once('/') else {
        return media_error(404, "media.not_found");
    };
    let mut response = match app.lock() {
        Ok(app) => match app.media_at_head(MediaUse::Bytes { method, range }, commit, descriptor) {
            Ok(response) => response,
            Err((status, code)) => media_error(status, code),
        },
        Err(_) => unavailable_response(),
    };
    // RFC 9110 9.3.2: HEAD carries the same status and headers, never a body.
    if method == MediaMethod::Head {
        response.body.clear();
    }
    response
}

/// What an authorised descriptor is read for: its bytes, or one bounded decode.
#[derive(Clone, Copy)]
enum MediaUse<'a> {
    Bytes {
        method: MediaMethod,
        range: Option<&'a str>,
    },
    Preview,
}

/// Answers `/preview/<commit>/<descriptor>` at HEAD: a bounded decode of one
/// committed image or PCM WAVE Blob, summarised as JSON (ORNA-MEDIA-002,
/// ORNA-MEDIA-005). The descriptor is authorised exactly as `/media` is, and
/// only GET and HEAD are served.
fn preview_route(app: &Mutex<ServeApp>, request: &Request, suffix: &str) -> Response {
    if request.method != "GET" && request.method != "HEAD" {
        return Response::new(
            405,
            "application/json",
            br#"{"error":"method_not_allowed"}"#.to_vec(),
        );
    }
    let Some((commit, descriptor)) = suffix.split_once('/') else {
        return media_error(404, "media.not_found");
    };
    let mut response = match app.lock() {
        Ok(app) => match app.media_at_head(MediaUse::Preview, commit, descriptor) {
            Ok(response) => response,
            Err((status, code)) => media_error(status, code),
        },
        Err(_) => unavailable_response(),
    };
    // RFC 9110 9.3.2: HEAD carries the same status and headers, never a body.
    if request.method == "HEAD" {
        response.body.clear();
    }
    response
}

/// Decodes one authorised representation through the bounded preview decoders.
/// The media-type split and the input cap are decided before any byte is read,
/// so an oversized or non-previewable Blob costs no read.
fn preview_representation(
    media_type: &str,
    suffix: Option<&str>,
    length: u64,
    read_span: impl FnOnce(u64, u64) -> Result<Vec<u8>, ()>,
) -> Result<Response, (u16, &'static str)> {
    let essence = media_type.split(';').next().unwrap_or_default().trim();
    if essence.starts_with("image/") {
        if length > MAX_PREVIEW_INPUT_BYTES {
            return Err((413, "media.preview.resource_limit"));
        }
        let blob = preview_blob(media_type, suffix, length, read_span)?;
        return decode_image_preview(&blob)
            .map(|image| {
                preview_json(&serde_json::json!({
                    "kind": "image",
                    "width": image.width,
                    "height": image.height,
                }))
            })
            .map_err(image_preview_error);
    }
    if essence.starts_with("audio/") {
        if length > MAX_WAVE_INPUT_BYTES {
            return Err((413, "media.preview.resource_limit"));
        }
        let blob = preview_blob(media_type, suffix, length, read_span)?;
        return inspect_pcm_wave(&blob)
            .map(|props| {
                preview_json(&serde_json::json!({
                    "kind": "pcm_wave",
                    "channels": props.channels,
                    "sample_rate": props.sample_rate,
                    "bits_per_sample": props.bits_per_sample,
                    "frame_count": props.frame_count,
                    "duration_nanos": props.duration_nanos,
                    "embedded_title": props.embedded_title,
                }))
            })
            .map_err(wave_preview_error);
    }
    Err((415, "media.preview.unsupported"))
}

/// Reads one verified representation into an inline Blob for a decoder.
fn preview_blob(
    media_type: &str,
    suffix: Option<&str>,
    length: u64,
    read_span: impl FnOnce(u64, u64) -> Result<Vec<u8>, ()>,
) -> Result<orna_value::Blob, (u16, &'static str)> {
    let bytes = if length == 0 {
        Vec::new()
    } else {
        read_span(0, length - 1).map_err(|()| (503, "media.preview.unavailable"))?
    };
    orna_value::Blob::from_bytes_with_annotation(bytes, media_type, suffix)
        .map_err(|_| (415, "media.preview.unsupported"))
}

fn preview_json(value: &serde_json::Value) -> Response {
    Response::new(200, "application/json", value.to_string().into_bytes())
}

/// Maps an image decode failure to its status and stable `media.preview.*` code.
fn image_preview_error(error: PreviewError) -> (u16, &'static str) {
    match error {
        PreviewError::UnsupportedMediaType => (415, "media.preview.unsupported"),
        PreviewError::Malformed => (422, "media.preview.malformed"),
        PreviewError::InputTooLarge | PreviewError::ResourceLimit => {
            (413, "media.preview.resource_limit")
        }
        PreviewError::ContentUnavailable => (503, "media.preview.unavailable"),
    }
}

/// Maps a PCM WAVE inspection failure to its status and stable code.
fn wave_preview_error(error: WaveInspectionError) -> (u16, &'static str) {
    match error {
        WaveInspectionError::UnsupportedFormat => (415, "media.preview.unsupported"),
        WaveInspectionError::MalformedContent => (422, "media.preview.malformed"),
        WaveInspectionError::ResourceLimit => (413, "media.preview.resource_limit"),
        WaveInspectionError::ContentUnavailable => (503, "media.preview.unavailable"),
    }
}

#[cfg(test)]
mod preview_status_tests {
    use super::{image_preview_error, preview_representation, wave_preview_error};
    use orna_serving::media_preview::{
        MAX_PREVIEW_INPUT_BYTES, MAX_WAVE_INPUT_BYTES, PreviewError, WaveInspectionError,
    };

    #[test]
    fn resource_bounds_are_413_and_unavailable_content_is_503() {
        assert_eq!(
            image_preview_error(PreviewError::InputTooLarge),
            (413, "media.preview.resource_limit")
        );
        assert_eq!(
            image_preview_error(PreviewError::ResourceLimit),
            (413, "media.preview.resource_limit")
        );
        assert_eq!(
            image_preview_error(PreviewError::ContentUnavailable),
            (503, "media.preview.unavailable")
        );
        assert_eq!(
            wave_preview_error(WaveInspectionError::ResourceLimit),
            (413, "media.preview.resource_limit")
        );
        assert_eq!(
            wave_preview_error(WaveInspectionError::ContentUnavailable),
            (503, "media.preview.unavailable")
        );
    }

    #[test]
    fn malformed_content_is_422_and_unsupported_format_is_415() {
        assert_eq!(
            image_preview_error(PreviewError::Malformed),
            (422, "media.preview.malformed")
        );
        assert_eq!(
            image_preview_error(PreviewError::UnsupportedMediaType),
            (415, "media.preview.unsupported")
        );
        assert_eq!(
            wave_preview_error(WaveInspectionError::MalformedContent),
            (422, "media.preview.malformed")
        );
        assert_eq!(
            wave_preview_error(WaveInspectionError::UnsupportedFormat),
            (415, "media.preview.unsupported")
        );
    }

    #[test]
    fn oversized_image_and_audio_are_refused_before_any_read() {
        let image = preview_representation(
            "image/png",
            None,
            MAX_PREVIEW_INPUT_BYTES + 1,
            |_: u64, _: u64| -> Result<Vec<u8>, ()> { unreachable!("no read above the cap") },
        );
        assert!(matches!(image, Err((413, "media.preview.resource_limit"))));
        let audio = preview_representation(
            "audio/wav",
            None,
            MAX_WAVE_INPUT_BYTES + 1,
            |_: u64, _: u64| -> Result<Vec<u8>, ()> { unreachable!("no read above the cap") },
        );
        assert!(matches!(audio, Err((413, "media.preview.resource_limit"))));
    }
}

/// A media error response: a JSON body naming a stable `media.*` code.
fn media_error(status: u16, code: &str) -> Response {
    Response::new(
        status,
        "application/json",
        format!(r#"{{"error":"{code}"}}"#).into_bytes(),
    )
}

/// Largest media body one response buffers (64 MiB). A selected span above it
/// is refused with `413 media.response_too_large` before any byte is read; the
/// client reads a larger payload as several `Range` requests.
const MEDIA_MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// Refuses a response whose planned body is larger than `cap`. The plan is
/// computed from the range alone, so an oversized span costs no read.
fn check_media_span(
    method: MediaMethod,
    range: Option<&str>,
    length: u64,
    cap: u64,
) -> Result<(), (u16, &'static str)> {
    let plan = plan_media_response(method, range, length);
    if plan.send_body && plan.content_length > cap {
        return Err((413, "media.response_too_large"));
    }
    Ok(())
}

/// Builds the media response from the plan: the status and range headers, the
/// descriptor OID as the validator, and the payload policy (an attachment with
/// `nosniff` and a sandbox unless the media type is allowlisted for inline).
fn media_response(reply: MediaReply, media_type: &str, descriptor: &NativeOid) -> Response {
    let mut response = Response::new(reply.plan.status, media_type, reply.body);
    apply_payload_policy(&mut response, media_type);
    response
        .headers
        .push(("accept-ranges".into(), "bytes".into()));
    response.headers.push((
        "content-length".into(),
        reply.plan.content_length.to_string(),
    ));
    if let Some(content_range) = &reply.plan.content_range {
        response
            .headers
            .push(("content-range".into(), content_range.clone()));
    }
    response
        .headers
        .push(("etag".into(), format!("\"{}\"", descriptor.to_hex())));
    response
}

/// True when `source` declares `serve` as a `pub fn` (the only serving entry).
fn serve_is_public(source: &str) -> bool {
    orna_syntax::parse_module(source).value.items.iter().any(|item| {
        matches!(item.visibility, orna_syntax::Visibility::Public { .. })
            && matches!(
                &item.declaration,
                orna_syntax::Declaration::Function { signature, .. } if signature.name == SERVE_ENTRY
            )
    })
}

/// The startup diagnostic for a project without a serving entry.
pub(super) fn missing_entry_diagnostic() -> Diagnostic {
    Diagnostic::target(
        "E2102",
        "no serving entry: main.orna does not declare `pub fn serve()`",
        "add `pub fn serve()` to main.orna to serve `/`; Git transport remains available",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_above_the_cap_is_refused_before_any_read() {
        assert_eq!(
            check_media_span(MediaMethod::Get, None, 100, 64),
            Err((413, "media.response_too_large"))
        );
        assert_eq!(
            check_media_span(MediaMethod::Get, Some("bytes=0-63"), 100, 64),
            Ok(()),
            "a 64-byte span is exactly at the cap"
        );
        assert_eq!(
            check_media_span(MediaMethod::Get, Some("bytes=0-63"), 100, 63),
            Err((413, "media.response_too_large")),
        );
    }

    #[test]
    fn head_carries_no_body_so_the_cap_does_not_apply() {
        assert_eq!(check_media_span(MediaMethod::Head, None, 100, 64), Ok(()));
    }

    #[test]
    fn payload_policy_sets_every_header_on_a_fresh_non_inline_response() {
        let mut response = Response::new(200, "image/svg+xml", Vec::new());
        apply_payload_policy(&mut response, "image/svg+xml");
        assert_eq!(response.content_type, "application/octet-stream");
        let value = |name: &str| {
            response
                .headers
                .iter()
                .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("content-disposition"), Some("attachment"));
        assert_eq!(value("x-content-type-options"), Some("nosniff"));
        assert_eq!(value("content-security-policy"), Some("sandbox"));
    }

    #[test]
    fn payload_policy_attaches_non_inline_types_once() {
        let mut response = Response::new(200, "image/svg+xml", Vec::new());
        response
            .headers
            .push(("x-content-type-options".into(), "nosniff".into()));
        apply_payload_policy(&mut response, "image/svg+xml");
        assert_eq!(response.content_type, "application/octet-stream");
        let count = |name: &str| {
            response
                .headers
                .iter()
                .filter(|(existing, _)| existing.eq_ignore_ascii_case(name))
                .count()
        };
        assert_eq!(count("x-content-type-options"), 1);
        assert_eq!(count("content-disposition"), 1);
        assert_eq!(count("content-security-policy"), 1);
    }
}
