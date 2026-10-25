//! ORNA-SERVE-003 / ORNA-TXN-005 / ORNA-TXN-006 (ornadb-03ol): `orna serve`
//! reads the committed `HEAD` `main.orna` and the committed rows overlaid with
//! the CWD generation of each activation. Durable CWD rows are served, a
//! `serve` that attempts a table write is refused without committing, and a
//! `HEAD` without a serving entry is reported rather than guessed.

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

const COUNT_SOURCE: &str =
    "pub table Note(id: Str) { body: Str, }\n\npub fn serve() = Note | count();\n";
const WRITE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n\npub fn serve() = Note.insert({ id: \"write\", body: \"refused\" });\n";
const MIXED_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n\npub table Other(id: Str) { body: Str, }\n\npub fn serve() = Note | count();\n";
const SCHEMA_CHANGE_SOURCE: &str = "pub table Note(id: Str) { body: Str, }\n\npub table Other(id: Str) { body: Str, }\n\npub fn serve() = (Note | count()) + 10;\n";
const COUNT_PLUS_TEN_SOURCE: &str =
    "pub table Note(id: Str) { body: Str, }\n\npub fn serve() = (Note | count()) + 10;\n";

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

fn git(root: &Path, arguments: &[&str]) {
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

/// `orna init`, a test-local commit identity, `main.orna` set to `source`, and
/// the first commit through `orna commit`.
fn committed_project(directory: &TempDir, source: &str) -> PathBuf {
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
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Declare Note"]);
    assert_success(&committed, "orna commit");
    root
}

fn insert_note(root: &Path, id: &str, body: &str) {
    let inserted = orna_repl(
        root,
        &format!(r#"main.Note.insert({{ id: "{id}", body: "{body}" }})"#),
    );
    assert_success(&inserted, "REPL insert");
    assert!(
        !transcript(&inserted).contains("error["),
        "the REPL accepts the row\n{}",
        transcript(&inserted)
    );
}

fn publish(root: &Path) {
    let published = orna(root, &["publish", "--message", "Add notes"]);
    assert_success(&published, "orna publish");
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

#[test]
fn committed_published_rows_are_counted_at_root() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "1 : Int");
}

#[test]
fn serve_that_writes_a_table_is_refused_without_committing() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, WRITE_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert!(body.contains("serve.write_refused"), "body: {body}");
    // The serving process owns the runtime; the count runs once it has exited.
    drop(_server);
    let counted = orna_repl(&root, "main.Note | count()");
    assert_success(&counted, "REPL count");
    assert!(
        transcript(&counted).contains("1 : Int"),
        "the refused write leaves the committed row unchanged\n{}",
        transcript(&counted)
    );
}

#[test]
fn durable_cwd_rows_are_served_without_publication() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    // Durable CWD change: inserted and never published. ORNA-TXN-005 makes it
    // visible to the next activation of this clone's CWD.
    insert_note(&root, "two", "second");
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "2 : Int", "committed and durable CWD rows");
}

#[test]
fn head_commit_changes_are_visible_to_the_next_request() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "1 : Int");
    // The serving process owns the runtime, so the HEAD change is a source
    // commit made outside it.
    fs::write(root.join("main.orna"), COUNT_PLUS_TEN_SOURCE).expect("write main.orna");
    git(&root, &["add", "main.orna"]);
    git(&root, &["commit", "-m", "Add ten"]);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(
        body.trim(),
        "11 : Int",
        "each request reads the current HEAD"
    );
}

#[test]
fn schema_changing_head_commit_is_admitted_at_the_next_request() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "1 : Int");
    // The commit adds a table and changes the served expression, so the next
    // request must admit the committed main.orna and its identity manifest.
    fs::write(root.join("main.orna"), SCHEMA_CHANGE_SOURCE).expect("write main.orna");
    git(&root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(&root, &["commit", "-m", "Add Other table"]);
    assert_success(&committed, "orna commit");
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(
        body.trim(),
        "11 : Int",
        "the schema-changing commit is served"
    );
}

