// Every command returns `Result<(), Diagnostic>` with the full command
// diagnostic: code, message and the hints a user needs to fix the call. Boxing
// it would push a Box onto every command without shrinking what a user reads.
#![allow(clippy::result_large_err)]

mod assertion_gate;
mod checkout_assertion_gate;
mod cli_annotate;
mod cli_args;
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
mod cli_source_edit;
mod cli_stage;
mod cli_status;
mod encoding;
#[cfg(test)]
#[path = "../tests/support/format3.rs"]
mod format3;
#[cfg(target_os = "linux")]
mod fuse_mount;
#[cfg(test)]
#[path = "../tests/support/media_commit.rs"]
// The shared capture fixture loads `table_schema` from `src` itself, so the
// binary's own copy and this one coexist during tests and never interact.
#[allow(clippy::duplicate_mod)]
mod media_commit;
mod media_range;
mod repl;
mod sys_host;
mod table_schema;
mod ui_render;

use cli_args::{Command, Invocation, Parsed, StatusFormat, parse_cli_reporting};
use cli_dispatch::execute;
use encoding::hex_digit;
use repl::{AnsiColor, write_rendered_error, write_styled, write_styled_display};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufReader, IsTerminal, Write as _};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

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
use orna_foundation::{CanonicalValue, OvbRaw, Value};
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
use sha2::{Digest, Sha256};

static CLI_REQUEST_NONCE: AtomicU64 = AtomicU64::new(1);

fn cli_request_nonce() -> u128 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (now << 32)
        ^ (u128::from(std::process::id()) << 16)
        ^ u128::from(CLI_REQUEST_NONCE.fetch_add(1, AtomicOrdering::Relaxed))
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiagnosticDocumentation {
    code: &'static str,
    title: &'static str,
    explanation: &'static str,
    help: &'static str,
}

/// The catalogued diagnostics `orna explain` documents and the REPL and
/// one-shot runners publish runtime failures under.
const DIAGNOSTIC_DOCUMENTATION: &[DiagnosticDocumentation] = &[
    DiagnosticDocumentation {
        code: "ORNA-S010-IMPORT",
        title: "imported module is unavailable",
        explanation: "The project names a module that is not part of its captured dependencies, so the module cannot be checked or executed.",
        help: "use a captured standard dependency or remove the import",
    },
    DiagnosticDocumentation {
        code: "ORNA-S011-AMBIGUOUS",
        title: "standard import name is ambiguous",
        explanation: "Two standard imports bind the same unqualified name to different functions, so the name cannot be resolved.",
        help: "import the functions under distinct names or use their qualified paths",
    },
    DiagnosticDocumentation {
        code: "ORNA-S012-UNRESOLVED",
        title: "name could not be resolved",
        explanation: "The source refers to a name that is not declared or imported in the current namespace.",
        help: "declare the name or add the matching `use` import before using it",
    },
    DiagnosticDocumentation {
        code: "ORNA-S021-TYPE",
        title: "expression has the wrong type",
        explanation: "The value produced by an expression does not satisfy the type required at that source location.",
        help: "change the expression or its declared type so the value and requirement agree",
    },
    DiagnosticDocumentation {
        code: "ORNA-REPL-EFFECT",
        title: "REPL preview cannot perform an effect",
        explanation: "Interactive previews are read-only and cannot perform network, filesystem, process, or other external effects.",
        help: "evaluate a pure expression or invoke the operation through its admitted runtime entry point",
    },
    DiagnosticDocumentation {
        code: "ORNA-REPL-EFFECT-UNCERTAIN",
        title: "REPL effect outcome is uncertain",
        explanation: "The input was evaluated before its request was durably committed, so external effects may already have happened. The outcome is uncertain and recovery is pending; the request is not replayed.",
        help: "check the external system for the effect before repeating the input, because repeating it may repeat an external effect",
    },
    DiagnosticDocumentation {
        code: "ORNA-REPL-AT",
        title: "snapshot selection failed",
        explanation: "The requested CWD, HEAD, or reference snapshot could not be loaded as an admitted REPL context.",
        help: "choose an existing snapshot and keep the repository available while selecting it",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-RETURN-ARROW",
        title: "function return type uses the arrow form",
        explanation: "Orna function declarations separate the parameter list from the return type with a colon.",
        help: "replace `->` with `:` in the function declaration",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-VAR",
        title: "local declaration uses `var`",
        explanation: "Orna uses `let` for local declarations; reassignment updates that binding when permitted.",
        help: "replace `var` with `let`",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-001",
        title: "refined type uses the legacy `where self` form",
        explanation: "Refined Orna types state their invariant in a brace-delimited assertion block owned by the type.",
        help: "replace `where self` with a brace-delimited `assert` block",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-002",
        title: "table assertion repeats its owner",
        explanation: "A table assertion is already evaluated for its owning table, so an explicit owner pipeline is redundant.",
        help: "remove the leading `self |` or table-name pipeline from the assertion",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-003",
        title: "module assertion belongs to one table",
        explanation: "An assertion that depends on only one table must live with that table so its owner is explicit.",
        help: "move the assertion into the table body it depends on",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-004",
        title: "assertion predicate has the wrong owner",
        explanation: "The declaration's predicate does not describe the type or table that owns the assertion.",
        help: "rewrite the predicate for the reported owner type",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-005",
        title: "assertion is missing its terminator",
        explanation: "An assertion clause must end with a semicolon before the next declaration.",
        help: "terminate the assertion clause with `;`",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-006",
        title: "assertion uses a dedicated recovery branch",
        explanation: "Assertions report failed propositions; they do not provide an assertion-specific `else` branch.",
        help: "use ordinary failure, recovery, or control flow instead",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-007",
        title: "assertion has a forbidden effect",
        explanation: "Declaration and refined assertions must be deterministic and cannot perform external effects.",
        help: "remove the identified effect or nondeterministic operation",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-008",
        title: "assertion is false",
        explanation: "The executable or refined assertion does not hold for the value being checked.",
        help: "inspect the owner and proposition, then provide a safe value",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-009",
        title: "table or cross-table assertion is false",
        explanation: "A table or cross-table invariant does not hold for the deterministic rows being checked.",
        help: "inspect the reported witness and repair the affected table data",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-010",
        title: "assertion form is unsupported",
        explanation: "Orna 1.0 supports the sole owner-local `assert` form for declarations and tables.",
        help: "rewrite the construct using the supported `assert` form",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-011",
        title: "assertion is empty",
        explanation: "An assertion must contain a proposition that can be checked.",
        help: "provide a proposition after `assert`",
    },
    DiagnosticDocumentation {
        code: "ORNA-A091-012",
        title: "module assertion has no table dependency",
        explanation: "A module assertion is only valid when it depends on a table; otherwise it has no executable owner.",
        help: "place the check in executable or test code",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-MATCH",
        title: "value branching uses the legacy `match` form",
        explanation: "Orna branches on values with `case` and colon-separated arms rather than `match` and arrows.",
        help: "replace `match` with `case` and use `:` for each arm",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-RESULT",
        title: "source uses an explicit `Result` value",
        explanation: "Orna returns the successful type directly and propagates failures automatically.",
        help: "return the successful type and use `|?` for recovery",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-POSTFIX-QUESTION",
        title: "operation uses postfix failure propagation",
        explanation: "Orna propagates operation failures automatically instead of using a postfix `?` operator.",
        help: "remove the postfix `?` and use `|?` only for recovery",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-CURRENCY",
        title: "source uses a special currency declaration",
        explanation: "Currencies are nominal types with a nested `Currency` implementation, not a separate declaration form.",
        help: "replace the currency declaration with a nominal type and nested `impl Currency`",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-CURRENCY-SYMBOL",
        title: "currency declares a universal symbol",
        explanation: "Currency symbols and placement depend on locale formatter data rather than a universal static field.",
        help: "move symbol and placement choices to the locale-aware money formatter",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-IMPL-FOR",
        title: "protocol implementation uses the legacy top-level form",
        explanation: "Orna protocol implementations are nested inside the type that owns them.",
        help: "nest `impl P` inside the owning type instead of writing `impl P for T`",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-BOUND-COLON",
        title: "generic bound uses the legacy colon form",
        explanation: "Generic protocol bounds use the `impl` keyword in Orna type parameters.",
        help: "replace `<T: P>` with `<T impl P>`",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-STATIC-FN",
        title: "protocol declares a static function",
        explanation: "Protocols expose static properties or ordinary functions rather than static function declarations.",
        help: "use a static property or an ordinary function",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-TRYFROM",
        title: "source uses `TryFrom`",
        explanation: "Orna's `From<S>` conversion may fail without wrapping the successful value in a separate result type.",
        help: "use `From<S>` for a conversion that may fail",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-CONVERSION-CHAIN",
        title: "conversion chain is implicit",
        explanation: "Orna does not search for unspoken multi-step conversions between types.",
        help: "name each conversion step explicitly",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-OPAQUE",
        title: "source uses an opaque type declaration",
        explanation: "Orna uses one nominal or refined `type` form instead of a separate opaque declaration.",
        help: "replace `opaque Name` with the unified nominal or refined `type` form",
    },
    DiagnosticDocumentation {
        code: "ORNA091-E-FIELD-CONSTRAINT",
        title: "field declares a legacy constraint",
        explanation: "Field uniqueness and checks are table assertions owned by the table, not field suffixes.",
        help: "write an owner-local table assertion using `all_unique` or `every`",
    },
];

fn diagnostic_documentation(code: &str) -> Option<DiagnosticDocumentation> {
    DIAGNOSTIC_DOCUMENTATION
        .iter()
        .find(|documentation| documentation.code == code)
        .copied()
}

fn explain_diagnostic(code: &str) -> Result<String, Diagnostic> {
    let Some(documentation) = diagnostic_documentation(code) else {
        return Err(Diagnostic::usage(
            "E1002",
            "diagnostic code is unknown",
            "supply a code printed by an Orna diagnostic, such as `ORNA-S010-IMPORT`",
        ));
    };
    Ok(format!(
        "{}: {}\n{}\nhelp: {}",
        documentation.code, documentation.title, documentation.explanation, documentation.help
    ))
}

fn cancellation_diagnostic(diagnostic: &orna_foundation::Diagnostic) -> Diagnostic {
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum Endpoint {
    ManagedLocal,
    Path(String),
    UnixSocket(String),
    RemoteTls(String),
}
impl Endpoint {
    fn parse(value: &str) -> Result<Self, Diagnostic> {
        if value.is_empty() {
            return Err(Diagnostic::usage(
                "E1004",
                "database endpoint is empty",
                "provide a local path, socket, or secure Orna URI",
            ));
        }
        if value.starts_with('-') || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(Diagnostic::usage(
                "E1004",
                "database endpoint is invalid",
                "use a local path, an absolute Orna Unix socket URI, or a secure Orna URI",
            ));
        }
        if value.contains('@') || value.contains('#') || value.contains('?') {
            return Err(Diagnostic::usage(
                "E1005",
                "database endpoint contains unsupported credentials or URI parts",
                "use transport authentication and omit credentials, fragments, and query parameters",
            ));
        }
        if let Some(path) = value.strip_prefix("orna+unix://") {
            return valid_unix_socket_path(path)
                .then(|| Self::UnixSocket(path.to_owned()))
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1004",
                        "database endpoint is invalid",
                        "use an absolute Orna Unix socket URI",
                    )
                });
        }
        if let Some(remainder) = value.strip_prefix("orna://") {
            let (authority, database) = remainder.split_once('/').ok_or_else(|| {
                Diagnostic::usage(
                    "E1004",
                    "database endpoint is invalid",
                    "use orna://HOST/DATABASE or orna://local/INSTANCE",
                )
            })?;
            if !valid_orna_authority(authority)
                || !valid_database_path(database)
                || ((authority == "local" || authority.starts_with("local:"))
                    && database.contains('/'))
                || authority.starts_with("local:")
            {
                return Err(Diagnostic::usage(
                    "E1004",
                    "database endpoint is invalid",
                    "use an absolute Orna Unix socket URI or a secure Orna URI with one valid authority",
                ));
            }
            return Ok(if authority == "local" {
                Self::Path(database.to_owned())
            } else {
                Self::RemoteTls(value.to_owned())
            });
        }
        (!value.contains("://") && valid_percent_text(value))
            .then(|| Self::Path(value.to_owned()))
            .ok_or_else(|| {
                Diagnostic::usage(
                    "E1004",
                    "database endpoint is invalid",
                    "use a local path, an absolute Orna Unix socket URI, or a secure Orna URI",
                )
            })
    }
}

fn valid_unix_socket_path(path: &str) -> bool {
    path.starts_with('/') && path.len() > 1 && valid_percent_text(path)
}

fn valid_orna_authority(authority: &str) -> bool {
    if authority.is_empty() || !valid_percent_text(authority) || authority.contains('@') {
        return false;
    }
    if let Some(host) = authority.strip_prefix('[') {
        let Some((host, suffix)) = host.split_once(']') else {
            return false;
        };
        return !host.is_empty()
            && (suffix.is_empty() || suffix.strip_prefix(':').and_then(valid_port).is_some());
    }
    let mut parts = authority.split(':');
    let Some(host) = parts.next() else {
        return false;
    };
    match (parts.next(), parts.next()) {
        (None, _) => !host.is_empty(),
        (Some(port), None) => !host.is_empty() && valid_port(port).is_some(),
        _ => false,
    }
}

fn valid_port(port: &str) -> Option<u16> {
    port.parse().ok().filter(|port| *port != 0)
}

fn valid_database_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| !segment.is_empty())
        && valid_percent_text(path)
}

fn valid_percent_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_control() || byte.is_ascii_whitespace() || matches!(byte, b'?' | b'#') {
            return false;
        }
        if byte == b'%' {
            let Some(high) = bytes.get(index + 1).and_then(|byte| hex_digit(*byte)) else {
                return false;
            };
            let Some(low) = bytes.get(index + 2).and_then(|byte| hex_digit(*byte)) else {
                return false;
            };
            if (high << 4 | low).is_ascii_control() {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

fn local_project_path(endpoint: &Endpoint) -> Result<&str, Diagnostic> {
    match endpoint {
        Endpoint::ManagedLocal => Ok("."),
        Endpoint::Path(path) => Ok(path),
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => Err(Diagnostic::target(
            "E2100",
            "project checking requires a local Git worktree",
            "use `check` with the current worktree or `--db PATH check`",
        )),
    }
}

fn pure_sys_host_context(
    endpoint: &Endpoint,
    std_snapshot: StdSnapshotPin,
) -> Option<SysHostContext> {
    let path = local_project_path(endpoint).ok()?;
    let repository = orna_repository::Repository::discover(path).ok()?;
    let (identity, _) = runtime_identity(&repository).ok()?;
    Some(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Run,
        std_snapshot,
    ))
}

const DISCOVER_WORKTREE_HELP: &str =
    "run the command inside a Git worktree or provide a local project path";

/// The local Git worktree `endpoint` names, or the `E2100` refusal whose
/// remedy is `help`.
fn discover_worktree(
    endpoint: &Endpoint,
    help: &'static str,
) -> Result<orna_repository::Repository, Diagnostic> {
    orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
        Diagnostic::target("E2100", "local Git worktree could not be discovered", help)
    })
}

