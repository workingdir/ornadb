//! End-to-end LSP protocol tests.
//!
//! Each test spawns the compiled `orna-lsp` binary, drives it through a
//! framed JSON-RPC client, and asserts the observable protocol behaviour.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use orna_compiler::{check_new_application, check_standard_library_source};
use orna_core::source::{SourceBundle, SourceUnit};
use orna_standard::{
    retained_standard_library_v11_snapshot, verify_standard_library_v11_snapshot,
};
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

/// Accepted SERVER UPDATE and DELETE mutations with declarations for every
/// referenced schema, object type, field, alias, and parameter.
const MUTATION_SOURCE: &str = concat!(
    "CREATE SCHEMA mutation_test;\n",
    "CREATE TYPE mutation_test.item AS OBJECT (\n",
    "    stored BOOLEAN NOT NULL\n",
    ");\n",
    "CREATE SERVER FUNCTION mutation_test.update_item(\n",
    "    p_item REF mutation_test.item, p_stored BOOLEAN\n",
    ")\n",
    "RETURNS ROWS (updated REF mutation_test.item)\n",
    "SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n",
    "AS UPDATE mutation_test.item AS updated\n",
    "SET stored = p_stored\n",
    "WHERE REF(updated) = p_item\n",
    "RETURNING REF(updated);\n",
    "CREATE SERVER FUNCTION mutation_test.delete_item(p_item REF mutation_test.item)\n",
    "RETURNS ROWS (deleted BOOLEAN)\n",
    "SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE\n",
    "AS DELETE FROM mutation_test.item AS deleted\n",
    "WHERE REF(deleted) = p_item\n",
    "RETURNING TRUE;\n",
);

/// The accepted identity-preserving object-field rename shape. The LSP
/// process has no user catalogue base, so its compiler diagnostics report the
/// missing historical object while syntax and source navigation still expose
/// the final field declaration and use.
const FIELD_RENAME_SOURCE: &str = concat!(
    "CREATE SCHEMA people;\n",
    "CREATE TYPE people.person AS OBJECT (\n",
    "    primary_email TEXT NOT NULL\n",
    ");\n",
    "ALTER TYPE people.person\n",
    "    RENAME FIELD email TO primary_email;\n",
    "CREATE SERVER FUNCTION people.list_emails()\n",
    "RETURNS ROWS (email TEXT)\n",
    "AS\n",
    "    SELECT person.primary_email\n",
    "    FROM people.person person;\n",
);

/// Accepted ORDER BY source used to pin ASC/DESC keyword highlighting.
const ORDER_BY_SOURCE: &str = concat!(
    "CREATE SCHEMA ordering;\n",
    "CREATE TYPE ordering.item AS OBJECT (\n",
    "    title TEXT NOT NULL\n",
    ");\n",
    "CREATE SERVER FUNCTION ordering.list_items()\n",
    "RETURNS ROWS (title TEXT)\n",
    "AS\n",
    "/* 😀 */ SELECT item.title FROM ordering.item item ORDER BY item.title ASC, item.title DESC;\n",
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
const SERVER_FUNCTION_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/server_function_dogfood.orna");
const CLIENT_LOCAL_ASSIGNMENT_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/client_local_assignment_dogfood.orna");
const CLIENT_STATE_SOURCE: &str =
    include_str!("../../orna-server/tests/fixtures/client_state_dogfood.orna");

/// The accepted action fixture shared with server offline checks.
const ACTION_SOURCE: &str = include_str!("../../orna-server/tests/fixtures/action_dogfood.orna");

/// The broken source used for negative diagnostics tests.
const BROKEN_SOURCE: &str = "CREATE SCHEMA broken_test;\n\
CREATE SERVER FUNCTION broken_test.f()\n\
RETURNS BOOLEAN\n\
AS SELECT THIS IS NOT SQL;\n";

/// One invalid source unit shared by the canonical source check and the LSP
/// parity matrix. The multibyte and combining scalars keep the compiler's
/// UTF-8 byte span distinct from the LSP's UTF-16 line/character range.
const SOURCE_CHECK_PARITY_ASCII_SOURCE: &str = concat!(
    "CREATE SCHEMA parity;\n",
    "CREATE TYPE app.task AS OBJECT (done BOOLEAN);\n",
);

const SOURCE_CHECK_PARITY_SOURCE: &str = concat!(
    "CREATE SCHEMA parity;\n",
    "/* 😀 ée\u{0301} */ CREATE TYPE app.task AS OBJECT (done BOOLEAN);\n",
);

/// The accepted editor corpus is the one source of truth for this LSP gate.
const ACCEPTED_MANIFEST: &str =
    include_str!("../../../editors/tree-sitter-orna/test/accepted-corpus.txt");
const CORPUS_DELIMITER: &str = "====================";

#[derive(Debug)]
struct CorpusCase {
    source: String,
    expected_tree: String,
    path: PathBuf,
}

fn accepted_case_names() -> Vec<String> {
    let names = ACCEPTED_MANIFEST
        .lines()
        .enumerate()
        .map(|(line_number, line)| {
            assert!(
                !line.is_empty() && line == line.trim(),
                "malformed accepted corpus manifest entry at line {}: {line:?}",
                line_number + 1
            );
            line.to_owned()
        })
        .collect::<Vec<_>>();

    assert!(
        !names.is_empty(),
        "accepted corpus manifest must enumerate at least one case"
    );
    let mut unique_names = names.clone();
    unique_names.sort();
    unique_names.dedup();
    assert_eq!(
        unique_names.len(),
        names.len(),
        "accepted corpus manifest contains duplicate case names"
    );
    names
}

fn corpus_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editors/tree-sitter-orna/test/corpus")
}

fn lines_with_offsets(source: &str) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let end = start + line.len();
        lines.push((start, end));
        start = end;
    }
    if start < source.len() {
        lines.push((start, source.len()));
    }
    lines
}

fn line_body(source: &str, range: (usize, usize)) -> &str {
    let line = &source[range.0..range.1];
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn parse_corpus_file(path: &Path, contents: &str) -> Vec<(String, CorpusCase)> {
    let lines = lines_with_offsets(contents);
    let mut cases = Vec::new();
    let mut cursor = 0;

    while cursor < lines.len() {
        let body = line_body(contents, lines[cursor]);
        if body.is_empty() {
            cursor += 1;
            continue;
        }
        assert_eq!(
            body,
            CORPUS_DELIMITER,
            "expected corpus case delimiter in {} at line {}",
            path.display(),
            cursor + 1
        );
        assert!(
            cursor + 2 < lines.len(),
            "truncated corpus case header in {} at line {}",
            path.display(),
            cursor + 1
        );
        let name = line_body(contents, lines[cursor + 1]);
        assert!(
            !name.is_empty() && name == name.trim(),
            "malformed corpus case name in {} at line {}: {name:?}",
            path.display(),
            cursor + 2
        );
        assert_eq!(
            line_body(contents, lines[cursor + 2]),
            CORPUS_DELIMITER,
            "malformed corpus case header in {} at line {}",
            path.display(),
            cursor + 3
        );

        let mut source_line = cursor + 3;
        // The blank line after the header is corpus framing, not source text.
        if source_line < lines.len() && line_body(contents, lines[source_line]).is_empty() {
            source_line += 1;
        }
        let separator_line = (source_line..lines.len())
            .find(|&index| line_body(contents, lines[index]) == "---")
            .unwrap_or_else(|| {
                panic!(
                    "corpus case {name:?} in {} has no `---` source separator",
                    path.display()
                )
            });
        let mut source_end = lines[separator_line].0;
        // The blank line before `---` is also corpus framing.
        if separator_line > source_line && line_body(contents, lines[separator_line - 1]).is_empty()
        {
            source_end = lines[separator_line - 1].0;
        }
        let source = contents[lines[source_line].0..source_end].to_owned();

        let expected_tree_start = lines[separator_line].1;
        let next_case = ((separator_line + 1)..lines.len())
            .find(|&index| line_body(contents, lines[index]) == CORPUS_DELIMITER);
        let expected_tree_end = next_case.map_or(contents.len(), |index| lines[index].0);
        let expected_tree = contents[expected_tree_start..expected_tree_end].trim();
        assert!(
            !expected_tree.is_empty(),
            "corpus case {name:?} in {} has no expected tree",
            path.display()
        );

        cases.push((
            name.to_owned(),
            CorpusCase {
                source,
                expected_tree: expected_tree.to_owned(),
                path: path.to_owned(),
            },
        ));
        cursor = next_case.unwrap_or(lines.len());
    }

    cases
}

fn corpus_cases() -> BTreeMap<String, CorpusCase> {
    let mut paths = fs::read_dir(corpus_directory())
        .unwrap_or_else(|error| panic!("read accepted corpus directory: {error}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read accepted corpus directory entry: {error}"))
                .path()
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.to_str() == Some("txt"))
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut cases = BTreeMap::new();
    for path in paths {
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read corpus file {}: {error}", path.display()));
        for (name, case) in parse_corpus_file(&path, &contents) {
            if let Some(previous) = cases.insert(name.clone(), case) {
                panic!(
                    "duplicate corpus case name {name:?} in {} and {}",
                    previous.path.display(),
                    path.display()
                );
            }
        }
    }
    assert!(!cases.is_empty(), "accepted corpus contains no cases");
    cases
}

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

    fn request_error(&mut self, method: &str, params: Value) -> Value {
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
            message.get("error").is_some(),
            "no error for {method}: {message}"
        );
        message["error"].clone()
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
    let capabilities = &result["capabilities"];
    assert!(
        capabilities["semanticTokensProvider"].is_object(),
        "semantic tokens capability: {result}"
    );
    assert_eq!(
        capabilities["positionEncoding"], "utf-16",
        "LSP positions use UTF-16 code units: {result}"
    );
    assert!(
        capabilities["diagnosticProvider"].is_object(),
        "diagnostic capability: {result}"
    );
    assert_eq!(
        capabilities["textDocumentSync"]["openClose"], true,
        "the server must advertise open/close synchronisation: {result}"
    );
    assert_eq!(
        capabilities["textDocumentSync"]["change"], 1,
        "the server must advertise full document synchronisation: {result}"
    );
    assert_eq!(
        capabilities["textDocumentSync"]["save"], true,
        "the server must advertise save synchronisation: {result}"
    );
    assert_eq!(
        capabilities["hoverProvider"], true,
        "the server must advertise hover support: {result}"
    );
    assert_eq!(
        capabilities["definitionProvider"], true,
        "the server must advertise definition support: {result}"
    );
    assert_eq!(
        capabilities["referencesProvider"], true,
        "the server must advertise reference support: {result}"
    );
    assert_eq!(
        capabilities["documentSymbolProvider"], true,
        "the server must advertise document-symbol support: {result}"
    );
    assert_eq!(
        capabilities["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","]),
        "the server must advertise signature-help triggers: {result}"
    );
    assert_eq!(
        capabilities["workspaceSymbolProvider"], true,
        "the server must advertise workspace-symbol support: {result}"
    );
    assert_eq!(
        capabilities["completionProvider"]["triggerCharacters"],
        json!([".", ":"]),
        "the server must advertise completion trigger characters: {result}"
    );
    assert_eq!(
        capabilities["semanticTokensProvider"]["range"], true,
        "the server must advertise semantic-token range requests: {result}"
    );
    assert_eq!(
        capabilities["semanticTokensProvider"]["full"], true,
        "the server must advertise full semantic-token requests: {result}"
    );
    assert_eq!(
        capabilities["diagnosticProvider"]["interFileDependencies"], false,
        "the server must keep diagnostics local to one document: {result}"
    );
    assert_eq!(
        capabilities["diagnosticProvider"]["workspaceDiagnostics"], false,
        "the server must not advertise workspace diagnostics: {result}"
    );
    client.notify("initialized", json!({}));
}

