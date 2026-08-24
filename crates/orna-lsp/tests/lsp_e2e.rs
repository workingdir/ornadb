//! End-to-end LSP protocol tests.
//!
//! Each test spawns the compiled `orna-lsp` binary, drives it through a
//! framed JSON-RPC client, and asserts the observable protocol behaviour.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// The valid application source used for positive tests.
///
/// The probe type carries DOCUMENTATION clauses so the rich hover
/// assertions can check documentation rendering.
// The `concat!` form preserves the leading spaces of the field line, which
// a backslash line continuation would strip.
const VALID_SOURCE: &str = concat!(
    "CREATE SCHEMA product_test;\n",
    "\n",
    "CREATE TYPE product_test.probe AS OBJECT (\n",
    "    stored BOOLEAN NOT NULL DOCUMENTATION 'whether the probe is stored'\n",
    ") DOCUMENTATION 'an object probe';\n",
    "\n",
    "CREATE SERVER FUNCTION product_test.create_probe()\n",
    "RETURNS ROWS (created REF product_test.probe)\n",
    "SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n",
    "AS INSERT INTO product_test.probe AS made (stored)\n",
    "VALUES (TRUE) RETURNING REF(made);\n",
    "\n",
    "CREATE SERVER FUNCTION product_test.read_probes()\n",
    "RETURNS ROWS (stored BOOLEAN)\n",
    "SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n",
    "AS SELECT probe.stored FROM product_test.probe probe;\n",
);

/// The accepted CLIENT source fixture shared with the syntax parser test.
const ACCEPTED_CLIENT_SOURCE: &str =
    include_str!("../../orna-syntax/testdata/accepted-client.orna");

/// Accepted CLIENT resource fixtures shared with the server's offline checks.
const SCALAR_RESOURCE_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/scalar_resource_dogfood.orna");
const STREAM_RESOURCE_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/stream_resource_dogfood.orna");

/// Accepted Inspector and expression CLIENT fixtures shared with offline checks.
const INSPECTOR_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/client_inspector_dogfood.orna");
const EXPRESSION_CLIENT_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/expression_client_dogfood.orna");

/// The accepted action fixture is currently assembled inline by the server
/// checks, so keep the same source here until it has a canonical fixture file.
const ACTION_SOURCE: &str = concat!(
    "CREATE SCHEMA action_fixture;\n",
    "\n",
    "CREATE CLIENT FUNCTION action_fixture.call(p_value INTEGER)\n",
    "RETURNS std.Action\n",
    "AS std.action.call(\n",
    "  target => std.invoke.echo,\n",
    "  arguments => std.call.args(p_value => p_value)\n",
    ");\n",
    "CREATE CLIENT FUNCTION action_fixture.local(p_value INTEGER)\n",
    "RETURNS INTEGER AS p_value;\n",
    "CREATE CLIENT FUNCTION action_fixture.call_local(p_value INTEGER)\n",
    "RETURNS std.Action\n",
    "AS std.action.call(\n",
    "  target => action_fixture.local,\n",
    "  arguments => std.call.args(p_value => p_value)\n",
    ");\n",
);

/// The broken source used for negative diagnostics tests.
const BROKEN_SOURCE: &str = "CREATE SCHEMA broken_test;\n\
CREATE SERVER FUNCTION broken_test.f()\n\
RETURNS BOOLEAN\n\
AS SELECT THIS IS NOT SQL;\n";

/// A framed JSON-RPC client attached to a spawned server.
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
            .expect("spawn orna-lsp binary");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = child.stdout.take().expect("server stdout");
        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).expect("serialise message");
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin
            .write_all(header.as_bytes())
            .expect("write header");
        self.stdin.write_all(&body).expect("write body");
        self.stdin.flush().expect("flush");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let message = self.read_message();
        assert_eq!(message["id"], id, "response id for {method}");
        assert!(
            message.get("result").is_some(),
            "no result for {method}: {message}"
        );
        message["result"].clone()
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    fn read_message(&mut self) -> Value {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read header line");
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(length) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(length.trim().parse::<usize>().expect("content length"));
            }
        }
        let length = content_length.expect("Content-Length header");
        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body).expect("read body");
        serde_json::from_slice(&body).expect("parse message body")
    }

    fn read_notification(&mut self, method: &str) -> Value {
        let message = self.read_message();
        assert_eq!(
            message["method"], method,
            "expected {method} notification, got {message}"
        );
        message["params"].clone()
    }

    fn shutdown(&mut self) {
        self.request("shutdown", json!(null));
        self.notify("exit", json!(null));
        let status = self.child.wait().expect("wait for server exit");
        assert!(status.success(), "server exit status {status}");
    }
}

fn initialize(client: &mut Client) {
    let result = client.request(
        "initialize",
        json!({
            "processId": null,
            "rootUri": null,
            "capabilities": {}
        }),
    );
    assert!(
        result["capabilities"]["semanticTokensProvider"].is_object(),
        "semantic tokens capability: {result}"
    );
    assert!(
        result["capabilities"]["diagnosticProvider"].is_object(),
        "diagnostic capability: {result}"
    );
    client.notify("initialized", json!({}));
}

