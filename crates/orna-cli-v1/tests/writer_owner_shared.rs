//! Every CLI process writes under its own owner (`process_owner_id`), never under the
//! repository ID. Each `orna run` and REPL process takes the persisted writer lease over
//! from the process before it, so the owner changes and the epoch strictly increases: a
//! departed process's token is fenced instead of being shared with its successor. This test
//! runs the real CLI binary and reads the persisted lease back after each process, so it
//! fails if a writer's owner stops being per process or a successor stops fencing its
//! predecessor.

use std::{
    path::Path,
    process::{Command, Output},
};

use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeState, WriterLease};

#[path = "support/format3.rs"]
mod format3;

const SOURCE: &str = "pub table music(id: Str) { title: Str, artist: Str, }\n\
pub fn seed() { music.insert({ id: \"london\", title: \"Original\", artist: \"Kieran\" }); }\n\
pub fn exercise() { music.update(\"london\", { title: \"Edited by run\" }); }\n\
pub fn main() {}\n";

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("orna CLI process")
}

/// The writer lease the last CLI process persisted, and the repository ID the CLI
/// derives for the worktree.
async fn persisted_lease(directory: &Path) -> (WriterLease, [u8; 16]) {
    let repository = Repository::discover(directory).expect("repository");
    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen the durable runtime");
    let lease = state
        .current_lease()
        .await
        .expect("read writer lease")
        .expect("a CLI writer persisted its lease");
    (lease, identity.repository_id)
}

fn assert_fenced_successor(previous: WriterLease, next: WriterLease, writer: &str) {
    assert_ne!(
        next.owner_id, previous.owner_id,
        "{writer} must write under its own process owner"
    );
    assert!(
        next.epoch > previous.epoch,
        "{writer} must fence the previous process with a higher epoch: {previous:?} then {next:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn each_cli_writer_process_takes_the_lease_over_under_its_own_owner() {
    let (directory, _repository, _) = format3::format3_repository_for_table(SOURCE, "music");
    let root = directory.path();

    let seed = orna(root, &["run", "seed"]);
    assert_eq!(seed.status.code(), Some(0), "seed run failed: {seed:?}");
    let (after_seed, repository_id) = persisted_lease(root).await;
    assert_ne!(
        after_seed.owner_id, repository_id,
        "the writer owner is per process, not the repository ID"
    );

    let exercise = orna(root, &["run", "exercise"]);
    assert_eq!(
        exercise.status.code(),
        Some(0),
        "second run failed: {exercise:?}"
    );
    let (after_exercise, _) = persisted_lease(root).await;
    assert_fenced_successor(after_seed, after_exercise, "a second `orna run`");

    let repl = orna(
        root,
        &[
            "repl",
            r#"main.music.insert({ id: "paris", title: "New title", artist: "REPL" })"#,
        ],
    );
    assert_eq!(repl.status.code(), Some(0), "REPL effect failed: {repl:?}");
    let (after_repl, _) = persisted_lease(root).await;
    assert_fenced_successor(after_exercise, after_repl, "the REPL");
}
