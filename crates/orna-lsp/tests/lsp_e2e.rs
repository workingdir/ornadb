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

use serde_json::{Value, json};

/// The valid application source used for positive tests.
///
/// The probe type carries DOCUMENTATION clauses so the rich hover
/// assertions can check documentation rendering.
const VALID_SOURCE: &str = include_str!("fixtures/lsp-e2e-001-module-valid-source.orna");

/// Accepted SERVER UPDATE and DELETE mutations with declarations for every
/// referenced schema, object type, field, alias, and parameter.
const MUTATION_SOURCE: &str = include_str!("fixtures/lsp-e2e-002-module-mutation-source.orna");

/// The accepted identity-preserving object-field rename shape. The LSP
/// process has no user catalogue base, so its compiler diagnostics report the
/// missing historical object while syntax and source navigation still expose
/// the final field declaration and use.
const FIELD_RENAME_SOURCE: &str =
    include_str!("fixtures/lsp-e2e-003-module-field-rename-source.orna");

/// Accepted ORDER BY source used to pin ASC/DESC keyword highlighting.
const ORDER_BY_SOURCE: &str = include_str!("fixtures/lsp-e2e-004-module-order-by-source.orna");

/// The accepted CLIENT source fixture shared with the syntax parser test.
const ACCEPTED_CLIENT_SOURCE: &str =
    include_str!("../../orna-syntax/testdata/accepted-client.orna");

/// A checked-in source with one persistent object and its resolved references.
const PERSISTENT_RENAME_SOURCE: &str =
    include_str!("../../orna-compiler/tests/fixtures/server-function-dogfood.orna");
/// The broken source used for negative diagnostics tests.
const BROKEN_SOURCE: &str = include_str!("fixtures/lsp-e2e-005-module-broken-source.orna");
/// A warning-only CLIENT source shared with the compiler and analysis tests.
const WARNING_SOURCE: &str = include_str!("fixtures/lsp-e2e-006-module-warning-source.orna");

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
        capabilities["renameProvider"], true,
        "the server must advertise semantic rename support: {result}"
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

fn final_name_range(source: &str, qualified_name: &str, final_name: &str) -> Value {
    assert!(
        qualified_name.ends_with(final_name),
        "final name must suffix the qualified name"
    );
    let start = source
        .find(qualified_name)
        .unwrap_or_else(|| panic!("missing qualified name {qualified_name:?}"))
        + qualified_name.len()
        - final_name.len();
    json!({
        "start": position_at_byte(source, start),
        "end": position_at_byte(source, start + final_name.len()),
    })
}

fn source_name_range(source: &str, name: &str) -> Value {
    let start = source
        .find(name)
        .unwrap_or_else(|| panic!("missing source name {name:?}"));
    json!({
        "start": position_at_byte(source, start),
        "end": position_at_byte(source, start + name.len()),
    })
}

