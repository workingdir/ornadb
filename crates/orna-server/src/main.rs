use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    process::ExitCode,
};

mod package_maintenance;
mod source_check;

const USAGE: &str = "Usage:\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>";

#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Run,
    BackendShell,
    Upgrade,
    SourceCheck(String),
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
            "Usage:\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>"
        );
    }
}
