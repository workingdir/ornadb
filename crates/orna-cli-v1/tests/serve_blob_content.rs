//! ORNA-APP-014 / DESIGN S3: stored Blob content is served as exact bytes with
//! explicit byte-range support, and a payload that is not an allowlisted media
//! type is served as an attachment that cannot run script.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
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

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn request(address: &str, method: &str, path: &str, extra: &str) -> std::io::Result<Reply> {
    let mut stream = TcpStream::connect(address)?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{extra}Connection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(raw.len(), |index| index + 4);
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = raw.get(split..).unwrap_or_default().to_vec();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_default();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body,
    })
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

/// Creates a committed project holding `serve()` plus the given files.
fn project_with_files(files: &[(&str, &[u8])]) -> tempfile::TempDir {
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
    std::fs::write(
        project.path().join("main.orna"),
        "pub fn serve(): Str = \"served\";\n",
    )
    .expect("write main.orna");
    for (name, bytes) in files {
        let path = project.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture directory");
        }
        std::fs::write(path, bytes).expect("write fixture file");
    }
    git(project.path(), &["add", "."]);
    git(
        project.path(),
        &[
            "commit",
            "--quiet",
            "-m",
            "add application source and fixtures",
        ],
    );
    project
}

fn head_commit(project: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .expect("read the fixture commit");
    assert!(output.status.success(), "rev-parse: {output:?}");
    String::from_utf8(output.stdout)
        .expect("commit id is UTF-8")
        .trim()
        .to_owned()
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn start(project: &Path) -> (RunningServer, String) {
    let port = free_port();
    let mut child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the serving process");
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(&address).is_ok() {
            return (RunningServer(child), address);
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("the serving process did not accept connections");
}

#[test]
fn content_route_serves_exact_bytes() {
    let project = project_with_files(&[("media/note.txt", b"hello world")]);
    let commit = head_commit(project.path());
    let (_server, address) = start(project.path());
    let reply = request(
        &address,
        "GET",
        &format!("/content/{commit}/media/note.txt"),
        "",
    )
    .expect("read the content route");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, b"hello world");
    assert_eq!(reply.header("accept-ranges"), Some("bytes"));
    assert_eq!(reply.header("content-length"), Some("11"));
}

#[test]
fn content_route_answers_a_byte_range_with_206_and_content_range() {
    let project = project_with_files(&[("media/note.txt", b"hello world")]);
    let commit = head_commit(project.path());
    let (_server, address) = start(project.path());
    let reply = request(
        &address,
        "GET",
        &format!("/content/{commit}/media/note.txt"),
        "Range: bytes=0-4\r\n",
    )
    .expect("read the ranged content route");
    assert_eq!(reply.status, 206);
    assert_eq!(reply.body, b"hello");
    assert_eq!(reply.header("content-range"), Some("bytes 0-4/11"));
    assert_eq!(reply.header("content-length"), Some("5"));
}

#[test]
fn content_route_reports_an_unsatisfiable_range_as_416() {
    let project = project_with_files(&[("media/note.txt", b"hello")]);
    let commit = head_commit(project.path());
    let (_server, address) = start(project.path());
    let reply = request(
        &address,
        "GET",
        &format!("/content/{commit}/media/note.txt"),
        "Range: bytes=99-\r\n",
    )
    .expect("read the unsatisfiable range");
    assert_eq!(reply.status, 416);
    assert_eq!(reply.header("content-range"), Some("bytes */5"));
    assert!(reply.body.is_empty());
}

#[test]
fn content_route_head_carries_headers_and_no_body() {
    let project = project_with_files(&[("media/note.txt", b"hello world")]);
    let commit = head_commit(project.path());
    let (_server, address) = start(project.path());
    let reply = request(
        &address,
        "HEAD",
        &format!("/content/{commit}/media/note.txt"),
        "",
    )
    .expect("read the head response");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-length"), Some("11"));
    assert!(reply.body.is_empty(), "HEAD carries no body");
}

#[test]
fn content_route_rejects_an_unresolvable_commit() {
    let project = project_with_files(&[("media/note.txt", b"hello")]);
    let (_server, address) = start(project.path());
    // A bare digest is not a listing commit, so the route must not serve bytes.
    let reply = request(
        &address,
        "GET",
        "/content/0123456789abcdef/media/note.txt",
        "",
    )
    .expect("read the unresolved content route");
    assert_eq!(reply.status, 404);
    assert!(
        reply.header("content-range").is_none(),
        "an unresolved commit carries no range metadata"
    );
}

#[test]
fn content_route_serves_a_stored_document_as_an_attachment() {
    let project = project_with_files(&[("media/page.html", b"<script>alert(1)</script>")]);
    let commit = head_commit(project.path());
    let (_server, address) = start(project.path());
    let reply = request(
        &address,
        "GET",
        &format!("/content/{commit}/media/page.html"),
        "",
    )
    .expect("read the document route");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-disposition"), Some("attachment"));
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(reply.header("content-security-policy"), Some("sandbox"));
}

#[test]
fn content_route_serves_an_allowlisted_image_inline() {
    let png = [0x89_u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let project = project_with_files(&[("media/cover.png", &png)]);
    let commit = head_commit(project.path());
    let (_server, address) = start(project.path());
    let reply = request(
        &address,
        "GET",
        &format!("/content/{commit}/media/cover.png"),
        "",
    )
    .expect("read the image route");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, png);
    assert!(
        reply.header("content-disposition").is_none(),
        "an allowlisted image renders inline"
    );
}
