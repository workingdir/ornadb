//! `orna --help` lists every Git-compatible verb the binary accepts (ORNA-CLI-001,
//! spec chapter 18 "Git-compatible commands"), so users can discover them.

use std::process::Command;

/// Git-compatible verbs the binary accepts that the spec lists and help must name.
const ACCEPTED_GIT_VERBS: [&str; 8] = [
    "checkout", "add", "restore", "clone", "tag", "show", "blame", "remote",
];

#[test]
fn help_lists_every_accepted_git_compatible_verb() {
    let directory = tempfile::tempdir().expect("empty directory");
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .arg("--help")
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run orna --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "orna --help: {stdout}");
    let missing: Vec<&str> = ACCEPTED_GIT_VERBS
        .into_iter()
        .filter(|verb| {
            !stdout.lines().any(|line| {
                line.strip_prefix("  ").is_some_and(|rest| {
                    rest.strip_prefix(verb)
                        .is_some_and(|after| after.starts_with(' ') || after.is_empty())
                })
            })
        })
        .collect();
    assert!(
        missing.is_empty(),
        "help omits verbs {missing:?}:\n{stdout}"
    );
}