fn apply_text_edits(source: &str, edits: &[Value]) -> String {
    let mut edits = edits
        .iter()
        .map(|edit| {
            (
                byte_offset_from_lsp_position(source, &edit["range"]["start"]),
                byte_offset_from_lsp_position(source, &edit["range"]["end"]),
                edit["newText"]
                    .as_str()
                    .expect("text edit replacement")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    edits.sort_by(|left, right| right.0.cmp(&left.0));
    let mut updated = source.to_owned();
    for (start, end, replacement) in edits {
        updated.replace_range(start..end, &replacement);
    }
    updated
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
    let source = include_str!(
        "fixtures/lsp-e2e-007-serves-accepted-client-semantic-tokens-with-utf16-and-nested-ranges-source.orna"
    );

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
fn serves_signature_help_and_workspace_symbols() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/extended-requests.orna";
    let source = include_str!(
        "fixtures/lsp-e2e-008-serves-signature-help-and-workspace-symbols-source.orna"
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
    let source = include_str!(
        "fixtures/lsp-e2e-009-serves-standard-function-hover-signature-and-unknown-fallback-source.orna"
    );
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
    let unknown_source = include_str!(
        "fixtures/lsp-e2e-010-serves-standard-function-hover-signature-and-unknown-fallback-unknown-source.orna"
    );
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
    let echo_source =
        include_str!("fixtures/lsp-e2e-011-serves-rich-hover-content-echo-source.orna");
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
    let std_source = include_str!("fixtures/lsp-e2e-012-serves-rich-hover-content-std-source.orna");
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
    let collision_source =
        include_str!("fixtures/lsp-e2e-013-serves-rich-hover-content-collision-source.orna");
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
fn semantic_rename_updates_references_for_one_persistent_object() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/server-function-dogfood.orna";
    open_document(&mut client, uri, PERSISTENT_RENAME_SOURCE, 1);
    assert_eq!(
        client.read_notification("textDocument/publishDiagnostics")["uri"],
        uri
    );

    let rename = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(
                PERSISTENT_RENAME_SOURCE,
                "CREATE TYPE dogfood.",
                "item",
            ),
            "newName": "renamed_item",
        }),
    );
    let changes = rename["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("resolved target returns a workspace edit: {rename}"));
    assert_eq!(changes.len(), 1, "one open source changes: {rename}");
    let edits = changes[uri]
        .as_array()
        .expect("declaration and references are edited");
    assert!(
        edits.len() > 1,
        "declaration and references change: {rename}"
    );
    assert!(edits.iter().all(|edit| edit["newText"] == "renamed_item"));
    assert!(
        edits.iter().any(|edit| edit["range"]
            == final_name_range(PERSISTENT_RENAME_SOURCE, "dogfood.item", "item")),
        "declaration final name is edited: {rename}"
    );
    for edit in edits {
        let start =
            byte_offset_from_lsp_position(PERSISTENT_RENAME_SOURCE, &edit["range"]["start"]);
        let end = byte_offset_from_lsp_position(PERSISTENT_RENAME_SOURCE, &edit["range"]["end"]);
        assert_eq!(
            &PERSISTENT_RENAME_SOURCE[start..end],
            "item",
            "rename changes only the resolved final-name spans: {edit}"
        );
    }
    let renamed_source = apply_text_edits(PERSISTENT_RENAME_SOURCE, edits);
    assert!(!renamed_source.contains("dogfood.item"));
    assert!(renamed_source.contains("CREATE TYPE dogfood.renamed_item"));
    assert!(renamed_source.contains("FROM dogfood.renamed_item item"));
    assert!(renamed_source.contains("item.value"));
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": renamed_source }],
        }),
    );
    assert_eq!(
        client.read_notification("textDocument/publishDiagnostics")["uri"],
        uri
    );

    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(
                &renamed_source,
                "FROM dogfood.",
                "renamed_item",
            ),
        }),
    );
    assert_eq!(
        definition["uri"], uri,
        "same persistent object remains selected: {definition}"
    );
    assert_eq!(
        definition["range"],
        source_name_range(&renamed_source, "dogfood.renamed_item"),
        "renamed reference resolves to the renamed declaration"
    );

    let unsupported = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(
                &renamed_source,
                "CREATE SERVER FUNCTION dogfood.read_item(",
                "p_item",
            ),
            "newName": "item_ref",
        }),
    );
    assert!(
        unsupported.is_null(),
        "a function parameter is not a persistent object: {unsupported}"
    );
    client.shutdown();
}