fn run_status(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args([
            "-c",
            "color.status=false",
            "status",
            "--porcelain",
        ]),
        "check that Git can read the local worktree, then retry `status --porcelain`",
    )?;
    io::stdout().write_all(&output.stdout).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --porcelain`",
        )
    })?;
    Ok(())
}

fn run_status_json(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args([
            "-c",
            "color.status=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ]),
        "check that Git can read the local worktree, then retry `status --format json`",
    )?;
    let branch_output = cli_status::git_in(repository.worktree())
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree branch could not be read",
                "check that Git can read the local worktree, then retry `status --format json`",
            )
        })?;
    let branch = cli_status::branch_name(branch_output.status.success(), &branch_output.stdout);
    let json = cli_status::encode_status_json(branch, &output.stdout).ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "retry `status --format json`",
        )
    })?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    let json = append_unpublished_count_json(json, unpublished_count).ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "retry `status --format json`",
        )
    })?;
    io::stdout().lock().write_all(&json).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --format json`",
        )
    })?;
    Ok(())
}

fn unpublished_mutation_count(
    repository: &orna_repository::Repository,
) -> Result<usize, Diagnostic> {
    if !repository.runtime_paths().state_db().exists() {
        return Ok(0);
    }
    let (identity, initial_digest) = runtime_identity(repository)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `status` after checking the local runtime state",
            )
        })?;
    let state = runtime
        .block_on(RuntimeState::open(repository, identity, initial_digest))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `status` after checking the local runtime state",
            )
        })?;
    runtime
        .block_on(state.publication_metadata_rows())
        .and_then(|rows| {
            if rows.sys_storage != rows.maintenance_job {
                return Err(orna_runtime::RuntimeError::RecoveryInvalid);
            }
            match rows.sys_storage.raw() {
                OvbRaw::Map(fields) => fields
                    .iter()
                    .find_map(|(key, value)| match (key, value) {
                        (OvbRaw::Text(key), OvbRaw::Int(value)) if key == "pending_rows" => {
                            usize::try_from(value.clone()).ok()
                        }
                        _ => None,
                    })
                    .ok_or(orna_runtime::RuntimeError::RecoveryInvalid),
                _ => Err(orna_runtime::RuntimeError::RecoveryInvalid),
            }
        })
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `status` after checking the local runtime state",
            )
        })
}

/// Counts the unpublished tail of a migration source without writing to it.
/// The count is read through a read-only handle to `state.db`: no runtime is
/// opened, no owner lock is taken and no schema is created or migrated, so a
/// source that refuses a copy is left byte-identical.
fn source_unpublished_mutation_count(
    repository: &orna_repository::Repository,
) -> Result<usize, Diagnostic> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `migrate copy` after checking the source runtime state",
            )
        })?;
    runtime
        .block_on(orna_runtime::RuntimeState::read_pending_count(repository))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication status could not be read",
                "retry `migrate copy` after checking the source runtime state",
            )
        })
}

fn runtime_publication_metadata_rows(
    endpoint: &Endpoint,
    std_snapshot: StdSnapshotPin,
) -> Result<(RuntimePublicationMetadataRows, SysHostContext), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "local runtime publication metadata could not be read",
            "run the command inside an initialized local Orna repository",
        )
    })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let sys_host_context = sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Embedded,
        std_snapshot,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication metadata could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let state = runtime
        .block_on(RuntimeState::open(&repository, identity, initial_digest))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication metadata could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let rows = runtime
        .block_on(state.publication_metadata_rows())
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "local runtime publication metadata could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    Ok((rows, sys_host_context))
}

fn write_unpublished_summary<W: io::Write>(writer: &mut W, count: usize) -> io::Result<()> {
    let noun = if count == 1 { "mutation" } else { "mutations" };
    writeln!(writer, "Unpublished CWD changes: {count} runtime {noun}")
}

fn append_unpublished_count_json(mut json: Vec<u8>, count: usize) -> Option<Vec<u8>> {
    let prefix = json.strip_suffix(b"]}\n")?.len();
    json.truncate(prefix);
    json.extend_from_slice(b"],\"unpublished_runtime_mutations\":");
    json.extend_from_slice(count.to_string().as_bytes());
    json.extend_from_slice(b"}\n");
    Some(json)
}

fn osc8_terminal_supports(
    term: Option<&str>,
    terminal_program: Option<&str>,
    terminal_program_version: Option<&str>,
    vte_version: Option<&str>,
    windows_terminal_session: Option<&str>,
    inside_tmux: bool,
) -> bool {
    if inside_tmux {
        return false;
    }

    let known_term = matches!(
        term,
        Some(
            "xterm-kitty"
                | "xterm-ghostty"
                | "foot"
                | "foot-extra"
                | "wezterm"
                | "wezterm-256color"
                | "alacritty"
                | "contour"
        )
    );
    let known_program = matches!(terminal_program, Some("WezTerm" | "ghostty"));
    let iterm_supports_osc8 = terminal_program == Some("iTerm.app")
        && terminal_program_version.is_some_and(|version| version_at_least(version, (3, 1, 0)));
    let alacritty_supports_osc8 = terminal_program == Some("Alacritty")
        && terminal_program_version.is_some_and(|version| version_at_least(version, (0, 13, 0)));
    let vscode_supports_osc8 = terminal_program == Some("vscode")
        && terminal_program_version.is_some_and(|version| version_at_least(version, (1, 72, 0)));
    let vte_supports_osc8 = vte_version
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version >= 5000);
    let windows_terminal_supports_osc8 =
        windows_terminal_session.is_some_and(|session| !session.is_empty());

    known_term
        || known_program
        || iterm_supports_osc8
        || alacritty_supports_osc8
        || vscode_supports_osc8
        || vte_supports_osc8
        || windows_terminal_supports_osc8
}

fn version_at_least(version: &str, minimum: (u32, u32, u32)) -> bool {
    let mut components = version.split('.');
    let Some(major) = components.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let Some(minor) = components.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let patch = match components.next() {
        Some(part) => match part.parse::<u32>() {
            Ok(patch) => patch,
            Err(_) => return false,
        },
        None => 0,
    };
    (major, minor, patch) >= minimum
}

fn terminal_supports_osc8() -> bool {
    let term = std::env::var("TERM").ok();
    let terminal_program = std::env::var("TERM_PROGRAM").ok();
    let terminal_program_version = std::env::var("TERM_PROGRAM_VERSION").ok();
    let vte_version = std::env::var("VTE_VERSION").ok();
    let windows_terminal_session = std::env::var("WT_SESSION").ok();
    let inside_tmux = std::env::var_os("TMUX").is_some();
    osc8_terminal_supports(
        term.as_deref(),
        terminal_program.as_deref(),
        terminal_program_version.as_deref(),
        vte_version.as_deref(),
        windows_terminal_session.as_deref(),
        inside_tmux,
    )
}

#[allow(clippy::items_after_statements)]
fn file_url_for_status_path(root: &std::path::Path, display_token: &[u8]) -> Option<String> {
    if !root.is_absolute()
        || display_token.is_empty()
        || display_token.first() == Some(&b'"')
        || display_token.iter().any(u8::is_ascii_control)
    {
        return None;
    }
    let path_text = std::str::from_utf8(display_token).ok()?;
    if path_text.chars().any(char::is_control) {
        return None;
    }

    let relative_path = std::path::Path::new(path_text);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let absolute_path = root.join(relative_path);
    let absolute_path = absolute_path.to_str()?;
    let absolute_path = if cfg!(windows) {
        absolute_path.replace('\\', "/")
    } else {
        absolute_path.to_owned()
    };
    let mut url = String::with_capacity(7 + absolute_path.len().saturating_mul(3));
    if cfg!(windows) {
        url.push_str("file:///");
    } else {
        url.push_str("file://");
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in absolute_path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(char::from(byte));
        } else {
            url.push('%');
            url.push(char::from(HEX[usize::from(byte >> 4)]));
            url.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    Some(url)
}

#[allow(clippy::items_after_statements)]
fn write_status_path<W: io::Write>(
    writer: &mut W,
    color: AnsiColor,
    path: &[u8],
    color_enabled: bool,
    link_root: Option<&std::path::Path>,
) -> io::Result<()> {
    let Some(url) = link_root.and_then(|root| file_url_for_status_path(root, path)) else {
        return write_styled(writer, color, path, color_enabled);
    };
    writer.write_all(b"\x1b]8;;")?;
    writer.write_all(url.as_bytes())?;
    writer.write_all(b"\x1b\\")?;
    write_styled(writer, color, path, color_enabled)?;
    writer.write_all(b"\x1b]8;;\x1b\\")
}

fn write_human_status_path<W: io::Write>(
    writer: &mut W,
    color: AnsiColor,
    action: &[u8],
    path: &[u8],
    color_enabled: bool,
    link_root: Option<&std::path::Path>,
) -> io::Result<()> {
    if matches!(action, b"renamed" | b"copied") {
        let separator = b" -> ";
        let Some(separator_start) = path
            .windows(separator.len())
            .position(|window| window == separator)
        else {
            return write_styled(writer, color, path, color_enabled);
        };
        if path[separator_start + separator.len()..]
            .windows(separator.len())
            .any(|window| window == separator)
        {
            return write_styled(writer, color, path, color_enabled);
        }
        write_status_path(
            writer,
            color,
            &path[..separator_start],
            color_enabled,
            link_root,
        )?;
        writer.write_all(separator)?;
        return write_status_path(
            writer,
            color,
            &path[separator_start + separator.len()..],
            color_enabled,
            link_root,
        );
    }
    write_status_path(writer, color, path, color_enabled, link_root)
}

fn write_human_status<W: io::Write>(
    output: &[u8],
    writer: &mut W,
    color_enabled: bool,
    link_root: Option<&std::path::Path>,
) -> io::Result<()> {
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        let has_newline = line.last() == Some(&b'\n');
        let content = &line[..line.len() - usize::from(has_newline)];
        if let Some(branch) = content.strip_prefix(b"On branch ") {
            writer.write_all(b"On branch ")?;
            write_styled(writer, AnsiColor::Cyan, branch, color_enabled)?;
        } else if content.starts_with(b"Changes to be committed:")
            || content.starts_with(b"Changes not staged for commit:")
            || content.starts_with(b"Untracked files:")
        {
            write_styled(writer, AnsiColor::Yellow, content, color_enabled)?;
        } else if content.starts_with(b"nothing to commit") {
            write_styled(writer, AnsiColor::Green, content, color_enabled)?;
        } else if content.starts_with(b"Your branch ") || content.starts_with(b"  (use ") {
            write_styled(writer, AnsiColor::Dim, content, color_enabled)?;
        } else if content.first() == Some(&b'\t')
            && let Some(colon) = content.iter().position(|byte| *byte == b':')
        {
            let action = &content[1..colon];
            let rest = &content[colon + 1..];
            let path_start = rest
                .iter()
                .position(|byte| !matches!(*byte, b' ' | b'\t'))
                .unwrap_or(rest.len());
            let color = if action.starts_with(b"both ")
                || action.starts_with(b"unmerged")
                || action.starts_with(b"added by ")
                || action.starts_with(b"deleted by ")
            {
                AnsiColor::Red
            } else {
                AnsiColor::Yellow
            };
            writer.write_all(b"\t")?;
            write_styled(writer, color, action, color_enabled)?;
            writer.write_all(b":")?;
            writer.write_all(&rest[..path_start])?;
            write_human_status_path(
                writer,
                AnsiColor::Cyan,
                action,
                &rest[path_start..],
                color_enabled,
                link_root,
            )?;
        } else if content.first() == Some(&b'\t') && content.len() > 1 {
            let rest = &content[1..];
            let path_start = rest
                .iter()
                .position(|byte| !matches!(*byte, b' ' | b'\t'))
                .unwrap_or(rest.len());
            writer.write_all(b"\t")?;
            writer.write_all(&rest[..path_start])?;
            let path = &rest[path_start..];
            if path.first() == Some(&b'(') {
                write_styled(writer, AnsiColor::Cyan, path, color_enabled)?;
            } else {
                write_human_status_path(
                    writer,
                    AnsiColor::Cyan,
                    b"",
                    path,
                    color_enabled,
                    link_root,
                )?;
            }
        } else {
            writer.write_all(content)?;
        }
        if has_newline {
            writer.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn write_short_status<W: io::Write>(
    output: &[u8],
    writer: &mut W,
    color_enabled: bool,
) -> io::Result<()> {
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        let has_newline = line.last() == Some(&b'\n');
        let content = &line[..line.len() - usize::from(has_newline)];
        if content.len() >= 3 && content[2] == b' ' {
            let code = &content[..2];
            let color = if code[0] == b'U'
                || code[1] == b'U'
                || matches!(
                    (code[0], code[1]),
                    (b'D' | b'U', b'D') | (b'A' | b'U', b'A') | (b'A' | b'D', b'U')
                ) {
                AnsiColor::Red
            } else {
                AnsiColor::Yellow
            };
            write_styled(writer, color, code, color_enabled)?;
            writer.write_all(b" ")?;
            write_styled(writer, AnsiColor::Cyan, &content[3..], color_enabled)?;
        } else {
            writer.write_all(content)?;
        }
        if has_newline {
            writer.write_all(b"\n")?;
        }
    }
    Ok(())
}

fn run_status_human(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    let output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args(["-c", "color.status=false", "status"]),
        "check that Git can read the local worktree, then retry `status`",
    )?;
    let stdout = io::stdout();
    let link_root = if stdout.is_terminal() && terminal_supports_osc8() {
        repository.worktree().canonicalize().ok()
    } else {
        None
    };
    let mut stdout = stdout.lock();
    write_human_status(
        &output.stdout,
        &mut stdout,
        color_enabled,
        link_root.as_deref(),
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status`",
        )
    })?;
    write_unpublished_summary(&mut stdout, unpublished_count).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status`",
        )
    })?;
    Ok(())
}

