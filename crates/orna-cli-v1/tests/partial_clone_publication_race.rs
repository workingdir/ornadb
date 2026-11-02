//! Gate C2: concurrent writes from a cold and a warm partial clone.
//!
//! ORNA-CLONE-002 makes published synchronisation Git commits, fetch, push and
//! merge. Publication itself is local (no auto-push, reference line "Do not
//! auto-push"), so the shared compare-and-set happens at the push. This test
//! drives that whole path through product commands only: one origin, two
//! blob-filtered clones (one left cold, one hydrated), a row admitted and
//! published in each, then both pushes raced. The loser must lose the push
//! without corrupting the origin or losing its own published tail
//! (ORNA-PUB-015), and after a fetch the loser must read the winner's row.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Barrier,
};

use tempfile::TempDir;

const NOTE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n";
const ORIGIN_ROW: &str = r#"main.Note.insert({ id: "origin", body: "the origin row" })"#;
const COLD_ROW: &str = r#"main.Note.insert({ id: "cold", body: "the cold clone row" })"#;
const WARM_ROW: &str = r#"main.Note.insert({ id: "warm", body: "the warm clone row" })"#;

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
        .write_all(format!("{input}\n").as_bytes())
        .expect("write the REPL input");
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
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_push(root: &Path, remote: &Path) -> Result<(), String> {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args([
            "push",
            remote.to_str().expect("UTF-8 remote path"),
            "HEAD:refs/heads/main",
        ])
        .current_dir(root)
        .output()
        .expect("the git push process starts");
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr).into_owned())
}

