#![allow(
    dead_code,
    reason = "The bounded binary exposes planning and session seams for a later integration adapter."
)]

mod repl;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, BufReader, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

use orna_conformance_v1::{
    AdmittedReplSession, BoundedEvaluator, DurableTransactionalEvaluator, ProjectEnvironment,
    ProjectExpectations, ProjectUnit, ReplError, RuntimeEvaluator, SourceUnit, StageOutcome,
};
use orna_application_v1::ApplicationAuthority;
use orna_evaluator_v1::{Environment, Limits};
use orna_foundation_v1::{OvbRaw, Value};
use orna_runtime_v1::{RunObservationStatus, RuntimeIdentity, RuntimeState};

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
    title: &'static str,
    help: &'static str,
    exit: Exit,
    detail: Option<String>,
}
impl Diagnostic {
    const fn usage(code: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            code,
            title,
            help,
            exit: Exit::Usage,
            detail: None,
        }
    }
    const fn target(code: &'static str, title: &'static str, help: &'static str) -> Self {
        Self {
            code,
            title,
            help,
            exit: Exit::Target,
            detail: None,
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
            title,
            help,
            exit: Exit::Target,
            detail: Some(detail.into()),
        }
    }
    const fn unavailable(title: &'static str, help: &'static str) -> Self {
        Self::target("E2000", title, help)
    }
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
        title: "operation was cancelled",
        help: "inspect the durable operation state before retrying",
        exit: Exit::Cancelled,
        detail: (code == "E2200").then(|| format!("cancellation code: {}", diagnostic.code())),
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.title)?;
        if let Some(detail) = &self.detail {
            write!(f, "\n  {detail}")?;
        }
        write!(f, "\nhelp: {}", self.help)
    }
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
#[derive(Clone, Debug, Eq, PartialEq)]
enum Invocation {
    Seed,
    Exercise,
    SensorsIngest,
    LibraryLend { book_id: String, borrower: String },
    ProjectFunction(String),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusFormat {
    Human,
    Porcelain,
    Short,
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Repl(Option<String>),
    Init(Option<PathBuf>),
    Status { format: StatusFormat },
    Fetch { remote: String, branch: String },
    Check,
    Explain(String),
    Invoke(String),
    Run(Invocation),
    Help,
    Version,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Parsed {
    endpoint: Endpoint,
    command: Command,
}
#[allow(
    clippy::too_many_lines,
    reason = "the command grammar deliberately keeps validation precedence in one auditable parser"
)]
fn parse_cli(arguments: &[String]) -> Result<Parsed, Diagnostic> {
    let mut endpoint = Endpoint::ManagedLocal;
    let mut has_explicit_endpoint = false;
    let mut words = arguments.iter().map(String::as_str).peekable();
    while matches!(words.peek(), Some(&"--db")) {
        words.next();
        has_explicit_endpoint = true;
        endpoint = Endpoint::parse(words.next().ok_or_else(|| {
            Diagnostic::usage(
                "E1001",
                "option `--db` needs a value",
                "supply an endpoint after `--db`",
            )
        })?)?;
    }
    let command = match words.next() {
        None => Command::Repl(None),
        Some("repl") => Command::Repl(words.next().map(str::to_owned)),
        Some("init") => {
            let first = words.next();
            let literal_target = first == Some("--");
            let target = if literal_target { words.next() } else { first }.map(PathBuf::from);
            if !literal_target
                && let Some(target) = target.as_deref()
                && target.as_os_str().to_string_lossy().starts_with('-')
            {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`init` option is not supported",
                    "use `init` or `init DIRECTORY` without Git options",
                ));
            }
            if words.next().is_some() {
                return Err(Diagnostic::usage(
                    "E1003",
                    "`init` accepts at most one directory",
                    "use `init` or `init DIRECTORY`",
                ));
            }
            if has_explicit_endpoint {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`init` does not accept `--db`",
                    "use `init` or `init DIRECTORY` to choose a local repository",
                ));
            }
            Command::Init(target)
        }
        Some("status") => match words.next() {
            Some("--porcelain") => Command::Status {
                format: StatusFormat::Porcelain,
            },
            Some("--short") => Command::Status {
                format: StatusFormat::Short,
            },
            Some(_) => {
                return Err(Diagnostic::usage(
                    "E1002",
                    "`status` supports no option, `--porcelain`, or `--short`",
                    "use `status`, `status --porcelain`, or `status --short`",
                ));
            }
            None => Command::Status {
                format: StatusFormat::Human,
            },
        },
        Some("fetch") => Command::Fetch {
            remote: words.next().unwrap_or("origin").to_owned(),
            branch: words.next().unwrap_or("main").to_owned(),
        },
        Some("explain") => Command::Explain(
            words
                .next()
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`explain` needs a diagnostic code",
                        "supply a diagnostic code after `explain`, such as `ORNA-S010-IMPORT`",
                    )
                })?
                .to_owned(),
        ),
        Some("check") => Command::Check,
        Some("invoke") => Command::Invoke(
            words
                .next()
                .ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`invoke` needs a function target",
                        "supply a reachable zero-argument pure function name after `invoke`",
                    )
                })?
                .to_owned(),
        ),
        Some("--help" | "-h" | "help") => Command::Help,
        Some("--version" | "-V") => Command::Version,
        Some("run") => match words.next() {
            Some("seed") => Command::Run(Invocation::Seed),
            Some("exercise") => Command::Run(Invocation::Exercise),
            Some("sensors.ingest") => Command::Run(Invocation::SensorsIngest),
            Some("library.lend") => {
                let book_id = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`run library.lend` needs a book ID",
                        "supply the book ID and borrower after `run library.lend`",
                    )
                })?;
                let borrower = words.next().ok_or_else(|| {
                    Diagnostic::usage(
                        "E1001",
                        "`run library.lend` needs a borrower",
                        "supply the book ID and borrower after `run library.lend`",
                    )
                })?;
                Command::Run(Invocation::LibraryLend {
                    book_id: book_id.to_owned(),
                    borrower: borrower.to_owned(),
                })
            }
            Some(target) => Command::Run(Invocation::ProjectFunction(target.to_owned())),
            None => Command::Run(Invocation::ProjectFunction("main.main".into())),
        },
        Some(_) => {
            return Err(Diagnostic::usage(
                "E1002",
                "unknown Orna command",
                "use `--help` to list supported commands",
            ));
        }
    };
    if words.next().is_some() {
        return Err(Diagnostic::usage(
            "E1003",
            "command has unexpected arguments",
            "this bounded slice accepts no extra arguments",
        ));
    }
    Ok(Parsed { endpoint, command })
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

