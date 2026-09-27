use std::process::{Command, Stdio};

use tempfile::tempdir;

const FIXTURE: &[u8] = include_bytes!("fixtures/status-hyperlinks/changed path.orna");

#[test]
fn status_hyperlinks_are_emitted_only_for_supported_terminal_stdout() {
    let repository = tempdir().expect("temporary repository");
    let fixture_path = repository.path().join("changed path.orna");
    std::fs::write(&fixture_path, FIXTURE).expect("copy checked-in ORNA fixture");
    let git_init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .output()
        .expect("git init");
    assert!(
        git_init.status.success(),
        "git init stderr: {:?}",
        git_init.stderr
    );

    let cli = env!("CARGO_BIN_EXE_orna-cli-v1");
    let plain = Command::new(cli)
        .args(["--color", "never", "status"])
        .current_dir(repository.path())
        .env("TERM", "xterm-kitty")
        .output()
        .expect("piped CLI status");
    println!(
        "plain: exit={:?} stdout={:?} stderr={:?}",
        plain.status.code(),
        String::from_utf8_lossy(&plain.stdout),
        String::from_utf8_lossy(&plain.stderr)
    );
    assert_eq!(
        plain.status.code(),
        Some(0),
        "plain stderr: {:?}",
        plain.stderr
    );
    assert!(plain.stderr.is_empty(), "plain stderr: {:?}", plain.stderr);
    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain_stdout.contains("changed path.orna"),
        "{plain_stdout:?}"
    );
    assert!(!plain_stdout.contains("\x1b]8;;"), "{plain_stdout:?}");

    let terminal_stderr_file = tempfile::NamedTempFile::new().expect("terminal stderr capture");
    let shell_command = format!(
        "exec {} --color never status 2> {}",
        shell_quote(cli),
        shell_quote(&terminal_stderr_file.path().to_string_lossy())
    );
    let terminal = Command::new("script")
        .args(["-q", "-e", "-c", &shell_command, "/dev/null"])
        .current_dir(repository.path())
        .env("TERM", "xterm-kitty")
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .output()
        .expect("script command with a pseudo-terminal");
    let terminal_cli_stderr = std::fs::read(terminal_stderr_file.path())
        .expect("read CLI stderr captured separately from the PTY");
    println!(
        "supported-terminal: exit={:?} stdout={:?} cli_stderr={:?} script_stderr={:?}",
        terminal.status.code(),
        String::from_utf8_lossy(&terminal.stdout),
        String::from_utf8_lossy(&terminal_cli_stderr),
        String::from_utf8_lossy(&terminal.stderr)
    );
    assert_eq!(
        terminal.status.code(),
        Some(0),
        "terminal stderr: {:?}",
        terminal.stderr
    );
    assert!(
        terminal_cli_stderr.is_empty(),
        "terminal CLI stderr: {:?}",
        terminal_cli_stderr
    );
    assert!(
        terminal.stderr.is_empty(),
        "script stderr: {:?}",
        terminal.stderr
    );
    let terminal_stdout = String::from_utf8_lossy(&terminal.stdout);
    let root = repository
        .path()
        .canonicalize()
        .expect("absolute repository root");
    let expected_url = format!("file://{}/changed%20path.orna", root.display());
    assert!(
        terminal_stdout.contains(&format!("\x1b]8;;{expected_url}\x1b\\")),
        "missing OSC 8 URL {expected_url:?} in {terminal_stdout:?}"
    );
    assert!(
        terminal_stdout.contains("changed path.orna"),
        "{terminal_stdout:?}"
    );
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