fn run_status_short(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    const RETRY: &str = "check that Git can read the local worktree, then retry `status --short`";
    let repository = discover_worktree(endpoint, DISCOVER_WORKTREE_HELP)?;
    let git_dir_output = cli_status::git_output(
        cli_status::git_in(repository.worktree()).args(["rev-parse", "--git-dir"]),
        RETRY,
    )?;
    let git_dir = String::from_utf8(git_dir_output.stdout).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            RETRY,
        )
    })?;
    let git_dir = std::path::Path::new(git_dir.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_owned()
    } else {
        repository.worktree().join(git_dir)
    };
    let output = cli_status::git_output(
        cli_status::scrubbed_git()
            .arg("--git-dir")
            .arg(&git_dir)
            .arg("--work-tree")
            .arg(repository.worktree())
            .args(["-c", "color.status=false", "status", "--short"]),
        RETRY,
    )?;
    let mut stdout = io::stdout().lock();
    write_short_status(&output.stdout, &mut stdout, color_enabled).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status --short`",
        )
    })?;
    Ok(())
}

fn load_project(endpoint: &Endpoint) -> Result<orna_project::LoadedProject, Diagnostic> {
    let project = load_project_without_standard_rejection(endpoint)?;
    reject_uncaptured_standard_modules(&project)?;
    Ok(project)
}

fn load_project_without_standard_rejection(
    endpoint: &Endpoint,
) -> Result<orna_project::LoadedProject, Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "project Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let project = orna_project::ProjectLoader::default()
        .load(&repository)
        .map_err(project_load_diagnostic)?;
    Ok(project)
}

fn reject_uncaptured_standard_modules(
    project: &orna_project::LoadedProject,
) -> Result<(), Diagnostic> {
    if !project.has_uncaptured_standard_imports() {
        return Ok(());
    }
    Err(Diagnostic::target(
        "ORNA-S010-IMPORT",
        "imported module is unavailable",
        "use a captured standard dependency or remove the import",
    ))
}

fn semantic_catalogue(
    project: &orna_project::LoadedProject,
) -> Result<orna_semantic::Catalogue, Diagnostic> {
    project.production_catalogue().map_err(|_| {
        Diagnostic::target(
            "ORNA-S010-IMPORT",
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    })
}

fn check_project(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic::SystemProviders::Production,
    );
    if analysis.is_ok() {
        write_success_line("project valid", color_enabled)
    } else {
        let first = analysis
            .diagnostics
            .first()
            .expect("non-empty diagnostics when semantic analysis fails");
        Err(Diagnostic::target_with_detail(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
            format!("{}: {}", first.code(), first.message()),
        ))
    }
}

fn execution_project(project: &orna_project::LoadedProject) -> ProjectUnit {
    let mut modules: Vec<SourceUnit> = project
        .modules()
        .iter()
        .zip(project.identities())
        .map(|(module, identity)| SourceUnit {
            fixture_id: "cli-project".into(),
            source_id: identity.logical_path().into(),
            parse_as: "module_unit".into(),
            source: module.source.clone(),
        })
        .collect();
    let loose_rows: Vec<SourceUnit> = project
        .loose_rows()
        .iter()
        .map(|row| SourceUnit {
            fixture_id: "cli-project".into(),
            source_id: row.logical_path().into(),
            parse_as: row.parse_as().into(),
            source: row.source().into(),
        })
        .collect();
    modules.extend(
        project
            .standard_sources()
            .iter()
            .filter(|(logical_path, _)| project.standard_runtime_modules().contains(logical_path))
            .map(|(logical_path, source)| SourceUnit {
                fixture_id: "cli-project".into(),
                source_id: logical_path.clone(),
                parse_as: "module_unit".into(),
                source: source.clone(),
            }),
    );
    ProjectUnit {
        fixture_id: "cli-project".into(),
        project_id: "cli-project".into(),
        environment_id: None,
        modules,
        loose_rows,
        expectations: ProjectExpectations {
            environment: ProjectEnvironment {
                network: false,
                credentials: false,
                intrinsics: "Orna 1.1.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

/// The bounded pure path receives reachable standard modules through
/// `load_standard_sources`, which verifies them against the captured profile.
/// The project unit therefore carries neither those source units nor a
/// standard dependency, so admission has nothing further to verify.
fn pure_execution_project(project: &orna_project::LoadedProject) -> ProjectUnit {
    let mut execution = execution_project(project);
    execution
        .modules
        .retain(|unit| !project.standard_runtime_modules().contains(&unit.source_id));
    execution
}

fn runtime_identity(
    repository: &orna_repository::Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), Diagnostic> {
    orna_runtime::repository_runtime_identity(repository).map_err(|error| match error {
        orna_runtime::RuntimeError::UnknownHeadFormat(
            refusal @ orna_repository::FormatContextError::MixedFormats { .. },
        ) => cli_commit::mixed_formats_at_head(&refusal),
        orna_runtime::RuntimeError::UnknownHeadFormat(refusal) => Diagnostic::target(
            refusal.code(),
            "repository format is unknown",
            "the repository HEAD records a format this build does not know; use a build that supports it",
        ),
        _ => Diagnostic::target(
            "E2200",
            "repository runtime metadata is unavailable",
            "initialize the Orna repository before running a durable project invocation",
        ),
    })
}

/// The writer owner for this process. A later process has a different owner, so
/// it takes the lease over (bumping the epoch) and fences a killed writer's token.
fn invocation_owner_id() -> [u8; 16] {
    orna_runtime::process_owner_id()
}

fn run_project_invocation(
    endpoint: &Endpoint,
    root_entry: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_invocation_with_arguments(endpoint, root_entry, &Environment::new(), color_enabled)
}

fn public_project_function(analysis: &orna_semantic::Analysis, target: &str) -> bool {
    let Some((module, function)) = target.rsplit_once('.') else {
        return false;
    };
    let namespace = if module == "main" {
        Vec::new()
    } else {
        module.split('.').map(str::to_owned).collect()
    };
    if function.is_empty() || namespace.iter().any(String::is_empty) {
        return false;
    }
    analysis
        .modules
        .get(&orna_semantic::Namespace(namespace))
        .and_then(|module| module.exports.get(function))
        .is_some_and(|symbol| symbol.kind == orna_semantic::SymbolKind::Function)
}
fn run_public_project_function(
    endpoint: &Endpoint,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_function(endpoint, target, false, color_enabled)
}
fn run_default_project_main(endpoint: &Endpoint, color_enabled: bool) -> Result<(), Diagnostic> {
    run_project_function(endpoint, "main.main", true, color_enabled)
}
fn run_project_function(
    endpoint: &Endpoint,
    target: &str,
    allow_private_root_main: bool,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = std::sync::Arc::new(semantic_catalogue(&project)?);
    let analysis = orna_semantic::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    let private_default_main_present = allow_private_root_main
        && target == "main.main"
        && analysis
            .modules
            .get(&orna_semantic::Namespace(Vec::new()))
            .and_then(|module| module.symbols.get("main"))
            .is_some_and(|symbol| symbol.kind == orna_semantic::SymbolKind::Function);
    if allow_private_root_main && !private_default_main_present {
        return Err(Diagnostic::target(
            "E2103",
            "no root main(): main.orna declares no `main()` function",
            "declare `main()` in main.orna, or run a qualified function such as `orna run MODULE.FUNCTION`",
        ));
    }
    if !private_default_main_present && !public_project_function(&analysis, target) {
        return Err(Diagnostic::unavailable(
            "durable project invocation is not available",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        ));
    }
    let execution = execution_project(&project);
    if DurableTransactionalEvaluator::default()
        .with_catalogue(std::sync::Arc::clone(&catalogue))
        .project_stream_root_admitted(&execution, target)
    {
        return run_project_stream_invocation_with_project(
            endpoint,
            target,
            &execution,
            sys_host::std_snapshot_pin(&project),
            catalogue,
            color_enabled,
        );
    }
    run_project_invocation(endpoint, target, color_enabled)
}
fn run_project_stream_invocation(
    endpoint: &Endpoint,
    root_entry: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = std::sync::Arc::new(semantic_catalogue(&project)?);
    let analysis = orna_semantic::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    let execution = execution_project(&project);
    run_project_stream_invocation_with_project(
        endpoint,
        root_entry,
        &execution,
        sys_host::std_snapshot_pin(&project),
        catalogue,
        color_enabled,
    )
}
fn run_project_invocation_with_arguments(
    endpoint: &Endpoint,
    root_entry: &str,
    arguments: &Environment,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository =
        orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not be discovered",
                "run the command inside an initialized local Git worktree",
            )
        })?;
    let project = load_project(endpoint)?;
    let catalogue = admit_project_analysis(&project)?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    refuse_legacy_bare_module_rows(&repository, identity, initial_digest, &project)?;
    let (request, fingerprint) = DurableTransactionalEvaluator::project_request_identity(
        identity,
        initial_digest,
        root_entry,
        arguments,
        cli_request_nonce(),
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "project request identity could not be created",
            "retry the durable project invocation",
        )
    })?;
    let std_snapshot = sys_host::std_snapshot_pin(&project);
    let table_object_ids = committed_table_object_ids(&repository, &project)?;
    let storage_orders = committed_storage_orders(&repository, &project)?;
    // Admission is bound to the committed snapshot when there is one; a
    // repository with no commit yet runs from the worktree project, which is
    // what the seed test exercises (bead ornadb-jm0d).
    let loaded_project = match repository.head() {
        Ok(Some(head)) => orna_project::ProjectLoader::default()
            .load_committed_snapshot(&repository, &head)
            .map_err(project_load_diagnostic)?,
        _ => project.clone(),
    };
    let project = execution_project(&loaded_project);
    let owner_id = invocation_owner_id();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not start",
                "retry the durable project invocation",
            )
        })?
        .block_on(
            DurableTransactionalEvaluator::default()
                .with_catalogue(catalogue)
                .with_sys_host_context(sys_host::sys_host_context(
                    repository.worktree(),
                    identity,
                    RuntimeMode::Run,
                    std_snapshot,
                ))
                .execute_project_request_with_arguments(
                    orna_conformance::RuntimeTarget {
                        repository: &repository,
                        identity,
                        owner_id,
                        initial_digest,
                    },
                    request,
                    fingerprint,
                    &project,
                    root_entry,
                    arguments,
                    &loaded_project,
                    &table_object_ids,
                    &storage_orders,
                ),
        )
        .map_err(|error| durable_invocation_failed(&error))?;
    report_project_outcome(endpoint, root_entry, arguments, color_enabled, outcome)
}

/// The production catalogue of a project that passes semantic analysis.
fn admit_project_analysis(
    project: &orna_project::LoadedProject,
) -> Result<std::sync::Arc<orna_semantic::Catalogue>, Diagnostic> {
    let catalogue = std::sync::Arc::new(semantic_catalogue(project)?);
    let analysis = orna_semantic::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    Ok(catalogue)
}

/// Routes a stream-root skip to the stream seam and reports the transaction's
/// outcome.
fn report_project_outcome(
    endpoint: &Endpoint,
    root_entry: &str,
    arguments: &Environment,
    color_enabled: bool,
    outcome: StageOutcome<orna_foundation::Diagnostic>,
) -> Result<(), Diagnostic> {
    // Project transaction admission does not run stream roots; those route to
    // the explicit finite-list stream seam. The adapter reports that skip as a
    // typed reason, so the routing cannot drift from its text.
    if arguments.is_empty() && outcome.is_stream_root_admission_skip() {
        return run_project_stream_invocation(endpoint, root_entry, color_enabled);
    }
    match outcome {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Failed(diagnostic) => Err(
            if let Some(mut published) = published_runtime_failure(&diagnostic) {
                published.detail = Some(
                    "the project transaction failed atomically; no partial changes were committed"
                        .into(),
                );
                published
            } else {
                let mut rejection = Diagnostic::target(
                    "E2200",
                    "durable project invocation was rejected",
                    "the project transaction failed atomically; no partial changes were committed",
                );
                rejection.detail = Some(failure_detail(&diagnostic));
                rejection
            },
        ),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "durable project invocation is not available",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        )),
    }
}
fn run_project_stream_invocation_with_project(
    endpoint: &Endpoint,
    root_entry: &str,
    project: &ProjectUnit,
    std_snapshot: StdSnapshotPin,
    catalogue: std::sync::Arc<orna_semantic::Catalogue>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository =
        orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not be discovered",
                "run the command inside an initialized local Git worktree",
            )
        })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let owner_id = invocation_owner_id();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not start",
                "retry the durable project invocation",
            )
        })?
        .block_on(
            DurableTransactionalEvaluator::default()
                .with_catalogue(catalogue)
                .with_sys_host_context(sys_host::sys_host_context(
                    repository.worktree(),
                    identity,
                    RuntimeMode::Run,
                    std_snapshot,
                ))
                .execute_project_stream(
                    &repository,
                    identity,
                    owner_id,
                    initial_digest,
                    project,
                    root_entry,
                ),
        )
        .map_err(|error| durable_invocation_failed(&error))?;
    match outcome {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Failed(diagnostic) => Err(stream_delivery_failed(&diagnostic)),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "durable project stream invocation is not available",
            "use a supported finite-list stream root",
        )),
    }
}

/// Publishes a runtime failure under its catalogued Orna code, with the
/// catalogue title and help. Uncatalogued evaluator codes return `None` so each
/// caller keeps its own fallback; no unpublished code is promoted.
fn published_runtime_failure(diagnostic: &orna_foundation::Diagnostic) -> Option<Diagnostic> {
    let documentation = diagnostic_documentation(diagnostic.code())?;
    Some(Diagnostic {
        code: documentation.code,
        title: Cow::Borrowed(documentation.title),
        help: documentation.help,
        exit: Exit::Target,
        detail: None,
        body: None,
    })
}

/// Carries the runtime failure's public code, or its non-disclosing display
/// text, into the diagnostic detail. The detail is shown with `--debug` only
/// (ORNA-UX-004 keeps technical causes out of the default rendering).
fn runtime_failure_detail(error: &orna_runtime::RuntimeError) -> String {
    if let orna_runtime::RuntimeError::LegacyHeadReadOnly(refusal) = error {
        return format!("runtime failure: {} ({})", refusal.code(), refusal.title());
    }
    let cause = error
        .public_code()
        .map_or_else(|| error.to_string(), |code| code.as_str().to_owned());
    format!("runtime failure: {cause}")
}

/// Renders the evaluator failure code with the codes of its nested causes.
/// Messages and spans stay redacted at the evaluator boundary.
fn failure_detail(diagnostic: &orna_foundation::Diagnostic) -> String {
    let causes = diagnostic.causes();
    if causes.is_empty() {
        return format!("failure code: {}", diagnostic.code());
    }
    let codes = causes
        .iter()
        .map(orna_foundation::Diagnostic::code)
        .collect::<Vec<_>>()
        .join(", ");
    format!("failure code: {} (causes: {codes})", diagnostic.code())
}

/// Maps a failed durable project invocation to `E2200`. The runtime's public
/// code, or its non-disclosing display text, is carried in the detail, which
/// renders only with `--debug` (ORNA-UX-004). A legacy head refusal names its
/// own remedy, because retrying the transaction cannot clear it.
fn durable_invocation_failed(error: &orna_runtime::RuntimeError) -> Diagnostic {
    let help = match error {
        orna_runtime::RuntimeError::LegacyHeadReadOnly(refusal) => refusal.remedy(),
        _ => "retry the project transaction after checking its durable state",
    };
    let mut diagnostic = Diagnostic::target("E2200", "durable project invocation failed", help);
    diagnostic.detail = Some(runtime_failure_detail(error));
    diagnostic
}

/// Maps a stream delivery the runtime rolled back to `E2200`, carrying the
/// evaluator failure code and its nested cause codes in the `--debug` detail.
fn stream_delivery_failed(diagnostic: &orna_foundation::Diagnostic) -> Diagnostic {
    let mut delivery = Diagnostic::target(
        "E2200",
        "durable project stream delivery failed",
        "the failed delivery was rolled back; prior committed deliveries and their checkpoint progress remain; inspect the recorded failure before retrying the stream",
    );
    delivery.detail = Some(failure_detail(diagnostic));
    delivery
}

/// A multi-module project that declares a table cannot run one-shot: the bounded
/// pure evaluator rejects any table declaration, and the committed-table route
/// admits a single module only. Single-module projects never reach this check.
fn multi_module_declares_tables(project: &orna_project::LoadedProject) -> bool {
    project.modules().len() != 1
        && project.modules().iter().any(|module| {
            orna_syntax::parse_module(&module.source)
                .value
                .items
                .iter()
                .any(|item| matches!(item.declaration, orna_syntax::Declaration::Table { .. }))
        })
}

fn run_pure_invocation(
    endpoint: &Endpoint,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    if run_authority_invocation(endpoint, &project, target, color_enabled)? {
        return Ok(());
    }
    admit_function_only_project(&project)?;
    run_bounded_invocation(&project, target, color_enabled)
}

/// Admits and evaluates `target` through the authority route when the only
/// module is the one `target` names. Returns whether it answered, so every
/// other project takes the bounded function-only path.
fn run_authority_invocation(
    endpoint: &Endpoint,
    project: &orna_project::LoadedProject,
    target: &str,
    color_enabled: bool,
) -> Result<bool, Diagnostic> {
    // Single-module projects that import pinned standard modules take the
    // authority route only when they need publication metadata. Every other
    // standard-importing project takes the bounded path below, which loads the
    // reachable standard bodies through the verified profile.
    let std_imports = !project.standard_runtime_modules().is_empty();
    if project.modules().len() != 1 {
        return Ok(false);
    }
    let identity = &project.identities()[0];
    let module = &project.modules()[0];
    let (module_name, function) = target.rsplit_once('.').unwrap_or(("", target));
    let namespace = if module_name.is_empty() {
        Vec::new()
    } else {
        module_name
            .split('.')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    if identity.namespace() != namespace {
        return Ok(false);
    }
    let authority = ApplicationAuthority::new(
        orna_semantic::Catalogue::authoritative_core(),
        Limits::default(),
    )
    .with_loaded_standard(project)
    .map_err(|_| {
        Diagnostic::target(
            "ORNA-S010-IMPORT",
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    })?;
    let admission = authority
        .admit_module(identity.logical_path(), module.source.clone(), function)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2101",
                "project semantic analysis failed",
                "fix the first reported source contract error, then run check again",
                error.to_string(),
            )
        });
    let admitted = match admission {
        Ok(admitted) => Some(admitted),
        // The bounded path reports the analysis error for std-importing projects.
        Err(_) if std_imports => None,
        Err(error) => return Err(error),
    };
    let Some(admitted) =
        admitted.filter(|candidate| !std_imports || candidate.requires_publication_metadata())
    else {
        return Ok(false);
    };
    if !admitted.table_schemas().is_empty() && admitted.entry_references_tables() {
        run_committed_table_invocation(
            endpoint,
            authority,
            &admitted,
            identity.logical_path(),
            &module.source,
            sys_host::std_snapshot_pin(project),
        )?;
    } else if admitted.requires_publication_metadata() {
        let (publication_rows, sys_host_context) =
            runtime_publication_metadata_rows(endpoint, sys_host::std_snapshot_pin(project))?;
        let authority = authority.with_sys_host_context(sys_host_context);
        let activation = authority
            .evaluate_staged_with_publication_rows(&admitted, &Environment::new(), publication_rows)
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "pure project invocation failed",
                    "fix the admitted function or retry the invocation",
                    error.to_string(),
                )
            })?;
        if !activation.mutations().is_empty() {
            return Err(Diagnostic::unavailable(
                "one-shot invocation cannot commit table mutations",
                "use a supported table transaction; stream roots require the explicit stream runtime",
            ));
        }
    } else {
        let std_snapshot = sys_host::std_snapshot_pin(project);
        let authority = match pure_sys_host_context(endpoint, std_snapshot) {
            Some(sys_host_context) => authority.with_sys_host_context(sys_host_context),
            None => authority,
        };
        authority
            .evaluate(&admitted, &Environment::new())
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "pure project invocation failed",
                    "fix the admitted function or retry the invocation",
                    error.to_string(),
                )
            })?;
    }
    write_success_line("invocation completed", color_enabled)?;
    Ok(true)
}

/// Proves the project is function-only and can be read by the bounded
/// evaluator.
fn admit_function_only_project(project: &orna_project::LoadedProject) -> Result<(), Diagnostic> {
    let catalogue = semantic_catalogue(project)?;
    let analysis = orna_semantic::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
        ));
    }
    if multi_module_declares_tables(project) {
        return Err(Diagnostic::target(
            "E2200",
            "table reads in multi-module projects are not supported by one-shot invocation; use the REPL or orna run",
            "one-shot invocation admits table declarations only in single-module projects",
        ));
    }
    Ok(())
}

/// Runs `target` through the bounded function-only evaluator, loading the
/// verified standard profile first when the project pins one.
fn run_bounded_invocation(
    project: &orna_project::LoadedProject,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let mut evaluator = BoundedEvaluator::default();
    if let Some(profile) = project.standard_profile() {
        // Admission supplies the runtime-reachable closure (requested roots plus
        // `main`) and the profile must accept exactly that key set, so every
        // supplied module digest is verified before any body is evaluated.
        let sources = project.standard_sources().to_vec();
        match evaluator.load_standard_sources(profile, sources) {
            StageOutcome::Passed => {}
            StageOutcome::Cancelled(diagnostic) => {
                return Err(cancellation_diagnostic(&diagnostic));
            }
            StageOutcome::Failed(diagnostic) => {
                return Err(published_runtime_failure(&diagnostic).unwrap_or_else(|| {
                    Diagnostic::target_with_detail(
                        "ORNA-S010-IMPORT",
                        "imported module is unavailable",
                        "use a captured standard dependency or remove the import",
                        format!(
                            "standard dependency load rejected with {}",
                            diagnostic.code()
                        ),
                    )
                }));
            }
            StageOutcome::Skipped { .. } => {
                return Err(Diagnostic::target(
                    "ORNA-S010-IMPORT",
                    "imported module is unavailable",
                    "use a captured standard dependency or remove the import",
                ));
            }
        }
    }
    match evaluator.evaluate_project(&pure_execution_project(project)) {
        StageOutcome::Passed => {}
        StageOutcome::Cancelled(diagnostic) => return Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Failed(diagnostic) => {
            return Err(published_runtime_failure(&diagnostic).unwrap_or_else(|| {
                Diagnostic::unavailable(
                    "one-shot invocation requires an executable function-only project",
                    "use reachable modules containing only pure functions; tables, effects, and streams require the integrated runtime",
                )
            }));
        }
        StageOutcome::Skipped { .. } => {
            return Err(Diagnostic::unavailable(
                "one-shot invocation requires an executable function-only project",
                "use reachable modules containing only pure functions; tables, effects, and streams require the integrated runtime",
            ));
        }
    }
    match evaluator.invoke(target) {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Failed(diagnostic) => Err(published_runtime_failure(&diagnostic)
            .unwrap_or_else(|| {
                Diagnostic::unavailable(
                    "requested invocation is not available",
                    "define a reachable zero-argument pure function with the requested name",
                )
            })),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "requested invocation is not available",
            "define a reachable zero-argument pure function with the requested name",
        )),
    }
}

/// Runtime authority retained by one CLI REPL session.
///
/// Project loading and later snapshot selection share this opened repository;
/// callers can also use it as the explicit injection point for repository
/// capabilities without process-global state.
struct ReplRuntime {
    repository: Option<orna_repository::Repository>,
    session: AdmittedReplSession,
    transactions: Option<ReplTransactions>,
}

struct ReplTransactions {
    project: orna_project::LoadedProject,
    commit: orna_repository::GitCommitRef,
    format_context: orna_repository::RepositoryFormatContext,
    authority: ApplicationAuthority,
    runtime: tokio::runtime::Runtime,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    tables: Vec<RuntimeTableIdentity>,
    identities: orna_project::ObjectIdentityManifest,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    table_schemas: BTreeMap<String, orna_semantic::TableSchema>,
    capture_relation_id: [u8; 16],
    session_id: [u8; 16],
    host_bindings: Rc<RefCell<Option<SysHostBindingRegistry>>>,
    faults: Arc<dyn FaultInjector>,
    // Drop the runtime owner lock only after capture candidates release their
    // refs, so the writer holding it stays the last field.
    writer: Option<ReplWriter>,
}

impl ReplTransactions {
    /// The runtime state this session's first input opened, for tests that
    /// inspect the request rows it left behind.
    #[cfg(test)]
    fn writer_state(&self) -> Option<&RuntimeState> {
        self.writer.as_ref().map(|writer| &writer.state)
    }
}

/// Reads the committed identity of each named admitted table from `commit`.
/// Each name must resolve to exactly one live table record; the error names the
/// table that does not, so the caller fails closed with a reason. The REPL and
/// one-shot `run` both bind tables through this function, so neither resolves a
/// table by name alone.
/// The stored field order of every admitted table at HEAD.
///
/// A published row is positional in this order, which is not the declaration
/// order, so the run path rebuilds seeded records from it (bead ornadb-jm0d).
fn committed_storage_orders(
    repository: &orna_repository::Repository,
    project: &orna_project::LoadedProject,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, Diagnostic> {
    let (table_names, schemas) = crate::assertion_gate::admitted_tables_of(project)
        .map_err(|_| unavailable_table_identity("project tables could not be identified".into()))?;
    let Ok(Some(head)) = repository.head() else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Ok(tables) = committed_table_identities(repository, &head, table_names) else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Ok((_rows, storage_orders)) = committed_rows_at(repository, &head, &schemas, &tables)
    else {
        return Ok(std::collections::BTreeMap::new());
    };
    Ok(storage_orders)
}

fn committed_table_object_ids(
    repository: &orna_repository::Repository,
    project: &orna_project::LoadedProject,
) -> Result<std::collections::BTreeMap<String, TableObjectId>, Diagnostic> {
    let (table_names, _schemas) = crate::assertion_gate::admitted_tables_of(project)
        .map_err(|_| unavailable_table_identity("project tables could not be identified".into()))?;
    // A repository with no commit yet has no admitted identities, and the run
    // still has to execute (bead ornadb-jm0d).
    let Ok(Some(head)) = repository.head() else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Ok(identities) = committed_table_identities(repository, &head, table_names) else {
        return Ok(std::collections::BTreeMap::new());
    };
    Ok(identities
        .into_iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect())
}

fn committed_table_identities(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    table_names: Vec<String>,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let identities =
        orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| {
                unavailable_table_identity(
                    "the committed object identity manifest could not be read".to_owned(),
                )
            })?
            .ok_or_else(|| {
                unavailable_table_identity(
                    "the commit carries no object identity inventory".to_owned(),
                )
            })?;
    table_identities_from_manifest(&identities, table_names)
}

/// Resolves each named table to its one live identity in `identities`. Each
/// name must resolve to exactly one live table record, so no caller binds a
/// table by name alone.
fn table_identities_from_manifest(
    identities: &orna_project::ObjectIdentityManifest,
    table_names: Vec<String>,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let mut tables = Vec::with_capacity(table_names.len());
    for name in table_names {
        let mut matching = identities.records().iter().filter(|record| {
            record.kind() == orna_project::ObjectKind::Table
                && !record.is_retired()
                && record.qualified_name() == name.as_str()
        });
        let record = matching.next().ok_or_else(|| {
            unavailable_table_identity(format!("table `{name}` has no live identity at HEAD"))
        })?;
        if matching.next().is_some() {
            return Err(unavailable_table_identity(format!(
                "table `{name}` has more than one live identity at HEAD"
            )));
        }
        let identity = RuntimeTableIdentity::new(name.clone(), TableObjectId::new(*record.id()))
            .map_err(|_| {
                unavailable_table_identity(format!(
                    "table `{name}` has an invalid identity at HEAD"
                ))
            })?;
        tables.push(identity);
    }
    Ok(tables)
}

fn unavailable_table_identity(detail: String) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2200",
        "committed table identities are unavailable",
        "run `orna commit -m MESSAGE` so every table has exactly one identity",
        detail,
    )
}

/// Returns the first module table whose rows still sit under its bare name. A
/// module table `library.Book` is stored as `library.Book`; earlier builds
/// stored it as `Book`, which the qualified table cannot see: reads miss those
/// rows and writes fork the relation. `candidates` comes from
/// [`legacy_bare_module_candidates`].
fn legacy_bare_module_table(
    state: &RuntimeState,
    runtime: &tokio::runtime::Runtime,
    candidates: &[(String, String)],
) -> Result<Option<String>, orna_runtime::RuntimeError> {
    for (qualified, bare) in candidates {
        if runtime.block_on(state.has_table_rows(bare))? {
            return Ok(Some(qualified.clone()));
        }
    }
    Ok(None)
}

/// The module tables whose rows may sit under a bare name, as `(qualified,
/// bare)` pairs. `declared` are the qualified names the caller admits; when
/// `commit` is given, every table recorded in its committed identity manifest
/// is added, retired ones included. A live `ns.B` is a candidate unless a table
/// named `B`, live or retired, exists: a bare name a root table owns, even one
/// since removed, is that table's data, never a legacy module row.
fn legacy_bare_module_candidates(
    repository: &orna_repository::Repository,
    commit: Option<&orna_repository::GitCommitRef>,
    declared: &[&str],
) -> Result<Vec<(String, String)>, orna_project::ObjectIdentityError> {
    let mut tables = declared
        .iter()
        .map(|name| ((*name).to_owned(), false))
        .collect::<Vec<_>>();
    let manifest = match commit {
        Some(commit) => Some(orna_project::ObjectIdentityManifest::read_committed(
            repository, commit,
        )?),
        None => None,
    };
    if let Some(manifest) = manifest {
        tables.extend(
            manifest
                .records()
                .iter()
                .filter(|record| record.kind() == orna_project::ObjectKind::Table)
                .map(|record| (record.qualified_name().to_owned(), record.is_retired())),
        );
    }
    let owned = tables
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let mut candidates = BTreeSet::new();
    for (qualified, retired) in &tables {
        if *retired {
            continue;
        }
        let Some((_, bare)) = qualified.rsplit_once('.') else {
            continue;
        };
        if !owned.contains(bare) {
            candidates.insert((qualified.clone(), bare.to_owned()));
        }
    }
    Ok(candidates.into_iter().collect())
}

/// Refuses a multi-module durable invocation whose store holds rows of a
/// declared module table under its bare name. The storage names are derived
/// from each module's namespace and table declarations, as admission names them.
/// The candidates come from the committed identity manifest at `HEAD` when a
/// commit exists, so a removed root table keeps its bare name.
fn refuse_legacy_bare_module_rows(
    repository: &orna_repository::Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    project: &orna_project::LoadedProject,
) -> Result<(), Diagnostic> {
    let mut declared = Vec::new();
    for (module, module_identity) in project.modules().iter().zip(project.identities()) {
        for item in orna_syntax::parse_module(&module.source).value.items {
            if let orna_syntax::Declaration::Table { name, .. } = item.declaration {
                let mut qualified = module_identity.namespace().to_vec();
                qualified.push(name);
                declared.push(qualified.join("."));
            }
        }
    }
    if declared.is_empty() {
        return Ok(());
    }
    let unreadable = || {
        Diagnostic::target(
            "E2200",
            "committed table rows could not be read",
            "retry the invocation after checking local runtime state",
        )
    };
    // An unborn HEAD has no committed table relation, so only the declared
    // names can name legacy rows. An unreadable HEAD is not evidence of
    // absence: refuse it.
    let Ok(commit) = repository.head() else {
        return Err(Diagnostic::target(
            "E2200",
            "committed HEAD is unreadable",
            "repair the repository HEAD reference, then run again",
        ));
    };
    if let Some(commit) = &commit {
        // A commit without an identity inventory never allocated its tables'
        // identities. Evaluating its table statements would touch no committed
        // relation, so refuse with the remedy that allocates the identities.
        if orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| unreadable())?
            .is_none()
        {
            return Err(unavailable_table_identity(
                "the commit carries no object identity inventory".to_owned(),
            ));
        }
    }
    let declared = declared.iter().map(String::as_str).collect::<Vec<_>>();
    let candidates = legacy_bare_module_candidates(repository, commit.as_ref(), &declared)
        .map_err(|_| unreadable())?;
    if candidates.is_empty() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| unreadable())?;
    let state = runtime
        .block_on(RuntimeState::open(repository, identity, initial_digest))
        .map_err(|_| unreadable())?;
    match legacy_bare_module_table(&state, &runtime, &candidates).map_err(|_| unreadable())? {
        Some(table) => Err(legacy_module_table_diagnostic(&table)),
        None => Ok(()),
    }
}