#[test]
fn data_only_commit_is_visible_to_the_next_server() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    let (server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "1 : Int");
    // The serving process owns the runtime, so a REPL write cannot run while it
    // is up. The data commit happens after it exits; the next server reads HEAD.
    drop(server);
    insert_note(&root, "two", "second");
    publish(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "2 : Int", "the data-only commit is served");
}

#[test]
fn working_tree_source_does_not_replace_committed_source() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    // Uncommitted edit: `Other` exists only in the working tree, so HEAD's
    // `main.orna` (which declares only `Note`) is still the served module.
    fs::write(root.join("main.orna"), MIXED_SOURCE).expect("rewrite main.orna");
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "1 : Int");
}

#[test]
fn unborn_head_reports_entry_unavailable() {
    let directory = TempDir::new().expect("create the test directory");
    let root = directory.path().join("library");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    // The working tree declares `serve`, so startup admits it, but no commit
    // exists yet to serve (ORNA-SERVE-003).
    fs::write(root.join("main.orna"), COUNT_SOURCE).expect("write main.orna");
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert!(body.contains("serve.entry_unavailable"), "body: {body}");
}

/// Walks `directory` and collects every regular file beneath it.
fn store_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read the store directory") {
        let path = entry.expect("read a store entry").path();
        if path.is_dir() {
            store_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// Overwrites every file of the committed format-3 store in a new commit. The
/// store holds both the commit's identity inventory and its rows (ORNA-REPO-004),
/// so the committed HEAD is unreadable and `serve` must refuse it.
fn commit_corrupt_store(root: &Path) {
    let mut files = Vec::new();
    store_files(&root.join(".orna/store"), &mut files);
    assert!(
        !files.is_empty(),
        "the committed store has files to corrupt"
    );
    for file in &files {
        fs::write(file, b"corrupt").expect("corrupt a store file");
    }
    git(root, &["add", ".orna/store"]);
    git(root, &["commit", "-m", "Corrupt store"]);
}

#[test]
fn corrupt_identity_manifest_is_refused_without_a_value() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    commit_corrupt_store(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert!(
        body.contains("serve.table_identity_unavailable"),
        "body: {body}"
    );
    assert!(!body.contains(": Int"), "no partial value: {body}");
}

#[test]
fn corrupt_committed_table_is_refused_without_a_value() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, COUNT_SOURCE);
    insert_note(&root, "one", "first");
    publish(&root);
    commit_corrupt_store(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    // Every store file is overwritten, so the identity inventory is unreadable
    // and fails before any committed row is read; no value is served.
    assert!(
        body.contains("serve.table_identity_unavailable"),
        "body: {body}"
    );
    assert!(!body.contains(": Int"), "no partial value: {body}");
}

const BLOB_SOURCE: &str = "pub table media(id: Str) { content: Blob, }\n\npub fn store(id: Str, content: Blob) = media.insert({ id: id, content: content });\n\npub fn serve() = media | count();\n";

#[test]
fn committed_blob_rows_are_refused_without_a_value() {
    let directory = TempDir::new().expect("create the test directory");
    let root = committed_project(&directory, BLOB_SOURCE);
    let pixel = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/media/pixel.png"
    ))
    .expect("read the pixel fixture");
    fs::write(root.join("pixel.png"), &pixel).expect("place the captured file");
    let root_text = root.to_string_lossy();
    let stored = orna_repl(
        &root,
        &format!(
            "let raw = sys.blob.capture_file({root_text:?}, \"pixel.png\", 65536);\nlet image = sys.blob.annotate(raw, \"image/png\");\nmain.store(\"one\", image).id"
        ),
    );
    assert_success(&stored, "REPL blob insert");
    assert!(
        !transcript(&stored).contains("error["),
        "the REPL accepts the blob row\n{}",
        transcript(&stored)
    );
    publish(&root);
    let (_server, address) = start_server(&root);
    let (status, body) = http_get(&address, "/").expect("GET /");
    assert_eq!(status, 500, "body: {body}");
    assert!(
        body.contains("serve.table_read_unsupported"),
        "body: {body}"
    );
    assert!(!body.contains(": Int"), "no partial value: {body}");
}
