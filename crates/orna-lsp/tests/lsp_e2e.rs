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
const EDITOR_HINTS_SOURCE: &str = include_str!("fixtures/editor-lsp-hints.orna");
const RENAME_PROVIDER_SOURCE: &str = include_str!("fixtures/rename-provider-v1.orna");
const RENAME_CALLER_SOURCE: &str = include_str!("fixtures/rename-caller-v1.orna");
const AMBIGUOUS_RENAME_SOURCE: &str = include_str!("fixtures/ambiguous-renames-v1.orna");
const AMBIGUOUS_RENAME_CALLER_SOURCE: &str =
    include_str!("fixtures/ambiguous-renames-caller-v1.orna");

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
    assert_eq!(
        result["capabilities"]["renameProvider"]["prepareProvider"],
        true
    );
    let token_types = result["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .unwrap();
    assert!(
        token_types
            .iter()
            .any(|token_type| token_type == "parameter")
    );
    assert!(
        token_types
            .iter()
            .any(|token_type| token_type == "function")
    );
    let modifiers = result["capabilities"]["semanticTokensProvider"]["legend"]["tokenModifiers"]
        .as_array()
        .unwrap();
    assert!(modifiers.iter().any(|modifier| modifier == "declaration"));
    assert_eq!(
        result["capabilities"]["inlayHintProvider"]["resolveProvider"],
        false
    );
    client.notify("initialized", json!({}));
}

fn open(client: &mut Client, uri: &str, source: &str) -> Value {
    client.notify(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"orna","version":1,"text":source}}),
    );
    client.notification("textDocument/publishDiagnostics")
}

fn decoded_semantic_tokens(source: &str, response: &Value) -> Vec<(String, u64, u64)> {
    let data = response["data"].as_array().unwrap();
    assert_eq!(data.len() % 5, 0, "semantic token data: {response}");
    let mut line = 0usize;
    let mut character = 0usize;
    data.chunks_exact(5)
        .map(|token| {
            let delta_line = token[0].as_u64().unwrap() as usize;
            let delta_start = token[1].as_u64().unwrap() as usize;
            if delta_line == 0 {
                character += delta_start;
            } else {
                line += delta_line;
                character = delta_start;
            }
            let length = token[2].as_u64().unwrap() as usize;
            let line_text = source.lines().nth(line).unwrap();
            (
                line_text[character..character + length].to_owned(),
                token[3].as_u64().unwrap(),
                token[4].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn v1_workspace_model_powers_editor_features_across_open_files() {
    let uri = "file:///workspace/expressions-v1.orna";
    let caller_uri = "file:///workspace/ji3t0-call-v1.orna";
    assert!(uri < caller_uri, "provider URI must sort before caller URI");
    let mut client = Client::spawn();
    initialize(&mut client);
    // The caller attaches before its provider. Workspace analysis must follow
    // the syntax-v1 source dependency order, not didOpen arrival order.
    let caller_diagnostics = open(&mut client, caller_uri, CALL_SOURCE);
    assert!(
        caller_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{caller_diagnostics}"
    );
    let diagnostics = open(&mut client, uri, SOURCE);
    assert!(
        diagnostics["diagnostics"].as_array().unwrap().is_empty(),
        "{diagnostics}"
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
    let references_without_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":uri},
            "position":inner_position,
            "context":{"includeDeclaration":false}
        }),
    );
    assert_eq!(references_without_declaration.as_array().unwrap().len(), 1);

    let prepared = client.request(
        "textDocument/prepareRename",
        json!({"textDocument":{"uri":uri},"position":inner_position}),
    );
    assert_eq!(prepared["placeholder"], "input");
    assert_eq!(
        prepared["range"],
        range_at(
            LOCAL_SCOPES_SOURCE,
            inner_use - 1,
            inner_use - 1 + "input".len()
        )
    );

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

    let invalid_rename = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":uri},"position":inner_position,"newName":"not-valid"}),
    );
    assert!(invalid_rename.is_null(), "{invalid_rename}");
    let non_identifier = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument":{"uri":uri},
            "position":position_of(LOCAL_SCOPES_SOURCE, "1", 0)
        }),
    );
    assert!(non_identifier.is_null(), "{non_identifier}");
    client.shutdown();
}

