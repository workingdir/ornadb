//! Protocol proofs for the Orna 1.0 language server.

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

#[path = "support/syntax_v1_depth_contract.rs"]
mod syntax_v1_depth_contract;

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
const SIGNATURE_ACTIONS_SOURCE: &str = include_str!("fixtures/signature-actions-v1.orna");
const MISSING_SEMICOLON_SOURCE: &str =
    include_str!("fixtures/missing-semicolon-code-action-v1.orna");
const DOCUMENT_LINKS_SOURCE: &str = include_str!("fixtures/document-links-v1.orna");
const DOCUMENT_LINK_TARGET_SOURCE: &str = include_str!("fixtures/document-link-target-v1.orna");
const WORKSPACE_HIERARCHY_PROVIDER_SOURCE: &str =
    include_str!("fixtures/workspace-hierarchy-provider-v1.orna");
const WORKSPACE_HIERARCHY_CALLER_SOURCE: &str =
    include_str!("fixtures/workspace-hierarchy-caller-v1.orna");
const FOLDING_SELECTION_SOURCE: &str = include_str!("fixtures/folding-selection-v1.orna");
const DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE: &str =
    include_str!("fixtures/document-highlight-code-lens-v1.orna");

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
        result["capabilities"]["documentHighlightProvider"], true,
        "initialize capabilities: {result}"
    );
    assert_eq!(
        result["capabilities"]["codeLensProvider"]["resolveProvider"],
        false
    );
    assert_eq!(
        result["capabilities"]["renameProvider"]["prepareProvider"], true,
        "initialize capabilities: {result}"
    );
    assert_ne!(
        result["capabilities"]["codeActionProvider"]["resolveProvider"], true,
        "server must not advertise code-action resolution: {result}"
    );
    assert_eq!(
        result["capabilities"]["codeActionProvider"]["codeActionKinds"],
        json!(["quickfix"]),
        "initialize capabilities: {result}"
    );
    let signature_triggers = result["capabilities"]["signatureHelpProvider"]["triggerCharacters"]
        .as_array()
        .unwrap();
    assert!(signature_triggers.iter().any(|trigger| trigger == "("));
    assert!(signature_triggers.iter().any(|trigger| trigger == ","));
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
    assert_eq!(
        result["capabilities"]["diagnosticProvider"]["identifier"],
        "orna-syntax-v1"
    );
    assert_eq!(
        result["capabilities"]["documentLinkProvider"]["resolveProvider"],
        false
    );
    assert_eq!(result["capabilities"]["foldingRangeProvider"], true);
    assert_eq!(result["capabilities"]["selectionRangeProvider"], true);
    let call_hierarchy = &result["capabilities"]["callHierarchyProvider"];
    assert!(
        call_hierarchy.as_bool() == Some(true) || call_hierarchy.is_object(),
        "call hierarchy capability was not enabled: {result}"
    );
    client.notify("initialized", json!({}));
}

