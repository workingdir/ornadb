//! Gate G / std.ui Image (Orna-1.1.0, ORNA-APP-013, ORNA-APP-014): a served
//! `Image(blob, alt)` page links the committed blob through
//! `/media/<commit>/<descriptor>` at HEAD, and that route returns the stored
//! bytes unchanged.
//!
//! The project pins the standard snapshot holding `ui.orna`, because standard
//! modules come from the committed `stdlib/std` gitlink. The blob row is stored
//! through the REPL and published, so the page reads it from the committed
//! store exactly as a served application does.

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_orna");
const STANDARD_ROOT: &str = include_str!("fixtures/second-app/standard-root.orna");
const STANDARD_UI: &str = include_str!("../../../stdlib/std/ui.orna");
const PIXEL: &[u8] = include_bytes!("fixtures/media/pixel.png");

const IMAGE_SOURCE: &str = "use std.ui.{Image};\n\npub table media(id: Str) { content: Blob, }\n\npub fn store(id: Str, content: Blob) = media.insert({ id: id, content: content });\n\npub fn serve(): UI = Image((media | one()).content, \"Pixel\");\n";

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

/// Sends one GET and returns the status, headers and the exact body bytes.
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
        body: raw.get(split..).unwrap_or_default().to_vec(),
    })
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    let port = listener.local_addr().expect("local address").port();
    drop(listener);
    port
}