fn run_status(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run the command inside a Git worktree or provide a local project path",
        )
    })?;
    let state = repository.worktree_state().map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be read",
            "check that Git can read the local worktree, then retry `status --porcelain`",
        )
    })?;
    io::stdout()
        .write_all(state.as_porcelain_v2_z())
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "local Git worktree status could not be written",
                "retry `status --porcelain`",
            )
        })?;
    Ok(())
}

fn run_status_human(endpoint: &Endpoint) -> Result<(), Diagnostic> {
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
        .arg("status")
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
    io::stdout().write_all(&output.stdout).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree status could not be written",
            "retry `status`",
        )
    })?;
    Ok(())
}

fn run_status_short(endpoint: &Endpoint) -> Result<(), Diagnostic> {
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
        .args(["status", "--short"])
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
    io::stdout().write_all(&output.stdout).map_err(|_| {
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
    orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(|_| {
            Diagnostic::target(
                "E2100",
                "project source could not be loaded",
                "fix the project module graph and source boundaries, then run check again",
            )
        })
}

fn reject_uncaptured_standard_modules(
    project: &orna_project_v1::LoadedProject,
) -> Result<(), Diagnostic> {
    if project.standard_modules().is_empty() {
        return Ok(());
    }
    Err(Diagnostic::target(
        "ORNA-S010-IMPORT",
        "imported module is unavailable",
        "use a captured standard dependency or remove the import",
    ))
}

fn semantic_catalogue() -> orna_semantic_v1::Catalogue {
    orna_semantic_v1::Catalogue::authoritative_core()
}

fn check_project(endpoint: &Endpoint) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue();
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if analysis.is_ok() {
        println!("project valid");
        Ok(())
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
    let modules: Vec<SourceUnit> = project
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
                intrinsics: "Orna 1.0.0 core".into(),
                stdlib: None,
                initial_tables: "empty".into(),
            },
            steps: Vec::new(),
            negative_cases: Vec::new(),
        },
    }
}

