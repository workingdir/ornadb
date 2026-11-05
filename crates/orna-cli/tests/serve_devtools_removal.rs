#![cfg(target_os = "linux")]

//! Gate G removal scenario (ornadb-5il1.392, audit G-13; ORNA-APP-004,
//! ORNA-SERVE-007..009): the installed frontend is ordinary, removable source.
//! With it installed, `GET /` lists the clone's tables; after it is removed and
//! `orna serve` restarts, `/` answers a diagnostic with no HTML, so no native
//! fallback page is installed. The same project keeps its Git transport, its
//! local `orna query` listing, a mounted row read and write, and a mounted
//! stored Blob read.
//!
//! `orna serve` selects one serving entry: the root `main.orna` module's
//! `pub fn serve()` (`cli_serve_app::SERVE_MODULE_PATH`, `SERVE_ENTRY`). The
//! frontend is installed there, next to the project's own table declaration,
//! because a vendored `devtools/` module cannot yet be imported from the entry:
//! serve admits `main.orna` alone, so `use devtools;` is refused with
//! ORNA-S010-IMPORT before any request is served. Removing the frontend
//! therefore removes its `serve()` entry from `main.orna` and leaves the table
//! declaration in place, which is what keeps local query and mount working.

use std::{
    fs,
    io::{BufRead, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

#[path = "support/editor_mount.rs"]
mod editor_mount;
#[path = "support/format3.rs"]
mod format3;
#[path = "support/media_commit.rs"]
mod media_commit;

const BINARY: &str = env!("CARGO_BIN_EXE_orna");
/// The ordinary frontend source a project installs (Gate G / DESIGN S5a).
const FRONTEND_SOURCE: &str = include_str!("fixtures/devtools/serve.orna");
const STANDARD_ROOT: &str = include_str!("fixtures/devtools/standard-root.orna");
const STANDARD_UI: &str = include_str!("../../../stdlib/std/ui.orna");
/// The stored Blob the scenario captures into a row, and the project-relative
/// file it is captured from.
const BLOB: &[u8] = include_bytes!("fixtures/media/pixel.png");
const MEDIA_SOURCE: &str = "media-source.png";

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
    ] {
        command.env_remove(name);
    }
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(BINARY);
    isolate_git(&mut command);
    command
        .current_dir(directory)
        .args(arguments)
        .output()
        .expect("run the orna binary")
}

