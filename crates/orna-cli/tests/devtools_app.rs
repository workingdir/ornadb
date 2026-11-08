//! Gate G / DESIGN S5a: the minimal devtools app is Orna source.
//!
//! `serve()` builds a `std.ui` page whose table list comes from the admitted
//! catalogue, so the UI, the REPL and the mount read one production API.
//!
//! The project pins a standard snapshot holding `ui.orna`, because standard
//! modules come from the committed `stdlib/std` gitlink rather than the
//! worktree. The snapshot root imports only modules the snapshot contains.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_orna");
const DEVTOOLS_SOURCE: &str = include_str!("fixtures/devtools/serve.orna");
const STANDARD_ROOT: &str = include_str!("fixtures/devtools/standard-root.orna");
const STANDARD_UI: &str = include_str!("../../../stdlib/std/ui.orna");

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
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn get(address: &str, path: &str) -> std::io::Result<Reply> {
    let mut stream = TcpStream::connect(address)?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(raw.len(), |index| index + 4);
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = String::from_utf8_lossy(raw.get(split..).unwrap_or_default()).into_owned();
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

fn initialize_project(directory: &Path) {
    let output = Command::new(BINARY)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(output.status.success(), "init stderr: {:?}", output.stderr);
}

fn git_output_at(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("Git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

fn configure_git(directory: &Path) {
    git_output_at(directory, &["config", "user.email", "kieran@drewett.dev"]);
    git_output_at(directory, &["config", "user.name", "kierandrewett"]);
    git_output_at(directory, &["config", "commit.gpgsign", "false"]);
}

fn commit_orna(directory: &Path, message: &str) {
    configure_git(directory);
    git_output_at(directory, &["add", "-A"]);
    let output = Command::new(BINARY)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--db",
            directory.to_str().expect("UTF-8 path"),
            "commit",
            "-m",
            message,
        ])
        .output()
        .expect("CLI commit");
    assert!(
        output.status.success(),
        "commit stderr: {:?}",
        output.stderr
    );
}

/// A standard snapshot in its own repository, outside the project tree.
///
/// Its root imports only modules the snapshot contains: a root that imports a
/// module it does not ship cannot be loaded, so the parent would bind no
/// profile and every standard import would stay uncaptured.
fn standard_snapshot() -> tempfile::TempDir {
    let module = tempfile::tempdir().expect("standard snapshot");
    git_output_at(module.path(), &["init", "-q"]);
    std::fs::write(module.path().join("main.orna"), STANDARD_ROOT).expect("standard root");
    std::fs::write(module.path().join("ui.orna"), STANDARD_UI).expect("standard ui module");
    configure_git(module.path());
    git_output_at(module.path(), &["add", "-A"]);
    git_output_at(
        module.path(),
        &["commit", "--quiet", "-m", "standard module snapshot"],
    );
    module
}

/// Creates a project that pins `snapshot`, declares `declarations`, and serves
/// the devtools page.
fn devtools_project_declaring(snapshot: &Path, declarations: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("devtools project");
    initialize_project(directory.path());
    configure_git(directory.path());
    let source = format!("{declarations}\n{DEVTOOLS_SOURCE}");
    std::fs::write(directory.path().join("main.orna"), source).expect("project source");
    // A local clone needs the file protocol; the submodule must be checked out
    // on disk for the standard sources to bind.
    git_output_at(
        directory.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            snapshot.to_str().expect("UTF-8 snapshot path"),
            "stdlib/std",
        ],
    );
    commit_orna(directory.path(), "Declare Note and the devtools page");
    directory
}

/// Creates a project that pins `snapshot`, declares `Note`, and serves the
/// devtools page.
fn devtools_project(snapshot: &Path) -> tempfile::TempDir {
    devtools_project_declaring(snapshot, "pub table Note(id: Int) {\n    body: Str,\n}\n")
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
// Specified: Gate G / DESIGN S5a; the devtools page is served from Orna source.
fn devtools_lists_tables_runs_input_and_refreshes() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());

    let reply = get(&address, "/").expect("read the devtools page");
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8"),
        "a std.ui page is served as HTML, not Inspect text"
    );

    // The table list comes from the admitted catalogue.
    assert!(reply.body.contains("<table"), "the page lists a table");
    assert!(reply.body.contains("Devtools"), "the page renders its text");

    // The page carries an input, a Run control and an explicit refresh.
    assert!(reply.body.contains("<input"), "the page carries an input");
    assert!(
        reply.body.contains("<label for=\"query\">Query</label>"),
        "the input is labelled and reachable"
    );
    assert!(
        reply.body.contains("<button type=\"submit\">"),
        "the page carries a submit control"
    );
    assert!(
        reply.body.contains("Refresh"),
        "the page carries a refresh control"
    );

    // A refresh re-reads the same production API and answers the same page.
    let refreshed = get(&address, "/").expect("refresh the devtools page");
    assert_eq!(refreshed.status, 200);
    assert_eq!(
        refreshed.body, reply.body,
        "a refresh renders the same page"
    );
}

/// The text between the first `open` and the next `close` after it.
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = start + text[start..].find(close)?;
    Some(&text[start..end])
}

/// The `name` column of the served table, in row order. The column is found
/// by its header, so a page with other columns still yields only table names.
fn served_table_names(body: &str) -> Vec<String> {
    let head = between(body, "<thead><tr>", "</tr></thead>").expect("the table has a header");
    let columns = head
        .split("<th>")
        .skip(1)
        .map(|cell| cell.split("</th>").next().unwrap_or_default())
        .collect::<Vec<_>>();
    let name = columns
        .iter()
        .position(|column| *column == "name")
        .expect("the table has a name column");
    let rows = between(body, "<tbody>", "</tbody>").expect("the table has a body");
    rows.split("<tr>")
        .skip(1)
        .map(|row| {
            row.split("<td>")
                .skip(1)
                .nth(name)
                .and_then(|cell| cell.split("</td>").next())
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[test]
// Specified: Gate G / DESIGN S5a and implementation/ACCEPTANCE.md Gate G; the
// served table list is the admitted catalogue, asserted by exact name.
fn devtools_lists_exactly_the_admitted_table_names() {
    let snapshot = standard_snapshot();
    let project = devtools_project_declaring(
        snapshot.path(),
        "pub table Note(id: Int) {\n    body: Str,\n}\n\npub table Task(id: Int) {\n    title: Str,\n}\n",
    );
    let (_server, address) = start(project.path());

    let reply = get(&address, "/").expect("read the devtools page");
    assert_eq!(reply.status, 200);
    let mut names = served_table_names(&reply.body);
    names.sort();
    assert_eq!(
        names,
        ["Note", "Task"],
        "the list is the admitted catalogue, with no extra or missing table\n{}",
        reply.body
    );
}