#[test]
fn rejects_initialize_when_client_offers_only_non_utf16_position_encoding() {
    let mut client = Client::spawn();
    let error = client.request_error(
        "initialize",
        json!({
            "processId": null,
            "rootUri": null,
            "capabilities": {
                "general": {
                    "positionEncodings": ["utf-8"]
                }
            }
        }),
    );

    assert_eq!(error["code"], -32602);
    let message = error["message"].as_str().expect("error message");
    assert!(
        message.contains("unsupported position encoding"),
        "initialize error should identify the unsupported encoding: {error}"
    );
    assert!(
        message.contains("UTF-16"),
        "initialize error should identify the supported encoding: {error}"
    );

    let status = client.child.wait().expect("wait for server exit");
    assert!(
        !status.success(),
        "unsupported negotiation must fail: {status}"
    );
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
    let byte = byte.min(source.len());
    assert!(
        source.is_char_boundary(byte),
        "position byte must be a boundary"
    );
    let starts = line_starts(source);
    let line = starts
        .partition_point(|&start| start <= byte)
        .saturating_sub(1);
    let line_start = starts[line];
    let line_end = line_end_byte(source, &starts, line);
    let character = source[line_start..byte.min(line_end)]
        .chars()
        .map(|source_character| source_character.len_utf16() as u64)
        .sum::<u64>();
    json!({ "line": line as u64, "character": character })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosticProjection {
    code: String,
    start_byte: usize,
    end_byte: usize,
    message: String,
}

/// Runs the same compiler-backed source check used by the offline source
/// checker, retaining its public diagnostic code, UTF-8 byte span, and text.
fn canonical_source_check_diagnostics(
    source: &str,
    logical_path: &str,
) -> Vec<DiagnosticProjection> {
    let snapshot =
        retained_standard_library_v11_snapshot().expect("retained V11 standard snapshot");
    let verified =
        verify_standard_library_v11_snapshot(snapshot).expect("verified V11 standard snapshot");
    let standard = check_standard_library_source(&verified).expect("checked standard source");
    let bundle = SourceBundle::new([SourceUnit::new(logical_path, source)])
        .expect("one nonempty logical source unit");
    let report = check_new_application(&bundle, &standard).expect("canonical source check");

    report
        .diagnostics()
        .iter()
        .map(|diagnostic| DiagnosticProjection {
            code: diagnostic.code().as_str().to_owned(),
            start_byte: diagnostic.location().span().start(),
            end_byte: diagnostic.location().span().end(),
            message: diagnostic.message().to_owned(),
        })
        .collect()
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, source_character) in source.char_indices() {
        if source_character == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// Returns the line end used by `PositionMapper`: LF is the line boundary,
/// and a CR immediately before that LF is part of the terminator rather than
/// the preceding line's text.
fn line_end_byte(source: &str, line_starts: &[usize], line: usize) -> usize {
    match line_starts.get(line + 1) {
        Some(&next_start) => {
            let line_end = next_start.saturating_sub(1);
            if source.as_bytes().get(line_end.saturating_sub(1)) == Some(&b'\r') {
                line_end.saturating_sub(1)
            } else {
                line_end
            }
        }
        None => source.len(),
    }
}

/// Converts an LSP line/UTF-16 position back to the source check's byte
/// coordinate. This intentionally mirrors `PositionMapper`: lines are split
/// on LF, CRLF's two terminator bytes share the preceding line-end position,
/// non-ASCII scalars contribute their UTF-16 width, and returned offsets stay
/// on UTF-8 character boundaries.
fn byte_offset_from_lsp_position(source: &str, position: &Value) -> usize {
    let target_line = position["line"].as_u64().expect("LSP diagnostic line");
    let target_character = position["character"]
        .as_u64()
        .expect("LSP diagnostic UTF-16 character");
    let starts = line_starts(source);
    let target_line = target_line as usize;
    let line_start = *starts.get(target_line).expect("diagnostic line must exist");
    let line_end = line_end_byte(source, &starts, target_line);
    let mut character = target_character as usize;

    for (index, source_character) in source[line_start..line_end].char_indices() {
        if character == 0 {
            return line_start + index;
        }
        let width = source_character.len_utf16();
        if character <= width {
            return line_start + index + source_character.len_utf8();
        }
        character -= width;
    }

    assert_eq!(character, 0, "diagnostic character must exist");
    line_end
}

fn lsp_diagnostic_ranges(source: &str, diagnostics: &[DiagnosticProjection]) -> Vec<Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "start": position_at_byte(source, diagnostic.start_byte),
                "end": position_at_byte(source, diagnostic.end_byte),
            })
        })
        .collect()
}

fn source_check_parity_cases() -> Vec<(&'static str, String)> {
    let escaped_name = "a\\b\n\r\t\u{001b}\u{2028}\u{2029}é";
    vec![
        ("lf", SOURCE_CHECK_PARITY_ASCII_SOURCE.to_owned()),
        (
            "crlf",
            SOURCE_CHECK_PARITY_ASCII_SOURCE.replace('\n', "\r\n"),
        ),
        (
            "no-final-lf",
            SOURCE_CHECK_PARITY_ASCII_SOURCE
                .strip_suffix('\n')
                .expect("parity fixture final LF")
                .to_owned(),
        ),
        ("bom", format!("\u{FEFF}{SOURCE_CHECK_PARITY_ASCII_SOURCE}")),
        (
            "bom-interior",
            "CREATE\u{FEFF} SCHEMA bom_interior;\n".to_owned(),
        ),
        (
            "word-joiner-leading",
            "\u{2060}CREATE SCHEMA word_joiner_leading;\n".to_owned(),
        ),
        (
            "word-joiner-interior",
            "CREATE\u{2060} SCHEMA word_joiner_interior;\n".to_owned(),
        ),
        ("unicode", SOURCE_CHECK_PARITY_SOURCE.to_owned()),
        (
            "multiple",
            concat!(
                "CREATE TYPE first.task AS OBJECT (done BOOLEAN);\n",
                "CREATE TYPE second.task AS OBJECT (done BOOLEAN);\n",
            )
            .to_owned(),
        ),
        (
            "escaped-controls",
            format!("CREATE SCHEMA \"{escaped_name}\";\nCREATE SCHEMA \"{escaped_name}\";"),
        ),
    ]
}

