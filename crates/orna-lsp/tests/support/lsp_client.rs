//! A JSON-RPC client that drives the real `orna-lsp` binary over stdio, and
//! Orna projects created the way `orna init` creates them.
#![allow(dead_code)]

use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

pub struct Client {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
    /// Server messages read while waiting for a different one.
    pending: VecDeque<Value>,
}

impl Client {
    /// Starts the server and completes the initialize handshake.
    pub fn start() -> Self {
        Self::start_with_env(&[])
    }

    /// Starts the server with extra environment variables.
    pub fn start_with_env(env: &[(&str, &Path)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orna-lsp"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn orna-lsp");
        let mut client = Self {
            stdin: child.stdin.take().unwrap(),
            reader: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 1,
            pending: VecDeque::new(),
        };
        client.result(
            "initialize",
            json!({"processId":null,"rootUri":null,"capabilities":{}}),
        );
        client.notify("initialized", json!({}));
        client
    }

    fn send(&mut self, value: Value) {
        let body = serde_json::to_vec(&value).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(&body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(
                self.reader.read_line(&mut line).unwrap() > 0,
                "orna-lsp closed its output"
            );
            if line.trim().is_empty() {
                break;
            }
            if let Some(value) = line.trim().strip_prefix("Content-Length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.reader.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Sends a request without waiting; returns its id.
    pub fn send_request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        id
    }

    /// Waits for the response to request `id`, keeping other messages.
    pub fn response(&mut self, id: i64) -> Value {
        if let Some(index) = self
            .pending
            .iter()
            .position(|message| message.get("method").is_none() && message["id"] == id)
        {
            return self.pending.remove(index).unwrap();
        }
        loop {
            let message = self.read();
            if message.get("method").is_none() && message["id"] == id {
                return message;
            }
            self.pending.push_back(message);
        }
    }

    /// Sends a request and returns the whole response (result or error).
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request(method, params);
        self.response(id)
    }

    /// Sends a request and returns its result, failing on an error response.
    pub fn result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        assert!(response.get("error").is_none(), "{method}: {response}");
        response["result"].clone()
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    pub fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"orna","version":1,"text":text}}),
        );
    }

    pub fn change(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":version},"contentChanges":[{"text":text}]}),
        );
    }

    /// Waits for the next diagnostics published for `uri`.
    pub fn diagnostics(&mut self, uri: &str) -> Value {
        let matches = |message: &Value| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == uri
        };
        if let Some(index) = self.pending.iter().position(matches) {
            return self.pending.remove(index).unwrap()["params"].clone();
        }
        loop {
            let message = self.read();
            if matches(&message) {
                return message["params"].clone();
            }
            self.pending.push_back(message);
        }
    }

    /// Waits until `uri` is published for `version` and returns those
    /// diagnostics, skipping earlier publications.
    pub fn diagnostics_at(&mut self, uri: &str, version: i32) -> Vec<Value> {
        loop {
            let published = self.diagnostics(uri);
            if published["version"] == version {
                return published["diagnostics"].as_array().unwrap().clone();
            }
        }
    }

    pub fn shutdown(mut self) {
        self.result("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        assert!(self.child.wait().unwrap().success());
    }
}

/// The LSP position of a byte offset in `source` (UTF-16 columns).
pub fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column_text = prefix.rsplit('\n').next().unwrap_or("");
    json!({"line":line,"character":column_text.encode_utf16().count()})
}

/// The LSP position of `offset` bytes into the first `needle` in `source`.
pub fn position_of(source: &str, needle: &str, offset: usize) -> Value {
    position_at(source, source.find(needle).expect(needle) + offset)
}

/// The LSP range of the first `needle` in `source`.
pub fn range_of(source: &str, needle: &str) -> Value {
    let start = source.find(needle).expect(needle);
    json!({"start":position_at(source, start),"end":position_at(source, start + needle.len())})
}

/// An Orna project in a temporary Git worktree, initialised by the same
/// library call `orna init` makes.
pub struct Project {
    directory: tempfile::TempDir,
    root: PathBuf,
}

impl Project {
    pub fn new(files: &[(&str, &str)]) -> Self {
        let directory = tempfile::tempdir().expect("project directory");
        let root = fs::canonicalize(directory.path()).unwrap();
        for (path, source) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        orna_repository_v1::initialize_repository(&root).expect("orna init");
        Self { directory, root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, path: &str, source: &str) {
        fs::write(self.root.join(path), source).unwrap();
    }

    pub fn uri(&self, path: &str) -> String {
        url::Url::from_file_path(self.root.join(path))
            .unwrap()
            .to_string()
    }
}