fn legacy_module_table_diagnostic(table: &str) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2200",
        "committed table rows are stored under a bare module name",
        "re-create the store from the committed project; rows under bare module names are not migrated",
        format!("module table `{table}` has rows stored under its bare name by an earlier build"),
    )
}

/// Committed `HEAD` rows of every admitted table, pinned to the published
/// generation they were read at. The rows are stored tuples in the runtime
/// keyspace: the runtime merges them with the live tail, so they stay in the
/// stored form. `storage_orders` names the stored fields each table's tuples
/// list, which is what converts the merged rows to evaluator records.
struct PinnedCommittedTables {
    rows: PinnedCommittedRows,
    storage_orders: BTreeMap<String, Vec<String>>,
}

/// Evaluates an admitted module over the activation at `state`: the snapshot
/// holds the committed rows of every admitted table, and the staged result
/// carries only the mutations this activation produced. `publication_rows`
/// supplies the durable publication snapshot when the module scans `sys`
/// relations.
///
/// This is the single merge point where stored tuples become evaluator records:
/// the runtime merges the committed base with the live tail in stored form, and
/// the admitted schemas then receive evaluator form only.
fn evaluate_over_committed_tables(
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    authority: &ApplicationAuthority,
    admitted: &AdmittedApplication,
    tables: &[RuntimeTableIdentity],
    committed: PinnedCommittedTables,
    publication_rows: Option<RuntimePublicationMetadataRows>,
) -> Result<(RuntimeTableActivationSnapshot, StagedActivation), Diagnostic> {
    let PinnedCommittedTables {
        rows,
        storage_orders,
    } = committed;
    let snapshot = runtime
        .block_on(state.begin_admitted_table_activation_over_committed(tables, rows))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "committed table rows could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let evaluator_rows = evaluator_snapshot_rows(&snapshot, admitted, &storage_orders)?;
    let staged = authority
        .evaluate_staged_with_table_snapshot_rows(
            admitted,
            &Environment::new(),
            &snapshot,
            evaluator_rows,
            publication_rows,
        )
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2200",
                "pure project invocation failed",
                "fix the admitted function or retry the invocation",
                error.to_string(),
            )
        })?;
    Ok((snapshot, staged))
}

