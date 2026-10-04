//! Protocol proofs for the Orna 1.0 language server.

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

const SOURCE: &str = include_str!("fixtures/expressions-v1.orna");
const CALL_SOURCE: &str = include_str!("fixtures/call-v1.orna");
const INCREMENTAL_SOURCE: &str = include_str!("fixtures/incremental-malformed-v1.orna");
const LOCAL_SCOPES_SOURCE: &str = include_str!("fixtures/local-scopes-v1.orna");
const HOVER_COMPLETION_SOURCE: &str = include_str!("fixtures/hover-completion-v1.orna");

struct Client {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl Client {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_orna-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn orna-lsp");
        Self {
            stdin: child.stdin.take().unwrap(),
            reader: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 1,
        }
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
            self.reader.read_line(&mut line).unwrap();
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

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let response = self.read();
        assert_eq!(response["id"], id, "response for {method}: {response}");
        response["result"].clone()
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    fn notification(&mut self, method: &str) -> Value {
        let response = self.read();
        assert_eq!(response["method"], method, "notification: {response}");
        response["params"].clone()
    }

    fn shutdown(&mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        assert!(self.child.wait().unwrap().success());
    }
}

fn position_at(source: &str, byte: usize) -> Value {
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column_text = prefix.rsplit('\n').next().unwrap_or("");
    let character = column_text.encode_utf16().count();
    json!({"line":line,"character":character})
}

fn position_of(source: &str, needle: &str, offset: usize) -> Value {
    position_at(source, source.find(needle).unwrap() + offset)
}

fn range_at(source: &str, start: usize, end: usize) -> Value {
    json!({"start":position_at(source, start),"end":position_at(source, end)})
}

fn initialize(client: &mut Client) {
    let result = client.request(
        "initialize",
        json!({"processId":null,"rootUri":null,"capabilities":{}}),
    );
    assert_eq!(result["capabilities"]["positionEncoding"], "utf-16");
    assert_eq!(result["capabilities"]["textDocumentSync"]["change"], 2);
    assert_eq!(result["capabilities"]["hoverProvider"], true);
    client.notify("initialized", json!({}));
}

fn open(client: &mut Client, uri: &str, source: &str) -> Value {
    client.notify(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"orna","version":1,"text":source}}),
    );
    client.notification("textDocument/publishDiagnostics")
}

#[test]
fn v1_workspace_model_powers_editor_features_across_open_files() {
    let uri = "file:///workspace/expressions-v1.orna";
    let caller_uri = "file:///workspace/call-v1.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, SOURCE);
    assert!(
        diagnostics["diagnostics"].as_array().unwrap().is_empty(),
        "{diagnostics}"
    );
    let caller_diagnostics = open(&mut client, caller_uri, CALL_SOURCE);
    assert!(
        caller_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{caller_diagnostics}"
    );

    let call_offset = CALL_SOURCE.find("add(value, 2)").unwrap();
    let call_position = position_at(CALL_SOURCE, call_offset + 1);
    let hover = client.request(
        "textDocument/hover",
        json!({"textDocument":{"uri":caller_uri},"position":call_position}),
    );
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("Add two integer values.")
    );
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("fn add(left: Int, right: Int): Int")
    );

    let definition = client.request(
        "textDocument/definition",
        json!({"textDocument":{"uri":caller_uri},"position":call_position}),
    );
    assert_eq!(definition["uri"], uri);
    assert_eq!(
        definition["range"]["start"],
        position_of(SOURCE, "pub fn add", "pub fn ".len())
    );

    let references = client.request("textDocument/references", json!({"textDocument":{"uri":caller_uri},"position":call_position,"context":{"includeDeclaration":true}}));
    assert_eq!(references.as_array().unwrap().len(), 3);
    assert!(
        references
            .as_array()
            .unwrap()
            .iter()
            .any(|location| location["uri"] == uri)
    );
    assert!(
        references
            .as_array()
            .unwrap()
            .iter()
            .any(|location| location["uri"] == caller_uri)
    );

    let renamed = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":caller_uri},"position":call_position,"newName":"sum"}),
    );
    let edits = renamed["changes"][uri].as_array().unwrap();
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|edit| edit["newText"] == "sum"));
    let caller_edits = renamed["changes"][caller_uri].as_array().unwrap();
    assert_eq!(caller_edits.len(), 1);
    assert_eq!(caller_edits[0]["newText"], "sum");

    let cursor = CALL_SOURCE.find("add(value, 2)").unwrap() + "add(value, ".len();
    let signature = client.request(
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":caller_uri},"position":position_at(CALL_SOURCE,cursor)}),
    );
    assert_eq!(signature["activeParameter"], 1);
    assert!(
        signature["signatures"][0]["label"]
            .as_str()
            .unwrap()
            .contains("fn add(left: Int, right: Int): Int")
    );

    let completion = client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":caller_uri},"position":position_at(CALL_SOURCE,call_offset)}),
    );
    let completion_items = completion
        .as_array()
        .or_else(|| completion["items"].as_array())
        .unwrap();
    let add = completion_items
        .iter()
        .find(|item| item["label"] == "add")
        .unwrap();
    assert_eq!(add["insertText"], "add(${1:left}, ${2:right})");

    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({"textDocument":{"uri":uri}}),
    );
    assert!(
        symbols
            .as_array()
            .unwrap()
            .iter()
            .any(|symbol| symbol["name"] == "Outcome")
    );
    client.shutdown();
}