fn lsp_diagnostic_projections(source: &str, diagnostics: &Value) -> Vec<DiagnosticProjection> {
    diagnostics
        .as_array()
        .expect("LSP diagnostic array")
        .iter()
        .map(|diagnostic| {
            let range = &diagnostic["range"];
            DiagnosticProjection {
                code: diagnostic["code"]
                    .as_str()
                    .expect("LSP diagnostic code")
                    .to_owned(),
                start_byte: byte_offset_from_lsp_position(source, &range["start"]),
                end_byte: byte_offset_from_lsp_position(source, &range["end"]),
                message: diagnostic["message"]
                    .as_str()
                    .expect("LSP diagnostic message")
                    .to_owned(),
            }
        })
        .collect()
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

fn read_case_diagnostics(client: &mut Client, case_name: &str, path: &Path) -> Value {
    let message = client.read_message();
    assert_eq!(
        message["method"],
        "textDocument/publishDiagnostics",
        "accepted corpus fixture {case_name:?} from {} expected a diagnostics notification, got {message}",
        path.display()
    );
    message["params"].clone()
}

fn case_position_byte_offset(
    source: &str,
    position: &Value,
    case_name: &str,
    path: &Path,
    diagnostic_index: usize,
    endpoint: &str,
) -> usize {
    let context = format!(
        "accepted corpus fixture {case_name:?} from {} diagnostic {diagnostic_index} {endpoint}",
        path.display()
    );
    let line = position
        .get("line")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{context} has no unsigned line"));
    let line = usize::try_from(line)
        .unwrap_or_else(|_| panic!("{context} line number does not fit in usize"));
    let character = position
        .get("character")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{context} has no unsigned UTF-16 character"));
    let starts = line_starts(source);
    let line_start = *starts
        .get(line)
        .unwrap_or_else(|| panic!("{context} line {line} is outside the source"));
    let line_end = line_end_byte(source, &starts, line);
    let line_width = source[line_start..line_end]
        .chars()
        .map(|source_character| source_character.len_utf16() as u64)
        .sum::<u64>();
    assert!(
        character <= line_width,
        "{context} UTF-16 character {character} exceeds line width {line_width}"
    );
    byte_offset_from_lsp_position(source, position)
}

fn assert_case_diagnostic_ranges(
    source: &str,
    diagnostics: &Value,
    case_name: &str,
    path: &Path,
) -> usize {
    let items = diagnostics
        .get("diagnostics")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "accepted corpus fixture {case_name:?} from {} returned a diagnostics notification without an array: {diagnostics}",
                path.display()
            )
        });
    for (diagnostic_index, diagnostic) in items.iter().enumerate() {
        let range = diagnostic
            .get("range")
            .and_then(Value::as_object)
            .unwrap_or_else(|| {
                panic!(
                    "accepted corpus fixture {case_name:?} from {} diagnostic {diagnostic_index} has no range: {diagnostic}",
                    path.display()
                )
            });
        let start = range.get("start").unwrap_or_else(|| {
            panic!(
                "accepted corpus fixture {case_name:?} from {} diagnostic {diagnostic_index} has no range start: {diagnostic}",
                path.display()
            )
        });
        let end = range.get("end").unwrap_or_else(|| {
            panic!(
                "accepted corpus fixture {case_name:?} from {} diagnostic {diagnostic_index} has no range end: {diagnostic}",
                path.display()
            )
        });
        let start_byte = case_position_byte_offset(
            source,
            start,
            case_name,
            path,
            diagnostic_index,
            "range start",
        );
        let end_byte =
            case_position_byte_offset(source, end, case_name, path, diagnostic_index, "range end");
        assert!(
            start_byte <= end_byte,
            "accepted corpus fixture {case_name:?} from {} diagnostic {diagnostic_index} has a reversed UTF-16 range {start:?}..{end:?}",
            path.display()
        );
    }
    items.len()
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

#[test]
fn serves_accepted_corpus_manifest_diagnostics_with_valid_utf16_ranges() {
    let names = accepted_case_names();
    let cases = corpus_cases();
    let mut client = Client::spawn();
    initialize(&mut client);

    for (index, name) in names.iter().enumerate() {
        let case = cases.get(name).unwrap_or_else(|| {
            panic!(
                "accepted corpus manifest case {name:?} has no source fixture under {}",
                corpus_directory().display()
            )
        });
        let uri = format!("file:///test/accepted-corpus/{:03}.orna", index + 1);
        open_document(&mut client, &uri, &case.source, (index + 1) as i64);
        let diagnostics = read_case_diagnostics(&mut client, name, &case.path);
        assert_eq!(
            diagnostics.get("uri").and_then(Value::as_str),
            Some(uri.as_str()),
            "accepted corpus fixture {name:?} from {} reported the wrong diagnostics URI: {diagnostics}",
            case.path.display()
        );
        let diagnostic_count =
            assert_case_diagnostic_ranges(&case.source, &diagnostics, name, &case.path);
        if case.expected_tree.contains("(ERROR") {
            assert!(
                diagnostic_count > 0,
                "accepted corpus fixture {name:?} from {} has an `(ERROR ...)` tree but no LSP diagnostics",
                case.path.display()
            );
        }
    }

    client.shutdown();
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecodedSemanticToken {
    line: u64,
    character: u64,
    length: u64,
    token_type: u64,
    modifiers: u64,
}

fn decode_semantic_tokens(result: &Value) -> Vec<DecodedSemanticToken> {
    let data = result["data"].as_array().expect("semantic token data");
    assert_eq!(data.len() % 5, 0, "tokens are delta quintuples");

    let mut line = 0;
    let mut character = 0;
    data.chunks_exact(5)
        .map(|token| {
            let delta_line = token[0].as_u64().expect("delta line");
            let delta_start = token[1].as_u64().expect("delta start");
            if delta_line == 0 {
                character += delta_start;
            } else {
                line += delta_line;
                character = delta_start;
            }
            DecodedSemanticToken {
                line,
                character,
                length: token[2].as_u64().expect("token length"),
                token_type: token[3].as_u64().expect("token type"),
                modifiers: token[4].as_u64().expect("token modifiers"),
            }
        })
        .collect()
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
fn serves_accepted_client_semantic_tokens_with_utf16_and_nested_ranges() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/accepted-client-semantic.orna";
    // Keep the canonical fixture intact while exercising a UTF-16 offset before CREATE.
    let source = format!("/* 😀 */ {ACCEPTED_CLIENT_SOURCE}");

    open_document(&mut client, uri, &source, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(
        diagnostics["diagnostics"],
        json!([]),
        "accepted source clean"
    );

    let tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    ));
    assert!(tokens.len() >= 3, "accepted fixture semantic tokens");
    let expected_prefix = vec![
        DecodedSemanticToken {
            line: 0,
            character: 0,
            length: 8,
            token_type: 8,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 0,
            character: 9,
            length: 6,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 0,
            character: 16,
            length: 6,
            token_type: 0,
            modifiers: 0,
        },
    ];
    assert_eq!(
        &tokens[..3],
        expected_prefix.as_slice(),
        "accepted fixture prefix tokens in source order"
    );

    let function_line: Vec<_> = tokens
        .iter()
        .filter(|token| token.line == 6)
        .cloned()
        .collect();
    let expected_function_line = vec![
        DecodedSemanticToken {
            line: 6,
            character: 0,
            length: 6,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 6,
            character: 7,
            length: 6,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 6,
            character: 14,
            length: 8,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 6,
            character: 23,
            length: 15,
            token_type: 4,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 6,
            character: 39,
            length: 8,
            token_type: 2,
            modifiers: 0,
        },
    ];
    assert_eq!(
        function_line, expected_function_line,
        "accepted CLIENT declaration tokens in source order"
    );

    let state_line: Vec<_> = tokens
        .iter()
        .filter(|token| token.line == 9)
        .cloned()
        .collect();
    let expected_state_line = vec![
        DecodedSemanticToken {
            line: 9,
            character: 4,
            length: 5,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 9,
            character: 10,
            length: 5,
            token_type: 3,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 9,
            character: 16,
            length: 7,
            token_type: 1,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 9,
            character: 24,
            length: 5,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 9,
            character: 30,
            length: 5,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 9,
            character: 36,
            length: 7,
            token_type: 0,
            modifiers: 0,
        },
        DecodedSemanticToken {
            line: 9,
            character: 44,
            length: 4,
            token_type: 0,
            modifiers: 0,
        },
    ];
    assert_eq!(
        state_line, expected_state_line,
        "nested CLIENT state tokens in source order"
    );

    client.shutdown();
}