/// Converts every admitted table's merged stored rows in `snapshot` to
/// evaluator records, keyed by table name.
fn evaluator_snapshot_rows(
    snapshot: &RuntimeTableActivationSnapshot,
    admitted: &AdmittedApplication,
    storage_orders: &BTreeMap<String, Vec<String>>,
) -> Result<RuntimeTableRows, Diagnostic> {
    let mut rows = BTreeMap::new();
    for (name, table) in admitted.table_schemas() {
        let storage_order = storage_orders
            .get(name)
            .ok_or_else(committed_rows_read_error)?;
        let stored = snapshot
            .table_rows()
            .get(name)
            .map_or(&[][..], Vec::as_slice);
        let table_rows =
            orna_project::evaluator_rows_from_stored(table.schema(), storage_order, stored)
                .map_err(|_| committed_rows_read_error())?;
        rows.insert(name.clone(), table_rows);
    }
    Ok(rows)
}

/// Reads the committed rows of every admitted table at `commit` and pins them
/// to the runtime's published generation.
fn committed_invocation_rows(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    admitted: &AdmittedApplication,
    tables: &[RuntimeTableIdentity],
) -> Result<PinnedCommittedTables, Diagnostic> {
    let schemas = admitted_table_schemas(admitted);
    let (rows, storage_orders) =
        committed_rows_at(repository, commit, &schemas, tables).map_err(|error| match error {
            CommittedRowsError::Limit => Diagnostic::target(
                "E2200",
                "committed table rows exceed the read bound",
                "keep each table within the evaluator collection limit and the source byte limit",
            ),
            CommittedRowsError::Unavailable => committed_rows_read_error(),
        })?;
    pin_committed_tables(runtime, state, rows, storage_orders)
        .map_err(|_| committed_rows_read_error())
}

/// Pins committed rows read at a commit to the runtime's published generation.
fn pin_committed_tables(
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    rows: RuntimeTableRows,
    storage_orders: BTreeMap<String, Vec<String>>,
) -> Result<PinnedCommittedTables, RuntimeError> {
    let pin = runtime.block_on(state.published_pin())?;
    Ok(PinnedCommittedTables {
        rows: PinnedCommittedRows::new(pin, rows)?,
        storage_orders,
    })
}

fn committed_rows_read_error() -> Diagnostic {
    Diagnostic::target(
        "E2200",
        "committed table rows could not be read",
        "retry the invocation after checking local runtime state",
    )
}

/// Why the committed rows of the admitted tables could not be returned.
enum CommittedRowsError {
    /// The committed base could not be read or does not match the admitted schemas.
    Unavailable,
    /// The committed base holds more rows or bytes than the committed read bound allows.
    Limit,
}

/// The schema of every admitted table, keyed by table name.
fn admitted_table_schemas(
    admitted: &AdmittedApplication,
) -> BTreeMap<String, orna_semantic::TableSchema> {
    admitted
        .table_schemas()
        .iter()
        .map(|(name, table)| (name.clone(), table.schema().clone()))
        .collect()
}

/// The stored tuples of every admitted table at `commit`, read from the
/// committed format-3 base, with the stored field order each table lists.
/// Shared by the one-shot invocation and `serve`. The read stops at the first
/// row past `orna_application::committed_read_limits`.
fn committed_rows_at(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    schemas: &BTreeMap<String, orna_semantic::TableSchema>,
    tables: &[RuntimeTableIdentity],
) -> Result<(RuntimeTableRows, BTreeMap<String, Vec<String>>), CommittedRowsError> {
    let format = repository
        .open_pinned_format_context(commit.as_str())
        .map_err(|_| CommittedRowsError::Unavailable)?;
    let table_ids: BTreeMap<String, TableObjectId> = tables
        .iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect();
    let mut committed = orna_project::committed_tables(
        repository,
        &format,
        commit,
        schemas,
        &table_ids,
        orna_application::ApplicationAuthority::committed_read_limits(),
    )
    .map_err(|error| match error {
        orna_project::CommittedReadError::Limit { .. } => CommittedRowsError::Limit,
        orna_project::CommittedReadError::Storage(_) => CommittedRowsError::Unavailable,
    })?;
    let mut rows = BTreeMap::new();
    let mut storage_orders = BTreeMap::new();
    for name in schemas.keys() {
        let table = committed
            .remove(name)
            .ok_or(CommittedRowsError::Unavailable)?;
        rows.insert(name.clone(), table.rows);
        storage_orders.insert(name.clone(), table.storage_order);
    }
    Ok((rows, storage_orders))
}

/// One-shot `run` of a single admitted module that declares tables and reads
/// them. Tables bind to their committed identities at `HEAD`, so reads see the
/// same committed rows as the REPL. The sys host context and publication
/// snapshot follow the pure path: a module that needs publication metadata runs
/// as an embedded host, and any other module runs with the run host. `run` never
/// commits the activation's table mutations.
fn run_committed_table_invocation(
    endpoint: &Endpoint,
    authority: ApplicationAuthority,
    admitted: &AdmittedApplication,
    logical_path: &str,
    source: &str,
    std_snapshot: StdSnapshotPin,
) -> Result<(), Diagnostic> {
    let repository = discover_committed_repository(endpoint)?;
    let commit = committed_head(&repository)?;
    let tables = committed_tables(&repository, &commit, admitted)?;
    let admitted = admit_committed_metadata(&authority, admitted, logical_path, source, &tables)?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let authority = invocation_host(authority, &repository, identity, &admitted, std_snapshot);
    let (runtime, state, publication_rows) =
        open_committed_runtime(&repository, identity, initial_digest, &admitted, &commit)?;
    let committed =
        committed_invocation_rows(&repository, &commit, &runtime, &state, &admitted, &tables)?;
    let (_snapshot, activation) = evaluate_over_committed_tables(
        &runtime,
        &state,
        &authority,
        &admitted,
        &tables,
        committed,
        publication_rows,
    )?;
    if !activation.mutations().is_empty() {
        return Err(Diagnostic::unavailable(
            "one-shot invocation cannot commit table mutations",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        ));
    }
    Ok(())
}

/// The repository a committed-table invocation reads.
fn discover_committed_repository(
    endpoint: &Endpoint,
) -> Result<orna_repository::Repository, Diagnostic> {
    orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "committed table identities could not be read",
            "run the command inside an initialized local Orna repository",
        )
    })
}

/// The commit whose table identities bind the invocation.
fn committed_head(
    repository: &orna_repository::Repository,
) -> Result<orna_repository::GitCommitRef, Diagnostic> {
    repository.head().ok().flatten().ok_or_else(|| {
        Diagnostic::target(
            "E2200",
            "table reads need a committed project",
            "commit the project, then run the command again",
        )
    })
}

/// The committed identities of the admitted tables.
fn committed_tables(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    admitted: &AdmittedApplication,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let names = admitted.table_schemas().keys().cloned().collect::<Vec<_>>();
    committed_table_identities(repository, commit, names)
}

/// Re-admits the module with the committed table metadata its reads bind.
fn admit_committed_metadata(
    authority: &ApplicationAuthority,
    admitted: &AdmittedApplication,
    logical_path: &str,
    source: &str,
    tables: &[RuntimeTableIdentity],
) -> Result<AdmittedApplication, Diagnostic> {
    let entry = admitted.entry().to_owned();
    let committed = tables
        .iter()
        .map(|table| CommittedTableIdentity::new(table.table(), table.object_id()));
    authority
        .admit_module_with_committed_table_metadata(logical_path, source, entry, committed)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2101",
                "project semantic analysis failed",
                "fix the first reported source contract error, then run check again",
                error.to_string(),
            )
        })
}

/// The authority with the host context a committed-table read runs under.
fn invocation_host(
    authority: ApplicationAuthority,
    repository: &orna_repository::Repository,
    identity: RuntimeIdentity,
    admitted: &AdmittedApplication,
    std_snapshot: StdSnapshotPin,
) -> ApplicationAuthority {
    let mode = if admitted.requires_publication_metadata() {
        RuntimeMode::Embedded
    } else {
        RuntimeMode::Run
    };
    authority.with_sys_host_context(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        mode,
        std_snapshot,
    ))
}

/// The runtime and state a committed-table read needs, with the publication
/// metadata rows it must see before any row is read.
fn open_committed_runtime(
    repository: &orna_repository::Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    admitted: &AdmittedApplication,
    commit: &orna_repository::GitCommitRef,
) -> Result<
    (
        tokio::runtime::Runtime,
        RuntimeState,
        Option<RuntimePublicationMetadataRows>,
    ),
    Diagnostic,
> {
    let unavailable = || {
        Diagnostic::target(
            "E2200",
            "committed table rows could not be read",
            "retry the invocation after checking local runtime state",
        )
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| unavailable())?;
    let state = runtime
        .block_on(RuntimeState::open(repository, identity, initial_digest))
        .map_err(|_| unavailable())?;
    let table_names = admitted
        .table_schemas()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let candidates = legacy_bare_module_candidates(repository, Some(commit), &table_names)
        .map_err(|_| unavailable())?;
    if let Some(table) =
        legacy_bare_module_table(&state, &runtime, &candidates).map_err(|_| unavailable())?
    {
        return Err(legacy_module_table_diagnostic(&table));
    }
    let publication_rows = if admitted.requires_publication_metadata() {
        Some(
            runtime
                .block_on(state.publication_metadata_rows())
                .map_err(|_| {
                    Diagnostic::target(
                        "E2200",
                        "local runtime publication metadata could not be read",
                        "retry the invocation after checking local runtime state",
                    )
                })?,
        )
    } else {
        None
    };
    Ok((runtime, state, publication_rows))
}

