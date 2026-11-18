//! The console the served page's Run control evaluates against (ORNA-APP-012).
//!
//! The console session is pure: it evaluates a pure input itself and refuses
//! every effect before it runs. An input that only reads tables is the one
//! exception. Its table reads, and so the system relations it reads, are served
//! from the commit `HEAD` names when it runs ([`ConsoleReads`]), because a
//! server outlives every commit it serves (ORNA-SERVE-003, ORNA-SYS-011).

use super::{Mutex, Response, ServeApp, action_error, unavailable_response};
use orna_application::ApplicationAuthority;
use orna_check::TableSchema;
use orna_project_repl::{AdmittedReplSession, ReplError};
use orna_value::CanonicalValue;
use std::collections::BTreeMap;
use std::sync::TryLockError;

/// What a console input that reads system relations is served from: at the
/// commit `HEAD` names when it runs, over the identities committed there.
pub(super) struct ConsoleReads {
    /// Project root whose `HEAD` the input observes.
    root: String,
    /// The page authority: the serve host context, with no `HEAD` pinned.
    authority: ApplicationAuthority,
    database_id: [u8; 16],
    /// The admitted tables the console's catalogue lists.
    tables: BTreeMap<String, TableSchema>,
}

impl ConsoleReads {
    /// What the console of `app` serves its system reads from.
    pub(super) fn of(app: &ServeApp) -> Self {
        Self {
            root: app.root.clone(),
            authority: app.authority.clone(),
            database_id: app.database_id,
            tables: app.tables.clone(),
        }
    }

    /// Evaluates `source`, which only reads tables, in `session`. A clone with
    /// no readable commit has nothing to pin the rows to, so a read that needs
    /// one fails closed with `sys.snapshot.incomplete`.
    fn submit(
        &self,
        session: &mut AdmittedReplSession,
        source: &str,
    ) -> Result<Option<CanonicalValue>, ReplError> {
        let commit = crate::catalogue_identities::request_commit(
            std::path::Path::new(&self.root),
            self.database_id,
        );
        self.authority
            .clone()
            .with_head_commit(commit.head)
            .submit_system_read(
                session,
                source,
                self.tables.clone(),
                commit.identities.as_ref(),
            )
    }
}

/// The admitted tables of `project`: the tables the console's `sys.catalog.*`
/// rows list, which are those of the project its session was admitted from.
pub(super) fn console_tables(
    project: &orna_project::LoadedProject,
    catalogue: &orna_check::Catalogue,
) -> BTreeMap<String, TableSchema> {
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        catalogue,
        orna_check::SystemProviders::Production,
    );
    orna_check::catalogue_rows::admitted_table_schemas_by_name(&analysis)
}

/// Evaluates one console input under the session lock. A session another
/// request holds is refused visibly instead of queueing the input behind an
/// evaluation that is still running. The evaluation carries no wall-clock
/// deadline: its bound is the evaluator's default step limit and the
/// 16 KiB input cap, and the session lock is held until that bound is reached.
pub(super) fn submit_console(
    console: &Mutex<Result<AdmittedReplSession, String>>,
    reads: &ConsoleReads,
    source: &str,
) -> Response {
    match console.try_lock() {
        Ok(mut session) => console_submit(&mut session, reads, source),
        Err(TryLockError::WouldBlock) => action_error(409, "serve.session_busy"),
        Err(TryLockError::Poisoned(_)) => unavailable_response(),
    }
}

/// Evaluates one console input and renders its value. The session evaluates a
/// pure input itself and serves an input that only reads tables from the commit
/// `HEAD` names ([`ConsoleReads`]). Any other effect is refused before it runs,
/// and that refusal is rendered like any other diagnostic. A session that
/// failed to admit answers with its stable code.
fn console_submit(
    console: &mut Result<AdmittedReplSession, String>,
    reads: &ConsoleReads,
    source: &str,
) -> Response {
    contained(|| {
        let session = match console {
            Ok(session) => session,
            Err(code) => return action_error(503, code),
        };
        let reads_tables = session
            .stage_activation(source)
            .is_ok_and(|staged| staged.is_table_read_only());
        let outcome = if reads_tables {
            reads.submit(session, source)
        } else {
            session.submit(source)
        };
        match outcome {
            Ok(Some(value)) => {
                let html =
                    crate::ui_render::render_value_html(value.raw(), None).unwrap_or_else(|| {
                        let mut text = String::from("<pre><code>");
                        crate::ui_render::push_escaped(
                            &crate::repl_inspect::inspect(&value),
                            &mut text,
                        );
                        text.push_str("</code></pre>");
                        text
                    });
                Response::new(200, "text/html; charset=utf-8", html.into_bytes())
            }
            Ok(None) => Response::new(200, "text/html; charset=utf-8", b"<p>no value</p>".to_vec()),
            Err(error) => action_error(422, error.code()),
        }
    })
}

/// Runs one evaluation so that a panic inside it answers 500 and returns
/// without poisoning the console lock. The session stays available to the next
/// input.
pub(super) fn contained(evaluate: impl FnOnce() -> Response) -> Response {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(evaluate))
        .unwrap_or_else(|_| action_error(500, "serve.evaluation_failed"))
}