#[test]
fn serves_accepted_order_by_semantic_tokens_with_utf16_positions() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/accepted-order-by-semantic.orna";
    open_document(&mut client, uri, ORDER_BY_SOURCE, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(
        diagnostics["diagnostics"],
        json!([]),
        "accepted ORDER BY source clean"
    );

    let tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    ));
    assert_eq!(
        tokens
            .iter()
            .find(|token| token.line == 7 && token.character == 71),
        Some(&DecodedSemanticToken {
            line: 7,
            character: 71,
            length: 3,
            token_type: 0,
            modifiers: 0,
        }),
        "ASC is a keyword at its UTF-16 position"
    );
    assert_eq!(
        tokens
            .iter()
            .find(|token| token.line == 7 && token.character == 87),
        Some(&DecodedSemanticToken {
            line: 7,
            character: 87,
            length: 4,
            token_type: 0,
            modifiers: 0,
        }),
        "DESC is a keyword at its UTF-16 position"
    );

    client.shutdown();
}

#[test]
fn serves_valid_update_delete_mutations() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/valid-mutations.orna";

    open_clean_document(&mut client, uri, MUTATION_SOURCE);

    let update_field = position_inside(
        MUTATION_SOURCE,
        "AS UPDATE mutation_test.item AS updated\nSET ",
        "stored",
    );
    assert_hover_contains(&mut client, uri, update_field.clone(), "**field**");
    assert_definition_starts_on(&mut client, uri, update_field, 2);

    let delete_start = MUTATION_SOURCE
        .find("AS DELETE FROM mutation_test.item AS deleted")
        .expect("DELETE statement")
        + "AS ".len();
    let delete_position = position_at_byte(MUTATION_SOURCE, delete_start);
    let delete_line = delete_position["line"].as_u64().expect("DELETE line");
    let delete_character = delete_position["character"]
        .as_u64()
        .expect("DELETE character");
    let tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    ));
    assert_eq!(
        tokens
            .iter()
            .find(|token| { token.line == delete_line && token.character == delete_character }),
        Some(&DecodedSemanticToken {
            line: delete_line,
            character: delete_character,
            length: 6,
            token_type: 0,
            modifiers: 0,
        }),
        "DELETE is tokenized as a keyword at its source position"
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
fn serves_ambiguous_target_references_fail_closed() {
    let mut client = Client::spawn();
    initialize(&mut client);

    let uri = "file:///test/ambiguous-action-target.orna";
    let source = format!(
        "{}\nCREATE SCHEMA target_fixture;\nCREATE TYPE target_fixture.row AS OBJECT (value INTEGER NOT NULL);\nCREATE SERVER FUNCTION action_fixture.local(p_value INTEGER)\nRETURNS INTEGER\nAS SELECT t.value FROM target_fixture.row t;\n",
        ACTION_SOURCE
    );
    open_document(&mut client, uri, &source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let target = position_inside(&source, "target => action_fixture.", "local");
    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": target,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        references,
        json!([]),
        "ambiguous target references: {references}"
    );

    client.shutdown();
}

#[test]
fn serves_target_function_navigation_and_semantic_kind_only_for_accepted_calls() {
    let mut client = Client::spawn();
    initialize(&mut client);

    let stream_uri = "file:///test/stream-resource-target-navigation.orna";
    open_clean_document(&mut client, stream_uri, STREAM_RESOURCE_SOURCE);
    let stream_target = position_inside(
        STREAM_RESOURCE_SOURCE,
        "target => stream_fixture.",
        "events",
    );
    assert_hover_contains(
        &mut client,
        stream_uri,
        stream_target.clone(),
        "server function",
    );
    assert_definition_starts_on(&mut client, stream_uri, stream_target.clone(), 6);
    let stream_target_position = stream_target.as_object().expect("stream target position");
    let stream_target_line = stream_target_position["line"]
        .as_u64()
        .expect("stream target line");
    let stream_target_character = stream_target_position["character"]
        .as_u64()
        .expect("stream target character")
        - 1;
    let stream_tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": stream_uri } }),
    ));
    assert!(
        stream_tokens.iter().any(|token| {
            token.line == stream_target_line
                && token.character == stream_target_character
                && token.length == "events".len() as u64
                && token.token_type == 2
        }),
        "target function must use the function semantic token: {stream_tokens:?}"
    );

    let stream_declaration = position_inside(
        STREAM_RESOURCE_SOURCE,
        "CREATE SERVER FUNCTION stream_fixture.",
        "events",
    );
    let stream_declaration_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": stream_uri },
            "position": stream_declaration,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        stream_declaration_references,
        json!([
            {
                "uri": stream_uri,
                "range": {
                    "start": { "line": 6, "character": 38 },
                    "end": { "line": 6, "character": 44 },
                },
            },
            {
                "uri": stream_uri,
                "range": {
                    "start": { "line": 17, "character": 33 },
                    "end": { "line": 17, "character": 39 },
                },
            },
        ]),
        "stream target declaration references: {stream_declaration_references}"
    );

    let action_uri = "file:///test/action-target-navigation.orna";
    open_clean_document(&mut client, action_uri, ACTION_SOURCE);
    let action_target = position_inside(ACTION_SOURCE, "target => action_fixture.", "local");
    assert_hover_contains(
        &mut client,
        action_uri,
        action_target.clone(),
        "client function",
    );
    assert_definition_starts_on(&mut client, action_uri, action_target.clone(), 8);
    let action_target_position = action_target.as_object().expect("action target position");
    let action_target_line = action_target_position["line"]
        .as_u64()
        .expect("action target line");
    let action_target_character = action_target_position["character"]
        .as_u64()
        .expect("action target character")
        - 1;
    let action_tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": action_uri } }),
    ));
    assert!(
        action_tokens.iter().any(|token| {
            token.line == action_target_line
                && token.character == action_target_character
                && token.length == "local".len() as u64
                && token.token_type == 2
        }),
        "action target must use the function semantic token: {action_tokens:?}"
    );
    let action_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": action_uri },
            "position": action_target,
            "context": { "includeDeclaration": true },
        }),
    );
    let action_reference_lines: Vec<u64> = action_references
        .as_array()
        .expect("action target references")
        .iter()
        .map(|reference| {
            reference["range"]["start"]["line"]
                .as_u64()
                .expect("reference line")
        })
        .collect();
    assert_eq!(
        action_reference_lines,
        vec![8, 13],
        "target references: {action_references}"
    );

    let action_declaration = position_inside(
        ACTION_SOURCE,
        "CREATE CLIENT FUNCTION action_fixture.",
        "local",
    );
    let action_declaration_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": action_uri },
            "position": action_declaration,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        action_declaration_references,
        json!([
            {
                "uri": action_uri,
                "range": {
                    "start": { "line": 8, "character": 42 },
                    "end": { "line": 8, "character": 47 },
                },
            },
            {
                "uri": action_uri,
                "range": {
                    "start": { "line": 13, "character": 31 },
                    "end": { "line": 13, "character": 36 },
                },
            },
        ]),
        "action target declaration references: {action_declaration_references}"
    );

    let expression_uri = "file:///test/non-target-field-path.orna";
    open_clean_document(&mut client, expression_uri, EXPRESSION_CLIENT_SOURCE);
    let field_position = position_inside(EXPRESSION_CLIENT_SOURCE, "AS p_item.", "title");
    let field_position = field_position.as_object().expect("field position");
    let field_line = field_position["line"].as_u64().expect("field line");
    let field_character = field_position["character"]
        .as_u64()
        .expect("field character")
        - 1;
    let field_tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": expression_uri } }),
    ));
    assert!(
        field_tokens.iter().any(|token| {
            token.line == field_line
                && token.character == field_character
                && token.length == "title".len() as u64
                && token.token_type == 5
        }),
        "ordinary field paths must remain properties: {field_tokens:?}"
    );
    assert!(!field_tokens.iter().any(|token| {
        token.line == field_line
            && token.character == field_character
            && token.length == "title".len() as u64
            && token.token_type == 2
    }));

    client.shutdown();
}

