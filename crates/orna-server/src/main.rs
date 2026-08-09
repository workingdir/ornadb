use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    process::ExitCode,
};

const USAGE: &str = "Usage: orna server backend-shell";

fn main() -> ExitCode {
    if !is_backend_shell_command(std::env::args_os()) {
        write_stderr_line(USAGE);
        return ExitCode::from(2);
    }

    match orna_server::run_backend_shell() {
        Ok(never) => match never {},
        Err(error) => {
            write_stderr_line(&error.to_string());
            ExitCode::from(1)
        }
    }
}

fn is_backend_shell_command<I>(args: I) -> bool
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let _argv0 = args.next();

    matches!(args.next().as_deref(), Some(value) if value == OsStr::new("server"))
        && matches!(
            args.next().as_deref(),
            Some(value) if value == OsStr::new("backend-shell")
        )
        && args.next().is_none()
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
    fn accepts_only_the_exact_backend_shell_command() {
        assert!(is_backend_shell_command(arguments(&[
            "orna",
            "server",
            "backend-shell",
        ])));
    }

    #[test]
    fn ignores_argv0_but_rejects_missing_or_extra_tokens() {
        assert!(is_backend_shell_command(arguments(&[
            "/some/path/orna",
            "server",
            "backend-shell",
        ])));
        for values in [
            vec!["orna", "server"],
            vec!["orna", "backend-shell"],
            vec!["orna", "server", "backend-shell", "--flag"],
            vec!["orna", "server", "backend-shell", "select 1"],
        ] {
            assert!(!is_backend_shell_command(arguments(&values)));
        }
    }

    #[test]
    fn rejects_flags_and_sql_in_the_command_position() {
        assert!(!is_backend_shell_command(arguments(&[
            "orna",
            "--server",
            "backend-shell",
        ])));
        assert!(!is_backend_shell_command(arguments(&[
            "orna",
            "server",
            "--command",
        ])));
        assert!(!is_backend_shell_command(arguments(&[
            "orna",
            "server",
            "backend-shell",
            "select",
            "1",
        ])));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_unicode_tokens() {
        use std::os::unix::ffi::OsStringExt;

        let non_unicode = OsString::from_vec(b"server\xff".to_vec());
        assert!(!is_backend_shell_command(vec![
            OsString::from("orna"),
            non_unicode,
            OsString::from("backend-shell"),
        ]));
    }

    #[test]
    fn usage_diagnostic_is_exact() {
        assert_eq!(USAGE, "Usage: orna server backend-shell");
    }
}