fn repl_transactions(
    repository: &orna_repository::Repository,
    workspace_project: &orna_project::LoadedProject,
) -> Option<ReplTransactions> {
    let session_id = cli_request_nonce().to_be_bytes();
    let (commit, format_context) = repository
        .open_pinned_format_context_with_commit("HEAD")
        .ok()?;
    let format_context = format_context.with_runtime_owner_id(session_id);
    if !format_context.supports_writes() || format_context.final_format_capability().is_err() {
        return None;
    }
    let project = orna_project::ProjectLoader::default()
        .load_committed_snapshot(repository, &commit)
        .ok()?;
    if !same_repl_project_inputs(workspace_project, &project) {
        return None;
    }
    let catalogue = semantic_catalogue(&project).ok()?;
    let analysis = orna_semantic::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return None;
    }
    let (table_names, table_schemas) = admitted_project_tables(&analysis)?;
    if table_names.is_empty() {
        return None;
    }
    let tables = committed_table_identities(repository, &commit, table_names).ok()?;
    let identities =
        orna_project::ObjectIdentityManifest::read_committed(repository, &commit).ok()?;
    let table_object_ids: Arc<BTreeMap<String, TableObjectId>> = Arc::new(
        tables
            .iter()
            .map(|table| (table.table().to_owned(), table.object_id()))
            .collect(),
    );
    let capture_relation_id = tables.first()?.object_id().bytes();
    let (identity, initial_digest) = runtime_identity(repository).ok()?;
    if format_context.require_database_id().ok()?.as_bytes() != &identity.database_id {
        return None;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    Some(ReplTransactions {
        project,
        commit,
        format_context,
        authority: ApplicationAuthority::new(catalogue, Limits::default()).with_sys_host_context(
            sys_host::sys_host_context(
                repository.worktree(),
                identity,
                RuntimeMode::Repl,
                sys_host::std_snapshot_pin(workspace_project),
            ),
        ),
        runtime,
        identity,
        initial_digest,
        tables,
        identities,
        table_object_ids,
        table_schemas,
        capture_relation_id,
        session_id,
        host_bindings: Rc::new(RefCell::new(None)),
        faults: Arc::new(NoFault),
        writer: None,
    })
}

/// Collects the admitted tables of an analyzed project: every table whose
/// schema carries an admission, under its qualified name, in module order.
/// Returns `None` when two admitted tables share a qualified name.
fn admitted_project_tables(
    analysis: &orna_semantic::Analysis,
) -> Option<(Vec<String>, BTreeMap<String, orna_semantic::TableSchema>)> {
    let mut table_names = Vec::new();
    let mut table_schemas = BTreeMap::new();
    for (namespace, module) in &analysis.modules {
        for (name, symbol) in &module.symbols {
            let Some(schema) = symbol.table_schema.as_ref() else {
                continue;
            };
            if symbol.kind == orna_semantic::SymbolKind::Table && schema.admission.is_some() {
                let table = match namespace.display().as_str() {
                    "" => name.clone(),
                    prefix => format!("{prefix}.{name}"),
                };
                table_schemas.insert(table.clone(), schema.clone());
                table_names.push(table);
            }
        }
    }
    if table_names.iter().collect::<BTreeSet<_>>().len() != table_names.len() {
        return None;
    }
    Some((table_names, table_schemas))
}

fn same_repl_project_inputs(
    workspace: &orna_project::LoadedProject,
    committed: &orna_project::LoadedProject,
) -> bool {
    workspace.modules() == committed.modules()
        && workspace.identities() == committed.identities()
        && workspace.loose_rows() == committed.loose_rows()
        && workspace.standard_profile() == committed.standard_profile()
        && workspace.standard_sources() == committed.standard_sources()
        && workspace.standard_runtime_modules() == committed.standard_runtime_modules()
        && workspace.has_standard_imports() == committed.has_standard_imports()
        && workspace.standard_modules() == committed.standard_modules()
}

/// The activation facts a project REPL session reports through
/// `sys.rt.info()`. `None` leaves the session unattached, so the evaluator
/// reports `sys.version.incompatible` rather than inventing host facts.
fn repl_sys_host_context(
    repository: &orna_repository::Repository,
    project: &orna_project::LoadedProject,
) -> Option<SysHostContext> {
    let (identity, _) = runtime_identity(repository).ok()?;
    Some(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Repl,
        sys_host::std_snapshot_pin(project),
    ))
}

fn repl_runtime(
    repository: Option<orna_repository::Repository>,
) -> Result<ReplRuntime, Diagnostic> {
    let (session, transactions) = if let Some(repository) = &repository {
        let project = orna_project::ProjectLoader::default()
            .load(repository)
            .map_err(project_load_diagnostic)?;
        let session = AdmittedReplSession::from_loaded_project(
            &project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .map_err(|error| repl_session_error(&error))?;
        let session = match repl_sys_host_context(repository, &project) {
            Some(context) => session.with_sys_host_context(context),
            None => session,
        };
        let transactions = repl_transactions(repository, &project);
        (session, transactions)
    } else {
        // Preserve the core-only REPL when the current directory has no Git
        // repository or the endpoint is not local.
        (AdmittedReplSession::new(Limits::default()), None)
    };
    Ok(ReplRuntime {
        repository,
        session,
        transactions,
    })
}

fn repl_session(endpoint: &Endpoint) -> Result<ReplRuntime, Diagnostic> {
    let repository = match endpoint {
        // The legacy implicit local REPL remains core-only when no worktree is
        // present. Keep the successful discovery result as session authority.
        Endpoint::ManagedLocal => orna_repository::Repository::discover(".").ok(),
        Endpoint::Path(path) => {
            Some(orna_repository::Repository::discover(path).map_err(|_| {
                Diagnostic::target(
                    "E2100",
                    "project Git worktree could not be discovered",
                    "run the command inside a Git worktree or provide a local project path",
                )
            })?)
        }
        // Snapshot selectors are deliberately repository-backed. Keep a
        // non-local REPL core-only so the loader can reject `:at` uniformly as
        // ORNA-REPL-AT instead of failing before the command loop starts.
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => None,
    };
    repl_runtime(repository)
}

fn repl_session_error(error: &ReplError) -> Diagnostic {
    if matches!(error.code(), "ORNA-REPL-STANDARD" | "ORNA-S010-IMPORT") {
        Diagnostic::target(
            "ORNA-S010-IMPORT",
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    } else if error.code().starts_with("ORNA-S") || error.code() == "ORNA-REPL-SEMANTIC" {
        Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error before starting a project REPL session",
        )
    } else {
        Diagnostic::target_with_detail(
            "E2200",
            "project REPL session is not available",
            "use a project supported by the configured REPL runtime",
            error.code().to_owned(),
        )
    }
}

/// Read-only bridge from REPL `:at` selectors to project admission.
///
/// The REPL owns replacement semantics: this loader only returns an owned,
/// fully admitted candidate once the selected source set has loaded. In
/// particular, it never checks out a commit or changes Git's index, worktree,
/// HEAD, or refs.
struct ReplSnapshotSessionLoader<'a> {
    repository: Option<&'a orna_repository::Repository>,
    host_bindings: Rc<RefCell<Option<SysHostBindingRegistry>>>,
}

impl ReplSnapshotSessionLoader<'_> {
    fn repository(&self) -> Result<&orna_repository::Repository, ()> {
        self.repository.ok_or(())
    }

    fn admit(&self, project: &orna_project::LoadedProject) -> Result<AdmittedReplSession, ()> {
        // Admit the project's own pinned standard sources; uncaptured imports
        // remain inadmissible because the project has no standard profile.
        let session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .map_err(|_| ())?;
        let context = self
            .repository
            .and_then(|repository| repl_sys_host_context(repository, project));
        Ok(match context {
            Some(context) => session.with_sys_host_context(context),
            None => session,
        })
    }
}

impl repl::SnapshotSessionLoader for ReplSnapshotSessionLoader<'_> {
    type Error = ();

    fn load_snapshot(
        &self,
        target: repl::SnapshotTarget,
    ) -> Result<AdmittedReplSession, Self::Error> {
        let session = match target {
            repl::SnapshotTarget::Cwd => {
                let project = orna_project::ProjectLoader::default()
                    .load(self.repository()?)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
            repl::SnapshotTarget::Head => {
                let repository = self.repository()?;
                let commit = repository.head().map_err(|_| ())?.ok_or(())?;
                let project = orna_project::ProjectLoader::default()
                    .load_committed_snapshot(repository, &commit)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
            repl::SnapshotTarget::Ref(reference) => {
                let repository = self.repository()?;
                let commit = repository.resolve_snapshot(&reference).map_err(|_| ())?;
                let project = orna_project::ProjectLoader::default()
                    .load_committed_snapshot(repository, &commit)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
        };
        self.host_bindings.borrow_mut().take();
        Ok(session)
    }
}

/// A located `ORNA-REPL-EFFECT` failure for a project binding the REPL cannot
/// honour, naming what is wrong and the remedy to apply.
fn repl_effect_failure(message: &str, remedy: &str) -> repl::ReplFailure {
    repl::ReplFailure::located("ORNA-REPL-EFFECT", message, vec![remedy.to_owned()])
}

/// Locates a REPL refusal to take the repository writer lease (ORNA-TURSO-005).
/// `LeaseHeld` from a process that could not take the runtime owner lock means a
/// live writer, such as a mounted editor, holds it; otherwise another owner is
/// bound to this lock. Any other lease failure keeps the bare code.
fn repl_lease_failure(error: &RuntimeError, state: &RuntimeState) -> repl::ReplFailure {
    if *error != RuntimeError::LeaseHeld {
        return repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned());
    }
    if state.holds_runtime_owner_lock() {
        // This process holds the runtime owner lock, so no mount is implicated
        // on this path and no remedy is offered for it.
        return repl::ReplFailure::located(
            "ORNA-REPL-EFFECT",
            "another writer owns the repository writer lease for this runtime owner lock",
            Vec::new(),
        );
    }
    repl_effect_failure(
        "the repository writer lease is held by a live writer: the runtime owner lock is held by another live process (such as a mounted editor), so this REPL cannot take the writer lease",
        "REPL database writes need the mount stopped: run `orna unmount DIR`, then rerun this REPL. File writes while it is mounted go through `orna edit`",
    )
}

/// Binds one REPL request row to its session, query, source text and the
/// project digest it was evaluated against, so a replayed identity cannot
/// adopt a different request's effects.
fn repl_request_fingerprint(
    session_id: [u8; 16],
    query_id: [u8; 16],
    source: &str,
    initial_digest: [u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"orna.repl.request.v1");
    hasher.update(session_id);
    hasher.update(query_id);
    hasher.update(initial_digest);
    hasher.update(
        u64::try_from(source.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(source.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
thread_local! {
    /// Running request rows counted when the first sys binding dispatches.
    static FIRST_SYS_DISPATCH_RUNNING: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// The runtime state and writer lease one REPL session owns once its first
/// transaction runs. Both are opened once, so every later input settles under
/// the same owner fence instead of re-taking the lease.
struct ReplWriter {
    state: RuntimeState,
    lease: WriterLease,
}

/// The transaction fields `ensure_writer` reads. They are passed separately so
/// the caller can keep borrowing them after the writer is opened.
struct ReplWriterInputs<'a> {
    repository: &'a orna_repository::Repository,
    commit: &'a orna_repository::GitCommitRef,
    format_context: &'a orna_repository::RepositoryFormatContext,
    runtime: &'a tokio::runtime::Runtime,
    tables: &'a [RuntimeTableIdentity],
    capture_relation_id: [u8; 16],
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    session_id: [u8; 16],
    host_bindings: &'a Rc<RefCell<Option<SysHostBindingRegistry>>>,
}

/// Opens the runtime state and takes the writer lease once, and initialises the
/// session's sys host bindings. Every failure keeps the session usable for a
/// later input, exactly as the previous per-input retry did.
fn ensure_writer<'a>(
    writer: &'a mut Option<ReplWriter>,
    inputs: &ReplWriterInputs<'_>,
) -> Result<&'a mut ReplWriter, repl::ReplFailure> {
    if writer.is_none() {
        let state = inputs
            .runtime
            .block_on(RuntimeState::open_with_owner_id(
                inputs.repository,
                inputs.identity,
                inputs.initial_digest,
                inputs.session_id,
            ))
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        let tables = inputs
            .tables
            .iter()
            .map(RuntimeTableIdentity::table)
            .collect::<Vec<_>>();
        let candidates =
            legacy_bare_module_candidates(inputs.repository, Some(inputs.commit), &tables)
                .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        if let Some(table) = legacy_bare_module_table(&state, inputs.runtime, &candidates)
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?
        {
            return Err(repl_effect_failure(
                &format!(
                    "module table `{table}` has rows stored under its bare name by an earlier build"
                ),
                "re-create the store from the committed project; rows under bare module names are not migrated",
            ));
        }
        ensure_host_bindings(inputs, &state)?;
        let lease = inputs
            .runtime
            .block_on(state.acquire_lease(invocation_owner_id()))
            .map_err(|error| repl_lease_failure(&error, &state))?;
        *writer = Some(ReplWriter { state, lease });
    }
    let opened = writer
        .as_mut()
        .ok_or_else(|| repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned()))?;
    ensure_host_bindings(inputs, &opened.state)?;
    Ok(opened)
}

/// Initialises the session's sys host bindings from the opened runtime state.
/// A `:at` snapshot replacement clears them, so they are rebuilt on demand.
fn ensure_host_bindings(
    inputs: &ReplWriterInputs<'_>,
    state: &RuntimeState,
) -> Result<(), repl::ReplFailure> {
    let mut host_bindings = inputs.host_bindings.borrow_mut();
    if host_bindings.is_some() {
        return Ok(());
    }
    let mut filesystem =
        FilesystemProvider::with_limits(65_536, 16).map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    filesystem
        .allow_root(inputs.repository.worktree())
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    if state.holds_runtime_owner_lock() {
        let capability = inputs
            .format_context
            .capture_capability(inputs.capture_relation_id)
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        bindings = bindings.with_repository_capture_capability(capability);
    }
    *host_bindings = Some(bindings);
    Ok(())
}

/// One REPL request the runtime has durably reserved.
///
/// The request lifecycle is a typestate: a [`Reserved`] row that cannot start
/// is cancelled, a [`Running`] row is completed, cancelled, or left Running for
/// fenced recovery, and every settlement consumes the value. A request can
/// therefore be settled at most once, and the external-dispatch flag that
/// decides between cancellation and recovery travels with the Running row.
struct ReplRequest<'a> {
    runtime: &'a tokio::runtime::Runtime,
    state: &'a RuntimeState,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    query_id: [u8; 16],
    writer: WriterLease,
}

/// A REQUEST-1 row that is durably reserved but not yet started.
struct Reserved<'a>(ReplRequest<'a>);

/// A REQUEST-1 row whose admission capability was consumed by a start.
struct Running<'a> {
    request: ReplRequest<'a>,
    /// Whether evaluation dispatched through an external sys binding. Such a
    /// request is left Running for fenced recovery and never cancelled.
    external_dispatched: bool,
}

impl ReplRequest<'_> {
    /// The request's terminal outcome, tagged with the operation settling it.
    fn terminal(&self, prefix: &'static [u8]) -> Option<TerminalOutcome> {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&self.query_id);
        TerminalOutcome::new(bytes).ok()
    }

    fn finish_observation(
        &self,
        status: orna_runtime::InvocationObservationStatus,
    ) -> Result<(), ()> {
        self.runtime
            .block_on(
                self.state
                    .finish_query_observation(self.writer, self.query_id, status),
            )
            .map(|_| ())
            .map_err(|_| ())
    }

    /// Cancels the row and reports `original`. A row whose input never reached
    /// an external binding is cancelled before its observation is recorded, so
    /// a failed observation write cannot strand it. A failed cancellation
    /// leaves the row unsettled, which is uncertain.
    fn abandon(self, original: repl::ReplFailure) -> repl::ReplFailure {
        let cancelled = self
            .terminal(b"orna.repl.cancel.v1")
            .is_some_and(|outcome| {
                self.runtime
                    .block_on(self.state.cancel_request_with_owner(
                        self.identity,
                        self.fingerprint,
                        self.writer,
                        outcome,
                    ))
                    .is_ok()
            });
        if self
            .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
            .is_err()
        {
            eprintln!("warning: query observation not recorded");
        }
        if cancelled {
            original
        } else {
            repl_effect_uncertain(settlement_cause(&original))
        }
    }
}