#[test]
fn serves_canonical_accepted_dogfood_fixtures_without_diagnostics() {
    let fixtures = [
        (
            "client_function_dogfood.orna",
            include_str!("../../orna-server/tests/fixtures/client_function_dogfood.orna"),
        ),
        ("scalar_resource_dogfood.orna", SCALAR_RESOURCE_SOURCE),
        ("stream_resource_dogfood.orna", STREAM_RESOURCE_SOURCE),
        ("action_dogfood.orna", ACTION_SOURCE),
        ("client_inspector_dogfood.orna", INSPECTOR_SOURCE),
        ("expression_client_dogfood.orna", EXPRESSION_CLIENT_SOURCE),
        ("server_function_dogfood.orna", SERVER_FUNCTION_SOURCE),
        (
            "client_local_assignment_dogfood.orna",
            CLIENT_LOCAL_ASSIGNMENT_SOURCE,
        ),
        ("client_state_dogfood.orna", CLIENT_STATE_SOURCE),
    ];

    let mut client = Client::spawn();
    initialize(&mut client);
    for (name, source) in fixtures {
        let uri = format!("file:///test/{name}");
        open_clean_document(&mut client, &uri, source);
    }
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
        &[
            "expr",
            "literal",
            "composed",
            "item",
            "ref_composed",
            "external",
        ],
    );
    assert_semantic_tokens_present(&mut client, uri);
    let tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    ));
    let assert_token = |prefix: &str, token: &str, token_type: u64| {
        let position = position_inside(EXPRESSION_CLIENT_SOURCE, prefix, token);
        let line = position["line"].as_u64().expect("token line");
        let character = position["character"].as_u64().expect("token character")
            - token
                .chars()
                .next()
                .expect("token first character")
                .len_utf16() as u64;
        assert!(
            tokens.iter().any(|semantic_token| {
                semantic_token.line == line
                    && semantic_token.character == character
                    && semantic_token.length == token.len() as u64
                    && semantic_token.token_type == token_type
            }),
            "missing semantic token {token:?} at {line}:{character} type {token_type}: {tokens:?}"
        );
    };
    assert_token("AS p_item.", "title", 5);
    assert_token("AS p_item.title ", "||", 9);
    assert!(
        tokens.iter().any(|semantic_token| {
            semantic_token.line == 14
                && semantic_token.character == 19
                && semantic_token.length == 3
                && semantic_token.token_type == 6
        }),
        "missing concatenation string token: {tokens:?}"
    );
    let completion = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": position_after(EXPRESSION_CLIENT_SOURCE, "AS p_item."),
            "context": { "triggerKind": 2, "triggerCharacter": "." },
        }),
    );
    let labels: Vec<&str> = completion
        .as_array()
        .expect("completion items")
        .iter()
        .map(|item| item["label"].as_str().expect("completion label"))
        .collect();
    assert!(
        labels.contains(&"title"),
        "dotted CLIENT field completion: {labels:?}"
    );
    assert!(
        completion
            .as_array()
            .expect("completion items")
            .iter()
            .all(|item| !item["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("function target"))),
        "ordinary field paths must not receive target completion details: {completion}"
    );
    assert!(
        labels.contains(&"CREATE"),
        "global completion preserved: {labels:?}"
    );

    let incomplete_source = "CREATE CLIENT FUNCTION app.probe(p_item REF expr.item) RETURNS TEXT AS p_item.;\n";
    let incomplete_parse = orna_syntax::parse(incomplete_source);
    assert!(
        incomplete_parse.diagnostics().iter().any(|diagnostic| diagnostic.message.contains("CLIENT expression dot")),
        "incomplete CLIENT member path must remain an explicit parser diagnostic"
    );

    assert_hover_contains(
        &mut client,
        uri,
        position_inside(EXPRESSION_CLIENT_SOURCE, "AS p_item.", "title"),
        "field",
    );
    assert_definition_starts_on(
        &mut client,
        uri,
        position_inside(EXPRESSION_CLIENT_SOURCE, "AS p_item.", "title"),
        10,
    );
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
fn serves_signature_help_and_workspace_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/extended-requests.orna";
    let source = concat!(
        "CREATE SCHEMA request_test;\n",
        "CREATE SERVER FUNCTION request_test.echo(p_value INTEGER, p_other BOOLEAN)\n",
        "RETURNS INTEGER AS SELECT p_value;\n",
        "CREATE SERVER FUNCTION request_test.call()\n",
        "RETURNS INTEGER AS SELECT request_test.echo(1, TRUE);\n",
    );
    open_document(&mut client, uri, source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let signature = client.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(source, "SELECT request_test.echo(", "1"),
        }),
    );
    assert!(
        signature.is_null() || signature["signatures"].is_array(),
        "signature-help response must use the LSP shape: {signature}"
    );

    let workspace_symbols = client.request("workspace/symbol", json!({ "query": "echo" }));
    assert!(
        workspace_symbols
            .as_array()
            .is_some_and(|symbols| symbols.iter().any(|symbol| symbol["name"] == "echo")),
        "workspace symbols must find the opened function: {workspace_symbols}"
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
fn serves_standard_function_hover_signature_and_unknown_fallback() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/standard-function.orna";
    let source =
        "CREATE CLIENT FUNCTION app.probe() RETURNS INTEGER RETURN std.math.increment(1);\n";
    open_document(&mut client, uri, source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(source, "RETURN ", "increment"),
        }),
    );
    let hover_value = hover["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("standard function hover response has no markdown value"));
    assert!(
        hover_value.contains("**CLIENT function**"),
        "standard hover domain: {hover_value}"
    );
    assert!(
        hover_value.contains("CLIENT FUNCTION std.math.increment"),
        "standard hover name: {hover_value}"
    );
    assert!(
        hover_value.contains("p_value"),
        "standard hover parameter: {hover_value}"
    );
    assert_eq!(
        hover_value.lines().find(|line| line.contains("RETURNS")),
        Some("CLIENT FUNCTION std.math.increment(p_value) RETURNS INTEGER"),
        "standard hover return type: {hover_value}",
    );

    let signature = client.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": uri },
            "position": position_after(source, "std.math.increment("),
        }),
    );
    let signature = signature.as_object().expect("standard signature response");
    let signatures = signature
        .get("signatures")
        .and_then(Value::as_array)
        .expect("standard signatures");
    assert_eq!(signatures.len(), 1);
    let label = signatures[0]["label"]
        .as_str()
        .expect("standard signature label");
    assert!(label.contains("CLIENT FUNCTION std.math.increment"));
    assert!(label.contains("p_value"));
    assert!(
        label.contains("RETURNS"),
        "standard signature return type: {label}"
    );

    let unknown_uri = "file:///test/unknown-standard-function.orna";
    let unknown_source =
        "CREATE CLIENT FUNCTION app.probe() RETURNS INTEGER RETURN std.math.unknown(1);\n";
    open_document(&mut client, unknown_uri, unknown_source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");
    let unknown_signature = client.request(
        "textDocument/signatureHelp",
        json!({
            "textDocument": { "uri": unknown_uri },
            "position": position_after(unknown_source, "std.math.unknown("),
        }),
    );
    assert!(
        unknown_signature.is_null(),
        "unknown standard function must fail closed: {unknown_signature}"
    );

    client.shutdown();
}

