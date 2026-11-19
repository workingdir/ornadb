//! The `orna --help` exit-status table lists exactly the codes the binary can
//! return. Code 4 has no producer in this binary, so the table must not name it.

use std::process::Command;

/// The codes `main.rs` returns, in the order the help table lists them.
const LISTED_EXIT_CODES: [i32; 7] = [0, 1, 2, 3, 5, 6, 7];

#[test]
fn help_exit_status_table_lists_exactly_the_codes_the_binary_returns() {
    let directory = tempfile::tempdir().expect("empty directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--help")
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run orna --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "orna --help: {stdout}");
    let codes: Vec<i32> = stdout
        .lines()
        .skip_while(|line| *line != "Exit status:")
        .skip(1)
        .take(2)
        .flat_map(str::split_whitespace)
        .filter_map(|token| token.parse::<i32>().ok())
        .collect();
    assert_eq!(codes, LISTED_EXIT_CODES, "exit status table:\n{stdout}");
}
