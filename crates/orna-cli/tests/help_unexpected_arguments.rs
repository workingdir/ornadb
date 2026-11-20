//! Extra arguments after a command are rejected with a remedy in Orna terms, not
//! delivery jargon (ORNA-UX-001, ORNA-UX-002).

use std::process::Command;

#[test]
fn unexpected_argument_remedy_names_the_help_command_without_internal_jargon() {
    let directory = tempfile::tempdir().expect("empty directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--help", "status"])
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run orna --help status");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(2), "{combined}");
    assert!(
        combined.contains("ORNA-CMD-ARGUMENT-UNEXPECTED"),
        "{combined}"
    );
    assert!(
        combined.contains("`orna --help` lists each command's usage"),
        "{combined}"
    );
    assert!(!combined.contains("bounded slice"), "{combined}");
}