fn position_inside(source: &str, prefix: &str, token: &str) -> Value {
    let prefix_start = source
        .find(prefix)
        .unwrap_or_else(|| panic!("missing position prefix {prefix:?}"));
    let token_start = prefix_start
        + prefix.len()
        + source[prefix_start + prefix.len()..]
            .find(token)
            .unwrap_or_else(|| panic!("missing token {token:?} after prefix {prefix:?}"));
    let first_character = token
        .chars()
        .next()
        .expect("cursor token must not be empty")
        .len_utf8();
    position_at_byte(source, token_start + first_character)
}

fn position_after(source: &str, prefix: &str) -> Value {
    let prefix_end = source
        .find(prefix)
        .unwrap_or_else(|| panic!("missing position prefix {prefix:?}"))
        + prefix.len();
    position_at_byte(source, prefix_end)
}

fn position_at_byte(source: &str, byte: usize) -> Value {
    let mut line = 0u64;
    let mut character = 0u64;
    for source_character in source[..byte].chars() {
        if source_character == '\n' {
            line += 1;
            character = 0;
        } else {
            character += source_character.len_utf16() as u64;
        }
    }
    json!({ "line": line, "character": character })
}

fn open_document(client: &mut Client, uri: &str, text: &str, version: i64) {
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "orna",
                "version": version,
                "text": text,
            }
        }),
    );
}

fn open_clean_document(client: &mut Client, uri: &str, source: &str) {
    open_document(client, uri, source, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["uri"], uri);
    assert_eq!(
        diagnostics["diagnostics"],
        json!([]),
        "accepted source clean"
    );

    let pull = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pull["kind"], "full");
    assert_eq!(
        pull["items"],
        json!([]),
        "accepted source pull diagnostics clean"
    );
}

fn assert_symbols_contain(client: &mut Client, uri: &str, expected: &[&str]) {
    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols = symbols.as_array().expect("document symbols");
    for name in expected {
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol["name"].as_str() == Some(*name)),
            "document symbols missing {name:?}: {symbols:?}"
        );
    }
}

fn assert_hover_contains(client: &mut Client, uri: &str, position: Value, expected: &str) {
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": position,
        }),
    );
    let value = hover["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("hover missing markdown contents: {hover}"));
    assert!(
        value.contains(expected),
        "hover missing {expected:?}: {value}"
    );
}

fn assert_definition_starts_on(
    client: &mut Client,
    uri: &str,
    position: Value,
    expected_line: u64,
) {
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": position,
        }),
    );
    assert_eq!(definition["uri"], uri, "definition URI: {definition}");
    assert_eq!(
        definition["range"]["start"]["line"], expected_line,
        "definition line: {definition}"
    );
}

fn assert_semantic_tokens_present(client: &mut Client, uri: &str) {
    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let data = tokens["data"].as_array().expect("semantic token data");
    assert!(!data.is_empty(), "semantic tokens present");
    assert_eq!(data.len() % 5, 0, "tokens are delta quintuples");
}

#[test]
fn serves_diagnostics_for_valid_and_broken_documents() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/valid.orna";

    open_document(&mut client, uri, VALID_SOURCE, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["uri"], uri);
    assert_eq!(diagnostics["diagnostics"], json!([]), "valid source clean");

    // The pull-based diagnostic request agrees with the pushed report.
    let pull = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pull["kind"], "full");
    assert_eq!(pull["items"], json!([]));

    // Replace the document with broken source and expect a syntax diagnostic.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": BROKEN_SOURCE }],
        }),
    );
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    let items = diagnostics["diagnostics"].as_array().expect("items");
    assert!(!items.is_empty(), "broken source reports diagnostics");
    let codes: Vec<&str> = items
        .iter()
        .map(|item| item["code"].as_str().expect("code"))
        .collect();
    assert!(
        codes.iter().any(|code| code.starts_with("ORNA")),
        "diagnostic codes: {codes:?}"
    );

    client.shutdown();
}

#[test]
fn serves_accepted_client_fixture_without_diagnostics_and_with_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/accepted-client.orna";

    open_document(&mut client, uri, ACCEPTED_CLIENT_SOURCE, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["uri"], uri);
    assert_eq!(
        diagnostics["diagnostics"],
        json!([]),
        "accepted CLIENT source clean"
    );

    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols = symbols.as_array().expect("document symbols");
    assert!(
        symbols.iter().any(|symbol| {
            symbol["detail"] == "client function"
                && matches!(symbol["name"].as_str(), Some("enabled" | "stateful"))
        }),
        "accepted CLIENT function symbol present: {symbols:?}"
    );

    client.shutdown();
}

