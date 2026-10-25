//! ORNA-APP-010 / DESIGN S1: `orna serve` answers `GET /` by invoking the
//! selected `main.orna` module's `pub fn serve()`, read-only.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_orna");

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

fn git(project: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(project)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Creates a committed Orna project whose `main.orna` is `source`.
fn project_with_main(source: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("temporary Orna project");
    let initialized = Command::new(BINARY)
        .arg("init")
        .current_dir(project.path())
        .output()
        .expect("initialize project");
    assert!(initialized.status.success(), "initialize: {initialized:?}");
    std::fs::write(project.path().join("main.orna"), source).expect("write main.orna");
    git(project.path(), &["add", "main.orna"]);
    git(
        project.path(),
        &[
            "-c",
            "user.name=Orna test",
            "-c",
            "user.email=orna-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "add application source",
        ],
    );
    project
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Starts `orna serve` in `project` and waits until its listener answers.
fn start_server(project: &Path) -> (RunningServer, String) {
    let port = free_port();
    let child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start local server");
    let server = RunningServer(child);
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if http_get(&address, "/").is_ok() {
            return (server, address);
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("local server becomes available");
}

#[test]
fn serve_result_is_returned_at_root() {
    let project = project_with_main("pub fn serve(): Str = \"serve-marker-7\";\n");
    let (_server, address) = start_server(project.path());
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("serve-marker-7"), "body: {body}");
}

#[test]
fn absent_serve_entry_reports_startup_diagnostic_and_no_root_page() {
    let project = project_with_main("pub fn main(): Int = 42;\n");
    let port = free_port();
    let mut child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start local server");
    let stderr = child.stderr.take().expect("piped stderr");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("E2102") {
                let _ = sender.send(line);
                return;
            }
        }
    });
    let server = RunningServer(child);
    let diagnostic = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("startup diagnostic on stderr");
    assert!(diagnostic.contains("serve"), "diagnostic: {diagnostic}");

    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let (status, body) = loop {
        match http_get(&address, "/") {
            Ok(response) => break response,
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Err(error) => panic!("server did not answer: {error}"),
        }
    };
    drop(server);
    assert_eq!(status, 404, "body: {body}");
    assert!(!body.contains("<html"), "no native root page: {body}");
}

#[test]
fn serve_that_attempts_a_write_is_refused_and_commits_nothing() {
    let project = project_with_main(
        "pub table Note(id: Int) {\n    body: Str,\n}\n\npub fn serve() {\n    Note.insert({ id: 1, body: \"written\" });\n}\n",
    );
    let (_server, address) = start_server(project.path());
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert!(body.contains("serve.write_refused"), "body: {body}");
    // Refusal is repeatable: nothing was committed by the first request.
    let (again, _) = http_get(&address, "/").expect("second GET /");
    assert_eq!(again, 500);
}

fn git_stdout(project: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(project)
        .output()
        .expect("run git query");
    assert!(output.status.success(), "git {arguments:?} failed");
    String::from_utf8(output.stdout).expect("git output is UTF-8")
}

/// Starts `orna serve` with stderr captured to a file, returning the server,
/// its address and everything written to stderr before the listener answered.
fn start_server_with_stderr(project: &Path) -> (RunningServer, String, String) {
    let stderr_capture = tempfile::NamedTempFile::new().expect("stderr capture");
    let port = free_port();
    let child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project)
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            stderr_capture.reopen().expect("reopen stderr capture"),
        ))
        .spawn()
        .expect("start local server");
    let server = RunningServer(child);
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if http_get(&address, "/").is_ok() {
            let stderr = std::fs::read_to_string(stderr_capture.path()).expect("read stderr");
            return (server, address, stderr);
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("local server becomes available");
}

#[test]
fn refused_write_leaves_repository_head_and_worktree_unchanged() {
    let project = project_with_main(
        "pub table Note(id: Int) {\n    body: Str,\n}\n\npub fn serve() {\n    Note.insert({ id: 1, body: \"written\" });\n}\n",
    );
    let head_before = git_stdout(project.path(), &["rev-parse", "HEAD"]);
    // `orna init` leaves its untracked `.orna/` metadata; the request must not change status.
    let status_before = git_stdout(project.path(), &["status", "--porcelain"]);
    let (_server, address) = start_server(project.path());
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert_eq!(
        git_stdout(project.path(), &["rev-parse", "HEAD"]),
        head_before
    );
    assert_eq!(git_stdout(project.path(), &["status", "--porcelain"]), status_before);
}