impl<'a> Reserved<'a> {
    /// Reserves the durable REQUEST-1 row and returns it with the admission
    /// capability its start consumes. A row this call did not insert is
    /// settled with a failed observation before the caller reports the code.
    fn reserve(
        runtime: &'a tokio::runtime::Runtime,
        state: &'a RuntimeState,
        writer: WriterLease,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        query_id: [u8; 16],
    ) -> Result<(Self, RequestAdmissionCapability), repl::ReplFailure> {
        let request = ReplRequest {
            runtime,
            state,
            identity,
            fingerprint,
            query_id,
            writer,
        };
        if let Ok((_, Some(admission))) =
            runtime.block_on(state.reserve_request_with_admission(identity, fingerprint))
        {
            Ok((Self(request), admission))
        } else {
            request
                .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
                .map_err(|()| repl::ReplFailure::from(REPL_EFFECT_UNCERTAIN.to_owned()))?;
            Err("ORNA-REPL-EFFECT".to_owned().into())
        }
    }

    /// Starts the reserved row. A failed start leaves it Reserved and no
    /// binding has run, so it is cancelled before the failure is reported.
    fn start(
        self,
        admission: RequestAdmissionCapability,
    ) -> Result<Running<'a>, repl::ReplFailure> {
        let started = self
            .0
            .runtime
            .block_on(self.0.state.start_request_with_owner_and_admission(
                self.0.identity,
                self.0.fingerprint,
                self.0.writer,
                admission,
            ));
        if started.is_err() {
            return Err(self.0.abandon("ORNA-REPL-EFFECT".to_owned().into()));
        }
        Ok(Running {
            request: self.0,
            external_dispatched: false,
        })
    }
}

impl Running<'_> {
    fn record_external_dispatch(&mut self, dispatched: bool) {
        self.external_dispatched |= dispatched;
    }

    fn identity(&self) -> RequestIdentity {
        self.request.identity
    }

    fn fingerprint(&self) -> [u8; 32] {
        self.request.fingerprint
    }

    fn writer(&self) -> WriterLease {
        self.request.writer
    }

    /// The request's terminal outcome, tagged with the settling operation.
    fn terminal(&self, prefix: &'static [u8]) -> Option<TerminalOutcome> {
        self.request.terminal(prefix)
    }

    /// Leaves a started request Running for fenced recovery after evaluation
    /// failed with an external effect possible. The input is never replayed.
    fn leave_running(self, failure: repl::ReplFailure) -> repl::ReplFailure {
        if self
            .request
            .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
            .is_err()
        {
            return REPL_EFFECT_UNCERTAIN.to_owned().into();
        }
        failure
    }

    /// Completes a read-only input with its terminal outcome. Read-only
    /// results are retained, so the request never stays Running.
    fn settle_read(self) -> Result<(), repl::ReplFailure> {
        let completed = self
            .request
            .terminal(b"orna.repl.read.v1")
            .is_some_and(|outcome| {
                self.request
                    .runtime
                    .block_on(self.request.state.complete_request_with_owner(
                        self.request.identity,
                        self.request.fingerprint,
                        self.request.writer,
                        outcome,
                    ))
                    .is_ok()
            });
        if !completed {
            return Err(self.unsettled());
        }
        self.record_committed();
        Ok(())
    }

    /// Failure after evaluation: uncertain when an external binding was
    /// dispatched (the row stays Running for fenced recovery); otherwise
    /// cancelled.
    fn unsettled(self) -> repl::ReplFailure {
        if self.external_dispatched {
            let _ = self
                .request
                .finish_observation(orna_runtime::InvocationObservationStatus::Failed);
            REPL_EFFECT_UNCERTAIN.to_owned().into()
        } else {
            self.request.abandon("ORNA-REPL-EFFECT".to_owned().into())
        }
    }

    /// The same decision as [`Self::unsettled`], naming `cause` in the detail.
    fn unsettled_because(self, cause: String) -> repl::ReplFailure {
        if self.external_dispatched {
            let _ = self
                .request
                .finish_observation(orna_runtime::InvocationObservationStatus::Failed);
            repl_effect_uncertain(cause)
        } else {
            self.request.abandon(repl_effect_cancelled(cause))
        }
    }

    /// A failed table commit after evaluation. A failed observation write
    /// makes the outcome uncertain instead of a proven failure.
    fn commit_failed(self, cause: String) -> repl::ReplFailure {
        if self
            .request
            .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
            .is_err()
        {
            return REPL_EFFECT_UNCERTAIN.to_owned().into();
        }
        repl_effect_uncertain(cause)
    }

    fn abandon(self, original: repl::ReplFailure) -> repl::ReplFailure {
        self.request.abandon(original)
    }

    /// The request is committed, so an observation write failure is only a
    /// warning and never reports the committed input as uncertain.
    fn record_committed(self) {
        if self
            .request
            .finish_observation(orna_runtime::InvocationObservationStatus::Succeeded)
            .is_err()
        {
            eprintln!("warning: committed; query observation not recorded");
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run_repl_input(
    repository: Option<&orna_repository::Repository>,
    transactions: Option<&mut ReplTransactions>,
    session: &mut AdmittedReplSession,
    source: &str,
) -> Result<Option<CanonicalValue>, repl::ReplFailure> {
    let Ok(staged) = session.stage_activation(source) else {
        return session.submit(source).map_err(repl::ReplFailure::from);
    };
    let (Some(repository), Some(transactions)) = (repository, transactions) else {
        return Err(repl::ReplFailure::from(
            orna_project_repl::effect_rejection(staged.effects()),
        ));
    };
    let (current_commit, _) = repository
        .open_pinned_format_context_with_commit("HEAD")
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    if current_commit != transactions.commit {
        return Err(repl_effect_failure(
            "the project changed since this REPL session started",
            "restart the REPL to bind to the current HEAD",
        ));
    }
    if !transactions.format_context.supports_writes() {
        return Err(repl_effect_failure(
            "this repository's format does not accept REPL writes",
            "open a repository whose format supports writes, then restart the REPL",
        ));
    }
    let writer = ensure_writer(
        &mut transactions.writer,
        &ReplWriterInputs {
            repository,
            commit: &transactions.commit,
            format_context: &transactions.format_context,
            runtime: &transactions.runtime,
            tables: &transactions.tables,
            capture_relation_id: transactions.capture_relation_id,
            identity: transactions.identity,
            initial_digest: transactions.initial_digest,
            session_id: transactions.session_id,
            host_bindings: &transactions.host_bindings,
        },
    )?;
    let state = &writer.state;
    let lease = writer.lease;
    let mut host_bindings = transactions.host_bindings.borrow_mut();
    let bindings = host_bindings
        .as_mut()
        .ok_or_else(|| repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned()))?;
    // The runtime doc for `begin_admitted_table_activation_over_committed`
    // requires the pin to be read before the rows it binds.
    let pin = transactions
        .runtime
        .block_on(state.published_pin())
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let (committed_rows, storage_orders) = committed_rows_at(
        repository,
        &transactions.commit,
        &transactions.table_schemas,
        &transactions.tables,
    )
    .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let committed =
        PinnedCommittedRows::new(pin, committed_rows).map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let snapshot = transactions
        .runtime
        .block_on(
            state.begin_admitted_table_activation_over_committed(&transactions.tables, committed),
        )
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let query_id = cli_request_nonce().to_be_bytes();
    let expression =
        (source.len() <= 16 * 1024 && !source.is_empty() && !source.as_bytes().contains(&0))
            .then(|| source.to_owned());
    let request = RequestIdentity {
        session_id: transactions.session_id,
        request_id: query_id,
    };
    #[cfg(test)]
    let observe_first_dispatch = || {
        FIRST_SYS_DISPATCH_RUNNING.with(|first| {
            if first.get().is_none() {
                let running = transactions
                    .runtime
                    .block_on(state.running_requests())
                    .expect("running requests at first sys dispatch");
                first.set(Some(running.len()));
            }
        });
    };
    let scope = ReplTransactionScope::new(
        &transactions.project,
        repository,
        &transactions.format_context,
        &snapshot,
        &storage_orders,
        request,
    );
    #[cfg(test)]
    let scope = scope.with_dispatch_observer(&observe_first_dispatch);
    transactions
        .runtime
        .block_on(state.begin_query_observation(
            lease,
            QueryObservationRegistration {
                id: query_id,
                expression,
                capture: snapshot.context().capture().clone(),
            },
        ))
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    // REQUEST-1 (Orna-1.1.0 30-protocol.md:104): the request row is reserved
    // and started before evaluation, so an external sys effect dispatched while
    // the input evaluates is covered by a durable Running row. A panic between
    // reservation and settlement leaves that row Running, which is the same
    // state as a crash and is settled by fenced recovery.
    let fingerprint = repl_request_fingerprint(
        transactions.session_id,
        query_id,
        source,
        transactions.initial_digest,
    );
    let (reserved, admission) = Reserved::reserve(
        &transactions.runtime,
        state,
        lease,
        request,
        fingerprint,
        query_id,
    )?;
    let mut running = reserved.start(admission)?;
    let evaluation = transactions
        .authority
        .evaluate_staged_repl_transaction(session, staged, &scope, bindings);
    let (value, successor, activation, external_dispatched) = match evaluation {
        Ok(result) => result,
        Err(failure) => {
            if failure.external_effects_possible {
                // The row stays Running: fenced recovery settles it and the
                // input is never replayed.
                return Err(running.leave_running(repl_evaluation_failure(failure)));
            }
            return Err(running.abandon(repl_evaluation_failure(failure)));
        }
    };
    running.record_external_dispatch(external_dispatched);
    if activation.mutations().is_empty() {
        // Read-only results are retained: the request completes with its
        // terminal outcome instead of being left running.
        running.settle_read()?;
        *session = successor;
        return Ok(value);
    }
    let Ok(activation) =
        activation.stage_for_request(&transactions.authority, snapshot.context(), request)
    else {
        return Err(running.unsettled());
    };
    // The application stages with NoFault; rebuild the identical unit carrying
    // the runtime's injector so the request boundary owns the fault seam.
    let Ok(request_staged) = StagedTableActivation::from_source(
        activation.context().clone(),
        activation.mutations().to_vec(),
        activation.next_digest(),
        Arc::clone(&transactions.faults),
        activation.effects(),
    ) else {
        return Err(running.unsettled());
    };
    let Some(outcome) = running.terminal(b"orna.repl.commit.v1") else {
        return Err(running.unsettled());
    };
    // ORNA-ASSERT-042: a project that declares assertions checks the
    // post-mutation candidate against them before any row, checkpoint or
    // terminal is published. The request-scoped activation publishes the
    // request identity, fingerprint and terminal outcome in the same step as
    // the rows. A project without assertions has nothing to check, so its
    // commit keeps the unvalidated activation path.
    if project_declares_assertions(&transactions.project) {
        let cwd_generation = u64::try_from(request_staged.context().capture().generation())
            .ok()
            .and_then(|runtime| {
                repository
                    .cwd_generation(orna_repository::RuntimeGeneration::new(runtime))
                    .ok()
            });
        let Some(cwd_generation) = cwd_generation else {
            return Err(running.unsettled());
        };
        let mut validator = match transactional_loaded_project_candidate_validator(
            &transactions.project,
            &transactions.identities,
            Some(cwd_generation),
            Arc::clone(&transactions.table_object_ids),
            Limits::default(),
        ) {
            Ok(validator) => validator,
            Err(outcome) => {
                return Err(running.unsettled_because(stage_outcome_cause(&outcome)));
            }
        };
        let committed =
            transactions
                .runtime
                .block_on(state.commit_validated_table_request_activation(
                    ValidatedTableRequestActivationCommit {
                        writer: running.writer(),
                        identity: running.identity(),
                        fingerprint: running.fingerprint(),
                        context: request_staged.context(),
                        mutations: request_staged.mutations(),
                        next_digest: request_staged.next_digest(),
                        outcome,
                        validator: &mut *validator,
                        faults: request_staged.faults(),
                        effects: request_staged.effects(),
                    },
                ));
        match committed {
            Ok(_) => {}
            // Validation runs before any publication, so a failed validation
            // leaves the input uncommitted. It is cancelled unless an external
            // binding was actually dispatched during evaluation. Only a false
            // assertion is ORNA-A091-009; other validator failures keep their
            // diagnostic in the detail.
            Err(TableActivationError::ValidationFailed(diagnostic))
                if !running.external_dispatched =>
            {
                let failure = if orna_conformance::is_table_assertion_false(diagnostic) {
                    repl::ReplFailure::from("ORNA-A091-009".to_owned())
                } else {
                    repl_effect_cancelled(format!(
                        "table assertion validation did not complete: {diagnostic:?}"
                    ))
                };
                return Err(running.abandon(failure));
            }
            Err(error) => {
                return Err(running.commit_failed(format!(
                    "table commit failed with {}",
                    table_activation_cause(&error)
                )));
            }
        }
    } else {
        let committed =
            transactions
                .runtime
                .block_on(state.commit_staged_table_request_activation(
                    running.writer(),
                    running.identity(),
                    running.fingerprint(),
                    &request_staged,
                    outcome,
                ));
        if let Err(error) = committed {
            return Err(running.commit_failed(format!("table commit failed with {error}")));
        }
    }
    running.record_committed();
    *session = successor;
    Ok(value)
}

/// Reported when external effects may already have happened after evaluation
/// and no terminal outcome is proven. The request row stays Running for fenced
/// recovery; the input is never replayed.
const REPL_EFFECT_UNCERTAIN: &str = "ORNA-REPL-EFFECT-UNCERTAIN";

fn repl_application_error(error: ApplicationError) -> String {
    match error {
        ApplicationError::Parse(codes) | ApplicationError::Semantic(codes) => codes
            .into_iter()
            .next()
            .unwrap_or_else(|| "ORNA-REPL-SEMANTIC".to_owned()),
        ApplicationError::Evaluation(code) | ApplicationError::EffectRejected(code) => code,
        ApplicationError::SourceEffectFailed(code) => code.as_str().to_owned(),
        ApplicationError::UnadmittedTable(_) => "ORNA-EVAL-TABLE-UNADMITTED".to_owned(),
        _ => "ORNA-REPL-EFFECT".to_owned(),
    }
}

/// Maps a failed REPL evaluation to its submission failure. When external
/// effects may already have happened, the code stays
/// `ORNA-REPL-EFFECT-UNCERTAIN` and the evaluator cause moves to the detail,
/// so the uncertain outcome stays diagnosable without a replay.
fn repl_evaluation_failure(failure: ReplEvaluationFailure) -> repl::ReplFailure {
    let cause = repl_application_error(failure.error);
    if !failure.external_effects_possible {
        return repl::ReplFailure::from(cause);
    }
    repl_effect_uncertain(format!(
        "external effects may already have happened after evaluation failed with {cause}"
    ))
}

/// Submission failure for an input whose external effects may already have
/// happened. The code stays `ORNA-REPL-EFFECT-UNCERTAIN`; `message` names the
/// cause that stopped settlement, so the outcome is diagnosable without replay.
fn repl_effect_uncertain(message: String) -> repl::ReplFailure {
    repl::ReplFailure::located(
        REPL_EFFECT_UNCERTAIN,
        message,
        vec![
            "the input is not replayed; the request row stays running for fenced recovery"
                .to_owned(),
        ],
    )
}

/// Submission failure for an input that was not published and is cancelled.
/// `message` names the cause so the failure stays diagnosable.
fn repl_effect_cancelled(message: String) -> repl::ReplFailure {
    repl::ReplFailure::located("ORNA-REPL-EFFECT", message, Vec::new())
}

/// The failure that stopped settlement, named for the uncertain detail.
fn settlement_cause(failure: &repl::ReplFailure) -> String {
    match &failure.detail {
        Some(detail) => format!("{}: {}", failure.code, detail.message),
        None => failure.code.clone(),
    }
}

/// The commit error's inner cause, for the uncertain detail.
fn table_activation_cause(error: &TableActivationError) -> String {
    match error {
        TableActivationError::Runtime(runtime) => runtime.to_string(),
        TableActivationError::ValidationFailed(diagnostic) => {
            if orna_conformance::is_table_assertion_false(*diagnostic) {
                "ORNA-A091-009".to_owned()
            } else {
                format!("{diagnostic:?}")
            }
        }
    }
}

/// The validator-build outcome's cause, for the unsettled detail.
fn stage_outcome_cause(outcome: &StageOutcome<orna_foundation::Diagnostic>) -> String {
    match outcome {
        StageOutcome::Failed(diagnostic) | StageOutcome::Cancelled(diagnostic) => {
            format!("{}: {}", diagnostic.code(), diagnostic.message())
        }
        StageOutcome::Skipped { reason } => reason.clone(),
        StageOutcome::Passed => "validator build produced no validator".to_owned(),
    }
}

fn run_repl_submission_with<W, S>(
    session: &mut AdmittedReplSession,
    source: &str,
    writer: &mut W,
    color_enabled: bool,
    submitter: &mut S,
) -> Result<(), Diagnostic>
where
    W: io::Write,
    S: FnMut(&mut AdmittedReplSession, &str) -> Result<Option<CanonicalValue>, repl::ReplFailure>,
{
    if source.trim() == ":quit" {
        return Ok(());
    }
    match submitter(session, source) {
        Ok(Some(value)) => {
            let rendered = repl::inspect(&value);
            let (result, ty) = rendered
                .rsplit_once(" : ")
                .unwrap_or((rendered.as_str(), ""));
            write_styled(writer, AnsiColor::Green, result.as_bytes(), color_enabled)
                .and_then(|()| {
                    if ty.is_empty() {
                        Ok(())
                    } else {
                        writer.write_all(b" : ")?;
                        write_styled(writer, AnsiColor::Dim, ty.as_bytes(), color_enabled)
                    }
                })
                .and_then(|()| writer.write_all(b"\n"))
                .map_err(|_| {
                    Diagnostic::target(
                        "E2200",
                        "REPL console I/O failed",
                        "check the terminal input and output streams, then start a new session",
                    )
                })?;
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(failure) => {
            let body = failure
                .detail
                .is_some()
                .then(|| repl::render_repl_failure(&failure, source));
            let mut diagnostic = diagnostic_documentation(&failure.code).map_or_else(
                || {
                    Diagnostic::unavailable(
                        "REPL expression could not be evaluated",
                        "revise the expression using names and values available in this REPL session",
                    )
                },
                |documentation| {
                    Diagnostic::target(
                        documentation.code,
                        documentation.title,
                        documentation.help,
                    )
                },
            );
            diagnostic.body = body;
            Err(diagnostic)
        }
    }
}
fn run_repl(
    endpoint: &Endpoint,
    expression: Option<&str>,
    color_mode: ColorMode,
) -> Result<(), Diagnostic> {
    if matches!(endpoint, Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_)) {
        return Err(Diagnostic::target(
            "E2100",
            "remote REPL sessions are unavailable",
            "use a local Git worktree until the Orna transport is available",
        ));
    }
    let mut runtime = repl_session(endpoint)?;
    let color_enabled = color_mode.stdout_enabled();
    let repository = runtime.repository.as_ref();
    let host_bindings = runtime.transactions.as_ref().map_or_else(
        || Rc::new(RefCell::new(None)),
        |transactions| transactions.host_bindings.clone(),
    );
    let mut transactions = runtime.transactions.as_mut();
    let mut submitter = |session: &mut AdmittedReplSession, source: &str| {
        run_repl_input(repository, transactions.as_deref_mut(), session, source)
    };
    if let Some(source) = expression {
        return run_repl_submission_with(
            &mut runtime.session,
            source,
            &mut io::stdout().lock(),
            color_enabled,
            &mut submitter,
        );
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let loader = ReplSnapshotSessionLoader {
        repository: runtime.repository.as_ref(),
        host_bindings,
    };
    // A watch observation names the runtime it observes, so the host is built
    // only when the session has a repository identity to pin it to.
    let watch_identity = runtime
        .repository
        .as_ref()
        .and_then(|repository| runtime_identity(repository).ok())
        .map(|(identity, _)| (identity.database_id, identity.repository_id));
    let mut watch = repl::ReplWatchHost::new(watch_identity, orna_protocol::Limits::default());
    let mut watch_state = repl::WatchCommandState::default();
    repl::run_with_loader_and_submitter(
        &mut BufReader::new(stdin.lock()),
        &mut stdout.lock(),
        &mut runtime.session,
        &loader,
        &mut submitter,
        (&mut watch, &mut watch_state),
        color_enabled,
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "REPL console I/O failed",
            "check the terminal input and output streams, then start a new session",
        )
    })
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

fn repository_init_diagnostic(error: &orna_repository::RepositoryInitError) -> Diagnostic {
    let code = error.code();
    let (title, help) = match error {
        orna_repository::RepositoryInitError::GitUnavailable => (
            "Git is not available",
            "install or enable Git, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::GitOperationFailed => (
            "Git could not initialize the repository",
            "check that Git can initialize the target directory, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::LocalStateUnavailable => (
            "local repository state is unavailable",
            "check local filesystem access for the target directory, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::UnsafeMetadataPath => (
            "repository metadata has an unsafe path",
            "replace unsafe metadata links or non-regular entries before retrying `orna init`",
        ),
        orna_repository::RepositoryInitError::MetadataIncomplete => (
            "repository metadata is incomplete",
            "restore complete metadata from a valid snapshot or initialize a new repository",
        ),
        orna_repository::RepositoryInitError::MetadataMalformed => (
            "repository metadata is invalid",
            "restore valid metadata from a compatible repository snapshot before retrying",
        ),
        orna_repository::RepositoryInitError::MetadataUnsupported => (
            "repository metadata format is unsupported",
            "use an Orna version that supports this repository format",
        ),
        orna_repository::RepositoryInitError::MetadataChanged => (
            "repository metadata changed during initialization",
            "ensure no other process changes repository metadata, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::RepositoryBusy => (
            "repository initialization is already in progress",
            "wait for the other initialization to finish, then retry `orna init`",
        ),
        orna_repository::RepositoryInitError::PlatformUnsupported => (
            "repository initialization is unsupported on this platform",
            "use a supported platform to initialize this repository",
        ),
    };
    Diagnostic::target(code, title, help)
}

fn initialize_repository(
    target: Option<&std::path::Path>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let target = target.unwrap_or_else(|| std::path::Path::new("."));
    orna_repository::initialize_repository(target)
        .map_err(|error| repository_init_diagnostic(&error))?;
    write_success_line("initialized Orna repository", color_enabled)
}
/// Maps a fetch failure to its documented exit status: an unavailable remote is a
/// connection failure (3) and a malformed advertisement is a protocol failure (7);
/// every other fetch refusal keeps the target status.
fn fetch_failure(error: &orna_repository::FetchError) -> Diagnostic {
    let exit = match error {
        orna_repository::FetchError::RemoteUnavailable => Exit::Connection,
        orna_repository::FetchError::MalformedAdvertisement => Exit::Protocol,
        _ => Exit::Target,
    };
    Diagnostic {
        code: "E2100",
        title: Cow::Borrowed("Git fetch failed"),
        help: "check the configured remote and retry `fetch`",
        exit,
        detail: Some(error.to_string()),
        body: None,
    }
}

fn run_fetch(
    endpoint: &Endpoint,
    remote: &str,
    branch: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository = discover_worktree(
        endpoint,
        "run `fetch` inside a Git worktree or provide a local project path",
    )?;
    let requested = orna_repository::RequestedRef::branch(branch.to_owned()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "fetch branch is invalid",
            "supply a valid Git branch name",
            error.to_string(),
        )
    })?;
    let report = repository
        .fetch_with_local_continuity(remote.to_owned(), [requested])
        .map_err(|error| fetch_failure(&error))?;
    if let Some(continuity) = report.continuity()
        && let Some(diagnostic) = fetch_continuity_diagnostic(continuity)
    {
        return Err(diagnostic);
    }
    let fetched = report.ordinary().first().ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "Git fetch returned no requested ref",
            "check the configured remote and retry `fetch`",
        )
    })?;
    let (state, state_color) = if fetched.updated() {
        ("updated", AnsiColor::Yellow)
    } else {
        ("unchanged", AnsiColor::Dim)
    };
    let mut stdout = io::stdout().lock();
    write_styled(&mut stdout, AnsiColor::Green, b"fetched ", color_enabled)
        .and_then(|()| {
            write_styled_display(
                &mut stdout,
                AnsiColor::Cyan,
                fetched.source(),
                color_enabled,
            )
        })
        .and_then(|()| stdout.write_all(b" -> "))
        .and_then(|()| {
            write_styled_display(
                &mut stdout,
                AnsiColor::Cyan,
                fetched.destination(),
                color_enabled,
            )
        })
        .and_then(|()| stdout.write_all(b" ("))
        .and_then(|()| write_styled_display(&mut stdout, state_color, state, color_enabled))
        .and_then(|()| stdout.write_all(b")\n"))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "could not write command output",
                "check that stdout is available, then retry the command",
            )
        })?;
    Ok(())
}

