//! Writer uniqueness at the publication entry point, through product
//! commands only: two `orna publish` processes start against one repository
//! holding one frozen tail, and the pair must admit at most one batch
//! (ORNA-PUB-009 compare-and-set, ORNA-PUB-006 preserved pending batch).
//!
//! `orna publish` deliberately takes no writer lease: the documented workflow
//! is a REPL that owns the writer lease while a separate process publishes the
//! durable tail (22-publication PUB-3 step 7 excludes cooperating mutators with
//! the publication lock, not with the runtime writer lease). What must hold
//! instead is that two publishers cannot each admit the same tail, so this test
//! races the two processes and reads the result back with `orna query`.

use std::{
    fs,
    io::BufRead,
    io::BufReader,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
};

use tempfile::TempDir;

const NOTE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n";

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
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the orna REPL exits")
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
    assert!(
        !transcript(output).contains("error["),
        "{step} reported an error\n{}",
        transcript(output)
    );
}

/// `orna init`, a test-local commit identity, `main.orna` replaced by the
/// source, then the source staged with Git and committed with `orna commit`.
fn committed_database(directory: &TempDir, source: &str) -> (PathBuf, String) {
    let root = directory.path().join("notes");
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
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare Note"]);
    assert_success(&committed, "orna commit");
    let relation = String::from_utf8_lossy(&committed.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("admitted table Note as relation "))
        .expect("orna commit admits the Note table")
        .trim()
        .to_owned();
    (root, relation)
}

fn query_rows(root: &Path, relation: &str) -> u64 {
    let output = orna(root, &["query", relation, "--format", "json"]);
    assert_success(&output, "orna query");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the command prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

/// Starts one `orna publish` without waiting for it, so two publishers can be
/// in flight at once.
fn spawn_publish(root: &Path, message: &str, log: &Path) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    command
        .args(["publish", "--message", message])
        .current_dir(root)
        .stdout(Stdio::from(
            fs::File::create(log).expect("create the publisher log"),
        ))
        .stderr(Stdio::from(
            fs::OpenOptions::new()
                .append(true)
                .open(log)
                .expect("reopen the publisher log"),
        ))
        .spawn()
        .expect("the publish process starts")
}

/// Two publishers start against one repository with one durable pending row.
/// Exactly one of them may admit that tail: the other must report nothing to
/// publish or a named refusal, never a second commit.
#[test]
fn concurrent_publish_processes_admit_one_batch() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation) = committed_database(&directory, NOTE_SOURCE);
    assert_success(
        &orna_repl(&root, r#"main.Note.insert({ id: "pump", body: "first" })"#),
        "REPL insert",
    );
    let base = git(&root, &["rev-parse", "HEAD"]).trim().to_owned();

    let first_log = directory.path().join("publish-1.log");
    let second_log = directory.path().join("publish-2.log");
    let mut first = spawn_publish(&root, "Publish from the first process", &first_log);
    let mut second = spawn_publish(&root, "Publish from the second process", &second_log);
    let first_status = first.wait().expect("the first publisher exits");
    let second_status = second.wait().expect("the second publisher exits");

    let mut logs = Vec::new();
    for log in [&first_log, &second_log] {
        logs.push(fs::read_to_string(log).expect("the publisher log is readable"));
    }
    let combined = logs.join("\n");

    // Publication is not a lease holder, so neither process may be told that a
    // writer lease is in the way: the pair must resolve on the publication
    // boundary itself.
    assert!(
        !combined.contains("writer lease"),
        "publish must not claim the runtime writer lease: {combined}"
    );
    assert!(
        first_status.success() || second_status.success(),
        "neither publisher admitted the tail: {combined}"
    );

    // Exactly one publication commit lands, and the loser leaves no batch
    // behind: ORNA-PUB-009 allows one compare-and-set winner, ORNA-PUB-006
    // keeps the pending batch instead of reporting a false publication.
    let committed = git(&root, &["rev-list", "--count", &format!("{base}..HEAD")]);
    assert_eq!(
        committed.trim(),
        "1",
        "publication produced {committed} commits instead of 1: {combined}"
    );
    let head_message = git(&root, &["log", "-1", "--format=%s", "HEAD"]);
    let loser_said_nothing = logs
        .iter()
        .filter(|log| log.contains("nothing to publish"))
        .count();
    assert!(
        head_message.contains("Publish from the first process")
            || head_message.contains("Publish from the second process"),
        "the publication commit carries no publisher message: {head_message}"
    );
    assert!(
        loser_said_nothing <= 1,
        "more than one publisher reported nothing to publish: {combined}"
    );

    // The row is readable afterwards, once.
    assert_eq!(
        query_rows(&root, &relation),
        1,
        "the published row reads back once: {combined}"
    );
    assert!(
        !root
            .join(".orna")
            .join("runtime")
            .join("publication-journal.bin")
            .exists(),
        "a publication journal remains after the pair completed"
    );
    assert!(
        !root.join(".git").join("index.lock").exists(),
        "a Git index lock remains after the pair completed"
    );
}

