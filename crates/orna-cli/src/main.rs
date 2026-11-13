// Every command returns `Result<(), Diagnostic>` with the full command
// diagnostic: code, message and the hints a user needs to fix the call. Boxing
// it would push a Box onto every command without shrinking what a user reads.
#![allow(clippy::result_large_err)]

mod assertion_gate;
mod checkout_assertion_gate;
mod cli_annotate;
mod cli_args;
mod cli_check_backend;
mod cli_checkout;
mod cli_commit;
mod cli_dispatch;
mod cli_export;
mod cli_help;
mod cli_history;
mod cli_import;
mod cli_legacy;
mod cli_migrate;
mod cli_mount;
mod cli_mv;
mod cli_options;
mod cli_publish;
mod cli_query;
mod cli_row_key;
mod cli_serve;
mod cli_stage;
mod cli_status;
mod committed;
mod diagnostic_docs;
mod endpoint;
#[cfg(test)]
#[path = "../tests/support/format3.rs"]
mod format3;
#[cfg(target_os = "linux")]
mod fuse_mount;
mod invoke;
#[cfg(test)]
#[path = "../tests/support/media_commit.rs"]
#[allow(clippy::duplicate_mod)]
mod media_commit;
mod media_range;
mod project_run;
mod repl;
mod repl_request;
mod repl_run;
mod repl_session;
mod repo_sync;
mod status_run;
mod sys_host;
mod ui_render;

use cli_args::{Command, Invocation, Parsed, StatusFormat, parse_cli_reporting};
use cli_dispatch::execute;
use committed::*;
use diagnostic_docs::*;
use endpoint::*;
use invoke::*;
use orna_base::hex_digit;
use project_run::*;
use repl::{AnsiColor, write_rendered_error, write_styled, write_styled_display};
use repl_request::*;
use repl_run::*;
use repl_session::*;
use repo_sync::*;
use status_run::*;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufReader, IsTerminal, Write as _};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;

use orna_application::{
    AdmittedApplication, ApplicationAuthority, ApplicationError, CommittedTableIdentity,
    ReplEvaluationFailure, ReplTransactionScope, StagedActivation,
};
use orna_conformance::{
    BoundedEvaluator, DurableTransactionalEvaluator, ProjectEnvironment, ProjectExpectations,
    ProjectUnit, RuntimeEvaluator, SourceUnit, StageOutcome, project_declares_assertions,
    transactional_loaded_project_candidate_validator,
};
use orna_evaluator::{Environment, Limits, SysHostBindingRegistry};
use orna_project_repl::{AdmittedReplSession, ReplError};
use orna_runtime::{
    FaultInjector, NoFault, PinnedCommittedRows, QueryObservationRegistration,
    RequestAdmissionCapability, RequestIdentity, RuntimeError, RuntimeIdentity,
    RuntimePublicationMetadataRows, RuntimeState, RuntimeTableActivationSnapshot,
    RuntimeTableIdentity, RuntimeTableRows, StagedTableActivation, TableActivationError,
    TableObjectId, TerminalOutcome, ValidatedTableRequestActivationCommit, WriterLease,
};
use orna_sys::{
    EnvironmentProvider, FilesystemProvider, RuntimeMode, StdSnapshotPin, SysHostContext,
};
use orna_value::{CanonicalValue, OvbRaw, Value};
use sha2::{Digest, Sha256};

