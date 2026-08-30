#![allow(clippy::single_element_loop)]
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use orna_protocol::CallFailure;

mod cli;
mod source_check;

use cli::{Command, ParsedInvocation, RawCallParameters, USAGE, parse_invocation, write_help};

#[cfg(test)]
use cli::{
    ColorChoice, HELP_TOP_LEVEL, HelpTopic, InvokeArguments, help_text, parse_command, render_help,
};
#[cfg(test)]
use orna_core::{
    FunctionId, InspectEpochId, InvocationId, PrincipalId, StateSlotId, TypeId,
    catalogue::QualifiedSemanticName,
    invocation::{InvocationTarget, InvocationTracePolicy},
    invocation_binding::CliArgumentInput,
    security::CATALOGUE_HEALTH_FUNCTION_ID,
};
#[cfg(test)]
use std::{ffi::OsString, path::PathBuf};

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let Some(parsed) = parse_invocation(arguments) else {
        write_stderr_line(USAGE);
        return ExitCode::from(2);
    };

    let ParsedInvocation {
        color,
        endpoint,
        endpoint_explicit,
        command,
    } = parsed;
    let endpoint_is_unsupported = endpoint_command_is_unsupported(&endpoint, &command);
    if endpoint_explicit
        && !matches!(&command, Command::Help(_) | Command::Version)
        && endpoint_is_unsupported
    {
        write_stderr_line(
            "orna: the selected endpoint needs a client transport that is not available yet",
        );
        return ExitCode::from(3);
    }
    match command {
        Command::Help(topic) => {
            let stdout = io::stdout();
            let terminal = stdout.is_terminal();
            let mut stdout = stdout.lock();
            match write_help(&mut stdout, topic, color, terminal) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
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
        Command::Run => match endpoint {
            orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                match orna_server::run_sqlite_server(path) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        write_stderr_line(&error.to_string());
                        ExitCode::from(1)
                    }
                }
            }
            _ => match orna_server::run_embedded_server() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            },
        },
        Command::BackendShell => match orna_server::run_backend_shell() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                write_stderr_line(&error.to_string());
                ExitCode::from(1)
            }
        },
        Command::SourceCheck(path) => {
            let stderr = io::stderr();
            let terminal = stderr.is_terminal();
            let mut stderr = stderr.lock();
            match source_check::run_with_output(
                &path,
                &mut stderr,
                terminal,
                color.enabled(terminal),
            ) {
                source_check::SourceCheckResult::Success => ExitCode::SUCCESS,
                source_check::SourceCheckResult::Failure => ExitCode::from(1),
                source_check::SourceCheckResult::Usage => {
                    let _ = writeln!(stderr, "{USAGE}");
                    ExitCode::from(2)
                }
            }
        }
        Command::SourceApply(path) => match &endpoint {
            orna_client::endpoint::DatabaseEndpoint::LocalPath {
                path: database_path,
            } => match orna_server::run_sqlite_source_apply(database_path, &path) {
                Ok(outcome) => write_sqlite_source_apply_outcome(outcome, color),
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            },
            _ => match orna_server::run_installed_source_apply(&path) {
                Ok(orna_server::InstalledSourceApplyOutcome::Diagnostics(diagnostics)) => {
                    let stderr = io::stderr();
                    let terminal = stderr.is_terminal();
                    let mut stderr = stderr.lock();
                    let bytes = if color.enabled(terminal) {
                        diagnostics.coloured_bytes()
                    } else if terminal {
                        diagnostics.human_bytes()
                    } else {
                        diagnostics.as_bytes()
                    };
                    let _ = stderr.write_all(bytes).and_then(|()| stderr.flush());
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
        },
        Command::SourceDiff(path) => match &endpoint {
            orna_client::endpoint::DatabaseEndpoint::LocalPath {
                path: database_path,
            } => match orna_server::run_sqlite_source_diff(database_path, &path) {
                Ok(outcome) => write_sqlite_source_diff_outcome(outcome, color),
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            },
            _ => match orna_server::run_installed_source_diff(&path) {
                Ok(orna_server::InstalledSourceDiffOutcome::Diagnostics(diagnostics)) => {
                    let stderr = io::stderr();
                    let terminal = stderr.is_terminal();
                    let mut stderr = stderr.lock();
                    let bytes = if color.enabled(terminal) {
                        diagnostics.coloured_bytes()
                    } else if terminal {
                        diagnostics.human_bytes()
                    } else {
                        diagnostics.as_bytes()
                    };
                    let _ = stderr.write_all(bytes).and_then(|()| stderr.flush());
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
        },
        Command::SecurityGrantExecute(function) => {
            let result = match endpoint {
                orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                    orna_server::run_sqlite_security_grant_execute(path, function)
                        .map_err(|error| error.to_string())
                }
                _ => orna_server::security_admin::run_installed_security_grant(function)
                    .map_err(|error| error.to_string()),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
        Command::SecurityAdmin(request) => {
            let mut stdout = std::io::stdout().lock();
            let result = match endpoint {
                orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                    orna_server::run_sqlite_security_admin(path, request, &mut stdout)
                }
                _ => orna_server::run_installed_security_admin(request, &mut stdout),
            };
            match result {
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
        Command::RawCall(function, parameters) => {
            let stdin = io::stdin();
            let mut stdin = stdin.lock();
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            let result = match endpoint {
                orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                    let parameter_ids = match parameters {
                        RawCallParameters::None => Vec::new(),
                        RawCallParameters::One(parameter) => vec![parameter],
                        RawCallParameters::Pair(first, second) => vec![first, second],
                    };
                    orna_server::run_sqlite_raw_call(
                        path,
                        function,
                        &parameter_ids,
                        &mut stdin,
                        &mut stdout,
                    )
                }
                _ => match parameters {
                    RawCallParameters::None => orna_server::run_local_raw_call(function),
                    RawCallParameters::One(parameter) => {
                        orna_server::run_local_raw_call_with_argument(function, parameter)
                    }
                    RawCallParameters::Pair(first, second) => {
                        orna_server::run_local_raw_call_with_argument_pair(function, first, second)
                    }
                },
            };
            match result {
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
            }
        }
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
            let result = match &endpoint {
                orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                    orna_server::run_sqlite_invoke(path, request, &mut stdout, &mut stderr)
                }
                _ if endpoint_explicit => orna_server::run_installed_invoke_at(
                    &endpoint,
                    request,
                    &mut stdout,
                    &mut stderr,
                ),
                _ => orna_server::run_installed_invoke(request, &mut stdout, &mut stderr),
            };
            match result {
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
            let result = match endpoint {
                orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                    orna_server::run_sqlite_user_state(path, request, &mut stdout)
                }
                _ => orna_server::run_installed_user_state(request, &mut stdout),
            };
            match result {
                Ok(orna_server::InstalledUserStateOutcome::Completed) => ExitCode::SUCCESS,
                // A future closed outcome falls back to internal.
                Ok(_) => ExitCode::from(7),
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(state_error_exit_code(&error))
                }
            }
        }
        Command::RuntimeDescribe(path) => {
            let library = match orna_client::RuntimeLibrary::load_qt(&path) {
                Ok(library) => library,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    return ExitCode::from(1);
                }
            };
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match write_runtime_descriptor_json(&mut stdout, library.descriptor()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
        Command::Inspect(request) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            let result = match endpoint {
                orna_client::endpoint::DatabaseEndpoint::LocalPath { path } => {
                    orna_server::run_sqlite_inspect(path, request, &mut stdout)
                }
                _ => orna_server::run_installed_inspect(request, &mut stdout),
            };
            match result {
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

fn endpoint_command_is_unsupported(
    endpoint: &orna_client::endpoint::DatabaseEndpoint,
    command: &Command,
) -> bool {
    match endpoint {
        orna_client::endpoint::DatabaseEndpoint::ManagedLocal { .. } => false,
        orna_client::endpoint::DatabaseEndpoint::LocalPath { .. } => !matches!(
            command,
            Command::Run
                | Command::SourceCheck(_)
                | Command::SourceApply(_)
                | Command::SourceDiff(_)
                | Command::Invoke(_)
                | Command::State(_)
                | Command::SecurityGrantExecute(_)
                | Command::SecurityAdmin(_)
                | Command::Inspect(_)
                | Command::RawCall(_, _)
        ),
        orna_client::endpoint::DatabaseEndpoint::UnixSocket { .. }
        | orna_client::endpoint::DatabaseEndpoint::RemoteTls { .. } => true,
    }
}

fn write_sqlite_source_apply_outcome(
    outcome: orna_server::SqliteSourceApplyOutcome,
    color: cli::ColorChoice,
) -> ExitCode {
    match outcome {
        orna_server::SqliteSourceApplyOutcome::Diagnostics {
            bytes,
            human_bytes,
            coloured_bytes,
        } => {
            let stderr = io::stderr();
            let terminal = stderr.is_terminal();
            let mut stderr = stderr.lock();
            let bytes = if color.enabled(terminal) {
                &coloured_bytes
            } else if terminal {
                &human_bytes
            } else {
                &bytes
            };
            let _ = stderr.write_all(bytes).and_then(|()| stderr.flush());
            ExitCode::from(1)
        }
        orna_server::SqliteSourceApplyOutcome::Applied(bytes) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            write_stderr_line("orna: local SQLite source apply returned an unsupported result");
            ExitCode::from(1)
        }
    }
}

fn write_sqlite_source_diff_outcome(
    outcome: orna_server::SqliteSourceDiffOutcome,
    color: cli::ColorChoice,
) -> ExitCode {
    match outcome {
        orna_server::SqliteSourceDiffOutcome::Diagnostics {
            bytes,
            human_bytes,
            coloured_bytes,
        } => {
            let stderr = io::stderr();
            let terminal = stderr.is_terminal();
            let mut stderr = stderr.lock();
            let bytes = if color.enabled(terminal) {
                &coloured_bytes
            } else if terminal {
                &human_bytes
            } else {
                &bytes
            };
            let _ = stderr.write_all(bytes).and_then(|()| stderr.flush());
            ExitCode::from(1)
        }
        orna_server::SqliteSourceDiffOutcome::Diff(bytes) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            match stdout.write_all(&bytes).and_then(|()| stdout.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    write_stderr_line(&error.to_string());
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            write_stderr_line("orna: local SQLite source diff returned an unsupported result");
            ExitCode::from(1)
        }
    }
}

#[derive(serde::Serialize)]
struct RuntimeDescriptorJson<'a> {
    runtime_name: &'a str,
    runtime_version: &'a str,
    build_id: &'a str,
    platform: &'a str,
    thread_model: i32,
    features: u64,
    sinks: Vec<RuntimeSinkJson<'a>>,
    contracts: Vec<RuntimeContractJson<'a>>,
}

#[derive(serde::Serialize)]
struct RuntimeSinkJson<'a> {
    type_name: &'a str,
    media_types: &'a [String],
    supports_streaming: bool,
    preference_rank: i32,
}

#[derive(serde::Serialize)]
struct RuntimeContractJson<'a> {
    name: &'a str,
    major: u32,
    minor: u32,
    features: &'a [String],
}