#[test]
fn references_and_rename_project_utf16_ranges_across_open_files() {
    let provider_uri = "file:///workspace/a-provider.orna";
    let caller_uri = "file:///workspace/z-caller.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let provider_diagnostics = open(&mut client, provider_uri, RENAME_PROVIDER_SOURCE);
    let caller_diagnostics = open(&mut client, caller_uri, RENAME_CALLER_SOURCE);
    assert!(
        provider_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        caller_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let call_start = RENAME_CALLER_SOURCE.find("target(value)").unwrap();
    let call_position = position_at(RENAME_CALLER_SOURCE, call_start + 1);
    let prepared = client.request(
        "textDocument/prepareRename",
        json!({"textDocument":{"uri":caller_uri},"position":call_position}),
    );
    assert_eq!(prepared["placeholder"], "target");
    assert_eq!(
        prepared["range"],
        range_at(
            RENAME_CALLER_SOURCE,
            call_start,
            call_start + "target".len()
        )
    );

    let with_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":caller_uri},
            "position":call_position,
            "context":{"includeDeclaration":true}
        }),
    );
    let locations = with_declaration.as_array().unwrap();
    assert_eq!(locations.len(), 3, "{with_declaration}");
    assert_eq!(locations[0]["uri"], provider_uri);
    let declaration_start = RENAME_PROVIDER_SOURCE.find("pub fn target").unwrap() + "pub fn ".len();
    assert_eq!(
        locations[0]["range"],
        range_at(
            RENAME_PROVIDER_SOURCE,
            declaration_start,
            declaration_start + "target".len()
        )
    );
    assert_eq!(locations[1]["uri"], provider_uri);
    assert_eq!(locations[2]["uri"], caller_uri);

    let without_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":caller_uri},
            "position":call_position,
            "context":{"includeDeclaration":false}
        }),
    );
    assert_eq!(without_declaration.as_array().unwrap().len(), 2);
    assert!(
        without_declaration
            .as_array()
            .unwrap()
            .iter()
            .all(|location| location["range"]["start"] != locations[0]["range"]["start"])
    );

    let renamed = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":caller_uri},"position":call_position,"newName":"renamed_target"}),
    );
    let provider_edits = renamed["changes"][provider_uri].as_array().unwrap();
    let caller_edits = renamed["changes"][caller_uri].as_array().unwrap();
    assert_eq!(provider_edits.len(), 2, "{renamed}");
    assert_eq!(caller_edits.len(), 1, "{renamed}");
    assert!(
        provider_edits
            .iter()
            .chain(caller_edits)
            .all(|edit| edit["newText"] == "renamed_target")
    );

    for rejected_name in ["not-valid", "let", "remote"] {
        let rejected = client.request(
            "textDocument/rename",
            json!({"textDocument":{"uri":caller_uri},"position":call_position,"newName":rejected_name}),
        );
        assert!(rejected.is_null(), "rename to {rejected_name}: {rejected}");
    }
    client.shutdown();
}

#[test]
fn ambiguous_global_names_fail_closed_for_definition_references_and_rename() {
    let first_uri = "file:///workspace/ambiguous-first.orna";
    let second_uri = "file:///workspace/ambiguous-second.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let first_diagnostics = open(&mut client, first_uri, AMBIGUOUS_RENAME_SOURCE);
    let second_diagnostics = open(&mut client, second_uri, AMBIGUOUS_RENAME_CALLER_SOURCE);
    assert!(
        first_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        second_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let call_start = AMBIGUOUS_RENAME_SOURCE.find("shared(value)").unwrap();
    let position = position_at(AMBIGUOUS_RENAME_SOURCE, call_start + 1);
    let definition = client.request(
        "textDocument/definition",
        json!({"textDocument":{"uri":first_uri},"position":position}),
    );
    assert!(definition.is_null(), "{definition}");
    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":first_uri},
            "position":position,
            "context":{"includeDeclaration":true}
        }),
    );
    assert_eq!(references, json!([]));
    let prepared = client.request(
        "textDocument/prepareRename",
        json!({"textDocument":{"uri":first_uri},"position":position}),
    );
    assert!(prepared.is_null(), "{prepared}");
    let renamed = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":first_uri},"position":position,"newName":"unique"}),
    );
    assert!(renamed.is_null(), "{renamed}");
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

