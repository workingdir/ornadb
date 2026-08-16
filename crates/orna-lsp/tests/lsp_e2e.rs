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
        "CREATE SERVER FUNCTION echo_test.echo_value(p_value BOOLEAN DOCUMENTATION 'the value to echo')\n",
        "RETURNS BOOLEAN\n",
        "SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n",
        "AS SELECT p_value;\n",
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
    assert!(value.contains("p_value"), "parameter listing: {value}");
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

    client.shutdown();
}

#[test]
fn serves_hover_and_definition() {
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

    client.shutdown();
}
