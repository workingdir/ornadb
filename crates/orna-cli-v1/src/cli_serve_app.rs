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

/// Repository-relative module that hosts the serving entry.
pub(super) const SERVE_MODULE_PATH: &str = "main.orna";
/// The function invoked for each `GET /`.
pub(super) const SERVE_ENTRY: &str = "serve";

/// The admitted serving entry and the authority that evaluates it.
pub(super) struct ServeApp {
    authority: ApplicationAuthority,
    admitted: AdmittedApplication,
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
            Ok(admitted) => Ok(Some(Self { authority, admitted })),
            Err(ApplicationError::MissingEntry(_)) => Ok(None),
            Err(error) => Err(Diagnostic::target_with_detail(
                "E2101",
                "serving module failed semantic analysis",
                "fix the first reported source contract error in main.orna, then retry `serve`",
                error.to_string(),
            )),
        }
    }

    /// Invokes `serve` read-only and renders its value as the `GET /` body.
    fn respond(&self) -> Response {
        let staged = match self
            .authority
            .evaluate_staged(&self.admitted, &Environment::new())
        {
            Ok(staged) => staged,
            Err(_) => {
                return Response::new(
                    500,
                    "application/json",
                    br#"{"error":"serve.evaluation_failed"}"#.to_vec(),
                );
            }
        };
        if !staged.mutations().is_empty() {
            return Response::new(
                500,
                "application/json",
                br#"{"error":"serve.write_refused"}"#.to_vec(),
            );
        }
        Response::new(
            200,
            "text/plain; charset=utf-8",
            crate::repl::inspect(staged.value()).into_bytes(),
        )
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
    if request.method != "GET" {
        return Some(Response::new(
            405,
            "application/json",
            br#"{"error":"method_not_allowed"}"#.to_vec(),
        ));
    }
    Some(match app.lock() {
        Ok(app) => app.respond(),
        Err(_) => unavailable_response(),
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
