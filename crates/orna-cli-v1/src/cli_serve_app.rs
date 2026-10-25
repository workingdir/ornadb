//! The selected source-hosted serving entry (ORNA-APP-010, DESIGN S1).
//!
//! `orna serve` admits the project's `main.orna` module and invokes its
//! `serve` function for `GET /`. The invocation is staged and read-only: a
//! `serve` that attempts a table write is refused, and nothing is committed.
//! Without that entry the server keeps its Git transport, installs no page at
//! `/`, and reports the absence once at startup.

use super::*;
use orna_application_v1::{AdmittedApplication, ApplicationAuthority, ApplicationError};
use orna_evaluator_v1::{Environment, Limits};
use orna_runtime_v1::{RuntimeState, RuntimeTableIdentity};
use std::sync::Arc;

/// Repository-relative module that hosts the serving entry.
pub(super) const SERVE_MODULE_PATH: &str = "main.orna";
/// The function invoked for each `GET /`.
pub(super) const SERVE_ENTRY: &str = "serve";

/// The admitted serving entry and the authority that evaluates it.
pub(super) struct ServeApp {
    authority: ApplicationAuthority,
    admitted: AdmittedApplication,
    /// The clone's durable runtime. `serve` reads stored rows through one
    /// activation snapshot of it; `None` refuses the request (never empty tables).
    runtime: Option<Arc<RuntimeState>>,
}

impl ServeApp {
    /// Admits the selected entry. Returns `Ok(None)` when the project has no
    /// `main.orna` module or that module declares no `serve` function.
    pub(super) fn admit(
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
                runtime: None,
            })),
            // A private `serve` is not the serving entry (ORNA-APP-001 explicit root `pub fn serve()`).
            Ok(_) => Ok(None),
            Err(ApplicationError::MissingEntry(_)) => Ok(None),
            Err(error) => Err(Diagnostic::target_with_detail(
                "E2101",
                "serving module failed semantic analysis",
                "fix the first reported source contract error in main.orna, then retry `serve`",
                error.to_string(),
            )),
        }
    }

    /// Installs the clone's durable runtime, the handle that table reads use.
    pub(super) fn with_runtime(mut self, runtime: Option<Arc<RuntimeState>>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Table identities of the admitted application that have a stable object id.
    fn table_identities(&self) -> Result<Vec<RuntimeTableIdentity>, orna_runtime_v1::RuntimeError> {
        self.admitted
            .table_schemas()
            .iter()
            .filter_map(|(name, table)| {
                table
                    .object_id()
                    .map(|id| RuntimeTableIdentity::new(name.clone(), id))
            })
            .collect()
    }

    /// Invokes `serve` read-only over one activation snapshot of the clone's
    /// stored rows, and renders its value as the `GET /` body.
    fn respond(&self) -> Response {
        let evaluation_failed = || {
            Response::new(
                500,
                "application/json",
                br#"{"error":"serve.evaluation_failed"}"#.to_vec(),
            )
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return evaluation_failed();
        };
        let Ok(executor) = current_thread_executor() else {
            return evaluation_failed();
        };
        // Tables with a committed identity are captured by that identity. A
        // project whose tables have none (not yet published) is captured by
        // name: every admitted table is then read from the live table state.
        let captured = if self
            .admitted
            .table_schemas()
            .values()
            .all(|table| table.object_id().is_some())
        {
            match self.table_identities() {
                Ok(identities) => {
                    executor.block_on(runtime.begin_admitted_table_activation(&identities))
                }
                Err(error) => Err(error),
            }
        } else {
            let names: Vec<&str> = self
                .admitted
                .table_schemas()
                .keys()
                .map(String::as_str)
                .collect();
            executor.block_on(runtime.begin_table_activation(&names))
        };
        let Ok(snapshot) = captured else {
            return evaluation_failed();
        };
        let staged = match self.authority.evaluate_staged_with_table_snapshot(
            &self.admitted,
            &Environment::new(),
            &snapshot,
        ) {
            Ok(staged) => staged,
            Err(_) => return evaluation_failed(),
        };
        if !staged.mutations().is_empty() {
            return Response::new(
                500,
                "application/json",
                br#"{"error":"serve.write_refused"}"#.to_vec(),
            );
        }
        let mut response = Response::new(
            200,
            "text/plain; charset=utf-8",
            crate::repl::inspect(staged.value()).into_bytes(),
        );
        response
            .headers
            .push(("X-Content-Type-Options".to_owned(), "nosniff".to_owned()));
        response
    }
}

/// Answers `/` through the serving entry when one is installed. Returns
/// `None` for every other request, which the Git transport then handles.
pub(super) fn serving_entry_route(
    app: Option<&Mutex<ServeApp>>,
    request: &Request,
) -> Option<Response> {
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