fn runtime_identity(
    repository: &orna_repository_v1::Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), Diagnostic> {
    let metadata = orna_repository_v1::inspect_metadata(repository).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "repository runtime metadata is unavailable",
            "initialize the Orna repository before running a durable project invocation",
        )
    })?;
    let Some(metadata) = metadata else {
        return Err(Diagnostic::target(
            "E2200",
            "repository runtime metadata is unavailable",
            "initialize the Orna repository before running a durable project invocation",
        ));
    };

    let database_id = *metadata.database_id().as_bytes();
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }

    let mut initial_digest = [0; 32];
    initial_digest[..16].copy_from_slice(&database_id);
    initial_digest[16..].copy_from_slice(&repository_id);
    if initial_digest == [0; 32] {
        initial_digest[0] = 1;
    }
    Ok((
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        initial_digest,
    ))
}

fn invocation_owner_id(identity: RuntimeIdentity) -> [u8; 16] {
    // The embedded runtime persists one local writer lease in the worktree's
    // private state. Reusing the repository-derived owner lets a later CLI
    // process resume that same local owner without stealing an unknown lease.
    identity.repository_id
}

fn run_project_invocation(endpoint: &Endpoint, root_entry: &str) -> Result<(), Diagnostic> {
    run_project_invocation_with_arguments(endpoint, root_entry, &Environment::new())
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
fn run_public_project_function(endpoint: &Endpoint, target: &str) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue();
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    if !public_project_function(&analysis, target) {
        return Err(Diagnostic::unavailable(
            "durable project invocation is not available",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        ));
    }
    let execution = execution_project(&project);
    if DurableTransactionalEvaluator::default().project_stream_root_admitted(&execution, target) {
        return run_project_stream_invocation_with_project(endpoint, target, &execution);
    }
    run_project_invocation(endpoint, target)
}

fn run_project_stream_invocation(endpoint: &Endpoint, root_entry: &str) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue();
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    let execution = execution_project(&project);
    run_project_stream_invocation_with_project(endpoint, root_entry, &execution)
}

fn run_project_invocation_with_arguments(
    endpoint: &Endpoint,
    root_entry: &str,
    arguments: &Environment,
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
    let catalogue = semantic_catalogue();
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run the project again",
        ));
    }
    let (identity, initial_digest) = runtime_identity(&repository)?;
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
    let project = execution_project(&project);
    let owner_id = invocation_owner_id(identity);
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
            DurableTransactionalEvaluator::default().execute_project_request_with_arguments(
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
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "durable project invocation failed",
                "retry the project transaction after checking its durable state",
            )
        })?;
    match outcome {
        StageOutcome::Passed => {
            println!("invocation completed");
            Ok(())
        }
        StageOutcome::Failed(_) => Err(Diagnostic::target(
            "E2200",
            "durable project invocation was rejected",
            "the project transaction failed atomically; no partial changes were committed",
        )),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { reason }
            if arguments.is_empty()
                && reason
                    == "project transaction admission does not run stream roots; use the explicit finite-list stream seam" =>
        {
            run_project_stream_invocation(endpoint, root_entry)
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
    let owner_id = invocation_owner_id(identity);
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
            DurableTransactionalEvaluator::default().execute_project_stream(
                &repository,
                identity,
                owner_id,
                initial_digest,
                project,
                root_entry,
            ),
        )
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "durable project invocation failed",
                "retry the project transaction after checking its durable state",
            )
        })?;
    match outcome {
        StageOutcome::Passed => {
            println!("invocation completed");
            Ok(())
        }
        StageOutcome::Failed(_) => Err(Diagnostic::target(
            "E2200",
            "durable project stream delivery failed",
            "the failed delivery was rolled back; prior committed deliveries and their checkpoint progress remain; inspect the recorded failure before retrying the stream",
        )),
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "durable project stream invocation is not available",
            "use a supported finite-list stream root",
        )),
    }
}

