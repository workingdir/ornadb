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

use super::*;
use orna_application_v1::{ApplicationAuthority, ApplicationError, StagedActivation};
use orna_evaluator_v1::Environment;

/// Repository-relative module that hosts the serving entry.
pub(super) const SERVE_MODULE_PATH: &str = "main.orna";
/// The function invoked for each `GET /`.
pub(super) const SERVE_ENTRY: &str = "serve";

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
        project: &orna_project_v1::LoadedProject,
        catalogue: &orna_semantic_v1::Catalogue,
    ) -> Result<Option<Self>, Diagnostic> {
        let Some(index) = project
            .identities()
            .iter()
            .position(|identity| identity.logical_path() == SERVE_MODULE_PATH)
        else {
            return Ok(None);
        };
        let authority = ApplicationAuthority::new(catalogue.clone(), Limits::default());
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
            Ok(_) => Ok(None),
            Err(ApplicationError::MissingEntry(_)) => Ok(None),
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
        let staged = match self.stage_at_head() {
            Ok(staged) => staged,
            Err(code) => return serve_error(code),
        };
        if !staged.mutations().is_empty() {
            return serve_error("serve.write_refused");
        }
        Response::new(
            200,
            "text/plain; charset=utf-8",
            crate::repl::inspect(staged.value()).into_bytes(),
        )
    }

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
    /// The served process holds the runtime owner lock, so an ordinary write
    /// cannot change the CWD while it serves. A publish that does not take the
    /// lease is the remaining writer path, and step 3 covers it.
    ///
    /// The committed `main.orna` at HEAD is admitted, its tables are bound to
    /// their committed identities, and the committed rows are overlaid with the
    /// CWD generation captured for this activation. A missing or unreadable HEAD
    /// entry is `serve.entry_unavailable`; a table whose committed identity is
    /// missing, or present for only some tables, is refused before any read.
    fn stage_at_head(&self) -> Result<StagedActivation, &'static str> {
        let repository = orna_repository_v1::Repository::discover(&self.root)
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
                orna_project_v1::ProjectLimits::default().max_source_bytes,
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
                .map_err(|_| "serve.evaluation_failed");
        }
        let table_names = names.iter().map(String::as_str).collect::<Vec<_>>();
        if legacy_bare_module_table(&state, &runtime, &table_names)
            .map_err(|_| "serve.table_rows_unavailable")?
            .is_some()
        {
            return Err("serve.table_rows_unavailable");
        }
        let tables = match committed_table_identities(&repository, &commit, names.clone()) {
            Ok(tables) => tables,
            Err(_) => {
                let bound = names
                    .iter()
                    .filter(|name| {
                        committed_table_identities(&repository, &commit, vec![(*name).clone()])
                            .is_ok()
                    })
                    .count();
                return Err(if bound == 0 || bound == names.len() {
                    "serve.table_identity_unavailable"
                } else {
                    "serve.mixed_table_identity"
                });
            }
        };
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
        let (rows, storage_orders) = committed_rows_at(&repository, &commit, &admitted, &tables)
            .map_err(|error| match error {
                CommittedRowsError::Limit => "serve.table_read_limit",
                CommittedRowsError::Unavailable => "serve.table_rows_unavailable",
            })?;
        // A stored Blob reference has no portable value to serve; refuse rather than
        // expose its placeholder as committed data.
        for (name, table) in admitted.table_schemas() {
            let stored = rows.get(name).map(Vec::as_slice).unwrap_or_default();
            let order = storage_orders.get(name).map(Vec::as_slice).unwrap_or_default();
            if orna_project_v1::stored_rows_hold_blob(table.schema(), order, stored)
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
        Ok(staged)
    }

    /// The HEAD commit of `repository`. An unborn HEAD has no commit to serve
    /// (ORNA-SERVE-003) and is `serve.entry_unavailable`.
    fn head_commit(
        repository: &orna_repository_v1::Repository,
    ) -> Result<orna_repository_v1::GitCommitRef, &'static str> {
        repository
            .head()
            .ok()
            .flatten()
            .ok_or("serve.entry_unavailable")
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
    response
        .headers
        .push(("X-Content-Type-Options".to_owned(), "nosniff".to_owned()));
    Some(response)
}

fn serving_entry_route_inner(app: Option<&Mutex<ServeApp>>, request: &Request) -> Option<Response> {
    let app = app?;
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

/// True when `source` declares `serve` as a `pub fn` (the only serving entry).
fn serve_is_public(source: &str) -> bool {
    orna_syntax_v1::parse_module(source).value.items.iter().any(|item| {
        matches!(item.visibility, orna_syntax_v1::Visibility::Public { .. })
            && matches!(
                &item.declaration,
                orna_syntax_v1::Declaration::Function { signature, .. } if signature.name == SERVE_ENTRY
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
