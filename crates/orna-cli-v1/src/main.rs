#![allow(
    dead_code,
    reason = "The bounded binary exposes planning and session seams for a later integration adapter."
)]

mod cli_args;
mod cli_dispatch;
mod cli_help;
mod cli_history;
mod cli_query;
mod cli_row_key;
mod cli_export;
mod cli_import;
mod cli_mount;
mod cli_stage;
#[cfg(target_os = "linux")]
mod fuse_mount;
mod cli_annotate;
mod cli_publish;
mod cli_commit;
mod cli_status;
mod cli_serve;
mod media_range;
mod repl;
mod sys_host;
mod table_schema;
#[cfg(test)]
#[path = "../tests/support/format3.rs"]
mod format3;
#[cfg(test)]
#[path = "../tests/support/media_commit.rs"]
mod media_commit;

use cli_args::{
    Command, Invocation, Parsed, StatusFormat, parse_cli, requested_color_mode,
    requested_debug_mode,
};
use cli_dispatch::execute;
use repl::{AnsiColor, write_rendered_error, write_styled, write_styled_display};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufReader, IsTerminal, Write as _};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use orna_application_v1::{
    AdmittedApplication, ApplicationAuthority, ApplicationError, CommittedTableIdentity,
    ReplEvaluationFailure, ReplTransactionScope, StagedActivation,
};
use orna_conformance_v1::{
    AdmittedReplSession, BoundedEvaluator, DurableTransactionalEvaluator, ProjectEnvironment,
    ProjectExpectations, ProjectUnit, ReplError, RuntimeEvaluator, SourceUnit, StageOutcome,
    project_declares_assertions, transactional_loaded_project_candidate_validator,
};
use orna_evaluator_v1::{Environment, Limits, SysHostBindingRegistry};
use orna_foundation_v1::{CanonicalValue, OvbRaw, Value};
use orna_runtime_v1::{
    FaultInjector, NoFault, PinnedCommittedRows, QueryObservationRegistration, RequestIdentity,
    RuntimeError, RuntimeIdentity, RuntimePublicationMetadataRows, RuntimeState,
    RuntimeTableActivationSnapshot, RuntimeTableIdentity, RuntimeTableRows, StagedTableActivation,
    TableActivationError, TableObjectId, TerminalOutcome, ValidatedTableRequestActivationCommit,
    WriterLease,
};
use orna_sys_v1::{
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


const SENSOR_SOURCE_IDENTITY: &str = "example:sensors:v1";

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


fn write_cli_line(
    text: &[u8],
    color: AnsiColor,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let mut stdout = io::stdout().lock();
    write_styled(&mut stdout, color, text, color_enabled)
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|_| {
            Diagnostic {
                code: "E2200",
                title: Cow::Borrowed("CLI output could not be written"),
                help: "check the output stream, then retry the command",
                exit: Exit::Presentation,
                detail: None,
                body: None,
            }
        })
}
#[allow(
    dead_code,
    reason = "The bounded CLI records the complete specified exit-status space."
)]
#[derive(Clone, Debug, Eq, PartialEq)]
enum Exit {
    Success = 0,
    Target = 1,
    Usage = 2,
    Connection = 3,
    Authorisation = 4,
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
        title: &'static str,
        help: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code,
            title: Cow::Borrowed(title),
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
impl From<orna_repository_v1::RepositoryError> for Diagnostic {
    fn from(error: orna_repository_v1::RepositoryError) -> Self {
        match error {
            orna_repository_v1::RepositoryError::RepositoryBusy => Self::target(
                "E2100",
                "repository coordination lock is busy",
                "wait for the active repository operation to finish, then retry mv",
            ),
            orna_repository_v1::RepositoryError::LocalStateUnavailable => Self::target(
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

fn project_load_diagnostic(error: orna_project_v1::ProjectLoadError) -> Diagnostic {
    if matches!(error, orna_project_v1::ProjectLoadError::SourceTreeLimit) {
        return Diagnostic::target(
            "E2100",
            "committed source tree exceeds the fixed walk limit",
            "reduce the committed tree's entry count, nesting depth or path length, then run check again",
        );
    }
    let invalid_unit = match &error {
        orna_project_v1::ProjectLoadError::InvalidModule {
            logical_path,
            diagnostics,
        }
        | orna_project_v1::ProjectLoadError::InvalidRowUnit {
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
        let location = match &first.span.start_position {
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

fn diagnostic_documentation(code: &str) -> Option<DiagnosticDocumentation> {
    Some(match code {
        "ORNA-S010-IMPORT" => DiagnosticDocumentation {
            code: "ORNA-S010-IMPORT",
            title: "imported module is unavailable",
            explanation:
                "The project names a module that is not part of its captured dependencies, so the module cannot be checked or executed.",
            help: "use a captured standard dependency or remove the import",
        },
        "ORNA-S011-AMBIGUOUS" => DiagnosticDocumentation {
            code: "ORNA-S011-AMBIGUOUS",
            title: "standard import name is ambiguous",
            explanation:
                "Two standard imports bind the same unqualified name to different functions, so the name cannot be resolved.",
            help: "import the functions under distinct names or use their qualified paths",
        },
        "ORNA-S012-UNRESOLVED" => DiagnosticDocumentation {
            code: "ORNA-S012-UNRESOLVED",
            title: "name could not be resolved",
            explanation:
                "The source refers to a name that is not declared or imported in the current namespace.",
            help: "declare the name or add the matching `use` import before using it",
        },
        "ORNA-S021-TYPE" => DiagnosticDocumentation {
            code: "ORNA-S021-TYPE",
            title: "expression has the wrong type",
            explanation:
                "The value produced by an expression does not satisfy the type required at that source location.",
            help: "change the expression or its declared type so the value and requirement agree",
        },
        "ORNA-REPL-EFFECT" => DiagnosticDocumentation {
            code: "ORNA-REPL-EFFECT",
            title: "REPL preview cannot perform an effect",
            explanation:
                "Interactive previews are read-only and cannot perform network, filesystem, process, or other external effects.",
            help: "evaluate a pure expression or invoke the operation through its admitted runtime entry point",
        },
        "ORNA-REPL-EFFECT-UNCERTAIN" => DiagnosticDocumentation {
            code: "ORNA-REPL-EFFECT-UNCERTAIN",
            title: "REPL effect outcome is uncertain",
            explanation:
                "The input was evaluated before its request was durably committed, so external effects may already have happened. The outcome is uncertain and recovery is pending; the request is not replayed.",
            help: "check the external system for the effect before repeating the input, because repeating it may repeat an external effect",
        },
        "ORNA-REPL-AT" => DiagnosticDocumentation {
            code: "ORNA-REPL-AT",
            title: "snapshot selection failed",
            explanation:
                "The requested CWD, HEAD, or reference snapshot could not be loaded as an admitted REPL context.",
            help: "choose an existing snapshot and keep the repository available while selecting it",
        },
        "ORNA091-E-RETURN-ARROW" => DiagnosticDocumentation {
            code: "ORNA091-E-RETURN-ARROW",
            title: "function return type uses the arrow form",
            explanation:
                "Orna function declarations separate the parameter list from the return type with a colon.",
            help: "replace `->` with `:` in the function declaration",
        },
        "ORNA091-E-VAR" => DiagnosticDocumentation {
            code: "ORNA091-E-VAR",
            title: "local declaration uses `var`",
            explanation:
                "Orna uses `let` for local declarations; reassignment updates that binding when permitted.",
            help: "replace `var` with `let`",
        },
        "ORNA-A091-001" => DiagnosticDocumentation {
            code: "ORNA-A091-001",
            title: "refined type uses the legacy `where self` form",
            explanation:
                "Refined Orna types state their invariant in a brace-delimited assertion block owned by the type.",
            help: "replace `where self` with a brace-delimited `assert` block",
        },
        "ORNA-A091-002" => DiagnosticDocumentation {
            code: "ORNA-A091-002",
            title: "table assertion repeats its owner",
            explanation:
                "A table assertion is already evaluated for its owning table, so an explicit owner pipeline is redundant.",
            help: "remove the leading `self |` or table-name pipeline from the assertion",
        },
        "ORNA-A091-003" => DiagnosticDocumentation {
            code: "ORNA-A091-003",
            title: "module assertion belongs to one table",
            explanation:
                "An assertion that depends on only one table must live with that table so its owner is explicit.",
            help: "move the assertion into the table body it depends on",
        },
        "ORNA-A091-004" => DiagnosticDocumentation {
            code: "ORNA-A091-004",
            title: "assertion predicate has the wrong owner",
            explanation:
                "The declaration's predicate does not describe the type or table that owns the assertion.",
            help: "rewrite the predicate for the reported owner type",
        },
        "ORNA-A091-005" => DiagnosticDocumentation {
            code: "ORNA-A091-005",
            title: "assertion is missing its terminator",
            explanation: "An assertion clause must end with a semicolon before the next declaration.",
            help: "terminate the assertion clause with `;`",
        },
        "ORNA-A091-006" => DiagnosticDocumentation {
            code: "ORNA-A091-006",
            title: "assertion uses a dedicated recovery branch",
            explanation:
                "Assertions report failed propositions; they do not provide an assertion-specific `else` branch.",
            help: "use ordinary failure, recovery, or control flow instead",
        },
        "ORNA-A091-007" => DiagnosticDocumentation {
            code: "ORNA-A091-007",
            title: "assertion has a forbidden effect",
            explanation:
                "Declaration and refined assertions must be deterministic and cannot perform external effects.",
            help: "remove the identified effect or nondeterministic operation",
        },
        "ORNA-A091-008" => DiagnosticDocumentation {
            code: "ORNA-A091-008",
            title: "assertion is false",
            explanation:
                "The executable or refined assertion does not hold for the value being checked.",
            help: "inspect the owner and proposition, then provide a safe value",
        },
        "ORNA-A091-009" => DiagnosticDocumentation {
            code: "ORNA-A091-009",
            title: "table assertion is false",
            explanation:
                "A table or cross-table invariant does not hold for the deterministic rows being checked.",
            help: "inspect the reported witness and repair the affected table data",
        },
        "ORNA-A091-010" => DiagnosticDocumentation {
            code: "ORNA-A091-010",
            title: "assertion form is unsupported",
            explanation: "Orna 1.0 supports the sole owner-local `assert` form for declarations and tables.",
            help: "rewrite the construct using the supported `assert` form",
        },
        "ORNA-A091-011" => DiagnosticDocumentation {
            code: "ORNA-A091-011",
            title: "assertion is empty",
            explanation: "An assertion must contain a proposition that can be checked.",
            help: "provide a proposition after `assert`",
        },
        "ORNA-A091-012" => DiagnosticDocumentation {
            code: "ORNA-A091-012",
            title: "module assertion has no table dependency",
            explanation:
                "A module assertion is only valid when it depends on a table; otherwise it has no executable owner.",
            help: "place the check in executable or test code",
        },
        "ORNA091-E-MATCH" => DiagnosticDocumentation {
            code: "ORNA091-E-MATCH",
            title: "value branching uses the legacy `match` form",
            explanation:
                "Orna branches on values with `case` and colon-separated arms rather than `match` and arrows.",
            help: "replace `match` with `case` and use `:` for each arm",
        },
        "ORNA091-E-RESULT" => DiagnosticDocumentation {
            code: "ORNA091-E-RESULT",
            title: "source uses an explicit `Result` value",
            explanation:
                "Orna returns the successful type directly and propagates failures automatically.",
            help: "return the successful type and use `|?` for recovery",
        },
        "ORNA091-E-POSTFIX-QUESTION" => DiagnosticDocumentation {
            code: "ORNA091-E-POSTFIX-QUESTION",
            title: "operation uses postfix failure propagation",
            explanation:
                "Orna propagates operation failures automatically instead of using a postfix `?` operator.",
            help: "remove the postfix `?` and use `|?` only for recovery",
        },
        "ORNA091-E-CURRENCY" => DiagnosticDocumentation {
            code: "ORNA091-E-CURRENCY",
            title: "source uses a special currency declaration",
            explanation:
                "Currencies are nominal types with a nested `Currency` implementation, not a separate declaration form.",
            help: "replace the currency declaration with a nominal type and nested `impl Currency`",
        },
        "ORNA091-E-CURRENCY-SYMBOL" => DiagnosticDocumentation {
            code: "ORNA091-E-CURRENCY-SYMBOL",
            title: "currency declares a universal symbol",
            explanation:
                "Currency symbols and placement depend on locale formatter data rather than a universal static field.",
            help: "move symbol and placement choices to the locale-aware money formatter",
        },
        "ORNA091-E-IMPL-FOR" => DiagnosticDocumentation {
            code: "ORNA091-E-IMPL-FOR",
            title: "protocol implementation uses the legacy top-level form",
            explanation:
                "Orna protocol implementations are nested inside the type that owns them.",
            help: "nest `impl P` inside the owning type instead of writing `impl P for T`",
        },
        "ORNA091-E-BOUND-COLON" => DiagnosticDocumentation {
            code: "ORNA091-E-BOUND-COLON",
            title: "generic bound uses the legacy colon form",
            explanation:
                "Generic protocol bounds use the `impl` keyword in Orna type parameters.",
            help: "replace `<T: P>` with `<T impl P>`",
        },
        "ORNA091-E-STATIC-FN" => DiagnosticDocumentation {
            code: "ORNA091-E-STATIC-FN",
            title: "protocol declares a static function",
            explanation:
                "Protocols expose static properties or ordinary functions rather than static function declarations.",
            help: "use a static property or an ordinary function",
        },
        "ORNA091-E-TRYFROM" => DiagnosticDocumentation {
            code: "ORNA091-E-TRYFROM",
            title: "source uses `TryFrom`",
            explanation:
                "Orna's `From<S>` conversion may fail without wrapping the successful value in a separate result type.",
            help: "use `From<S>` for a conversion that may fail",
        },
        "ORNA091-E-CONVERSION-CHAIN" => DiagnosticDocumentation {
            code: "ORNA091-E-CONVERSION-CHAIN",
            title: "conversion chain is implicit",
            explanation:
                "Orna does not search for unspoken multi-step conversions between types.",
            help: "name each conversion step explicitly",
        },
        "ORNA091-E-OPAQUE" => DiagnosticDocumentation {
            code: "ORNA091-E-OPAQUE",
            title: "source uses an opaque type declaration",
            explanation:
                "Orna uses one nominal or refined `type` form instead of a separate opaque declaration.",
            help: "replace `opaque Name` with the unified nominal or refined `type` form",
        },
        "ORNA091-E-FIELD-CONSTRAINT" => DiagnosticDocumentation {
            code: "ORNA091-E-FIELD-CONSTRAINT",
            title: "field declares a legacy constraint",
            explanation:
                "Field uniqueness and checks are table assertions owned by the table, not field suffixes.",
            help: "write an owner-local table assertion using `all_unique` or `every`",
        },
        _ => return None,
    })
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


fn cancellation_diagnostic(diagnostic: &orna_foundation_v1::Diagnostic) -> Diagnostic {
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

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' | b'A'..=b'F' => Some(byte.to_ascii_lowercase() - b'a' + 10),
        _ => None,
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionState {
    Open,
    Closing,
    Closed,
}
trait SessionAdapter {
    fn is_terminal(&mut self, child: u64) -> Result<bool, Diagnostic>;
    fn cancel(&mut self, child: u64) -> Result<(), Diagnostic>;
    fn drain_terminal(&mut self) -> Result<(), Diagnostic>;
    fn close_transport(&mut self) -> Result<(), Diagnostic>;
}
struct Session {
    state: SessionState,
    owned: BTreeSet<u64>,
}
impl Session {
    fn new() -> Self {
        Self {
            state: SessionState::Open,
            owned: BTreeSet::new(),
        }
    }
    fn own(&mut self, child: u64) -> Result<(), Diagnostic> {
        if self.state != SessionState::Open {
            return Err(Diagnostic::target(
                "E1201",
                "cannot add an invocation to a closed session",
                "start a new root session before creating child actions",
            ));
        }
        self.owned.insert(child);
        Ok(())
    }
    fn close<A: SessionAdapter>(&mut self, adapter: &mut A) -> Result<bool, Diagnostic> {
        if self.state == SessionState::Closed {
            return Ok(false);
        }
        self.state = SessionState::Closing;
        let mut first_error = None;
        for child in self.owned.iter().copied() {
            let should_cancel = match adapter.is_terminal(child) {
                Ok(terminal) => !terminal,
                Err(error) => {
                    first_error.get_or_insert(error);
                    true
                }
            };
            if should_cancel && let Err(error) = adapter.cancel(child) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        adapter.drain_terminal()?;
        adapter.close_transport()?;
        self.owned.clear();
        self.state = SessionState::Closed;
        Ok(true)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
struct ReferenceState {
    initialized: bool,
    books: BTreeSet<&'static str>,
    loans: BTreeMap<&'static str, &'static str>,
    stock: BTreeMap<(&'static str, &'static str), i32>,
    readings: BTreeMap<(&'static str, u8), &'static str>,
    checkpoint: u8,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum Step {
    Init,
    Seed,
    Exercise,
    Sensors,
    Noop(&'static str),
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Plan {
    steps: Vec<Step>,
}
fn seeded(s: &ReferenceState) -> bool {
    s.books == BTreeSet::from(["book-1", "book-2"])
        && s.stock == BTreeMap::from([(("north", "pencil"), 12), (("south", "pencil"), 4)])
}
fn exercised(s: &ReferenceState) -> bool {
    s.loans == BTreeMap::from([("book-1", "reader-1")])
        && s.stock == BTreeMap::from([(("north", "pencil"), 9), (("south", "pencil"), 7)])
}
fn seed(s: &mut ReferenceState) -> Result<(), Diagnostic> {
    if !s.initialized {
        return Err(Diagnostic::target(
            "E2002",
            "reference database is not initialized",
            "initialize it before invoking the reference seed",
        ));
    }
    if !s.books.is_empty() || !s.stock.is_empty() {
        return Err(Diagnostic::target(
            "E2003",
            "reference seed would create duplicate keys",
            "use a fresh database or workflow planning after confirming the seed postcondition",
        ));
    }
    s.books.extend(["book-1", "book-2"]);
    s.stock.insert(("north", "pencil"), 12);
    s.stock.insert(("south", "pencil"), 4);
    Ok(())
}
fn exercise(s: &mut ReferenceState) -> Result<(), Diagnostic> {
    if !seeded(s) {
        return Err(Diagnostic::target(
            "E2004",
            "reference exercise requires the exact seeded state",
            "seed an empty initialized reference database before exercising it",
        ));
    }
    if !s.loans.is_empty() {
        return Err(Diagnostic::target(
            "E2005",
            "reference exercise would create a duplicate loan key",
            "use workflow planning after confirming the exercise postcondition",
        ));
    }
    s.stock.insert(("north", "pencil"), 9);
    s.stock.insert(("south", "pencil"), 7);
    s.loans.insert("book-1", "reader-1");
    Ok(())
}
fn sensors(s: &mut ReferenceState) -> Result<(), Diagnostic> {
    if !s.initialized {
        return Err(Diagnostic::target(
            "E2002",
            "reference database is not initialized",
            "initialize it before invoking sensor ingestion",
        ));
    }
    for (sensor, sequence, value) in [
        ("greenhouse", 0, "18.25"),
        ("greenhouse", 1, "18.50"),
        ("greenhouse", 2, "18.75"),
    ]
    .into_iter()
    .skip(usize::from(s.checkpoint))
    {
        if s.readings.insert((sensor, sequence), value).is_some() {
            return Err(Diagnostic::target(
                "E2009",
                "sensor ingestion would create a duplicate reading key",
                "restore the matching checkpoint or use a fresh reference database",
            ));
        }
        s.checkpoint = sequence + 1;
    }
    Ok(())
}
fn plan(s: &ReferenceState) -> Result<Plan, Diagnostic> {
    let mut p = s.clone();
    let mut steps = Vec::new();
    if p.initialized {
        steps.push(Step::Noop("already initialized"));
    } else {
        p.initialized = true;
        steps.push(Step::Init);
    }
    if seeded(&p) || exercised(&p) {
        steps.push(Step::Noop("seed postcondition holds"));
    } else {
        seed(&mut p)?;
        steps.push(Step::Seed);
    }
    if exercised(&p) {
        steps.push(Step::Noop("exercise postcondition holds"));
    } else {
        exercise(&mut p)?;
        steps.push(Step::Exercise);
    }
    if p.checkpoint == 3 && p.readings.len() == 3 {
        steps.push(Step::Noop("sensor consumer at exhaustion"));
    } else {
        sensors(&mut p)?;
        steps.push(Step::Sensors);
    }
    Ok(Plan { steps })
}
fn apply(s: &mut ReferenceState, p: &Plan) -> Result<(), Diagnostic> {
    let mut candidate = s.clone();
    for step in &p.steps {
        match step {
            Step::Init => {
                if candidate.initialized {
                    return Err(Diagnostic::target(
                        "E2001",
                        "reference database is already initialized",
                        "use workflow planning to make initialization idempotent",
                    ));
                }
                candidate.initialized = true;
            }
            Step::Seed => seed(&mut candidate)?,
            Step::Exercise => exercise(&mut candidate)?,
            Step::Sensors => sensors(&mut candidate)?,
            Step::Noop(_) => {}
        }
    }
    *s = candidate;
    Ok(())
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
    let repository = orna_repository_v1::Repository::discover(path).ok()?;
    let (identity, _) = runtime_identity(&repository).ok()?;
    Some(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Run,
        std_snapshot,
    ))
}

fn run_status(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository.worktree())
        .args(["-c", "color.status=false", "status", "--porcelain"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be read",
                "check that Git can read the local worktree, then retry `status --porcelain`",
            )
        })?;
    if !output.status.success() {
        return Err(Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status --porcelain`",
        ));
    }
    io::stdout()
        .write_all(&output.stdout)
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be written",
                "retry `status --porcelain`",
            )
        })?;
    Ok(())
}

fn run_status_json(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository.worktree())
        .args([
            "-c",
            "color.status=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be read",
                "check that Git can read the local worktree, then retry `status --format json`",
            )
        })?;
    if !output.status.success() {
        return Err(Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status --format json`",
        ));
    }
    let branch_output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository.worktree())
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree branch could not be read",
                "check that Git can read the local worktree, then retry `status --format json`",
            )
        })?;
    let branch =
        cli_status::branch_name(branch_output.status.success(), &branch_output.stdout);
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
    repository: &orna_repository_v1::Repository,
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
                return Err(orna_runtime_v1::RuntimeError::RecoveryInvalid);
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
                    .ok_or(orna_runtime_v1::RuntimeError::RecoveryInvalid),
                _ => Err(orna_runtime_v1::RuntimeError::RecoveryInvalid),
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

fn runtime_publication_metadata_rows(
    endpoint: &Endpoint,
    std_snapshot: StdSnapshotPin,
) -> Result<(RuntimePublicationMetadataRows, SysHostContext), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
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
                    (b'D', b'D')
                        | (b'A', b'A')
                        | (b'A', b'U')
                        | (b'U', b'A')
                        | (b'D', b'U')
                        | (b'U', b'D')
                )
            {
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
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let unpublished_count = unpublished_mutation_count(&repository)?;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository.worktree())
        .args(["-c", "color.status=false", "status"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be read",
                "check that Git can read the local worktree, then retry `status`",
            )
        })?;
    if !output.status.success() {
        return Err(Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status`",
        ));
    }
    let stdout = io::stdout();
    let link_root = if stdout.is_terminal() && terminal_supports_osc8() {
        repository.worktree().canonicalize().ok()
    } else {
        None
    };
    let mut stdout = stdout.lock();
    write_human_status(&output.stdout, &mut stdout, color_enabled, link_root.as_deref()).map_err(
        |_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be written",
                "retry `status`",
            )
        },
    )?;
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
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let mut git_dir = std::process::Command::new("git");
    let git_dir_output = git_dir
        .arg("-C")
        .arg(repository.worktree())
        .args(["rev-parse", "--git-dir"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be read",
                "check that Git can read the local worktree, then retry `status --short`",
            )
        })?;
    if !git_dir_output.status.success() {
        return Err(Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status --short`",
        ));
    }
    let git_dir = String::from_utf8(git_dir_output.stdout).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status --short`",
        )
    })?;
    let git_dir = std::path::Path::new(git_dir.trim());
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_owned()
    } else {
        repository.worktree().join(git_dir)
    };
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(&git_dir)
        .arg("--work-tree")
        .arg(repository.worktree())
        .args(["-c", "color.status=false", "status", "--short"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be read",
                "check that Git can read the local worktree, then retry `status --short`",
            )
        })?;
    if !output.status.success() {
        return Err(Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status --short`",
        ));
    }
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


fn load_project(endpoint: &Endpoint) -> Result<orna_project_v1::LoadedProject, Diagnostic> {
    let project = load_project_without_standard_rejection(endpoint)?;
    reject_uncaptured_standard_modules(&project)?;
    Ok(project)
}

fn load_project_without_standard_rejection(
    endpoint: &Endpoint,
) -> Result<orna_project_v1::LoadedProject, Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "project Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(project_load_diagnostic)?;
    Ok(project)
}

fn reject_uncaptured_standard_modules(
    project: &orna_project_v1::LoadedProject,
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
    project: &orna_project_v1::LoadedProject,
) -> Result<orna_semantic_v1::Catalogue, Diagnostic> {
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
    let analysis = orna_semantic_v1::analyze_with_providers(project.modules(), &catalogue, orna_semantic_v1::SystemProviders::Production);
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

fn execution_project(project: &orna_project_v1::LoadedProject) -> ProjectUnit {
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
            .filter(|(logical_path, _)| {
                project.standard_runtime_modules().contains(logical_path)
            })
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
fn pure_execution_project(project: &orna_project_v1::LoadedProject) -> ProjectUnit {
    let mut execution = execution_project(project);
    execution
        .modules
        .retain(|unit| !project.standard_runtime_modules().contains(&unit.source_id));
    execution
}

fn runtime_identity(
    repository: &orna_repository_v1::Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), Diagnostic> {
    orna_runtime_v1::repository_runtime_identity(repository).map_err(|error| match error {
        orna_runtime_v1::RuntimeError::UnknownHeadFormat(
            refusal @ orna_repository_v1::FormatContextError::MixedFormats { .. },
        ) => cli_commit::mixed_formats_at_head(&refusal),
        orna_runtime_v1::RuntimeError::UnknownHeadFormat(refusal) => Diagnostic::target(
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
    orna_runtime_v1::process_owner_id()
}

fn run_project_invocation(
    endpoint: &Endpoint,
    root_entry: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_invocation_with_arguments(
        endpoint,
        root_entry,
        &Environment::new(),
        color_enabled,
    )
}

fn public_project_function(analysis: &orna_semantic_v1::Analysis, target: &str) -> bool {
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
        .get(&orna_semantic_v1::Namespace(namespace))
        .and_then(|module| module.exports.get(function))
        .is_some_and(|symbol| symbol.kind == orna_semantic_v1::SymbolKind::Function)
}
fn run_public_project_function(
    endpoint: &Endpoint,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    run_project_function(endpoint, target, false, color_enabled)
}
fn run_default_project_main(
    endpoint: &Endpoint,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
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
    let analysis = orna_semantic_v1::analyze_with_providers(project.modules(), &catalogue, orna_semantic_v1::SystemProviders::Production);
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
            .get(&orna_semantic_v1::Namespace(Vec::new()))
            .and_then(|module| module.symbols.get("main"))
            .is_some_and(|symbol| symbol.kind == orna_semantic_v1::SymbolKind::Function);
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
    let analysis = orna_semantic_v1::analyze_with_providers(project.modules(), &catalogue, orna_semantic_v1::SystemProviders::Production);
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
    let repository = orna_repository_v1::Repository::discover(local_project_path(endpoint)?)
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "project runtime could not be discovered",
                "run the command inside an initialized local Git worktree",
            )
        })?;
    let project = load_project(endpoint)?;
    let catalogue = std::sync::Arc::new(semantic_catalogue(&project)?);
    let analysis = orna_semantic_v1::analyze_with_providers(project.modules(), &catalogue, orna_semantic_v1::SystemProviders::Production);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
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
    let project = execution_project(&project);
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
                    orna_conformance_v1::RuntimeTarget {
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
                ),
        )
        .map_err(|error| durable_invocation_failed(&error))?;
    match outcome {
        StageOutcome::Passed => write_success_line("invocation completed", color_enabled),
        StageOutcome::Failed(diagnostic) => Err(match published_runtime_failure(&diagnostic) {
            Some(mut published) => {
                published.detail = Some(
                    "the project transaction failed atomically; no partial changes were committed"
                        .into(),
                );
                published
            }
            None => {
                let mut rejection = Diagnostic::target(
                    "E2200",
                    "durable project invocation was rejected",
                    "the project transaction failed atomically; no partial changes were committed",
                );
                rejection.detail = Some(failure_detail(&diagnostic));
                rejection
            }
        }),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { reason }
            if arguments.is_empty()
                && reason
                    == "project transaction admission does not run stream roots; use the explicit finite-list stream seam" =>
        {
            run_project_stream_invocation(endpoint, root_entry, color_enabled)
        }
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
    catalogue: std::sync::Arc<orna_semantic_v1::Catalogue>,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let repository = orna_repository_v1::Repository::discover(local_project_path(endpoint)?)
        .map_err(|_| {
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
fn published_runtime_failure(diagnostic: &orna_foundation_v1::Diagnostic) -> Option<Diagnostic> {
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
fn runtime_failure_detail(error: &orna_runtime_v1::RuntimeError) -> String {
    if let orna_runtime_v1::RuntimeError::LegacyHeadReadOnly(refusal) = error {
        return format!("runtime failure: {} ({})", refusal.code(), refusal.title());
    }
    let cause = error
        .public_code()
        .map_or_else(|| error.to_string(), str::to_owned);
    format!("runtime failure: {cause}")
}

/// Renders the evaluator failure code with the codes of its nested causes.
/// Messages and spans stay redacted at the evaluator boundary.
fn failure_detail(diagnostic: &orna_foundation_v1::Diagnostic) -> String {
    let causes = diagnostic.causes();
    if causes.is_empty() {
        return format!("failure code: {}", diagnostic.code());
    }
    let codes = causes
        .iter()
        .map(orna_foundation_v1::Diagnostic::code)
        .collect::<Vec<_>>()
        .join(", ");
    format!("failure code: {} (causes: {codes})", diagnostic.code())
}

/// Maps a failed durable project invocation to `E2200`. The runtime's public
/// code, or its non-disclosing display text, is carried in the detail, which
/// renders only with `--debug` (ORNA-UX-004). A legacy head refusal names its
/// own remedy, because retrying the transaction cannot clear it.
fn durable_invocation_failed(error: &orna_runtime_v1::RuntimeError) -> Diagnostic {
    let help = match error {
        orna_runtime_v1::RuntimeError::LegacyHeadReadOnly(refusal) => refusal.remedy(),
        _ => "retry the project transaction after checking its durable state",
    };
    let mut diagnostic = Diagnostic::target("E2200", "durable project invocation failed", help);
    diagnostic.detail = Some(runtime_failure_detail(error));
    diagnostic
}

/// Maps a stream delivery the runtime rolled back to `E2200`, carrying the
/// evaluator failure code and its nested cause codes in the `--debug` detail.
fn stream_delivery_failed(diagnostic: &orna_foundation_v1::Diagnostic) -> Diagnostic {
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
fn multi_module_declares_tables(project: &orna_project_v1::LoadedProject) -> bool {
    project.modules().len() != 1
        && project.modules().iter().any(|module| {
            orna_syntax_v1::parse_module(&module.source)
                .value
                .items
                .iter()
                .any(|item| matches!(item.declaration, orna_syntax_v1::Declaration::Table { .. }))
        })
}

fn run_pure_invocation(
    endpoint: &Endpoint,
    target: &str,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    // Single-module projects that import pinned standard modules take the
    // authority route only when they need publication metadata. Every other
    // standard-importing project takes the bounded path below, which loads the
    // reachable standard bodies through the verified profile.
    let std_imports = !project.standard_runtime_modules().is_empty();
    if project.modules().len() == 1 {
        let identity = &project.identities()[0];
        let module = &project.modules()[0];
        let (module_name, function) = target.rsplit_once('.').unwrap_or(("", target));
        let namespace = if module_name.is_empty() {
            Vec::new()
        } else {
            module_name.split('.').map(str::to_owned).collect::<Vec<_>>()
        };
        if identity.namespace() == namespace {
            let authority = ApplicationAuthority::new(
                orna_semantic_v1::Catalogue::authoritative_core(),
                Limits::default(),
            )
            .with_loaded_standard(&project)
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
            if let Some(admitted) = admitted
                .filter(|candidate| !std_imports || candidate.requires_publication_metadata())
            {
                if !admitted.table_schemas().is_empty() && admitted.entry_references_tables() {
                    run_committed_table_invocation(
                        endpoint,
                        authority,
                        &admitted,
                        identity.logical_path(),
                        &module.source,
                        sys_host::std_snapshot_pin(&project),
                    )?;
                    write_success_line("invocation completed", color_enabled)?;
                    return Ok(());
                }
                if admitted.requires_publication_metadata() {
                    let (publication_rows, sys_host_context) = runtime_publication_metadata_rows(
                        endpoint,
                        sys_host::std_snapshot_pin(&project),
                    )?;
                    let authority = authority.with_sys_host_context(sys_host_context);
                    let activation = authority
                        .evaluate_staged_with_publication_rows(
                            &admitted,
                            &Environment::new(),
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
                    if !activation.mutations().is_empty() {
                        return Err(Diagnostic::unavailable(
                            "one-shot invocation cannot commit table mutations",
                            "use a supported table transaction; stream roots require the explicit stream runtime",
                        ));
                    }
                } else {
                    let std_snapshot = sys_host::std_snapshot_pin(&project);
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
                return Ok(());
            }
        }
    }
    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic_v1::analyze_with_providers(project.modules(), &catalogue, orna_semantic_v1::SystemProviders::Production);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
        ));
    }
    if multi_module_declares_tables(&project) {
        return Err(Diagnostic::target(
            "E2200",
            "table reads in multi-module projects are not supported by one-shot invocation; use the REPL or orna run",
            "one-shot invocation admits table declarations only in single-module projects",
        ));
    }

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
                        format!("standard dependency load rejected with {}", diagnostic.code()),
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
    match evaluator.evaluate_project(&pure_execution_project(&project)) {
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
        StageOutcome::Failed(diagnostic) => Err(published_runtime_failure(&diagnostic).unwrap_or_else(|| Diagnostic::unavailable(
            "requested invocation is not available",
            "define a reachable zero-argument pure function with the requested name",
        ))),
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
    repository: Option<orna_repository_v1::Repository>,
    session: AdmittedReplSession,
    transactions: Option<ReplTransactions>,
}

struct ReplTransactions {
    project: orna_project_v1::LoadedProject,
    commit: orna_repository_v1::GitCommitRef,
    format_context: orna_repository_v1::RepositoryFormatContext,
    authority: ApplicationAuthority,
    runtime: tokio::runtime::Runtime,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    tables: Vec<RuntimeTableIdentity>,
    identities: orna_project_v1::ObjectIdentityManifest,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    table_schemas: BTreeMap<String, orna_semantic_v1::TableSchema>,
    capture_relation_id: [u8; 16],
    session_id: [u8; 16],
    host_bindings: Rc<RefCell<Option<SysHostBindingRegistry>>>,
    faults: Arc<dyn FaultInjector>,
    lease: Option<WriterLease>,
    // Drop the runtime owner lock only after capture candidates release their refs.
    state: Option<RuntimeState>,
}

/// Reads the committed identity of each named admitted table from `commit`.
/// Each name must resolve to exactly one live table record; the error names the
/// table that does not, so the caller fails closed with a reason. The REPL and
/// one-shot `run` both bind tables through this function, so neither resolves a
/// table by name alone.
fn committed_table_identities(
    repository: &orna_repository_v1::Repository,
    commit: &orna_repository_v1::GitCommitRef,
    table_names: Vec<String>,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let identities =
        orna_project_v1::ObjectIdentityManifest::read_committed_if_present(repository, commit)
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
    let mut tables = Vec::with_capacity(table_names.len());
    for name in table_names {
        let mut matching = identities.records().iter().filter(|record| {
            record.kind() == orna_project_v1::ObjectKind::Table
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
) -> Result<Option<String>, orna_runtime_v1::RuntimeError> {
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
    repository: &orna_repository_v1::Repository,
    commit: Option<&orna_repository_v1::GitCommitRef>,
    declared: &[&str],
) -> Result<Vec<(String, String)>, orna_project_v1::ObjectIdentityError> {
    let mut tables = declared
        .iter()
        .map(|name| ((*name).to_owned(), false))
        .collect::<Vec<_>>();
    let manifest = match commit {
        Some(commit) => Some(orna_project_v1::ObjectIdentityManifest::read_committed(
            repository, commit,
        )?),
        None => None,
    };
    if let Some(manifest) = manifest {
        tables.extend(
            manifest
                .records()
                .iter()
                .filter(|record| record.kind() == orna_project_v1::ObjectKind::Table)
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
    repository: &orna_repository_v1::Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    project: &orna_project_v1::LoadedProject,
) -> Result<(), Diagnostic> {
    let mut declared = Vec::new();
    for (module, module_identity) in project.modules().iter().zip(project.identities()) {
        for item in orna_syntax_v1::parse_module(&module.source).value.items {
            if let orna_syntax_v1::Declaration::Table { name, .. } = item.declaration {
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
    let commit = match repository.head() {
        Ok(commit) => commit,
        Err(_) => {
            return Err(Diagnostic::target(
                "E2200",
                "committed HEAD is unreadable",
                "repair the repository HEAD reference, then run again",
            ));
        }
    };
    if let Some(commit) = &commit {
        // A commit without an identity inventory never allocated its tables'
        // identities. Evaluating its table statements would touch no committed
        // relation, so refuse with the remedy that allocates the identities.
        if orna_project_v1::ObjectIdentityManifest::read_committed_if_present(repository, commit)
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
            orna_project_v1::evaluator_rows_from_stored(table.schema(), storage_order, stored)
                .map_err(|_| committed_rows_read_error())?;
        rows.insert(name.clone(), table_rows);
    }
    Ok(rows)
}

/// Reads the committed rows of every admitted table at `commit` and pins them
/// to the runtime's published generation.
fn committed_invocation_rows(
    repository: &orna_repository_v1::Repository,
    commit: &orna_repository_v1::GitCommitRef,
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    admitted: &AdmittedApplication,
    tables: &[RuntimeTableIdentity],
) -> Result<PinnedCommittedTables, Diagnostic> {
    let schemas = admitted_table_schemas(admitted);
    let (rows, storage_orders) = committed_rows_at(repository, commit, &schemas, tables).map_err(
        |error| match error {
            CommittedRowsError::Limit => Diagnostic::target(
                "E2200",
                "committed table rows exceed the read bound",
                "keep each table within the evaluator collection limit and the source byte limit",
            ),
            CommittedRowsError::Unavailable => committed_rows_read_error(),
        },
    )?;
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
) -> BTreeMap<String, orna_semantic_v1::TableSchema> {
    admitted
        .table_schemas()
        .iter()
        .map(|(name, table)| (name.clone(), table.schema().clone()))
        .collect()
}

/// The stored tuples of every admitted table at `commit`, read from the
/// committed format-3 base, with the stored field order each table lists.
/// Shared by the one-shot invocation and `serve`. The read stops at the first
/// row past `orna_application_v1::committed_read_limits`.
fn committed_rows_at(
    repository: &orna_repository_v1::Repository,
    commit: &orna_repository_v1::GitCommitRef,
    schemas: &BTreeMap<String, orna_semantic_v1::TableSchema>,
    tables: &[RuntimeTableIdentity],
) -> Result<(RuntimeTableRows, BTreeMap<String, Vec<String>>), CommittedRowsError> {
    let format = repository
        .open_pinned_format_context(commit.as_str())
        .map_err(|_| CommittedRowsError::Unavailable)?;
    let table_ids: BTreeMap<String, TableObjectId> = tables
        .iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect();
    let mut committed = orna_project_v1::committed_tables(
        repository,
        &format,
        commit,
        &schemas,
        &table_ids,
        orna_application_v1::ApplicationAuthority::committed_read_limits(),
    )
    .map_err(|error| match error {
        orna_project_v1::CommittedReadError::Limit { .. } => CommittedRowsError::Limit,
        orna_project_v1::CommittedReadError::Storage(_) => CommittedRowsError::Unavailable,
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
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "committed table identities could not be read",
            "run the command inside an initialized local Orna repository",
        )
    })?;
    let commit = repository.head().ok().flatten().ok_or_else(|| {
        Diagnostic::target(
            "E2200",
            "table reads need a committed project",
            "commit the project, then run the command again",
        )
    })?;
    let names = admitted.table_schemas().keys().cloned().collect::<Vec<_>>();
    let tables = committed_table_identities(&repository, &commit, names)?;
    let entry = admitted.entry().to_owned();
    let committed = tables
        .iter()
        .map(|table| CommittedTableIdentity::new(table.table(), table.object_id()));
    let admitted = authority
        .admit_module_with_committed_table_metadata(logical_path, source, entry, committed)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2101",
                "project semantic analysis failed",
                "fix the first reported source contract error, then run check again",
                error.to_string(),
            )
        })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let mode = if admitted.requires_publication_metadata() {
        RuntimeMode::Embedded
    } else {
        RuntimeMode::Run
    };
    let authority = authority.with_sys_host_context(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        mode,
        std_snapshot,
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "committed table rows could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let state = runtime
        .block_on(RuntimeState::open(&repository, identity, initial_digest))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "committed table rows could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let table_names = admitted
        .table_schemas()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let candidates = legacy_bare_module_candidates(&repository, Some(&commit), &table_names)
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "committed table rows could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    if let Some(table) = legacy_bare_module_table(&state, &runtime, &candidates).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "committed table rows could not be read",
            "retry the invocation after checking local runtime state",
        )
    })? {
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

fn repl_transactions(
    repository: &orna_repository_v1::Repository,
    workspace_project: &orna_project_v1::LoadedProject,
) -> Option<ReplTransactions> {
    let session_id = cli_request_nonce().to_be_bytes();
    let (commit, format_context) = repository
        .open_pinned_format_context_with_commit("HEAD")
        .ok()?;
    let format_context = format_context.with_runtime_owner_id(session_id);
    if !format_context.supports_writes() || format_context.final_format_capability().is_err() {
        return None;
    }
    let project = orna_project_v1::ProjectLoader::default()
        .load_committed_snapshot(repository, &commit)
        .ok()?;
    if !same_repl_project_inputs(workspace_project, &project) {
        return None;
    }
    let catalogue = semantic_catalogue(&project).ok()?;
    let analysis = orna_semantic_v1::analyze_with_providers(project.modules(), &catalogue, orna_semantic_v1::SystemProviders::Production);
    if !analysis.is_ok() {
        return None;
    }
    let mut table_names = Vec::new();
    let mut table_schemas = BTreeMap::new();
    for (namespace, module) in &analysis.modules {
        for (name, symbol) in &module.symbols {
            let Some(schema) = symbol.table_schema.as_ref() else {
                continue;
            };
            if symbol.kind == orna_semantic_v1::SymbolKind::Table && schema.admission.is_some() {
                let table = match namespace.display().as_str() {
                    "" => name.clone(),
                    prefix => format!("{prefix}.{name}"),
                };
                table_schemas.insert(table.clone(), schema.clone());
                table_names.push(table);
            }
        }
    }
    if table_names.is_empty()
        || table_names.iter().collect::<BTreeSet<_>>().len() != table_names.len()
    {
        return None;
    }
    let tables = committed_table_identities(repository, &commit, table_names).ok()?;
    let identities = orna_project_v1::ObjectIdentityManifest::read_committed(repository, &commit)
        .ok()?;
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
        state: None,
        lease: None,
        host_bindings: Rc::new(RefCell::new(None)),
        faults: Arc::new(NoFault),
    })
}

fn same_repl_project_inputs(
    workspace: &orna_project_v1::LoadedProject,
    committed: &orna_project_v1::LoadedProject,
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
    repository: &orna_repository_v1::Repository,
    project: &orna_project_v1::LoadedProject,
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
    repository: Option<orna_repository_v1::Repository>,
) -> Result<ReplRuntime, Diagnostic> {
    let (session, transactions) = if let Some(repository) = &repository {
        let project = orna_project_v1::ProjectLoader::default()
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
        Endpoint::ManagedLocal => orna_repository_v1::Repository::discover(".").ok(),
        Endpoint::Path(path) => Some(
            orna_repository_v1::Repository::discover(path).map_err(|_| {
                Diagnostic::target(
                    "E2100",
                    "project Git worktree could not be discovered",
                    "run the command inside a Git worktree or provide a local project path",
                )
            })?,
        ),
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
    repository: Option<&'a orna_repository_v1::Repository>,
    host_bindings: Rc<RefCell<Option<SysHostBindingRegistry>>>,
}

impl ReplSnapshotSessionLoader<'_> {
    fn repository(&self) -> Result<&orna_repository_v1::Repository, ()> {
        self.repository.ok_or(())
    }

    fn admit(&self, project: &orna_project_v1::LoadedProject) -> Result<AdmittedReplSession, ()> {
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
                let project = orna_project_v1::ProjectLoader::default()
                    .load(self.repository()?)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
            repl::SnapshotTarget::Head => {
                let repository = self.repository()?;
                let commit = repository.head().map_err(|_| ())?.ok_or(())?;
                let project = orna_project_v1::ProjectLoader::default()
                    .load_committed_snapshot(repository, &commit)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
            repl::SnapshotTarget::Ref(reference) => {
                let repository = self.repository()?;
                let commit = repository.resolve_snapshot(&reference).map_err(|_| ())?;
                let project = orna_project_v1::ProjectLoader::default()
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
    repl::ReplFailure {
        code: "ORNA-REPL-EFFECT".to_owned(),
        detail: Some(orna_evaluator_v1::ReplDetail {
            code: "ORNA-REPL-EFFECT".to_owned(),
            message: message.to_owned(),
            span: None,
            notes: Vec::new(),
            help: vec![remedy.to_owned()],
        }),
    }
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
        return repl::ReplFailure {
            code: "ORNA-REPL-EFFECT".to_owned(),
            detail: Some(orna_evaluator_v1::ReplDetail {
                code: "ORNA-REPL-EFFECT".to_owned(),
                message:
                    "another writer owns the repository writer lease for this runtime owner lock"
                        .to_owned(),
                span: None,
                notes: Vec::new(),
                help: Vec::new(),
            }),
        };
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
    hasher.update(u64::try_from(source.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(source.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
thread_local! {
    /// Running request rows counted when the first sys binding dispatches.
    static FIRST_SYS_DISPATCH_RUNNING: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

fn run_repl_input(
    repository: Option<&orna_repository_v1::Repository>,
    transactions: Option<&mut ReplTransactions>,
    session: &mut AdmittedReplSession,
    source: &str,
) -> Result<Option<CanonicalValue>, repl::ReplFailure> {
    let staged = match session.stage_activation(source) {
        Ok(staged) => staged,
        Err(_) => return session.submit(source).map_err(repl::ReplFailure::from),
    };
    let (Some(repository), Some(transactions)) = (repository, transactions) else {
        return Err(repl::ReplFailure::from(
            orna_evaluator_v1::effect_rejection(staged.effects()),
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
    if transactions.state.is_none() {
        let state = transactions
            .runtime
            .block_on(RuntimeState::open_with_owner_id(
                repository,
                transactions.identity,
                transactions.initial_digest,
                transactions.session_id,
            ))
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        let tables = transactions
            .tables
            .iter()
            .map(RuntimeTableIdentity::table)
            .collect::<Vec<_>>();
        let candidates =
            legacy_bare_module_candidates(repository, Some(&transactions.commit), &tables)
                .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        if let Some(table) = legacy_bare_module_table(&state, &transactions.runtime, &candidates)
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?
        {
            return Err(repl_effect_failure(
                &format!("module table `{table}` has rows stored under its bare name by an earlier build"),
                "re-create the store from the committed project; rows under bare module names are not migrated",
            ));
        }
        transactions.state = Some(state);
    }
    let mut host_bindings = transactions.host_bindings.borrow_mut();
    if host_bindings.is_none() {
        let mut filesystem = FilesystemProvider::with_limits(65_536, 16)
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        filesystem
            .allow_root(repository.worktree())
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
            .with_filesystem_provider(filesystem);
        if transactions
            .state
            .as_ref()
            .is_some_and(|state| state.holds_runtime_owner_lock())
        {
            let capability = transactions
                .format_context
                .capture_capability(transactions.capture_relation_id)
                .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
            bindings = bindings.with_repository_capture_capability(capability);
        }
        *host_bindings = Some(bindings);
    }
    let bindings = host_bindings
        .as_mut()
        .expect("host bindings initialized");
    if transactions.lease.is_none() {
        let state = transactions
            .state
            .as_ref()
            .expect("runtime state initialized");
        let lease = transactions
            .runtime
            .block_on(state.acquire_lease(invocation_owner_id()))
            .map_err(|error| repl_lease_failure(&error, state))?;
        transactions.lease = Some(lease);
    }
    let state = transactions
        .state
        .as_ref()
        .expect("runtime state initialized");
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
    let committed = PinnedCommittedRows::new(pin, committed_rows)
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let snapshot = transactions
        .runtime
        .block_on(state.begin_admitted_table_activation_over_committed(
            &transactions.tables,
            committed,
        ))
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let query_id = cli_request_nonce().to_be_bytes();
    let expression = (source.len() <= 16 * 1024
        && !source.is_empty()
        && !source.as_bytes().contains(&0))
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
    let writer = transactions.lease.expect("runtime writer lease initialized");
    transactions
        .runtime
        .block_on(state.begin_query_observation(
            writer,
            QueryObservationRegistration {
                id: query_id,
                expression,
                capture: snapshot.context().capture().clone(),
            },
        ))
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let finish_query = |status| {
        transactions
            .runtime
            .block_on(state.finish_query_observation(writer, query_id, status))
            .map(|_| ())
            .map_err(|_| REPL_EFFECT_UNCERTAIN.to_owned())
    };
    let terminal = |prefix: &'static [u8]| {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&query_id);
        TerminalOutcome::new(bytes).ok()
    };
    // A row whose input never reached an external binding is cancelled before
    // any observation is recorded, so a failed observation write cannot strand
    // it. A failed cancellation leaves the row unsettled, which is uncertain.
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
    let abandon = |original: repl::ReplFailure| -> repl::ReplFailure {
        let cancelled = terminal(b"orna.repl.cancel.v1").is_some_and(|outcome| {
            transactions
                .runtime
                .block_on(state.cancel_request_with_owner(request, fingerprint, writer, outcome))
                .is_ok()
        });
        if finish_query(orna_runtime_v1::InvocationObservationStatus::Failed).is_err() {
            eprintln!("warning: query observation not recorded");
        }
        if cancelled {
            original
        } else {
            repl_effect_uncertain(settlement_cause(&original))
        }
    };
    let admission = match transactions
        .runtime
        .block_on(state.reserve_request_with_admission(request, fingerprint))
    {
        Ok((_, Some(admission))) => admission,
        _ => {
            finish_query(orna_runtime_v1::InvocationObservationStatus::Failed)?;
            return Err("ORNA-REPL-EFFECT".to_owned().into());
        }
    };
    if transactions
        .runtime
        .block_on(state.start_request_with_owner_and_admission(
            request,
            fingerprint,
            writer,
            admission,
        ))
        .is_err()
    {
        // The failed start leaves the row Reserved and no binding has run.
        return Err(abandon("ORNA-REPL-EFFECT".to_owned().into()));
    }
    let evaluation = transactions
        .authority
        .evaluate_staged_repl_transaction(session, staged, &scope, bindings);
    let (value, successor, activation, external_dispatched) = match evaluation {
        Ok(result) => result,
        Err(failure) => {
            if failure.external_effects_possible {
                // The row stays Running: fenced recovery settles it and the
                // input is never replayed.
                finish_query(orna_runtime_v1::InvocationObservationStatus::Failed)?;
                return Err(repl_evaluation_failure(failure));
            }
            return Err(abandon(repl_evaluation_failure(failure)));
        }
    };
    // Known once evaluation succeeded: an external binding may already have run.
    let external_possible = matches!(
        activation.effects(),
        orna_runtime_v1::ActivationEffectReport::ExternalPossible
    );
    // The request is committed, so an observation write failure is only a
    // warning and never reports the committed input as uncertain.
    let record_committed = |status: orna_runtime_v1::InvocationObservationStatus| {
        if finish_query(status).is_err() {
            eprintln!("warning: committed; query observation not recorded");
        }
    };
    // Failure after evaluation: uncertain when external effects may have run
    // (the row stays Running for fenced recovery); otherwise cancelled.
    let unsettled_after_evaluation = || -> repl::ReplFailure {
        if external_possible {
            let _ = finish_query(orna_runtime_v1::InvocationObservationStatus::Failed);
            REPL_EFFECT_UNCERTAIN.to_owned().into()
        } else {
            abandon("ORNA-REPL-EFFECT".to_owned().into())
        }
    };
    let unsettled_after_evaluation_because = |cause: String| -> repl::ReplFailure {
        if external_possible {
            let _ = finish_query(orna_runtime_v1::InvocationObservationStatus::Failed);
            repl_effect_uncertain(cause)
        } else {
            abandon(repl_effect_cancelled(cause))
        }
    };
    if activation.mutations().is_empty() {
        // Read-only results are retained: the request completes with its
        // terminal outcome instead of being left running.
        let completed = terminal(b"orna.repl.read.v1").is_some_and(|outcome| {
            transactions
                .runtime
                .block_on(state.complete_request_with_owner(request, fingerprint, writer, outcome))
                .is_ok()
        });
        if !completed {
            return Err(unsettled_after_evaluation());
        }
        record_committed(orna_runtime_v1::InvocationObservationStatus::Succeeded);
        *session = successor;
        return Ok(value);
    }
    let activation = match activation
        .stage_for_request(&transactions.authority, snapshot.context(), request)
    {
        Ok(activation) => activation,
        Err(_) => return Err(unsettled_after_evaluation()),
    };
    // The application stages with NoFault; rebuild the identical unit carrying
    // the runtime's injector so the request boundary owns the fault seam.
    let request_staged = match StagedTableActivation::from_source(
        activation.context().clone(),
        activation.mutations().to_vec(),
        activation.next_digest(),
        Arc::clone(&transactions.faults),
        activation.effects(),
    ) {
        Ok(staged) => staged,
        Err(_) => return Err(unsettled_after_evaluation()),
    };
    let Some(outcome) = terminal(b"orna.repl.commit.v1") else {
        return Err(unsettled_after_evaluation());
    };
    // ORNA-ASSERT-042: a project that declares assertions checks the
    // post-mutation candidate against them before any row, checkpoint or
    // terminal is published. A project without assertions has nothing to
    // check, so its commit keeps the unvalidated activation path.
    if project_declares_assertions(&transactions.project) {
        let cwd_generation = u64::try_from(request_staged.context().capture().generation())
            .ok()
            .and_then(|runtime| {
                repository
                    .cwd_generation(orna_repository_v1::RuntimeGeneration::new(runtime))
                    .ok()
            });
        let Some(cwd_generation) = cwd_generation else {
            return Err(unsettled_after_evaluation());
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
                return Err(unsettled_after_evaluation_because(stage_outcome_cause(
                    &outcome,
                )));
            }
        };
        let committed = transactions
            .runtime
            .block_on(state.commit_validated_table_request_activation(
                ValidatedTableRequestActivationCommit {
                    writer,
                    identity: request,
                    fingerprint,
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
            Err(TableActivationError::ValidationFailed(diagnostic)) if !external_dispatched => {
                let failure = if orna_conformance_v1::is_table_assertion_false(diagnostic) {
                    repl::ReplFailure::from("ORNA-A091-009".to_owned())
                } else {
                    repl_effect_cancelled(format!(
                        "table assertion validation did not complete: {diagnostic:?}"
                    ))
                };
                return Err(abandon(failure));
            }
            Err(error) => {
                finish_query(orna_runtime_v1::InvocationObservationStatus::Failed)?;
                return Err(repl_effect_uncertain(format!(
                    "table commit failed with {}",
                    table_activation_cause(&error)
                )));
            }
        }
    } else {
        let committed = transactions
            .runtime
            .block_on(state.commit_staged_table_request_activation(
                writer,
                request,
                fingerprint,
                &request_staged,
                outcome,
            ));
        if let Err(error) = committed {
            finish_query(orna_runtime_v1::InvocationObservationStatus::Failed)?;
            return Err(repl_effect_uncertain(format!(
                "table commit failed with {error}"
            )));
        }
    }
    record_committed(orna_runtime_v1::InvocationObservationStatus::Succeeded);
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
        ApplicationError::Evaluation(code)
        | ApplicationError::EffectRejected(code)
        | ApplicationError::SourceEffectFailed(code) => code,
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
    repl::ReplFailure {
        code: REPL_EFFECT_UNCERTAIN.to_owned(),
        detail: Some(orna_evaluator_v1::ReplDetail {
            code: REPL_EFFECT_UNCERTAIN.to_owned(),
            message,
            span: None,
            notes: Vec::new(),
            help: vec![
                "the input is not replayed; the request row stays running for fenced recovery"
                    .to_owned(),
            ],
        }),
    }
}

/// Submission failure for an input that was not published and is cancelled.
/// `message` names the cause so the failure stays diagnosable.
fn repl_effect_cancelled(message: String) -> repl::ReplFailure {
    repl::ReplFailure {
        code: "ORNA-REPL-EFFECT".to_owned(),
        detail: Some(orna_evaluator_v1::ReplDetail {
            code: "ORNA-REPL-EFFECT".to_owned(),
            message,
            span: None,
            notes: Vec::new(),
            help: Vec::new(),
        }),
    }
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
        TableActivationError::ValidationFailed(diagnostic) => format!("{diagnostic:?}"),
    }
}

/// The validator-build outcome's cause, for the unsettled detail.
fn stage_outcome_cause(outcome: &StageOutcome<orna_foundation_v1::Diagnostic>) -> String {
    match outcome {
        StageOutcome::Failed(diagnostic) | StageOutcome::Cancelled(diagnostic) => {
            format!("{}: {}", diagnostic.code(), diagnostic.message())
        }
        StageOutcome::Skipped { reason } => reason.clone(),
        StageOutcome::Passed => "validator build produced no validator".to_owned(),
    }
}

fn run_repl_submission<W: io::Write>(
    session: &mut AdmittedReplSession,
    source: &str,
    writer: &mut W,
    color_enabled: bool,
) -> Result<(), Diagnostic> {
    let mut submitter = |session: &mut AdmittedReplSession, source: &str| {
        session.submit(source).map_err(repl::ReplFailure::from)
    };
    run_repl_submission_with(session, source, writer, color_enabled, &mut submitter)
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
            write_styled(
                writer,
                AnsiColor::Green,
                result.as_bytes(),
                color_enabled,
            )
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
    let host_bindings = runtime
        .transactions
        .as_ref()
        .map(|transactions| transactions.host_bindings.clone())
        .unwrap_or_else(|| Rc::new(RefCell::new(None)));
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
    repl::run_with_snapshot_loader_and_color_and_submitter(
        &mut BufReader::new(stdin.lock()),
        &mut stdout.lock(),
        &mut runtime.session,
        &loader,
        &mut submitter,
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
    let requested_color = requested_color_mode(&args);
    let debug_enabled = requested_debug_mode(&args);
    let parsed = match parse_cli(&args) {
        Ok(value) => value,
        Err(error) => {
            let _ = write_diagnostic(
                &mut io::stderr().lock(),
                &error,
                requested_color.stderr_enabled(),
                debug_enabled,
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
                debug_enabled,
            );
            ExitCode::from(error.exit as u8)
        }
    }
}


fn repository_init_diagnostic(error: &orna_repository_v1::RepositoryInitError) -> Diagnostic {
    let code = error.code();
    let (title, help) = match error {
        orna_repository_v1::RepositoryInitError::GitUnavailable => (
            "Git is not available",
            "install or enable Git, then retry `orna init`",
        ),
        orna_repository_v1::RepositoryInitError::GitOperationFailed => (
            "Git could not initialize the repository",
            "check that Git can initialize the target directory, then retry `orna init`",
        ),
        orna_repository_v1::RepositoryInitError::LocalStateUnavailable => (
            "local repository state is unavailable",
            "check local filesystem access for the target directory, then retry `orna init`",
        ),
        orna_repository_v1::RepositoryInitError::UnsafeMetadataPath => (
            "repository metadata has an unsafe path",
            "replace unsafe metadata links or non-regular entries before retrying `orna init`",
        ),
        orna_repository_v1::RepositoryInitError::MetadataIncomplete => (
            "repository metadata is incomplete",
            "restore complete metadata from a valid snapshot or initialize a new repository",
        ),
        orna_repository_v1::RepositoryInitError::MetadataMalformed => (
            "repository metadata is invalid",
            "restore valid metadata from a compatible repository snapshot before retrying",
        ),
        orna_repository_v1::RepositoryInitError::MetadataUnsupported => (
            "repository metadata format is unsupported",
            "use an Orna version that supports this repository format",
        ),
        orna_repository_v1::RepositoryInitError::MetadataChanged => (
            "repository metadata changed during initialization",
            "ensure no other process changes repository metadata, then retry `orna init`",
        ),
        orna_repository_v1::RepositoryInitError::RepositoryBusy => (
            "repository initialization is already in progress",
            "wait for the other initialization to finish, then retry `orna init`",
        ),
        orna_repository_v1::RepositoryInitError::PlatformUnsupported => (
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
    orna_repository_v1::initialize_repository(target)
        .map_err(|error| repository_init_diagnostic(&error))?;
    write_success_line("initialized Orna repository", color_enabled)
}
/// Maps a fetch failure to its documented exit status: an unavailable remote is a
/// connection failure (3) and a malformed advertisement is a protocol failure (7);
/// every other fetch refusal keeps the target status.
fn fetch_failure(error: &orna_repository_v1::FetchError) -> Diagnostic {
    let exit = match error {
        orna_repository_v1::FetchError::RemoteUnavailable => Exit::Connection,
        orna_repository_v1::FetchError::MalformedAdvertisement => Exit::Protocol,
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
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run `fetch` inside a Git worktree or provide a local project path",
        )
    })?;
    let requested = orna_repository_v1::RequestedRef::branch(branch.to_owned()).map_err(
        |error| {
            Diagnostic::target_with_detail(
                "E2100",
                "fetch branch is invalid",
                "supply a valid Git branch name",
                error.to_string(),
            )
        },
    )?;
    let report = repository
        .fetch_with_local_continuity(remote.to_owned(), [requested])
        .map_err(|error| fetch_failure(&error))?;
    if let Some(continuity) = report.continuity() {
        if let Some(diagnostic) = fetch_continuity_diagnostic(continuity) {
            return Err(diagnostic);
        }
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
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run `push` inside a Git worktree or provide a local project path",
        )
    })?;
    let branch = match branch {
        Some(branch) => branch.to_owned(),
        None => repository.checked_out_branch().map_err(|_| {
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
        runtime
            .block_on(orna_storage_v1::RuntimePublicationCoordinator::recover(
                &repository,
                &state,
            ))
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "pending local publication could not be recovered",
                    "resolve the publication recovery error before retrying `push`",
                    error.to_string(),
                )
            })?;
        if repository
            .read_publication_journal()
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2200",
                    "local publication state could not be verified",
                    "verify the pending publication before retrying `push`",
                    error.to_string(),
                )
            })?
            .is_some()
        {
            return Err(Diagnostic::target(
                "E2200",
                "local publication recovery remains pending",
                "resolve the publication recovery before retrying `push`",
            ));
        }
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
        .and_then(|()| {
            write_styled_display(&mut stdout, AnsiColor::Cyan, remote, color_enabled)
        })
        .and_then(|()| stdout.write_all(b" -> "))
        .and_then(|()| {
            write_styled_display(&mut stdout, AnsiColor::Cyan, &branch, color_enabled)
        })
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

fn fetch_continuity_diagnostic(
    continuity: orna_repository_v1::RemoteContinuity,
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
mod tests {
    use super::*;
    use std::path::PathBuf;
    use orna_runtime_v1::{Mutation, NoFault, RunObservationStatus, RuntimeState};
    use sha2::{Digest, Sha256};
    #[test]
    fn color_mode_respects_stream_tty_and_explicit_overrides() {
        assert!(!ColorMode::Auto.enabled(false));
        assert!(ColorMode::Auto.enabled(true));
        assert!(ColorMode::Always.enabled(false));
        assert!(ColorMode::Always.enabled(true));
        assert!(!ColorMode::Never.enabled(false));
        assert!(!ColorMode::Never.enabled(true));
    }

    #[test]
    fn fetch_accepts_only_continuous_witnesses() {
        assert!(
            fetch_continuity_diagnostic(orna_repository_v1::RemoteContinuity::Continuous).is_none()
        );

        for continuity in [
            orna_repository_v1::RemoteContinuity::Missing,
            orna_repository_v1::RemoteContinuity::Stale,
            orna_repository_v1::RemoteContinuity::InvalidEvidence,
            orna_repository_v1::RemoteContinuity::Unverifiable,
        ] {
            let diagnostic = fetch_continuity_diagnostic(continuity)
                .expect("non-continuous witnesses are rejected");
            assert_eq!(diagnostic.code, "E2100");
            assert_eq!(diagnostic.title, "Orna continuity witness was rejected");
            assert_eq!(
                diagnostic.help,
                "do not claim allocator or checkpoint continuity until required refs/orna witnesses match"
            );
            assert_eq!(
                diagnostic.detail.as_deref(),
                Some(format!("remote continuity state: {continuity:?}").as_str())
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table-driven behavioral scenario documents stable CLI parse precedence"
    )]
    fn parsing_is_bounded_and_diagnostic_is_stable() {
        let parsed = parse_cli(&[
            "--db".into(),
            "orna://host/reference".into(),
            "run".into(),
            "seed".into(),
        ])
        .expect("parses");
        assert_eq!(parsed.command, Command::Run(Invocation::Seed));
        assert_eq!(
            parsed.endpoint,
            Endpoint::RemoteTls("orna://host/reference".into())
        );
        assert_eq!(
            parse_cli(&["status".into()]).expect("default color mode parses").color,
            ColorMode::Auto
        );
        let colored = parse_cli(&[
            "--color".into(),
            "always".into(),
            "--db".into(),
            "project".into(),
            "status".into(),
        ])
        .expect("global color and database options parse before the command");
        assert_eq!(colored.color, ColorMode::Always);
        assert_eq!(colored.endpoint, Endpoint::Path("project".into()));
        assert_eq!(
            parse_cli(&["--color".into(), "never".into(), "status".into()])
                .expect("never mode parses")
                .color,
            ColorMode::Never
        );
        let error = parse_cli(&["--color".into(), "invalid".into(), "status".into()])
            .expect_err("invalid color mode is rejected");
        assert_eq!(
            (error.code, error.title.as_ref(), error.help),
            (
                "E1002",
                "unsupported value for `--color`",
                "choose `auto`, `always`, or `never`"
            )
        );
        let error = parse_cli(&["--color".into()]).expect_err("color mode value is required");
        assert_eq!(
            (error.code, error.title.as_ref()),
            ("E1001", "option `--color` needs a value")
        );
        assert_eq!(
            parse_cli(&["check".into()]).unwrap().command,
            Command::Check
        );
        assert_eq!(
            parse_cli(&["invoke".into(), "library.value".into()])
                .expect("parses")
                .command,
            Command::Invoke("library.value".into())
        );
        assert_eq!(
            parse_cli(&["repl".into(), "1 + 2".into()])
                .expect("parses")
                .command,
            Command::Repl(Some("1 + 2".into()))
        );
        assert_eq!(
            parse_cli(&["status".into(), "--porcelain".into()])
                .expect("parses")
                .command,
            Command::Status {
                format: StatusFormat::Porcelain,
            }
        );
        assert_eq!(
            parse_cli(&["status".into()])
                .expect("human status parses")
                .command,
            Command::Status {
                format: StatusFormat::Human,
            }
        );
        assert_eq!(
            parse_cli(&["status".into(), "--short".into()])
                .expect("short status parses")
                .command,
            Command::Status {
                format: StatusFormat::Short,
            }
        );
        assert_eq!(
            parse_cli(&["status".into(), "--format".into(), "json".into()])
                .expect("JSON status parses")
                .command,
            Command::Status {
                format: StatusFormat::Json,
            }
        );
        let error = parse_cli(&["status".into(), "--porcelain=v2".into()])
            .expect_err("status format is exact");
        assert_eq!(error.code, "E1002");
        assert_eq!(
            (error.title.as_ref(), error.help),
            (
                "unsupported `status` option or option placement",
                "use `status`, `status --porcelain`, `status --short`, `status --format human|short|json`, or `--format human|short|json status`"
            )
        );
        assert_eq!(
            parse_cli(&["init".into()]).expect("parses").command,
            Command::Init(None)
        );
        assert_eq!(
            parse_cli(&["init".into(), "project".into()])
                .expect("parses")
                .command,
            Command::Init(Some(PathBuf::from("project")))
        );
        assert_eq!(
            parse_cli(&["init".into(), "--".into(), "--bare".into()])
                .expect("literal target parses")
                .command,
            Command::Init(Some(PathBuf::from("--bare")))
        );
        assert_eq!(
            parse_cli(&["--db".into(), "project".into(), "init".into()])
                .expect_err("init must not use a database endpoint")
                .code,
            "E1002"
        );
        assert_eq!(
            parse_cli(&["init".into(), "--bare".into()])
                .expect_err("unsupported init option")
                .code,
            "E1002"
        );
        assert_eq!(
            parse_cli(&["init".into(), "one".into(), "two".into()])
                .expect_err("only one target is accepted")
                .code,
            "E1003"
        );
        assert_eq!(
            parse_cli(&["invoke".into()])
                .expect_err("target is required")
                .code,
            "E1001"
        );
        assert_eq!(
            parse_cli(&["serve".into()]).unwrap().command,
            Command::Serve { port: 8080, allow_push: false }
        );
        assert_eq!(
            parse_cli(&["serve".into(), "--port".into(), "0".into()])
                .unwrap()
                .command,
            Command::Serve { port: 0, allow_push: false }
        );
    }

    #[test]
    fn explain_documents_a_stable_diagnostic_and_rejects_unknown_codes() {
        assert_eq!(
            parse_cli(&["explain".into(), "ORNA-S010-IMPORT".into()])
                .expect("explain parses")
                .command,
            Command::Explain("ORNA-S010-IMPORT".into())
        );
        assert_eq!(
            explain_diagnostic("ORNA-S010-IMPORT").expect("known diagnostic"),
            "ORNA-S010-IMPORT: imported module is unavailable\nThe project names a module that is not part of its captured dependencies, so the module cannot be checked or executed.\nhelp: use a captured standard dependency or remove the import"
        );
        let error = explain_diagnostic("ORNA-NOT-A-CODE").expect_err("unknown diagnostic");
        assert_eq!((error.code, error.exit), ("E1002", Exit::Usage));
    }

    #[test]
    fn semantic_legend_unknown_class_or_option_exits_with_usage_code_two() {
        for words in [
            &["semantic-legend", "--class", "string"][..],
            &["semantic-legend", "--format", "xml"][..],
            &["semantic-legend", "--limit", "0"][..],
        ] {
            let arguments = words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
            match parse_cli(&arguments) {
                Ok(_) => panic!("{words:?} should be rejected"),
                Err(error) => {
                    assert_eq!((error.code, error.exit as i32), ("E1002", 2), "{words:?}");
                }
            }
        }
    }

    #[test]
    fn explain_documents_frozen_assertion_and_syntax_diagnostics() {
        assert_eq!(
            explain_diagnostic("ORNA-A091-001").expect("assertion diagnostic is documented"),
            "ORNA-A091-001: refined type uses the legacy `where self` form\nRefined Orna types state their invariant in a brace-delimited assertion block owned by the type.\nhelp: replace `where self` with a brace-delimited `assert` block"
        );
        for code in [
            "ORNA-A091-002",
            "ORNA-A091-003",
            "ORNA-A091-004",
            "ORNA-A091-005",
            "ORNA-A091-006",
            "ORNA-A091-007",
            "ORNA-A091-008",
            "ORNA-A091-009",
            "ORNA-A091-010",
            "ORNA-A091-011",
            "ORNA-A091-012",
            "ORNA091-E-MATCH",
            "ORNA091-E-RESULT",
            "ORNA091-E-POSTFIX-QUESTION",
            "ORNA091-E-CURRENCY",
            "ORNA091-E-CURRENCY-SYMBOL",
            "ORNA091-E-IMPL-FOR",
            "ORNA091-E-BOUND-COLON",
            "ORNA091-E-STATIC-FN",
            "ORNA091-E-TRYFROM",
            "ORNA091-E-CONVERSION-CHAIN",
            "ORNA091-E-OPAQUE",
            "ORNA091-E-FIELD-CONSTRAINT",
        ] {
            assert!(
                explain_diagnostic(code).is_ok(),
                "frozen diagnostic {code} must have user guidance"
            );
        }
    }

    /// ORNA-CLI-006: supported terminals receive safe OSC 8 status-path hyperlinks.
    #[test]
    fn human_status_links_safe_paths_only_for_supported_terminal_capabilities() {
        const FIXTURE: &str = include_str!("../tests/fixtures/function-expression.orna");
        let directory = tempfile::tempdir().expect("status repository");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git init")
                .success()
        );
        for name in ["function-expression.orna", "function expression#?.orna"] {
            std::fs::write(directory.path().join(name), FIXTURE).expect("fixture source");
        }

        let human_output = std::process::Command::new("git")
            .args(["-c", "color.status=false", "status", "--untracked-files=all"])
            .current_dir(directory.path())
            .output()
            .expect("human Git status");
        assert!(human_output.status.success());
        let root = directory.path().canonicalize().expect("absolute repository root");
        let mut without_links = Vec::new();
        write_human_status(&human_output.stdout, &mut without_links, true, None)
            .expect("unlinked human status");
        let mut linked = Vec::new();
        write_human_status(
            &human_output.stdout,
            &mut linked,
            true,
            Some(&root),
        )
        .expect("linked human status");

        let linked_text = std::str::from_utf8(&linked).expect("UTF-8 human status");
        for (name, encoded_suffix) in [
            ("function-expression.orna", "/function-expression.orna"),
            ("function expression#?.orna", "/function%20expression%23%3F.orna"),
        ] {
            let url = file_url_for_status_path(&root, name.as_bytes()).expect("safe file URL");
            assert!(url.starts_with("file:///"));
            assert!(url.ends_with(encoded_suffix), "{url}");
            assert!(
                linked_text.contains(&format!("\x1b]8;;{url}\x1b\\")),
                "missing OSC 8 URL for {name:?}"
            );
            assert!(linked_text.contains(name), "missing visible path {name:?}");
        }
        assert_eq!(visible_without_osc8(&linked), without_links);
        assert!(file_url_for_status_path(&root, b"\"quoted path\"").is_none());
        assert!(file_url_for_status_path(&root, b"control\npath").is_none());
        assert!(file_url_for_status_path(&root, b"../outside").is_none());
        assert!(file_url_for_status_path(&root, &[0xff]).is_none());

        assert!(osc8_terminal_supports(
            Some("xterm-kitty"),
            None,
            None,
            None,
            None,
            false,
        ));
        assert!(!osc8_terminal_supports(
            Some("xterm-256color"),
            None,
            None,
            None,
            None,
            false,
        ));
        assert!(!osc8_terminal_supports(
            Some("xterm-kitty"),
            None,
            None,
            None,
            None,
            true,
        ));
        assert!(osc8_terminal_supports(
            None,
            Some("iTerm.app"),
            Some("3.1.0"),
            None,
            None,
            false,
        ));
        assert!(!osc8_terminal_supports(
            None,
            Some("iTerm.app"),
            Some("3.0.9"),
            None,
            None,
            false,
        ));

        let short_output = std::process::Command::new("git")
            .args(["status", "--short", "--untracked-files=all"])
            .current_dir(directory.path())
            .output()
            .expect("short Git status");
        assert!(short_output.status.success());
        let mut rendered_short = Vec::new();
        write_short_status(&short_output.stdout, &mut rendered_short, false)
            .expect("rendered short status");
        assert_eq!(rendered_short, short_output.stdout);
    }

    #[test]
    fn unpublished_status_summary_reports_zero_and_remaining_runtime_mutations() {
        let mut rendered = Vec::new();
        write_unpublished_summary(&mut rendered, 0).expect("empty unpublished summary");
        write_unpublished_summary(&mut rendered, 1).expect("singular unpublished summary");
        write_unpublished_summary(&mut rendered, 2).expect("plural unpublished summary");
        assert_eq!(
            rendered,
            b"Unpublished CWD changes: 0 runtime mutations\nUnpublished CWD changes: 1 runtime mutation\nUnpublished CWD changes: 2 runtime mutations\n"
        );
    }

    #[test]
    fn json_status_includes_unpublished_runtime_mutation_count() {
        assert_eq!(
            append_unpublished_count_json(
                b"{\"branch\":\"main\",\"changes\":[]}\n".to_vec(),
                4,
            )
            .expect("JSON status terminator"),
            b"{\"branch\":\"main\",\"changes\":[],\"unpublished_runtime_mutations\":4}\n"
        );
        assert!(append_unpublished_count_json(b"{}".to_vec(), 0).is_none());
    }

    #[test]
    fn unpublished_mutation_count_reads_the_runtime_pending_tail() {
        let directory = tempfile::tempdir().expect("status repository");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git init")
                .success()
        );
        orna_repository_v1::initialize_repository(directory.path())
            .expect("initialize Orna repository");
        let repository = orna_repository_v1::Repository::discover(directory.path())
            .expect("discover initialized repository");
        let (identity, initial_digest) = runtime_identity(&repository).expect("runtime identity");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime executor");
        let state = runtime
            .block_on(RuntimeState::open(&repository, identity, initial_digest))
            .expect("open runtime state");
        assert_eq!(unpublished_mutation_count(&repository), Ok(0));

        let payload = b"pending table mutation".to_vec();
        let mutation = Mutation {
            id: [1; 16],
            digest: Sha256::digest(&payload).into(),
            payload,
        };
        runtime
            .block_on(async {
                let lease = state.acquire_lease(identity.repository_id).await?;
                let capture = state.capture().await?;
                state
                    .commit(lease, &capture, &mutation, [2; 32], &NoFault)
                    .await
            })
            .expect("append durable runtime mutation");
        drop(state);

        assert_eq!(unpublished_mutation_count(&repository), Ok(1));
    }

    fn visible_without_osc8(output: &[u8]) -> Vec<u8> {
        let mut visible = Vec::with_capacity(output.len());
        let mut remaining = output;
        while let Some(start) = remaining.windows(4).position(|window| window == b"\x1b]8;") {
            visible.extend_from_slice(&remaining[..start]);
            let link = &remaining[start..];
            let end = link
                .windows(2)
                .position(|window| window == b"\x1b\\")
                .expect("OSC 8 terminator");
            remaining = &link[end + 2..];
        }
        visible.extend_from_slice(remaining);
        visible
    }

    #[test]
    fn bare_status_dispatches_to_git_human_status() {
        let directory = tempfile::tempdir().expect("status repository");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git init")
                .success()
        );
        let parsed = parse_cli(&["status".into()]).expect("human status parses");
        assert_eq!(
            execute(&Parsed {
                endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
                command: parsed.command,
                color: ColorMode::Auto,
            }),
            Ok(())
        );
    }

    #[test]
    fn project_repl_factory_retains_imports_in_a_scripted_session() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn run(): Int = library.twice(21);",
        )
        .expect("main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn twice(value: Int): Int = value + value; fn hidden(value: Int): Int = value;",
        )
        .expect("library source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let mut runtime = repl_session(&endpoint).expect("project session");
        let mut input =
            b"use library;\nlet n: Int = 21;\nlibrary.hidden(n)\nlibrary.twice(n)\n:quit\n"
                .as_slice();
        let mut output = Vec::new();
        repl::run(&mut input, &mut output, &mut runtime.session).expect("scripted session");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.contains("error[ORNA-S012-UNRESOLVED]: "), "{output}");
        assert!(output.contains("hidden"), "{output}");
        assert!(output.contains("line 1, column 9"), "{output}");
        assert!(output.contains("> 42 : Int\n> "), "{output}");
    }

    #[test]
    fn project_repl_rejects_an_uncaptured_standard_import() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            include_str!("../tests/fixtures/reference-project/page.orna"),
        )
        .expect("accepted source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let error = repl_session(&endpoint)
            .err()
            .expect("uncaptured standard module");
        assert_eq!(error.code, "ORNA-S010-IMPORT");
        assert_eq!(error.exit, Exit::Target);
        assert_eq!(error.title, "imported module is unavailable");
        assert_eq!(
            error.help,
            "use a captured standard dependency or remove the import"
        );
    }


    #[test]
    fn repl_repository_authority_is_reused_for_cwd_head_and_named_refs_without_git_mutation() {
        let directory = tempfile::tempdir().expect("temporary project");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git init")
                .success()
        );
        std::fs::write(directory.path().join("main.orna"), "use library;")
            .expect("committed source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn answer(): Int = 42;",
        )
        .expect("committed library source");
        assert!(
            std::process::Command::new("git")
                .args(["add", "main.orna", "library.orna"])
                .current_dir(directory.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args([
                    "-c",
                    "user.email=kieran@drewett.dev",
                    "-c",
                    "user.name=kierandrewett",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "snapshot",
                ])
                .current_dir(directory.path())
                .status()
                .expect("git commit")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["branch", "snapshot"])
                .current_dir(directory.path())
                .status()
                .expect("git branch")
                .success()
        );
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn answer(): Int = 7;",
        )
        .expect("CWD source");
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(directory.path())
            .output()
            .expect("Git status before snapshot reads");

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let runtime = repl_session(&endpoint).expect("project REPL runtime");
        let repository = runtime
            .repository
            .as_ref()
            .expect("opened project repository remains session authority");
        assert_eq!(repository.worktree(), directory.path());
        let loader = ReplSnapshotSessionLoader {
            repository: Some(repository),
            host_bindings: Rc::new(RefCell::new(None)),
        };
        for (target, expected) in [
            (repl::SnapshotTarget::Cwd, "7 : Int"),
            (repl::SnapshotTarget::Head, "42 : Int"),
            (repl::SnapshotTarget::Ref("snapshot".into()), "42 : Int"),
        ] {
            let mut session = repl::SnapshotSessionLoader::load_snapshot(&loader, target)
                .expect("snapshot project is admitted");
            let imported = session.submit("use library;");
            assert!(imported.is_ok(), "snapshot import failed: {imported:?}");
            let value = session
                .submit("library.answer()")
                .expect("snapshot function evaluates")
                .expect("visible result");
            assert_eq!(repl::inspect(&value), expected);
        }

        let after = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(directory.path())
            .output()
            .expect("Git status after snapshot reads");
        assert_eq!(after.stdout, status.stdout);
        assert_eq!(after.stderr, status.stderr);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the regression preserves every Git observation needed to prove read-only snapshot selection"
    )]
    fn repl_snapshot_loader_keeps_a_selected_ref_stable_after_it_moves() {
        let directory = tempfile::tempdir().expect("temporary project");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git init")
                .success()
        );
        std::fs::write(directory.path().join("main.orna"), "use library;")
            .expect("initial main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn answer(): Int = 42;",
        )
        .expect("initial library source");
        assert!(
            std::process::Command::new("git")
                .args(["add", "main.orna", "library.orna"])
                .current_dir(directory.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args([
                    "-c",
                    "user.email=orna@example.test",
                    "-c",
                    "user.name=Orna Test",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "selected snapshot",
                ])
                .current_dir(directory.path())
                .status()
                .expect("initial git commit")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["branch", "snapshot"])
                .current_dir(directory.path())
                .status()
                .expect("snapshot branch")
                .success()
        );
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn answer(): Int = 7;",
        )
        .expect("advanced library source");
        assert!(
            std::process::Command::new("git")
                .args(["add", "library.orna"])
                .current_dir(directory.path())
                .status()
                .expect("git add advanced source")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args([
                    "-c",
                    "user.email=orna@example.test",
                    "-c",
                    "user.name=Orna Test",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "advanced source",
                ])
                .current_dir(directory.path())
                .status()
                .expect("advanced git commit")
                .success()
        );

        let git_output = |arguments: &[&str]| {
            std::process::Command::new("git")
                .args(arguments)
                .current_dir(directory.path())
                .output()
                .expect("git output")
        };
        let selected_ref = git_output(&["rev-parse", "snapshot"]);
        assert!(selected_ref.status.success());
        let head = git_output(&["rev-parse", "HEAD"]);
        assert!(head.status.success());
        assert_ne!(selected_ref.stdout, head.stdout);
        let symbolic_head = git_output(&["symbolic-ref", "--quiet", "HEAD"]);
        assert!(symbolic_head.status.success());
        let index_path = git_output(&["rev-parse", "--git-path", "index"]);
        assert!(index_path.status.success());
        let index_path = directory.path().join(
            std::str::from_utf8(&index_path.stdout)
                .expect("UTF-8 index path")
                .trim(),
        );
        let index = std::fs::read(&index_path).expect("index before snapshot selection");
        let main =
            std::fs::read(directory.path().join("main.orna")).expect("main before selection");
        let library =
            std::fs::read(directory.path().join("library.orna")).expect("library before selection");
        let status = git_output(&["status", "--porcelain=v1", "-z"]);
        assert!(status.status.success());

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let runtime = repl_session(&endpoint).expect("project REPL runtime");
        let repository = runtime
            .repository
            .as_ref()
            .expect("opened project repository remains session authority");
        let host_bindings = Rc::new(RefCell::new(Some(SysHostBindingRegistry::new(
            EnvironmentProvider::default(),
        ))));
        let loader = ReplSnapshotSessionLoader {
            repository: Some(repository),
            host_bindings: host_bindings.clone(),
        };
        let mut session = repl::SnapshotSessionLoader::load_snapshot(
            &loader,
            repl::SnapshotTarget::Ref("snapshot".into()),
        )
        .expect("selected snapshot project is admitted");
        assert!(host_bindings.borrow().is_none());

        assert!(
            std::process::Command::new("git")
                .args(["branch", "--force", "snapshot", "HEAD"])
                .current_dir(directory.path())
                .status()
                .expect("move selected branch")
                .success()
        );
        let moved_ref = git_output(&["rev-parse", "snapshot"]);
        assert!(moved_ref.status.success());
        assert_eq!(moved_ref.stdout, head.stdout);

        session
            .submit("use library;")
            .expect("selected import succeeds");
        let value = session
            .submit("library.answer()")
            .expect("selected function evaluates")
            .expect("visible result");
        assert_eq!(repl::inspect(&value), "42 : Int");

        let after_head = git_output(&["rev-parse", "HEAD"]);
        let after_symbolic_head = git_output(&["symbolic-ref", "--quiet", "HEAD"]);
        let after_status = git_output(&["status", "--porcelain=v1", "-z"]);
        assert_eq!(after_head.stdout, head.stdout);
        assert_eq!(after_symbolic_head.stdout, symbolic_head.stdout);
        assert_eq!(
            std::fs::read(&index_path).expect("index after selection"),
            index
        );
        assert_eq!(
            std::fs::read(directory.path().join("main.orna")).expect("main after selection"),
            main
        );
        assert_eq!(
            std::fs::read(directory.path().join("library.orna")).expect("library after selection"),
            library
        );
        assert_eq!(after_status.stdout, status.stdout);
        assert_eq!(after_status.stderr, status.stderr);
    }

    #[test]
    fn repl_snapshot_loader_rejects_non_local_endpoints_as_at_errors() {
        let endpoint = Endpoint::RemoteTls("orna://host/project".into());
        let mut runtime = repl_session(&endpoint).expect("core-only remote REPL session");
        let loader = ReplSnapshotSessionLoader {
            repository: runtime.repository.as_ref(),
            host_bindings: Rc::new(RefCell::new(None)),
        };
        let mut input = b":at HEAD\n:at snapshot\n:quit\n".as_slice();
        let mut output = Vec::new();

        repl::run_with_snapshot_loader(&mut input, &mut output, &mut runtime.session, &loader)
            .expect("REPL runs");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> error[ORNA-REPL-AT]: requested project snapshot could not be loaded\nhelp: choose an available snapshot with `:at CWD`, `:at HEAD`, or `:at ref`\n> "
        );
    }

    #[test]
    fn project_source_rejects_repl_only_bindings() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(directory.path().join("main.orna"), "pub fn status() = $?;")
            .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        assert_eq!(
            repl_session(&endpoint)
                .err()
                .expect("module source rejects status binding")
                .code,
            "E2101"
        );
    }

    #[test]
    fn diagnostic_output_preserves_detail_help_and_final_newline() {
        let diagnostic = Diagnostic::target_with_detail(
            "E2300",
            "request failed",
            "retry the operation",
            "the expected remote response was unavailable",
        );
        let mut plain = Vec::new();
        write_diagnostic(&mut plain, &diagnostic, false, false)
            .expect("plain diagnostic write");
        assert_eq!(
            plain,
            b"error[E2300]: request failed\nhelp: retry the operation\n",
            "technical details are hidden in normal output"
        );

        let mut colored = Vec::new();
        write_diagnostic(&mut colored, &diagnostic, true, true)
            .expect("colored diagnostic write");
        assert_eq!(
            String::from_utf8(colored).expect("diagnostic is UTF-8"),
            "\x1b[31merror\x1b[0m[\x1b[31mE2300\x1b[0m]: \x1b[31mrequest failed\x1b[0m\n  \x1b[2mthe expected remote response was unavailable\x1b[0m\nhelp: \x1b[2mretry the operation\x1b[0m\n"
        );
    }

    #[test]
    fn durable_invocation_failure_carries_public_code_and_hides_it_without_debug() {
        let diagnostic = durable_invocation_failed(&orna_runtime_v1::RuntimeError::AdminBusy);
        assert_eq!(diagnostic.code, "E2200");
        assert_eq!(
            diagnostic.detail.as_deref(),
            Some("runtime failure: sys.admin.busy")
        );

        let mut plain = Vec::new();
        write_diagnostic(&mut plain, &diagnostic, false, false).expect("plain write");
        let plain = String::from_utf8(plain).expect("diagnostic is UTF-8");
        assert!(!plain.contains("sys.admin.busy"), "plain output: {plain}");
        assert!(!plain.contains("runtime failure"), "plain output: {plain}");

        let mut debug = Vec::new();
        write_diagnostic(&mut debug, &diagnostic, false, true).expect("debug write");
        let debug = String::from_utf8(debug).expect("diagnostic is UTF-8");
        assert!(
            debug.contains("runtime failure: sys.admin.busy"),
            "debug output: {debug}"
        );
    }

    #[test]
    fn durable_invocation_failure_without_public_code_carries_display_text() {
        let diagnostic = durable_invocation_failed(&orna_runtime_v1::RuntimeError::LeaseHeld);
        assert_eq!(diagnostic.code, "E2200");
        assert_eq!(
            diagnostic.detail.as_deref(),
            Some("runtime failure: runtime writer is held")
        );
    }

    #[test]
    fn failure_detail_formats_attached_cause_codes() {
        let severity = orna_foundation_v1::DiagnosticSeverity::Error;
        let message = || orna_foundation_v1::SafeText::new("redacted").expect("safe text");
        let cause = orna_foundation_v1::Diagnostic::new(
            orna_foundation_v1::SafeText::new("ORNA-EVAL-DIVIDE-BY-ZERO").expect("safe code"),
            severity,
            message(),
        )
        .expect("cause diagnostic");
        let failure = orna_foundation_v1::Diagnostic::new(
            orna_foundation_v1::SafeText::new("ORNA-EVAL-SURFACE").expect("safe code"),
            severity,
            message(),
        )
        .expect("failure diagnostic")
        .with_cause(cause);

        let delivery = stream_delivery_failed(&failure);
        assert_eq!(delivery.code, "E2200");
        assert_eq!(
            delivery.detail.as_deref(),
            Some("failure code: ORNA-EVAL-SURFACE (causes: ORNA-EVAL-DIVIDE-BY-ZERO)")
        );
    }

    #[test]
    fn single_expression_repl_reports_visible_success_failure_and_quit() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let mut output = Vec::new();
        assert_eq!(
            run_repl_submission(&mut session, "1 + 2", &mut output, false),
            Ok(())
        );
        let error = run_repl_submission(&mut session, "missing()", &mut output, false)
            .expect_err("failure is reported");
        assert_eq!(error.code, "ORNA-S012-UNRESOLVED");
        assert_eq!(error.exit, Exit::Target);
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "error[ORNA-S012-UNRESOLVED]: name `missing` cannot be resolved\n  --> line 1, column 1\n  |\n1 | missing()\n  | ^^^^^^^\nnote: `missing` is not declared in this session and is not imported\nhelp: declare `missing` before using it, for example `let missing = <value>`"
        );
        assert_eq!(
            run_repl_submission(&mut session, ":quit", &mut output, false),
            Ok(())
        );
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "3 : Int\n"
        );
    }

    struct BrokenWriter;

    impl std::io::Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("writer failed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn single_expression_repl_converts_writer_failure_to_console_diagnostic() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let error =
            run_repl_submission(&mut session, "1", &mut BrokenWriter, false)
                .expect_err("writer failure");
        assert_eq!((error.code, error.exit), ("E2200", Exit::Target));
    }
    #[test]
    fn execution_project_preserves_reachable_loose_row_metadata() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::create_dir_all(directory.path().join("contacts/Contact"))
            .expect("row directory");
        std::fs::write(
            directory.path().join("main.orna"),
            "use contacts.main; pub fn run() {}",
        )
        .expect("root source");
        std::fs::write(
            directory.path().join("contacts/main.orna"),
            "pub table Contact(id: Str) { name: Str, }",
        )
        .expect("table source");
        let row_source = "{ name: \"Alice Smith\" }\n";
        std::fs::write(
            directory.path().join("contacts/Contact/alice-smith.orna"),
            row_source,
        )
        .expect("row source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let repository =
            orna_repository_v1::Repository::discover(directory.path()).expect("repository");
        let project = orna_project_v1::ProjectLoader::default()
            .load(&repository)
            .expect("project loads");
        let execution = execution_project(&project);
        assert_eq!(execution.loose_rows.len(), 1);
        let row = &execution.loose_rows[0];
        assert_eq!(row.source_id, "contacts/Contact/alice-smith.orna");
        assert_eq!(row.parse_as, "row_unit");
        assert_eq!(row.source.as_bytes(), row_source.as_bytes());
    }

    #[test]
    fn check_loads_reachable_project_sources_and_ignores_unreachable_modules() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn run() {}",
        )
        .expect("main source");
        std::fs::write(directory.path().join("library.orna"), "pub fn seed() {}").expect("library");
        std::fs::write(directory.path().join("unused.orna"), "not a module").expect("unused");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        assert_eq!(check_project(&endpoint, false), Ok(()));
    }

    #[test]
    fn authoritative_reference_project_runs_seed_exercise_and_sensor_stream() {
        let directory = tempfile::tempdir().expect("temporary reference project");
        for (name, source) in [
            ("main.orna", include_str!("../tests/fixtures/reference-project/main.orna")),
            ("library.orna", include_str!("../tests/fixtures/reference-project/library.orna")),
            ("warehouse.orna", include_str!("../tests/fixtures/reference-project/warehouse.orna")),
            ("sensors.orna", include_str!("../tests/fixtures/reference-project/sensors.orna")),
            ("values.orna", include_str!("../tests/fixtures/reference-project/values.orna")),
        ] {
            std::fs::write(directory.path().join(name), source).expect("reference source");
        }
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["add", "."])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        orna_repository_v1::initialize_repository(directory.path()).expect("runtime metadata");
        let endpoint = Endpoint::Path(directory.path().display().to_string());
        assert_eq!(
            execute(&Parsed {
                endpoint: endpoint.clone(),
                command: Command::Check,
                color: ColorMode::Auto,
            }),
            Ok(())
        );
        assert_eq!(
            execute(&Parsed {
                endpoint: endpoint.clone(),
                command: Command::Run(Invocation::Seed),
                color: ColorMode::Auto,
            }),
            Ok(())
        );
        let repository =
            orna_repository_v1::Repository::discover(directory.path()).expect("repository");
        let (identity, initial_digest) = runtime_identity(&repository).expect("runtime identity");
        let observations = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                RuntimeState::open(&repository, identity, initial_digest)
                    .await
                    .expect("reopened runtime")
                    .run_observations()
                    .await
                    .expect("run observations")
            });
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].status, RunObservationStatus::Completed);
        assert!(!observations[0].live);
        let error = execute(&Parsed {
            endpoint: endpoint.clone(),
            command: Command::Run(Invocation::Seed),
            color: ColorMode::Auto,
        })
        .expect_err("duplicate seed must be rejected");
        assert_eq!((error.code, error.exit), ("E2200", Exit::Target));
        assert_eq!(
            execute(&Parsed {
                endpoint: endpoint.clone(),
                command: Command::Run(Invocation::Exercise),
                color: ColorMode::Auto,
            }),
            Ok(())
        );
        assert_eq!(
            execute(&Parsed {
                endpoint,
                command: Command::Run(Invocation::SensorsIngest),
                color: ColorMode::Auto,
            }),
            Ok(())
        );
    }

    #[test]
    fn run_seed_executes_a_reachable_standard_free_project() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn seed(): Int = library.value();",
        )
        .expect("main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn value(): Int = 42;",
        )
        .expect("library source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        orna_repository_v1::initialize_repository(directory.path()).expect("runtime metadata");

        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Run(Invocation::Seed),
            color: ColorMode::Auto,
        };
        assert_eq!(execute(&parsed), Ok(()));
    }

    #[test]
    fn invoke_executes_a_qualified_reachable_pure_function() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use library; pub fn seed(): Int = library.value();",
        )
        .expect("main source");
        std::fs::write(
            directory.path().join("library.orna"),
            "pub fn value(): Int = 42;",
        )
        .expect("library source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Invoke("library.value".into()),
            color: ColorMode::Auto,
        };
        assert_eq!(execute(&parsed), Ok(()));
    }

    #[test]
    fn check_rejects_an_uncaptured_standard_module() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; pub fn run(): Int = std.math.increment(41);",
        )
        .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let error = check_project(&endpoint, false).expect_err("uncaptured standard module");
        assert_eq!(
            (error.code, error.exit, error.title.as_ref(), error.help),
            (
                "ORNA-S010-IMPORT",
                Exit::Target,
                "imported module is unavailable",
                "use a captured standard dependency or remove the import",
            )
        );
    }

    #[test]
    fn check_rejects_each_uncaptured_standard_module() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; use std.text; use std.json; use std.prelude as _; pub fn run() {}",
        )
        .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let error = check_project(&endpoint, false).expect_err("unbundled standard module");
        assert_eq!(
            (error.code, error.exit, error.title.as_ref(), error.help),
            (
                "ORNA-S010-IMPORT",
                Exit::Target,
                "imported module is unavailable",
                "use a captured standard dependency or remove the import",
            )
        );
    }

    #[test]
    fn invoke_rejects_an_uncaptured_standard_function() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; pub fn seed(): Bool = std.math.is_zero(std.math.decrement(1));",
        )
        .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Invoke("seed".into()),
            color: ColorMode::Auto,
        };
        let error = execute(&parsed).expect_err("uncaptured standard module");
        assert_eq!(
            (error.code, error.exit, error.title.as_ref(), error.help),
            (
                "ORNA-S010-IMPORT",
                Exit::Target,
                "imported module is unavailable",
                "use a captured standard dependency or remove the import",
            )
        );
    }

    #[test]
    fn invoke_rejects_composed_uncaptured_standard_math() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "use std.math; pub fn seed(): Int = std.math.clamp(std.math.max(2, 9), 3, 7);",
        )
        .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        let parsed = Parsed {
            endpoint: Endpoint::Path(directory.path().to_string_lossy().into_owned()),
            command: Command::Invoke("seed".into()),
            color: ColorMode::Auto,
        };
        let error = execute(&parsed).expect_err("uncaptured standard module");
        assert_eq!(
            (error.code, error.exit, error.title.as_ref(), error.help),
            (
                "ORNA-S010-IMPORT",
                Exit::Target,
                "imported module is unavailable",
                "use a captured standard dependency or remove the import",
            )
        );
    }

    #[test]
    fn check_rejects_semantic_errors_with_a_stable_cli_diagnostic() {
        let directory = tempfile::tempdir().expect("temporary project");
        std::fs::write(
            directory.path().join("main.orna"),
            "pub fn run(): Int = true;",
        )
        .expect("main source");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );

        let endpoint = Endpoint::Path(directory.path().to_string_lossy().into_owned());
        let error = check_project(&endpoint, false).expect_err("semantic error");
        let rendered = error.to_string();
        assert_eq!((error.code, error.exit), ("E2101", Exit::Target));
        assert!(rendered.contains("ORNA-S021-TYPE"));
        assert!(rendered.contains("static types are incompatible"));
        assert!(rendered.contains("fix the first reported source contract error"));
    }

    #[test]
    fn stream_failure_diagnostic_describes_delivery_scoped_rollback() {
        let diagnostic = Diagnostic::target(
            "E2200",
            "durable project stream delivery failed",
            "the failed delivery was rolled back; prior committed deliveries and their checkpoint progress remain; inspect the recorded failure before retrying the stream",
        );
        let rendered = diagnostic.to_string();

        assert!(rendered.contains("the failed delivery was rolled back"));
        assert!(
            rendered.contains("prior committed deliveries and their checkpoint progress remain")
        );
        assert!(rendered.contains("inspect the recorded failure before retrying the stream"));
        assert!(!rendered.contains("failed atomically"));
    }

    #[test]
    fn cancellation_mapping_retains_safe_identity_and_cancelled_exit() {
        let diagnostic = orna_foundation_v1::Diagnostic::new(
            orna_foundation_v1::SafeText::new("ORNA-LIST-STREAM-CANCELLED").unwrap(),
            orna_foundation_v1::DiagnosticSeverity::Error,
            orna_foundation_v1::SafeText::redacted(),
        )
        .unwrap();
        let mapped = cancellation_diagnostic(&diagnostic);

        assert_eq!(mapped.code, "ORNA-LIST-STREAM-CANCELLED");
        assert_eq!(mapped.exit, Exit::Cancelled);
        assert!(mapped.detail.is_none());
    }

    #[test]
    fn endpoints_reject_secrets() {
        assert_eq!(
            Endpoint::parse("orna://a@host/db")
                .expect_err("credentials")
                .code,
            "E1005"
        );
        assert_eq!(
            Endpoint::parse("orna://host")
                .expect_err("no database")
                .code,
            "E1004"
        );
    }

    #[test]
    fn endpoint_parsing_matches_the_closed_client_transport_grammar() {
        for (endpoint, code) in [
            ("postgres://host/db", "E1004"),
            ("orna+unix://relative.sock", "E1004"),
            ("orna+unix:///tmp/orna.sock?debug=1", "E1005"),
            ("orna://local:7443/default", "E1004"),
            ("orna://db.example.test:0/work", "E1004"),
            ("orna://db.example.test/%zz", "E1004"),
            ("orna://[::1/work", "E1004"),
            ("orna://db.example.test/work\u{1f}", "E1004"),
        ] {
            let error = Endpoint::parse(endpoint).expect_err(endpoint);
            assert_eq!(error.code, code, "{endpoint}");
            assert_eq!(error.exit, Exit::Usage, "{endpoint}");
        }
        assert_eq!(
            Endpoint::parse("orna+unix:///tmp/orna.sock").expect("absolute Unix socket"),
            Endpoint::UnixSocket("/tmp/orna.sock".into())
        );
        assert_eq!(
            Endpoint::parse("orna://[::1]:7443/team%2Fwork").expect("IPv6 TLS endpoint"),
            Endpoint::RemoteTls("orna://[::1]:7443/team%2Fwork".into())
        );
    }
    #[test]
    fn workflow_is_deterministic_and_idempotent() {
        let mut state = ReferenceState::default();
        let first = plan(&state).expect("plan");
        assert_eq!(
            first.steps,
            vec![Step::Init, Step::Seed, Step::Exercise, Step::Sensors]
        );
        apply(&mut state, &first).expect("apply");
        assert_eq!(
            (
                state.checkpoint,
                state.readings.len(),
                state.stock.get(&("north", "pencil"))
            ),
            (3, 3, Some(&9))
        );
        let repeat = plan(&state).expect("replan");
        assert!(
            repeat
                .steps
                .iter()
                .all(|step| matches!(step, Step::Noop(_)))
        );
    }
    #[test]
    fn invalid_workflow_rolls_back_candidate() {
        let mut state = ReferenceState {
            initialized: true,
            ..ReferenceState::default()
        };
        let before = state.clone();
        let invalid = Plan {
            steps: vec![Step::Exercise],
        };
        assert_eq!(
            apply(&mut state, &invalid).expect_err("invalid").code,
            "E2004"
        );
        assert_eq!(state, before);
    }
    #[test]
    fn direct_seed_preserves_reference_duplicate_failure() {
        let mut state = ReferenceState {
            initialized: true,
            ..ReferenceState::default()
        };
        seed(&mut state).expect("first seed");
        assert_eq!(seed(&mut state).expect_err("duplicate").code, "E2003");
    }
    #[derive(Default)]
    struct Recording {
        calls: Vec<String>,
        terminal: BTreeSet<u64>,
        fail_cancel: BTreeSet<u64>,
        fail_drain: bool,
    }
    impl SessionAdapter for Recording {
        fn is_terminal(&mut self, child: u64) -> Result<bool, Diagnostic> {
            self.calls.push(format!("terminal:{child}"));
            Ok(self.terminal.contains(&child))
        }
        fn cancel(&mut self, child: u64) -> Result<(), Diagnostic> {
            self.calls.push(format!("cancel:{child}"));
            if self.fail_cancel.contains(&child) {
                return Err(Diagnostic::target(
                    "E1203",
                    "session child cancellation is incomplete",
                    "retry close after child cancellation becomes available",
                ));
            }
            Ok(())
        }
        fn drain_terminal(&mut self) -> Result<(), Diagnostic> {
            self.calls.push("drain".into());
            if self.fail_drain {
                return Err(Diagnostic::target(
                    "E1202",
                    "session cleanup is incomplete",
                    "retry close after child cleanup completes",
                ));
            }
            Ok(())
        }
        fn close_transport(&mut self) -> Result<(), Diagnostic> {
            self.calls.push("close".into());
            Ok(())
        }
    }
    #[test]
    fn close_cancels_only_owned_unfinished_children_then_drains_and_is_idempotent() {
        let mut session = Session::new();
        session.own(9).expect("open");
        session.own(2).expect("open");
        let mut adapter = Recording {
            terminal: BTreeSet::from([9]),
            ..Recording::default()
        };
        assert_eq!(session.close(&mut adapter), Ok(true));
        assert_eq!(
            adapter.calls,
            ["terminal:2", "cancel:2", "terminal:9", "drain", "close"]
        );
        assert_eq!(session.close(&mut adapter), Ok(false));
        assert_eq!(adapter.calls.len(), 5);
    }
    #[test]
    fn failed_close_keeps_the_session_closed_to_new_children_until_cleanup_retries() {
        let mut session = Session::new();
        session.own(2).expect("open");
        let mut adapter = Recording {
            fail_drain: true,
            ..Recording::default()
        };

        assert_eq!(
            session.close(&mut adapter).expect_err("drain failure").code,
            "E1202"
        );
        assert_eq!(session.own(3).expect_err("admission sealed").code, "E1201");

        adapter.fail_drain = false;
        assert_eq!(session.close(&mut adapter), Ok(true));
        assert_eq!(
            adapter.calls,
            [
                "terminal:2",
                "cancel:2",
                "drain",
                "terminal:2",
                "cancel:2",
                "drain",
                "close"
            ]
        );
    }
    #[test]
    fn failed_child_cancellation_still_attempts_every_owned_child() {
        let mut session = Session::new();
        session.own(2).expect("open");
        session.own(3).expect("open");
        let mut adapter = Recording {
            fail_cancel: BTreeSet::from([2]),
            ..Recording::default()
        };

        assert_eq!(
            session
                .close(&mut adapter)
                .expect_err("first cancellation fails")
                .code,
            "E1203"
        );
        assert_eq!(
            adapter.calls,
            ["terminal:2", "cancel:2", "terminal:3", "cancel:3"]
        );
        assert_eq!(session.own(4).expect_err("admission sealed").code, "E1201");

        adapter.fail_cancel.clear();
        assert_eq!(session.close(&mut adapter), Ok(true));
        assert_eq!(
            adapter.calls,
            [
                "terminal:2",
                "cancel:2",
                "terminal:3",
                "cancel:3",
                "terminal:2",
                "cancel:2",
                "terminal:3",
                "cancel:3",
                "drain",
                "close",
            ]
        );
    }
    #[test]
    fn identity_is_declared_for_adapter_integration() {
        assert_eq!(SENSOR_SOURCE_IDENTITY, "example:sensors:v1");
    }
    #[test]
    fn init_parse_defaults_to_the_current_directory_target() {
        assert_eq!(
            parse_cli(&["init".into()]).expect("parses").command,
            Command::Init(None)
        );
    }

    #[test]
    fn repository_initialization_errors_keep_codes_and_give_specific_remedies() {
        for (error, code, help) in [
            (
                orna_repository_v1::RepositoryInitError::GitUnavailable,
                "ORNA-REPO-INIT-001",
                "install or enable Git, then retry `orna init`",
            ),
            (
                orna_repository_v1::RepositoryInitError::GitOperationFailed,
                "ORNA-REPO-INIT-002",
                "check that Git can initialize the target directory, then retry `orna init`",
            ),
            (
                orna_repository_v1::RepositoryInitError::LocalStateUnavailable,
                "ORNA-REPO-INIT-003",
                "check local filesystem access for the target directory, then retry `orna init`",
            ),
            (
                orna_repository_v1::RepositoryInitError::UnsafeMetadataPath,
                "ORNA-REPO-INIT-004",
                "replace unsafe metadata links or non-regular entries before retrying `orna init`",
            ),
            (
                orna_repository_v1::RepositoryInitError::MetadataIncomplete,
                "ORNA-REPO-INIT-005",
                "restore complete metadata from a valid snapshot or initialize a new repository",
            ),
            (
                orna_repository_v1::RepositoryInitError::MetadataMalformed,
                "ORNA-REPO-INIT-006",
                "restore valid metadata from a compatible repository snapshot before retrying",
            ),
            (
                orna_repository_v1::RepositoryInitError::MetadataUnsupported,
                "ORNA-REPO-INIT-007",
                "use an Orna version that supports this repository format",
            ),
            (
                orna_repository_v1::RepositoryInitError::MetadataChanged,
                "ORNA-REPO-INIT-008",
                "ensure no other process changes repository metadata, then retry `orna init`",
            ),
            (
                orna_repository_v1::RepositoryInitError::RepositoryBusy,
                "ORNA-REPO-INIT-009",
                "wait for the other initialization to finish, then retry `orna init`",
            ),
            (
                orna_repository_v1::RepositoryInitError::PlatformUnsupported,
                "ORNA-REPO-INIT-010",
                "use a supported platform to initialize this repository",
            ),
        ] {
            let diagnostic = repository_init_diagnostic(&error);
            assert_eq!(diagnostic.code, code);
            assert_eq!(diagnostic.help, help);
        }
    }
    #[test]
    fn fetch_command_consumes_git_transport_without_mutating_head_or_worktree() {
        let root = tempfile::tempdir().expect("fetch fixture");
        let remote = root.path().join("remote.git");
        let local = root.path().join("local");
        let updater = root.path().join("updater");
        let git = |directory: &std::path::Path, arguments: &[&str]| -> String {
            let output = std::process::Command::new("git")
                .current_dir(directory)
                .args(arguments)
                .output()
                .expect("git starts");
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("git output is UTF-8")
                .trim()
                .to_owned()
        };
        std::fs::create_dir_all(&local).expect("local directory");
        git(root.path(), &["init", "--bare", "remote.git"]);
        git(&local, &["init", "-b", "main"]);
        git(&local, &["config", "user.email", "kieran@drewett.dev"]);
        git(&local, &["config", "user.name", "kierandrewett"]);
        std::fs::write(local.join("tracked.txt"), "base\n").expect("base file");
        git(&local, &["add", "tracked.txt"]);
        git(&local, &["commit", "-m", "base"]);
        let initial = git(&local, &["rev-parse", "HEAD"]);
        let remote_path = remote.to_str().expect("remote path");
        git(&local, &["remote", "add", "origin", remote_path]);
        git(&local, &["push", "origin", "HEAD:refs/heads/main"]);
        git(
            root.path(),
            &["--git-dir", "remote.git", "symbolic-ref", "HEAD", "refs/heads/main"],
        );
        git(root.path(), &["clone", "--branch", "main", remote_path, "updater"]);
        assert_eq!(git(&updater, &["rev-parse", "HEAD"]), initial);
        git(&updater, &["config", "user.email", "kieran@drewett.dev"]);
        git(&updater, &["config", "user.name", "kierandrewett"]);
        std::fs::write(updater.join("tracked.txt"), "advanced\n").expect("advanced file");
        git(&updater, &["add", "tracked.txt"]);
        git(&updater, &["commit", "-m", "advance"]);
        let advanced = git(&updater, &["rev-parse", "HEAD"]);
        git(&updater, &["push", "origin", "HEAD:refs/heads/main"]);

        let endpoint = Endpoint::Path(local.display().to_string());
        execute(&Parsed {
            endpoint,
            command: Command::Fetch {
                remote: "origin".into(),
                branch: "main".into(),
            },
            color: ColorMode::Auto,
        })
        .expect("fetch command");
        assert_eq!(git(&local, &["rev-parse", "HEAD"]), initial);
        assert_eq!(
            git(&local, &["rev-parse", "refs/remotes/origin/main"]),
            advanced
        );
        assert!(git(&local, &["status", "--porcelain"]).is_empty());
    }
}