fn write_runtime_descriptor_json(
    output: &mut impl Write,
    descriptor: &orna_client::RuntimeDescriptor,
) -> io::Result<()> {
    let json = RuntimeDescriptorJson {
        runtime_name: &descriptor.runtime_name,
        runtime_version: &descriptor.runtime_version,
        build_id: &descriptor.build_id,
        platform: &descriptor.platform,
        thread_model: descriptor.thread_model.0,
        features: descriptor.features,
        sinks: descriptor
            .sinks
            .iter()
            .map(|sink| RuntimeSinkJson {
                type_name: &sink.type_name,
                media_types: &sink.media_types,
                supports_streaming: sink.supports_streaming,
                preference_rank: sink.preference_rank,
            })
            .collect(),
        contracts: descriptor
            .contracts
            .iter()
            .map(|contract| RuntimeContractJson {
                name: &contract.name,
                major: contract.major,
                minor: contract.minor,
                features: &contract.features,
            })
            .collect(),
    };
    serde_json::to_writer(&mut *output, &json).map_err(io::Error::other)?;
    output.write_all(b"\n")
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
    fn accepts_the_user_facing_server_commands() {
        assert_eq!(
            parse_command(arguments(&["orna", "server", "run"])),
            Some(Command::Run),
        );
        assert_eq!(
            parse_command(arguments(&["orna", "server", "backend-shell"])),
            Some(Command::BackendShell),
        );
    }

    #[test]
    fn accepts_the_daemon_aliases() {
        for alias in ["--daemon", "-d"] {
            assert_eq!(
                parse_command(arguments(&["orna", alias])),
                Some(Command::Run),
                "{alias}",
            );
        }
        for values in [
            vec!["orna", "--daemon", "extra"],
            vec!["orna", "-d", "--help"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
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
    fn no_command_starts_the_function_backed_repl() {
        let Some(Command::Invoke(arguments)) = parse_command(arguments(&["orna"])) else {
            panic!("no-command form must invoke the REPL function");
        };
        assert_eq!(
            arguments.target,
            InvocationTarget::qualified_name(
                QualifiedSemanticName::new(["std", "cli", "repl"])
                    .expect("the REPL target is qualified"),
            )
            .expect("the REPL target is valid"),
        );
        assert!(arguments.arguments.is_empty());
        assert!(arguments.runtime.is_none());
    }

    #[test]
    fn explicit_repl_starts_the_same_function() {
        assert_eq!(
            parse_command(arguments(&["orna", "repl"])),
            parse_command(arguments(&["orna"])),
        );
        assert_eq!(
            parse_command(arguments(&["orna", "help", "repl"])),
            Some(Command::Help(HelpTopic::Repl)),
        );
    }

    #[test]
    fn rejects_malformed_and_extra_version_shapes() {
        for values in [
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
    fn accepts_one_exact_source_diff_path() {
        assert_eq!(
            parse_command(arguments(&["orna", "source", "diff", "nested/app.orna"])),
            Some(Command::SourceDiff("nested/app.orna".to_owned()))
        );
    }

    #[test]
    fn accepts_one_exact_runtime_describe_path() {
        let path = "/opt/orna/lib/liborna-runtime-qt.so";
        assert_eq!(
            parse_command(arguments(&["orna", "runtime", "describe", path])),
            Some(Command::RuntimeDescribe(PathBuf::from(path)))
        );
    }

    #[test]
    fn accepts_runtime_describe_help() {
        assert_eq!(
            parse_command(arguments(&["orna", "runtime", "describe", "--help"])),
            Some(Command::Help(HelpTopic::Runtime))
        );
    }

    #[test]
    fn rejects_malformed_and_extra_runtime_describe_shapes() {
        for values in [
            vec!["orna", "runtime"],
            vec!["orna", "runtime", "describe"],
            vec![
                "orna",
                "runtime",
                "inspect",
                "/opt/orna/lib/liborna-runtime-qt.so",
            ],
            vec![
                "orna",
                "runtime",
                "describe",
                "/opt/orna/lib/liborna-runtime-qt.so",
                "extra",
            ],
            vec!["orna", "runtime", "describe", "/first", "/second"],
            vec!["orna", "runtime", "describe", "--help", "extra"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn serializes_runtime_descriptor_metadata_in_source_order() {
        let descriptor = orna_client::RuntimeDescriptor {
            abi_major: 1,
            abi_minor: 0,
            runtime_name: "orna-runtime-qt".to_owned(),
            runtime_version: "1.0.0".to_owned(),
            build_id: "test-build".to_owned(),
            platform: "linux-x86_64".to_owned(),
            thread_model: orna_client::AbiThreadModel::CALLER_PUMPS,
            features: 1,
            sinks: vec![orna_client::RuntimeSink {
                type_name: "std.ui.UI".to_owned(),
                media_types: vec!["application/test".to_owned()],
                supports_streaming: false,
                preference_rank: 0,
            }],
            contracts: vec![orna_client::RuntimeContract {
                name: "std.ui.window".to_owned(),
                major: 1,
                minor: 0,
                features: vec!["title".to_owned()],
            }],
        };
        let mut output = Vec::new();
        write_runtime_descriptor_json(&mut output, &descriptor).expect("metadata writes");

        assert_eq!(
            String::from_utf8(output).expect("JSON is UTF-8"),
            concat!(
                r#"{"runtime_name":"orna-runtime-qt","runtime_version":"1.0.0","build_id":"test-build","platform":"linux-x86_64","thread_model":3,"features":1,"sinks":[{"type_name":"std.ui.UI","media_types":["application/test"],"supports_streaming":false,"preference_rank":0}],"contracts":[{"name":"std.ui.window","major":1,"minor":0,"features":["title"]}]}"#,
                "\n"
            )
        );
    }

    #[test]
    fn rejects_global_runtime_override_on_server_run() {
        assert_eq!(
            parse_command(arguments(&["orna", "--runtime", "tty", "server", "run"])),
            None,
            "runtime override must be rejected for server run"
        );
    }

    #[test]
    fn rejects_retired_server_upgrade_command() {
        assert_eq!(
            parse_command(arguments(&["orna", "server", "upgrade"])),
            None,
        );
        assert_eq!(
            parse_command(arguments(&["orna", "server", "upgrade", "--help"])),
            None,
        );
    }

    #[test]
    fn rejects_global_runtime_override_on_server_backend_shell() {
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "--runtime",
                "tty",
                "server",
                "backend-shell",
            ])),
            None,
            "runtime override must be rejected for server backend-shell"
        );
    }

    #[test]
    fn rejects_runtime_override_on_source_commands() {
        for subcommand in ["check", "apply", "diff"] {
            assert_eq!(
                parse_command(arguments(&[
                    "orna",
                    "--runtime",
                    "tty",
                    "source",
                    subcommand,
                    "app.orna",
                ])),
                None,
                "runtime override must be rejected for source {subcommand}"
            );
        }
    }

    #[test]
    fn rejects_global_runtime_override_on_non_invoke_commands() {
        let function = FunctionId::from_bytes([0x11; 16]).canonical();
        let root = FunctionId::from_bytes([0x22; 16]).canonical();
        let invocation = InvocationId::from_bytes([0x33; 16]).canonical();
        for values in [
            vec!["orna", "--runtime", "tty", "--version"],
            vec!["orna", "--runtime", "tty", "raw-call", function.as_str()],
            vec!["orna", "--runtime", "tty", "state", "get", root.as_str()],
            vec!["orna", "--runtime", "tty", "inspect", invocation.as_str()],
            vec!["orna", "--runtime", "tty", "security", "whoami"],
            vec![
                "orna",
                "--runtime",
                "tty",
                "security",
                "grant-execute",
                function.as_str(),
            ],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
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
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
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
    fn rejects_invalid_source_diff_shapes_and_paths() {
        for values in [
            vec!["orna", "source"],
            vec!["orna", "source", "diff"],
            vec!["orna", "source", "diff", ""],
            vec!["orna", "source", "diff", "-x"],
            vec!["orna", "source", "diff", "app.orna", "extra"],
            vec!["orna", "source", "DIFF", "app.orna"],
            vec!["orna", "source", "--diff", "app.orna"],
            vec!["orna", "source", "diff", "line\nbreak"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
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
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "--runtime",
                "qt",
                "invoke",
                "std.invoke.echo",
            ])),
            Some(invoke_command(
                echo_target(),
                Vec::new(),
                None,
                None,
                false,
                false,
                Some(RuntimeFamily::Qt),
            ))
        );
        assert_eq!(
            parse_command(arguments(&["orna", "--runtime", "tty"])),
            parse_command(arguments(&["orna", "--runtime", "tty", "repl"])),
        );
        for values in [
            vec!["orna", "--runtime", "gtk", "invoke", "std.invoke.echo"],
            vec!["orna", "--runtime"],
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
        assert_eq!(
            parse_command(arguments(&[
                "orna",
                "invoke",
                "std.invoke.echo",
                "--runtime",
                "qt"
            ])),
            Some(invoke_command(
                echo_target(),
                Vec::new(),
                None,
                None,
                false,
                false,
                Some(RuntimeFamily::Qt),
            ))
        );
        for values in [vec!["orna", "invoke", "std.invoke.echo", "--runtime"]] {
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
    fn admits_explicit_local_path_state_to_the_sqlite_route() {
        let root = FunctionId::from_bytes([0x11; 16]);
        let parsed = parse_invocation(arguments(&[
            "orna",
            "--db",
            "./state.sqlite",
            "state",
            "get",
            &root.canonical(),
        ]))
        .expect("explicit local path state command should parse");

        assert_eq!(
            parsed.endpoint,
            orna_client::endpoint::DatabaseEndpoint::LocalPath {
                path: PathBuf::from("./state.sqlite"),
            }
        );
        assert!(parsed.endpoint_explicit);
        assert!(matches!(&parsed.command, Command::State(_)));
        assert!(
            !endpoint_command_is_unsupported(&parsed.endpoint, &parsed.command),
            "the LocalPath capability gate must admit state for the SQLite adapter",
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
    fn accepts_top_level_and_scoped_help_topics() {
        for (values, topic) in [
            (vec!["orna", "--help"], HelpTopic::TopLevel),
            (vec!["orna", "help"], HelpTopic::TopLevel),
            (vec!["orna", "help", "server"], HelpTopic::Server),
            (vec!["orna", "help", "server", "run"], HelpTopic::ServerRun),
            (
                vec!["orna", "help", "server", "backend-shell"],
                HelpTopic::ServerBackendShell,
            ),
            (vec!["orna", "help", "source"], HelpTopic::Source),
            (vec!["orna", "help", "invoke"], HelpTopic::Invoke),
            (vec!["orna", "help", "state"], HelpTopic::State),
            (vec!["orna", "help", "inspect"], HelpTopic::Inspect),
            (vec!["orna", "help", "runtime"], HelpTopic::Runtime),
            (vec!["orna", "help", "security"], HelpTopic::Security),
            (vec!["orna", "help", "raw-call"], HelpTopic::RawCall),
            (vec!["orna", "server", "--help"], HelpTopic::Server),
            (
                vec!["orna", "server", "run", "--help"],
                HelpTopic::ServerRun,
            ),
            (
                vec!["orna", "server", "backend-shell", "--help"],
                HelpTopic::ServerBackendShell,
            ),
            (vec!["orna", "source", "--help"], HelpTopic::Source),
            (vec!["orna", "invoke", "--help"], HelpTopic::Invoke),
            (vec!["orna", "state", "--help"], HelpTopic::State),
            (vec!["orna", "inspect", "--help"], HelpTopic::Inspect),
            (vec!["orna", "runtime", "--help"], HelpTopic::Runtime),
            (
                vec!["orna", "runtime", "describe", "--help"],
                HelpTopic::Runtime,
            ),
            (vec!["orna", "security", "--help"], HelpTopic::Security),
            (vec!["orna", "raw-call", "--help"], HelpTopic::RawCall),
        ] {
            assert_eq!(
                parse_command(arguments(&values)),
                Some(Command::Help(topic)),
                "{values:?}"
            );
        }
    }

    #[test]
    fn rejects_unknown_trailing_and_runtime_help_shapes() {
        for values in [
            vec!["orna", "--help", "server"],
            vec!["orna", "help", "unknown"],
            vec!["orna", "help", "server", "unknown"],
            vec!["orna", "help", "server", "run", "extra"],
            vec!["orna", "help", "source", "extra"],
            vec!["orna", "server", "--help", "extra"],
            vec!["orna", "server", "run", "--help", "extra"],
            vec!["orna", "source", "--help", "extra"],
            vec!["orna", "invoke", "--help", "extra"],
            vec!["orna", "invoke", "std.invoke.echo", "--help", "value"],
            vec!["orna", "state", "--help", "extra"],
            vec!["orna", "inspect", "--help", "extra"],
            vec!["orna", "runtime", "--help", "extra"],
            vec!["orna", "security", "--help", "extra"],
            vec!["orna", "runtime", "describe", "--help", "extra"],
            vec!["orna", "raw-call", "--help", "extra"],
            vec!["orna", "--runtime", "tty", "--help"],
            vec!["orna", "--runtime", "tty", "help"],
            vec!["orna", "--runtime", "tty", "help", "invoke"],
            vec!["orna", "--runtime", "tty", "server", "--help"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn help_text_describes_the_direct_session_commands() {
        let top_level = help_text(HelpTopic::TopLevel);
        assert!(top_level.contains("Orna command line"));
        assert!(top_level.contains("function-backed REPL"));
        for command in [
            "invoke",
            "repl",
            "source",
            "inspect",
            "--daemon",
            "--db",
            "--runtime",
        ] {
            assert!(
                top_level.contains(command),
                "{command} is missing from top-level help",
            );
        }
        assert!(top_level.contains("Operational Commands:"));
        assert!(top_level.contains("server ..."));
        assert!(top_level.contains("security ..."));
        assert!(top_level.contains("raw-call ..."));
        assert!(!top_level.contains("runtime ..."));
        assert!(help_text(HelpTopic::Invoke).contains("--runtime <family>"));
        assert!(help_text(HelpTopic::State).contains("--value-file <path>"));
        assert!(help_text(HelpTopic::Inspect).contains("--projection <name>"));
        assert!(help_text(HelpTopic::Runtime).contains("runtime describe"));
        assert!(help_text(HelpTopic::Security).contains("security grant-execute"));
        assert!(help_text(HelpTopic::RawCall).contains("raw-call"));
        assert!(top_level.contains("--color <auto|always|never>"));
    }

    #[test]
    fn local_path_help_matches_sqlite_value_and_trace_contracts() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let parameter = ParameterId::from_bytes([0x22; 16]);
        let raw_call = parse_invocation(arguments(&[
            "orna",
            "--db",
            "./state.sqlite",
            "raw-call",
            &function.canonical(),
            &parameter.canonical(),
        ]))
        .expect("LocalPath raw-call parameter form should parse");
        assert!(matches!(
            raw_call.command,
            Command::RawCall(_, RawCallParameters::One(parsed)) if parsed == parameter
        ));

        let raw_help = parse_invocation(arguments(&[
            "orna",
            "--db",
            "./state.sqlite",
            "raw-call",
            "--help",
        ]))
        .expect("LocalPath raw-call help should parse");
        assert_eq!(raw_help.command, Command::Help(HelpTopic::RawCallLocalPath),);
        let raw_help_text = help_text(HelpTopic::RawCallLocalPath);
        assert!(raw_help_text.contains("ORV5"));
        assert!(!raw_help_text.contains("ORV1"));
        assert!(help_text(HelpTopic::RawCall).contains("ORV1"));

        let invoke_help = parse_invocation(arguments(&[
            "orna",
            "--db",
            "./state.sqlite",
            "invoke",
            "--help",
        ]))
        .expect("LocalPath invoke help should parse");
        assert_eq!(
            invoke_help.command,
            Command::Help(HelpTopic::InvokeLocalPath),
        );
        let invoke_help_text = help_text(HelpTopic::InvokeLocalPath);
        assert!(invoke_help_text.contains("SQLite LocalPath"));
        assert!(invoke_help_text.contains("--trace"));
        assert!(invoke_help_text.contains("does not support"));
        assert!(!help_text(HelpTopic::Invoke).contains("SQLite LocalPath"));

        let parsed_invoke = parse_invocation(arguments(&[
            "orna",
            "--db",
            "./state.sqlite",
            "invoke",
            "std.invoke.echo",
            "--trace",
            "normal",
        ]))
        .expect("LocalPath invoke trace option should remain parseable");
        let Command::Invoke(arguments) = parsed_invoke.command else {
            panic!("expected invoke command");
        };
        assert_eq!(arguments.trace, Some(InvocationTracePolicy::Normal));
    }

    #[test]
    fn usage_diagnostic_keeps_the_direct_command_list() {
        assert!(USAGE.starts_with(
            "Usage:\n  orna\n  orna repl\n  orna --db <target> [command] [options]\n"
        ));
        for command in ["invoke", "repl", "source", "inspect", "raw-call"] {
            assert!(USAGE.contains(command));
        }
        assert!(USAGE.contains("orna raw-call <canonical-function-id>"));
        assert!(!USAGE.ends_with('\n'));
        assert_ne!(USAGE, HELP_TOP_LEVEL);
    }

    #[test]
    fn parses_global_colour_modes_before_the_command() {
        for (value, expected) in [
            ("auto", ColorChoice::Auto),
            ("always", ColorChoice::Always),
            ("never", ColorChoice::Never),
        ] {
            let parsed = parse_invocation(arguments(&["orna", "--color", value, "--help"]))
                .expect("colour mode should parse");
            assert_eq!(parsed.color, expected);
            assert_eq!(parsed.command, Command::Help(HelpTopic::TopLevel));
        }
    }

    #[test]
    fn rejects_invalid_or_late_colour_options() {
        for values in [
            vec!["orna", "--color"],
            vec!["orna", "--color", "purple", "--help"],
            vec!["orna", "--help", "--color", "always"],
            vec!["orna", "help", "invoke", "--color", "never"],
            vec!["orna", "--color", "always", "help", "invoke", "extra"],
        ] {
            assert_eq!(parse_invocation(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn parses_one_leading_database_endpoint() {
        let parsed = parse_invocation(arguments(&[
            "orna",
            "--db",
            "orna://db.example.test/work",
            "--help",
        ]))
        .expect("database endpoint should parse");
        assert_eq!(
            parsed.endpoint,
            orna_client::endpoint::DatabaseEndpoint::RemoteTls {
                host: "db.example.test".to_owned(),
                port: orna_client::endpoint::DEFAULT_REMOTE_PORT,
                database: "work".to_owned(),
            },
        );
        assert!(parsed.endpoint_explicit);
        assert_eq!(parsed.command, Command::Help(HelpTopic::TopLevel));
    }

    #[test]
    fn parses_a_positional_database_endpoint_before_the_command() {
        let parsed = parse_invocation(arguments(&[
            "orna",
            "orna+unix:///run/orna/default/orna.sock",
            "invoke",
            "demo.main",
        ]))
        .expect("positional endpoint should parse");
        assert_eq!(
            parsed.endpoint,
            orna_client::endpoint::DatabaseEndpoint::UnixSocket {
                path: PathBuf::from("/run/orna/default/orna.sock"),
            },
        );
        assert!(parsed.endpoint_explicit);
        assert!(matches!(parsed.command, Command::Invoke(_)));
    }

    #[test]
    fn rejects_duplicate_or_late_database_endpoint_options() {
        for values in [
            vec!["orna", "--db"],
            vec![
                "orna",
                "--db",
                "orna://db.example.test/work",
                "--db",
                "./other",
            ],
            vec!["orna", "--help", "--db", "./other"],
            vec!["orna", "invoke", "demo.main", "--db", "./other"],
        ] {
            assert_eq!(parse_invocation(arguments(&values)), None, "{values:?}");
        }
    }

    #[test]
    fn preserves_colour_named_invoke_arguments() {
        let parsed = parse_invocation(arguments(&[
            "orna",
            "invoke",
            "demo.main",
            "--color",
            "red",
        ]))
        .expect("friendly colour argument should parse");
        let Command::Invoke(arguments) = parsed.command else {
            panic!("expected invoke command");
        };
        assert_eq!(
            arguments.arguments,
            vec![CliArgumentInput::Friendly {
                name: "color".to_owned(),
                value: "red".to_owned(),
            }]
        );
    }

    #[test]
    fn renders_plain_and_coloured_help_deterministically() {
        let plain = render_help(HelpTopic::TopLevel, ColorChoice::Never, true);
        assert_eq!(plain, HELP_TOP_LEVEL);
        assert_eq!(
            render_help(HelpTopic::TopLevel, ColorChoice::Auto, false),
            HELP_TOP_LEVEL,
        );

        let coloured = render_help(HelpTopic::TopLevel, ColorChoice::Always, false);
        assert!(coloured.contains("\x1b[1;36mOrna command line\x1b[0m"));
        assert!(coloured.contains("\x1b[1;36mCommands:\x1b[0m"));
    }
}