#[test]
fn document_highlights_classify_reads_writes_and_respect_shadowing() {
    let uri = "file:///workspace/document-highlight-code-lens-v1.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE);
    assert!(
        diagnostics["diagnostics"].as_array().unwrap().is_empty(),
        "{diagnostics}"
    );

    let count_cursor = position_of(DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE, "count =", 1);
    let count_highlights = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument":{"uri":uri},
            "position":count_cursor
        }),
    );
    let count_highlights = count_highlights.as_array().unwrap();
    assert_eq!(
        count_highlights.len(),
        5,
        "count highlights: {count_highlights:?}"
    );
    assert_eq!(
        count_highlights
            .iter()
            .filter(|highlight| highlight["kind"] == 3)
            .count(),
        2,
        "declaration and assignment target are writes: {count_highlights:?}"
    );
    assert_eq!(
        count_highlights
            .iter()
            .filter(|highlight| highlight["kind"] == 2)
            .count(),
        3,
        "value reads: {count_highlights:?}"
    );
    let count_ranges = DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE
        .match_indices("count")
        .map(|(start, name)| {
            range_at(
                DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
                start,
                start + name.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        count_highlights
            .iter()
            .map(|highlight| highlight["range"].clone())
            .collect::<Vec<_>>(),
        count_ranges
    );

    let first_item = DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE
        .find("let item")
        .unwrap()
        + "let ".len();
    let first_item_highlights = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument":{"uri":uri},
            "position":position_at(DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE, first_item + 1)
        }),
    );
    assert_eq!(
        first_item_highlights.as_array().unwrap().len(),
        2,
        "the first branch binding must not capture the second branch: {first_item_highlights:?}"
    );
    let first_item_reference = DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE[first_item + "item".len()..]
        .find("item")
        .map(|offset| first_item + "item".len() + offset)
        .unwrap();
    assert_eq!(
        first_item_highlights
            .as_array()
            .unwrap()
            .iter()
            .map(|highlight| highlight["range"].clone())
            .collect::<Vec<_>>(),
        vec![
            range_at(
                DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
                first_item,
                first_item + "item".len()
            ),
            range_at(
                DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
                first_item_reference,
                first_item_reference + "item".len()
            ),
        ]
    );

    let twice_declaration = DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE.find("twice").unwrap();
    let twice_highlights = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument":{"uri":uri},
            "position":position_at(DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE, twice_declaration + 1)
        }),
    );
    assert_eq!(twice_highlights.as_array().unwrap().len(), 3);
    assert_eq!(
        twice_highlights
            .as_array()
            .unwrap()
            .iter()
            .filter(|highlight| highlight["kind"] == 3)
            .count(),
        1,
        "function declaration is a write: {twice_highlights:?}"
    );
    client.shutdown();
}

#[test]
fn code_lenses_count_only_uniquely_resolved_workspace_calls() {
    let provider_uri = "file:///workspace/workspace-hierarchy-provider-v1.orna";
    let caller_uri = "file:///workspace/workspace-hierarchy-caller-v1.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    open(&mut client, caller_uri, WORKSPACE_HIERARCHY_CALLER_SOURCE);
    open(
        &mut client,
        provider_uri,
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
    );

    let lenses = client.request(
        "textDocument/codeLens",
        json!({"textDocument":{"uri":provider_uri}}),
    );
    let lenses = lenses.as_array().unwrap();
    let lens_for = |name: &str| {
        let start = WORKSPACE_HIERARCHY_PROVIDER_SOURCE.find(name).unwrap();
        lenses
            .iter()
            .find(|lens| {
                lens["range"]
                    == range_at(
                        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
                        start,
                        start + name.len(),
                    )
            })
            .unwrap_or_else(|| panic!("missing code lens for {name}: {lenses:?}"))
    };

    let seed = lens_for("seed");
    assert_eq!(seed["command"]["title"], "3 incoming calls");
    assert_eq!(seed["command"]["command"], "editor.action.showReferences");
    assert_eq!(seed["command"]["arguments"][0], provider_uri);
    assert_eq!(seed["command"]["arguments"][2].as_array().unwrap().len(), 3);
    assert!(
        seed["command"]["arguments"][2]
            .as_array()
            .unwrap()
            .iter()
            .all(|location| location["uri"] == provider_uri)
    );

    assert_eq!(lens_for("mid")["command"]["title"], "2 incoming calls");
    let root = lens_for("root");
    assert_eq!(root["command"]["title"], "2 incoming calls");
    assert!(
        root["command"]["arguments"][2]
            .as_array()
            .unwrap()
            .iter()
            .any(|location| location["uri"] == caller_uri)
    );
    assert_eq!(lens_for("shadowed")["command"]["title"], "0 incoming calls");
    assert_eq!(lens_for("isolated")["command"]["title"], "0 incoming calls");
    assert_eq!(
        lens_for("unresolved")["command"]["title"],
        "0 incoming calls"
    );
    assert_eq!(lens_for("collide")["command"]["title"], "0 incoming calls");

    let caller_lenses = client.request(
        "textDocument/codeLens",
        json!({"textDocument":{"uri":caller_uri}}),
    );
    let ambiguous = caller_lenses
        .as_array()
        .unwrap()
        .iter()
        .find(|lens| {
            lens["range"]
                == range_at(
                    WORKSPACE_HIERARCHY_CALLER_SOURCE,
                    WORKSPACE_HIERARCHY_CALLER_SOURCE
                        .find("ambiguous_call")
                        .unwrap(),
                    WORKSPACE_HIERARCHY_CALLER_SOURCE
                        .find("ambiguous_call")
                        .unwrap()
                        + "ambiguous_call".len(),
                )
        })
        .unwrap();
    assert_eq!(ambiguous["command"]["title"], "0 incoming calls");
    client.shutdown();
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
fn pull_diagnostics_reuse_results_and_refresh_after_versioned_changes() {
    let uri = "file:///workspace/incremental.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let published = open(&mut client, uri, INCREMENTAL_SOURCE);

    let first = client.request(
        "textDocument/diagnostic",
        json!({"textDocument":{"uri":uri}}),
    );
    assert_eq!(first["kind"], "full", "{first}");
    assert_eq!(first["items"], published["diagnostics"]);
    let first_id = first["resultId"].as_str().expect("pull result ID");
    assert!(!first_id.is_empty());

    let unchanged = client.request(
        "textDocument/diagnostic",
        json!({
            "textDocument":{"uri":uri},
            "previousResultId":first_id
        }),
    );
    assert_eq!(unchanged["kind"], "unchanged", "{unchanged}");
    assert_eq!(unchanged["resultId"], first_id);

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument":{"uri":uri,"version":2},
            "contentChanges":[{"text":DOCUMENT_LINKS_SOURCE}]
        }),
    );
    let updated = client.notification("textDocument/publishDiagnostics");
    assert_eq!(updated["version"], 2);
    assert!(updated["diagnostics"].as_array().unwrap().is_empty());

    let refreshed = client.request(
        "textDocument/diagnostic",
        json!({
            "textDocument":{"uri":uri},
            "previousResultId":first_id
        }),
    );
    assert_eq!(refreshed["kind"], "full", "{refreshed}");
    assert!(refreshed["items"].as_array().unwrap().is_empty());
    let refreshed_id = refreshed["resultId"].as_str().expect("new pull result ID");
    assert_ne!(refreshed_id, first_id);

    let still_current = client.request(
        "textDocument/diagnostic",
        json!({
            "textDocument":{"uri":uri},
            "previousResultId":refreshed_id
        }),
    );
    assert_eq!(still_current["kind"], "unchanged", "{still_current}");
    client.shutdown();
}