#[test]
fn semantic_rename_rejects_ambiguous_persistent_object() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let first_uri = "file:///test/first-dogfood.orna";
    let second_uri = "file:///test/second-dogfood.orna";
    open_document(&mut client, first_uri, PERSISTENT_RENAME_SOURCE, 1);
    assert_eq!(
        client.read_notification("textDocument/publishDiagnostics")["uri"],
        first_uri
    );
    open_document(&mut client, second_uri, PERSISTENT_RENAME_SOURCE, 1);
    assert_eq!(
        client.read_notification("textDocument/publishDiagnostics")["uri"],
        second_uri
    );

    let rename = client.request(
        "textDocument/rename",
        json!({
            "textDocument": { "uri": first_uri },
            "position": position_inside(
                PERSISTENT_RENAME_SOURCE,
                "CREATE TYPE dogfood.",
                "item",
            ),
            "newName": "record",
        }),
    );
    assert!(
        rename.is_null(),
        "duplicate persistent declarations are ambiguous: {rename}"
    );
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
    let source = include_str!(
        "fixtures/lsp-e2e-014-scoped-navigation-resolves-owner-paths-and-fails-closed-source.orna"
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
    let source = include_str!(
        "fixtures/lsp-e2e-015-semantic-token-range-includes-intersecting-multiline-comment-segments-source.orna"
    );
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
    let source = include_str!(
        "fixtures/lsp-e2e-016-serves-semantic-compiler-diagnostics-for-unknown-schema-in-push-and-pull-source.orna"
    );

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
fn serves_warning_diagnostic_with_related_return_location_in_push_and_pull() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/warning.orna";

    open_document(&mut client, uri, WARNING_SOURCE, 1);
    let pushed = client.read_notification("textDocument/publishDiagnostics");
    assert_eq!(pushed["uri"], uri);
    let pushed_items = pushed["diagnostics"].as_array().expect("diagnostic items");
    assert_eq!(pushed_items.len(), 1, "warning diagnostic: {pushed}");
    let diagnostic = &pushed_items[0];

    let unreachable_start = WARNING_SOURCE
        .find("LET ignored")
        .expect("unreachable statement");
    let unreachable_end = unreachable_start + "LET ignored := FALSE;".len();
    assert_eq!(
        diagnostic["range"],
        json!({
            "start": position_at_byte(WARNING_SOURCE, unreachable_start),
            "end": position_at_byte(WARNING_SOURCE, unreachable_end),
        })
    );
    assert_eq!(diagnostic["severity"], 2);
    assert_eq!(diagnostic["code"], "ORNA0401");
    assert_eq!(diagnostic["source"], "orna");
    assert_eq!(diagnostic["message"], "unreachable statement");
    assert_eq!(diagnostic["data"]["severity"], "warning");
    assert_eq!(diagnostic["data"]["primaryLabel"], "unreachable code");

    let related = diagnostic["relatedInformation"]
        .as_array()
        .expect("warning related information");
    assert_eq!(related.len(), 1);
    let return_start = WARNING_SOURCE
        .find("RETURN TRUE;")
        .expect("return statement");
    let return_end = return_start + "RETURN TRUE;".len();
    assert_eq!(related[0]["location"]["uri"], uri);
    assert_eq!(
        related[0]["location"]["range"],
        json!({
            "start": position_at_byte(WARNING_SOURCE, return_start),
            "end": position_at_byte(WARNING_SOURCE, return_end),
        })
    );
    assert_eq!(
        related[0]["message"],
        "this statement returns from the function"
    );
    assert_eq!(diagnostic["data"]["related"][0]["path"], uri);
    assert_eq!(diagnostic["data"]["related"][0]["start"], return_start);
    assert_eq!(diagnostic["data"]["related"][0]["end"], return_end);
    assert_eq!(
        diagnostic["data"]["related"][0]["label"],
        "this statement returns from the function"
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
fn serves_syntax_diagnostic_for_malformed_schema_in_push_and_pull() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/syntax-invalid.orna";
    // The parser reports the semicolon at byte span 29..30. The emoji prefix
    // makes the corresponding LSP range use UTF-16 characters 27..28.
    let source = include_str!(
        "fixtures/lsp-e2e-017-serves-syntax-diagnostic-for-malformed-schema-in-push-and-pull-source.orna"
    );

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
fn qualified_name_navigation_keeps_same_final_names_in_their_namespace() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/qualified-navigation.orna";
    let source = include_str!(
        "fixtures/lsp-e2e-018-qualified-name-navigation-keeps-same-final-names-in-their-namespace-source.orna"
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

#[test]
fn external_client_hover_preserves_runtime_and_capability_metadata() {
    let mut client = Client::spawn();
    initialize(&mut client);
    let uri = "file:///test/external-hover.orna";
    let source = include_str!(
        "fixtures/lsp-e2e-019-external-client-hover-preserves-runtime-and-capability-metadata-source.orna"
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

    let ordinary_hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri },
            "position": position_inside(source, "CREATE CLIENT FUNCTION inspector.", "local"),
        }),
    );
    let ordinary_value = ordinary_hover["contents"]["value"]
        .as_str()
        .expect("ordinary CLIENT hover value");
    assert!(
        ordinary_value.contains("CREATE CLIENT FUNCTION inspector.local()\nRETURNS BOOLEAN"),
        "ordinary CLIENT signature: {ordinary_value}"
    );
    assert!(
        !ordinary_value.contains("CREATE EXTERNAL CLIENT FUNCTION"),
        "ordinary CLIENT hover must not use the external signature: {ordinary_value}"
    );
    assert!(
        !ordinary_value.contains("RUNTIME CONTRACT"),
        "ordinary CLIENT hover must not include runtime metadata: {ordinary_value}"
    );
    assert!(
        !ordinary_value.contains("REQUIRES CAPABILITY"),
        "ordinary CLIENT hover must not include capability metadata: {ordinary_value}"
    );

    client.shutdown();
}
