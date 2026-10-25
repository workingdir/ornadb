//! The selected source-hosted serving entry (ORNA-APP-010, DESIGN S1).
//!
//! `orna serve` admits the project's `main.orna` module and invokes its
//! `serve` function for `GET /`. The invocation is staged and read-only: a
//! `serve` that attempts a table write is refused, and nothing is committed.
//! `serve` reads of tables see the committed `HEAD` rows of the project's tables,
//! bound to their committed identities per request (ORNA-TXN-006). Uncommitted
//! runtime rows are never read. Without that entry the server keeps its Git
//! transport, installs no page at `/`, and reports the absence once at startup.

use super::*;
use orna_application_v1::{
    AdmittedApplication, ApplicationAuthority, ApplicationError, StagedActivation,
};
use orna_evaluator_v1::{Environment, Limits};

/// Repository-relative module that hosts the serving entry.
pub(super) const SERVE_MODULE_PATH: &str = "main.orna";
/// The function invoked for each `GET /`.
pub(super) const SERVE_ENTRY: &str = "serve";

/// The admitted serving entry and the authority that evaluates it.
pub(super) struct ServeApp {
    authority: ApplicationAuthority,
    admitted: AdmittedApplication,
    /// Project root whose committed `HEAD` supplies table rows.
    root: String,
    /// The `main.orna` source, re-admitted per request when the entry reads tables.
    source: String,
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
            Ok(admitted) if serve_is_public(&project.modules()[index].source) => Ok(Some(Self {
                authority,
                admitted,
                root: root.to_owned(),
                source: project.modules()[index].source.clone(),
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
        let staged = if self.admitted.table_schemas().is_empty() {
            self.authority
                .evaluate_staged(&self.admitted, &Environment::new())
                .map_err(|_| "serve.evaluation_failed")
        } else {
            self.stage_over_committed_tables()
        };
        let staged = match staged {
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

    /// Stages `serve` over the committed `HEAD` rows of its tables. Admission and
    /// the table identity binding are repeated per request so that a new commit is
    /// served as soon as it lands; runtime rows that are not committed are never read.
    fn stage_over_committed_tables(&self) -> Result<StagedActivation, &'static str> {
        let repository = orna_repository_v1::Repository::discover(&self.root)
            .map_err(|_| "serve.table_identity_unavailable")?;
        let commit = repository
            .head()
            .ok()
            .flatten()
            .ok_or("serve.table_identity_unavailable")?;
        let names = self
            .admitted
            .table_schemas()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
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
                &self.source,
                SERVE_ENTRY.to_owned(),
                committed,
            )
            .map_err(|_| "serve.evaluation_failed")?;
        if admitted.requires_publication_metadata() {
            return Err("serve.table_read_unsupported");
        }
        let rows = committed_rows_at(&repository, &commit, &admitted, &tables)
            .map_err(|_| "serve.table_rows_unavailable")?;
        self.authority
            .evaluate_staged_over_committed_rows(&admitted, &Environment::new(), rows, None)
            .map_err(|_| "serve.evaluation_failed")
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