#[test]
fn document_links_resolve_open_imports_and_suppress_missing_or_ambiguous_targets() {
    let source_uri = "file:///workspace/main.orna";
    let flat_target_uri = "file:///workspace/library/math.orna";
    let directory_target_uri = "file:///workspace/library/math/main.orna";
    let mut client = Client::spawn();
    initialize(&mut client);

    let source_diagnostics = open(&mut client, source_uri, DOCUMENT_LINKS_SOURCE);
    assert!(
        source_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let no_open_targets = client.request(
        "textDocument/documentLink",
        json!({"textDocument":{"uri":source_uri}}),
    );
    assert!(
        no_open_targets.as_array().unwrap().is_empty(),
        "{no_open_targets}"
    );

    let target_diagnostics = open(&mut client, flat_target_uri, DOCUMENT_LINK_TARGET_SOURCE);
    assert!(
        target_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let links = client.request(
        "textDocument/documentLink",
        json!({"textDocument":{"uri":source_uri}}),
    );
    assert_eq!(links.as_array().unwrap().len(), 1, "{links}");
    assert_eq!(
        links[0]["range"],
        range_at(
            DOCUMENT_LINKS_SOURCE,
            DOCUMENT_LINKS_SOURCE.find("library.math").unwrap(),
            DOCUMENT_LINKS_SOURCE.find("library.math").unwrap() + "library.math".len()
        )
    );
    assert_eq!(links[0]["target"], flat_target_uri);
    assert_eq!(links[0]["tooltip"], "Open module `library.math`");

    let duplicate_diagnostics = open(
        &mut client,
        directory_target_uri,
        DOCUMENT_LINK_TARGET_SOURCE,
    );
    assert!(
        duplicate_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let ambiguous_links = client.request(
        "textDocument/documentLink",
        json!({"textDocument":{"uri":source_uri}}),
    );
    assert!(
        ambiguous_links.as_array().unwrap().is_empty(),
        "{ambiguous_links}"
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
fn signature_help_tracks_nested_arguments_and_named_parameter_indices() {
    let uri = "file:///workspace/signature-actions.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, SIGNATURE_ACTIONS_SOURCE);
    assert!(diagnostics["diagnostics"].as_array().unwrap().is_empty());

    let tuple_value = SIGNATURE_ACTIONS_SOURCE.find("(1, 2)").unwrap() + "(1, ".len();
    let nested_argument = client.request(
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":uri},"position":position_at(SIGNATURE_ACTIONS_SOURCE,tuple_value)}),
    );
    assert!(
        nested_argument["signatures"][0]["label"]
            .as_str()
            .unwrap()
            .contains("fn wrap(")
    );
    assert_eq!(nested_argument["activeParameter"], 0, "{nested_argument}");

    let last_value = SIGNATURE_ACTIONS_SOURCE.find("last: 3").unwrap() + "last: ".len();
    let named_argument = client.request(
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":uri},"position":position_at(SIGNATURE_ACTIONS_SOURCE,last_value)}),
    );
    assert!(
        named_argument["signatures"][0]["label"]
            .as_str()
            .unwrap()
            .contains("fn wrap(")
    );
    assert_eq!(named_argument["activeParameter"], 2, "{named_argument}");

    let extra_value = SIGNATURE_ACTIONS_SOURCE.find("extra: value").unwrap() + "extra: ".len();
    let nested_named_argument = client.request(
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":uri},"position":position_at(SIGNATURE_ACTIONS_SOURCE,extra_value)}),
    );
    assert!(
        nested_named_argument["signatures"][0]["label"]
            .as_str()
            .unwrap()
            .contains("fn add(")
    );
    assert_eq!(
        nested_named_argument["activeParameter"], 2,
        "{nested_named_argument}"
    );

    let shadow_call = SIGNATURE_ACTIONS_SOURCE.find("add(1)").unwrap() + 1;
    let shadowed_signature = client.request(
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":uri},"position":position_at(SIGNATURE_ACTIONS_SOURCE,shadow_call)}),
    );
    assert!(shadowed_signature.is_null(), "{shadowed_signature}");
    client.shutdown();
}

#[test]
fn code_actions_offer_only_verified_missing_semicolon_fixes() {
    let uri = "file:///workspace/missing-semicolon.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let published = open(&mut client, uri, MISSING_SEMICOLON_SOURCE);
    let diagnostics = published["diagnostics"].as_array().unwrap();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic["code"] == "ORNA-PARSE-002")
            .count(),
        1,
        "{published}"
    );

    let params = json!({
        "textDocument":{"uri":uri},
        "range":range_at(MISSING_SEMICOLON_SOURCE, 0, MISSING_SEMICOLON_SOURCE.len()),
        "context":{"diagnostics":diagnostics,"only":["quickfix"]}
    });
    let actions = client.request("textDocument/codeAction", params.clone());
    assert_eq!(actions.as_array().unwrap().len(), 1, "{actions}");
    let action = &actions[0];
    assert_eq!(action["title"], "Insert missing `;`");
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["isPreferred"], true);
    assert_eq!(action["diagnostics"][0]["code"], "ORNA-PARSE-002");
    let edit = &action["edit"]["changes"][uri][0];
    assert_eq!(edit["newText"], ";");

    let insertion = MISSING_SEMICOLON_SOURCE
        .find("value\n    intermediate")
        .unwrap()
        + "value".len();
    assert_eq!(
        edit["range"],
        range_at(MISSING_SEMICOLON_SOURCE, insertion, insertion)
    );
    let repaired = format!(
        "{};{}",
        &MISSING_SEMICOLON_SOURCE[..insertion],
        &MISSING_SEMICOLON_SOURCE[insertion..]
    );
    assert!(
        orna_syntax_v1::parse_module(&repaired)
            .diagnostics
            .is_empty()
    );

    let mut non_quickfix_params = params.clone();
    non_quickfix_params["context"]["only"] = json!(["refactor"]);
    assert_eq!(
        client.request("textDocument/codeAction", non_quickfix_params),
        json!([])
    );
    let outside_diagnostic = json!({
        "textDocument":{"uri":uri},
        "range":range_at(MISSING_SEMICOLON_SOURCE, 0, 1),
        "context":{"diagnostics":diagnostics,"only":["quickfix"]}
    });
    assert_eq!(
        client.request("textDocument/codeAction", outside_diagnostic),
        json!([])
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
    syntax_v1_depth_contract::assert_semantic_depth_contract(
        EDITOR_HINTS_SOURCE,
        &full,
        &ranged,
        "LSP protocol",
    );
    syntax_v1_depth_contract::assert_inlay_hint_depth_contract(
        EDITOR_HINTS_SOURCE,
        &full_hints,
        &call_hints,
        &local_hints,
        &annotated_hints,
        &shadow_hints,
        "LSP protocol",
    );
    client.shutdown();
}

#[test]
fn workspace_symbols_rank_stably_and_call_hierarchy_tracks_resolved_calls() {
    let provider_uri = "file:///workspace/hierarchy/provider.orna";
    let caller_uri = "file:///workspace/hierarchy/caller.orna";
    let mut client = Client::spawn();
    initialize(&mut client);

    let provider_diagnostics = open(
        &mut client,
        provider_uri,
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
    );
    assert!(
        provider_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{provider_diagnostics}"
    );
    let caller_diagnostics = open(&mut client, caller_uri, WORKSPACE_HIERARCHY_CALLER_SOURCE);
    assert!(
        caller_diagnostics["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{caller_diagnostics}"
    );

    let ranked = client.request("workspace/symbol", json!({"query":"mid"}));
    let ranked_names = ranked
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ranked_names, ["mid", "remote_mid"], "{ranked}");
    assert_eq!(ranked[0]["containerName"], Value::Null);
    assert_eq!(
        client.request("workspace/symbol", json!({"query":"mid"})),
        ranked,
        "workspace symbol order changed between identical requests"
    );

    let prefix_ranked = client.request("workspace/symbol", json!({"query":"rem"}));
    assert_eq!(
        prefix_ranked
            .as_array()
            .unwrap()
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["remote_mid", "remote_root"],
        "prefix workspace symbol ranking: {prefix_ranked}"
    );

    let fuzzy = client.request("workspace/symbol", json!({"query":"remi"}));
    assert_eq!(
        fuzzy
            .as_array()
            .unwrap()
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["remote_mid"],
        "fuzzy workspace symbol results: {fuzzy}"
    );

    let root_definition = WORKSPACE_HIERARCHY_PROVIDER_SOURCE
        .find("pub fn root")
        .unwrap();
    let root_cursor = root_definition + "pub fn ".len();
    let root_item = client.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument":{"uri":provider_uri},
            "position":position_at(WORKSPACE_HIERARCHY_PROVIDER_SOURCE, root_cursor)
        }),
    );
    assert_eq!(root_item.as_array().unwrap().len(), 1, "{root_item}");
    assert_eq!(root_item[0]["name"], "root");
    assert_eq!(root_item[0]["uri"], provider_uri);
    assert_eq!(
        root_item[0]["selectionRange"],
        range_at(
            WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
            root_cursor,
            root_cursor + "root".len()
        )
    );

    let outgoing = client.request("callHierarchy/outgoingCalls", json!({"item":root_item[0]}));
    let outgoing_pairs = outgoing
        .as_array()
        .unwrap()
        .iter()
        .map(|call| {
            (
                call["to"]["name"].as_str().unwrap(),
                call["fromRanges"].as_array().unwrap().len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(outgoing_pairs, [("seed", 2), ("mid", 1)], "{outgoing}");
    let seed_calls = [
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE
            .find("mid(seed(value))")
            .map(|offset| offset + "mid(".len())
            .unwrap(),
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE
            .find("+ seed(value)")
            .map(|offset| offset + "+ ".len())
            .unwrap(),
    ];
    assert_eq!(
        outgoing[0]["fromRanges"],
        json!([
            range_at(
                WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
                seed_calls[0],
                seed_calls[0] + "seed".len()
            ),
            range_at(
                WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
                seed_calls[1],
                seed_calls[1] + "seed".len()
            )
        ])
    );

    let incoming = client.request("callHierarchy/incomingCalls", json!({"item":root_item[0]}));
    assert_eq!(incoming.as_array().unwrap().len(), 2, "{incoming}");
    assert_eq!(incoming[0]["from"]["name"], "remote_root");
    assert_eq!(incoming[1]["from"]["name"], "remote_mid");
    assert_eq!(
        incoming[0]["fromRanges"][0],
        range_at(
            WORKSPACE_HIERARCHY_CALLER_SOURCE,
            WORKSPACE_HIERARCHY_CALLER_SOURCE
                .find("root(value)")
                .unwrap(),
            WORKSPACE_HIERARCHY_CALLER_SOURCE
                .find("root(value)")
                .unwrap()
                + "root".len()
        )
    );

    let seed_cursor = WORKSPACE_HIERARCHY_PROVIDER_SOURCE
        .find("pub fn seed")
        .map(|offset| offset + "pub fn ".len())
        .unwrap();
    let seed_item = client.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument":{"uri":provider_uri},
            "position":position_at(WORKSPACE_HIERARCHY_PROVIDER_SOURCE, seed_cursor)
        }),
    );
    let seed_incoming = client.request("callHierarchy/incomingCalls", json!({"item":seed_item[0]}));
    assert_eq!(
        seed_incoming.as_array().unwrap().len(),
        2,
        "{seed_incoming}"
    );
    assert_eq!(seed_incoming[0]["from"]["name"], "mid");
    assert_eq!(seed_incoming[1]["from"]["name"], "root");
    assert_eq!(seed_incoming[1]["fromRanges"].as_array().unwrap().len(), 2);

    let shadow_cursor = WORKSPACE_HIERARCHY_PROVIDER_SOURCE
        .find("pub fn shadowed")
        .map(|offset| offset + "pub fn shadowed".len())
        .unwrap();
    let shadow_item = client.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument":{"uri":provider_uri},
            "position":position_at(WORKSPACE_HIERARCHY_PROVIDER_SOURCE, shadow_cursor)
        }),
    );
    assert_eq!(shadow_item[0]["name"], "shadowed");
    assert!(
        client
            .request(
                "callHierarchy/outgoingCalls",
                json!({"item":shadow_item[0]}),
            )
            .as_array()
            .unwrap()
            .is_empty(),
        "local parameter call was resolved as a workspace function"
    );

    let unresolved_cursor = WORKSPACE_HIERARCHY_PROVIDER_SOURCE
        .find("pub fn unresolved")
        .map(|offset| offset + "pub fn unresolved".len())
        .unwrap();
    let unresolved_item = client.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument":{"uri":provider_uri},
            "position":position_at(WORKSPACE_HIERARCHY_PROVIDER_SOURCE, unresolved_cursor)
        }),
    );
    assert!(
        client
            .request(
                "callHierarchy/outgoingCalls",
                json!({"item":unresolved_item[0]}),
            )
            .as_array()
            .unwrap()
            .is_empty(),
        "unresolved call was emitted in the hierarchy"
    );

    let reference_cursor = WORKSPACE_HIERARCHY_CALLER_SOURCE
        .find("root(value)")
        .unwrap();
    let prepared_reference = client.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument":{"uri":caller_uri},
            "position":position_at(WORKSPACE_HIERARCHY_CALLER_SOURCE, reference_cursor)
        }),
    );
    assert_eq!(prepared_reference[0]["name"], "root");
    assert_eq!(prepared_reference[0]["uri"], provider_uri);

    let ambiguous_cursor = WORKSPACE_HIERARCHY_CALLER_SOURCE
        .find("collide(value)")
        .unwrap();
    let ambiguous_reference = client.request(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument":{"uri":caller_uri},
            "position":position_at(WORKSPACE_HIERARCHY_CALLER_SOURCE, ambiguous_cursor)
        }),
    );
    assert!(
        ambiguous_reference.is_null(),
        "ambiguous function reference unexpectedly resolved: {ambiguous_reference}"
    );
    client.shutdown();
}