fn run_pure_invocation(endpoint: &Endpoint, target: &str) -> Result<(), Diagnostic> {
    let project = load_project(endpoint)?;
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
            let authority =
                ApplicationAuthority::new(semantic_catalogue(), Limits::default());
            let admitted = authority
                .admit_module(identity.logical_path(), module.source.clone(), function)
                .map_err(|error| {
                    Diagnostic::target_with_detail(
                        "E2101",
                        "project semantic analysis failed",
                        "fix the first reported source contract error, then run check again",
                        error.to_string(),
                    )
                })?;
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
            println!("invocation completed");
            return Ok(());
        }
    }
    let catalogue = semantic_catalogue();
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error, then run check again",
        ));
    }

    let mut evaluator = BoundedEvaluator::default();
    match evaluator.evaluate_project(&execution_project(&project)) {
        StageOutcome::Passed => {}
        StageOutcome::Cancelled(diagnostic) => return Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Failed(_) | StageOutcome::Skipped { .. } => {
            return Err(Diagnostic::unavailable(
                "one-shot invocation requires an executable function-only project",
                "use reachable modules containing only pure functions; tables, effects, and streams require the integrated runtime",
            ));
        }
    }
    match evaluator.invoke(target) {
        StageOutcome::Passed => {
            println!("invocation completed");
            Ok(())
        }
        StageOutcome::Cancelled(diagnostic) => Err(cancellation_diagnostic(&diagnostic)),
        StageOutcome::Failed(_) | StageOutcome::Skipped { .. } => Err(Diagnostic::unavailable(
            "requested invocation is not available",
            "define a reachable zero-argument pure function with the requested name",
        )),
    }
}

fn repl_session(endpoint: &Endpoint) -> Result<AdmittedReplSession, Diagnostic> {
    let project_context = match endpoint {
        Endpoint::ManagedLocal => orna_repository_v1::Repository::discover(".").is_ok(),
        Endpoint::Path(_) => true,
        // Snapshot selectors are deliberately repository-backed. Keep a
        // non-local REPL core-only so the loader can reject `:at` uniformly as
        // ORNA-REPL-AT instead of failing before the command loop starts.
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => false,
    };
    if project_context {
        let project = load_project_without_standard_rejection(endpoint)?;
        return AdmittedReplSession::from_loaded_project(&project, [], Limits::default())
            .map_err(|error| repl_session_error(&error));
    }
    Ok(AdmittedReplSession::new(Limits::default()))
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
        Diagnostic::target(
            "E2200",
            "project REPL session is not available",
            "use a project supported by the configured REPL runtime",
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
    endpoint: &'a Endpoint,
}

impl ReplSnapshotSessionLoader<'_> {
    fn repository(&self) -> Result<orna_repository_v1::Repository, ()> {
        let path = local_project_path(self.endpoint).map_err(|_| ())?;
        orna_repository_v1::Repository::discover(path).map_err(|_| ())
    }

    fn admit(project: &orna_project_v1::LoadedProject) -> Result<AdmittedReplSession, ()> {
        // Keep the existing standard-import boundary intact: no host standard
        // sources are supplied, so uncaptured imports remain inadmissible.
        AdmittedReplSession::from_loaded_project(project, [], Limits::default()).map_err(|_| ())
    }
}

impl repl::SnapshotSessionLoader for ReplSnapshotSessionLoader<'_> {
    type Error = ();

    fn load_snapshot(
        &self,
        target: repl::SnapshotTarget,
    ) -> Result<AdmittedReplSession, Self::Error> {
        match target {
            repl::SnapshotTarget::Cwd => {
                let project =
                    load_project_without_standard_rejection(self.endpoint).map_err(|_| ())?;
                Self::admit(&project)
            }
            repl::SnapshotTarget::Head => {
                let repository = self.repository()?;
                let commit = repository.head().map_err(|_| ())?.ok_or(())?;
                let project = orna_project_v1::ProjectLoader::default()
                    .load_committed_snapshot(&repository, &commit)
                    .map_err(|_| ())?;
                Self::admit(&project)
            }
            repl::SnapshotTarget::Ref(reference) => {
                let repository = self.repository()?;
                let commit = repository.resolve_snapshot(&reference).map_err(|_| ())?;
                let project = orna_project_v1::ProjectLoader::default()
                    .load_committed_snapshot(&repository, &commit)
                    .map_err(|_| ())?;
                Self::admit(&project)
            }
        }
    }
}

