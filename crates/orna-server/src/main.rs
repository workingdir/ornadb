use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    process::ExitCode,
};

use orna_core::{
    FunctionId, InspectEpochId, InvocationId, ParameterId as RawCallParameterId, PrincipalId,
    StateSlotId, TypeId,
    catalogue::QualifiedSemanticName,
    invocation::{InvocationTarget, InvocationTracePolicy},
    invocation_binding::CliArgumentInput,
    security::{CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME},
};
use orna_protocol::CallFailure;

mod package_maintenance;
mod source_check;

const USAGE: &str = "Usage:\n  orna --version\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna source apply <file.orna>\n  orna source diff <file.orna>\n  orna security grant-execute <canonical-function-id>\n  orna security user create|disable <canonical-principal-id>\n  orna security role create|grant|revoke <canonical-principal-id> [canonical-principal-id]\n  orna security grants grant|revoke <canonical-principal-id> <class> [canonical-function-id]\n  orna security grants list <canonical-principal-id>\n  orna security check can-execute <canonical-principal-id> <canonical-function-id>\n  orna security check has-privilege <canonical-principal-id> <class> [canonical-function-id]\n  orna security whoami\n  orna raw-call <canonical-function-id>\n  orna raw-call <canonical-function-id> <canonical-parameter-id>\n  orna [--runtime <family>] invoke <qualified-name | canonical-function-id> [options]\n  orna state get <root-function-id> [options]\n  orna state set <root-function-id> [options]\n  orna inspect <invocation-id> [options]";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawCallParameters {
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
struct InvokeArguments {
    /// The target selector exactly as supplied.
    target: InvocationTarget,
    /// Raw CLI arguments to bind against the resolved signature.
    arguments: Vec<CliArgumentInput>,
    /// The raw `--output <alias|media-type|type-name>` value, when present.
    output: Option<String>,
    /// The `--trace` policy, when present; absent means off.
    trace: Option<InvocationTracePolicy>,
    /// Suppress progress diagnostics (`--no-progress`).
    no_progress: bool,
    /// Print the plan instead of dispatching (`--explain`).
    explain: bool,
    /// The `--runtime <family>` override, when present; absent selects the
    /// deterministic default runtime (ADR 0063).
    runtime: Option<orna_server::RuntimeFamily>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Version,
    Run,
    BackendShell,
    Upgrade,
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

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let Some(result) = package_maintenance::run_if_selected(arguments.len()) {
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        };
    }
    let Some(command) = parse_command(arguments) else {
        write_stderr_line(USAGE);
        return ExitCode::from(2);
    };

    match command {
        Command::Version => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match writeln!(stdout, "orna {}", env!("CARGO_PKG_VERSION")) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
        Command::Run => match orna_server::run_embedded_server() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        },
        Command::BackendShell => match orna_server::run_backend_shell() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        },
        Command::Upgrade => match orna_server::run_embedded_upgrade() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        },
        Command::SourceCheck(path) => {
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            match source_check::run(&path, &mut stderr) {
                source_check::SourceCheckResult::Success => ExitCode::SUCCESS,
                source_check::SourceCheckResult::Failure => ExitCode::from(1),
                source_check::SourceCheckResult::Usage => {
                    let _ = writeln!(stderr, "{USAGE}");
                    ExitCode::from(2)
                }
            }
        }
        Command::SourceApply(path) => match orna_server::run_installed_source_apply(&path) {
            Ok(orna_server::InstalledSourceApplyOutcome::Diagnostics(diagnostics)) => {
                let stderr = io::stderr();
                let mut stderr = stderr.lock();
                let _ = stderr
                    .write_all(diagnostics.as_bytes())
                    .and_then(|()| stderr.flush());
                ExitCode::from(1)
            }
            Ok(orna_server::InstalledSourceApplyOutcome::Applied(document)) => {
                let stdout = io::stdout();
                let mut stdout = stdout.lock();
                match document.write_to(&mut stdout) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        write_stderr_line(&error.to_string());
                        ExitCode::from(1)
                    }
                }
            }
            Ok(_) => {
                write_stderr_line("orna: source apply returned an unsupported result");
                ExitCode::from(1)
            }
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        },
        Command::SourceDiff(path) => match orna_server::run_installed_source_diff(&path) {
            Ok(orna_server::InstalledSourceDiffOutcome::Diagnostics(diagnostics)) => {
                let stderr = io::stderr();
                let mut stderr = stderr.lock();
                let _ = stderr
                    .write_all(diagnostics.as_bytes())
                    .and_then(|()| stderr.flush());
                ExitCode::from(1)
            }
            Ok(orna_server::InstalledSourceDiffOutcome::Diff(report)) => {
                let stdout = io::stdout();
                let mut stdout = stdout.lock();
                match report.write_to(&mut stdout) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        write_stderr_line(&error.to_string());
                        ExitCode::from(1)
                    }
                }
            }
            Ok(_) => {
                write_stderr_line("orna: source diff returned an unsupported result");
                ExitCode::from(1)
            }
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        },
        Command::SecurityGrantExecute(function) => {
            match orna_server::security_admin::run_installed_security_grant(function) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
        Command::SecurityAdmin(request) => {
            let mut stdout = std::io::stdout().lock();
            match orna_server::run_installed_security_admin(request, &mut stdout) {
                Ok(_) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    match error.kind() {
                        orna_server::InstalledSecurityAdminErrorKind::Usage
                        | orna_server::InstalledSecurityAdminErrorKind::Internal => {
                            ExitCode::from(2)
                        }
                        orna_server::InstalledSecurityAdminErrorKind::Kernel => ExitCode::from(4),
                        orna_server::InstalledSecurityAdminErrorKind::Rendering => {
                            ExitCode::from(5)
                        }
                        _ => ExitCode::from(7),
                    }
                }
            }
        }
        Command::RawCall(function, parameters) => match match parameters {
            RawCallParameters::None => orna_server::run_local_raw_call(function),
            RawCallParameters::One(parameter) => {
                orna_server::run_local_raw_call_with_argument(function, parameter)
            }
            RawCallParameters::Pair(first, second) => {
                orna_server::run_local_raw_call_with_argument_pair(function, first, second)
            }
        } {
            Ok(orna_server::LocalRawCallOutcome::Completed) => ExitCode::SUCCESS,
            Ok(orna_server::LocalRawCallOutcome::Failed(failure)) => {
                write_stderr_line(&format!("raw call failed: {}", failure_name(failure)));
                ExitCode::from(1)
            }
            Ok(orna_server::LocalRawCallOutcome::Cancelled) => ExitCode::from(6),
            Err(
                error @ (orna_server::LocalRawCallError::Connection
                | orna_server::LocalRawCallError::Negotiation),
            ) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(3)
            }
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(7)
            }
        },
        Command::Invoke(arguments) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            let stderr = io::stderr();
            let mut stderr = stderr.lock();
            let request = orna_server::InstalledInvokeRequest::new(
                arguments.target,
                arguments.arguments,
                arguments.output,
                arguments.trace,
                arguments.no_progress,
                arguments.explain,
                arguments.runtime,
            );
            match orna_server::run_installed_invoke(request, &mut stdout, &mut stderr) {
                Ok(orna_server::InstalledInvokeOutcome::Completed) => ExitCode::SUCCESS,
                Ok(orna_server::InstalledInvokeOutcome::TargetFailure) => ExitCode::from(1),
                Ok(orna_server::InstalledInvokeOutcome::Denied) => ExitCode::from(4),
                Ok(orna_server::InstalledInvokeOutcome::Cancelled) => ExitCode::from(6),
                // A future closed outcome falls back to protocol / internal.
                Ok(_) => ExitCode::from(7),
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(invoke_error_exit_code(&error))
                }
            }
        }
        Command::State(request) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match orna_server::run_installed_user_state(request, &mut stdout) {
                Ok(orna_server::InstalledUserStateOutcome::Completed) => ExitCode::SUCCESS,
                // A future closed outcome falls back to internal.
                Ok(_) => ExitCode::from(7),
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(state_error_exit_code(&error))
                }
            }
        }
        Command::Inspect(request) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match orna_server::run_installed_inspect(request, &mut stdout) {
                Ok(orna_server::InstalledInspectOutcome::Completed) => ExitCode::SUCCESS,
                // A future closed outcome falls back to internal.
                Ok(_) => ExitCode::from(7),
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(inspect_error_exit_code(&error))
                }
            }
        }
    }
}