#[test]
fn serves_accepted_resource_fixtures_without_diagnostics_and_with_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);

    let scalar_uri = "file:///test/scalar-resource-dogfood.orna";
    open_clean_document(&mut client, scalar_uri, SCALAR_RESOURCE_SOURCE);
    assert_symbols_contain(&mut client, scalar_uri, &["scalar_fixture", "call"]);
    assert_semantic_tokens_present(&mut client, scalar_uri);
    assert_hover_contains(
        &mut client,
        scalar_uri,
        position_inside(
            SCALAR_RESOURCE_SOURCE,
            "CREATE CLIENT FUNCTION scalar_fixture.",
            "call",
        ),
        "client function",
    );
    assert_definition_starts_on(
        &mut client,
        scalar_uri,
        position_inside(
            SCALAR_RESOURCE_SOURCE,
            "CREATE CLIENT FUNCTION scalar_fixture.",
            "call",
        ),
        1,
    );

    let stream_uri = "file:///test/stream-resource-dogfood.orna";
    open_clean_document(&mut client, stream_uri, STREAM_RESOURCE_SOURCE);
    assert_symbols_contain(
        &mut client,
        stream_uri,
        &["stream_fixture", "events", "read"],
    );
    assert_semantic_tokens_present(&mut client, stream_uri);
    assert_hover_contains(
        &mut client,
        stream_uri,
        position_inside(
            STREAM_RESOURCE_SOURCE,
            "CREATE SERVER FUNCTION stream_fixture.",
            "events",
        ),
        "server function",
    );
    assert_definition_starts_on(
        &mut client,
        stream_uri,
        position_inside(
            STREAM_RESOURCE_SOURCE,
            "CREATE SERVER FUNCTION stream_fixture.",
            "events",
        ),
        6,
    );

    client.shutdown();
}

#[test]
fn serves_accepted_action_fixture_without_diagnostics_and_with_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/action-dogfood.orna";

    open_clean_document(&mut client, uri, ACTION_SOURCE);
    assert_symbols_contain(
        &mut client,
        uri,
        &["action_fixture", "call", "local", "call_local"],
    );
    assert_semantic_tokens_present(&mut client, uri);
    assert_hover_contains(
        &mut client,
        uri,
        position_inside(ACTION_SOURCE, "RETURNS std.", "Action"),
        "standard opaque value type",
    );
    assert_hover_contains(
        &mut client,
        uri,
        position_inside(
            ACTION_SOURCE,
            "CREATE CLIENT FUNCTION action_fixture.",
            "local",
        ),
        "client function",
    );
    assert_definition_starts_on(
        &mut client,
        uri,
        position_inside(
            ACTION_SOURCE,
            "CREATE CLIENT FUNCTION action_fixture.",
            "local",
        ),
        8,
    );

    client.shutdown();
}

#[test]
fn serves_accepted_inspector_fixture_without_diagnostics_and_with_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/client-inspector-dogfood.orna";

    open_clean_document(&mut client, uri, INSPECTOR_SOURCE);
    assert_symbols_contain(
        &mut client,
        uri,
        &["inspector_app", "inspector_renderer", "inspector"],
    );
    assert_semantic_tokens_present(&mut client, uri);
    assert_hover_contains(
        &mut client,
        uri,
        position_inside(
            INSPECTOR_SOURCE,
            "CREATE EXTERNAL CLIENT FUNCTION inspector_app.",
            "inspector_renderer",
        ),
        "client function",
    );
    assert_definition_starts_on(
        &mut client,
        uri,
        position_inside(
            INSPECTOR_SOURCE,
            "sys.inspect.invocation_nodes(p_snapshot => ",
            "snapshot",
        ),
        17,
    );

    client.shutdown();
}

#[test]
fn serves_accepted_expression_client_fixture_without_diagnostics_and_with_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/expression-client-dogfood.orna";

    open_clean_document(&mut client, uri, EXPRESSION_CLIENT_SOURCE);
    assert_symbols_contain(
        &mut client,
        uri,
        &["expr", "literal", "composed", "external"],
    );
    assert_semantic_tokens_present(&mut client, uri);
    assert_hover_contains(
        &mut client,
        uri,
        position_inside(EXPRESSION_CLIENT_SOURCE, "AS expr.", "literal"),
        "client function",
    );
    assert_definition_starts_on(
        &mut client,
        uri,
        position_inside(EXPRESSION_CLIENT_SOURCE, "AS expr.", "literal"),
        2,
    );

    client.shutdown();
}

#[test]
fn serves_semantic_tokens_document_symbols_and_completion() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/semantic.orna";
    open_document(&mut client, uri, VALID_SOURCE, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    let data = tokens["data"].as_array().expect("token data");
    assert!(!data.is_empty(), "semantic tokens present");
    assert_eq!(data.len() % 5, 0, "tokens are delta quintuples");
    let types: std::collections::HashSet<u64> = data
        .iter()
        .skip(4)
        .step_by(5)
        .map(|value| value.as_u64().expect("token type"))
        .collect();
    assert!(
        types.contains(&0) || types.contains(&1),
        "keyword or type tokens present: {types:?}"
    );

    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .expect("symbols")
        .iter()
        .map(|symbol| symbol["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec!["product_test", "probe", "create_probe", "read_probes"],
        "outline symbols"
    );

    let completion = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 0 },
            "context": { "triggerKind": 1 },
        }),
    );
    let labels: Vec<&str> = completion
        .as_array()
        .expect("completion items")
        .iter()
        .map(|item| item["label"].as_str().expect("label"))
        .collect();
    assert!(labels.contains(&"CREATE"), "keyword completion");
    assert!(labels.contains(&"create_probe"), "function completion");
    assert!(labels.contains(&"boolean"), "standard type completion");
    assert!(labels.contains(&"BOOL"), "scalar completion");

    client.shutdown();
}