fn run_repl_submission<W: std::io::Write>(
    session: &mut AdmittedReplSession,
    source: &str,
    writer: &mut W,
) -> Result<(), Diagnostic> {
    if source.trim() == ":quit" {
        return Ok(());
    }
    match session.submit(source) {
        Ok(Some(value)) => {
            writeln!(writer, "{}", repl::inspect(&value)).map_err(|_| {
                Diagnostic::target(
                    "E2200",
                    "REPL console I/O failed",
                    "check the terminal input and output streams, then start a new session",
                )
            })?;
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => {
            writeln!(writer, "error[{}]", error.code()).map_err(|_| {
                Diagnostic::target(
                    "E2200",
                    "REPL console I/O failed",
                    "check the terminal input and output streams, then start a new session",
                )
            })?;
            Err(Diagnostic::unavailable(
                "REPL submission failed",
                "use source supported by the current evaluator session",
            ))
        }
    }
}

fn run_repl(endpoint: &Endpoint, expression: Option<&str>) -> Result<(), Diagnostic> {
    if matches!(endpoint, Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_)) {
        return Err(Diagnostic::target(
            "E2100",
            "remote REPL sessions are unavailable",
            "use a local Git worktree until the Orna transport is available",
        ));
    }
    let mut session = repl_session(endpoint)?;
    if let Some(source) = expression {
        return run_repl_submission(&mut session, source, &mut io::stdout().lock());
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let loader = ReplSnapshotSessionLoader { endpoint };
    repl::run_with_snapshot_loader(
        &mut BufReader::new(stdin.lock()),
        &mut stdout.lock(),
        &mut session,
        &loader,
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "REPL console I/O failed",
            "check the terminal input and output streams, then start a new session",
        )
    })
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

fn initialize_repository(target: Option<&std::path::Path>) -> Result<(), Diagnostic> {
    let target = target.unwrap_or_else(|| std::path::Path::new("."));
    orna_repository_v1::initialize_repository(target)
        .map_err(|error| repository_init_diagnostic(&error))?;
    println!("initialized Orna repository");
    Ok(())
}
fn run_fetch(endpoint: &Endpoint, remote: &str, branch: &str) -> Result<(), Diagnostic> {
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
    let request = orna_repository_v1::FetchRequest::new(remote.to_owned(), [requested], [])
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2100",
                "fetch request is invalid",
                "supply a configured remote and valid branch",
                error.to_string(),
            )
        })?;
    let report = repository.fetch(&request).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2100",
            "Git fetch failed",
            "check the configured remote and retry `fetch`",
            error.to_string(),
        )
    })?;
    let fetched = report.ordinary().first().ok_or_else(|| {
        Diagnostic::target(
            "E2100",
            "Git fetch returned no requested ref",
            "check the configured remote and retry `fetch`",
        )
    })?;
    let state = if fetched.updated() {
        "updated"
    } else {
        "unchanged"
    };
    println!(
        "fetched {} -> {} ({state})",
        fetched.source(),
        fetched.destination()
    );
    Ok(())
}


