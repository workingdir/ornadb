//! Runs the `orna-cli-v1` binary from a chosen working directory.

use std::{
    path::Path,
    process::{Command, Output},
};

/// Runs `orna-cli-v1 <arguments>` with `directory` as the working directory.
pub fn run_in(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .current_dir(directory)
        .args(arguments)
        .output()
        .unwrap()
}