use orna_cli_support::request_nonce;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auto" => Self::Auto,
            "always" => Self::Always,
            "never" => Self::Never,
            _ => return None,
        })
    }

    fn stdout_enabled(self) -> bool {
        self.enabled(io::stdout().is_terminal())
    }

    fn stderr_enabled(self) -> bool {
        self.enabled(io::stderr().is_terminal())
    }

    const fn enabled(self, is_terminal: bool) -> bool {
        match self {
            Self::Auto => is_terminal,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

fn write_cli_line(text: &[u8], color: AnsiColor, color_enabled: bool) -> Result<(), Diagnostic> {
    let mut stdout = io::stdout().lock();
    write_styled(&mut stdout, color, text, color_enabled)
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|_| Diagnostic {
            code: "E2200",
            title: Cow::Borrowed("CLI output could not be written"),
            help: "check the output stream, then retry the command",
            exit: Exit::Presentation,
            detail: None,
            body: None,
        })
}
/// The process exit status of a failed command, as the `--help` exit status
/// table lists it. Status 0 (success) is the absence of a diagnostic and
/// status 4 (authorisation) belongs to remote transports this build does not
/// have, so neither is a status a `Diagnostic` can carry.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Exit {
    Target = 1,
    Usage = 2,
    Connection = 3,
    Presentation = 5,
    Cancelled = 6,
    Protocol = 7,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Diagnostic {
    code: &'static str,
    title: Cow<'static, str>,
    help: &'static str,
    exit: Exit,
    detail: Option<String>,
    body: Option<String>,
}
impl Diagnostic {
    const fn usage(code: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            code,
            title: Cow::Borrowed(title),
            help,
            exit: Exit::Usage,
            detail: None,
            body: None,
        }
    }
    const fn target(code: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            code,
            title: Cow::Borrowed(title),
            help,
            exit: Exit::Target,
            detail: None,
            body: None,
        }
    }
    fn target_with_detail(
        code: &'static str,
        title: impl Into<Cow<'static, str>>,
        help: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            title: title.into(),
            help,
            exit: Exit::Target,
            detail: Some(detail.into()),
            body: None,
        }
    }
    const fn unavailable(title: &'static str, help: &'static str) -> Self {
        Self::target("E2000", title, help)
    }
}
impl From<orna_repository::RepositoryError> for Diagnostic {
    fn from(error: orna_repository::RepositoryError) -> Self {
        match error {
            orna_repository::RepositoryError::RepositoryBusy => Self::target(
                "E2100",
                "repository coordination lock is busy",
                "wait for the active repository operation to finish, then retry mv",
            ),
            orna_repository::RepositoryError::LocalStateUnavailable => Self::target(
                "E2100",
                "repository coordination state is unavailable",
                "check local Git worktree permissions, then retry mv",
            ),
            _ => Self::target(
                "E2100",
                "repository coordination lock could not be acquired",
                "check local Git worktree state, then retry mv",
            ),
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn project_load_diagnostic(error: orna_project::ProjectLoadError) -> Diagnostic {
    if matches!(error, orna_project::ProjectLoadError::SourceTreeLimit) {
        return Diagnostic::target(
            "E2100",
            "committed source tree exceeds the fixed walk limit",
            "reduce the committed tree's entry count, nesting depth or path length, then run check again",
        );
    }
    if let orna_project::ProjectLoadError::InvalidStandardBody(diagnostic) = &error {
        let detail = if diagnostic.notes.is_empty() {
            format!(
                "{}: {} ({}:{})",
                diagnostic.code, diagnostic.message, diagnostic.logical_path, diagnostic.line
            )
        } else {
            format!(
                "{}: {}: {} ({}:{})",
                diagnostic.code,
                diagnostic.message,
                diagnostic.notes.join("; "),
                diagnostic.logical_path,
                diagnostic.line
            )
        };
        return Diagnostic::target_with_detail(
            "E2101",
            "pinned standard module failed type checking",
            "fix the pinned standard module or pin a corrected standard snapshot, then retry",
            detail,
        );
    }
    let invalid_unit = match &error {
        orna_project::ProjectLoadError::InvalidModule {
            logical_path,
            diagnostics,
        }
        | orna_project::ProjectLoadError::InvalidRowUnit {
            logical_path,
            diagnostics,
        } => Some((logical_path, diagnostics)),
        _ => None,
    };
    if let Some((logical_path, diagnostics)) = invalid_unit
        && let Some(first) = diagnostics.first()
    {
        // The first parser diagnostic is the primary code: it is the spec code
        // that names the source contract violation, not a loader wrapper.
        let location = match first.span.start_position() {
            Some(position) => format!("{logical_path}:{}:{}", position.line, position.column),
            None => logical_path.clone(),
        };
        return Diagnostic {
            code: first.code,
            title: Cow::Owned(format!("{} ({location})", first.message)),
            help: "fix the reported source error, then retry",
            exit: Exit::Target,
            detail: None,
            body: None,
        };
    }
    Diagnostic::target(
        "E2100",
        "project source could not be loaded",
        "fix the project module graph and source boundaries, then run check again",
    )
}

fn cancellation_diagnostic(diagnostic: &orna_value::Diagnostic) -> Diagnostic {
    let code = match diagnostic.code() {
        "ORNA-LIST-STREAM-CANCELLED" => "ORNA-LIST-STREAM-CANCELLED",
        _ => "E2200",
    };
    Diagnostic {
        code,
        title: Cow::Borrowed("operation was cancelled"),
        help: "inspect the durable operation state before retrying",
        exit: Exit::Cancelled,
        detail: (code == "E2200").then(|| format!("cancellation code: {}", diagnostic.code())),
        body: None,
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(body) = &self.body {
            return f.write_str(body);
        }
        write!(f, "error[{}]: {}", self.code, self.title)?;
        if let Some(detail) = &self.detail {
            write!(f, "\n  {detail}")?;
        }
        write!(f, "\nhelp: {}", self.help)
    }
}

fn write_diagnostic<W: io::Write>(
    writer: &mut W,
    diagnostic: &Diagnostic,
    color_enabled: bool,
    debug_enabled: bool,
) -> io::Result<()> {
    if let Some(body) = &diagnostic.body {
        return write_rendered_error(writer, body, color_enabled);
    }
    write_styled(writer, AnsiColor::Red, b"error", color_enabled)?;
    writer.write_all(b"[")?;
    write_styled(
        writer,
        AnsiColor::Red,
        diagnostic.code.as_bytes(),
        color_enabled,
    )?;
    writer.write_all(b"]: ")?;
    write_styled(
        writer,
        AnsiColor::Red,
        diagnostic.title.as_bytes(),
        color_enabled,
    )?;
    // Semantic source diagnostics remain user-facing. Other attached details
    // can contain repository/runtime implementation data and require --debug.
    if let Some(detail) = &diagnostic.detail
        && (debug_enabled || diagnostic.code == "E2101")
    {
        writer.write_all(b"\n  ")?;
        write_styled(writer, AnsiColor::Dim, detail.as_bytes(), color_enabled)?;
    }
    writer.write_all(b"\nhelp: ")?;
    write_styled(
        writer,
        AnsiColor::Dim,
        diagnostic.help.as_bytes(),
        color_enabled,
    )?;
    writer.write_all(b"\n")
}

fn write_success_line(message: &str, color_enabled: bool) -> Result<(), Diagnostic> {
    write_cli_line(message.as_bytes(), AnsiColor::Green, color_enabled)
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let cli_args::ParseReport {
        modes,
        result: parsed,
    } = parse_cli_reporting(&args);
    let parsed = match parsed {
        Ok(value) => value,
        Err(error) => {
            let _ = write_diagnostic(
                &mut io::stderr().lock(),
                &error,
                modes.color.stderr_enabled(),
                modes.debug,
            );
            return ExitCode::from(error.exit as u8);
        }
    };
    match execute(&parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = write_diagnostic(
                &mut io::stderr().lock(),
                &error,
                parsed.color.stderr_enabled(),
                modes.debug,
            );
            ExitCode::from(error.exit as u8)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod repl_request_effect_tests;
