use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    process::ExitCode,
};

use orna_core::{
    FunctionId,
    security::{CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME},
};
use orna_protocol::CallFailure;

mod package_maintenance;
mod source_check;

const USAGE: &str = "Usage:\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna raw-call <canonical-function-id>";

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Run,
    BackendShell,
    Upgrade,
    SourceCheck(String),
    RawCall(FunctionId),
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
        Command::RawCall(function) => match orna_server::run_local_raw_call(function) {
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
    }
}

fn parse_command<I>(args: I) -> Option<Command>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let _argv0 = args.next();

    match args.next().as_deref() {
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
            if !matches!(args.next().as_deref(), Some(value) if value == OsStr::new("check")) {
                return None;
            }
            let path = args.next()?.into_string().ok()?;
            (args.next().is_none() && valid_source_path(&path))
                .then_some(Command::SourceCheck(path))
        }
        Some(value) if value == OsStr::new("raw-call") => {
            let function = args.next()?.into_string().ok()?;
            if args.next().is_some() {
                return None;
            }
            if function == CATALOGUE_HEALTH_FUNCTION_NAME {
                Some(Command::RawCall(CATALOGUE_HEALTH_FUNCTION_ID))
            } else {
                FunctionId::from_canonical(&function)
                    .ok()
                    .map(Command::RawCall)
            }
        }
        _ => None,
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

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
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
    fn accepts_one_exact_source_check_path() {
        assert_eq!(
            parse_command(arguments(&["orna", "source", "check", "app.orna"])),
            Some(Command::SourceCheck("app.orna".to_owned()))
        );
    }

    #[test]
    fn accepts_one_exact_canonical_raw_call_identity() {
        let function = FunctionId::from_bytes([0x11; 16]);
        let canonical = function.canonical();
        assert_eq!(
            parse_command(arguments(&["orna", "raw-call", &canonical])),
            Some(Command::RawCall(function))
        );
        let health = CATALOGUE_HEALTH_FUNCTION_ID;
        assert_eq!(
            parse_command(arguments(&["orna", "raw-call", "sys.catalog.health"])),
            Some(Command::RawCall(health))
        );
        assert_eq!(
            parse_command(arguments(&["orna", "raw-call", &health.canonical()])),
            Some(Command::RawCall(health))
        );
        for values in [
            vec!["orna", "raw-call"],
            vec!["orna", "raw-call", "sys.catalog.HEALTH"],
            vec!["orna", "raw-call", "sys.catalog.health.extra"],
            vec!["orna", "raw-call", "sys.catalog.health "],
            vec!["orna", "raw-call", "function:not-an-id"],
            vec!["orna", "raw-call", &canonical, "extra"],
        ] {
            assert_eq!(parse_command(arguments(&values)), None, "{values:?}");
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

    #[test]
    fn usage_diagnostic_is_exact() {
        assert_eq!(
            USAGE,
            "Usage:\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna raw-call <canonical-function-id>"
        );
    }
}
