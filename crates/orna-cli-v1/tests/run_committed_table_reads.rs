//! One-shot table reads over committed identities (ornadb-03ol, Orna-1.1.0
//! 10-execution.md). `orna run <module>.<fn>` evaluates a public entry through the
//! durable runtime, which binds tables to their committed rows. `orna invoke <fn>`
//! evaluates a root-module entry with its tables bound to their committed `HEAD`
//! identities: a read sees the committed rows, and a mutating entry fails closed
//! because one-shot invocation never commits table mutations.

use std::{
    path::Path,
    process::{Command, Output},
};

#[path = "support/format3.rs"]
mod format3;

const READ_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn count_music() = music | count();\n\
pub fn main() = music | count();\n";

const WRITE_SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn seed() { music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" }); }\n\
pub fn main() = music | count();\n";

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

#[test]
fn run_public_table_read_exits_zero_over_committed_identity() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(READ_SOURCE, "music");
    let output = orna(directory.path(), &["run", "main.count_music"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "public table read via run failed: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("invocation completed"),
        "public table read via run did not complete: {}",
        combined(&output)
    );
}

#[test]
fn invoke_table_read_exits_zero_over_committed_identity() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(READ_SOURCE, "music");
    let output = orna(directory.path(), &["invoke", "count_music"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "committed-identity table read via invoke failed: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("invocation completed"),
        "committed-identity table read via invoke did not complete: {}",
        combined(&output)
    );
}

#[test]
fn invoke_mutating_table_entry_fails_closed() {
    let (directory, _repository, _relation) =
        format3::format3_repository_for_table(WRITE_SOURCE, "music");
    let output = orna(directory.path(), &["invoke", "seed"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "one-shot invoke must not commit table mutations: {}",
        combined(&output)
    );
    assert!(
        combined(&output).contains("table mutations"),
        "mutating invoke must name the rejected mutation: {}",
        combined(&output)
    );
}
