//! A failed read of an archive is named as a read. `export --check` and
//! `export ARCHIVE --restore DEST` on an absent archive report that the archive
//! could not be read (ORNA-CMD-TARGET-UNAVAILABLE), not that it could not be
//! written: the title follows the operation that failed.

use std::process::{Command, Output};

fn orna(arguments: &[&str]) -> Output {
    let directory = tempfile::tempdir().expect("empty directory");
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(arguments)
        .current_dir(directory.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run orna")
}

fn assert_names_a_read(arguments: &[&str]) {
    let output = orna(arguments);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Archive could not be read"),
        "orna {arguments:?} names the read: {stderr}"
    );
    assert!(
        !stderr.contains("could not be written"),
        "orna {arguments:?} reports a write: {stderr}"
    );
}

#[test]
fn check_of_an_absent_archive_names_a_read() {
    assert_names_a_read(&["export", "--check", "absent.orna"]);
}

#[test]
fn restore_of_an_absent_archive_names_a_read() {
    assert_names_a_read(&["export", "absent.orna", "--restore", "out"]);
}
