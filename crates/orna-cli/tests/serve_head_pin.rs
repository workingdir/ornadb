//! ORNA-SERVE-003 / ORNA-SYS-011 / ORNA-SYS-111 (ornadb-5il1.276, ornadb-jzanz):
//! `orna serve` serves each request at the commit `HEAD` names when the request
//! runs. That commit pins the `sys.git.*` references and the `sys.catalog.*`
//! rows the request reads, so a served page reads them as `orna run` does. A
//! clone with no commit has nothing to pin them to, and its page is refused.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

const BRANCH_COUNT_SOURCE: &str =
    "pub table Note(id: Str) { body: Str, }\n\npub fn serve() = sys.git.branches | count();\n";
const TABLELESS_BRANCH_COUNT_SOURCE: &str = "pub fn serve() = sys.git.branches | count();\n";
const CURRENT_BRANCH_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n\npub fn serve() = sys.git.branches | filter(branch => branch.current) | map(branch => branch.name) | first() ?? \"none\";\n";
const CATALOGUE_COUNT_SOURCE: &str =
    "pub table Note(id: Str) { body: Str, }\n\npub fn serve() = sys.catalog.tables | count();\n";
const TABLELESS_CATALOGUE_COUNT_SOURCE: &str = "pub fn serve() = sys.catalog.tables | count();\n";
/// A one-shot invocation that reads a table beside the Git state, so it takes
/// the committed table read path.
const ONE_SHOT_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n\npub fn notes_and_branches(): Int = (Note | count()) + (sys.git.branches | count());\n\npub fn serve() = 0;\n";

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

/// Runs `git` in `root` and returns its trimmed standard output.
fn git(root: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args([
            "-c",
            "user.name=Orna Test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git prints UTF-8")
        .trim()
        .to_owned()
}

/// `orna init`, `main.orna` set to `source`, and the first commit through
/// `orna commit`, on a branch named `main`.
fn committed_project(directory: &TempDir, source: &str) -> PathBuf {
    let root = directory.path().join("library");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    // CI has no Git identity, and `orna commit` refuses to commit without one.
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare the project"]);
    assert_success(&committed, "orna commit");
    if git(&root, &["symbolic-ref", "--short", "HEAD"]) != "main" {
        git(&root, &["branch", "-m", "main"]);
    }
    root
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);
    port
}

struct RunningServer(Child);

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http_get(address: &str, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(address)?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (headers, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_default();
    Ok((status, body.to_owned()))
}

/// Starts `orna serve` in `root` and waits until its listener answers.
fn start_server(root: &Path) -> (RunningServer, String) {
    let port = free_port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let child = command
        .args(["serve", "--port", &port.to_string()])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start local server");
    let server = RunningServer(child);
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if http_get(&address, "/").is_ok() {
            return (server, address);
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("local server becomes available on {address}");
}

/// The page the running server answers, which must be a 200.
fn served(address: &str) -> String {
    let (status, body) = http_get(address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    body.trim().to_owned()
}

#[test]
// Specified: ORNA-SERVE-003; a page reads the local branches of the clone it
// serves, and each request reads them as they are when it runs.
fn a_served_page_counts_the_local_branches_of_the_clone() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, BRANCH_COUNT_SOURCE);
    let (_server, address) = start_server(&root);
    assert_eq!(served(&address), "1 : Int");
    git(&root, &["branch", "topic"]);
    assert_eq!(
        served(&address),
        "2 : Int",
        "a branch created after the server started is read by the next request"
    );
}

#[test]
// Specified: ORNA-SERVE-003; a project that declares no table has its branches
// pinned the same way, since the pin comes from the clone's HEAD and not from a
// table identity.
fn a_served_page_of_a_project_with_no_tables_counts_the_local_branches() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, TABLELESS_BRANCH_COUNT_SOURCE);
    let (_server, address) = start_server(&root);
    assert_eq!(served(&address), "1 : Int");
    git(&root, &["branch", "topic"]);
    assert_eq!(served(&address), "2 : Int");
}

#[test]
// Specified: 34-system-reference.md `sys.Branch`; `current` marks the branch
// HEAD is attached to when the request runs.
fn a_served_page_marks_the_branch_head_is_attached_to() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, CURRENT_BRANCH_SOURCE);
    let (_server, address) = start_server(&root);
    assert_eq!(served(&address), "\"main\" : Str");
    git(&root, &["checkout", "-q", "-b", "topic"]);
    assert_eq!(
        served(&address),
        "\"topic\" : Str",
        "the next request follows HEAD to the branch it is attached to"
    );
}

#[test]
// Specified: ORNA-SYS-011; the commit a request is pinned to is the one HEAD
// names when it runs, so a commit made while the server runs does not leave the
// next request without a pin.
fn a_served_page_reads_the_branches_after_head_moves_to_a_new_commit() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, BRANCH_COUNT_SOURCE);
    let (_server, address) = start_server(&root);
    assert_eq!(served(&address), "1 : Int");
    git(&root, &["commit", "-q", "--allow-empty", "-m", "Move HEAD"]);
    git(&root, &["branch", "topic"]);
    assert_eq!(served(&address), "2 : Int");
}

#[test]
// Specified: ORNA-SYS-111, ORNA-SYS-112; the catalogue rows of a page carry the
// identities committed at the request's commit.
fn a_served_page_reads_the_catalogue_at_the_head() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, CATALOGUE_COUNT_SOURCE);
    let (_server, address) = start_server(&root);
    assert_eq!(served(&address), "1 : Int");
}

#[test]
// Specified: ORNA-SYS-111; a project of no tables serves an empty catalogue.
fn a_served_page_of_a_project_with_no_tables_reads_an_empty_catalogue() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, TABLELESS_CATALOGUE_COUNT_SOURCE);
    let (_server, address) = start_server(&root);
    assert_eq!(served(&address), "0 : Int");
}

#[test]
// Specified: ORNA-SERVE-003, ORNA-SYS-111; a clone whose HEAD names no commit
// has nothing to pin its branches to. Another branch exists, so serving rows
// would need a pin the page does not have: the page is refused with no value.
fn a_served_page_of_a_clone_with_no_commit_is_refused() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, BRANCH_COUNT_SOURCE);
    git(&root, &["checkout", "-q", "--orphan", "unborn"]);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert_eq!(body.trim(), r#"{"error":"serve.entry_unavailable"}"#);
    assert!(!body.contains(": Int"), "no value is served: {body}");
}

#[test]
// Specified: ORNA-SYS-011; a one-shot invocation that reads a table is pinned to
// the commit HEAD names, as a served page is.
fn a_one_shot_table_invocation_reads_the_branches_of_the_clone() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, ONE_SHOT_SOURCE);
    let ran = orna(&root, &["run", "main.notes_and_branches"]);
    assert_success(&ran, "orna run");
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        "1 : Int\n",
        "{}",
        transcript(&ran)
    );
}
