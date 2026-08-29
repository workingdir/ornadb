#![allow(clippy::while_let_loop)]
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::PathBuf,
};

use orna_client::endpoint::DatabaseEndpoint;

use orna_core::{
    FunctionId, InspectEpochId, InvocationId, ParameterId as RawCallParameterId, PrincipalId,
    StateSlotId, TypeId,
    catalogue::QualifiedSemanticName,
    invocation::{InvocationTarget, InvocationTracePolicy},
    invocation_binding::CliArgumentInput,
    security::{CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME},
};

pub(crate) const USAGE: &str = "Usage:\n  orna [OPTIONS] [URI]\n  orna [OPTIONS] [URI] invoke <function> [OPTIONS]\n  orna [OPTIONS] -d\n  orna --help\n  orna --version\n\nCommands:\n  invoke       Run one stored function.\n  source       Check or apply Orna source.\n  inspect      Inspect a completed invocation.\n\nOptions:\n  --db <URI>   Select the database URI.\n  -d, --daemon Run the local server in the foreground.\n  --runtime <family>  Select tty or qt for invoke.\n  --color <auto|always|never>  Control terminal colour.\n  -h, --help   Show help.\n  -V, --version  Show the version.\n";

pub(crate) const HELP_TOP_LEVEL: &str = "Orna command line\n\nOpen a database session or run a stored function.\n\nUsage:\n  orna [OPTIONS] [URI]\n  orna [OPTIONS] [URI] invoke <function> [OPTIONS]\n  orna [OPTIONS] -d\n\nCommands:\n  invoke ...   Run one stored function.\n  inspect ...  Inspect a completed invocation.\n  source ...   Check or apply one source file.\n\nOptions:\n  --db <URI>   Select a local path, Unix socket, or remote Orna URI.\n  --runtime <family>  Select tty or qt for invoke.\n  -d, --daemon Run the local server in the foreground for a supervisor.\n  -h, --help   Show help for a command.\n  -V, --version  Show the Orna version.\n\nThe default command opens the function-backed REPL.\n";
const HELP_SERVER: &str = "Manage an Orna server.\n\nUsage:\n  orna server run\n  orna server backend-shell\n\nCommands:\n  run            Start the server in the foreground.\n  backend-shell  Open a shell for the ready server.\n\nRun `orna server COMMAND --help` for more information.\n";
const HELP_SERVER_RUN: &str = "Start the Orna server in the foreground.\n\nUsage:\n  orna server run\n\nThis command accepts no options. Use a service manager to supervise the process.\n";
const HELP_SERVER_BACKEND_SHELL: &str = "Open a shell for the ready Orna server.\n\nUsage:\n  orna server backend-shell\n\nThis command accepts no options.\n";
const HELP_SOURCE: &str = "Work with Orna source.\n\nUsage:\n  orna source check <file.orna>\n  orna source apply <file.orna>\n  orna source diff <file.orna>\n\nCommands:\n  check  Check one source file without changing the database.\n  apply  Check and apply one source file.\n  diff   Compare one source file with the current database.\n";
const HELP_INVOKE: &str = "Run a stored function.\n\nUsage:\n  orna [--runtime <family>] invoke <qualified-name | canonical-function-id> [options]\n\nOptions:\n  --arg <parameter>=<value>  Bind a parameter.\n  --args-file <path>        Read arguments from a JSON file.\n  --output <value>          Select an output format or type.\n  --trace <policy>          Set tracing: off, basic, normal, verbose, or profile.\n  --runtime <family>        Select tty or qt.\n  --explain                 Show the request without running it.\n  --no-progress             Hide progress diagnostics.\n";
const HELP_REPL: &str = "Open the standard function-backed Orna session.\n\nUsage:\n  orna\n  orna repl\n\nThe session is a normal CLIENT function invocation. The selected local runtime\nowns terminal or graphical surfaces and input events.\n";
const HELP_STATE: &str = "Read or update user state.\n\nUsage:\n  orna state get <root-function-id> [options]\n  orna state set <root-function-id> [options]\n\nOptions for get:\n  --profile <state-profile>\n  --instance <canonical-function-id> [--instance-key <instance-key>]\n  --expect-type <canonical-function-id> <canonical-state-slot-id> <canonical-type-id>\n\nOptions for set:\n  --function <canonical-function-id>\n  --instance-key <instance-key>\n  --slot <canonical-state-slot-id>\n  --revision <create|revision-number>\n  --type <canonical-type-id>\n  --value-file <path>\n  --profile <state-profile>\n";
const HELP_INSPECT: &str = "Inspect a completed invocation.\n\nUsage:\n  orna inspect <invocation-id> [options]\n\nOptions:\n  --projection <name>  Select one of: invocation_nodes, calls, resources, state_cells, ui_nodes, presentation_candidates, runtime_bindings, security_decisions.\n  --trace              Include trace events.\n  --after <n>          Resume after a sequence number.\n  --include-values     Include value data where permitted.\n  --include-source     Include source provenance.\n  --include-security   Include security decisions.\n  --include-runtime    Include runtime bindings.\n  --epoch <epoch-id>   Inspect an exact epoch.\n";

