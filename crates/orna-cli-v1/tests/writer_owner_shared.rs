//! The CLI writer owner is the repository ID (`invocation_owner_id`). `orna run` and the
//! REPL both acquire the persisted writer lease under that owner. This test runs the real
//! CLI binary and reads the persisted lease back, so it fails if either writer's owner
//! derivation changes.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::RuntimeState;

#[path = "support/format3.rs"]
mod format3;

const SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn seed() { music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" }); }\n\
pub fn exercise() { music.update(\"london\", { title: \"Edited by run\" }); }\n\
pub fn main() {}\n";

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("orna CLI process")
}

fn run_repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL submission");
    child.wait_with_output().expect("wait for Orna REPL")
}

/// The repository ID the CLI derives as its writer owner, and the owner of the lease
/// persisted in the durable runtime state after the last CLI invocation.
async fn derived_and_persisted_owner(directory: &Path) -> ([u8; 16], Option<[u8; 16]>) {
    let repository = Repository::discover(directory).expect("repository");
    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen the durable runtime");
    let lease = state.current_lease().await.expect("read writer lease");
    (identity.repository_id, lease.map(|lease| lease.owner_id))
}

#[tokio::test(flavor = "current_thread")]
async fn orna_run_and_repl_effect_write_under_the_repository_owner() {
    let (directory, _repository, _) = format3::format3_repository_for_table(SOURCE, "music");
    let root = directory.path();

    let run = orna(root, &["run", "seed"]);
    assert_eq!(run.status.code(), Some(0), "seed run failed: {run:?}");
    let (derived, after_run) = derived_and_persisted_owner(root).await;
    assert_eq!(
        after_run,
        Some(derived),
        "orna run must persist its writer lease under the repository owner"
    );

    let repl = run_repl(
        root,
        r#"main.music.insert({ id: "paris", title: "New title", artist: "REPL" })"#,
    );
    assert_eq!(repl.status.code(), Some(0), "REPL effect failed: {repl:?}");
    let (derived, after_repl) = derived_and_persisted_owner(root).await;
    assert_eq!(
        after_repl,
        Some(derived),
        "the REPL effect must persist its writer lease under the same repository owner as orna run"
    );
}