fn selection_chain(selection: &Value) -> Vec<Value> {
    let mut chain = Vec::new();
    let mut current = selection;
    loop {
        chain.push(current.clone());
        let Some(parent) = current.get("parent") else {
            break;
        };
        current = parent;
    }
    chain
}

fn folding_range_at(source: &str, start: usize, end: usize, kind: &str) -> Value {
    let start = position_at(source, start);
    let end = position_at(source, end);
    json!({
        "startLine": start["line"],
        "startCharacter": start["character"],
        "endLine": end["line"],
        "endCharacter": end["character"],
        "kind": kind,
    })
}

#[test]
fn folding_and_selection_ranges_follow_syntax_and_preserve_utf16_positions() {
    let uri = "file:///workspace/folding-selection.orna";
    let mut client = Client::spawn();
    initialize(&mut client);
    let diagnostics = open(&mut client, uri, FOLDING_SELECTION_SOURCE);
    assert!(
        diagnostics["diagnostics"].as_array().unwrap().is_empty(),
        "{diagnostics}"
    );

    let folds = client.request(
        "textDocument/foldingRange",
        json!({"textDocument":{"uri":uri}}),
    );
    let folds = folds.as_array().unwrap();
    assert!(!folds.is_empty(), "no folding ranges were returned");
    let mut previous = None;
    let mut unique = std::collections::BTreeSet::new();
    for fold in folds {
        let key = (
            fold["startLine"].as_u64().unwrap(),
            fold["startCharacter"].as_u64().unwrap(),
            fold["endLine"].as_u64().unwrap(),
            fold["endCharacter"].as_u64().unwrap(),
        );
        assert!(key.0 < key.2, "single-line fold: {fold}");
        assert!(
            previous.is_none_or(|previous| previous <= key),
            "unsorted folds: {folds:?}"
        );
        assert!(unique.insert(key), "duplicate folding coordinates: {fold}");
        previous = Some(key);
    }
    let comment_start = FOLDING_SELECTION_SOURCE.find("/* range docs").unwrap();
    let comment_end = FOLDING_SELECTION_SOURCE.find("*/").unwrap() + 2;
    let comment_fold = folds
        .iter()
        .find(|fold| fold["kind"] == "comment")
        .expect("multi-line comment folding range");
    assert_eq!(
        comment_fold,
        &folding_range_at(
            FOLDING_SELECTION_SOURCE,
            comment_start,
            comment_end,
            "comment"
        )
    );
    let import_start = FOLDING_SELECTION_SOURCE.find("use std.math.{abs}").unwrap();
    let import_end =
        FOLDING_SELECTION_SOURCE.find("use std.math.{min}").unwrap() + "use std.math.{min};".len();
    let import_fold = folds
        .iter()
        .find(|fold| fold["kind"] == "imports")
        .expect("contiguous import folding range");
    assert_eq!(
        import_fold,
        &folding_range_at(
            FOLDING_SELECTION_SOURCE,
            import_start,
            import_end,
            "imports"
        )
    );
    let line_comment_start = FOLDING_SELECTION_SOURCE.find("// fold together").unwrap();
    let line_comment_end = FOLDING_SELECTION_SOURCE
        .find("// contiguous comments")
        .unwrap()
        + "// contiguous comments".len();
    let line_comment_fold = folds
        .iter()
        .find(|fold| {
            fold["kind"] == "comment"
                && fold["startLine"]
                    == position_at(FOLDING_SELECTION_SOURCE, line_comment_start)["line"]
        })
        .expect("contiguous line-comment folding range");
    assert_eq!(
        line_comment_fold,
        &folding_range_at(
            FOLDING_SELECTION_SOURCE,
            line_comment_start,
            line_comment_end,
            "comment",
        )
    );
    let control_start = FOLDING_SELECTION_SOURCE.find("if total > 0").unwrap();
    assert!(
        folds.iter().any(|fold| {
            fold["startLine"] == position_at(FOLDING_SELECTION_SOURCE, control_start)["line"]
                && fold["endLine"]
                    == position_at(
                        FOLDING_SELECTION_SOURCE,
                        FOLDING_SELECTION_SOURCE
                            .find("\n    }\n}\n\npub enum")
                            .unwrap()
                            + 6,
                    )["line"]
        }),
        "control body folding range missing: {folds:?}"
    );
    let outer_start = FOLDING_SELECTION_SOURCE.find("pub fn outer").unwrap();
    let outer_end = FOLDING_SELECTION_SOURCE
        .find("\n}\n\npub enum Outcome")
        .unwrap()
        + 2;
    assert!(
        folds.iter().any(|fold| {
            fold["startLine"] == position_at(FOLDING_SELECTION_SOURCE, outer_start)["line"]
                && fold["startCharacter"]
                    == position_at(FOLDING_SELECTION_SOURCE, outer_start)["character"]
                && fold["endLine"] == position_at(FOLDING_SELECTION_SOURCE, outer_end)["line"]
        }),
        "top-level function folding range missing: {folds:?}"
    );
    let enum_start = FOLDING_SELECTION_SOURCE.find("pub enum Outcome").unwrap();
    let enum_end = FOLDING_SELECTION_SOURCE
        .find("failed { reason: Str },")
        .unwrap()
        + "failed { reason: Str },\n}".len();
    assert!(
        folds.iter().any(|fold| {
            fold["startLine"] == position_at(FOLDING_SELECTION_SOURCE, enum_start)["line"]
                && fold["startCharacter"]
                    == position_at(FOLDING_SELECTION_SOURCE, enum_start)["character"]
                && fold["endLine"] == position_at(FOLDING_SELECTION_SOURCE, enum_end)["line"]
        }),
        "enum folding range missing: {folds:?}"
    );

    let value_start = FOLDING_SELECTION_SOURCE.find("abs(value)").unwrap() + "abs(".len();
    let value_end = value_start + "value".len();
    let compass_start = FOLDING_SELECTION_SOURCE.find("\"🧭\"").unwrap();
    let compass_end = compass_start + "\"🧭\"".len();
    let marker_start = FOLDING_SELECTION_SOURCE
        .find("\"/* string content, never a comment */\"")
        .unwrap();
    let marker_end = marker_start + "\"/* string content, never a comment */\"".len();
    let blank_line = FOLDING_SELECTION_SOURCE.find("\n\npub fn compass").unwrap() + 1;
    let selections = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument":{"uri":uri},
            "positions":[
                position_at(FOLDING_SELECTION_SOURCE, value_start + 2),
                position_at(FOLDING_SELECTION_SOURCE, compass_end - 1),
                position_of(FOLDING_SELECTION_SOURCE, "compass 🧭 remains", "compass ".len()),
                position_of(FOLDING_SELECTION_SOURCE, "string content", "string ".len()),
                position_at(FOLDING_SELECTION_SOURCE, blank_line),
            ]
        }),
    );
    let selections = selections.as_array().unwrap();
    assert_eq!(selections.len(), 5, "{selections:?}");
    let value_chain = selection_chain(&selections[0]);
    assert_eq!(
        value_chain[0]["range"],
        range_at(FOLDING_SELECTION_SOURCE, value_start, value_end)
    );
    let call_start = FOLDING_SELECTION_SOURCE.find("abs(value)").unwrap();
    let call_end = call_start + "abs(value)".len();
    assert_eq!(
        value_chain[1]["range"],
        range_at(FOLDING_SELECTION_SOURCE, call_start, call_end)
    );
    let statement_start = FOLDING_SELECTION_SOURCE.find("let total =").unwrap();
    let statement_end = statement_start + "let total = abs(value)".len();
    assert_eq!(
        value_chain[2]["range"],
        range_at(FOLDING_SELECTION_SOURCE, statement_start, statement_end)
    );
    assert!(
        value_chain.len() >= 5,
        "selection ancestry must include syntax parents through the document: {value_chain:?}"
    );
    assert_eq!(
        value_chain.last().unwrap()["range"],
        range_at(FOLDING_SELECTION_SOURCE, 0, FOLDING_SELECTION_SOURCE.len())
    );

    let compass_chain = selection_chain(&selections[1]);
    assert_eq!(
        compass_chain[0]["range"],
        range_at(FOLDING_SELECTION_SOURCE, compass_start, compass_end),
        "UTF-16 cursor after supplementary character should select the string token"
    );
    let comment_chain = selection_chain(&selections[2]);
    assert_eq!(
        comment_chain[0]["range"],
        range_at(FOLDING_SELECTION_SOURCE, comment_start, comment_end)
    );
    let string_chain = selection_chain(&selections[3]);
    assert_eq!(
        string_chain[0]["range"],
        range_at(FOLDING_SELECTION_SOURCE, marker_start, marker_end),
        "comment delimiters inside strings must remain string selection text"
    );
    let blank_chain = selection_chain(&selections[4]);
    assert_eq!(
        blank_chain[0]["range"],
        range_at(FOLDING_SELECTION_SOURCE, blank_line, blank_line),
        "blank-line cursors should select their line before the document"
    );
    assert_eq!(
        blank_chain[1]["range"],
        range_at(FOLDING_SELECTION_SOURCE, 0, FOLDING_SELECTION_SOURCE.len())
    );
    client.shutdown();
}