fn execute(parsed: &Parsed) -> Result<(), Diagnostic> {
    match parsed.command.clone() {
        Command::Help => {
            println!(
                "orna-cli-v1 [--db ENDPOINT] [repl [EXPRESSION]|status|status --porcelain|status --short|fetch [REMOTE] [BRANCH]|check|explain CODE|invoke TARGET|run [QUALIFIED_FUNCTION]|run seed|run exercise|run sensors.ingest|run library.lend BOOK_ID BORROWER]"
            );
            println!("orna-cli-v1 init [DIRECTORY]");
            Ok(())
        }
        Command::Version => {
            println!("orna-cli-v1 0.1.0");
            Ok(())
        }
        Command::Init(ref target) => initialize_repository(target.as_deref()),
        Command::Fetch { ref remote, ref branch } => run_fetch(&parsed.endpoint, remote, branch),
        Command::Status {
            format: StatusFormat::Human,
        } => run_status_human(&parsed.endpoint),
        Command::Status {
            format: StatusFormat::Porcelain,
        } => run_status(&parsed.endpoint),
        Command::Status {
            format: StatusFormat::Short,
        } => run_status_short(&parsed.endpoint),
        Command::Check => check_project(&parsed.endpoint),
        Command::Invoke(ref target) => run_pure_invocation(&parsed.endpoint, target),
        Command::Explain(code) => {
            println!("{}", explain_diagnostic(&code)?);
            Ok(())
        }
        Command::Repl(ref expression) => run_repl(&parsed.endpoint, expression.as_deref()),
        Command::Run(Invocation::Seed) => run_project_invocation(&parsed.endpoint, "main.seed"),
        Command::Run(Invocation::Exercise) => {
            run_project_invocation(&parsed.endpoint, "main.exercise")
        }
        Command::Run(Invocation::SensorsIngest) => {
            run_project_stream_invocation(&parsed.endpoint, "sensors.ingest")
        }
        Command::Run(Invocation::LibraryLend {
            ref book_id,
            ref borrower,
        }) => {
            let arguments = Environment::from([
                (
                    "book_id".into(),
                    Value::new(OvbRaw::Text(book_id.clone())).map_err(|_| {
                        Diagnostic::usage(
                            "E1002",
                            "`run library.lend` received an invalid book ID",
                            "supply a valid Orna string value for the book ID",
                        )
                    })?,
                ),
                (
                    "borrower".into(),
                    Value::new(OvbRaw::Text(borrower.clone())).map_err(|_| {
                        Diagnostic::usage(
                            "E1002",
                            "`run library.lend` received an invalid borrower",
                            "supply a valid Orna string value for the borrower",
                        )
                    })?,
                ),
            ]);
            run_project_invocation_with_arguments(&parsed.endpoint, "library.lend", &arguments)
        }
        Command::Run(Invocation::ProjectFunction(ref target)) => {
            run_public_project_function(&parsed.endpoint, target)
        }
    }
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let parsed = match parse_cli(&args) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(error.exit as u8);
        }
    };
    match execute(&parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit as u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let error = parse_cli(&["status".into(), "--porcelain=v2".into()])
            .expect_err("status format is exact");
        assert_eq!(error.code, "E1002");
        assert_eq!(
            (error.title, error.help),
            (
                "`status` supports no option, `--porcelain`, or `--short`",
                "use `status`, `status --porcelain`, or `status --short`"
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
        let error = parse_cli(&["serve".into()]).expect_err("unknown command");
        assert_eq!(
            (error.code, error.exit, error.title, error.help),
            (
                "E1002",
                Exit::Usage,
                "unknown Orna command",
                "use `--help` to list supported commands",
            )
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
        let mut session = repl_session(&endpoint).expect("project session");
        let mut input =
            b"use library;\nlet n: Int = 21;\nlibrary.hidden(n)\nlibrary.twice(n)\n:quit\n"
                .as_slice();
        let mut output = Vec::new();
        repl::run(&mut input, &mut output, &mut session).expect("scripted session");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> > > error[ORNA-S012-UNRESOLVED]\n> 42 : Int\n> "
        );
    }

    #[test]
    fn project_repl_rejects_an_uncaptured_standard_import() {
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
        let error = repl_session(&endpoint).expect_err("uncaptured standard module");
        assert_eq!(error.code, "ORNA-S010-IMPORT");
        assert_eq!(error.exit, Exit::Target);
        assert_eq!(error.title, "imported module is unavailable");
        assert_eq!(
            error.help,
            "use a captured standard dependency or remove the import"
        );
    }

    #[test]
    fn repl_snapshot_loader_reads_cwd_head_and_named_refs_without_git_mutation() {
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
                    "user.email=orna@example.test",
                    "-c",
                    "user.name=Orna Test",
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
        let loader = ReplSnapshotSessionLoader {
            endpoint: &endpoint,
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
        let loader = ReplSnapshotSessionLoader {
            endpoint: &endpoint,
        };
        let mut session = repl::SnapshotSessionLoader::load_snapshot(
            &loader,
            repl::SnapshotTarget::Ref("snapshot".into()),
        )
        .expect("selected snapshot project is admitted");

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
        let loader = ReplSnapshotSessionLoader {
            endpoint: &endpoint,
        };
        let mut session = repl_session(&endpoint).expect("core-only remote REPL session");
        let mut input = b":at HEAD\n:at snapshot\n:quit\n".as_slice();
        let mut output = Vec::new();

        repl::run_with_snapshot_loader(&mut input, &mut output, &mut session, &loader)
            .expect("REPL runs");

        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "> error[ORNA-REPL-AT]\n> error[ORNA-REPL-AT]\n> "
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
                .expect_err("module source rejects status binding")
                .code,
            "E2101"
        );
    }

    #[test]
    fn single_expression_repl_reports_visible_success_failure_and_quit() {
        let mut session = AdmittedReplSession::new(Limits::default());
        let mut output = Vec::new();
        assert_eq!(
            run_repl_submission(&mut session, "1 + 2", &mut output),
            Ok(())
        );
        let error = run_repl_submission(&mut session, "missing()", &mut output)
            .expect_err("failure is reported");
        assert_eq!(error.code, "E2000");
        assert_eq!(
            run_repl_submission(&mut session, ":quit", &mut output),
            Ok(())
        );
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(output.starts_with("3 : Int\nerror[ORNA-S012-UNRESOLVED]"));
        assert_eq!(output.matches('\n').count(), 2);
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
            run_repl_submission(&mut session, "1", &mut BrokenWriter).expect_err("writer failure");
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
        assert_eq!(check_project(&endpoint), Ok(()));
    }

    #[test]
    fn authoritative_reference_project_runs_seed_exercise_and_sensor_stream() {
        let directory = tempfile::tempdir().expect("temporary reference project");
        let reference = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../reference/Orna-1.0.0/examples/reference");
        for name in [
            "main.orna",
            "library.orna",
            "warehouse.orna",
            "sensors.orna",
            "values.orna",
        ] {
            std::fs::copy(reference.join(name), directory.path().join(name))
                .expect("reference source");
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
            }),
            Ok(())
        );
        assert_eq!(
            execute(&Parsed {
                endpoint: endpoint.clone(),
                command: Command::Run(Invocation::Seed),
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
        })
        .expect_err("duplicate seed must be rejected");
        assert_eq!((error.code, error.exit), ("E2200", Exit::Target));
        assert_eq!(
            execute(&Parsed {
                endpoint: endpoint.clone(),
                command: Command::Run(Invocation::Exercise),
            }),
            Ok(())
        );
        assert_eq!(
            execute(&Parsed {
                endpoint,
                command: Command::Run(Invocation::SensorsIngest),
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
        let error = check_project(&endpoint).expect_err("uncaptured standard module");
        assert_eq!(
            (error.code, error.exit, error.title, error.help),
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
        let error = check_project(&endpoint).expect_err("unbundled standard module");
        assert_eq!(
            (error.code, error.exit, error.title, error.help),
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
        };
        let error = execute(&parsed).expect_err("uncaptured standard module");
        assert_eq!(
            (error.code, error.exit, error.title, error.help),
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
        };
        let error = execute(&parsed).expect_err("uncaptured standard module");
        assert_eq!(
            (error.code, error.exit, error.title, error.help),
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
        let error = check_project(&endpoint).expect_err("semantic error");
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
        git(&local, &["config", "user.email", "fetch@example.invalid"]);
        git(&local, &["config", "user.name", "Fetch test"]);
        std::fs::write(local.join("tracked.txt"), "base\n").expect("base file");
        git(&local, &["add", "tracked.txt"]);
        git(&local, &["commit", "-m", "base"]);
        let initial = git(&local, &["rev-parse", "HEAD"]);
        let remote_path = remote.to_str().expect("remote path");
        git(&local, &["remote", "add", "origin", remote_path]);
        git(&local, &["push", "origin", "HEAD:refs/heads/main"]);
        git(root.path(), &["clone", remote_path, "updater"]);
        git(&updater, &["config", "user.email", "fetch@example.invalid"]);
        git(&updater, &["config", "user.name", "Fetch test"]);
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
