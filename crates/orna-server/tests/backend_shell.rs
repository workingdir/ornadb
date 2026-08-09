use std::{
    ffi::OsString,
    process::{Command, Output},
};

const USAGE: &[u8] = b"Usage: orna server backend-shell\n";
const TERMINAL_REQUIRED: &[u8] = b"orna: backend-shell must be run in an interactive terminal\n";

fn run(arguments: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .env_clear()
        .output()
        .expect("orna process starts")
}

fn assert_usage(output: &Output) {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, USAGE);
}

#[test]
fn command_shape_failures_have_exact_process_results() {
    for arguments in [
        vec![],
        vec![OsString::from("server")],
        vec![OsString::from("backend-shell")],
        vec![
            OsString::from("server"),
            OsString::from("backend-shell"),
            OsString::from("--command"),
        ],
        vec![
            OsString::from("server"),
            OsString::from("backend-shell"),
            OsString::from("select 1"),
        ],
    ] {
        assert_usage(&run(arguments));
    }
}

#[cfg(unix)]
#[test]
fn non_unicode_command_tokens_are_usage_errors() {
    use std::os::unix::ffi::OsStringExt;

    assert_usage(&run([
        OsString::from("server"),
        OsString::from_vec(b"backend-shell\xff".to_vec()),
    ]));
}

#[test]
fn exact_command_requires_terminals_before_reading_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["server", "backend-shell"])
        .env_clear()
        .env(
            "ORNA_SERVER_POSTGRES_URL",
            "not-a-valid-URL-containing-super-secret",
        )
        .output()
        .expect("orna process starts");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, TERMINAL_REQUIRED);
    assert!(
        !output
            .stderr
            .windows(b"super-secret".len())
            .any(|bytes| bytes == b"super-secret")
    );
}