fn parse_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter().peekable();
    let _argv0 = args.next();

    // The optional global `--runtime <family>` override (ADR 0063) is
    // consumed before the command word so `orna --runtime tty invoke ...`
    // works. A missing value or an unknown family is a usage error (`None`).
    // The override is threaded into the invoke command below and ignored by
    // every other command; unknown leading flags still fall to `_ => None`.
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

    match args.next().as_deref() {
        Some(value) if value == OsStr::new("--version") => {
            args.next().is_none().then_some(Command::Version)
        }
        Some(value) if value == OsStr::new("server") => {
            let command = match args.next().as_deref() {
                Some(value) if value == OsStr::new("run") => Command::Run,
                Some(value) if value == OsStr::new("backend-shell") => Command::BackendShell,
                Some(value) if value == OsStr::new("upgrade") => Command::Upgrade,
                _ => return None,
            };
            args.next().is_none().then_some(command)
        }
        Some(value) if value == OsStr::new("source") => {
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
            let mut command = parse_invoke_command(args)?;
            // The global override (when given) takes precedence over the
            // post-command form; otherwise the parser's own value stands.
            if let (Command::Invoke(arguments), Some(runtime)) = (&mut command, runtime) {
                arguments.runtime = Some(runtime);
            }
            Some(command)
        }
        Some(value) if value == OsStr::new("state") => parse_state_command(args),
        Some(value) if value == OsStr::new("inspect") => parse_inspect_command(args),
        Some(value) if value == OsStr::new("security") => {
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
/// `--slot <canonical-state-slot-id>`, `--revision <create|revision>`,
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

/// Maps one installed invoke failure to its ADR 0056 spec exit code.
const fn invoke_error_exit_code(error: &orna_server::InstalledInvokeError) -> u8 {
    match error.kind() {
        orna_server::InstalledInvokeErrorKind::Usage => 2,
        orna_server::InstalledInvokeErrorKind::Authentication => 3,
        orna_server::InstalledInvokeErrorKind::Authorisation => 4,
        orna_server::InstalledInvokeErrorKind::Presentation => 5,
        orna_server::InstalledInvokeErrorKind::Cancelled => 6,
        orna_server::InstalledInvokeErrorKind::Internal => 7,
        // A future closed kind falls back to protocol / internal.
        _ => 7,
    }
}

/// Maps one installed state failure to its closed exit code.
const fn state_error_exit_code(error: &orna_server::InstalledUserStateError) -> u8 {
    match error.kind() {
        orna_server::InstalledUserStateErrorKind::Authentication => 3,
        orna_server::InstalledUserStateErrorKind::State => 1,
        orna_server::InstalledUserStateErrorKind::Presentation => 5,
        orna_server::InstalledUserStateErrorKind::Internal => 7,
        // A future closed kind falls back to internal.
        _ => 7,
    }
}

/// Maps one installed inspect failure to its closed exit code.
const fn inspect_error_exit_code(error: &orna_server::InstalledInspectError) -> u8 {
    match error.kind() {
        orna_server::InstalledInspectErrorKind::Usage => 2,
        orna_server::InstalledInspectErrorKind::Kernel => 1,
        orna_server::InstalledInspectErrorKind::Rendering => 5,
        orna_server::InstalledInspectErrorKind::Internal => 7,
        // A future closed kind falls back to internal.
        _ => 7,
    }
}

const fn failure_name(failure: CallFailure) -> &'static str {
    match failure {
        CallFailure::ExecuteDenied => "EXECUTE_DENIED",
        CallFailure::TargetUnavailable => "TARGET_UNAVAILABLE",
        CallFailure::ClientEvaluationFailed => "CLIENT_EVALUATION_FAILED",
        CallFailure::InternalFailure => "INTERNAL_FAILURE",
    }
}

fn valid_source_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('-')
        && !path
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
}

