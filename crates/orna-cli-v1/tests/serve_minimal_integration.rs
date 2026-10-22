use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[path = "support/format3.rs"]
mod format3;
#[path = "support/orna_cli_run.rs"]
mod cli;

const BINARY: &str = env!("CARGO_BIN_EXE_orna-cli-v1");

struct RunningServer(Child);

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http_get(address: &str, path: &str) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(address)?;
    // Host must name the listener's own authority, port included (RFC 9110);
    // `orna serve` refuses any other Host.
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")?;
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

fn git(project: &std::path::Path, arguments: &[&str]) {
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

#[test]
fn devtools_are_absent_while_local_query_and_serving_remain_available() {
    let (query_project, _, relation_id) =
        format3::format3_repository(include_str!("fixtures/media/empty-media-main.orna"));
    let relation_id = format3::hex(&relation_id);
    let query = cli::run_in(
        query_project.path(),
        &["query", &relation_id, "--format", "json"],
    );
    assert!(
        query.status.success(),
        "local query failed: {}",
        String::from_utf8_lossy(&query.stderr)
    );

    let project = tempfile::tempdir().expect("temporary Orna project");
    let initialized = Command::new(BINARY)
        .arg("init")
        .current_dir(project.path())
        .output()
        .expect("initialize project");
    assert!(initialized.status.success(), "initialize: {initialized:?}");
    std::fs::write(project.path().join("main.orna"), "pub fn main(): Int = 42;\n")
        .expect("write application source");
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

    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start local server");
    let _server = RunningServer(child);
    let address = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut root = None;
    while Instant::now() < deadline {
        if let Ok(response) = http_get(&address, "/") {
            root = Some(response);
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let (status, body) = root.expect("local server becomes available");
    assert_eq!(status, 200);
    assert!(!body.contains("/devtools/tables"));
    let (status, _) = http_get(&address, "/devtools/tables").expect("request removed route");
    assert_eq!(status, 404, "removed devtools route must not be reinstalled");
}