const HELP_RUNTIME: &str = "Describe an installed runtime.\n\nUsage:\n  orna runtime describe <runtime-shared-library>\n\nCommands:\n  describe  Show metadata for an installed runtime.\n";
const HELP_SECURITY: &str = "Manage users, roles, and grants.\n\nUsage:\n  orna security grant-execute <canonical-function-id>\n  orna security user create|disable <canonical-principal-id>\n  orna security role create|grant|revoke <canonical-principal-id> [canonical-principal-id]\n  orna security grants grant|revoke <canonical-principal-id> <class> [canonical-function-id]\n  orna security grants list <canonical-principal-id>\n  orna security check can-execute <canonical-principal-id> <canonical-function-id>\n  orna security check has-privilege <canonical-principal-id> <class> [canonical-function-id]\n  orna security whoami\n\nPrivilege classes:\n  execute, security_admin, inspect:own-invocation, inspect:session-invocations,\n  inspect:any-invocation, inspect:values, inspect:source,\n  inspect:security-details, inspect:runtime-internals.\n\nUse these commands to administer access and inspect the current principal.\n";
const HELP_RAW_CALL: &str = "Make a low-level local call.\n\nUsage:\n  orna raw-call <canonical-function-id>\n  orna raw-call <canonical-function-id> <canonical-parameter-id>\n  orna raw-call <canonical-function-id> <canonical-parameter-id-1> <canonical-parameter-id-2>\n\nThe first form sends no arguments. The other forms read one or two complete ORV1 values from standard input.\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    fn parse(value: &OsStr) -> Option<Self> {
        match value.to_str()? {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    pub(crate) fn enabled(self, terminal: bool) -> bool {
        match self {
            Self::Auto => terminal,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedInvocation {
    pub(crate) color: ColorChoice,
    pub(crate) endpoint: DatabaseEndpoint,
    pub(crate) endpoint_explicit: bool,
    pub(crate) command: Command,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HelpTopic {
    TopLevel,
    Server,
    ServerRun,
    ServerBackendShell,
    Source,
    Invoke,
    Repl,
    State,
    Inspect,
    Runtime,
    Security,
    RawCall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RawCallParameters {
    None,
    One(RawCallParameterId),
    Pair(RawCallParameterId, RawCallParameterId),
}

/// One parsed `orna invoke` command (ADR 0056 step 4).
///
/// The parser strips option prefixes, splits `--arg <parameter>=<value>`
/// pairs, and reads `--args-file` documents into [`CliArgumentInput`] values
/// before the host reflects the resolved signature and binds them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InvokeArguments {
    /// The target selector exactly as supplied.
    pub(crate) target: InvocationTarget,
    /// Raw CLI arguments to bind against the resolved signature.
    pub(crate) arguments: Vec<CliArgumentInput>,
    /// The raw `--output <alias|media-type|type-name>` value, when present.
    pub(crate) output: Option<String>,
    /// The `--trace` policy, when present; absent means off.
    pub(crate) trace: Option<InvocationTracePolicy>,
    /// Suppress progress diagnostics (`--no-progress`).
    pub(crate) no_progress: bool,
    /// Print the plan instead of dispatching (`--explain`).
    pub(crate) explain: bool,
    /// The `--runtime <family>` override, when present; absent selects the
    /// deterministic default runtime (ADR 0063).
    pub(crate) runtime: Option<orna_server::RuntimeFamily>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Help(HelpTopic),
    Version,
    Run,
    BackendShell,
    RuntimeDescribe(PathBuf),
    SourceCheck(String),
    SourceApply(String),
    SourceDiff(String),
    SecurityGrantExecute(FunctionId),
    SecurityAdmin(orna_server::InstalledSecurityAdminRequest),
    RawCall(FunctionId, RawCallParameters),
    Invoke(InvokeArguments),
    State(orna_server::InstalledUserStateRequest),
    Inspect(orna_server::InstalledInspectRequest),
}

pub(crate) fn help_text(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::TopLevel => HELP_TOP_LEVEL,
        HelpTopic::Server => HELP_SERVER,
        HelpTopic::ServerRun => HELP_SERVER_RUN,
        HelpTopic::ServerBackendShell => HELP_SERVER_BACKEND_SHELL,
        HelpTopic::Source => HELP_SOURCE,
        HelpTopic::Invoke => HELP_INVOKE,
        HelpTopic::Repl => HELP_REPL,
        HelpTopic::State => HELP_STATE,
        HelpTopic::Inspect => HELP_INSPECT,
        HelpTopic::Runtime => HELP_RUNTIME,
        HelpTopic::Security => HELP_SECURITY,
        HelpTopic::RawCall => HELP_RAW_CALL,
    }
}

#[inline]
fn is_help_heading(line: &str, line_number: usize) -> bool {
    line_number == 0
        || matches!(
            line,
            "Usage:"
                | "Common Commands:"
                | "Management Commands:"
                | "Host Mode:"
                | "Commands:"
                | "Options:"
                | "Topics:"
        )
}

pub(crate) fn render_help(topic: HelpTopic, color: ColorChoice, terminal: bool) -> String {
    let text = help_text(topic);
    if !color.enabled(terminal) {
        return text.to_owned();
    }
    let mut rendered = String::with_capacity(text.len() + 64);
    for (line_number, segment) in text.split_inclusive('\n').enumerate() {
        let (line, newline) = segment
            .strip_suffix('\n')
            .map_or((segment, ""), |line| (line, "\n"));
        if is_help_heading(line, line_number) {
            rendered.push_str("\x1b[1;36m");
            rendered.push_str(line);
            rendered.push_str("\x1b[0m");
        } else {
            rendered.push_str(line);
        }
        rendered.push_str(newline);
    }
    rendered
}

pub(crate) fn write_help(
    output: &mut impl Write,
    topic: HelpTopic,
    color: ColorChoice,
    terminal: bool,
) -> io::Result<()> {
    output.write_all(render_help(topic, color, terminal).as_bytes())?;
    output.flush()
}

fn parse_help_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let topic = match args.next().as_deref() {
        None => HelpTopic::TopLevel,
        Some(value) if value == OsStr::new("server") => match args.next().as_deref() {
            None => HelpTopic::Server,
            Some(value) if value == OsStr::new("run") => HelpTopic::ServerRun,
            Some(value) if value == OsStr::new("backend-shell") => HelpTopic::ServerBackendShell,
            _ => return None,
        },
        Some(value) if value == OsStr::new("source") => HelpTopic::Source,
        Some(value) if value == OsStr::new("invoke") => HelpTopic::Invoke,
        Some(value) if value == OsStr::new("repl") => HelpTopic::Repl,
        Some(value) if value == OsStr::new("state") => HelpTopic::State,
        Some(value) if value == OsStr::new("inspect") => HelpTopic::Inspect,
        Some(value) if value == OsStr::new("runtime") => HelpTopic::Runtime,
        Some(value) if value == OsStr::new("security") => HelpTopic::Security,
        Some(value) if value == OsStr::new("raw-call") => HelpTopic::RawCall,
        _ => return None,
    };
    args.next().is_none().then_some(Command::Help(topic))
}

fn parse_server_leaf<I>(args: &mut I, command: Command, topic: HelpTopic) -> Option<Command>
where
    I: Iterator<Item = OsString>,
{
    match args.next().as_deref() {
        None => Some(command),
        Some(value) if value == OsStr::new("--help") => {
            args.next().is_none().then_some(Command::Help(topic))
        }
        _ => None,
    }
}
#[cfg(test)]
pub(crate) fn parse_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    parse_invocation(args).map(|parsed| parsed.command)
}

pub(crate) fn parse_invocation<I>(args: I) -> Option<ParsedInvocation>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();
    let _argv0 = args.next();
    let mut color = ColorChoice::Auto;
    let mut color_seen = false;
    let mut endpoint = DatabaseEndpoint::managed_local();
    let mut endpoint_explicit = false;

    loop {
        let Some(value) = args.peek() else {
            break;
        };
        if value == OsStr::new("--color") {
            if color_seen {
                return None;
            }
            let _ = args.next();
            color = ColorChoice::parse(&args.next()?)?;
            color_seen = true;
            continue;
        }
        if value == OsStr::new("--db") {
            if endpoint_explicit {
                return None;
            }
            let _ = args.next();
            let value = args.next()?.into_string().ok()?;
            endpoint = DatabaseEndpoint::parse(&value).ok()?;
            endpoint_explicit = true;
            continue;
        }
        break;
    }
    let positional_endpoint = args.peek().cloned();
    if let Some(value) = positional_endpoint
        && !is_command_name(&value)
    {
        let value = value.into_string().ok()?;
        endpoint = DatabaseEndpoint::parse(&value).ok()?;
        endpoint_explicit = true;
        let _ = args.next();
    }

    let command_args = args.collect::<Vec<_>>();
    Some(ParsedInvocation {
        color,
        endpoint,
        endpoint_explicit,
        command: parse_command_args(command_args)?,
    })
}