#[test]
fn legacy_sql_is_rejected_and_parser_diagnostic_range_is_precise() {
    let uri = "file:///workspace/invalid.orna";
    let source = "pub fn broken(value: Int): Int = value + ;";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, source)["diagnostics"].clone();
    assert_eq!(diagnostics.as_array().unwrap().len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic["source"], "orna-syntax-v1");
    assert_eq!(diagnostic["range"]["start"], position_of(source, ";", 0));

    let legacy = "CREATE SCHEMA app;";
    let legacy_diagnostics =
        open(&mut client, "file:///workspace/legacy.orna", legacy)["diagnostics"].clone();
    assert!(!legacy_diagnostics.as_array().unwrap().is_empty());
    assert!(
        legacy_diagnostics
            .as_array()
            .unwrap()
            .iter()
            .all(|diagnostic| diagnostic["source"] == "orna-syntax-v1")
    );
    client.shutdown();
}

#[test]
fn incremental_changes_apply_in_order_with_utf16_positions_and_ignore_stale_versions() {
    let uri = "file:///workspace/incremental.orna";
    let source = INCREMENTAL_SOURCE;
    let semicolon = source.find(';').unwrap();
    let mut intermediate = source.to_owned();
    intermediate.replace_range(semicolon..semicolon + 1, "10;");
    let mut expected = intermediate.clone();
    expected.replace_range(semicolon + 1..semicolon + 2, "2");
    assert!(
        orna_syntax_v1::parse_module(&expected)
            .diagnostics
            .is_empty(),
        "expected source: {expected:?}"
    );

    let mut client = Client::spawn();
    initialize(&mut client);
    let initial = open(&mut client, uri, source);
    assert!(!initial["diagnostics"].as_array().unwrap().is_empty());

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument":{"uri":uri,"version":2},
            "contentChanges":[
                {"range":range_at(source, semicolon, semicolon + 1),"text":"10;"},
                {"range":range_at(&intermediate, semicolon + 1, semicolon + 2),"text":"2"}
            ]
        }),
    );
    let updated = client.notification("textDocument/publishDiagnostics");
    assert_eq!(updated["version"], 2);
    assert!(
        updated["diagnostics"].as_array().unwrap().is_empty(),
        "{updated}"
    );

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument":{"uri":uri,"version":1},
            "contentChanges":[{"range":range_at(&expected, semicolon, semicolon + 2),"text":";"}]
        }),
    );
    client.notify("textDocument/didSave", json!({"textDocument":{"uri":uri}}));
    let after_stale_change = client.notification("textDocument/publishDiagnostics");
    assert_eq!(after_stale_change["version"], 2);
    assert!(
        after_stale_change["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "stale didChange corrupted the open document: {after_stale_change}"
    );
    client.shutdown();
}