#[test]
fn serves_rich_hover_content() {
    let mut client = Client::spawn();
    initialize(&mut client);
    // The URI sits inside the workspace tree so the spec-bundle walk finds
    // the grammar reference and hovers carry a Spec link.
    let uri = format!(
        "file://{}/../../rich-hover.orna",
        env!("CARGO_MANIFEST_DIR")
    );
    open_document(&mut client, &uri, VALID_SOURCE, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let hover_at = |client: &mut Client, line: u64, character: u64| {
        client.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
    };

    // Function hover: signature, usage example, spec link.
    let hover = hover_at(&mut client, 6, 40);
    let value = hover["contents"]["value"].as_str().expect("hover value");
    assert!(value.contains("server function"), "kind badge: {value}");
    assert!(value.contains("RETURNS ROWS"), "returns: {value}");
    assert!(
        value.contains("orna invoke product_test.create_probe"),
        "usage example: {value}"
    );
    assert!(value.contains("**Spec**"), "spec link: {value}");
    assert!(value.contains("orna.ebnf"), "spec link target: {value}");

    // Type hover: fields with modifiers and type-level documentation.
    let hover = hover_at(&mut client, 2, 27);
    let value = hover["contents"]["value"].as_str().expect("hover value");
    assert!(value.contains("object type"), "kind badge: {value}");
    assert!(value.contains("stored"), "field listing: {value}");
    assert!(value.contains("NOT NULL"), "field modifier: {value}");
    assert!(
        value.contains("an object probe"),
        "type documentation: {value}"
    );

    // Field hover: the field name shadows the BOOLEAN scalar spelling.
    let hover = hover_at(&mut client, 3, 7);
    let value = hover["contents"]["value"].as_str().expect("hover value");
    assert!(value.starts_with("**field**"), "field hover: {value}");
    assert!(value.contains("BOOLEAN"), "field type: {value}");
    assert!(
        value.contains("whether the probe is stored"),
        "field documentation: {value}"
    );

    // Scalar hover on the type position of the same line.
    let hover = hover_at(&mut client, 3, 14);
    let value = hover["contents"]["value"].as_str().expect("hover value");
    assert!(value.starts_with("**`BOOLEAN`**"), "scalar hover: {value}");
    assert!(value.contains("boolean type"), "scalar summary: {value}");

    // Keyword hover.
    let hover = hover_at(&mut client, 7, 3);
    let value = hover["contents"]["value"].as_str().expect("hover value");
    assert!(
        value.contains("**`RETURNS`** keyword"),
        "keyword hover: {value}"
    );
    assert!(value.contains("result shape"), "keyword summary: {value}");
    assert!(value.contains("**Example**"), "keyword example: {value}");

    // Parameter hover needs a document with parameters. The parameter-select
    // body is a parser-level form that the application checker rejects, so
    // this document carries diagnostics; hover still serves the parsed
    // parameter documentation.
    let echo_uri = format!("file://{}/../../echo.orna", env!("CARGO_MANIFEST_DIR"));
    let echo_source = concat!(
        "CREATE SCHEMA echo_test;\n",
        "CREATE SERVER FUNCTION echo_test.echo_value(p_stored BOOLEAN DOCUMENTATION 'the value to echo')\n",
        "RETURNS BOOLEAN\n",
        "SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n",
        "AS SELECT p_stored;\n",
    );
    open_document(&mut client, &echo_uri, echo_source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let parameter_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": echo_uri },
            "position": { "line": 1, "character": 50 },
        }),
    );
    let value = parameter_hover["contents"]["value"]
        .as_str()
        .expect("parameter hover value");
    assert!(
        value.starts_with("**parameter**"),
        "parameter hover: {value}"
    );
    assert!(value.contains("BOOLEAN"), "parameter type: {value}");
    assert!(
        value.contains("the value to echo"),
        "parameter documentation: {value}"
    );

    let function_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": echo_uri },
            "position": { "line": 1, "character": 40 },
        }),
    );
    let value = function_hover["contents"]["value"]
        .as_str()
        .expect("echo function hover value");
    assert!(
        value.contains("**Parameters**"),
        "parameters section: {value}"
    );
    assert!(value.contains("p_stored"), "parameter listing: {value}");
    assert!(
        value.contains("the value to echo"),
        "parameter documentation: {value}"
    );

    // SQL column hover: `stored` in the SELECT projection resolves to the
    // field of the FROM object type.
    let hover = hover_at(&mut client, 15, 21);
    let value = hover["contents"]["value"]
        .as_str()
        .expect("select column hover");
    assert!(
        value.starts_with("**field**"),
        "select column hover: {value}"
    );
    assert!(
        value.contains("whether the probe is stored"),
        "select column docs: {value}"
    );

    // SQL column hover: the INSERT column list resolves the same way.
    let hover = hover_at(&mut client, 9, 45);
    let value = hover["contents"]["value"]
        .as_str()
        .expect("insert column hover");
    assert!(
        value.starts_with("**field**"),
        "insert column hover: {value}"
    );
    assert!(
        value.contains("whether the probe is stored"),
        "insert column docs: {value}"
    );

    // Standard-library type hover: a qualified std type reference resolves
    // through the verified standard catalogue.
    let std_uri = format!("file://{}/../../std-type.orna", env!("CARGO_MANIFEST_DIR"));
    let std_source = concat!(
        "CREATE SCHEMA std_test;\n",
        "CREATE TYPE std_test.token AS VALUE (t std.types.OPAQUE_TOKEN) IMMUTABLE PERSISTABLE;\n",
    );
    open_document(&mut client, &std_uri, std_source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");
    let std_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": std_uri },
            "position": { "line": 1, "character": 53 },
        }),
    );
    let value = std_hover["contents"]["value"]
        .as_str()
        .expect("std type hover");
    assert!(
        value.contains("standard opaque value type"),
        "std type hover: {value}"
    );
    assert!(
        value.contains("orna.std.value.opaque-token@1"),
        "std type contract: {value}"
    );

    let collision_uri = "file:///test/hover-collision.orna";
    let collision_source = concat!(
        "CREATE SCHEMA status;\n",
        "CREATE SCHEMA product_test;\n",
        "CREATE TYPE product_test.probe AS OBJECT (status BOOLEAN DOCUMENTATION 'field status docs');\n",
        "CREATE SERVER FUNCTION read_status() RETURNS ROWS (status BOOLEAN) AS\n",
        "SELECT p.status FROM product_test.probe p;\n",
    );
    open_document(&mut client, collision_uri, collision_source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");
    let sql_status_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": collision_uri },
            "position": position_after(collision_source, "SELECT p."),
        }),
    );
    let sql_status_value = sql_status_hover["contents"]["value"]
        .as_str()
        .expect("SQL status hover value");
    assert!(
        sql_status_value.starts_with("**field**"),
        "SQL field beats schema hover: {sql_status_value}"
    );
    assert!(
        sql_status_value.contains("BOOLEAN"),
        "SQL field type: {sql_status_value}"
    );
    assert!(
        sql_status_value.contains("field status docs"),
        "SQL field docs: {sql_status_value}"
    );
    let schema_status_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": collision_uri },
            "position": position_after(collision_source, "CREATE SCHEMA "),
        }),
    );
    let schema_status_value = schema_status_hover["contents"]["value"]
        .as_str()
        .expect("schema status hover value");
    assert!(
        schema_status_value.starts_with("**schema**"),
        "schema declaration remains schema hover: {schema_status_value}"
    );

    client.shutdown();
}