fn is_command_name(value: &OsStr) -> bool {
    value.to_str().is_some_and(|value| {
        matches!(
            value,
            "--color"
                | "--db"
                | "--runtime"
                | "--daemon"
                | "-d"
                | "--help"
                | "--version"
                | "help"
                | "server"
                | "runtime"
                | "source"
                | "raw-call"
                | "invoke"
                | "state"
                | "inspect"
                | "security"
                | "repl"
                | "version"
                | "backend-shell"
        )
    })
}

fn default_repl_command(runtime: Option<orna_server::RuntimeFamily>) -> Option<Command> {
    Some(Command::Invoke(InvokeArguments {
        target: parse_qualified_name("std.cli.repl")?,
        arguments: Vec::new(),
        output: None,
        trace: None,
        no_progress: false,
        explain: false,
        runtime,
    }))
}

fn parse_command_args<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();

    // The optional global `--runtime <family>` override (ADR 0063) is
    // consumed before the command word so `orna --runtime tty invoke ...`
    // works. A missing value or an unknown family is a usage error (`None`).
    // The override is threaded into the invoke command below. Source and
    // server commands reject it per their accepted command contracts. Unknown
    // leading flags still fall to `_ => None`.
    let runtime = if args
        .peek()
        .is_some_and(|value| value == OsStr::new("--runtime"))
    {
        let _ = args.next();
        let value = args.next()?.into_string().ok()?;
        match orna_server::RuntimeFamily::parse(&value) {
            Some(runtime) => Some(runtime),
            None => return None,
        }
    } else {
        None
    };

    // The global override belongs to a root invocation. It is valid for the
    // explicit `invoke` form and the function-backed REPL.
    if runtime.is_some()
        && !args
            .peek()
            .is_none_or(|value| value == OsStr::new("invoke") || value == OsStr::new("repl"))
    {
        return None;
    }

    if args.peek().is_none() {
        return default_repl_command(runtime);
    }

    match args.next().as_deref() {
        Some(value) if value == OsStr::new("--daemon") || value == OsStr::new("-d") => {
            args.next().is_none().then_some(Command::Run)
        }
        Some(value) if value == OsStr::new("--help") => args
            .next()
            .is_none()
            .then_some(Command::Help(HelpTopic::TopLevel)),
        Some(value) if value == OsStr::new("help") => parse_help_command(args),
        Some(value) if value == OsStr::new("--version") => {
            args.next().is_none().then_some(Command::Version)
        }
        Some(value) if value == OsStr::new("repl") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::Repl));
            }
            if args.next().is_some() {
                None
            } else {
                Some(Command::Invoke(InvokeArguments {
                    target: parse_qualified_name("std.cli.repl")?,
                    arguments: Vec::new(),
                    output: None,
                    trace: None,
                    no_progress: false,
                    explain: false,
                    runtime,
                }))
            }
        }
        Some(value) if value == OsStr::new("server") => match args.next().as_deref() {
            Some(value) if value == OsStr::new("--help") => args
                .next()
                .is_none()
                .then_some(Command::Help(HelpTopic::Server)),
            Some(value) if value == OsStr::new("run") => {
                parse_server_leaf(&mut args, Command::Run, HelpTopic::ServerRun)
            }
            Some(value) if value == OsStr::new("backend-shell") => parse_server_leaf(
                &mut args,
                Command::BackendShell,
                HelpTopic::ServerBackendShell,
            ),
            _ => None,
        },
        Some(value) if value == OsStr::new("runtime") => match args.next().as_deref() {
            Some(value) if value == OsStr::new("--help") => args
                .next()
                .is_none()
                .then_some(Command::Help(HelpTopic::Runtime)),
            Some(value) if value == OsStr::new("describe") => {
                let path = PathBuf::from(args.next()?);
                args.next()
                    .is_none()
                    .then_some(Command::RuntimeDescribe(path))
            }
            _ => None,
        },
        Some(value) if value == OsStr::new("source") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::Source));
            }
            let command = match args.next().as_deref() {
                Some(value) if value == OsStr::new("check") => Command::SourceCheck,
                Some(value) if value == OsStr::new("apply") => Command::SourceApply,
                Some(value) if value == OsStr::new("diff") => Command::SourceDiff,
                _ => return None,
            };
            let path = args.next()?.into_string().ok()?;
            (args.next().is_none() && valid_source_path(&path)).then(|| command(path))
        }
        Some(value) if value == OsStr::new("raw-call") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::RawCall));
            }
            let function = args.next()?.into_string().ok()?;
            let first = match args.next() {
                Some(parameter) => {
                    RawCallParameterId::from_canonical(&parameter.into_string().ok()?)
                        .ok()
                        .map(RawCallParameters::One)?
                }
                None => RawCallParameters::None,
            };
            let parameters = match (first, args.next()) {
                (RawCallParameters::One(first), Some(second)) => {
                    let second =
                        RawCallParameterId::from_canonical(&second.into_string().ok()?).ok()?;
                    if first == second {
                        return None;
                    }
                    RawCallParameters::Pair(first, second)
                }
                (parameters, None) => parameters,
                (RawCallParameters::None | RawCallParameters::Pair(_, _), Some(_)) => {
                    return None;
                }
            };
            if args.next().is_some() {
                return None;
            }
            if function == CATALOGUE_HEALTH_FUNCTION_NAME {
                (parameters == RawCallParameters::None).then_some(Command::RawCall(
                    CATALOGUE_HEALTH_FUNCTION_ID,
                    RawCallParameters::None,
                ))
            } else {
                FunctionId::from_canonical(&function)
                    .ok()
                    .map(|function| Command::RawCall(function, parameters))
            }
        }
        Some(value) if value == OsStr::new("invoke") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::Invoke));
            }
            let mut command = parse_invoke_command(args)?;
            // The global override (when given) takes precedence over the
            // post-command form; otherwise the parser's own value stands.
            if let (Command::Invoke(arguments), Some(runtime)) = (&mut command, runtime) {
                arguments.runtime = Some(runtime);
            }
            Some(command)
        }
        Some(value) if value == OsStr::new("state") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::State));
            }
            parse_state_command(args)
        }
        Some(value) if value == OsStr::new("inspect") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::Inspect));
            }
            parse_inspect_command(args)
        }
        Some(value) if value == OsStr::new("security") => {
            if args
                .peek()
                .is_some_and(|value| value == OsStr::new("--help"))
            {
                let _ = args.next();
                return args
                    .next()
                    .is_none()
                    .then_some(Command::Help(HelpTopic::Security));
            }
            let subcommand = args.next()?.into_string().ok()?;
            match subcommand.as_str() {
                "grant-execute" => {
                    let function = args.next()?.into_string().ok()?;
                    if args.next().is_some() {
                        return None;
                    }
                    FunctionId::from_canonical(&function)
                        .ok()
                        .map(Command::SecurityGrantExecute)
                }
                _ => parse_security_admin_command(&subcommand, args),
            }
        }
        _ => None,
    }
}

