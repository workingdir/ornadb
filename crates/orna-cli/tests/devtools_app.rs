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
const NOTES_SOURCE: &str = r#"use std.ui.{Button, Cols, Table, Text};

pub fn serve(): UI = Cols([
    Text("Notes"),
    Table(Note | filter(note => true)),
    Button("Refresh", std.ui.action("notes.refresh", as: Text))
]);
"#;
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
    Ok(parse_reply(&raw))
}

/// Posts one form to `path` the way a browser does, with `origin` as the
/// request's Origin header when given.
fn post_form(address: &str, path: &str, origin: Option<&str>, body: &str) -> Reply {
    let origin = origin
        .map(|origin| format!("Origin: {origin}\r\n"))
        .unwrap_or_default();
    post_request(
        address,
        path,
        &format!("{origin}Content-Type: application/x-www-form-urlencoded\r\n"),
        body,
    )
}

/// Sends one POST with the given extra header lines (each ending in CRLF).
fn post_request(address: &str, path: &str, headers: &str, body: &str) -> Reply {
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(address).expect("connect to the serving process");
    stream
        .write_all(request.as_bytes())
        .expect("send the action request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read the action reply");
    parse_reply(&raw)
}

fn parse_reply(raw: &[u8]) -> Reply {
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
    Reply {
        status,
        headers,
        body,
    }
}