/// Starts `orna serve` in `root` and waits until its listener answers.
fn start(root: &Path) -> (RunningServer, String) {
    let port = free_port();
    let child = Command::new(BINARY)
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
        if TcpStream::connect(&address).is_ok() {
            return (server, address);
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("local server becomes available on {address}");
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(BINARY)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

fn orna_repl(root: &Path, input: &str) -> Output {
    let mut child = Command::new(BINARY)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", root.to_str().expect("UTF-8 path"), "repl"])
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
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the orna REPL exits")
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
fn standard_snapshot() -> TempDir {
    let module = tempfile::tempdir().expect("standard snapshot");
    git_output_at(module.path(), &["init", "-q"]);
    fs::write(module.path().join("main.orna"), STANDARD_ROOT).expect("standard root");
    fs::write(module.path().join("ui.orna"), STANDARD_UI).expect("standard ui module");
    configure_git(module.path());
    git_output_at(module.path(), &["add", "-A"]);
    git_output_at(
        module.path(),
        &["commit", "--quiet", "-m", "standard module snapshot"],
    );
    module
}

/// Creates a project that pins `snapshot` and commits `source` as `main.orna`.
fn project(snapshot: &Path, source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("application project");
    let init = Command::new(BINARY)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init stderr: {:?}", init.stderr);
    configure_git(directory.path());
    fs::write(directory.path().join("main.orna"), source).expect("project source");
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
    commit_orna(directory.path(), "Declare the image application");
    directory
}

/// The value of the first `src="..."` attribute in `html`.
fn first_src(html: &str) -> &str {
    let start = html
        .find("src=\"")
        .expect("the page carries an image source")
        + "src=\"".len();
    let end = html[start..].find('"').expect("the image source is closed") + start;
    &html[start..end]
}

/// Captures the pixel fixture into `root` and stores it as `id` through the REPL.
/// The next publish commits the row.
fn store_pixel(root: &Path, id: &str) {
    fs::write(root.join("pixel.png"), PIXEL).expect("place the captured file");
    let root_text = root.to_string_lossy();
    let stored = orna_repl(
        root,
        &format!(
            "let raw = sys.blob.capture_file({root_text:?}, \"pixel.png\", 65536);\nlet image = sys.blob.annotate(raw, \"image/png\");\nmain.store(\"{id}\", image).id"
        ),
    );
    assert_repl_accepts(&stored, "REPL blob insert");
}

/// Stores one `notes` row through the REPL. The next publish commits the row.
fn store_note(root: &Path, id: &str, title: &str) {
    let stored = orna_repl(root, &format!("main.note(\"{id}\", \"{title}\").id"));
    assert_repl_accepts(&stored, "REPL note insert");
}

/// The REPL exits cleanly and reports no error for the input it was given.
fn assert_repl_accepts(output: &Output, step: &str) {
    let transcript = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0), "{step}\n{transcript}");
    assert!(!transcript.contains("error["), "{step}\n{transcript}");
}

#[test]
// Specified: Orna-1.1.0 std.ui Image links a committed blob through the explicit
// media route and never inlines its bytes; the route answers the stored bytes.
fn served_image_links_committed_blob_bytes_through_the_media_route() {
    let snapshot = standard_snapshot();
    let directory = project(snapshot.path(), IMAGE_SOURCE);
    let root = directory.path();
    store_pixel(root, "one");
    let published = orna(root, &["publish", "--message", "Add the image"]);
    assert_eq!(
        published.status.code(),
        Some(0),
        "orna publish\n{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let commit = git_output_at(root, &["rev-parse", "HEAD"]);
    let (_server, address) = start(root);
    let page = get(&address, "/").expect("GET /");
    assert_eq!(page.status, 200, "page body: {:?}", page.body);
    let html = String::from_utf8(page.body).expect("UTF-8 page");
    assert!(
        html.starts_with("<img src=\"/media/"),
        "the page is the rendered Image node: {html}"
    );
    assert!(html.ends_with("alt=\"Pixel\">"), "{html}");

    let href = first_src(&html).to_owned();
    assert!(
        href.starts_with(&format!("/media/{commit}/")),
        "the link names HEAD: {href}"
    );

    let media = get(&address, &href).expect("GET the media route");
    assert_eq!(media.status, 200, "media body: {:?}", media.body);
    assert_eq!(
        media.body, PIXEL,
        "the media route answers the committed bytes unchanged"
    );
    assert!(
        media
            .header("content-type")
            .is_some_and(|value| value.starts_with("image/png")),
        "the media route answers the annotated media type: {:?}",
        media.header("content-type")
    );
}

const TWO_TABLE_SOURCE: &str = "use std.ui.{Image, Stack, Text};\n\npub table media(id: Str) { content: Blob, }\n\npub table notes(id: Str) { title: Str, }\n\npub fn store(id: Str, content: Blob) = media.insert({ id: id, content: content });\n\npub fn note(id: Str, title: Str) = notes.insert({ id: id, title: title });\n\npub fn serve(): UI = Stack([Text((notes | one()).title), Image((media | one()).content, \"Pixel\")]);\n";

#[test]
// Specified: Orna-1.1.0 ORNA-APP-013: one served evaluation reads a Blob table and a
// plain table together. The plain row is served by value and the Blob by its
// committed descriptor, whose bytes the media route answers unchanged.
fn served_page_reads_a_blob_table_and_a_plain_table_in_one_evaluation() {
    let snapshot = standard_snapshot();
    let directory = project(snapshot.path(), TWO_TABLE_SOURCE);
    let root = directory.path();
    store_pixel(root, "one");
    store_note(root, "one", "Pixel caption");
    let published = orna(
        root,
        &["publish", "--message", "Add the image and its caption"],
    );
    assert_eq!(
        published.status.code(),
        Some(0),
        "orna publish\n{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let commit = git_output_at(root, &["rev-parse", "HEAD"]);
    let (_server, address) = start(root);
    let page = get(&address, "/").expect("GET /");
    assert_eq!(page.status, 200, "page body: {:?}", page.body);
    let html = String::from_utf8(page.body).expect("UTF-8 page");
    assert!(
        html.contains("Pixel caption"),
        "the plain table row is served by value: {html}"
    );
    let href = first_src(&html).to_owned();
    assert!(
        href.starts_with(&format!("/media/{commit}/")),
        "the Blob names HEAD: {href}"
    );
    let media = get(&address, &href).expect("GET the media route");
    assert_eq!(media.status, 200, "media body: {:?}", media.body);
    assert_eq!(
        media.body, PIXEL,
        "the media route answers the committed bytes unchanged"
    );
}