/// Parses one `orna state <get|set> ...` command (ADR 0061 step 5).
///
/// `get` accepts exactly one root-function positional followed by the
/// optional `--profile <state-profile>`, repeated `--instance
/// <canonical-function-id> [--instance-key <instance-key>]` filters, and
/// repeated `--expect-type <canonical-function-id> <canonical-state-slot-id>
/// <canonical-type-id>` entry triples. `set` accepts exactly one root-function
/// positional followed by `--function <canonical-function-id>`,
/// `--slot <canonical-state-slot-id>`, `--revision <create|revision-number>`,
/// `--type <canonical-type-id>`, `--value-file <path>`, the optional
/// `--profile <state-profile>`, and the optional
/// `--instance-key <instance-key>`. Any other shape is a usage error
/// (`None`).
fn parse_state_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let operation = match args.next().as_deref() {
        Some(value) if value == OsStr::new("get") => parse_state_get(&mut args)?,
        Some(value) if value == OsStr::new("set") => parse_state_set(&mut args)?,
        _ => return None,
    };
    Some(Command::State(orna_server::InstalledUserStateRequest::new(
        operation,
    )))
}

/// Parses one `orna state get <root-function-id> [options]` command.
fn parse_state_get<I>(args: &mut I) -> Option<orna_server::InstalledUserStateOperation>
where
    I: Iterator<Item = OsString>,
{
    let root_function = FunctionId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
    let mut state_profile = String::new();
    let mut instances = Vec::new();
    let mut expected_types = Vec::new();
    loop {
        match args.next().as_deref() {
            None => break,
            Some(flag) if flag == OsStr::new("--profile") => {
                state_profile = args.next()?.into_string().ok()?;
            }
            Some(flag) if flag == OsStr::new("--instance") => {
                let function =
                    FunctionId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
                instances.push(orna_server::InstalledUserStateInstance {
                    function,
                    instance_key: String::new(),
                });
            }
            Some(flag) if flag == OsStr::new("--instance-key") => {
                instances.last_mut()?.instance_key = args.next()?.into_string().ok()?;
            }
            Some(flag) if flag == OsStr::new("--expect-type") => {
                let function =
                    FunctionId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
                let state_slot =
                    StateSlotId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
                let value_type = TypeId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
                expected_types.push(orna_server::InstalledUserStateExpectedType {
                    function,
                    state_slot,
                    value_type,
                });
            }
            Some(_) => return None,
        }
    }
    Some(orna_server::InstalledUserStateOperation::Load {
        root_function,
        state_profile,
        instances,
        expected_types,
    })
}