/// The Run control evaluates its form field as one console input: `1 + 1` is
/// served as its value, `2 : Int`, through the real `orna serve` process.
#[test]
// Specified: Gate G / DESIGN S5a; the served page's Run control evaluates through the production API.
fn run_action_evaluates_the_form_input_over_http() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let reply = post_form(
        &address,
        "/action/devtools.run",
        Some(&origin),
        "query=1+%2B+1",
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        reply.body.contains("<pre><code>2 : Int</code></pre>"),
        "the input's value is served: {}",
        reply.body
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; a failing console input is answered with its diagnostic code.
fn run_action_renders_an_input_error_as_its_code() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let reply = post_form(
        &address,
        "/action/devtools.run",
        Some(&origin),
        "query=use%0A",
    );
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(
        reply
            .body
            .contains("<p class=\"diagnostic\">ORNA-EVAL-PARSE</p>"),
        "the diagnostic is rendered with its stable code: {}",
        reply.body
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; the action route refuses foreign and originless writes and serves only POST.
fn run_action_refuses_cross_origin_and_serves_only_post() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());

    let foreign = post_form(
        &address,
        "/action/devtools.run",
        Some("http://evil.example"),
        "query=1+%2B+1",
    );
    assert_eq!(foreign.status, 403, "{}", foreign.body);
    assert_eq!(foreign.body, r#"{"error":"origin_denied"}"#);

    let originless = post_form(&address, "/action/devtools.run", None, "query=1+%2B+1");
    assert_eq!(originless.status, 403, "{}", originless.body);

    let read = get(&address, "/action/devtools.run").expect("read the action route");
    assert_eq!(read.status, 405, "an action is not a read: {}", read.body);
}

#[test]
// Specified: Gate G / DESIGN S5a; a served string value is escaped, not interpreted as markup.
fn run_action_escapes_the_rendered_value() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let reply = post_form(
        &address,
        "/action/devtools.run",
        Some(&origin),
        "query=%22%3Cb%3E%22",
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(
        reply.body.contains("&lt;b&gt;"),
        "the value's markup is escaped: {}",
        reply.body
    );
    assert!(
        !reply.body.contains("<b>"),
        "no raw markup reaches the page: {}",
        reply.body
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; an action the page does not offer is not found.
fn run_action_answers_an_unoffered_action_with_not_found() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let reply = post_form(
        &address,
        "/action/devtools.missing",
        Some(&origin),
        "query=1",
    );
    assert_eq!(reply.status, 404, "{}", reply.body);
    assert!(
        reply.body.contains(r#""error":"action.not_found""#),
        "{}",
        reply.body
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; an input over the console limit is refused before evaluation.
fn run_action_refuses_an_oversize_input() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let body = format!("query={}", "1".repeat(16 * 1024 + 1));
    let reply = post_form(&address, "/action/devtools.run", Some(&origin), &body);
    assert_eq!(reply.status, 413, "{}", reply.body);
    assert!(
        reply.body.contains("action.input_too_large"),
        "{}",
        reply.body
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; a malformed or non-UTF-8 form value is an invalid form.
fn run_action_refuses_a_malformed_form_value() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    for body in ["query=%zz", "query=%4", "query=%ff"] {
        let reply = post_form(&address, "/action/devtools.run", Some(&origin), body);
        assert_eq!(reply.status, 400, "{body}: {}", reply.body);
        assert!(
            reply.body.contains("action.invalid_form"),
            "{body}: {}",
            reply.body
        );
    }
}

#[test]
// Specified: Gate G / DESIGN S5a; a request naming a foreign Host is refused even with a matching Origin.
fn run_action_refuses_a_foreign_host() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");
    let body = "query=1+%2B+1";

    let request = format!(
        "POST /action/devtools.run HTTP/1.1\r\nHost: evil.example\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(&address).expect("connect to the serving process");
    stream
        .write_all(request.as_bytes())
        .expect("send the action request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read the action reply");
    let reply = parse_reply(&raw);
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert_eq!(reply.body, r#"{"error":"host_denied"}"#);
}

#[test]
// Specified: Gate G / DESIGN S5a; an effectful input is refused by the served console, not evaluated, and leaves the served page unchanged.
fn run_action_refuses_an_effectful_input() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let before = get(&address, "/").expect("read the page before the input");
    assert_eq!(before.status, 200, "{}", before.body);

    let reply = post_form(
        &address,
        "/action/devtools.run",
        Some(&origin),
        "query=main.Note.insert%28%7B+id%3A+1%2C+body%3A+%22x%22+%7D%29",
    );
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(
        reply.body.contains("ORNA-REPL-EFFECT"),
        "the served console refuses the effect: {}",
        reply.body
    );

    let after = get(&address, "/").expect("read the page after the input");
    assert_eq!(after.status, 200, "{}", after.body);
    assert_eq!(
        after.body, before.body,
        "a refused effect leaves the served tables unchanged"
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; an action body over its route limit is refused from the declared length, before the body is read.
fn run_action_refuses_a_body_over_its_limit_before_reading_it() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    // Only the head is sent: a refusal that waited for the body would time out.
    let declared = 60 * 1024;
    let head = format!(
        "POST /action/devtools.run HTTP/1.1\r\nHost: {address}\r\nOrigin: {origin}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
    );
    let mut stream = TcpStream::connect(&address).expect("connect to the serving process");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    stream
        .write_all(head.as_bytes())
        .expect("send the action head");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("the refusal arrives before any body");
    let reply = parse_reply(&raw);
    assert_eq!(reply.status, 413, "{}", reply.body);
    assert_eq!(reply.body, r#"{"error":"payload_too_large"}"#);
}

/// Declarations for the long Run. Each `tree(6, i)` term is 64 `spin(40, tag)`
/// leaves whose tags are unique to that term, so the activation memo cannot
/// collapse the work across terms: every term costs real evaluation. One term
/// is about 1.8 s in a debug build; the step budget (`DEFAULT_STEPS`) admits
/// eight terms and refuses sixteen, so the Run below uses four.
const SPIN_DECLARATION: &str = "pub fn spin(n: Int, tag: Int): Int = if n == 0 { 0 } else { 1 + spin(n - 1, tag) };\npub fn tree(depth: Int, tag: Int): Int = if depth == 0 { spin(40, tag) } else { tree(depth - 1, tag * 2) + tree(depth - 1, tag * 2 + 1) };\n";

/// A Run body of `terms` distinct `tree(6, i)` terms.
fn spin_run_body(terms: usize) -> String {
    let terms: Vec<String> = (1..=terms)
        .map(|index| format!("main.tree(6, {index})"))
        .collect();
    format!("query=[{}, 0]", terms.join(", "))
}

#[test]
// Specified: Gate G / DESIGN S5a; the page answers while a Run is still evaluating.
fn get_root_answers_while_a_long_run_evaluates() {
    let snapshot = standard_snapshot();
    let project = devtools_project_declaring(snapshot.path(), SPIN_DECLARATION);
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    // Calibration: one term sets the unit of work on this machine.
    let probe_started = Instant::now();
    let probe = post_form(
        &address,
        "/action/devtools.run",
        Some(&origin),
        &spin_run_body(1),
    );
    let unit = probe_started.elapsed();
    assert_eq!(probe.status, 200, "{}", probe.body);

    let run_started = Instant::now();
    let run = thread::spawn({
        let address = address.clone();
        let origin = origin.clone();
        move || {
            let reply = post_form(
                &address,
                "/action/devtools.run",
                Some(&origin),
                &spin_run_body(4),
            );
            (reply, Instant::now())
        }
    });

    // Latencies of GETs that complete while the Run has not yet finished.
    let mut in_flight = Vec::new();
    while !run.is_finished() {
        let began = Instant::now();
        let page = get(&address, "/").expect("read the page during the run");
        let latency = began.elapsed();
        assert_eq!(page.status, 200, "{}", page.body);
        if !run.is_finished() {
            in_flight.push(latency);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let (reply, finished) = run.join().expect("the Run thread");
    let run_elapsed = finished.duration_since(run_started);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.body.contains("Array"), "{}", reply.body);

    assert!(
        run_elapsed >= unit * 3,
        "the Run ({run_elapsed:?}) must last at least three probe terms ({unit:?})"
    );
    assert!(
        !in_flight.is_empty(),
        "the page must answer while the Run is still evaluating"
    );
    let slowest = in_flight.iter().max().expect("an in-flight GET");
    assert!(
        *slowest < run_elapsed / 4,
        "an in-flight GET took {slowest:?} against a Run of {run_elapsed:?}"
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; an action identifier outside the valid format is not found, never routed.
fn run_action_answers_a_malformed_action_id_with_not_found() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    for path in [
        "/action/bad*id".to_owned(),
        format!("/action/{}", "a".repeat(129)),
    ] {
        let reply = post_form(&address, &path, Some(&origin), "query=1");
        assert_eq!(reply.status, 404, "{path}: {}", reply.body);
        assert!(
            reply.body.contains(r#""error":"action.not_found""#),
            "{path}: {}",
            reply.body
        );
    }
}

#[test]
// Specified: Gate G / DESIGN S5a; text that spells a formaction is not an offered action.
fn run_action_refuses_an_action_only_spelled_in_page_text() {
    let snapshot = standard_snapshot();
    let source = concat!(
        "use std.ui.{Cols, Text};\n",
        "\n",
        "pub fn serve(): UI = Cols([\n",
        "    Text(\"formaction=\\\"/action/devtools.spoof\\\"\")\n",
        "]);\n",
    );
    let project = project_with_source(snapshot.path(), source);
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let page = get(&address, "/").expect("read the page");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        !page.body.contains("<form"),
        "text alone offers no form: {}",
        page.body
    );

    let reply = post_form(&address, "/action/devtools.spoof", Some(&origin), "query=1");
    assert_eq!(reply.status, 404, "{}", reply.body);
    assert!(
        reply.body.contains(r#""error":"action.not_found""#),
        "{}",
        reply.body
    );
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
    project_with_source(snapshot, &format!("{declarations}\n{DEVTOOLS_SOURCE}"))
}

/// Creates a project that pins `snapshot` and whose `main.orna` is `source`.
fn project_with_source(snapshot: &Path, source: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("devtools project");
    initialize_project(directory.path());
    configure_git(directory.path());
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
    commit_orna(directory.path(), "Declare the project source");
    directory
}

/// Creates a project that pins `snapshot`, declares `Note`, and serves the
/// devtools page.
fn devtools_project(snapshot: &Path) -> tempfile::TempDir {
    devtools_project_declaring(snapshot, "pub table Note(id: Int) {\n    body: Str,\n}\n")
}

/// Creates a project that pins `snapshot`, declares `Note`, and serves its
/// rows as a table.
fn notes_project(snapshot: &Path) -> tempfile::TempDir {
    project_with_source(
        snapshot,
        &format!("pub table Note(id: Int) {{\n    body: Str,\n}}\n\n{NOTES_SOURCE}"),
    )
}

/// Inserts through the product REPL of the project at `project`, as a
/// developer does, and asserts the REPL accepted the input.
fn repl_insert(project: &Path, input: &str) {
    let mut child = Command::new(BINARY)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", project.to_str().expect("UTF-8 path"), "repl"])
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the REPL");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    let output = child.wait_with_output().expect("the REPL exits");
    let transcript = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{transcript}");
    assert!(!transcript.contains("error["), "{transcript}");
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
        reply
            .body
            .contains("<button type=\"submit\" formaction=\"/action/devtools.run\">Run</button>"),
        "the Run control posts its input to the console action"
    );
    assert!(
        reply.body.contains("<form method=\"post\">"),
        "the page is one POST form so its controls submit"
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

#[test]
// Specified: Gate G / DESIGN S5a; a Run form posts one field and the route refuses other content types.
fn run_action_requires_a_form_body() {
    let snapshot = standard_snapshot();
    let project = devtools_project(snapshot.path());
    let (_server, address) = start(project.path());
    let origin = format!("http://{address}");

    let reply = post_request(
        &address,
        "/action/devtools.run",
        &format!("Origin: {origin}\r\nContent-Type: application/json\r\n"),
        "{}",
    );
    assert_eq!(reply.status, 415, "{}", reply.body);
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

#[test]
// Specified: Gate G3 / implementation/ACCEPTANCE.md Gate G; a page request after
// a REPL insert shows the new row, and the pre-change page did not. The served
// process holds the runtime owner lock (ORNA-APP-011), so the REPL write runs
// only once the served process has stopped; the post-change request is served
// by a fresh process that overlays the working-directory rows on committed HEAD.
fn explicit_refresh_shows_a_row_inserted_through_the_repl() {
    let snapshot = standard_snapshot();
    let project = notes_project(snapshot.path());
    let (server, address) = start(project.path());

    let before = get(&address, "/").expect("read the page before the change");
    assert_eq!(before.status, 200);
    assert!(
        before.body.contains("<table"),
        "the page lists rows\n{}",
        before.body
    );
    assert!(
        !before.body.contains("<td>first</td>"),
        "the pre-change page holds no row\n{}",
        before.body
    );
    drop(server);

    repl_insert(
        project.path(),
        r#"main.Note.insert({ id: 1, body: "first" })"#,
    );

    let (_server, address) = start(project.path());
    let refreshed = get(&address, "/").expect("request the page after the change");
    assert_eq!(refreshed.status, 200);
    assert!(
        refreshed.body.contains("<td>first</td>"),
        "the refreshed page shows the inserted row\n{}",
        refreshed.body
    );
    assert_ne!(
        refreshed.body, before.body,
        "the refresh reflects the change"
    );
}