#[test]
fn serves_hover_definition_and_references() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/nav.orna";
    open_document(&mut client, uri, VALID_SOURCE, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    // Hover over the function declaration name on line 6 (0-based).
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 6, "character": 40 },
        }),
    );
    assert!(
        hover["contents"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("server function")),
        "function hover: {hover}"
    );

    // Definition of the type reference inside the insert body.
    // Line 9 contains "AS INSERT INTO product_test.probe AS made (stored)".
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 9, "character": 30 },
        }),
    );
    assert_eq!(definition["uri"], uri);
    assert_eq!(
        definition["range"]["start"]["line"], 2,
        "type declaration line: {definition}"
    );

    // References for the field selected in the SELECT projection.
    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 15, "character": 21 },
            "context": { "includeDeclaration": true },
        }),
    );
    let reference_locations: Vec<(u64, u64, u64, u64)> = references
        .as_array()
        .expect("references")
        .iter()
        .map(|reference| {
            assert_eq!(reference["uri"], uri, "reference URI: {reference}");
            (
                reference["range"]["start"]["line"]
                    .as_u64()
                    .expect("start line"),
                reference["range"]["start"]["character"]
                    .as_u64()
                    .expect("start character"),
                reference["range"]["end"]["line"]
                    .as_u64()
                    .expect("end line"),
                reference["range"]["end"]["character"]
                    .as_u64()
                    .expect("end character"),
            )
        })
        .collect();
    // Field references stay within the selected object field; same-spelled
    // ROWS return columns are not field references.
    let expected_references = [(3, 4, 3, 10), (9, 43, 9, 49), (15, 16, 15, 22)];
    assert_eq!(
        reference_locations.len(),
        expected_references.len(),
        "reference count: {references}"
    );
    for expected in expected_references {
        assert!(
            reference_locations.contains(&expected),
            "missing reference {expected:?}: {references}"
        );
    }

    // Select the field declaration so the false flag exercises a non-top-level symbol.
    let references_without_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 3, "character": 4 },
            "context": { "includeDeclaration": false },
        }),
    );
    let references_without_declaration = references_without_declaration
        .as_array()
        .expect("references without declaration");
    let expected_without_declaration = [(9, 43, 9, 49), (15, 16, 15, 22)];
    let actual_without_declaration: Vec<_> = references_without_declaration
        .iter()
        .map(|reference| {
            (
                reference["range"]["start"]["line"]
                    .as_u64()
                    .expect("start line"),
                reference["range"]["start"]["character"]
                    .as_u64()
                    .expect("start character"),
                reference["range"]["end"]["line"]
                    .as_u64()
                    .expect("end line"),
                reference["range"]["end"]["character"]
                    .as_u64()
                    .expect("end character"),
            )
        })
        .collect();
    assert_eq!(
        actual_without_declaration.len(),
        expected_without_declaration.len(),
        "references without declaration: {references_without_declaration:?}"
    );
    for expected in expected_without_declaration {
        assert!(
            actual_without_declaration.contains(&expected),
            "missing reference without declaration {expected:?}: {references_without_declaration:?}"
        );
    }

    client.shutdown();
}

