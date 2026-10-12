use std::{
    net::TcpListener,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_orna-cli-v1");

struct RunningServer(Child);

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct HttpResponse {
    status: u16,
    headers: String,
    body: String,
}

fn run(command: &mut Command, description: &str) -> Output {
    let output = command.output().unwrap_or_else(|error| {
        panic!("{description} could not start: {error}");
    });
    assert!(
        output.status.success(),
        "{description} exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn git(project: &std::path::Path, arguments: &[&str]) {
    let mut command = Command::new("git");
    command.args(arguments).current_dir(project);
    let _ = run(&mut command, "Git fixture setup");
}

fn curl(url: &str) -> Result<HttpResponse, String> {
    let output = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--include",
            "--noproxy",
            "*",
            "--max-time",
            "5",
        ])
        .arg(url)
        .output()
        .map_err(|error| format!("curl: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let response = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .or_else(|| response.split_once("\n\n"))
        .ok_or_else(|| "curl response had no header boundary".to_owned())?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "curl response had no HTTP status".to_owned())?;
    Ok(HttpResponse {
        status,
        headers: headers.to_owned(),
        body: body.to_owned(),
    })
}

fn response_header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve a local test port")
        .local_addr()
        .expect("read test port")
        .port()
}

fn wait_until_serving(server: &mut RunningServer, base_url: &str) {
    let started = Instant::now();
    loop {
        if let Some(status) = server.0.try_wait().expect("check serving process") {
            panic!("orna serve exited before accepting HTTP requests: {status}");
        }
        if curl(&format!("{base_url}/")).is_ok() {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "orna serve did not start listening within 15 seconds"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn orna_serve_lists_committed_source_tables_and_refreshes_after_commit() {
    let project = tempfile::tempdir().expect("temporary Orna database");
    let mut initialize = Command::new(BINARY);
    initialize.arg("init").current_dir(project.path());
    let _ = run(&mut initialize, "initialize source-backed table project");

    std::fs::write(
        project.path().join("main.orna"),
        "pub fn main(): Int = 42;\n",
    )
    .expect("write root module");
    std::fs::write(
        project.path().join("contacts.orna"),
        "pub table Contact(id: Str) { name: Str, }\n",
    )
    .expect("write ordinary table declaration");
    git(project.path(), &["add", "main.orna", "contacts.orna"]);
    git(
        project.path(),
        &[
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "add source table",
        ],
    );

    let port = free_port();
    let child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start orna serve");
    let mut server = RunningServer(child);
    let base_url = format!("http://127.0.0.1:{port}");
    wait_until_serving(&mut server, &base_url);

    let root = curl(&format!("{base_url}/")).expect("fetch root listing");
    assert_eq!(root.status, 200);
    assert!(root.body.contains("/devtools/tables"));

    let initial =
        curl(&format!("{base_url}/devtools/tables")).expect("fetch committed source table list");
    assert_eq!(initial.status, 200);
    assert_eq!(
        response_header(&initial.headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        response_header(&initial.headers, "cache-control"),
        Some("no-store")
    );
    assert!(initial.body.contains("Database tables"));
    assert!(initial.body.contains("Contact"));
    assert!(initial.body.contains("name"));
    assert!(initial.body.contains("Refresh the committed table list"));
    assert!(initial.body.contains("/blob/"));
    assert!(!initial.body.contains("<script"));

    std::fs::write(
        project.path().join("inventory.orna"),
        "pub table Item(id: Str) { label: Str, }\n",
    )
    .expect("write an uncommitted ordinary table declaration");
    let uncommitted =
        curl(&format!("{base_url}/devtools/tables")).expect("refresh before source commit");
    assert_eq!(uncommitted.status, 200);
    assert!(!uncommitted.body.contains("Item"));

    git(project.path(), &["add", "inventory.orna"]);
    git(
        project.path(),
        &[
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "add another source table",
        ],
    );
    let refreshed =
        curl(&format!("{base_url}/devtools/tables")).expect("refresh committed source table list");
    assert_eq!(refreshed.status, 200);
    assert!(refreshed.body.contains("Contact"));
    assert!(refreshed.body.contains("Item"));
    assert!(refreshed.body.contains("label"));
    assert_ne!(initial.body, refreshed.body);
    assert!(server
        .0
        .try_wait()
        .expect("probe orna serve after source refresh")
        .is_none());
}