/// Parses one `orna state set <root-function-id> [options]` command.
fn parse_state_set<I>(args: &mut I) -> Option<orna_server::InstalledUserStateOperation>
where
    I: Iterator<Item = OsString>,
{
    let root_function = FunctionId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
    let mut state_profile = String::new();
    let mut function = None;
    let mut instance_key = String::new();
    let mut state_slot = None;
    let mut expected_revision = None;
    let mut value_type = None;
    let mut value_file = None;
    loop {
        match args.next().as_deref() {
            None => break,
            Some(flag) if flag == OsStr::new("--profile") => {
                state_profile = args.next()?.into_string().ok()?;
            }
            Some(flag) if flag == OsStr::new("--function") => {
                function =
                    Some(FunctionId::from_canonical(&args.next()?.into_string().ok()?).ok()?);
            }
            Some(flag) if flag == OsStr::new("--instance-key") => {
                instance_key = args.next()?.into_string().ok()?;
            }
            Some(flag) if flag == OsStr::new("--slot") => {
                state_slot =
                    Some(StateSlotId::from_canonical(&args.next()?.into_string().ok()?).ok()?);
            }
            Some(flag) if flag == OsStr::new("--revision") => {
                let value = args.next()?.into_string().ok()?;
                expected_revision = Some(if value == "create" {
                    None
                } else {
                    Some(value.parse::<u64>().ok()?)
                });
            }
            Some(flag) if flag == OsStr::new("--type") => {
                value_type = Some(TypeId::from_canonical(&args.next()?.into_string().ok()?).ok()?);
            }
            Some(flag) if flag == OsStr::new("--value-file") => {
                value_file = Some(args.next()?);
            }
            Some(_) => return None,
        }
    }
    let function = function?;
    let state_slot = state_slot?;
    let expected_revision = expected_revision?;
    let value_type = value_type?;
    let value_bytes = std::fs::read(value_file?).ok()?;
    Some(orna_server::InstalledUserStateOperation::Write {
        root_function,
        state_profile,
        change: orna_server::InstalledUserStateChange {
            function,
            instance_key,
            state_slot,
            expected_revision,
            value_type,
            value_bytes,
        },
    })
}