#[test]
fn scoped_navigation_resolves_owner_paths_and_fails_closed() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/scoped-navigation.orna";
    let source = concat!(
        "CREATE SCHEMA owners;\n",
        "CREATE TYPE owners.first_obj AS OBJECT (stored BOOLEAN);\n",
        "CREATE TYPE owners.second_obj AS OBJECT (stored BOOLEAN);\n",
        "CREATE TYPE owners.quoted_obj AS OBJECT (\"display_name\" BOOLEAN);\n",
        "CREATE TYPE owners.client_obj AS OBJECT (\"display_name\" BOOLEAN);\n",
        "CREATE SCHEMA unicode_nav;\n",
        "CREATE TYPE unicode_nav.café AS OBJECT (résumé BOOLEAN);\n",
        "CREATE SERVER FUNCTION read_first() RETURNS ROWS (stored BOOLEAN) AS\n",
        "SELECT first_alias.stored FROM owners.first_obj first_alias;\n",
        "CREATE SERVER FUNCTION read_second() RETURNS ROWS (stored BOOLEAN) AS\n",
        "SELECT second_alias.stored FROM owners.second_obj second_alias;\n",
        "CREATE SERVER FUNCTION read_quoted() RETURNS ROWS (\"display_name\" BOOLEAN) AS\n",
        "SELECT quoted_alias.\"display_name\" FROM owners.quoted_obj quoted_alias;\n",
        "CREATE SERVER FUNCTION read_unicode() RETURNS ROWS (résumé BOOLEAN) AS\n",
        "SELECT unicode_alias.RÉSUMÉ FROM unicode_nav.CAFÉ unicode_alias;\n",
        "CREATE SERVER FUNCTION unresolved_property() RETURNS BOOLEAN AS\n",
        "SELECT unknown_alias.missing FROM owners.first_obj unknown_alias;\n",
        "CREATE CLIENT FUNCTION client_read(entry REF owners.client_obj) RETURNS BOOLEAN AS\n",
        "entry.\"display_name\";\n",
        "CREATE CLIENT FUNCTION client_field_shadow(entry REF owners.client_obj) RETURNS BOOLEAN IS\n",
        "    STATE entry BOOLEAN SCOPE LOCAL DEFAULT TRUE;\n",
        "BEGIN\n",
        "    RETURN entry.\"display_name\";\n",
        "END;\n",
        "CREATE CLIENT FUNCTION client_call(entry BOOLEAN) RETURNS BOOLEAN AS entry;\n",
        "CREATE CLIENT FUNCTION client_caller() RETURNS BOOLEAN AS\n",
        "owners.client_call(entry => TRUE);\n",
        "CREATE TYPE owners.child_obj AS OBJECT (stored BOOLEAN);\n",
        "CREATE TYPE owners.parent_obj AS OBJECT (child REF owners.child_obj);\n",
        "CREATE SERVER FUNCTION read_nested() RETURNS ROWS (stored BOOLEAN) AS\n",
        "SELECT nested_alias.child.stored FROM owners.parent_obj nested_alias;\n",
        "CREATE SERVER FUNCTION invalid_alias() RETURNS ROWS (stored BOOLEAN) AS\n",
        "SELECT wrong_alias.stored FROM owners.first_obj real_alias;\n",
        "CREATE SERVER FUNCTION unknown_insert(p_stored BOOLEAN) RETURNS ROWS (created REF owners.first_obj) AS\n",
        "INSERT INTO owners.first_obj AS made (\"missing\") VALUES (p_stored) RETURNING REF(made);\n",
        "CREATE SERVER FUNCTION unknown_update(p_stored BOOLEAN, p_key REF owners.first_obj) RETURNS ROWS (changed REF owners.first_obj) AS\n",
        "UPDATE owners.first_obj AS changed SET \"missing\" = p_stored WHERE REF(changed) = p_key RETURNING REF(changed);\n",
        "CREATE CLIENT FUNCTION shadow(entry BOOLEAN) RETURNS BOOLEAN IS\n",
        "    STATE entry BOOLEAN SCOPE LOCAL DEFAULT TRUE;\n",
        "BEGIN\n",
        "    RETURN entry;\n",
        "END;\n",
        "CREATE CLIENT FUNCTION future() RETURNS BOOLEAN IS\n",
        "    STATE first BOOLEAN SCOPE LOCAL DEFAULT later;\n",
        "    STATE later BOOLEAN SCOPE LOCAL DEFAULT TRUE;\n",
        "BEGIN\n",
        "    RETURN first;\n",
        "END;\n",
    );
    open_document(&mut client, uri, source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let first_use = position_inside(source, "SELECT first_alias.", "stored");
    let first_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": first_use }),
    );
    assert_eq!(
        first_definition["range"]["start"]["line"], 1,
        "first owner field definition: {first_definition}"
    );
    let first_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": first_use,
            "context": { "includeDeclaration": false },
        }),
    );
    assert_eq!(
        first_references
            .as_array()
            .expect("first field references")
            .len(),
        1,
        "same-spelled second-owner field leaked: {first_references}"
    );

    let second_use = position_inside(source, "SELECT second_alias.", "stored");
    let second_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": second_use }),
    );
    assert_eq!(
        second_definition["range"]["start"]["line"], 2,
        "second owner field definition: {second_definition}"
    );

    let quoted_use = position_inside(source, "SELECT quoted_alias.", "\"display_name\"");
    let quoted_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": quoted_use }),
    );
    assert_eq!(
        quoted_definition["range"]["start"]["line"], 3,
        "quoted SQL field definition: {quoted_definition}"
    );

    let unicode_use = position_inside(source, "SELECT unicode_alias.", "RÉSUMÉ");
    let unicode_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": unicode_use }),
    );
    assert_eq!(
        unicode_definition["range"]["start"]["line"], 6,
        "Unicode-cased owner/field definition: {unicode_definition}"
    );

    let client_use = position_inside(source, "entry.", "\"display_name\"");
    let client_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": client_use }),
    );
    assert_eq!(
        client_definition["range"]["start"]["line"], 4,
        "quoted CLIENT field definition: {client_definition}"
    );
    let client_shadow_use = position_inside(source, "RETURN entry.", "\"display_name\"");
    let client_shadow_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": client_shadow_use }),
    );
    assert_eq!(
        client_shadow_definition["range"]["start"]["line"], 4,
        "CLIENT field root prefers parameter over same-named state: {client_shadow_definition}"
    );

    let client_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": client_use,
            "context": { "includeDeclaration": false },
        }),
    );
    assert_eq!(
        client_references
            .as_array()
            .expect("CLIENT field references")
            .len(),
        2,
        "CLIENT field references missed a same-owner use or escaped its owner: {client_references}"
    );

    let unresolved_use = position_inside(source, "SELECT unknown_alias.", "missing");
    let unresolved_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": unresolved_use }),
    );
    assert!(
        unresolved_definition.is_null(),
        "unresolved property acquired a definition: {unresolved_definition}"
    );
    let mutation_insert_use = position_inside(
        source,
        "INSERT INTO owners.first_obj AS made (",
        "\"missing\"",
    );
    let mutation_insert_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": mutation_insert_use }),
    );
    assert!(
        mutation_insert_definition.is_null(),
        "unknown quoted INSERT field acquired a definition: {mutation_insert_definition}"
    );
    let mutation_insert_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": mutation_insert_use,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        mutation_insert_references
            .as_array()
            .expect("unknown INSERT references")
            .len(),
        0,
        "unknown quoted INSERT field leaked references: {mutation_insert_references}"
    );

    let mutation_update_use = position_inside(
        source,
        "UPDATE owners.first_obj AS changed SET ",
        "\"missing\"",
    );
    let mutation_update_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": mutation_update_use }),
    );
    assert!(
        mutation_update_definition.is_null(),
        "unknown quoted UPDATE field acquired a definition: {mutation_update_definition}"
    );
    let mutation_update_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": mutation_update_use,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        mutation_update_references
            .as_array()
            .expect("unknown UPDATE references")
            .len(),
        0,
        "unknown quoted UPDATE field leaked references: {mutation_update_references}"
    );

    let unresolved_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": unresolved_use,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        unresolved_references
            .as_array()
            .expect("unresolved references")
            .len(),
        0,
        "unresolved property leaked references: {unresolved_references}"
    );

    let argument_label = position_inside(source, "owners.client_call(", "entry");
    let label_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": argument_label }),
    );
    assert!(
        label_definition.is_null(),
        "named-call argument label acquired a definition: {label_definition}"
    );
    let label_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": argument_label,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        label_references
            .as_array()
            .expect("argument label references")
            .len(),
        0,
        "named-call argument label leaked variable references: {label_references}"
    );

    let nested_use = position_inside(source, "SELECT nested_alias.child.", "stored");
    let nested_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": nested_use }),
    );
    assert_eq!(
        nested_definition["range"]["start"]["line"], 27,
        "nested SQL member resolves through child owner: {nested_definition}"
    );

    let invalid_alias_use = position_inside(source, "SELECT wrong_alias.", "stored");
    let invalid_alias_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": invalid_alias_use }),
    );
    assert!(
        invalid_alias_definition.is_null(),
        "invalid SQL alias acquired a definition: {invalid_alias_definition}"
    );
    let invalid_alias_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": invalid_alias_use,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        invalid_alias_references
            .as_array()
            .expect("invalid alias references")
            .len(),
        0,
        "invalid SQL alias leaked references: {invalid_alias_references}"
    );

    let return_parameter_use = position_inside(
        source,
        "CREATE CLIENT FUNCTION client_call(entry BOOLEAN) RETURNS BOOLEAN AS ",
        "entry",
    );
    let return_parameter_hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": uri }, "position": return_parameter_use }),
    );
    let return_parameter_value = return_parameter_hover["contents"]["value"]
        .as_str()
        .expect("CLIENT return parameter hover");
    assert!(
        return_parameter_value.starts_with("**parameter**"),
        "CLIENT return parameter hover: {return_parameter_value}"
    );
    assert!(
        return_parameter_value.contains("BOOLEAN"),
        "CLIENT return parameter type: {return_parameter_value}"
    );

    let client_field_use = position_inside(
        source,
        "CREATE CLIENT FUNCTION client_field_shadow(entry REF owners.client_obj) RETURNS BOOLEAN IS\n    STATE entry BOOLEAN SCOPE LOCAL DEFAULT TRUE;\nBEGIN\n    RETURN entry.",
        "\"display_name\"",
    );
    let client_field_hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": uri }, "position": client_field_use }),
    );
    let client_field_value = client_field_hover["contents"]["value"]
        .as_str()
        .expect("CLIENT field hover");
    assert!(
        client_field_value.starts_with("**field**"),
        "CLIENT field use hover: {client_field_value}"
    );
    assert!(
        client_field_value.contains("BOOLEAN"),
        "CLIENT field use type: {client_field_value}"
    );

    let shadow_use = position_inside(
        source,
        "CREATE CLIENT FUNCTION shadow(entry BOOLEAN) RETURNS BOOLEAN IS\n    STATE entry BOOLEAN SCOPE LOCAL DEFAULT TRUE;\nBEGIN\n    RETURN ",
        "entry",
    );
    let state_hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": uri }, "position": shadow_use }),
    );
    let state_value = state_hover["contents"]["value"]
        .as_str()
        .expect("CLIENT state hover");
    assert!(
        state_value.starts_with("**parameter**"),
        "CLIENT state use hover: {state_value}"
    );
    assert!(
        state_value.contains("BOOLEAN"),
        "CLIENT state use type: {state_value}"
    );

    let shadow_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": shadow_use }),
    );
    assert_eq!(
        shadow_definition["range"]["start"]["line"], 37,
        "CLIENT parameter shadows same-named state: {shadow_definition}"
    );

    let future_use = position_inside(source, "STATE first BOOLEAN SCOPE LOCAL DEFAULT ", "later");
    let future_definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": future_use }),
    );
    assert!(
        future_definition.is_null(),
        "future local acquired a definition: {future_definition}"
    );
    let future_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": future_use,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        future_references
            .as_array()
            .expect("future local references")
            .len(),
        0,
        "future local leaked references: {future_references}"
    );

    client.shutdown();
}