/// Publishes the checked-out branch together with the continuity refs an
/// ordinary push synchronizes (`ORNA-GIT-008`).
///
/// The branch defaults to the branch `HEAD` is attached to, so pushing from a
/// detached `HEAD` is refused rather than guessing a name. A remote that is
/// missing a selected continuity ref, or names it with a different object ID,
/// is reported as a conflict before anything is transferred.
/// A pending publication is recovered first; an unresolved journal blocks push.
fn run_push(
    endpoint: &Endpoint,
    remote: &str,
    branch: Option<&str>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository = discover_worktree(
        endpoint,
        "run `push` inside a Git worktree or provide a local project path",
    )?;
    let branch = match branch {
        Some(branch) => branch.to_owned(),
        None => repository
            .checked_out_branch()
            .map_err(|_| {
                Diagnostic::target(
                    "E2100",
                    "the checked-out branch could not be read",
                    "run `push REMOTE BRANCH` or check out a branch first",
                )
            })?
            .ok_or_else(|| {
                Diagnostic::target(
                    "E2100",
                    "`push` needs a branch when HEAD is detached",
                    "run `push REMOTE BRANCH` or check out a branch first",
                )
            })?,
    };
    if repository
        .read_publication_journal()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2200",
                "local publication state could not be read",
                "recover or repair the pending local publication before retrying `push`",
                error.to_string(),
            )
        })?
        .is_some()
    {
        if !repository.runtime_paths().state_db().exists() {
            return Err(Diagnostic::target(
                "E2200",
                "pending local publication has no runtime state",
                "restore the repository's runtime state and recover the publication before `push`",
            ));
        }
        let (identity, initial_digest) = runtime_identity(&repository)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                Diagnostic::target(
                    "E2200",
                    "local runtime publication recovery could not start",
                    "retry `push` after checking the local runtime state",
                )
            })?;
        let state = runtime
            .block_on(RuntimeState::open(&repository, identity, initial_digest))
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "local runtime publication state could not be opened",
                    "recover or repair the pending local publication before retrying `push`",
                    error.to_string(),
                )
            })?;
        cli_publish::recover_pending_publication(&runtime, &repository, &state)
            .map_err(push_publication_recovery_diagnostic)?;
    }
    repository
        .push_ordinary_with_local_continuity(remote, branch.clone())
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "Git push failed",
                "check the configured remote, its continuity refs, and retry `push`",
                error.to_string(),
            )
        })?;
    let mut stdout = io::stdout().lock();
    write_styled(&mut stdout, AnsiColor::Green, b"pushed ", color_enabled)
        .and_then(|()| write_styled_display(&mut stdout, AnsiColor::Cyan, remote, color_enabled))
        .and_then(|()| stdout.write_all(b" -> "))
        .and_then(|()| write_styled_display(&mut stdout, AnsiColor::Cyan, &branch, color_enabled))
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "could not write command output",
                "check that stdout is available, then retry the command",
            )
        })?;
    Ok(())
}

/// Renders a recovered-publication failure for `orna push`. The recovery
/// sequence is shared with `orna publish`; only the remedy text differs.
fn push_publication_recovery_diagnostic(
    failure: cli_publish::PublicationRecoveryFailure,
) -> Diagnostic {
    use cli_publish::PublicationRecoveryFailure;
    match failure {
        PublicationRecoveryFailure::Recover(error) => Diagnostic::target_with_detail(
            "E2200",
            "pending local publication could not be recovered",
            "resolve the publication recovery error before retrying `push`",
            error.to_string(),
        ),
        PublicationRecoveryFailure::Unreadable(error) => Diagnostic::target_with_detail(
            "E2200",
            "local publication state could not be verified",
            "verify the pending publication before retrying `push`",
            error.to_string(),
        ),
        PublicationRecoveryFailure::StillPending => Diagnostic::target(
            "E2200",
            "local publication recovery remains pending",
            "resolve the publication recovery before retrying `push`",
        ),
    }
}

fn fetch_continuity_diagnostic(
    continuity: orna_repository::RemoteContinuity,
) -> Option<Diagnostic> {
    if continuity.permits_continuity_claim() {
        return None;
    }
    Some(Diagnostic::target_with_detail(
        "E2100",
        "Orna continuity witness was rejected",
        "do not claim allocator or checkpoint continuity until required refs/orna witnesses match",
        format!("remote continuity state: {continuity:?}"),
    ))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod repl_request_effect_tests;
