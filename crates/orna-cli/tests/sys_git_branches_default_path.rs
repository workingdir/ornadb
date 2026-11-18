//! The default path serves the linked `sys.git.branches` member (`sys.Branch`)
//! from the repository of a project, through both `orna run` and `orna repl`:
//! one row per local branch, the branch `HEAD` is attached to marked `current`,
//! and `ahead`/`behind` counted against the upstream a branch tracks and null
//! when it has none (ORNA-SYS-111, ORNA-SYS-112). Rows are never served without
//! the commit pin their references need.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const SOURCE: &str = r#"pub table Item(id: Str) {
    name: Str,
}

pub fn branch_count(): Int {
    sys.git.branches | count()
}

pub fn current_branch(): Str {
    sys.git.branches | filter(b => b.current) | map(b => b.name) | first() ?? "none"
}

pub fn topic_counts(): Int {
    sys.git.branches | filter(b => b.name == "topic") | map(b => (b.ahead ?? -1) * 10 + (b.behind ?? -1)) | first() ?? -99
}

pub fn main_counts(): Int {
    sys.git.branches | filter(b => b.name == "main") | map(b => (b.ahead ?? -1) * 10 + (b.behind ?? -1)) | first() ?? -99
}

pub fn pairs_sharing_a_commit(): Int {
    sys.git.branches | flat_map(a => sys.git.branches | filter(b => a.name != b.name && a.commit == b.commit) | map(b => 1)) | count()
}

pub fn pairs_sharing_a_reference(): Int {
    sys.git.branches | flat_map(a => sys.git.branches | filter(b => a.name != b.name && a.reference == b.reference) | map(b => 1)) | count()
}
"#;

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

/// Runs `git` in `directory` under a fixed identity and returns its trimmed
/// standard output.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .output()
        .expect("git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

/// An initialised project with `SOURCE` staged and no commit.
fn uncommitted_project() -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), SOURCE).expect("project source");
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Orna Test"]);
    git(
        directory.path(),
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(directory.path(), &["add", "-A"]);
    directory
}

/// A project published with `orna commit` on a branch named `main`.
fn committed_project() -> TempDir {
    let directory = uncommitted_project();
    let committed = orna(directory.path(), &["commit", "-m", "publish branches"]);
    assert!(committed.status.success(), "commit: {committed:?}");
    if git(directory.path(), &["symbolic-ref", "--short", "HEAD"]) != "main" {
        git(directory.path(), &["branch", "-m", "main"]);
    }
    directory
}

/// A committed project whose `topic` branch tracks `main` and has diverged
/// from it: `topic` is one commit ahead and two behind, and `main` tracks
/// nothing. `HEAD` stays attached to `main`, and the work tree is unchanged.
fn diverged_project() -> TempDir {
    let directory = committed_project();
    let root = directory.path();
    let base = git(root, &["rev-parse", "HEAD"]);
    git(root, &["branch", "topic"]);
    git(root, &["commit", "--allow-empty", "-m", "main one"]);
    git(root, &["commit", "--allow-empty", "-m", "main two"]);
    let tree = format!("{base}^{{tree}}");
    let topic = git(
        root,
        &["commit-tree", &tree, "-p", &base, "-m", "topic one"],
    );
    git(root, &["update-ref", "refs/heads/topic", &topic]);
    git(root, &["branch", "--set-upstream-to=main", "topic"]);
    directory
}