/// A publisher that runs while a REPL holds the runtime writer lease is the
/// documented workflow, not a conflict: the lease guards runtime writes, the
/// publication lock guards the ref transition. This pins that separation so a
/// later change cannot turn a routine publish behind a live REPL into a refund
/// of the user's tail.
#[test]
fn publish_behind_a_live_writer_lease_still_admits_the_tail() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation) = committed_database(&directory, NOTE_SOURCE);

    // A REPL that stays attached after its write holds the runtime writer
    // lease for as long as it runs.
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut repl = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the orna REPL starts");
    let mut stdin = repl.stdin.take().expect("REPL stdin");
    stdin
        .write_all(b"main.Note.insert({ id: \"lease\", body: \"held\" })\n")
        .expect("write the REPL insert");
    stdin.flush().expect("flush the REPL insert");
    // The REPL echoes the admitted record after the write is durable, so the
    // echo is the barrier: publishing before it would race the runtime write
    // rather than test the lease.
    let mut echoed = String::new();
    let mut reader = BufReader::new(repl.stdout.take().expect("REPL stdout"));
    while !echoed.contains("\"lease\"") {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read the REPL echo");
        assert!(
            read > 0,
            "the REPL exited without echoing the insert: {echoed}"
        );
        echoed.push_str(&line);
    }

    let published = orna(
        &root,
        &["publish", "--message", "Publish behind a live REPL"],
    );
    assert_success(&published, "orna publish behind a live REPL");
    let head_message = git(&root, &["log", "-1", "--format=%s", "HEAD"]);
    assert!(
        head_message.contains("Publish behind a live REPL"),
        "the live-lease publication reached HEAD: {head_message}"
    );
    assert_eq!(
        query_rows(&root, &relation),
        1,
        "the row written under the live lease reads back once"
    );

    stdin.write_all(b":quit\n").expect("quit the REPL");
    drop(stdin);
    let status = repl.wait().expect("the orna REPL exits");
    assert!(
        status.success(),
        "the REPL exited with {status:?}: {echoed}"
    );
    assert!(
        !echoed.contains("error["),
        "the REPL reported an error: {echoed}"
    );
}

/// A second REPL while one is live is the writer-uniqueness refusal itself: the
/// process that did not take the lease must be told another writer owns it and
/// what to do, and it must not write. ORNA-TURSO-005 names the refusal; the
/// remedy names the mount so the user is not left guessing which writer.
#[test]
fn a_second_repl_is_refused_naming_the_live_writer() {
    let directory = TempDir::new().expect("create the test directory");
    let (root, relation) = committed_database(&directory, NOTE_SOURCE);

    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut holder = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the holding REPL starts");
    let mut holder_stdin = holder.stdin.take().expect("holding REPL stdin");
    holder_stdin
        .write_all(b"main.Note.insert({ id: \"holder\", body: \"kept\" })\n")
        .expect("write the holding insert");
    holder_stdin.flush().expect("flush the holding insert");
    let mut echoed = String::new();
    let mut reader = BufReader::new(holder.stdout.take().expect("holding REPL stdout"));
    while !echoed.contains("\"holder\"") {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read the holding echo");
        assert!(read > 0, "the holding REPL exited early: {echoed}");
        echoed.push_str(&line);
    }

    // The second REPL names the live writer and offers the remedy. The REPL
    // reports a failed statement and keeps its prompt, so the process itself
    // still exits successfully; the refusal is the diagnostic, not the exit
    // status.
    let refused = orna_repl(&root, r#"main.Note.insert({ id: "second", body: "no" })"#);
    let refusal = transcript(&refused);
    assert!(
        refusal.contains("writer lease"),
        "the refusal does not name the writer lease: {refusal}"
    );
    assert!(
        refusal.contains("orna unmount") || refusal.contains("live writer"),
        "the refusal does not name the holder or the remedy: {refusal}"
    );
    assert!(
        !refusal.contains(r#""id": "second""#),
        "the refused REPL still wrote its row: {refusal}"
    );

    holder_stdin
        .write_all(b":quit\n")
        .expect("quit the holding REPL");
    drop(holder_stdin);
    let status = holder.wait().expect("the holding REPL exits");
    assert!(status.success(), "the holding REPL exited with {status:?}");

    // After the holder exits, the row it wrote is still durable and readable
    // once its tail is published.
    let published = orna(
        &root,
        &["publish", "--message", "Publish after the holder exits"],
    );
    assert_success(&published, "orna publish after the holder exits");
    assert_eq!(
        query_rows(&root, &relation),
        1,
        "only the holder's row is durable after the refusal"
    );
}
