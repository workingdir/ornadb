//! ORNA-APP-013 / ORNA-APP-014 (Gate G): `orna serve` answers
//! `/media/<commit>/<descriptor>` for a Blob descriptor that a committed HEAD
//! row stores. The served bytes are the committed payload, the descriptor is
//! the validator, `Range` selects verified slices, `HEAD` carries headers
//! without a body, and only GET and HEAD read the payload.

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

const SERVE_SOURCE: &str = "pub table Object(id: Str) {\n    payload: Blob,\n    content: Str,\n}\n\npub fn serve() = Object | count();\n";
const TONE_FIXTURE: &str = "tests/fixtures/media/tone.wav";
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

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

fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut child = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
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

fn git_stdout(root: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git(root: &Path, arguments: &[&str]) {
    git_stdout(root, arguments);
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

/// Commits `Object`, stores one annotated WAV payload through the REPL, and
/// publishes it to HEAD. Returns the project root and the table's relation id.
fn committed_blob_project(directory: &TempDir) -> (PathBuf, String) {
    let root = directory.path().join("library");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), SERVE_SOURCE).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare Object"]);
    assert_success(&committed, "orna commit");
    let relation = transcript(&committed)
        .lines()
        .find_map(|line| {
            line.split_once("as relation ")
                .map(|(_, id)| id.trim().to_owned())
        })
        .expect("commit reports the admitted relation");

    // The blob source must lie inside the repository worktree (the capture root).
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(TONE_FIXTURE),
        root.join("tone.wav"),
    )
    .expect("copy the WAV fixture into the worktree");
    let input = format!(
        "main.Object.insert({{ id: \"a\", payload: sys.blob.annotate(sys.blob.capture_file(\"{}\", \"tone.wav\", 65536), \"audio/wav\"), content: \"x\" }})",
        root.to_str().expect("UTF-8 test path")
    );
    let inserted = orna_repl(&root, &input);
    assert_success(&inserted, "REPL blob insert");
    assert!(
        !transcript(&inserted).contains("error["),
        "the REPL admits the blob row\n{}",
        transcript(&inserted)
    );

    let published = orna(&root, &["publish", "--message", "Add object"]);
    assert_success(&published, "orna publish");
    (root, relation)
}

/// The descriptor OID the committed row stores, read through the committed-row
/// listing rather than recomputed by the test.
fn committed_descriptor(root: &Path, relation: &str) -> String {
    let listing = orna(root, &["query", relation, "--format", "json"]);
    assert_success(&listing, "orna query");
    let value: serde_json::Value =
        serde_json::from_slice(&listing.stdout).expect("the listing is JSON");
    value["listings"][0]["descriptor"]
        .as_str()
        .expect("the committed blob reports its descriptor")
        .to_owned()
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

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One HTTP/1.1 request over a fresh connection; the body is kept as bytes.
fn request(
    address: &str,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
) -> std::io::Result<Reply> {
    let mut stream = TcpStream::connect(address)?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n");
    for (name, value) in extra {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("the response has a header terminator");
    let head_text = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("the status line carries a code");
    let headers = head_text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    })
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
        if request(&address, "GET", "/", &[]).is_ok() {
            return (server, address);
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("local server becomes available on {address}");
}

/// A running server over a committed blob, with the bytes it must serve.
struct Served {
    _directory: TempDir,
    _server: RunningServer,
    address: String,
    media_path: String,
    descriptor: String,
    expected: Vec<u8>,
}

fn served_blob() -> Served {
    let directory = TempDir::new().expect("temporary project directory");
    let (root, relation) = committed_blob_project(&directory);
    let commit = git_stdout(&root, &["rev-parse", "HEAD"]);
    let descriptor = committed_descriptor(&root, &relation);
    let expected = fs::read(root.join("tone.wav")).expect("read the committed payload source");
    let (server, address) = start_server(&root);
    Served {
        _directory: directory,
        _server: server,
        media_path: format!("/media/{commit}/{descriptor}"),
        address,
        descriptor,
        expected,
    }
}

#[test]
fn get_serves_the_committed_payload_with_the_descriptor_as_validator() {
    let served = served_blob();
    let reply = request(&served.address, "GET", &served.media_path, &[]).expect("GET succeeds");
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.body, served.expected,
        "the bytes are the committed payload"
    );
    assert_eq!(reply.header("content-type"), Some("audio/wav"));
    assert_eq!(reply.header("accept-ranges"), Some("bytes"));
    assert_eq!(
        reply.header("content-length"),
        Some(served.expected.len().to_string().as_str())
    );
    assert_eq!(
        reply.header("etag"),
        Some(format!("\"{}\"", served.descriptor).as_str())
    );
    assert_eq!(
        reply.header("content-disposition"),
        None,
        "allowlisted audio is served inline"
    );
}