#[cfg(test)]
mod repl_request_effect_tests {
    use super::*;
    use crate::format3::format3_repository_for_table;
    use orna_runtime_v1::{FaultPoint, RecoveryDisposition, RuntimeError};

    const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";
    const INSERT_LONDON: &str = r#"main.Music.insert({ id: "london", title: "Live in London" })"#;

    struct FailAt(FaultPoint);

    impl FaultInjector for FailAt {
        fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
            if point == self.0 {
                Err(RuntimeError::FaultInjected(point))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn committed_repl_insert_finishes_its_request_row() {
        let (_directory, repository, _relation) =
            format3_repository_for_table(MUSIC_SOURCE, "Music");
        let ReplRuntime {
            repository,
            mut session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        let mut transactions = transactions.expect("repl transactions");
        assert!(
            run_repl_input(
                repository.as_ref(),
                Some(&mut transactions),
                &mut session,
                INSERT_LONDON,
            )
            .is_ok(),
            "the REPL insert commits"
        );
        let state = transactions.state.as_ref().expect("runtime state");
        let running = transactions
            .runtime
            .block_on(state.running_requests())
            .expect("running requests");
        assert!(running.is_empty(), "a committed REPL insert leaves no running request");
    }

    #[test]
    fn sys_dispatch_observes_request_row_already_running() {
        let (directory, repository, _relation) =
            format3_repository_for_table(MUSIC_SOURCE, "Music");
        let ReplRuntime {
            repository,
            mut session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        let mut transactions = transactions.expect("repl transactions");
        FIRST_SYS_DISPATCH_RUNNING.with(|first| first.set(None));
        std::fs::write(directory.path().join("probe.bin"), b"probe").expect("probe file");
        let input = format!(
            "sys.blob.capture_file({:?}, \"probe.bin\", 65536)",
            directory.path().to_string_lossy().as_ref()
        );
        run_repl_input(
            repository.as_ref(),
            Some(&mut transactions),
            &mut session,
            &input,
        )
        .expect("the blob capture succeeds");
        let running_at_first_dispatch = FIRST_SYS_DISPATCH_RUNNING.with(std::cell::Cell::get);
        assert_eq!(
            running_at_first_dispatch,
            Some(1),
            "the request row is reserved before the first sys binding runs"
        );
    }

    /// Runs an insert whose commit fails at `point` and asserts the uncertain
    /// diagnostic and the still-running request row, then that fenced recovery
    /// settles it without proving a rollback.
    fn assert_commit_fault_reports_uncertain(point: FaultPoint) {
        let (_directory, repository, _relation) =
            format3_repository_for_table(MUSIC_SOURCE, "Music");
        let ReplRuntime {
            repository,
            mut session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        let mut transactions = transactions.expect("repl transactions");
        transactions.faults = Arc::new(FailAt(point));
        let failure = run_repl_input(
            repository.as_ref(),
            Some(&mut transactions),
            &mut session,
            INSERT_LONDON,
        )
        .err()
        .unwrap_or_else(|| panic!("{point:?}: the injected fault fails the REPL request"));
        assert_eq!(failure.code, "ORNA-REPL-EFFECT-UNCERTAIN", "{point:?}");

        let state = transactions.state.as_ref().expect("runtime state");
        let running = transactions
            .runtime
            .block_on(state.running_requests())
            .expect("running requests");
        assert_eq!(
            running.len(),
            1,
            "{point:?}: the failed commit leaves its own row running"
        );
        let row = &running[0];
        let lost = transactions
            .runtime
            .block_on(state.request_owner(row.identity, row.fingerprint))
            .expect("request owner")
            .expect("the running row has an owner");
        let fence = transactions
            .runtime
            .block_on(state.recover_abandoned(lost.owner_id, [0x52; 16]))
            .expect("fence the abandoned writer");
        let rollback = TerminalOutcome::new(b"orna.repl.rollback.v1".to_vec()).expect("outcome");
        let uncertain = TerminalOutcome::new(b"orna.repl.uncertain.v1".to_vec()).expect("outcome");
        let recovered = transactions
            .runtime
            .block_on(state.recover_running_request_with_outcomes(
                row.identity,
                row.fingerprint,
                lost,
                fence,
                rollback,
                uncertain,
            ))
            .expect("recover the running request");
        assert_ne!(
            recovered.disposition,
            RecoveryDisposition::RollbackProven,
            "{point:?}: a REPL activation declares ExternalPossible effects, so a rollback is never proven"
        );
        assert_eq!(
            recovered.disposition,
            RecoveryDisposition::ExternalEffectsUncertain,
            "{point:?}"
        );
        let after = transactions
            .runtime
            .block_on(state.running_requests())
            .expect("running requests after recovery");
        assert!(
            after.is_empty(),
            "{point:?}: recovery settles the fenced request row"
        );
    }

    #[test]
    fn repl_commit_fault_reports_uncertain_and_keeps_its_request_row_running() {
        for point in [
            FaultPoint::BeforeTableWrite,
            FaultPoint::AfterTableWrite,
            FaultPoint::BeforeTerminalClaim,
            FaultPoint::AfterTerminalClaim,
        ] {
            assert_commit_fault_reports_uncertain(point);
        }
    }

    #[test]
    fn repl_pure_evaluation_failure_keeps_its_evaluator_code() {
        let (_directory, repository, _relation) =
            format3_repository_for_table(MUSIC_SOURCE, "Music");
        let ReplRuntime {
            repository,
            mut session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        let mut transactions = transactions.expect("repl transactions");
        let failure = run_repl_input(
            repository.as_ref(),
            Some(&mut transactions),
            &mut session,
            "1 / 0",
        )
        .err()
        .expect("the division by zero fails");
        assert_eq!(
            failure.code, "ORNA-EVAL-DIVIDE-BY-ZERO",
            "a failure with no external effect keeps the evaluator code"
        );
    }

    #[test]
    fn repl_effect_uncertain_failure_keeps_the_evaluation_cause() {
        let failure = repl_evaluation_failure(ReplEvaluationFailure {
            error: ApplicationError::Evaluation("ORNA-EVAL-DIVIDE-BY-ZERO".to_owned()),
            external_effects_possible: true,
        });
        assert_eq!(failure.code, REPL_EFFECT_UNCERTAIN);
        let detail = failure
            .detail
            .expect("an uncertain outcome carries its cause in the detail");
        assert_eq!(detail.code, REPL_EFFECT_UNCERTAIN);
        assert!(
            detail.message.contains("ORNA-EVAL-DIVIDE-BY-ZERO"),
            "the detail names the evaluation cause: {}",
            detail.message
        );
    }

    #[test]
    fn repl_effect_free_failure_keeps_its_own_code_without_detail() {
        let failure = repl_evaluation_failure(ReplEvaluationFailure {
            error: ApplicationError::Evaluation("ORNA-EVAL-DIVIDE-BY-ZERO".to_owned()),
            external_effects_possible: false,
        });
        assert_eq!(failure.code, "ORNA-EVAL-DIVIDE-BY-ZERO");
        assert!(failure.detail.is_none());
    }

    #[test]
    fn repl_effectful_failure_before_any_external_binding_leaves_no_running_row() {
        // `n` is an Int, so `1 / 0` type-checks and the insert stages as an
        // effectful activation that fails during evaluation, before any binding.
        let (_directory, repository, _relation) =
            format3_repository_for_table("pub table Counter(id: Str) { n: Int, }\n", "Counter");
        let ReplRuntime {
            repository,
            mut session,
            transactions,
        } = repl_runtime(Some(repository)).expect("repl runtime");
        let mut transactions = transactions.expect("repl transactions");
        let failure = run_repl_input(
            repository.as_ref(),
            Some(&mut transactions),
            &mut session,
            r#"main.Counter.insert({ id: "zero", n: 1 / 0 })"#,
        )
        .err()
        .expect("the division by zero fails before any write");
        assert_ne!(
            failure.code, "ORNA-REPL-EFFECT-UNCERTAIN",
            "no external binding ran, so the failure is not uncertain"
        );
        let state = transactions
            .state
            .as_ref()
            .expect("the effectful input opened the runtime state");
        let running = transactions
            .runtime
            .block_on(state.running_requests())
            .expect("running requests");
        assert!(
            running.is_empty(),
            "a failure before any external effect cancels its reserved row"
        );
    }
}
