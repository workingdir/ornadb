//! Gate G / ORNA-APP-021 (ornadb-5il1.392, audit item G-14): a second ordinary
//! application on the same renderer and route stack.
//!
//! The devtools application (`fixtures/devtools/serve.orna`) and this second
//! application (`fixtures/second-app/serve.orna`) are separate projects with
//! their own tables and their own `pub fn serve()`. Both are served by one
//! `orna` binary through one route, `serving_entry_route`
//! (`crates/orna-cli-v1/src/cli_serve_app.rs:286`); the request reaches
//! `ServeApp::respond`, whose only presentation step is
//! `crate::ui_render::render_ui_html` (`crates/orna-cli-v1/src/ui_render.rs:13`,
//! called at `crates/orna-cli-v1/src/cli_serve_app.rs:104`). Nothing in the
//! route or the renderer names an application, so this test drives both and
//! pins the emitted markup to the renderer's own fragments.
//!
//! The projects pin a standard snapshot holding `ui.orna`, because standard
//! modules come from the committed `stdlib/std` gitlink rather than the
//! worktree. Each application's fixture tree carries the snapshot root it
//! pins, so neither application's sources are read by the other's project.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const BINARY: &str = env!("CARGO_BIN_EXE_orna");
/// The devtools application's page (Gate G / DESIGN S5a).
const DEVTOOLS_SOURCE: &str = include_str!("fixtures/devtools/serve.orna");
/// The second ordinary application: its own tables and its own `serve()`.
const SECOND_APP_SOURCE: &str = include_str!("fixtures/second-app/serve.orna");
const STANDARD_ROOT: &str = include_str!("fixtures/second-app/standard-root.orna");
const STANDARD_UI: &str = include_str!("../../../stdlib/std/ui.orna");

/// Every fragment `render_ui_html` emits around the devtools application's
/// table node, in the order its `Cols` children declare them.
const DEVTOOLS_PREFIX: &str = concat!(
    "<div><p>Devtools</p>",
    "<label for=\"query\">Query</label><input id=\"query\" name=\"query\" type=\"text\">"
);
const DEVTOOLS_SUFFIX: &str = "<button type=\"submit\">Refresh</button></div>";

/// The same for the second application. Its `Cols` children are a text, a
/// field, the table, a details group, an input and a button, so the fragments
/// around the table exercise renderer branches the devtools page never emits.
const SECOND_PREFIX: &str = concat!(
    "<div><p>Ledger</p>",
    "<div><span class=\"label\">mode</span> <span class=\"value\">serve</span></div>"
);
const SECOND_SUFFIX: &str = concat!(
    "<dl>",
    "<div><span class=\"label\">accounts</span> <span class=\"value\">two</span></div>",
    "<pre><code>pub fn serve(): UI</code></pre>",
    "</dl>",
    "<label for=\"filter\">Filter</label><input id=\"filter\" name=\"filter\" type=\"text\">",
    "<button type=\"submit\">Reload</button>",
    "</div>"
);

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

/// What one served project answered on the route the devtools page uses.
struct Served {
    page: Reply,
    head: Reply,
    method_refusal: Reply,
}