#[test]
fn serves_rich_hover_content() {
    let fixture_root =
        std::env::temp_dir().join(format!("orna-lsp-rich-hover-{}", std::process::id()));
    let spec_directory = fixture_root.join("spec").join("spec");
    fs::create_dir_all(&spec_directory).expect("spec fixture directory");
    fs::write(spec_directory.join("orna.ebnf"), "start = 'fixture';\n").expect("spec fixture");
    let document_path = fixture_root.join("rich-hover.orna");
    fs::write(&document_path, VALID_SOURCE).expect("rich-hover fixture");
    let uri = format!("file://{}", document_path.display());

    let mut client = Client::spawn();
    initialize(&mut client);
    // The document sits beside a temporary spec bundle so hovers carry a
    // deterministic Spec link without depending on a sibling checkout.
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
    fs::remove_dir_all(fixture_root).expect("remove rich-hover fixture");
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
fn serves_final_field_name_through_accepted_rename_transition() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/field-rename.orna";

    open_document(&mut client, uri, FIELD_RENAME_SOURCE, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(diagnostics["uri"], uri);
    let items = diagnostics["diagnostics"].as_array().expect("diagnostics");
    assert_eq!(
        items.len(),
        1,
        "rename transition has one base-catalogue diagnostic"
    );
    assert_eq!(items[0]["code"], "ORNA0101");
    assert_eq!(
        items[0]["message"],
        "field rename requires existing object type people.person"
    );
    assert_eq!(
        items[0]["range"],
        json!({
            "start": { "line": 4, "character": 11 },
            "end": { "line": 4, "character": 24 },
        })
    );

    let tokens = decode_semantic_tokens(&client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    ));
    let rename_line: Vec<_> = tokens
        .iter()
        .filter(|token| token.line == 5)
        .cloned()
        .collect();
    assert_eq!(
        rename_line,
        vec![
            DecodedSemanticToken {
                line: 5,
                character: 4,
                length: 6,
                token_type: 0,
                modifiers: 0,
            },
            DecodedSemanticToken {
                line: 5,
                character: 11,
                length: 5,
                token_type: 0,
                modifiers: 0,
            },
            DecodedSemanticToken {
                line: 5,
                character: 17,
                length: 5,
                token_type: 5,
                modifiers: 0,
            },
            DecodedSemanticToken {
                line: 5,
                character: 23,
                length: 2,
                token_type: 0,
                modifiers: 0,
            },
            DecodedSemanticToken {
                line: 5,
                character: 26,
                length: 13,
                token_type: 5,
                modifiers: 0,
            },
        ],
        "ALTER FIELD rename tokens preserve old and final property spellings"
    );

    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": uri } }),
    );
    let symbols = symbols.as_array().expect("document symbols");
    let person = symbols
        .iter()
        .find(|symbol| symbol["name"] == "person")
        .expect("person object symbol");
    let fields = person["children"].as_array().expect("object fields");
    assert!(
        fields.iter().any(|field| field["name"] == "primary_email"),
        "final field symbol present: {fields:?}"
    );
    assert!(
        fields.iter().all(|field| field["name"] != "email"),
        "transition-only old field is not a document symbol: {fields:?}"
    );

    let final_use = position_inside(FIELD_RENAME_SOURCE, "SELECT person.", "primary_email");
    assert_hover_contains(&mut client, uri, final_use.clone(), "**field**");
    assert_hover_contains(&mut client, uri, final_use.clone(), "TEXT");
    assert_definition_starts_on(&mut client, uri, final_use.clone(), 2);

    let renamed_name = position_inside(
        FIELD_RENAME_SOURCE,
        "RENAME FIELD email TO ",
        "primary_email",
    );
    assert_hover_contains(&mut client, uri, renamed_name.clone(), "**field**");
    assert_definition_starts_on(&mut client, uri, renamed_name.clone(), 2);

    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": renamed_name,
            "context": { "includeDeclaration": true },
        }),
    );
    let reference_locations: Vec<(u64, u64, u64, u64)> = references
        .as_array()
        .expect("final field references")
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
    let expected_references = [(2, 4, 2, 17), (5, 26, 5, 39), (9, 18, 9, 31)];
    assert_eq!(
        reference_locations.len(),
        expected_references.len(),
        "final field reference count: {references}"
    );
    for expected in expected_references {
        assert!(
            reference_locations.contains(&expected),
            "missing final field reference {expected:?}: {references}"
        );
    }

    let references_without_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": final_use,
            "context": { "includeDeclaration": false },
        }),
    );
    let references_without_declaration = references_without_declaration
        .as_array()
        .expect("final field references without declaration");
    let expected_without_declaration = [(5, 26, 5, 39), (9, 18, 9, 31)];
    assert_eq!(
        references_without_declaration.len(),
        expected_without_declaration.len(),
        "final field references without declaration: {references_without_declaration:?}"
    );
    for expected in expected_without_declaration {
        assert!(
            references_without_declaration.iter().any(|reference| {
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
                ) == expected
            }),
            "missing final field reference without declaration {expected:?}: {references_without_declaration:?}"
        );
    }

    let old_name = position_inside(FIELD_RENAME_SOURCE, "RENAME FIELD ", "email");
    assert!(
        client
            .request(
                "textDocument/hover",
                json!({ "textDocument": { "uri": uri }, "position": old_name.clone() }),
            )
            .is_null(),
        "old rename spelling is transition-only"
    );
    assert!(
        client
            .request(
                "textDocument/definition",
                json!({ "textDocument": { "uri": uri }, "position": old_name.clone() }),
            )
            .is_null(),
        "old rename spelling has no definition"
    );
    let old_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": old_name,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        old_references,
        json!([]),
        "old rename spelling has no references"
    );

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

#[test]
fn serves_source_check_diagnostic_matrix_with_lsp_byte_span_parity() {
    // Keep the test-side conversion honest about the accepted CRLF policy:
    // both terminator bytes map to the previous line's end, while the next
    // byte starts the next LSP line. The inverse position is the CR byte,
    // exactly as PositionMapper::byte_offset defines it.
    let crlf = "first\r\nsecond\r\n";
    assert_eq!(
        position_at_byte(crlf, 5),
        json!({ "line": 0, "character": 5 })
    );
    assert_eq!(
        position_at_byte(crlf, 6),
        json!({ "line": 0, "character": 5 })
    );
    assert_eq!(
        position_at_byte(crlf, 7),
        json!({ "line": 1, "character": 0 })
    );
    assert_eq!(
        byte_offset_from_lsp_position(crlf, &json!({ "line": 0, "character": 5 })),
        5
    );

    let cases = source_check_parity_cases();
    let mut client = Client::spawn();
    initialize(&mut client);

    for (version, (label, source)) in cases.iter().enumerate() {
        let uri = format!("file:///test/source-check-parity-{label}.orna");
        let canonical = canonical_source_check_diagnostics(source.as_str(), &uri);
        assert!(
            !canonical.is_empty(),
            "{label} canonical source check is clean"
        );

        match *label {
            "lf" | "crlf" | "no-final-lf" => {
                let start_byte = source.find("app.task").expect("unknown object type name");
                assert_eq!(
                    canonical,
                    vec![DiagnosticProjection {
                        code: "ORNA0101".to_owned(),
                        start_byte,
                        end_byte: start_byte + "app.task".len(),
                        message: "unknown schema app for object type app.task".to_owned(),
                    }],
                    "{label} canonical diagnostic"
                );
            }
            "bom" => assert_eq!(
                canonical,
                vec![DiagnosticProjection {
                    code: "ORNA0001".to_owned(),
                    start_byte: 0,
                    end_byte: "\u{FEFF}".len(),
                    message: "expected a CREATE, ALTER, or EXPORT declaration".to_owned(),
                }],
                "BOM must remain in the canonical source-check span"
            ),
            "bom-interior" | "word-joiner-interior" => {
                let unexpected = if *label == "bom-interior" {
                    '\u{FEFF}'
                } else {
                    '\u{2060}'
                };
                let start_byte = source
                    .find(unexpected)
                    .expect("interior invisible character");
                assert_eq!(
                    canonical,
                    vec![DiagnosticProjection {
                        code: "ORNA0001".to_owned(),
                        start_byte,
                        end_byte: start_byte + unexpected.len_utf8(),
                        message: "expected keyword SCHEMA".to_owned(),
                    }],
                    "{label} must retain the interior invisible-character span"
                );
            }
            "word-joiner-leading" => assert_eq!(
                canonical,
                vec![DiagnosticProjection {
                    code: "ORNA0001".to_owned(),
                    start_byte: 0,
                    end_byte: '\u{2060}'.len_utf8(),
                    message: "expected a CREATE, ALTER, or EXPORT declaration".to_owned(),
                }],
                "leading word joiner must remain in the canonical source-check span"
            ),
            "unicode" => {
                let start_byte = source.find("app.task").expect("unknown object type name");
                assert_eq!(
                    canonical,
                    vec![DiagnosticProjection {
                        code: "ORNA0101".to_owned(),
                        start_byte,
                        end_byte: start_byte + "app.task".len(),
                        message: "unknown schema app for object type app.task".to_owned(),
                    }],
                    "multibyte and combining canonical diagnostic"
                );
            }
            "multiple" => {
                let first_start = source.find("first.task").expect("first type name");
                let second_start = source.find("second.task").expect("second type name");
                assert_eq!(
                    canonical,
                    vec![
                        DiagnosticProjection {
                            code: "ORNA0101".to_owned(),
                            start_byte: first_start,
                            end_byte: first_start + "first.task".len(),
                            message: "unknown schema first for object type first.task".to_owned(),
                        },
                        DiagnosticProjection {
                            code: "ORNA0101".to_owned(),
                            start_byte: second_start,
                            end_byte: second_start + "second.task".len(),
                            message: "unknown schema second for object type second.task".to_owned(),
                        },
                    ],
                    "multiple canonical diagnostics must stay in source order"
                );
            }
            "escaped-controls" => {
                let escaped_name = "a\\b\n\r\t\u{001b}\u{2028}\u{2029}é";
                let first = format!("CREATE SCHEMA \"{escaped_name}\";\n");
                let start_byte = first.len() + "CREATE SCHEMA ".len();
                assert_eq!(
                    canonical,
                    vec![DiagnosticProjection {
                        code: "ORNA0103".to_owned(),
                        start_byte,
                        end_byte: start_byte + escaped_name.len() + 2,
                        message: format!("duplicate schema definition {escaped_name}"),
                    }],
                    "compiler diagnostic escaping must remain exact"
                );
            }
            other => panic!("unknown source-check parity case {other}"),
        }

        open_document(&mut client, &uri, source, (version + 1) as i64);
        let pushed = client.read_notification("textDocument/publishDiagnostics");
        assert_eq!(pushed["uri"], uri);
        let pushed_diagnostics = pushed["diagnostics"]
            .as_array()
            .expect("pushed diagnostic array");
        assert_eq!(
            pushed_diagnostics
                .iter()
                .map(|diagnostic| diagnostic["range"].clone())
                .collect::<Vec<_>>(),
            lsp_diagnostic_ranges(source, &canonical),
            "{label} push ranges must use the canonical byte spans"
        );
        assert_eq!(
            lsp_diagnostic_projections(source, &pushed["diagnostics"]),
            canonical,
            "{label} LSP push diagnostics must retain source-check code, byte span, and message"
        );

        let pull = client.request(
            "textDocument/diagnostic",
            json!({ "textDocument": { "uri": uri } }),
        );
        assert_eq!(pull["kind"], "full");
        assert_eq!(
            pull["items"], pushed["diagnostics"],
            "{label} push/pull parity"
        );
        assert_eq!(
            lsp_diagnostic_projections(source, &pull["items"]),
            canonical,
            "{label} LSP pull diagnostics must retain source-check code, byte span, and message"
        );
    }

    client.shutdown();
}

