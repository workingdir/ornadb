//! A table renamed through the language server keeps its ObjectId through
//! `orna commit` (ORNA-OBJECT-003, ORNA-EVOL-001; Gate A "identities survive
//! rename"). The real `orna-lsp` binary is driven over stdio: `initialize`,
//! `textDocument/didOpen`, `textDocument/rename` of a table, the returned
//! workspace edit applied to `main.orna`, `didChange`, `didSave`, `shutdown`.
//! Then the real `orna commit` runs and the table keeps its committed
//! identity, as `rename_identity_e2e.rs` asserts for `orna mv`.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio},
};

use orna_project_v1::{ObjectIdentityManifest, ObjectKind};
use orna_repository_v1::Repository;
use serde_json::{json, Value};
use tempfile::TempDir;

const MUSIC_SOURCE: &str = "pub table Music(id: Str) { title: Str, }\n";

/// The `orna-lsp` binary built alongside this test executable. It is a
/// separate workspace binary, so `cargo build -p orna-lsp` must have produced
/// it; the test refuses to run without it rather than skipping.
fn orna_lsp_binary() -> PathBuf {
    let test_exe = std::env::current_exe().expect("the test executable path");
    let profile_dir = test_exe
        .parent()
        .and_then(Path::parent)
        .expect("the test executable lives in target/<profile>/deps");
    let binary = profile_dir.join("orna-lsp");
    assert!(
        binary.exists(),
        "{} is missing; run `cargo build -p orna-lsp` into the same target directory first",
        binary.display()
    );
    binary
}

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

fn git(root: &Path, arguments: &[&str]) -> String {
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
    String::from_utf8(output.stdout).expect("git prints UTF-8")
}

fn transcript(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, step: &str) {
    assert_eq!(output.status.code(), Some(0), "{step} failed\n{}", transcript(output));
}

/// `orna init` into a fresh directory with a test-local commit identity and
/// `main.orna` set to `source`.
fn initialized_database(directory: &TempDir, source: &str) -> PathBuf {
    let root = directory.path().join("library");
    let init = orna(directory.path(), &["init", root.to_str().expect("UTF-8 test path")]);
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(&root, &["config", "user.email", "orna-test@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    root
}

/// Stages the source with Git and commits it with `orna commit`.
fn commit_source(root: &Path, message: &str) {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", message]);
    assert_success(&committed, "orna commit");
}

fn head_identities(root: &Path) -> ObjectIdentityManifest {
    let repository = Repository::discover(root).expect("discover the repository");
    let head = repository.head().expect("read HEAD").expect("a committed HEAD");
    ObjectIdentityManifest::read_committed(&repository, &head).expect("committed identities")
}

/// A stdio client speaking LSP framing to one `orna-lsp` process. Messages
/// that are not the awaited response (notifications) are kept in `shown`.
struct Client {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
    shown: Vec<String>,
}

impl Client {
    fn spawn(root: &Path) -> Self {
        let mut child = Command::new(orna_lsp_binary())
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn orna-lsp");
        Self {
            stdin: child.stdin.take().expect("orna-lsp stdin"),
            reader: BufReader::new(child.stdout.take().expect("orna-lsp stdout")),
            child,
            next_id: 1,
            shown: Vec::new(),
        }
    }

    fn send(&mut self, value: &Value) {
        let body = serde_json::to_vec(value).expect("serialise the message");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("write header");
        self.stdin.write_all(&body).expect("write body");
        self.stdin.flush().expect("flush");
    }

    fn read(&mut self) -> Value {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).expect("read a header line");
            assert!(read > 0, "orna-lsp closed its stdout");
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = Some(value.trim().parse::<usize>().expect("a numeric length"));
            }
        }
        let mut body = vec![0; length.expect("a Content-Length header")];
        std::io::Read::read_exact(&mut self.reader, &mut body).expect("read the body");
        serde_json::from_slice(&body).expect("a JSON-RPC message")
    }

    /// Sends a request and returns its `result`, collecting any
    /// `window/showMessage` notifications that arrive first.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let message = self.read();
            if message.get("method").and_then(Value::as_str) == Some("window/showMessage") {
                self.shown.push(message["params"]["message"].as_str().unwrap_or_default().to_owned());
                continue;
            }
            if message.get("id") == Some(&json!(id)) {
                assert!(message.get("error").is_none(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn finish(mut self) -> Vec<String> {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let status = self.child.wait().expect("orna-lsp exits");
        assert!(status.success(), "orna-lsp exit status: {status}");
        self.shown
    }
}

/// Applies single-line ASCII `TextEdit`s from a `WorkspaceEdit` result, which
/// the test sources guarantee.
fn apply_edits(source: &str, edits: &[Value]) -> String {
    let mut text = source.to_owned();
    let mut ordered: Vec<&Value> = edits.iter().collect();
    ordered.sort_by_key(|edit| std::cmp::Reverse(edit["range"]["start"]["character"].as_u64()));
    for edit in ordered {
        let line = edit["range"]["start"]["line"].as_u64().unwrap() as usize;
        let start = edit["range"]["start"]["character"].as_u64().unwrap() as usize;
        let end = edit["range"]["end"]["character"].as_u64().unwrap() as usize;
        let offset = text.split_inclusive('\n').take(line).map(str::len).sum::<usize>();
        text.replace_range(offset + start..offset + end, edit["newText"].as_str().unwrap());
    }
    text
}

#[test]
fn lsp_rename_then_commit_keeps_the_table_object_id() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MUSIC_SOURCE);
    commit_source(&root, "Declare Music");
    let before = head_identities(&root);
    let music_id = *before.id(ObjectKind::Table, "Music").expect("Music has a committed identity");

    let main = root.join("main.orna");
    let uri = format!("file://{}", main.display());
    let mut client = Client::spawn(&root);
    client.request("initialize", json!({ "processId": null, "rootUri": null, "capabilities": {} }));
    client.notify("initialized", json!({}));
    client.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "orna", "version": 1, "text": MUSIC_SOURCE } }),
    );

    // Position 11 is inside `Music` on line 0 of `pub table Music(...)`.
    let rename = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 11 },
            "newName": "Archive",
        }),
    );
    let edits = rename["changes"][&uri].as_array().expect("the rename edits main.orna").clone();
    let renamed = apply_edits(MUSIC_SOURCE, &edits);
    assert_eq!(renamed, "pub table Archive(id: Str) { title: Str, }\n");
    fs::write(&main, &renamed).expect("write the reviewed rename");
    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": renamed }] }),
    );
    client.notify("textDocument/didSave", json!({ "textDocument": { "uri": uri } }));
    let shown = client.finish();
    assert!(shown.is_empty(), "a table rename with identity carries no warning: {shown:?}");

    // The save staged the renamed source and the identity inventory; no
    // `git add` is needed before `orna commit` takes them.
    let committed = orna(&root, &["commit", "-m", "Rename Music to Archive"]);
    assert_success(&committed, "orna commit");
    let after = head_identities(&root);
    assert_eq!(after.id(ObjectKind::Table, "Archive"), Some(&music_id));
    assert_eq!(after.id(ObjectKind::Table, "Music"), None);
    for field in ["id", "title"] {
        assert_eq!(
            after.id(ObjectKind::Field, &format!("Archive.{field}")),
            before.id(ObjectKind::Field, &format!("Music.{field}")),
            "field {field} keeps its ID"
        );
    }
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
}