fn request(address: &str, method: &str, path: &str) -> std::io::Result<Reply> {
    let mut stream = TcpStream::connect(address)?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\n"
    )?;
    // ORNA-APP-012: a write must carry the request's own origin, so a probe of
    // the route's method branch sends one instead of being refused at the
    // boundary.
    if !matches!(method, "GET" | "HEAD") {
        write!(stream, "Origin: http://{address}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
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

/// Creates a project that pins `snapshot` and commits `source` as `main.orna`.
fn project(snapshot: &Path, source: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("application project");
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
    commit_orna(directory.path(), "Declare the application");
    directory
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

/// Serves `project` and records what the shared route answers: the page, its
/// HEAD answer, and its refusal of a non-GET method.
fn serve_project(project: &Path) -> Served {
    let (_server, address) = start(project);
    Served {
        page: request(&address, "GET", "/").expect("read the served page"),
        head: request(&address, "HEAD", "/").expect("read the route's HEAD answer"),
        method_refusal: request(&address, "POST", "/").expect("read the route's refusal"),
    }
}

/// The `<table>`…`</table>` fragment of a served page: the only part of the
/// page whose content comes from the clone's admitted catalogue.
fn table_fragment(page: &str) -> &str {
    let start = page.find("<table>").expect("the page renders a table node");
    let end = page[start..]
        .find("</table>")
        .expect("the table node is closed")
        + start
        + "</table>".len();
    &page[start..end]
}

/// The markup of a fragment with every text node removed, so two applications'
/// pages compare by the tags the renderer emitted instead of by their content.
fn tag_skeleton(fragment: &str) -> String {
    let mut skeleton = String::new();
    let mut in_tag = false;
    for character in fragment.chars() {
        match character {
            '<' => {
                in_tag = true;
                skeleton.push(character);
            }
            '>' => {
                in_tag = false;
                skeleton.push(character);
            }
            other if in_tag => skeleton.push(other),
            _ => {}
        }
    }
    skeleton
}

/// Asserts `page` is exactly the renderer's fragments around the table node,
/// and that the table node lists `tables`.
fn assert_page_is_renderer_output(page: &str, prefix: &str, suffix: &str, tables: &[&str]) {
    let after_prefix = page
        .strip_prefix(prefix)
        .expect("the page starts with the renderer's markup for its first nodes");
    let start = after_prefix
        .find("<table>")
        .expect("the table node follows the first nodes");
    assert_eq!(
        start,
        0,
        "nothing separates the first nodes from the table node: {}",
        &after_prefix[..start]
    );
    let table = table_fragment(after_prefix);
    let after_table = &after_prefix[table.len()..];
    assert_eq!(
        after_table, suffix,
        "the page ends with the renderer's markup for its remaining nodes"
    );
    assert!(
        table.starts_with("<table>") && table.ends_with("</tbody></table>"),
        "the table node is the renderer's table: {table}"
    );
    assert!(
        table.contains("<thead><tr><th>"),
        "a catalogue record has scalar fields, so the table carries a header: {table}"
    );
    for table_name in tables {
        assert!(
            table.contains(&format!("<td>{table_name}</td>")),
            "the table lists {table_name}: {table}"
        );
    }
    assert!(
        !page.contains("std.ui."),
        "the page is the rendered node tree, not the Inspect fallback: {page}"
    );
}

#[test]
// Specified: Gate G / DESIGN S5a; ORNA-APP-021 replaceability.
fn second_ordinary_app_uses_the_same_renderer_and_routes() {
    let snapshot = standard_snapshot();

    // The devtools application, through the shared route.
    let devtools = project(
        snapshot.path(),
        &format!("pub table Note(id: Int) {{\n    body: Str,\n}}\n\n{DEVTOOLS_SOURCE}"),
    );
    let devtools = serve_project(devtools.path());

    // The second application: its own tables and its own `serve()`.
    let second = project(snapshot.path(), SECOND_APP_SOURCE);
    let second = serve_project(second.path());

    // One route served both pages: `GET /` from the selected entry.
    for served in [&devtools, &second] {
        assert_eq!(served.page.status, 200, "`GET /` is served");
        assert_eq!(
            served.page.header("content-type"),
            Some("text/html; charset=utf-8"),
            "a std.ui page is served as HTML, not Inspect text"
        );
        assert_eq!(
            served.page.header("x-content-type-options"),
            Some("nosniff")
        );
    }

    // Both pages are exactly the fragments `render_ui_html` emits for their
    // declared node trees, and each lists only its own admitted tables.
    assert_page_is_renderer_output(
        &devtools.page.body,
        DEVTOOLS_PREFIX,
        DEVTOOLS_SUFFIX,
        &["Note"],
    );
    assert_page_is_renderer_output(
        &second.page.body,
        SECOND_PREFIX,
        SECOND_SUFFIX,
        &["Account", "Entry"],
    );

    // Neither page carries the other application's text, so no route or
    // renderer branch answers one application's page for the other.
    assert!(
        !second.page.body.contains("Devtools"),
        "{}",
        second.page.body
    );
    assert!(
        !devtools.page.body.contains("Ledger"),
        "{}",
        devtools.page.body
    );

    // The same table node renders the same way for both applications: one
    // header row from the catalogue record's scalar fields.
    let header = |page: &str| {
        let table = table_fragment(page);
        let end = table.find("</tr>").expect("the table has a header row");
        tag_skeleton(&table[..end + "</tr>".len()])
    };
    assert_eq!(
        header(&devtools.page.body),
        header(&second.page.body),
        "one table renderer produced both headers"
    );

    // The route is one code path: the same HEAD answer and the same refusal
    // for a non-GET method, for both applications.
    assert_eq!(devtools.head.status, 200, "`HEAD /` answers as `GET /`");
    for served in [&devtools, &second] {
        assert!(
            served.head.body.is_empty(),
            "RFC 9110 9.3.2: HEAD sends no content"
        );
        assert_eq!(
            served.head.header("content-type"),
            Some("text/html; charset=utf-8")
        );
    }
    assert_eq!(
        devtools.head.header("x-content-type-options"),
        second.head.header("x-content-type-options")
    );
    assert_eq!(devtools.method_refusal.status, 405);
    assert_eq!(
        devtools.method_refusal.header("content-type"),
        Some("application/json")
    );
    assert_eq!(
        devtools.method_refusal.body, second.method_refusal.body,
        "the route refuses a method identically for both applications"
    );
    assert_eq!(second.method_refusal.status, devtools.method_refusal.status);
    assert_eq!(
        devtools.method_refusal.header("x-content-type-options"),
        second.method_refusal.header("x-content-type-options")
    );
}
