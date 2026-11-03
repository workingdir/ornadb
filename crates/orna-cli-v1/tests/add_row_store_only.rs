//! `orna add --row` and `orna unstage --row` in a format-3 repository whose rows
//! live only in `.orna/store`.
//!
//! Rows have no physical Git paths in format 3 (ORNA-REPO-004), so a row that
//! exists in the store has nothing to stage. The refusal must name that reason,
//! because the operator's next step is to change the row through the database
//! and publish it, not to repair a worktree. This test runs the real CLI binary
//! against a repository seeded through the REPL and published, so the store is
//! the only place the row exists.

use std::{
    path::Path,
    process::{Command, Output},
};

#[path = "support/format3.rs"]
mod format3;

const SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\npub fn main() {}\n";

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("orna CLI process")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn staged(directory: &Path) -> String {
    let output = Command::new("git")
        .args(["ls-files", "--stage"])
        .current_dir(directory)
        .output()
        .expect("git ls-files");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A format-3 repository whose `music` row `london` is committed in the store
/// and has no loose file in the worktree.
fn repository_with_store_only_row() -> tempfile::TempDir {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(SOURCE, "music");
    let inserted = orna(
        directory.path(),
        &[
            "repl",
            r#"main.music.insert({ id: "london", title: "Original", artist: "Kieran" })"#,
        ],
    );
    assert_eq!(inserted.status.code(), Some(0), "{}", combined(&inserted));
    let published = orna(directory.path(), &["publish", "--message", "seed"]);
    assert_eq!(published.status.code(), Some(0), "{}", combined(&published));
    assert!(
        !directory.path().join("music/london.orna").exists(),
        "the published row must exist only in the store"
    );
    directory
}

#[test]
fn add_row_names_the_store_when_the_row_has_no_row_file() {
    let directory = repository_with_store_only_row();
    let before = staged(directory.path());
    let refused = orna(directory.path(), &["add", "--row", "music", "\"london\""]);
    let message = combined(&refused);
    assert_eq!(refused.status.code(), Some(1), "{message}");
    assert!(
        message.contains(".orna/store"),
        "the refusal names the store as the only row location: {message}"
    );
    assert!(
        !message.contains("logical row was not found"),
        "the row exists, so the refusal must not deny it: {message}"
    );
    assert!(
        !message.contains("run this command inside a local Orna Git worktree"),
        "the command ran inside a worktree, so the help must name the real next step: {message}"
    );
    assert_eq!(
        staged(directory.path()),
        before,
        "nothing was staged in the Git index"
    );
}

#[test]
fn unstage_row_names_the_store_when_the_row_has_no_row_file() {
    let directory = repository_with_store_only_row();
    let before = staged(directory.path());
    let refused = orna(
        directory.path(),
        &["restore", "--staged", "--row", "music", "\"london\""],
    );
    let message = combined(&refused);
    assert_eq!(refused.status.code(), Some(1), "{message}");
    assert!(
        message.contains(".orna/store"),
        "the refusal names the store as the only row location: {message}"
    );
    assert!(
        !message.contains("logical row was not found"),
        "the row exists, so the refusal must not deny it: {message}"
    );
    assert_eq!(
        staged(directory.path()),
        before,
        "nothing was unstaged in the Git index"
    );
}