#[test]
fn syntax_v1_semantic_tokens_and_inlay_hints_follow_scope_and_requested_range() {
    let uri = "file:///workspace/editor-lsp-hints.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, EDITOR_HINTS_SOURCE);
    assert!(
        diagnostics["diagnostics"].as_array().unwrap().is_empty(),
        "{diagnostics}"
    );

    let full = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":uri}}),
    );
    let tokens = decoded_semantic_tokens(EDITOR_HINTS_SOURCE, &full);
    assert!(tokens.contains(&("User".to_owned(), 6, 3)), "{tokens:?}");
    assert!(tokens.contains(&("add".to_owned(), 9, 3)), "{tokens:?}");
    assert!(tokens.contains(&("add".to_owned(), 9, 2)), "{tokens:?}");
    assert!(tokens.contains(&("left".to_owned(), 10, 1)), "{tokens:?}");
    assert!(tokens.contains(&("add".to_owned(), 10, 0)), "{tokens:?}");
    assert!(
        tokens.contains(&("inferred".to_owned(), 1, 1)),
        "{tokens:?}"
    );

    let function_start = EDITOR_HINTS_SOURCE.find("pub fn add").unwrap();
    let function_end = EDITOR_HINTS_SOURCE.find("pub fn caller").unwrap();
    let ranged = client.request(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument":{"uri":uri},
            "range":range_at(EDITOR_HINTS_SOURCE, function_start, function_end)
        }),
    );
    let ranged_tokens = decoded_semantic_tokens(EDITOR_HINTS_SOURCE, &ranged);
    assert!(
        ranged_tokens.contains(&("add".to_owned(), 9, 3)),
        "{ranged_tokens:?}"
    );
    assert!(!ranged_tokens.iter().any(|(word, _, _)| word == "User"));

    let full_hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument":{"uri":uri},
            "range":range_at(EDITOR_HINTS_SOURCE, 0, EDITOR_HINTS_SOURCE.len())
        }),
    );
    let hints = full_hints.as_array().unwrap();
    let labels = hints
        .iter()
        .map(|hint| hint["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(labels.contains(&": Int"), "{full_hints}");
    assert!(labels.contains(&"left: "), "{full_hints}");
    assert!(labels.contains(&"right: "), "{full_hints}");

    let call_start = EDITOR_HINTS_SOURCE.find("add(inferred").unwrap();
    let call_end = EDITOR_HINTS_SOURCE[call_start..]
        .find(';')
        .map(|offset| call_start + offset + 1)
        .unwrap();
    let call_hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument":{"uri":uri},
            "range":range_at(EDITOR_HINTS_SOURCE, call_start, call_end)
        }),
    );
    assert_eq!(
        call_hints
            .as_array()
            .unwrap()
            .iter()
            .map(|hint| hint["label"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["left: ", "right: "]
    );

    let local_start = EDITOR_HINTS_SOURCE.find("let inferred").unwrap();
    let local_end = EDITOR_HINTS_SOURCE[local_start..]
        .find(';')
        .map(|offset| local_start + offset + 1)
        .unwrap();
    let local_hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument":{"uri":uri},
            "range":range_at(EDITOR_HINTS_SOURCE, local_start, local_end)
        }),
    );
    assert_eq!(local_hints.as_array().unwrap().len(), 1, "{local_hints}");
    assert_eq!(local_hints[0]["label"], ": Int");

    let annotated_start = EDITOR_HINTS_SOURCE.find("let annotated").unwrap();
    let annotated_end = EDITOR_HINTS_SOURCE[annotated_start..]
        .find(';')
        .map(|offset| annotated_start + offset + 1)
        .unwrap();
    let annotated_hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument":{"uri":uri},
            "range":range_at(EDITOR_HINTS_SOURCE, annotated_start, annotated_end)
        }),
    );
    assert!(
        annotated_hints.as_array().unwrap().is_empty(),
        "{annotated_hints}"
    );

    let shadow_call_start = EDITOR_HINTS_SOURCE.find("add(1)").unwrap();
    let shadow_call_end = shadow_call_start + "add(1)".len();
    let shadow_hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument":{"uri":uri},
            "range":range_at(EDITOR_HINTS_SOURCE, shadow_call_start, shadow_call_end)
        }),
    );
    assert!(
        shadow_hints.as_array().unwrap().is_empty(),
        "{shadow_hints}"
    );
    client.shutdown();
}