#[test]
fn serves_canonical_source_check_diagnostic_parity_for_one_invalid_fixture() {
    let source = SOURCE_CHECK_PARITY_ASCII_SOURCE;
    let uri = "file:///test/source-check-parity.orna";
    let canonical = canonical_source_check_diagnostics(source, uri);
    assert_eq!(canonical.len(), 1);

    let mut client = Client::spawn();
    initialize(&mut client);
    open_document(&mut client, uri, source, 1);
    let pushed = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(
        lsp_diagnostic_projections(source, &pushed["diagnostics"]),
        canonical,
        "check_document diagnostics must match source-check code, byte span, and message",
    );
    let pulled = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pulled["kind"], "full");
    assert_eq!(
        lsp_diagnostic_projections(source, &pulled["items"]),
        canonical,
        "pulled check_document diagnostics must match source-check code, byte span, and message",
    );
    client.shutdown();
}

#[test]
fn serves_syntax_diagnostic_for_malformed_schema_in_push_and_pull() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/syntax-invalid.orna";
    // The parser reports the semicolon at byte span 29..30. The emoji prefix
    // makes the corresponding LSP range use UTF-16 characters 27..28.
    let source = "/* 😀 */ CREATE SCHEMA crm.;";

    open_document(&mut client, uri, source, 1);
    let pushed = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(pushed["uri"], uri);
    let pushed_items = pushed["diagnostics"].as_array().expect("diagnostic items");
    assert_eq!(pushed_items.len(), 1, "syntax diagnostic: {pushed}");
    let pushed_diagnostic = &pushed_items[0];
    assert_eq!(
        pushed_diagnostic["range"],
        json!({
            "start": { "line": 0, "character": 27 },
            "end": { "line": 0, "character": 28 },
        })
    );
    assert_eq!(pushed_diagnostic["severity"], 1);
    assert_eq!(pushed_diagnostic["code"], "ORNA0001");
    assert_eq!(pushed_diagnostic["source"], "orna");
    assert_eq!(pushed_diagnostic["message"], "expected a name after '.'");

    let pull = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pull["kind"], "full");
    assert_eq!(pull["items"], pushed["diagnostics"]);

    client.shutdown();
}

#[test]
fn serves_client_resource_target_diagnostic_identically_in_push_and_pull() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/client-resource-target.orna";
    let source = SCALAR_RESOURCE_SOURCE.replace("std.invoke.echo", "scalar_fixture.call");

    open_document(&mut client, uri, &source, 1);
    let pushed = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(pushed["uri"], uri);
    let pushed_items = pushed["diagnostics"].as_array().expect("diagnostic items");
    assert_eq!(
        pushed_items.len(),
        1,
        "client resource target diagnostic: {pushed}"
    );
    let pushed_diagnostic = &pushed_items[0];
    assert_eq!(
        pushed_diagnostic["range"],
        json!({
            "start": { "line": 3, "character": 49 },
            "end": { "line": 3, "character": 68 },
        })
    );
    assert_eq!(pushed_diagnostic["severity"], 1);
    assert_eq!(pushed_diagnostic["code"], "ORNA0303");
    assert_eq!(pushed_diagnostic["source"], "orna");
    assert_eq!(
        pushed_diagnostic["message"],
        "resource target scalar_fixture.call must be a SERVER function"
    );

    let pull = client.request(
        "textDocument/diagnostic",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(pull["kind"], "full");
    assert_eq!(pull["items"], pushed["diagnostics"]);

    client.shutdown();
}

#[test]
fn qualified_name_navigation_keeps_same_final_names_in_their_namespace() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/qualified-navigation.orna";
    let source = concat!(
        "CREATE SCHEMA alpha;\n",
        "CREATE SCHEMA beta;\n",
        "CREATE TYPE alpha.item AS OBJECT (value BOOLEAN);\n",
        "CREATE TYPE beta.item AS OBJECT (value BOOLEAN);\n",
        "CREATE TYPE alpha.stage AS ENUM ('open');\n",
        "CREATE TYPE beta.stage AS ENUM ('closed');\n",
        "CREATE TYPE alpha.record AS VALUE (stage alpha.stage) IMMUTABLE PERSISTABLE;\n",
        "CREATE TYPE beta.record AS VALUE (stage beta.stage) IMMUTABLE PERSISTABLE;\n",
        "CREATE TYPE alpha.scalar AS VALUE PRIMITIVE KERNEL CONTRACT 'alpha.scalar@1' IMMUTABLE PERSISTABLE;\n",
        "CREATE TYPE beta.scalar AS VALUE PRIMITIVE KERNEL CONTRACT 'beta.scalar@1' IMMUTABLE PERSISTABLE;\n",
        "CREATE TYPE alpha.opaque AS VALUE OPAQUE KERNEL CONTRACT 'alpha.opaque@1' IMMUTABLE TRANSIENT;\n",
        "CREATE TYPE beta.opaque AS VALUE OPAQUE KERNEL CONTRACT 'beta.opaque@1' IMMUTABLE TRANSIENT;\n",
        "CREATE TYPE alpha.holder AS OBJECT (item alpha.item, stage alpha.stage, record alpha.record, scalar alpha.scalar, opaque alpha.opaque);\n",
        "CREATE TYPE beta.holder AS OBJECT (item beta.item, stage beta.stage, record beta.record, scalar beta.scalar, opaque beta.opaque);\n",
        "CREATE SERVER FUNCTION alpha.run() RETURNS BOOLEAN AS SELECT TRUE FROM alpha.holder t;\n",
        "CREATE SERVER FUNCTION beta.run() RETURNS BOOLEAN AS SELECT TRUE FROM beta.holder t;\n",
        "CREATE CLIENT FUNCTION alpha.client() RETURNS BOOLEAN AS TRUE;\n",
        "CREATE CLIENT FUNCTION beta.client() RETURNS BOOLEAN AS TRUE;\n",
        "CREATE CLIENT FUNCTION alpha.use() RETURNS BOOLEAN IS\n",
        "BEGIN\n",
        "    RETURN AWAIT std.data.resource(target => alpha.run, arguments => std.call.args());\n",
        "END;\n",
        "CREATE CLIENT FUNCTION beta.use() RETURNS BOOLEAN IS\n",
        "BEGIN\n",
        "    RETURN AWAIT std.data.resource(target => beta.run, arguments => std.call.args());\n",
        "END;\n",
    );
    open_document(&mut client, uri, source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let beta_schema = position_inside(source, "CREATE SCHEMA ", "beta");
    assert_hover_contains(&mut client, uri, beta_schema.clone(), "**schema**");
    assert_definition_starts_on(&mut client, uri, beta_schema, 1);

    let categories = [
        (
            "CREATE TYPE beta.holder AS OBJECT (item beta.",
            "item",
            "beta.item",
        ),
        (
            "CREATE TYPE beta.holder AS OBJECT (item beta.item, stage beta.",
            "stage",
            "beta.stage",
        ),
        (
            "CREATE TYPE beta.holder AS OBJECT (item beta.item, stage beta.stage, record beta.",
            "record",
            "beta.record",
        ),
        (
            "CREATE TYPE beta.holder AS OBJECT (item beta.item, stage beta.stage, record beta.record, scalar beta.",
            "scalar",
            "beta.scalar",
        ),
        (
            "CREATE TYPE beta.holder AS OBJECT (item beta.item, stage beta.stage, record beta.record, scalar beta.scalar, opaque beta.",
            "opaque",
            "beta.opaque",
        ),
    ];
    for (prefix, token, expected) in categories {
        let position = position_inside(source, prefix, token);
        assert_hover_contains(&mut client, uri, position.clone(), expected);
        let hover = client.request(
            "textDocument/hover",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        );
        assert!(
            hover["contents"]["value"]
                .as_str()
                .is_some_and(|value| value.contains(expected)),
            "qualified type hover {expected}: {hover}"
        );
    }

    let beta_item_use = position_inside(
        source,
        "CREATE TYPE beta.holder AS OBJECT (item beta.",
        "item",
    );
    let beta_item_decl_line = source
        .lines()
        .position(|line| line.contains("CREATE TYPE beta.item"))
        .expect("beta item declaration") as u64;
    let beta_holder_line = source
        .lines()
        .position(|line| line.contains("CREATE TYPE beta.holder"))
        .expect("beta holder declaration") as u64;
    assert_definition_starts_on(&mut client, uri, beta_item_use.clone(), beta_item_decl_line);
    let beta_item_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": beta_item_use,
            "context": { "includeDeclaration": true },
        }),
    );
    let beta_item_lines: Vec<_> = beta_item_references
        .as_array()
        .expect("beta item references")
        .iter()
        .map(|reference| {
            reference["range"]["start"]["line"]
                .as_u64()
                .expect("reference line")
        })
        .collect();
    assert_eq!(
        beta_item_lines.len(),
        2,
        "beta item references: {beta_item_references}"
    );
    assert!(
        beta_item_lines
            .iter()
            .all(|line| *line == beta_item_decl_line || *line == beta_holder_line),
        "alpha item reference leaked into beta item references: {beta_item_references}"
    );

    let beta_run = position_inside(source, "CREATE SERVER FUNCTION beta.", "run");
    let beta_run_decl_line = source
        .lines()
        .position(|line| line.contains("CREATE SERVER FUNCTION beta.run"))
        .expect("beta server function declaration") as u64;
    let beta_run_use_line = source
        .lines()
        .position(|line| line.contains("target => beta.run"))
        .expect("beta target function use") as u64;
    assert_hover_contains(&mut client, uri, beta_run.clone(), "beta.run");
    assert_definition_starts_on(&mut client, uri, beta_run.clone(), beta_run_decl_line);
    let beta_run_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": beta_run,
            "context": { "includeDeclaration": true },
        }),
    );
    let beta_run_lines: Vec<_> = beta_run_references
        .as_array()
        .expect("beta function references")
        .iter()
        .map(|reference| {
            reference["range"]["start"]["line"]
                .as_u64()
                .expect("reference line")
        })
        .collect();
    assert_eq!(
        beta_run_lines.len(),
        2,
        "beta function references: {beta_run_references}"
    );
    assert!(
        beta_run_lines
            .iter()
            .all(|line| *line == beta_run_decl_line || *line == beta_run_use_line),
        "alpha function reference leaked into beta function references: {beta_run_references}"
    );

    let beta_client = position_inside(source, "CREATE CLIENT FUNCTION beta.", "client");
    let beta_client_decl_line = source
        .lines()
        .position(|line| line.contains("CREATE CLIENT FUNCTION beta.client"))
        .expect("beta client function declaration") as u64;
    assert_hover_contains(&mut client, uri, beta_client.clone(), "beta.client");
    assert_definition_starts_on(&mut client, uri, beta_client, beta_client_decl_line);

    client.shutdown();
}