fn orna_succeeds(directory: &Path, arguments: &[&str]) -> Output {
    let output = orna(directory, arguments);
    assert!(
        output.status.success(),
        "orna {arguments:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn git_succeeds(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// A standard snapshot in its own repository, outside the project tree.
///
/// Its root imports only modules the snapshot contains, so the project's pinned
/// `stdlib/std` gitlink binds the `std.ui` the frontend page uses.
fn standard_snapshot() -> TempDir {
    let snapshot = tempfile::tempdir().expect("standard snapshot");
    git_succeeds(snapshot.path(), &["init", "-q"]);
    fs::write(snapshot.path().join("main.orna"), STANDARD_ROOT).expect("standard root");
    fs::write(snapshot.path().join("ui.orna"), STANDARD_UI).expect("standard ui module");
    git_succeeds(
        snapshot.path(),
        &[
            "-c",
            "user.name=Orna test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "add",
            "-A",
        ],
    );
    git_succeeds(
        snapshot.path(),
        &[
            "-c",
            "user.name=Orna test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "standard module snapshot",
        ],
    );
    snapshot
}

/// The project source: its two tables, plus the frontend's serving entry when
/// `with_frontend` is set. Removal rewrites this file without the entry.
///
/// `Note` is scalar so the served page can read it: serve refuses a page whose
/// admitted tables hold stored Blob rows (`serve.table_read_unsupported`), so
/// the `Asset` row is captured after the frontend is gone.
fn project_source(with_frontend: bool) -> String {
    let tables = "pub table Note(id: Str) {\n    body: Str,\n}\n\npub table Asset(id: Str) {\n    content: Blob,\n}\n";
    if with_frontend {
        format!("{tables}\n{FRONTEND_SOURCE}")
    } else {
        tables.to_owned()
    }
}

/// Creates and commits the project with the frontend installed and seeds one
/// scalar row, returning the project and each table's relation id (the
/// argument `orna query` takes).
fn installed_project(snapshot: &Path) -> (TempDir, (String, String)) {
    let project = tempfile::tempdir().expect("temporary Orna project");
    let initialized = Command::new(BINARY)
        .arg("init")
        .current_dir(project.path())
        .output()
        .expect("initialize project");
    assert!(initialized.status.success(), "initialize: {initialized:?}");
    let root = project.path();
    git_succeeds(root, &["config", "user.name", "Orna test"]);
    git_succeeds(root, &["config", "user.email", "orna-test@example.invalid"]);
    git_succeeds(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), project_source(true)).expect("install the frontend source");
    // A local clone needs the file protocol; the submodule must be checked out
    // on disk for the standard sources to bind.
    git_succeeds(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "-c",
            "user.name=Orna test",
            "-c",
            "user.email=orna-test@example.invalid",
            "submodule",
            "add",
            "-q",
            snapshot.to_str().expect("UTF-8 snapshot path"),
            "stdlib/std",
        ],
    );
    git_succeeds(root, &["add", "-A"]);
    let committed = orna_succeeds(root, &["commit", "-m", "install the devtools frontend"]);
    let relations = String::from_utf8_lossy(&committed.stdout)
        .lines()
        .filter_map(|line| {
            line.strip_prefix("admitted table ")
                .and_then(|admitted| admitted.split_once(" as relation "))
                .map(|(table, relation)| (table.to_owned(), relation.to_owned()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let note = relations
        .get("Note")
        .expect("orna commit admits Note")
        .clone();
    let asset = relations
        .get("Asset")
        .expect("orna commit admits Asset")
        .clone();

    let capture = "main.Note.insert({ id: \"note-one\", body: \"before\" })\n:quit\n";
    let mut repl = Command::new(BINARY);
    isolate_git(&mut repl);
    let mut repl = repl
        .args(["--db", root.to_str().expect("UTF-8 project path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the REPL");
    repl.stdin
        .take()
        .expect("REPL stdin")
        .write_all(capture.as_bytes())
        .expect("write the row");
    let seeded = repl.wait_with_output().expect("the REPL exits");
    assert!(
        seeded.status.success() && seeded.stderr.is_empty(),
        "seeding the row failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&seeded.stdout),
        String::from_utf8_lossy(&seeded.stderr),
    );
    orna_succeeds(root, &["publish"]);
    (project, (note, asset))
}

/// Removes the installed frontend's serving entry from the project source and
/// commits the removal with plain Git, exactly as an operator deleting the
/// frontend source would.
fn remove_frontend(root: &Path) {
    fs::write(root.join("main.orna"), project_source(false)).expect("remove the frontend source");
    git_succeeds(root, &["add", "main.orna"]);
    git_succeeds(
        root,
        &[
            "-c",
            "user.name=Orna test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "remove the installed devtools frontend",
        ],
    );
}

struct RunningServer {
    child: Child,
    stderr: Option<mpsc::Receiver<String>>,
}

impl RunningServer {
    /// Stops the server and waits for it, so its runtime owner lock is released
    /// before a mount opens the same clone.
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Drains the piped stderr until a line contains `needle`.
    fn await_stderr(&mut self, needle: &str) -> String {
        let receiver = self
            .stderr
            .as_mut()
            .expect("a server started with captured stderr");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(error) => {
                    panic!("the serving process never wrote {needle} to stderr: {error}")
                }
            }
        }
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn start(project: &Path, capture_stderr: bool) -> (RunningServer, String) {
    let port = free_port();
    let mut child = Command::new(BINARY);
    isolate_git(&mut child);
    let mut child = child
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project)
        .stdout(Stdio::null())
        .stderr(if capture_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()
        .expect("start the serving process");
    let stderr = child.stderr.take().map(|stderr| {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                let _ = sender.send(line);
            }
        });
        receiver
    });
    let address = format!("127.0.0.1:{port}");
    let server = RunningServer { child, stderr };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(&address).is_ok() {
            return (server, address);
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("the serving process did not accept connections");
}

fn http_get(address: &str, path: &str) -> std::io::Result<(u16, String, String)> {
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
    let content_type = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.trim().to_owned())
        .unwrap_or_default();
    Ok((status, content_type, body.to_owned()))
}

/// Waits until `path` appears inside the mount and returns its bytes.
fn read_mounted(path: &Path) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match fs::read(path) {
            Ok(bytes) => return bytes,
            Err(error) => assert!(
                Instant::now() < deadline,
                "mount never exposed {}: {error}",
                path.display()
            ),
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Waits until the mounted document at `path` reads back `needle`.
fn read_mounted_until(path: &Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let document = String::from_utf8(read_mounted(path)).expect("row document is UTF-8");
        if document.contains(needle) || Instant::now() >= deadline {
            return document;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn devtools_removal_keeps_local_query_mount_and_content_io() {
    if !editor_mount::require_fuse_or_skip() {
        return;
    }
    let snapshot = standard_snapshot();
    let (project, (note_relation, asset_relation)) = installed_project(snapshot.path());
    let root = project.path();

    // (1) The installed frontend answers `/` and lists the clone's tables.
    let (server, address) = start(root, false);
    let (status, content_type, page) = http_get(&address, "/").expect("GET / with the frontend");
    assert_eq!(status, 200, "page: {page}");
    assert_eq!(content_type, "text/html; charset=utf-8");
    assert!(page.contains("<table"), "the page carries a table: {page}");
    assert!(
        page.contains("<th>name</th>")
            && page.contains("<td>Note</td>")
            && page.contains("<td>Asset</td>"),
        "the table list names the clone's tables: {page}"
    );
    assert!(
        page.contains("<input") && page.contains("Refresh"),
        "the frontend page carries its input and refresh control: {page}"
    );
    server.stop();

    // (2) Removing the frontend leaves no page behind: the restart reports the
    // absent entry and `/` is a JSON diagnostic, never HTML.
    remove_frontend(root);
    let (mut server, address) = start(root, true);
    let diagnostic = server.await_stderr("E2102");
    assert!(
        diagnostic.contains("no serving entry"),
        "diagnostic: {diagnostic}"
    );
    let (status, content_type, body) = http_get(&address, "/").expect("GET / after removal");
    assert_eq!(status, 404, "body: {body}");
    assert_eq!(content_type, "application/json");
    assert_eq!(body, r#"{"error":"not_found"}"#);
    assert!(
        !body.contains('<'),
        "the removed frontend leaves no HTML behind: {body}"
    );
    // The retired host-generated devtools route is not reinstalled either.
    let (route, _, _) = http_get(&address, "/devtools/tables").expect("GET the retired route");
    assert_eq!(route, 404);

    // (3) Git transport still clones the served clone.
    let clone_parent = tempfile::tempdir().expect("clone parent");
    let checkout = clone_parent.path().join("checkout");
    let mut clone = Command::new("git");
    isolate_git(&mut clone);
    let cloned = clone
        .args(["-c", "http.proxy=", "clone", "--quiet"])
        .arg(format!("http://{address}/git"))
        .arg(&checkout)
        .output()
        .expect("git clone over the served transport");
    assert!(
        cloned.status.success(),
        "Git clone failed: {}",
        String::from_utf8_lossy(&cloned.stderr)
    );
    let served_head = git_succeeds(root, &["rev-parse", "HEAD"]);
    let cloned_head = git_succeeds(&checkout, &["rev-parse", "HEAD"]);
    assert_eq!(
        String::from_utf8_lossy(&cloned_head.stdout).trim(),
        String::from_utf8_lossy(&served_head.stdout).trim(),
        "the clone holds the served HEAD"
    );

    // (4) Local query still lists the committed row.
    let queried = orna_succeeds(root, &["query", &note_relation, "--format", "json"]);
    let report: serde_json::Value =
        serde_json::from_slice(&queried.stdout).expect("orna query prints JSON");
    assert_eq!(report["rows"].as_u64(), Some(1), "report: {report}");
    server.stop();

    // (5) Stored content still lands in the same clone after the frontend is
    // gone: a Blob row is captured, published, and listed by local query.
    fs::write(root.join(MEDIA_SOURCE), BLOB).expect("write the Blob capture source");
    let capture = format!(
        "let raw = sys.blob.capture_file({root:?}, {source:?}, 65536);\n\
         let image = sys.blob.annotate(raw, \"image/png\");\n\
         main.Asset.insert({{ id: \"blob-one\", content: image }})\n\
         :quit\n",
        root = root.to_str().expect("UTF-8 project path"),
        source = MEDIA_SOURCE,
    );
    let mut repl = Command::new(BINARY);
    isolate_git(&mut repl);
    let mut repl = repl
        .args(["--db", root.to_str().expect("UTF-8 project path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the REPL");
    repl.stdin
        .take()
        .expect("REPL stdin")
        .write_all(capture.as_bytes())
        .expect("write the Blob row");
    let seeded = repl.wait_with_output().expect("the REPL exits");
    assert!(
        seeded.status.success() && seeded.stderr.is_empty(),
        "capturing the Blob failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&seeded.stdout),
        String::from_utf8_lossy(&seeded.stderr),
    );
    orna_succeeds(root, &["publish"]);
    let queried = orna_succeeds(root, &["query", &asset_relation, "--format", "json"]);
    let report: serde_json::Value =
        serde_json::from_slice(&queried.stdout).expect("orna query prints JSON");
    assert_eq!(report["rows"].as_u64(), Some(1), "report: {report}");
    assert_eq!(report["blobs"].as_u64(), Some(1), "report: {report}");

    // (6) The writable mount still reads the row, accepts a row write, and
    // reads the stored Blob.
    let mount_parent = tempfile::tempdir().expect("mount parent");
    let mountpoint = mount_parent.path().join("view");
    let mounted = editor_mount::spawn_mount_process(root, &mountpoint);
    let row_directory = mountpoint.join("Note/note-one");
    let document = read_mounted(&row_directory.join("data.orna"));
    let document = String::from_utf8(document).expect("row document is UTF-8");
    assert!(
        document.contains("body: \"before\""),
        "the mounted row reads its committed value: {document}"
    );
    let content = read_mounted(&mountpoint.join("Asset/blob-one/content.png"));
    assert_eq!(content, BLOB, "the mounted Blob reads its stored bytes");

    // A save is an editor temporary renamed over the row (41-vfs.md EDIT-1):
    // the mount captures the row baseline when the scratch is created.
    let replacement = "{\n    body: \"after\",\n}\n";
    let temporary = row_directory.join(".data.orna.editor-tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .expect("create the editor temporary");
    file.write_all(replacement.as_bytes())
        .expect("write the editor temporary");
    file.sync_all().expect("sync the editor temporary");
    drop(file);
    fs::rename(&temporary, row_directory.join("data.orna"))
        .expect("rename the editor temporary over the row");
    let written = read_mounted_until(&row_directory.join("data.orna"), "body: \"after\"");
    assert!(
        written.contains("body: \"after\""),
        "the mount accepts the row write: {written}"
    );
    assert!(
        !mounted.stderr_text().contains("not accepted"),
        "the mount accepted every write: {}",
        mounted.stderr_text()
    );
}