/// Parses one `orna inspect <invocation-id> [options]` command (ADR 0064
/// wave 3).
///
/// The invocation identity is exactly one positional in its canonical
/// `type:base32` text form. Options are the optional `--projection <name>`
/// selector (one of the eight closed projection names), the value-less
/// `--trace` switch, the optional `--after <n>` resume sequence, the four
/// value-less classifier flags `--include-values`, `--include-source`,
/// `--include-security`, and `--include-runtime`, and the optional `--epoch
/// <epoch-id>` exact override. A missing or malformed identity, an unknown
/// projection name, a non-numeric `--after`, a malformed epoch identity, an
/// unknown flag, or a trailing positional is a usage error (`None`).
fn parse_inspect_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let invocation = InvocationId::from_canonical(&args.next()?.into_string().ok()?).ok()?;
    let mut epoch = None;
    let mut projection = None;
    let mut trace = false;
    let mut after_sequence = 0_u64;
    let mut include_values = false;
    let mut include_source = false;
    let mut include_security = false;
    let mut include_runtime = false;
    loop {
        match args.next().as_deref() {
            None => break,
            Some(flag) if flag == OsStr::new("--epoch") => {
                epoch =
                    Some(InspectEpochId::from_canonical(&args.next()?.into_string().ok()?).ok()?);
            }
            Some(flag) if flag == OsStr::new("--projection") => {
                projection = Some(orna_server::InstalledInspectProjection::parse(
                    &args.next()?.into_string().ok()?,
                )?);
            }
            Some(flag) if flag == OsStr::new("--trace") => trace = true,
            Some(flag) if flag == OsStr::new("--after") => {
                after_sequence = args.next()?.into_string().ok()?.parse::<u64>().ok()?;
            }
            Some(flag) if flag == OsStr::new("--include-values") => include_values = true,
            Some(flag) if flag == OsStr::new("--include-source") => include_source = true,
            Some(flag) if flag == OsStr::new("--include-security") => include_security = true,
            Some(flag) if flag == OsStr::new("--include-runtime") => include_runtime = true,
            Some(_) => return None,
        }
    }
    Some(Command::Inspect(orna_server::InstalledInspectRequest::new(
        invocation,
        epoch,
        projection,
        trace,
        after_sequence,
        include_values,
        include_source,
        include_security,
        include_runtime,
    )))
}