fn write_stderr_line(line: &str) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::ParameterId;
    use orna_server::RuntimeFamily;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    /// Writes one caller-owned state value payload below the system temp
    /// directory and returns its path.
    fn state_value_file(bytes: &[u8]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "orna-state-value-{}-{}.bin",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).expect("state value file must write");
        path
    }

    #[test]
    fn accepts_the_exact_server_commands() {
        assert_eq!(
            parse_command(arguments(&["orna", "server", "run"])),
            Some(Command::Run)
        );
        assert_eq!(
            parse_command(arguments(&["orna", "server", "backend-shell"])),
            Some(Command::BackendShell)
        );
        assert_eq!(
            parse_command(arguments(&["orna", "server", "upgrade"])),
            Some(Command::Upgrade)
        );
    }

    #[test]
    fn accepts_the_exact_sole_version_command() {
        assert_eq!(
            parse_command(arguments(&["orna", "--version"])),
            Some(Command::Version)
        );
        assert_eq!(
            parse_command(arguments(&["/usr/bin/orna", "--version"])),
            Some(Command::Version)
        );
    }

    #[test]
    fn rejects_malformed_and_extra_version_shapes() {
        for values in [
            vec!["orna"],
            vec!["orna", "-v"],
            vec!["orna", "-version"],
            vec!["orna", "version"],
            vec!["orna", "--Version"],
            vec!["orna", "--version=0.1.0"],
            vec!["orna", "--version", "0.1.0"],
            vec!["orna", "--version", "extra"],
            vec!["orna", "--version", "--version"],
            vec!["orna", "server", "--version"],
            vec!["orna", "--", "--version"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn accepts_one_exact_source_check_path() {
        assert_eq!(
            parse_command(arguments(&["orna", "source", "check", "app.orna"])),
            Some(Command::SourceCheck("app.orna".to_owned()))
        );
    }

    #[test]
    fn accepts_one_exact_source_apply_path() {
        assert_eq!(
            parse_command(arguments(&["orna", "source", "apply", "app.orna"])),
            Some(Command::SourceApply("app.orna".to_owned()))
        );
    }

    #[test]
    fn accepts_one_exact_canonical_raw_call_identity() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let canonical = function.canonical();
        assert_eq!(
            parse_command(arguments(&["orna", "raw-call", &canonical])),
            Some(Command::RawCall(function, RawCallParameters::None))
        );
        let health = CATALOGUE_HEALTH_FUNCTION_ID;
        assert_eq!(
            parse_command(arguments(&["orna", "raw-call", "sys.catalog.health"])),
            Some(Command::RawCall(health, RawCallParameters::None))
        );
        assert_eq!(
            parse_command(arguments(&["orna", "raw-call", &health.canonical()])),
            Some(Command::RawCall(health, RawCallParameters::None))
        );
        let parameter = ParameterId::from_bytes([0x22; 16]);
        let parameter_canonical = parameter.canonical();
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "raw-call",
                &canonical,
                &parameter_canonical
            ])),
            Some(Command::RawCall(
                function,
                RawCallParameters::One(parameter)
            ))
        );
        for values in [
            vec!["orna", "raw-call"],
            vec!["orna", "raw-call", "sys.catalog.HEALTH"],
            vec!["orna", "raw-call", "sys.catalog.health.extra"],
            vec!["orna", "raw-call", "sys.catalog.health "],
            vec!["orna", "raw-call", "function:not-an-id"],
            vec!["orna", "raw-call", &canonical, "extra"],
            vec!["orna", "raw-call", &canonical, "parameter:not-an-id"],
            vec![
                "orna",
                "raw-call",
                "sys.catalog.health",
                &parameter_canonical,
            ],
            vec![
                "orna",
                "raw-call",
                &canonical,
                &parameter_canonical,
                "extra",
            ],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn accepts_two_distinct_raw_call_parameter_ids_in_token_order_and_rejects_duplicates() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let first = ParameterId::from_bytes([0x22; 16]);
        let second = ParameterId::from_bytes([0x33; 16]);
        let function_canonical = function.canonical();
        let first_canonical = first.canonical();
        let second_canonical = second.canonical();
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "raw-call",
                &function_canonical,
                &first_canonical,
                &second_canonical,
            ])),
            Some(Command::RawCall(
                function,
                RawCallParameters::Pair(first, second),
            ))
        );
        for values in [
            vec![
                "orna",
                "raw-call",
                &function_canonical,
                &first_canonical,
                &first_canonical,
            ],
            vec![
                "orna",
                "raw-call",
                &function_canonical,
                &first_canonical,
                &second_canonical,
                "extra",
            ],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn accepts_one_exact_security_grant_execute_identity() {
        let function = FunctionId::from_bytes([0x33; 16]);
        let canonical = function.canonical();
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "grant-execute",
                &canonical
            ])),
            Some(Command::SecurityGrantExecute(function))
        );
    }

    #[test]
    fn accepts_the_security_admin_identity_commands() {
        let principal = PrincipalId::from_bytes([0x44; 16]);
        let canonical = principal.canonical();
        let request = |operation| {
            Some(Command::SecurityAdmin(
                orna_server::InstalledSecurityAdminRequest::new(operation),
            ))
        };
        assert_eq!(
            parse_command(arguments(&["orna", "security", "whoami"])),
            request(orna_server::InstalledSecurityAdminOperation::SessionPrincipal)
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna", "security", "user", "create", &canonical,
            ])),
            request(
                orna_server::InstalledSecurityAdminOperation::CreatePrincipal {
                    principal,
                    kind: orna_core::security::PrincipalKind::User,
                }
            )
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna", "security", "user", "disable", &canonical,
            ])),
            request(orna_server::InstalledSecurityAdminOperation::DisablePrincipal { principal })
        );
    }

    #[test]
    fn accepts_security_role_and_grant_shapes() {
        let role = PrincipalId::from_bytes([0x44; 16]);
        let member = PrincipalId::from_bytes([0x55; 16]);
        let grantee = PrincipalId::from_bytes([0x66; 16]);
        let function = FunctionId::from_bytes([0x77; 16]);
        let role_canonical = role.canonical();
        let member_canonical = member.canonical();
        let grantee_canonical = grantee.canonical();
        let function_canonical = function.canonical();
        let request = |operation| {
            Some(Command::SecurityAdmin(
                orna_server::InstalledSecurityAdminRequest::new(operation),
            ))
        };
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "role",
                "create",
                &role_canonical,
            ])),
            request(orna_server::InstalledSecurityAdminOperation::CreateRole { role })
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "role",
                "grant",
                &role_canonical,
                &member_canonical,
            ])),
            request(orna_server::InstalledSecurityAdminOperation::GrantRole { role, member })
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "grants",
                "grant",
                &grantee_canonical,
                "execute",
                &function_canonical,
            ])),
            request(
                orna_server::InstalledSecurityAdminOperation::GrantPrivilege {
                    grantee,
                    class: orna_core::security::PrivilegeClass::Execute,
                    object: Some(function),
                }
            )
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "grants",
                "list",
                &grantee_canonical,
            ])),
            request(orna_server::InstalledSecurityAdminOperation::ListGrants { grantee })
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "check",
                "can-execute",
                &grantee_canonical,
                &function_canonical,
            ])),
            request(orna_server::InstalledSecurityAdminOperation::CanExecute {
                principal: grantee,
                function,
            })
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "security",
                "check",
                "has-privilege",
                &grantee_canonical,
                "security_admin",
            ])),
            request(orna_server::InstalledSecurityAdminOperation::HasPrivilege {
                principal: grantee,
                class: orna_core::security::PrivilegeClass::SecurityAdmin,
                object: None,
            })
        );
    }

    #[test]
    fn rejects_malformed_security_admin_shapes() {
        let grantee_canonical = PrincipalId::from_bytes([0x88; 16]).canonical();
        let function_canonical = FunctionId::from_bytes([0x99; 16]).canonical();
        for values in [
            vec!["orna", "security", "user"],
            vec!["orna", "security", "user", "create"],
            vec!["orna", "security", "user", "bogus", "principal:deadbeef"],
            vec!["orna", "security", "role", "revoke", "r1"],
            vec!["orna", "security", "grants", "list"],
            vec!["orna", "security", "grants", "list", "not-a-principal"],
            vec![
                "orna",
                "security",
                "grants",
                "list",
                grantee_canonical.as_str(),
                "execute",
            ],
            vec![
                "orna",
                "security",
                "grants",
                "list",
                grantee_canonical.as_str(),
                "execute",
                function_canonical.as_str(),
            ],
            vec!["orna", "security", "grants", "grant", "g1", "not-a-class"],
            vec!["orna", "security", "check", "can-execute", "p1"],
            vec!["orna", "security", "check", "bogus"],
        ] {
            assert_eq!(
                parse_command(arguments(&values)),
                None,
                "expected rejection: {values:?}"
            );
        }
    }

    #[test]
    fn public_call_failure_names_are_exact() {
        assert_eq!(failure_name(CallFailure::ExecuteDenied), "EXECUTE_DENIED");
        assert_eq!(
            failure_name(CallFailure::TargetUnavailable),
            "TARGET_UNAVAILABLE"
        );
        assert_eq!(
            failure_name(CallFailure::ClientEvaluationFailed),
            "CLIENT_EVALUATION_FAILED"
        );
        assert_eq!(
            failure_name(CallFailure::InternalFailure),
            "INTERNAL_FAILURE"
        );
    }

    #[test]
    fn ignores_argv0_but_rejects_missing_or_extra_tokens() {
        assert_eq!(
            parse_command(arguments(&["/some/path/orna", "server", "backend-shell",])),
            Some(Command::BackendShell)
        );
        for values in [
            vec!["orna", "server"],
            vec!["orna", "backend-shell"],
            vec!["orna", "server", "backend-shell", "--flag"],
            vec!["orna", "server", "backend-shell", "select 1"],
            vec!["orna", "server", "upgrade", "--force"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None);
        }
    }

    #[test]
    fn rejects_flags_and_sql_in_the_command_position() {
        assert_eq!(
            parse_command(arguments(&["orna", "--server", "backend-shell",])),
            None
        );
        assert_eq!(
            parse_command(arguments(&["orna", "server", "--command",])),
            None
        );
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "server",
                "backend-shell",
                "select",
                "1",
            ])),
            None
        );
    }

    #[test]
    fn rejects_invalid_source_check_shapes_and_paths() {
        for values in [
            vec!["orna", "source"],
            vec!["orna", "source", "check"],
            vec!["orna", "source", "check", ""],
            vec!["orna", "source", "check", "-"],
            vec!["orna", "source", "check", "-x"],
            vec!["orna", "source", "check", "a", "b"],
            vec!["orna", "source", "--check", "a"],
            vec!["orna", "--source", "check", "a"],
            vec!["orna", "source", "check", "line\nbreak"],
            vec!["orna", "source", "check", "line\u{2028}break"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
        assert_eq!(
            parse_command(arguments(&["orna", "source", "check", "./-x"])),
            Some(Command::SourceCheck("./-x".to_owned()))
        );
    }

    #[test]
    fn rejects_invalid_source_apply_shapes_and_paths() {
        for values in [
            vec!["orna", "source", "apply"],
            vec!["orna", "source", "apply", ""],
            vec!["orna", "source", "apply", "-"],
            vec!["orna", "source", "apply", "-x"],
            vec!["orna", "source", "apply", "a", "b"],
            vec!["orna", "source", "--apply", "a"],
            vec!["orna", "--source", "apply", "a"],
            vec!["orna", "source", "apply", "line\nbreak"],
            vec!["orna", "source", "apply", "line\u{2028}break"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
        assert_eq!(
            parse_command(arguments(&["orna", "source", "apply", "./-x"])),
            Some(Command::SourceApply("./-x".to_owned()))
        );
    }

    #[test]
    fn rejects_invalid_security_grant_execute_shapes() {
        let canonical = FunctionId::from_bytes([0x33; 16]).canonical();
        for values in [
            vec!["orna", "security"],
            vec!["orna", "security", "grant-execute"],
            vec!["orna", "security", "grant"],
            vec!["orna", "security", "revoke-execute", &canonical],
            vec!["orna", "security", "grant-execute", "function:not-an-id"],
            vec!["orna", "security", "grant-execute", "sys.catalog.health"],
            vec!["orna", "security", "grant-execute", &canonical, "extra"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_tokens() {
        use std::os::unix::ffi::OsStringExt;

        let non_unicode = OsString::from_vec(b"server\xff".to_vec());
        assert_eq!(
            parse_command(vec![
                OsString::from("orna"),
                non_unicode,
                OsString::from("backend-shell"),
            ]),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_source_apply_path() {
        use std::os::unix::ffi::OsStringExt;

        let non_unicode = OsString::from_vec(b"app\xff.orna".to_vec());
        assert_eq!(
            parse_command(vec![
                OsString::from("orna"),
                OsString::from("source"),
                OsString::from("apply"),
                non_unicode,
            ]),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_security_grant_execute_identity() {
        use std::os::unix::ffi::OsStringExt;

        let non_unicode = OsString::from_vec(b"function:\xff".to_vec());
        assert_eq!(
            parse_command(vec![
                OsString::from("orna"),
                OsString::from("security"),
                OsString::from("grant-execute"),
                non_unicode,
            ]),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_raw_call_parameter() {
        use std::os::unix::ffi::OsStringExt;

        let canonical = FunctionId::from_bytes([0x11; 16]).canonical();
        let non_unicode = OsString::from_vec(b"parameter:\xff".to_vec());
        assert_eq!(
            parse_command(vec![
                OsString::from("orna"),
                OsString::from("raw-call"),
                OsString::from(canonical),
                non_unicode,
            ]),
            None
        );
    }

    fn invoke_command(
        target: InvocationTarget,
        arguments: Vec<CliArgumentInput>,
        output: Option<String>,
        trace: Option<InvocationTracePolicy>,
        no_progress: bool,
        explain: bool,
        runtime: Option<RuntimeFamily>,
    ) -> Command {
        Command::Invoke(InvokeArguments {
            target,
            arguments,
            output,
            trace,
            no_progress,
            explain,
            runtime,
        })
    }

    fn echo_target() -> InvocationTarget {
        InvocationTarget::qualified_name(
            QualifiedSemanticName::new(["std", "invoke", "echo"]).expect("qualified name"),
        )
        .expect("target")
    }

    fn write_temp_args_file(contents: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "orna-invoke-args-file-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, contents).expect("write temporary args file");
        path
    }

    #[test]
    fn accepts_one_exact_invoke_qualified_name_target() {
        assert_eq!(
            parse_command(arguments(&["orna", "invoke", "std.invoke.echo"])),
            Some(invoke_command(
                echo_target(),
                Vec::new(),
                None,
                None,
                false,
                false,
                None,
            ))
        );
    }

    #[test]
    fn accepts_one_exact_invoke_canonical_identity_target() {
        let function = FunctionId::from_bytes([0x44; 16]);
        let canonical = function.canonical();
        assert_eq!(
            parse_command(arguments(&["orna", "invoke", &canonical])),
            Some(invoke_command(
                InvocationTarget::function_id(function),
                Vec::new(),
                None,
                None,
                false,
                false,
                None,
            ))
        );
    }

    #[test]
    fn accepts_invoke_friendly_flag_sugar() {
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                "std.invoke.echo",
                "--name",
                "hello"
            ])),
            Some(invoke_command(
                echo_target(),
                vec![CliArgumentInput::Friendly {
                    name: "name".to_owned(),
                    value: "hello".to_owned(),
                }],
                None,
                None,
                false,
                false,
                None,
            ))
        );
    }

    #[test]
    fn accepts_invoke_canonical_arg_pairs() {
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                "std.invoke.echo",
                "--arg",
                "p_name=value",
                "--arg",
                "p_count=41",
            ])),
            Some(invoke_command(
                echo_target(),
                vec![
                    CliArgumentInput::Canonical {
                        parameter: "p_name".to_owned(),
                        value: "value".to_owned(),
                    },
                    CliArgumentInput::Canonical {
                        parameter: "p_count".to_owned(),
                        value: "41".to_owned(),
                    },
                ],
                None,
                None,
                false,
                false,
                None,
            ))
        );
    }

    #[test]
    fn accepts_every_invoke_trace_value() {
        for (value, policy) in [
            ("off", InvocationTracePolicy::Off),
            ("basic", InvocationTracePolicy::Basic),
            ("normal", InvocationTracePolicy::Normal),
            ("verbose", InvocationTracePolicy::Verbose),
            ("profile", InvocationTracePolicy::Profile),
        ] {
            assert_eq!(
                parse_command(arguments(&[
                    "orna",
                    "invoke",
                    "std.invoke.echo",
                    "--trace",
                    value,
                ])),
                Some(invoke_command(
                    echo_target(),
                    Vec::new(),
                    None,
                    Some(policy),
                    false,
                    false,
                    None,
                )),
                "{value}"
            );
        }
    }

    #[test]
    fn accepts_invoke_options_in_any_order() {
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                "std.invoke.echo",
                "--arg",
                "p_name=value",
                "--p_count",
                "41",
                "--output",
                "json",
                "--runtime",
                "tty",
                "--trace",
                "normal",
                "--explain",
                "--no-progress",
            ])),
            Some(invoke_command(
                echo_target(),
                vec![
                    CliArgumentInput::Canonical {
                        parameter: "p_name".to_owned(),
                        value: "value".to_owned(),
                    },
                    CliArgumentInput::Friendly {
                        name: "p_count".to_owned(),
                        value: "41".to_owned(),
                    },
                ],
                Some("json".to_owned()),
                Some(InvocationTracePolicy::Normal),
                true,
                true,
                Some(RuntimeFamily::Tty),
            ))
        );
    }

    #[test]
    fn global_runtime_flag_parses_before_the_command() {
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "--runtime",
                "tty",
                "invoke",
                "std.invoke.echo"
            ])),
            Some(invoke_command(
                echo_target(),
                Vec::new(),
                None,
                None,
                false,
                false,
                Some(RuntimeFamily::Tty),
            ))
        );
        for values in [
            vec!["orna", "--runtime", "qt", "invoke", "std.invoke.echo"],
            vec!["orna", "--runtime", "gtk", "invoke", "std.invoke.echo"],
            vec!["orna", "--runtime"],
            vec!["orna", "--runtime", "tty"],
            vec!["orna", "--runtime", "tty", "invoke"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn post_command_runtime_flag_is_explicit() {
        // The post-command form is accepted...
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                "std.invoke.echo",
                "--runtime",
                "tty"
            ])),
            Some(invoke_command(
                echo_target(),
                Vec::new(),
                None,
                None,
                false,
                false,
                Some(RuntimeFamily::Tty),
            ))
        );
        // ...and `--runtime` is never a friendly argument named `runtime`:
        // a missing value or an unknown family is a usage error instead of
        // silently binding a parameter called `runtime`. The accepted form
        // above also pins `arguments` to the empty vector, so no friendly
        // argument named `runtime` is ever emitted.
        for values in [
            vec!["orna", "invoke", "std.invoke.echo", "--runtime"],
            vec!["orna", "invoke", "std.invoke.echo", "--runtime", "qt"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn accepts_an_invoke_args_file_document() {
        let path = write_temp_args_file(
            r#"{"target": {"qualified_name": "std.invoke.echo"}, "arguments": {"p_name": "value"}}"#,
        );
        let path_text = path.to_str().expect("temp path is unicode");
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                "std.invoke.echo",
                "--args-file",
                path_text,
            ])),
            Some(invoke_command(
                echo_target(),
                vec![CliArgumentInput::Canonical {
                    parameter: "p_name".to_owned(),
                    value: "value".to_owned(),
                }],
                None,
                None,
                false,
                false,
                None,
            ))
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn accepts_an_invoke_args_file_with_a_matching_canonical_target() {
        let function = FunctionId::from_bytes([0x44; 16]);
        let canonical = function.canonical();
        let path = write_temp_args_file(&format!(
            r#"{{"target": {{"function_id": "{canonical}"}}, "arguments": {{"p_name": "value"}}}}"#
        ));
        let path_text = path.to_str().expect("temp path is unicode");
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                &canonical,
                "--args-file",
                path_text
            ])),
            Some(invoke_command(
                InvocationTarget::function_id(function),
                vec![CliArgumentInput::Canonical {
                    parameter: "p_name".to_owned(),
                    value: "value".to_owned(),
                }],
                None,
                None,
                false,
                false,
                None,
            ))
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_invoke_usage_shapes() {
        let function = FunctionId::from_bytes([0x44; 16]);
        let canonical = function.canonical();
        let mut invalid = vec![
            vec!["orna", "invoke"],
            vec!["orna", "invoke", "std.invoke.echo", "extra"],
            vec!["orna", "invoke", "std.invoke.echo", "std.invoke.echo"],
            vec!["orna", "invoke", "std.invoke.echo", "--bogus"],
            vec!["orna", "invoke", "std.invoke.echo", "--trace", "chatty"],
            vec!["orna", "invoke", "std.invoke.echo", "--output", ""],
            vec!["orna", "invoke", "std.invoke.echo", "--arg", "p_name"],
            vec!["orna", "invoke", "std.invoke.echo", "--arg", "=value"],
            vec!["orna", "invoke", "std.invoke.echo", "--arg"],
            vec!["orna", "invoke", "std.invoke.echo", "--trace"],
            vec!["orna", "invoke", "std.invoke.echo", "--output"],
            vec!["orna", "invoke", "std.invoke.echo", "--runtime"],
            vec!["orna", "invoke", "std.invoke.echo", "--runtime", "qt"],
            vec!["orna", "invoke", "std.invoke.echo", "--name"],
            vec!["orna", "invoke", "echo"],
            vec!["orna", "invoke", ""],
            vec!["orna", "invoke", "function:not-an-id"],
            vec!["orna", "invoke", "std.invoke.echo", "trailing", "extra"],
        ];
        invalid.push(vec!["orna", "invoke", &canonical, "--no-progress", "extra"]);
        for values in invalid {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn rejects_invoke_args_file_shapes() {
        let mismatched_target = write_temp_args_file(
            r#"{"target": {"qualified_name": "sys.other.func"}, "arguments": {"p_name": "value"}}"#,
        );
        let extra_key = write_temp_args_file(
            r#"{"target": {"qualified_name": "std.invoke.echo"}, "arguments": {"p_name": "value"}, "trace": "normal"}"#,
        );
        let typed_value = write_temp_args_file(
            r#"{"target": {"qualified_name": "std.invoke.echo"}, "arguments": {"p_name": 41}}"#,
        );
        let both_targets = write_temp_args_file(
            r#"{"target": {"function_id": "function:289144gj289144gj289144gj28", "qualified_name": "std.invoke.echo"}, "arguments": {}}"#,
        );
        let neither_target = write_temp_args_file(r#"{"target": {}, "arguments": {}}"#);
        let malformed_json = write_temp_args_file(r#"{"target": "std.invoke.echo""#);
        let missing_file = std::env::temp_dir().join(format!(
            "orna-invoke-args-file-missing-{}.json",
            std::process::id()
        ));
        let mut paths = Vec::new();
        for path in [
            mismatched_target,
            extra_key,
            typed_value,
            both_targets,
            neither_target,
            malformed_json,
        ] {
            paths.push(path.to_str().expect("temp path is unicode").to_owned());
            let _ = std::fs::remove_file(&path);
        }
        paths.push(
            missing_file
                .to_str()
                .expect("temp path is unicode")
                .to_owned(),
        );
        let invalid = paths
            .iter()
            .map(|path| {
                vec![
                    "orna",
                    "invoke",
                    "std.invoke.echo",
                    "--args-file",
                    path.as_str(),
                ]
            })
            .collect::<Vec<_>>();
        for values in invalid {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_invoke_target() {
        use std::os::unix::ffi::OsStringExt;

        let non_unicode = OsString::from_vec(b"std.invoke.\xff".to_vec());
        assert_eq!(
            parse_command(vec![
                OsString::from("orna"),
                OsString::from("invoke"),
                non_unicode,
            ]),
            None
        );
    }

    #[test]
    fn invoke_error_exit_codes_follow_the_spec_table() {
        use orna_server::{InstalledInvokeError, InstalledInvokeErrorKind};

        for (kind, expected) in [
            (InstalledInvokeErrorKind::Usage, 2),
            (InstalledInvokeErrorKind::Authentication, 3),
            (InstalledInvokeErrorKind::Authorisation, 4),
            (InstalledInvokeErrorKind::Presentation, 5),
            (InstalledInvokeErrorKind::Cancelled, 6),
            (InstalledInvokeErrorKind::Internal, 7),
        ] {
            let error = InstalledInvokeError::new(kind, "message".to_owned());
            assert_eq!(invoke_error_exit_code(&error), expected, "{kind:?}");
        }
    }

    #[test]
    fn accepts_an_exact_state_get_command() {
        let root = FunctionId::from_bytes([0x11; 16]);
        assert_eq!(
            parse_command(arguments(&["orna", "state", "get", &root.canonical()])),
            Some(Command::State(orna_server::InstalledUserStateRequest::new(
                orna_server::InstalledUserStateOperation::Load {
                    root_function: root,
                    state_profile: String::new(),
                    instances: Vec::new(),
                    expected_types: Vec::new(),
                }
            )))
        );
    }

    #[test]
    fn accepts_state_get_filters_and_expected_types() {
        let root = FunctionId::from_bytes([0x11; 16]);
        let function = FunctionId::from_bytes([0x21; 16]);
        let state_slot = StateSlotId::from_bytes([0x31; 16]);
        let value_type = TypeId::from_bytes([0x41; 16]);
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "state",
                "get",
                &root.canonical(),
                "--profile",
                "p1",
                "--instance",
                &function.canonical(),
                "--instance-key",
                "player-7",
                "--expect-type",
                &function.canonical(),
                &state_slot.canonical(),
                &value_type.canonical(),
            ])),
            Some(Command::State(orna_server::InstalledUserStateRequest::new(
                orna_server::InstalledUserStateOperation::Load {
                    root_function: root,
                    state_profile: "p1".to_owned(),
                    instances: vec![orna_server::InstalledUserStateInstance {
                        function,
                        instance_key: "player-7".to_owned(),
                    }],
                    expected_types: vec![orna_server::InstalledUserStateExpectedType {
                        function,
                        state_slot,
                        value_type,
                    }],
                }
            )))
        );
    }

    #[test]
    fn accepts_an_exact_state_set_command() {
        let root = FunctionId::from_bytes([0x11; 16]);
        let function = FunctionId::from_bytes([0x21; 16]);
        let state_slot = StateSlotId::from_bytes([0x31; 16]);
        let value_type = TypeId::from_bytes([0x41; 16]);
        let path = state_value_file(&[0x0a, 0x0b]);
        let command = parse_command(arguments(&[
            "orna",
            "state",
            "set",
            &root.canonical(),
            "--profile",
            "p1",
            "--function",
            &function.canonical(),
            "--instance-key",
            "player-7",
            "--slot",
            &state_slot.canonical(),
            "--revision",
            "create",
            "--type",
            &value_type.canonical(),
            "--value-file",
            &path.to_string_lossy(),
        ]));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            command,
            Some(Command::State(orna_server::InstalledUserStateRequest::new(
                orna_server::InstalledUserStateOperation::Write {
                    root_function: root,
                    state_profile: "p1".to_owned(),
                    change: orna_server::InstalledUserStateChange {
                        function,
                        instance_key: "player-7".to_owned(),
                        state_slot,
                        expected_revision: None,
                        value_type,
                        value_bytes: vec![0x0a, 0x0b],
                    },
                }
            )))
        );
    }

    #[test]
    fn rejects_malformed_state_shapes() {
        for values in [
            vec!["orna", "state"],
            vec!["orna", "state", "get"],
            vec!["orna", "state", "get", "not-an-id"],
            vec!["orna", "state", "get", "function:x", "extra"],
            vec!["orna", "state", "set"],
            vec!["orna", "state", "set", "function:x"],
            vec!["orna", "state", "set", "function:x", "--function"],
            vec!["orna", "state", "set", "function:x", "--revision", "seven"],
            vec!["orna", "state", "set", "function:x", "--unknown", "value"],
            vec!["orna", "state", "dump"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn state_error_exit_codes_follow_the_closed_table() {
        use orna_server::{InstalledUserStateError, InstalledUserStateErrorKind};

        for (kind, expected) in [
            (InstalledUserStateErrorKind::Authentication, 3),
            (InstalledUserStateErrorKind::State, 1),
            (InstalledUserStateErrorKind::Presentation, 5),
            (InstalledUserStateErrorKind::Internal, 7),
        ] {
            let error = InstalledUserStateError::new(kind, "message".to_owned());
            assert_eq!(state_error_exit_code(&error), expected, "{kind:?}");
        }
    }

    #[test]
    fn accepts_an_exact_inspect_command() {
        let invocation = InvocationId::from_bytes([0x11; 16]);
        assert_eq!(
            parse_command(arguments(&["orna", "inspect", &invocation.canonical()])),
            Some(Command::Inspect(orna_server::InstalledInspectRequest::new(
                invocation, None, None, false, 0, false, false, false, false,
            )))
        );
    }

    #[test]
    fn accepts_inspect_projection_trace_and_classifiers() {
        let invocation = InvocationId::from_bytes([0x11; 16]);
        let epoch = InspectEpochId::from_bytes([0x22; 16]);
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "inspect",
                &invocation.canonical(),
                "--projection",
                "state_cells",
                "--trace",
                "--after",
                "3",
                "--include-values",
                "--include-source",
                "--include-security",
                "--include-runtime",
                "--epoch",
                &epoch.canonical(),
            ])),
            Some(Command::Inspect(orna_server::InstalledInspectRequest::new(
                invocation,
                Some(epoch),
                Some(orna_server::InstalledInspectProjection::StateCells),
                true,
                3,
                true,
                true,
                true,
                true,
            )))
        );
    }

    #[test]
    fn accepts_every_inspect_projection_name() {
        for (name, projection) in [
            (
                "invocation_nodes",
                orna_server::InstalledInspectProjection::InvocationNodes,
            ),
            ("calls", orna_server::InstalledInspectProjection::Calls),
            (
                "resources",
                orna_server::InstalledInspectProjection::Resources,
            ),
            (
                "state_cells",
                orna_server::InstalledInspectProjection::StateCells,
            ),
            ("ui_nodes", orna_server::InstalledInspectProjection::UiNodes),
            (
                "presentation_candidates",
                orna_server::InstalledInspectProjection::PresentationCandidates,
            ),
            (
                "runtime_bindings",
                orna_server::InstalledInspectProjection::RuntimeBindings,
            ),
            (
                "security_decisions",
                orna_server::InstalledInspectProjection::SecurityDecisions,
            ),
        ] {
            let invocation = InvocationId::from_bytes([0x11; 16]);
            let parsed = parse_command(arguments(&[
                "orna",
                "inspect",
                &invocation.canonical(),
                "--projection",
                name,
            ]));
            assert_eq!(
                parsed,
                Some(Command::Inspect(orna_server::InstalledInspectRequest::new(
                    invocation,
                    None,
                    Some(projection),
                    false,
                    0,
                    false,
                    false,
                    false,
                    false,
                ))),
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_malformed_inspect_shapes() {
        let invocation = InvocationId::from_bytes([0x11; 16]);
        let invocation_text = invocation.canonical();
        let epoch_text = InspectEpochId::from_bytes([0x22; 16]).canonical();
        for values in [
            vec!["orna", "inspect"],
            vec!["orna", "inspect", "not-an-id"],
            vec!["orna", "inspect", "invocation:x"],
            vec!["orna", "inspect", invocation_text.as_str(), "extra"],
            vec![
                "orna",
                "inspect",
                invocation_text.as_str(),
                "--projection",
                "unknown",
            ],
            vec!["orna", "inspect", invocation_text.as_str(), "--projection"],
            vec![
                "orna",
                "inspect",
                invocation_text.as_str(),
                "--after",
                "seven",
            ],
            vec!["orna", "inspect", invocation_text.as_str(), "--after"],
            vec!["orna", "inspect", invocation_text.as_str(), "--after", "-1"],
            vec![
                "orna",
                "inspect",
                invocation_text.as_str(),
                "--epoch",
                "not-an-epoch",
            ],
            vec![
                "orna",
                "inspect",
                invocation_text.as_str(),
                "--epoch",
                epoch_text.as_str(),
                "extra",
            ],
            vec![
                "orna",
                "inspect",
                invocation_text.as_str(),
                "--unknown",
                "value",
            ],
            vec![
                "orna",
                "inspect",
                invocation_text.as_str(),
                "--trace",
                "extra",
            ],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn inspect_error_exit_codes_follow_the_closed_table() {
        use orna_server::{InstalledInspectError, InstalledInspectErrorKind};

        for (kind, expected) in [
            (InstalledInspectErrorKind::Usage, 2),
            (InstalledInspectErrorKind::Kernel, 1),
            (InstalledInspectErrorKind::Rendering, 5),
            (InstalledInspectErrorKind::Internal, 7),
        ] {
            let error = InstalledInspectError::new(kind, "message".to_owned());
            assert_eq!(inspect_error_exit_code(&error), expected, "{kind:?}");
        }
    }

    #[test]
    fn usage_diagnostic_is_exact() {
        assert_eq!(
            USAGE,
            "Usage:\n  orna --version\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna source apply <file.orna>\n  orna source diff <file.orna>\n  orna security grant-execute <canonical-function-id>\n  orna security user create|disable <canonical-principal-id>\n  orna security role create|grant|revoke <canonical-principal-id> [canonical-principal-id]\n  orna security grants grant|revoke <canonical-principal-id> <class> [canonical-function-id]\n  orna security grants list <canonical-principal-id>\n  orna security check can-execute <canonical-principal-id> <canonical-function-id>\n  orna security check has-privilege <canonical-principal-id> <class> [canonical-function-id]\n  orna security whoami\n  orna raw-call <canonical-function-id>\n  orna raw-call <canonical-function-id> <canonical-parameter-id>\n  orna [--runtime <family>] invoke <qualified-name | canonical-function-id> [options]\n  orna state get <root-function-id> [options]\n  orna state set <root-function-id> [options]\n  orna inspect <invocation-id> [options]"
        );
    }
}