#[test]
fn local_navigation_and_rename_follow_shadowed_bindings() {
    let uri = "file:///workspace/local-scopes.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, LOCAL_SCOPES_SOURCE);
    assert!(diagnostics["diagnostics"].as_array().unwrap().is_empty());

    let inner_use = LOCAL_SCOPES_SOURCE.rfind("input\n").unwrap() + 1;
    let inner_position = position_at(LOCAL_SCOPES_SOURCE, inner_use);
    let definition = client.request(
        "textDocument/definition",
        json!({"textDocument":{"uri":uri},"position":inner_position}),
    );
    assert_eq!(definition["uri"], uri);
    assert_eq!(
        definition["range"]["start"],
        position_of(LOCAL_SCOPES_SOURCE, "let input", "let ".len())
    );
    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":uri},
            "position":inner_position,
            "context":{"includeDeclaration":true}
        }),
    );
    assert_eq!(references.as_array().unwrap().len(), 2);

    let local_rename = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":uri},"position":inner_position,"newName":"inner_value"}),
    );
    let local_edits = local_rename["changes"][uri].as_array().unwrap();
    assert_eq!(local_edits.len(), 2);
    assert!(
        local_edits
            .iter()
            .all(|edit| edit["newText"] == "inner_value")
    );

    let parameter = position_of(LOCAL_SCOPES_SOURCE, "shadow(input", "shadow(".len());
    let parameter_rename = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":uri},"position":parameter,"newName":"renamed_input"}),
    );
    let parameter_edits = parameter_rename["changes"][uri].as_array().unwrap();
    assert_eq!(parameter_edits.len(), 4);
    assert!(
        parameter_edits
            .iter()
            .all(|edit| edit["newText"] == "renamed_input")
    );
    client.shutdown();
}

#[test]
fn hover_resolves_shadowed_bindings_and_completion_ranks_visible_locals() {
    let uri = "file:///workspace/hover-completion.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, HOVER_COMPLETION_SOURCE);
    assert!(
        diagnostics["diagnostics"].as_array().unwrap().is_empty(),
        "{diagnostics}"
    );

    let shadowed_use = HOVER_COMPLETION_SOURCE.find("input + 1").unwrap() + 1;
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument":{"uri":uri},
            "position":position_at(HOVER_COMPLETION_SOURCE, shadowed_use)
        }),
    );
    let hover_text = hover["contents"]["value"].as_str().unwrap();
    assert!(hover_text.contains("**parameter** `input`"), "{hover_text}");
    assert!(hover_text.contains("input: Int"), "{hover_text}");
    assert!(!hover_text.contains("Describes the global input function."));

    let global_declaration =
        HOVER_COMPLETION_SOURCE.find("pub fn input").unwrap() + "pub fn ".len();
    let global_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument":{"uri":uri},
            "position":position_at(HOVER_COMPLETION_SOURCE, global_declaration)
        }),
    );
    assert!(
        global_hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("Describes the global input function.")
    );

    let prefix = HOVER_COMPLETION_SOURCE.rfind("ne\n").unwrap();
    let completion = client.request(
        "textDocument/completion",
        json!({
            "textDocument":{"uri":uri},
            "position":position_at(HOVER_COMPLETION_SOURCE, prefix + 2)
        }),
    );
    let items = completion.as_array().unwrap();
    assert!(!items.is_empty(), "{completion}");
    assert_eq!(items[0]["label"], "nearby");
    assert_eq!(items[0]["kind"], 6);
    assert_eq!(items[0]["preselect"], true);
    assert!(
        items
            .iter()
            .all(|item| item["label"].as_str().unwrap().starts_with("ne")),
        "prefix filtering returned unrelated items: {completion}"
    );
    assert!(
        items[0]["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("let nearby: Int")
    );
    client.shutdown();
}