#[test]
fn private_serve_is_not_the_serving_entry() {
    let project = project_with_main("fn serve(): Str = \"private-marker-9\";\n");
    let (_server, address, stderr) = start_server_with_stderr(project.path());
    assert!(stderr.contains("E2102"), "stderr: {stderr}");
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 404, "body: {body}");
    assert!(!body.contains("private-marker-9"), "body: {body}");
}

#[test]
fn broken_main_reports_and_git_transport_still_serves() {
    let project = project_with_main("pub fn serve( = ;\n");
    let (_server, address, stderr) = start_server_with_stderr(project.path());
    assert!(stderr.contains("ORNA-PARSE-001"), "stderr: {stderr}");
    let (status, _) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 404);
    let clone_root = tempfile::tempdir().expect("clone root");
    let clone = Command::new("git")
        .args([
            "clone",
            "--quiet",
            &format!("http://{address}/git"),
            "clone",
        ])
        .current_dir(clone_root.path())
        .output()
        .expect("run git clone");
    assert!(
        clone.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
}

#[test]
fn semantic_broken_main_reports_and_git_transport_still_serves() {
    let project = project_with_main("pub fn serve(): Int = \"text\";\n");
    let (_server, address, stderr) = start_server_with_stderr(project.path());
    assert!(stderr.contains("E2101"), "stderr: {stderr}");
    assert!(stderr.contains("serving module failed semantic analysis"), "stderr: {stderr}");
    let (status, _) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 404);
    let clone_root = tempfile::tempdir().expect("clone root");
    let clone = Command::new("git")
        .args([
            "clone",
            "--quiet",
            &format!("http://{address}/git"),
            "clone",
        ])
        .current_dir(clone_root.path())
        .output()
        .expect("run git clone");
    assert!(
        clone.status.success(),
        "git clone: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
}

/// Creates a project whose `Note` table is committed through `orna commit`, so
/// the table has a stable object id. Returns the project and the relation id.
fn committed_project(source: &str) -> (tempfile::TempDir, String) {
    let project = tempfile::tempdir().expect("temporary Orna project");
    let initialized = Command::new(BINARY)
        .arg("init")
        .current_dir(project.path())
        .output()
        .expect("initialize project");
    assert!(initialized.status.success(), "initialize: {initialized:?}");
    git(project.path(), &["config", "user.name", "Orna test"]);
    git(
        project.path(),
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(project.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::write(project.path().join("main.orna"), source).expect("write main.orna");
    git(project.path(), &["add", "main.orna", ".orna/database.orna"]);
    let committed = Command::new(BINARY)
        .args(["commit", "-m", "Declare Note"])
        .current_dir(project.path())
        .output()
        .expect("orna commit");
    assert!(committed.status.success(), "orna commit: {committed:?}");
    let relation = String::from_utf8_lossy(&committed.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("admitted table Note as relation "))
        .expect("orna commit admits the Note table")
        .trim()
        .to_owned();
    (project, relation)
}

fn query_row_count(project: &Path, relation: &str) -> u64 {
    let query = Command::new(BINARY)
        .args(["query", relation, "--format", "json"])
        .current_dir(project)
        .output()
        .expect("orna query");
    assert!(query.status.success(), "orna query: {query:?}");
    let report: serde_json::Value =
        serde_json::from_slice(&query.stdout).expect("orna query prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

#[test]
fn refused_write_stores_no_row_in_committed_table() {
    let source = "pub table Note(id: Str) {\n    body: Str,\n}\n\npub fn serve() {\n    Note.insert({ id: \"1\", body: \"written\" });\n}\n";
    let (project, relation) = committed_project(source);
    let (_server, address) = start_server(project.path());
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert!(body.contains("serve.write_refused"), "body: {body}");
    assert_eq!(query_row_count(project.path(), &relation), 0);
}