/// Parses one `orna security <subcommand> ...` administrative command
/// (ADR 0065).
///
/// The subcommand verb is consumed by the dispatcher and passed here. The
/// closed shapes are: `user create|disable <principal-id>`; `role
/// create|grant|revoke <role-id> [member-id]`; `grants grant|revoke
/// <grantee-id> <class> [<object-id>]`; `grants list <grantee-id>`;
/// `check can-execute <principal-id> <function-id>`; `check has-privilege
/// <principal-id> <class> [<object-id>]`; and `whoami`. Any other shape is a usage error (`None`).
fn parse_security_admin_command<I>(subcommand: &str, args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    use orna_server::InstalledSecurityAdminOperation as Operation;

    let mut args = args.into_iter();
    let operation = match subcommand {
        "whoami" => {
            if args.next().is_some() {
                return None;
            }
            Operation::SessionPrincipal
        }
        "user" => {
            let action = args.next()?.into_string().ok()?;
            let principal = parse_principal_id(args.next()?.into_string().ok()?)?;
            if args.next().is_some() {
                return None;
            }
            match action.as_str() {
                "create" => Operation::CreatePrincipal {
                    principal,
                    kind: orna_core::security::PrincipalKind::User,
                },
                "disable" => Operation::DisablePrincipal { principal },
                _ => return None,
            }
        }
        "role" => {
            let action = args.next()?.into_string().ok()?;
            let role = parse_principal_id(args.next()?.into_string().ok()?)?;
            match action.as_str() {
                "create" => {
                    if args.next().is_some() {
                        return None;
                    }
                    Operation::CreateRole { role }
                }
                "grant" | "revoke" => {
                    let member = parse_principal_id(args.next()?.into_string().ok()?)?;
                    if args.next().is_some() {
                        return None;
                    }
                    if action == "grant" {
                        Operation::GrantRole { role, member }
                    } else {
                        Operation::RevokeRole { role, member }
                    }
                }
                _ => return None,
            }
        }
        "grants" => {
            let action = args.next()?.into_string().ok()?;
            let grantee = parse_principal_id(args.next()?.into_string().ok()?)?;
            match action.as_str() {
                "list" => {
                    if args.next().is_some() {
                        return None;
                    }
                    Operation::ListGrants { grantee }
                }
                "grant" | "revoke" => {
                    let class_text = args.next()?.into_string().ok()?;
                    let class = orna_server::parse_privilege_class(&class_text)?;
                    let object = match args.next() {
                        Some(object) => Some(parse_function_id(object.into_string().ok()?)?),
                        None => None,
                    };
                    if args.next().is_some() {
                        return None;
                    }
                    if action == "grant" {
                        Operation::GrantPrivilege {
                            grantee,
                            class,
                            object,
                        }
                    } else {
                        Operation::RevokePrivilege {
                            grantee,
                            class,
                            object,
                        }
                    }
                }
                _ => return None,
            }
        }
        "check" => {
            let action = args.next()?.into_string().ok()?;
            let principal = parse_principal_id(args.next()?.into_string().ok()?)?;
            match action.as_str() {
                "can-execute" => {
                    let function = parse_function_id(args.next()?.into_string().ok()?)?;
                    if args.next().is_some() {
                        return None;
                    }
                    Operation::CanExecute {
                        principal,
                        function,
                    }
                }
                "has-privilege" => {
                    let class_text = args.next()?.into_string().ok()?;
                    let class = orna_server::parse_privilege_class(&class_text)?;
                    let object = match args.next() {
                        Some(object) => Some(parse_function_id(object.into_string().ok()?)?),
                        None => None,
                    };
                    if args.next().is_some() {
                        return None;
                    }
                    Operation::HasPrivilege {
                        principal,
                        class,
                        object,
                    }
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(Command::SecurityAdmin(
        orna_server::InstalledSecurityAdminRequest::new(operation),
    ))
}

/// Parses one canonical `PrincipalId` text value.
fn parse_principal_id(value: String) -> Option<PrincipalId> {
    PrincipalId::from_canonical(&value).ok()
}

/// Parses one canonical `FunctionId` text value.
fn parse_function_id(value: String) -> Option<FunctionId> {
    FunctionId::from_canonical(&value).ok()
}

/// Parses one `orna invoke <target> [options]` command (ADR 0056 step 4).
///
/// The target is exactly one positional: a dotted qualified name of two or
/// more parts or a canonical opaque [`FunctionId`]. Options are `--arg
/// <parameter>=<value>` (canonical), `--<anything-else> <value>` (friendly),
/// `--args-file <path>`, `--output <value>`, `--trace <value>`, the runtime
/// override `--runtime <family>` (ADR 0063), and the value-less `--explain`
/// and `--no-progress`. A second positional, an unknown flag without a
/// value, an empty `--output`, an invalid trace or runtime value, or a
/// malformed `--arg` pair is a usage error (`None`). `--runtime` is parsed
/// explicitly so it is never emitted as a friendly argument named
/// `runtime`.
fn parse_invoke_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let target = parse_invoke_target(&args.next()?.into_string().ok()?)?;
    let mut arguments = Vec::new();
    let mut output = None;
    let mut trace = None;
    let mut no_progress = false;
    let mut explain = false;
    let mut runtime = None;
    while let Some(flag) = args.next() {
        let flag = flag.into_string().ok()?;
        let name = flag.strip_prefix("--")?;
        match name {
            "help" => return None,
            "arg" => {
                let pair = args.next()?.into_string().ok()?;
                let (parameter, value) = pair.split_once('=')?;
                if parameter.is_empty() {
                    return None;
                }
                arguments.push(CliArgumentInput::Canonical {
                    parameter: parameter.to_owned(),
                    value: value.to_owned(),
                });
            }
            "args-file" => {
                let path = args.next()?.into_string().ok()?;
                arguments.extend(parse_args_file(&path, &target)?);
            }
            "output" => {
                let value = args.next()?.into_string().ok()?;
                if value.is_empty() {
                    return None;
                }
                output = Some(value);
            }
            "trace" => {
                let value = args.next()?.into_string().ok()?;
                trace = Some(parse_trace(&value)?);
            }
            "runtime" => {
                let value = args.next()?.into_string().ok()?;
                runtime = Some(orna_server::RuntimeFamily::parse(&value)?);
            }
            "explain" => explain = true,
            "no-progress" => no_progress = true,
            "db" => return None,
            _ => {
                let value = args.next()?.into_string().ok()?;
                arguments.push(CliArgumentInput::Friendly {
                    name: name.to_owned(),
                    value,
                });
            }
        }
    }
    Some(Command::Invoke(InvokeArguments {
        target,
        arguments,
        output,
        trace,
        no_progress,
        explain,
        runtime,
    }))
}

/// Parses one invoke target: a qualified name first, then a canonical
/// [`FunctionId`]. A name of fewer than two parts cannot be a qualified
/// function name and falls through to the opaque identity form.
fn parse_invoke_target(value: &str) -> Option<InvocationTarget> {
    parse_qualified_name(value).or_else(|| {
        FunctionId::from_canonical(value)
            .ok()
            .map(InvocationTarget::function_id)
    })
}

/// Parses one dotted qualified name of two or more parts.
fn parse_qualified_name(value: &str) -> Option<InvocationTarget> {
    let name = QualifiedSemanticName::new(value.split('.').collect::<Vec<_>>()).ok()?;
    InvocationTarget::qualified_name(name).ok()
}

/// Validates one `--trace` value against the five trace policies.
fn parse_trace(value: &str) -> Option<InvocationTracePolicy> {
    match value {
        "off" => Some(InvocationTracePolicy::Off),
        "basic" => Some(InvocationTracePolicy::Basic),
        "normal" => Some(InvocationTracePolicy::Normal),
        "verbose" => Some(InvocationTracePolicy::Verbose),
        "profile" => Some(InvocationTracePolicy::Profile),
        _ => None,
    }
}

/// Parses one `--args-file` document into typed CLI argument inputs.
///
/// The accepted minimal closed form is the ADR 0056 subset of the spec
/// `invoke_request_v2` JSON representation: an object with exactly `target`
/// and `arguments` keys. `target` is either `{"function_id": ...}` or
/// `{"qualified_name": ...}` and must resolve to the same target as the
/// command line; `arguments` maps each parameter selector (canonical
/// [`FunctionId`]-style [`orna_core::ParameterId`] text or source name) to
/// its CLI string value. Any other shape is a usage error.
fn parse_args_file(path: &str, command_target: &InvocationTarget) -> Option<Vec<CliArgumentInput>> {
    let text = std::fs::read_to_string(path).ok()?;
    let document: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).ok()?;
    if document.len() != 2 {
        return None;
    }
    let file_target = parse_json_target(document.get("target")?)?;
    if &file_target != command_target {
        return None;
    }
    let arguments = document.get("arguments")?.as_object()?;
    let mut inputs = Vec::with_capacity(arguments.len());
    for (parameter, value) in arguments {
        inputs.push(CliArgumentInput::Canonical {
            parameter: parameter.clone(),
            value: value.as_str()?.to_owned(),
        });
    }
    Some(inputs)
}

/// Parses one `invoke_request_v2` JSON target value, which is exactly one of
/// the opaque identity or qualified-name forms.
fn parse_json_target(value: &serde_json::Value) -> Option<InvocationTarget> {
    let object = value.as_object()?;
    match (object.get("function_id"), object.get("qualified_name")) {
        (Some(function_id), None) => FunctionId::from_canonical(function_id.as_str()?)
            .ok()
            .map(InvocationTarget::function_id),
        (None, Some(qualified_name)) => parse_qualified_name(qualified_name.as_str()?),
        _ => None,
    }
}

fn valid_source_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('-')
        && !path
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
}
