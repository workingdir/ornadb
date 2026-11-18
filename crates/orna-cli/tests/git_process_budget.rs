//! Bounds the number of Git processes a read command starts on a repository
//! holding fifty published rows. Git's trace2 event log records one `start`
//! event per process, so the count is exact and independent of timing.
//! Reads go through the persistent object session wherever possible; a
//! regression that spawns Git per row would multiply this count by the row
//! total and fail the bound.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const SOURCE: &str =
    "pub table music(id: Str) { title: Str, artist: Str, }\npub fn main() = music | count();\n";
const ROWS: usize = 50;
/// `orna status` on the fixture starts ten Git processes: repository discovery
/// (two), the HEAD resolutions, the object-format probe and object session,
/// the pin listing, and `git status`. The bound leaves no slack for a per-row
/// spawn.
const STATUS_GIT_PROCESS_BUDGET: usize = 10;

/// Keeps the host's Git configuration and repository routing out of every
/// process this test starts.
fn isolate_git(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut child = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the orna REPL starts");
    let stdin = child.stdin.take().expect("REPL stdin");
    // Feed stdin from a writer thread so a long script cannot deadlock against
    // the child's stdout.
    let payload = format!("{input}\n:quit\n");
    let writer = std::thread::spawn(move || {
        let mut stdin = stdin;
        stdin.write_all(payload.as_bytes())
    });
    let output = child.wait_with_output().expect("the orna REPL exits");
    writer
        .join()
        .expect("REPL writer thread joins")
        .expect("write REPL input");
    output
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git prints UTF-8")
}

fn transcript(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, step: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{step} failed\n{}",
        transcript(output)
    );
}

/// A database with `ROWS` published rows in `music`.
fn fifty_row_database(directory: &TempDir) -> PathBuf {
    let root = directory.path().join("music");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), SOURCE).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    assert_success(
        &orna(&root, &["commit", "-m", "Declare music"]),
        "orna commit",
    );

    let inserts = (0..ROWS)
        .map(|index| {
            format!(
                r#"main.music.insert({{ id: "r{index:02}", title: "T{index:02}", artist: "A" }})"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_success(&orna_repl(&root, &inserts), "REPL inserts");
    assert_success(
        &orna(&root, &["publish", "--message", "Add fifty rows"]),
        "orna publish",
    );
    root
}

/// The number of Git processes `orna` starts for `arguments`, read from the
/// trace2 event log every child inherits.
fn git_processes_started(root: &Path, arguments: &[&str]) -> usize {
    let trace = root
        .parent()
        .expect("the fixture has a parent")
        .join("trace2.json");
    let _ = fs::remove_file(&trace);
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(root)
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .expect("the orna process starts");
    assert_success(&output, &format!("orna {arguments:?}"));
    let events = fs::read_to_string(&trace).expect("trace2 writes the event log");
    events
        .lines()
        .filter(|line| line.contains("\"event\":\"start\""))
        .count()
}

#[test]
fn status_on_a_fifty_row_repository_starts_a_bounded_number_of_git_processes() {
    let directory = TempDir::new().expect("create the test directory");
    let root = fifty_row_database(&directory);

    let started = git_processes_started(&root, &["status"]);
    assert!(
        started <= STATUS_GIT_PROCESS_BUDGET,
        "orna status started {started} git processes on {ROWS} rows; budget is {STATUS_GIT_PROCESS_BUDGET}"
    );
}
