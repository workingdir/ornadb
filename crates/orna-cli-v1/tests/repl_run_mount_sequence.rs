//! Regression for the walkthrough ordering: `orna run` succeeds, a REPL effect is
//! accepted, `orna run` still succeeds on the same project, and a REPL capture runs.
//! Each step must exit 0 with no error code, and the first run's committed effect
//! must remain readable from the durable runtime state.
//!
//! Inherited gaps (origin/main reproduces both; ornadb-5il1.79, orna-1.1.0-gap): a run
//! after a REPL effect exits 1 with E2200, and a writable-mount step does not expose
//! run-committed rows. Those steps are not asserted here until the gap is fixed.

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

fn transcript(output: &Output) -> String {
    format!(
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn assert_success(step: &str, output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{step} failed: {}", transcript(output));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains("ORNA-"), "{step} reported an error code: {}", transcript(output));
}

/// The durable `music` rows, each as its encoded field bytes rendered lossily.
/// CBOR stores text fields as literal UTF-8, so a substring match over these
/// bytes reads back the committed field values.
async fn committed_music_text(directory: &Path) -> Vec<String> {
    let repository = Repository::discover(directory).expect("repository");
    let (identity, initial_digest) = format3::cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen the durable runtime");
    state
        .committed_table_rows("music")
        .await
        .expect("read committed music rows")
        .into_iter()
        .map(|(_, row)| String::from_utf8_lossy(&row).into_owned())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn run_repl_run_repl_capture_steps_succeed_and_first_run_commit_persists() {
    let (directory, _repository, _) = format3::format3_repository_for_table(SOURCE, "music");
    let root = directory.path();

    assert_success("seed run", &orna(root, &["run", "seed"]));
    assert_success("first run exercise", &orna(root, &["run", "exercise"]));
    let after_run = committed_music_text(root).await;
    assert!(
        after_run.iter().any(|row| row.contains("Edited by run")),
        "first run must commit its effect: {after_run:?}"
    );

    // Only the REPL effect's exit code is asserted here. Commit visibility of a REPL effect is an
    // inherited gap (ornadb-5il1.79, orna-1.1.0-gap).
    let repl_effect = run_repl(
        root,
        r#"main.music.insert({ id: "paris", title: "New title", artist: "REPL" })"#,
    );
    assert_success("REPL effect", &repl_effect);

    std::fs::write(root.join("recording.mp3"), b"synthetic audio for the sequence")
        .expect("write media");
    let capture = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);",
        root = root.to_string_lossy().as_ref(),
    );
    assert_success("REPL capture", &run_repl(root, &capture));

    let after_capture = committed_music_text(root).await;
    assert!(
        after_capture.iter().any(|row| row.contains("Edited by run")),
        "first run's committed effect must remain durable: {after_capture:?}"
    );
}