fn repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the REPL exits")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Runs `main.FUNCTION` and returns what it printed.
fn run(directory: &Path, function: &str) -> String {
    let output = orna(directory, &["run", &format!("main.{function}")]);
    let text = combined(&output);
    assert!(output.status.success(), "{function}: {text}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
// Specified: 34-system-reference.md `sys.Branch`; one row per local branch.
fn orna_run_counts_the_local_branches() {
    let project = committed_project();
    assert_eq!(run(project.path(), "branch_count"), "1 : Int\n");
    git(project.path(), &["branch", "topic"]);
    assert_eq!(run(project.path(), "branch_count"), "2 : Int\n");
    git(project.path(), &["branch", "third"]);
    assert_eq!(run(project.path(), "branch_count"), "3 : Int\n");
}

#[test]
fn orna_run_marks_the_branch_head_is_attached_to_as_current() {
    let project = diverged_project();
    assert_eq!(run(project.path(), "current_branch"), "\"main\" : Str\n");
    git(project.path(), &["checkout", "-q", "topic"]);
    assert_eq!(run(project.path(), "current_branch"), "\"topic\" : Str\n");
}

#[test]
// Specified: `ahead` and `behind` are null only when there is no upstream.
fn orna_run_counts_ahead_and_behind_against_the_upstream() {
    let project = diverged_project();
    assert_eq!(
        run(project.path(), "topic_counts"),
        "12 : Int\n",
        "topic is one commit ahead of main and two behind"
    );
    assert_eq!(
        run(project.path(), "main_counts"),
        "-11 : Int\n",
        "main tracks nothing, so it has neither count"
    );
}

#[test]
// Specified: ORNA-SYS-112; a reference is the coordinates of its row, so two
// branches at distinct commits have distinct commit references and two at one
// commit share it.
fn orna_run_gives_branches_at_distinct_commits_distinct_commit_references() {
    let project = diverged_project();
    assert_eq!(run(project.path(), "pairs_sharing_a_commit"), "0 : Int\n");
    assert_eq!(
        run(project.path(), "pairs_sharing_a_reference"),
        "0 : Int\n"
    );

    let topic = git(project.path(), &["rev-parse", "topic"]);
    git(project.path(), &["branch", "twin", &topic]);
    assert_eq!(
        run(project.path(), "pairs_sharing_a_commit"),
        "2 : Int\n",
        "topic and twin name one commit"
    );
    assert_eq!(
        run(project.path(), "pairs_sharing_a_reference"),
        "0 : Int\n",
        "every branch has a reference of its own"
    );
}

#[test]
// Specified: ORNA-SYS-111. A repository with no commit has no branch.
fn orna_run_of_a_project_with_no_commit_has_no_branches() {
    let project = uncommitted_project();
    assert_eq!(run(project.path(), "branch_count"), "0 : Int\n");
}

#[test]
// Specified: ORNA-SYS-111. With branches but no `HEAD` commit there is no pin
// for their references, so the read fails closed with `sys.snapshot.incomplete`
// instead of serving rows with null or invented references.
fn orna_run_refuses_branches_it_cannot_pin() {
    let project = committed_project();
    git(project.path(), &["checkout", "-q", "--orphan", "fresh"]);
    let output = orna(project.path(), &["run", "main.branch_count"]);
    let text = combined(&output);
    assert!(
        !output.status.success(),
        "branches are not served without a commit pin: {text}"
    );
    assert!(
        text.contains("sys.git.branches"),
        "the refusal points at the branch read: {text}"
    );
    assert!(!text.contains(": Int"), "no row was served: {text}");
}

#[test]
fn repl_reads_the_branches_of_the_project() {
    let project = diverged_project();
    let output = repl(project.path(), "sys.git.branches | map(b => b.name)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"main\"") && text.contains("\"topic\""),
        "one row per local branch: {text}"
    );
}

#[test]
// The REPL elides the contents of a reference, so this pins only that a
// tracking branch's `upstream` is a present `sys.RefRef`; the key of that
// reference (the full tracked ref name) is pinned by
// `crates/orna-evaluator/tests/sys_git_branches.rs`.
fn repl_reads_the_upstream_of_a_tracking_branch_as_a_reference() {
    let project = diverged_project();
    let output = repl(
        project.path(),
        r#"sys.git.branches | filter(b => b.name == "topic") | map(b => b.upstream)"#,
    );
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("Relation<sys.RefRef?>") && text.contains("Option([1, RowRef("),
        "the upstream is a present `sys.RefRef`: {text}"
    );
}