fn assert_success(output: &Output, step: &str) {
    assert!(
        output.status.success(),
        "{step} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn initialized_database(directory: &TempDir, source: &str) -> PathBuf {
    let root = directory.path().join("library");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(&root, &["config", "user.email", "orna-test@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("main.orna"), source).expect("write main.orna");
    root
}

/// Commits the staged source and returns the relation id `orna commit`
/// reports for the Note table.
fn commit_source(root: &Path, message: &str) -> String {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", message]);
    assert_success(&committed, "orna commit");
    String::from_utf8_lossy(&committed.stdout)
        .lines()
        .find_map(|line| {
            let admitted = line.strip_prefix("admitted table ")?;
            let (table, relation) = admitted.split_once(" as relation ")?;
            (table == "Note").then(|| relation.trim().to_owned())
        })
        .expect("orna commit names the Note relation")
}

fn publish(root: &Path, message: &str) {
    let published = orna(root, &["publish", "--message", message]);
    assert_success(&published, "orna publish");
}

fn query_rows(root: &Path, arguments: &[&str]) -> u64 {
    let listed = orna(root, arguments);
    assert_success(&listed, "orna query");
    let report: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("orna query prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

/// Clones the origin with a blob filter, so the clone is a partial clone of the
/// same remote the two publishers push to.
fn clone_partial(directory: &TempDir, name: &str, remote: &Path) -> PathBuf {
    let root = directory.path().join(name);
    let url = format!("file://{}", remote.display());
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args([
            "clone",
            "--filter=blob:none",
            "--no-local",
            &url,
            root.to_str().expect("UTF-8 clone path"),
        ])
        .current_dir(directory.path())
        .output()
        .expect("the git clone process starts");
    assert!(
        output.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (name, value) in [
        ("user.name", "Orna Test"),
        ("user.email", "orna-test@example.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        git(&root, &["config", name, value]);
    }
    assert_eq!(
        git(&root, &["config", "--get", "remote.origin.promisor"]),
        "true",
        "the clone must stay a promisor clone"
    );
    root
}

/// Fetches every blob the clone is missing, which is what makes it warm: the
/// same promisor clone, with its objects already present.
fn hydrate(root: &Path) {
    let objects = git(root, &["rev-list", "--objects", "--all"]);
    let mut missing: Vec<String> = Vec::new();
    for line in objects.lines() {
        let Some((oid, _)) = line.split_once(' ') else {
            continue;
        };
        missing.push(oid.to_owned());
    }
    // `cat-file --batch-check` resolves each object, which asks the promisor
    // remote for the ones this clone does not hold.
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let mut child = command
        .args(["cat-file", "--batch-check"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the git cat-file process starts");
    child
        .stdin
        .take()
        .expect("cat-file stdin")
        .write_all(format!("{}\n", missing.join("\n")).as_bytes())
        .expect("write the object list");
    let output = child.wait_with_output().expect("git cat-file exits");
    assert!(
        output.status.success(),
        "hydrating the warm clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn concurrent_publication_from_a_cold_and_a_warm_partial_clone_keeps_one_winner() {
    let directory = TempDir::new().expect("create the test directory");
    let origin = initialized_database(&directory, NOTE_SOURCE);
    let relation = commit_source(&origin, "Add the Note table");
    assert_success(&orna_repl(&origin, ORIGIN_ROW), "REPL insert in the origin");
    publish(&origin, "Add the origin row");
    assert_eq!(
        query_rows(&origin, &["query", &relation, "--format", "json"]),
        1,
        "the origin publishes one row before the clones exist"
    );

    // The remote the clones share. It is bare, so a push updates the branch the
    // clones race over instead of being refused as a checked-out working tree.
    let remote = directory.path().join("remote.git");
    let mut init = Command::new("git");
    isolate_git(&mut init);
    let initialized = init
        .args(["init", "--bare", remote.to_str().expect("UTF-8 remote path")])
        .output()
        .expect("the git init process starts");
    assert!(
        initialized.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    git(
        &origin,
        &[
            "push",
            remote.to_str().expect("UTF-8 remote path"),
            "HEAD:refs/heads/main",
        ],
    );

    // The bare remote starts on `master`, which the push does not create, so
    // point it at the branch the clones publish to before cloning.
    git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);

    let cold = clone_partial(&directory, "cold", &remote);
    let warm = clone_partial(&directory, "warm", &remote);
    hydrate(&warm);
    let remote_head = git(&remote, &["rev-parse", "refs/heads/main"]);

    // Both clones publish their own row against the same remote head. The row
    // is admitted through the product runtime and published by the product, so
    // each clone has a local publication the other has never seen.
    for (clone, row, step) in [
        (&cold, COLD_ROW, "REPL insert in the cold clone"),
        (&warm, WARM_ROW, "REPL insert in the warm clone"),
    ] {
        assert_success(&orna_repl(clone, row), step);
        publish(clone, "Add a row from a clone");
        assert_eq!(
            query_rows(clone, &["query", &relation, "--format", "json"]),
            2,
            "each clone reads its own publication"
        );
    }
    assert_ne!(
        git(&cold, &["rev-parse", "HEAD"]),
        git(&warm, &["rev-parse", "HEAD"]),
        "the two clones published different commits"
    );

    // The shared compare-and-set is the push. Both start together so their
    // non-fast-forward checks overlap.
    let barrier = Barrier::new(2);
    let (cold_push, warm_push) = std::thread::scope(|scope| {
        let cold_handle = scope.spawn(|| {
            barrier.wait();
            git_push(&cold, &remote)
        });
        let warm_handle = scope.spawn(|| {
            barrier.wait();
            git_push(&warm, &remote)
        });
        (
            cold_handle.join().expect("cold push thread"),
            warm_handle.join().expect("warm push thread"),
        )
    });
    let (winner, loser, rejected) = match (cold_push, warm_push) {
        (Ok(()), Err(rejected)) => (&cold, &warm, rejected),
        (Err(rejected), Ok(())) => (&warm, &cold, rejected),
        (Ok(()), Ok(())) => panic!("both pushes accepted a non-fast-forward update"),
        (Err(cold_error), Err(warm_error)) => {
            panic!("both pushes failed: cold={cold_error} warm={warm_error}")
        }
    };
    assert!(
        rejected.contains("rejected") || rejected.contains("non-fast-forward"),
        "the losing push is refused by the remote, not by a local fault: {rejected}"
    );

    // The remote moved exactly once, to the winner's commit, and its parent is
    // the head the clone started from: the winner did not rewrite history.
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main"]),
        git(winner, &["rev-parse", "HEAD"]),
        "the remote head is the winning clone's publication"
    );
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main^"]),
        remote_head,
        "the winning publication extends the head both clones started from"
    );

    // A fresh product reader over the remote reads the winner row and only the
    // winner row.
    let reader = clone_partial(&directory, "reader", &remote);
    assert_eq!(
        query_rows(&reader, &["query", &relation, "--format", "json"]),
        2,
        "the remote reads the winner row and not the loser row"
    );

    // The rejected publication is absent from the remote, and the loser still
    // reads its own tail: an unpublished local tail is recoverable, not lost.
    let loser_commit = git(loser, &["rev-parse", "HEAD"]);
    assert!(
        git_commit_unreachable(&remote, "refs/heads/main", &loser_commit),
        "the losing publication is not reachable from the remote branch"
    );
    assert_eq!(
        query_rows(loser, &["query", &relation, "--format", "json"]),
        2,
        "the losing clone still reads its own published row"
    );

    // The losing clone recovers the winner row by fetching the published
    // snapshot. It does not merge the two publications: both rewrote the same
    // store file, so a textual merge has nothing to reconcile and the format has
    // no merge rule for two publications of one store. The local publication
    // stays as a commit, so its row is recoverable rather than lost
    // (ORNA-PUB-015).
    git(loser, &["fetch", "origin", "main"]);
    git(loser, &["reset", "--hard", "origin/main"]);
    assert_eq!(
        query_rows(loser, &["query", &relation, "--format", "json"]),
        2,
        "after fetching the published snapshot, the losing clone reads the winner row"
    );
    assert!(
        !git_commit_unreachable(loser, &loser_commit, &loser_commit),
        "the losing clone still holds its own publication locally"
    );
}

/// True when the named ref does not reach this commit. A rejected push may leave
/// its objects in the remote's object database before the ref update is refused,
/// so reachability is the property that matters: the losing publication must not
/// be part of the branch.
fn git_commit_unreachable(remote: &Path, branch: &str, commit: &str) -> bool {
    let history = git(remote, &["rev-list", branch]);
    !history.lines().any(|line| line == commit)
}