/// Incomplete CLIENT member expressions are outside the accepted grammar. The
/// editor contract is to avoid inventing field or proposal-only completion
/// labels until the user has supplied a complete dotted path.
#[test]
fn incomplete_client_member_cursor_has_no_field_or_proposal_completion() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/incomplete-client-member.orna";
    let source = EXPRESSION_CLIENT_SOURCE.replace("AS p_item.title", "AS p_item.");

    open_document(&mut client, uri, &source, 1);
    let diagnostics = client.read_notification("textDocument/publishDiagnostics");
    assert!(
        !diagnostics["diagnostics"]
            .as_array()
            .expect("diagnostic items")
            .is_empty(),
        "the parser must retain its incomplete-member diagnostic: {diagnostics}"
    );

    let completion = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": position_after(&source, "AS p_item."),
            "context": { "triggerKind": 2, "triggerCharacter": "." },
        }),
    );
    let labels: Vec<&str> = completion
        .as_array()
        .expect("completion items")
        .iter()
        .map(|item| item["label"].as_str().expect("completion label"))
        .collect();
    assert!(
        labels
            .iter()
            .all(|label| *label != "title" && !label.contains('.')),
        "incomplete CLIENT member must not leak field or proposal labels: {labels:?}"
    );

    client.shutdown();
}

#[test]
fn serves_same_document_target_function_completion_for_accepted_constructors() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/target-completion.orna";
    let resource_source =
        SCALAR_RESOURCE_SOURCE.replace("std.invoke.echo", "target_fixture.resource_server");
    let source = format!(
        "CREATE SCHEMA target_fixture;\n\
         CREATE TYPE target_fixture.row AS OBJECT (value INTEGER NOT NULL);\n\
         CREATE SERVER FUNCTION target_fixture.resource_server() RETURNS INTEGER\n\
         AS SELECT t.value FROM target_fixture.row t;\n\
         {resource_source}\n{STREAM_RESOURCE_SOURCE}\n{ACTION_SOURCE}\n\
         CREATE CLIENT FUNCTION action_fixture.invalid(p_item REF target_fixture.row)\n\
         RETURNS std.Action AS std.action.call(\n\
             target => p_item.value,\n\
             arguments => std.call.args()\n\
         );\n\
         CREATE SCHEMA shadow_item;\n\
         CREATE SERVER FUNCTION shadow_item.read() RETURNS INTEGER\n\
         AS SELECT t.value FROM target_fixture.row t;\n\
         CREATE CLIENT FUNCTION action_fixture.shadowed(shadow_item INTEGER)\n\
         RETURNS std.Action AS std.action.call(\n\
             target => shadow_item.read,\n\
             arguments => std.call.args()\n\
         );\n\
         CREATE EXTERNAL CLIENT FUNCTION action_fixture.transient_target()\n\
         RETURNS std.ui.UI\n\
         RUNTIME CONTRACT 'test.transient@1';\n\
         CREATE CLIENT FUNCTION action_fixture.call_transient()\n\
         RETURNS std.Action AS std.action.call(\n\
             target => action_fixture.transient_target,\n\
             arguments => std.call.args()\n\
         );"
    );

    open_document(&mut client, uri, &source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let assert_target_items = |client: &mut Client, prefix: &str, expected: &[(&str, &str)]| {
        let completion = client.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": position_after(&source, prefix),
                "context": { "triggerKind": 2, "triggerCharacter": "." },
            }),
        );
        let items = completion.as_array().expect("completion items");
        for (label, detail) in expected {
            let item = items
                .iter()
                .find(|item| item["label"] == *label)
                .unwrap_or_else(|| panic!("missing target completion {label}: {completion}"));
            assert_eq!(
                item["kind"].as_u64(),
                Some(3),
                "target completion kind: {item}"
            );
            assert!(
                item["detail"]
                    .as_str()
                    .is_some_and(|value| value == *detail),
                "target completion detail for {label}: {item}"
            );
        }
    };

    assert_target_items(
        &mut client,
        "target => target_fixture.",
        &[
            ("resource_server", "server function target"),
            ("events", "server function"),
            ("local", "client function"),
        ],
    );
    assert_target_items(
        &mut client,
        "target => stream_fixture.",
        &[
            ("events", "server function target"),
            ("resource_server", "server function"),
            ("local", "client function"),
        ],
    );
    assert_target_items(
        &mut client,
        "target => action_fixture.",
        &[
            ("local", "client function target"),
            ("resource_server", "server function target"),
            ("events", "server function"),
            ("transient_target", "client function"),
        ],
    );
    assert_target_items(
        &mut client,
        "target => shadow_item.",
        &[("read", "server function target")],
    );
    let shadow_target = position_inside(&source, "target => shadow_item.", "read");
    assert_hover_contains(&mut client, uri, shadow_target.clone(), "server function");
    let shadow_declaration_line = source
        .lines()
        .position(|line| line.contains("CREATE SERVER FUNCTION shadow_item.read"))
        .expect("shadowed target declaration") as u64;
    assert_definition_starts_on(
        &mut client,
        uri,
        shadow_target.clone(),
        shadow_declaration_line,
    );
    let shadow_references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": uri },
            "position": shadow_target,
            "context": { "includeDeclaration": true },
        }),
    );
    assert_eq!(
        shadow_references.as_array().map(|values| values.len()),
        Some(2),
        "shadowed target references: {shadow_references}"
    );

    assert_target_items(
        &mut client,
        "target => p_item.",
        &[
            ("local", "client function"),
            ("resource_server", "server function"),
        ],
    );
    client.shutdown();
}

#[test]
fn external_client_hover_preserves_runtime_and_capability_metadata() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/external-hover.orna";
    let source = concat!(
        "CREATE EXTERNAL CLIENT FUNCTION inspector.render(p_snapshot sys.inspect.snapshot)\n",
        "RETURNS std.ui.UI\n",
        "RUNTIME CONTRACT 'std.inspect.render@1'\n",
        "REQUIRES CAPABILITY sys.inspect.render('snapshot');\n",
    );
    open_document(&mut client, uri, source, 1);
    let _ = client.read_notification("textDocument/publishDiagnostics");

    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(source, "CREATE EXTERNAL CLIENT FUNCTION inspector.", "render"),
        }),
    );
    let value = hover["contents"]["value"]
        .as_str()
        .expect("external CLIENT hover value");
    assert!(
        value.contains("CREATE EXTERNAL CLIENT FUNCTION inspector.render"),
        "external CLIENT signature: {value}"
    );
    assert!(
        value.contains("RUNTIME CONTRACT 'std.inspect.render@1'"),
        "runtime contract metadata: {value}"
    );
    assert!(
        value.contains("REQUIRES CAPABILITY sys.inspect.render('snapshot')"),
        "capability metadata: {value}"
    );

    client.shutdown();
}