#[test]
fn semantic_token_range_includes_intersecting_multiline_comment_segments() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/semantic-range.orna";
    let source = "/* first line\nsecond line\nthird line */\n";
    open_document(&mut client, uri, source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let tokens = client.request(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 2, "character": 0 },
            },
        }),
    );
    let data = tokens["data"].as_array().expect("semantic token data");
    assert_eq!(
        data.len(),
        5,
        "only the multiline comment segment intersecting the range is returned: {data:?}"
    );
    assert_eq!(data[0], json!(1), "segment starts on the requested line");
    assert_eq!(
        data[1],
        json!(0),
        "segment starts at the requested line start"
    );
    assert_eq!(data[2], json!(11), "segment covers the ASCII line contents");
    assert_eq!(data[3], json!(8), "segment is a comment token");
    assert_eq!(data[4], json!(0), "comment has no modifiers");

    client.shutdown();
}

#[test]
fn did_save_republishes_diagnostics_and_did_close_clears_document_state() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/save-close.orna";

    open_document(&mut client, uri, BROKEN_SOURCE, 1);
    let opened = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(opened["uri"], uri);
    let opened_items = opened["diagnostics"]
        .as_array()
        .expect("didOpen diagnostics");
    assert!(
        !opened_items.is_empty(),
        "broken source reports diagnostics"
    );
    assert_eq!(opened["version"], 1);

    client.notify(
        "textDocument/didSave",
        json!({
            "textDocument": { "uri": uri },
        }),
    );
    let saved = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(saved["uri"], uri);
    assert_eq!(saved["diagnostics"], opened["diagnostics"]);
    assert_eq!(saved["version"], 1);

    client.notify(
        "textDocument/didClose",
        json!({
            "textDocument": { "uri": uri },
        }),
    );
    let closed = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(closed["uri"], uri);
    assert_eq!(closed["diagnostics"], json!([]));
    assert!(closed.get("version").is_none());

    let pull = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pull["kind"], "full");
    assert_eq!(pull["items"], json!([]));

    client.shutdown();
}

#[test]
fn serves_semantic_compiler_diagnostics_for_unknown_schema_in_push_and_pull() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/semantic-invalid.orna";
    let source = "CREATE TYPE app.task AS OBJECT (done BOOLEAN);\n";

    open_document(&mut client, uri, source, 1);
    let pushed = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(pushed["uri"], uri);
    let pushed_items = pushed["diagnostics"].as_array().expect("diagnostic items");
    assert_eq!(pushed_items.len(), 1, "semantic diagnostic: {pushed}");
    let expected_range = json!({
        "start": { "line": 0, "character": 12 },
        "end": { "line": 0, "character": 20 },
    });
    let pushed_diagnostic = &pushed_items[0];
    assert_eq!(pushed_diagnostic["range"], expected_range);
    assert_eq!(pushed_diagnostic["severity"], 1);
    assert_eq!(pushed_diagnostic["code"], "ORNA0101");
    assert_eq!(pushed_diagnostic["source"], "orna");
    assert_eq!(
        pushed_diagnostic["message"],
        "unknown schema app for object type app.task"
    );

    let pull = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pull["kind"], "full");
    assert_eq!(pull["items"], pushed["diagnostics"]);

    client.shutdown();
}