#[test]
fn range_requests_return_the_verified_slice_or_416() {
    let served = served_blob();
    let length = served.expected.len();

    let slice = request(
        &served.address,
        "GET",
        &served.media_path,
        &[("Range", "bytes=100-199")],
    )
    .expect("ranged GET succeeds");
    assert_eq!(slice.status, 206);
    assert_eq!(slice.body, served.expected[100..200].to_vec());
    assert_eq!(
        slice.header("content-range"),
        Some(format!("bytes 100-199/{length}").as_str())
    );

    let unsatisfiable = request(
        &served.address,
        "GET",
        &served.media_path,
        &[("Range", &format!("bytes={length}-"))],
    )
    .expect("unsatisfiable range succeeds at transport");
    assert_eq!(unsatisfiable.status, 416);
    assert!(unsatisfiable.body.is_empty(), "a 416 carries no payload");
    assert_eq!(
        unsatisfiable.header("content-range"),
        Some(format!("bytes */{length}").as_str())
    );
}

#[test]
fn head_carries_the_get_headers_without_a_body() {
    let served = served_blob();
    let reply = request(&served.address, "HEAD", &served.media_path, &[]).expect("HEAD succeeds");
    assert_eq!(reply.status, 200);
    assert!(reply.body.is_empty(), "HEAD never carries a body");
    assert_eq!(
        reply.header("content-length"),
        Some(served.expected.len().to_string().as_str())
    );
    assert_eq!(
        reply.header("etag"),
        Some(format!("\"{}\"", served.descriptor).as_str())
    );
}

#[test]
fn unknown_descriptor_and_unknown_commit_are_not_found() {
    let served = served_blob();
    let commit = served
        .media_path
        .trim_start_matches("/media/")
        .split('/')
        .next()
        .expect("media path names a commit")
        .to_owned();

    let unknown_descriptor = request(
        &served.address,
        "GET",
        &format!("/media/{commit}/{ZERO_OID}"),
        &[],
    )
    .expect("GET succeeds");
    assert_eq!(unknown_descriptor.status, 404);
    assert_eq!(
        unknown_descriptor.body,
        br#"{"error":"media.not_found"}"#.to_vec()
    );

    let unknown_commit = request(
        &served.address,
        "GET",
        &format!("/media/{ZERO_OID}/{}", served.descriptor),
        &[],
    )
    .expect("GET succeeds");
    assert_eq!(unknown_commit.status, 404);
}

#[test]
fn same_origin_writes_are_refused_with_method_not_allowed() {
    let served = served_blob();
    let origin = format!("http://{}", served.address);
    let reply = request(
        &served.address,
        "POST",
        &served.media_path,
        &[("Origin", &origin)],
    )
    .expect("POST succeeds at transport");
    assert_eq!(reply.status, 405);
    assert_eq!(reply.body, br#"{"error":"method_not_allowed"}"#.to_vec());

    let unauthenticated = request(&served.address, "POST", &served.media_path, &[])
        .expect("POST succeeds at transport");
    assert_eq!(
        unauthenticated.status, 403,
        "a write without same origin is denied first"
    );
}
