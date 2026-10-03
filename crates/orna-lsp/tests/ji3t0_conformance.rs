//! End-to-end protocol and editor artifact proofs for the Orna 1.0 language.

use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use orna_syntax_v1::Keyword;
use serde_json::{Value, json};

const SOURCE: &str = include_str!("fixtures/ji3t0-lsp-v1.orna");
const INVALID_SOURCE: &str = include_str!("fixtures/ji3t0-invalid-v1.orna");
const PRE_V1_SOURCE: &str = include_str!("fixtures/ji3t0-pre-v1.orna");

const LEGACY_SYNTAX_WORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "ATOMIC",
    "AWAIT",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CALL",
    "CAPABILITY",
    "CASCADE",
    "CASE",
    "CHECK",
    "CLIENT",
    "CONST",
    "CONTRACT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DEFINER",
    "DELETE",
    "DESC",
    "DISABLED",
    "DISTINCT",
    "DOCUMENTATION",
    "DROP",
    "ELSE",
    "ELSIF",
    "END",
    "ENUM",
    "EXECUTE",
    "EXISTS",
    "EXPORT",
    "EXTERNAL",
    "FALSE",
    "FIELD",
    "FINAL",
    "FIRST",
    "FOR",
    "FROM",
    "FULL",
    "FUNCTION",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IMMUTABLE",
    "IN",
    "INNER",
    "INSERT",
    "INSPECT",
    "INTO",
    "INVOKER",
    "IS",
    "JOIN",
    "KERNEL",
    "LAST",
    "LEFT",
    "LET",
    "LIKE",
    "LIMIT",
    "LIST",
    "LOCAL",
    "LOOP",
    "MANUAL",
    "MAP",
    "NOT",
    "NULL",
    "NULLS",
    "OBJECT",
    "OFFSET",
    "ON",
    "ONLY",
    "OPAQUE",
    "OPTION",
    "OR",
    "ORDER",
    "OUTER",
    "PERSISTABLE",
    "PRELUDE",
    "PRIMITIVE",
    "READ",
    "REF",
    "RENAME",
    "REQUIRES",
    "RESTRICT",
    "RETURN",
    "RETURNING",
    "RETURNS",
    "REVOKE",
    "RIGHT",
    "ROLE",
    "ROWS",
    "RUNTIME",
    "SCHEMA",
    "SCOPE",
    "SEALED",
    "SECURITY",
    "SELECT",
    "SERVER",
    "SESSION",
    "SET",
    "STABLE",
    "STATE",
    "STREAM",
    "TABLE",
    "THEN",
    "TO",
    "TRANSACTION",
    "TRANSIENT",
    "TRUE",
    "TYPE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USER",
    "VALUE",
    "VALUES",
    "VOLATILE",
    "VOLATILITY",
    "WHEN",
    "WHERE",
    "WHILE",
    "BOOLEAN",
    "BIGINT",
    "BOOL",
    "BYTES",
    "BINARY",
    "CHARACTER",
    "DATE",
    "DECIMAL",
    "DURATION",
    "FLOAT",
    "INT",
    "INTEGER",
    "LARGE",
    "TEXT",
    "TIME",
    "TIMESTAMP",
    "UUID",
    "VOID",
];

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
        assert!(response.get("error").is_none(), "{method}: {response}");
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
    json!({"line":line,"character":column_text.encode_utf16().count()})
}

fn position_of(source: &str, needle: &str, offset: usize) -> Value {
    position_at(source, source.find(needle).unwrap() + offset)
}

fn initialize(client: &mut Client) -> Value {
    let result = client.request(
        "initialize",
        json!({"processId":null,"rootUri":null,"capabilities":{}}),
    );
    client.notify("initialized", json!({}));
    result
}

fn open(client: &mut Client, uri: &str, source: &str) -> Value {
    client.notify(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":uri,"languageId":"orna","version":1,"text":source}}),
    );
    client.notification("textDocument/publishDiagnostics")
}

fn assert_no_legacy_words(value: &Value, surface: &str) {
    match value {
        Value::String(text) => {
            let matcher_normalized = text
                .replace("\\\\s+", " ")
                .replace("\\s+", " ")
                .replace("\\s\\+", " ");
            for word in LEGACY_SYNTAX_WORDS {
                assert!(
                    !contains_word(&matcher_normalized, word),
                    "pre-1.0.0 token {word} escaped through {surface}"
                );
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_no_legacy_words(value, surface);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                assert_no_legacy_words(value, surface);
            }
        }
        _ => {}
    }
}

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(start, matched)| {
        let before = text[..start].chars().next_back();
        let after = text[start + matched.len()..].chars().next();
        !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char)
    })
}

fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

#[test]
fn protocol_conformance_uses_v1_for_every_advertised_editor_feature() {
    let uri = "file:///workspace/ji3t0-lsp-v1.orna";
    let mut client = Client::spawn();
    let initialized = initialize(&mut client);
    let capabilities = &initialized["capabilities"];
    assert_eq!(capabilities["hoverProvider"], true);
    assert_eq!(capabilities["definitionProvider"], true);
    assert_eq!(capabilities["renameProvider"], true);
    assert!(capabilities["signatureHelpProvider"].is_object());
    assert!(capabilities["completionProvider"].is_object());
    assert!(capabilities["diagnosticProvider"].is_object());
    assert!(capabilities["semanticTokensProvider"]["legend"]["tokenTypes"].is_array());
    assert_no_legacy_words(&initialized, "initialize response");

    let published = open(&mut client, uri, SOURCE);
    assert_no_legacy_words(&published, "published diagnostics");
    assert!(
        published["diagnostics"].as_array().unwrap().is_empty(),
        "{published}"
    );

    let pull = client.request(
        "textDocument/diagnostic",
        json!({"textDocument":{"uri":uri}}),
    );
    assert_no_legacy_words(&pull, "pull diagnostics");
    assert!(pull["items"].as_array().unwrap().is_empty(), "{pull}");

    let call = SOURCE.find("add(value, 2)").unwrap();
    let call_position = position_at(SOURCE, call + 1);

    let hover = client.request(
        "textDocument/hover",
        json!({"textDocument":{"uri":uri},"position":call_position}),
    );
    assert_no_legacy_words(&hover, "hover response");
    let hover_text = hover["contents"]["value"].as_str().unwrap();
    assert!(
        hover_text.contains("fn add(left: Int, right: Int): Int"),
        "{hover_text}"
    );
    assert!(
        hover_text.contains("Add two integer values."),
        "{hover_text}"
    );

    let signature_cursor = call + "add(value, ".len();
    let signature = client.request(
        "textDocument/signatureHelp",
        json!({"textDocument":{"uri":uri},"position":position_at(SOURCE,signature_cursor)}),
    );
    assert_no_legacy_words(&signature, "signature-help response");
    assert_eq!(signature["activeParameter"], 1);
    assert!(
        signature["signatures"][0]["label"]
            .as_str()
            .unwrap()
            .contains("fn add(")
    );

    let definition = client.request(
        "textDocument/definition",
        json!({"textDocument":{"uri":uri},"position":call_position}),
    );
    assert_no_legacy_words(&definition, "go-to-definition response");
    assert_eq!(definition["uri"], uri);
    assert_eq!(
        definition["range"]["start"],
        position_of(SOURCE, "pub fn add", "pub fn ".len())
    );

    let renamed = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":uri},"position":call_position,"newName":"sum"}),
    );
    assert_no_legacy_words(&renamed, "rename response");
    let edits = renamed["changes"][uri].as_array().unwrap();
    assert_eq!(edits.len(), 2, "{renamed}");
    assert!(edits.iter().all(|edit| edit["newText"] == "sum"));

    let completion = client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":position_at(SOURCE,SOURCE.len())}),
    );
    assert_no_legacy_words(&completion, "completion response");
    let items = completion.as_array().unwrap();
    let keyword_kind = serde_json::to_value(lsp_types::CompletionItemKind::KEYWORD).unwrap();
    let actual_keywords = items
        .iter()
        .filter(|item| item["kind"] == keyword_kind)
        .map(|item| item["label"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let expected_keywords = Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_keywords, expected_keywords,
        "ORNA-LEX-007 completion inventory"
    );
    assert!(items.iter().any(|item| item["label"] == "add"));

    let semantic = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":uri}}),
    );
    assert_no_legacy_words(&semantic, "semantic-highlight response");
    let legends = capabilities["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .unwrap();
    let keyword_type = legends.iter().position(|value| value == "keyword").unwrap() as u64;
    let highlighted = semantic_words(SOURCE, &semantic);
    for word in ["pub", "fn"] {
        assert!(
            highlighted
                .iter()
                .any(|(text, kind)| text == word && *kind == keyword_type),
            "missing syntax-v1 keyword highlight for {word}: {highlighted:?}"
        );
    }
    assert!(highlighted.iter().any(|(text, _)| text == "add"));

    let broken_uri = "file:///workspace/ji3t0-invalid-v1.orna";
    let broken = open(&mut client, broken_uri, INVALID_SOURCE);
    assert_no_legacy_words(&broken, "syntax diagnostic");
    let diagnostics = broken["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{broken}");
    assert_eq!(diagnostics[0]["source"], "orna-syntax-v1");
    assert_eq!(
        diagnostics[0]["range"]["start"],
        position_of(INVALID_SOURCE, ";", 0)
    );

    let legacy_uri = "file:///workspace/ji3t0-pre-v1.orna";
    let legacy = open(&mut client, legacy_uri, PRE_V1_SOURCE);
    assert_no_legacy_words(&legacy, "rejected pre-1.0.0 diagnostic");
    assert!(!legacy["diagnostics"].as_array().unwrap().is_empty());
    assert!(
        legacy["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["source"] == "orna-syntax-v1"),
        "{legacy}"
    );
    let legacy_highlighting = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":legacy_uri}}),
    );
    assert_no_legacy_words(
        &legacy_highlighting,
        "rejected pre-1.0.0 semantic highlights",
    );
    let rendered = semantic_words(PRE_V1_SOURCE, &legacy_highlighting);
    assert!(
        rendered
            .iter()
            .all(|(_, token_type)| *token_type != keyword_type),
        "pre-1.0.0 source received syntax-v1 keyword highlighting: {rendered:?}"
    );
    client.shutdown();
}

fn semantic_words(source: &str, response: &Value) -> Vec<(String, u64)> {
    let mut line = 0usize;
    let mut character = 0usize;
    let mut words = Vec::new();
    let data = response["data"].as_array().unwrap();
    assert_eq!(
        data.len() % 5,
        0,
        "semantic token data is five integers per token"
    );
    for token in data.chunks_exact(5) {
        let delta_line = token[0].as_u64().unwrap() as usize;
        let delta_start = token[1].as_u64().unwrap() as usize;
        if delta_line == 0 {
            character += delta_start;
        } else {
            line += delta_line;
            character = delta_start;
        }
        let length = token[2].as_u64().unwrap() as usize;
        let text = source.lines().nth(line).unwrap();
        words.push((
            text[character..character + length].to_owned(),
            token[3].as_u64().unwrap(),
        ));
    }
    words
}

#[test]
fn vscode_extension_loads_v1_highlights_from_the_in_crate_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates");
    let (editor, keywords) = vscode_keywords(root, &expected_keywords());
    assert_fixture_keywords(editor, &keywords);
}

#[test]
fn vim_extension_loads_v1_highlights_from_the_in_crate_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates");
    let (editor, keywords) = vim_keywords(root, &expected_keywords());
    assert_fixture_keywords(editor, &keywords);
}

#[test]
fn emacs_extension_loads_v1_highlights_from_the_in_crate_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates");
    let (editor, keywords) = emacs_keywords(root, &expected_keywords());
    assert_fixture_keywords(editor, &keywords);
    emacs_batch_highlight(root, SOURCE);
}

#[test]
fn sublime_extension_loads_v1_highlights_from_the_in_crate_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates");
    let (editor, keywords) = sublime_keywords(root, &expected_keywords());
    assert_fixture_keywords(editor, &keywords);
}

fn expected_keywords() -> BTreeSet<String> {
    Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling().to_owned())
        .collect()
}

fn assert_fixture_keywords(editor: &str, keywords: &BTreeSet<String>) {
    assert_eq!(
        keywords,
        &expected_keywords(),
        "{editor} ORNA-LEX-007 inventory"
    );
    for word in ["pub", "fn"] {
        assert!(
            SOURCE
                .split(|character: char| !is_identifier_char(character))
                .any(|token| token == word)
                && keywords.contains(word),
            "{editor} does not highlight {word} in the in-crate fixture"
        );
    }
}

fn vscode_keywords(root: &Path, expected: &BTreeSet<String>) -> (&'static str, BTreeSet<String>) {
    let manifest_path = root.join("editors/vscode/package.json");
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert!(
        manifest["contributes"]["languages"][0]["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|extension| extension == ".orna")
    );
    assert_eq!(manifest["contributes"]["grammars"][0]["language"], "orna");
    let grammar_path = root.join("editors/vscode").join(
        manifest["contributes"]["grammars"][0]["path"]
            .as_str()
            .unwrap(),
    );
    let grammar: Value = serde_json::from_str(&fs::read_to_string(grammar_path).unwrap()).unwrap();
    assert_no_legacy_words(
        &Value::String(grammar.to_string()),
        "VS Code grammar output",
    );
    assert_eq!(grammar["scopeName"], "source.orna");
    assert_eq!(
        grammar["repository"]["keywords"]["name"],
        "keyword.control.orna"
    );
    let pattern = grammar["repository"]["keywords"]["match"].as_str().unwrap();
    let keywords = textmate_keywords(pattern);
    assert_surface_has_no_legacy(&keywords, expected, "VS Code TextMate");
    ("VS Code", keywords)
}

fn vim_keywords(root: &Path, expected: &BTreeSet<String>) -> (&'static str, BTreeSet<String>) {
    let path = root.join("editors/vim/syntax/orna.vim");
    let source = fs::read_to_string(path).unwrap();
    assert_no_legacy_words(&Value::String(source.clone()), "Vim syntax output");
    assert!(
        source.contains("b:current_syntax"),
        "Vim syntax guard missing"
    );
    assert!(
        source.contains("let b:current_syntax = \"orna\""),
        "Vim loader did not finish"
    );
    assert!(
        source.contains("syntax case match"),
        "Orna v1 keywords are case-sensitive"
    );
    assert!(source.contains("hi def link ornaKeyword Statement"));
    let detector = fs::read_to_string(root.join("editors/vim/ftdetect/orna.vim")).unwrap();
    assert!(detector.contains("*.orna") && detector.contains("setfiletype orna"));
    let mut words = BTreeSet::new();
    let mut in_keywords = false;
    for line in source.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("syntax keyword ornaKeyword ") {
            in_keywords = true;
            words.extend(rest.split_whitespace().map(str::to_ascii_lowercase));
        } else if let Some(rest) = line.strip_prefix("\\ ") {
            if in_keywords {
                words.extend(rest.split_whitespace().map(str::to_ascii_lowercase));
            }
        } else if !line.is_empty() {
            in_keywords = false;
        }
    }
    assert_surface_has_no_legacy(&words, expected, "Vim syntax");
    ("Vim", words)
}

fn emacs_keywords(root: &Path, expected: &BTreeSet<String>) -> (&'static str, BTreeSet<String>) {
    let path = root.join("editors/emacs/orna-eglot.el");
    let source = fs::read_to_string(path).unwrap();
    assert_no_legacy_words(&Value::String(source.clone()), "Emacs extension output");
    let start = source
        .find("(defvar orna-keywords")
        .expect("Emacs keyword definition");
    let end = source[start..].find("\n    \"Orna keywords").unwrap() + start;
    let declaration = &source[start..end];
    let words = quoted_words(declaration);
    assert!(
        source.contains("(define-derived-mode orna-mode"),
        "Emacs mode is not defined"
    );
    assert!(source.contains("font-lock-keyword-face"));
    assert!(
        source.contains("(orna-font-lock-keywords nil nil)"),
        "Orna v1 keywords are case-sensitive"
    );
    assert_surface_has_no_legacy(&words, expected, "Emacs font-lock");
    ("Emacs", words)
}

fn sublime_keywords(root: &Path, expected: &BTreeSet<String>) -> (&'static str, BTreeSet<String>) {
    let path = root.join("editors/sublime/Orna.sublime-syntax");
    let source = fs::read_to_string(path).unwrap();
    assert_no_legacy_words(&Value::String(source.clone()), "Sublime syntax output");
    assert!(source.starts_with("%YAML 1.2"), "Sublime syntax header");
    assert!(
        source.contains("file_extensions: [orna]"),
        "Sublime extension registration"
    );
    assert!(source.contains("contexts:") && source.contains("  main:"));
    let lines = source.lines().collect::<Vec<_>>();
    let keyword_line = lines
        .iter()
        .position(|line| line.contains("scope: keyword.control.orna"))
        .expect("Sublime keyword scope");
    let pattern_line = lines[..keyword_line]
        .iter()
        .rev()
        .find(|line| line.trim_start().starts_with("- match:"))
        .expect("Sublime keyword matcher");
    let pattern = pattern_line
        .split_once('"')
        .unwrap()
        .1
        .rsplit_once('"')
        .unwrap()
        .0;
    let keywords = textmate_keywords(pattern);
    assert_surface_has_no_legacy(&keywords, expected, "Sublime syntax");
    ("Sublime", keywords)
}

fn textmate_keywords(pattern: &str) -> BTreeSet<String> {
    assert!(
        !pattern.contains("(?i"),
        "Orna v1 keyword matchers are case-sensitive: {pattern}"
    );
    let Some(start) = pattern.find("(?:") else {
        panic!("keyword matcher has no explicit vocabulary: {pattern}");
    };
    let remainder = &pattern[start + 3..];
    let end = remainder
        .find(")\\b")
        .unwrap_or_else(|| panic!("keyword matcher is not word-bounded: {pattern}"));
    remainder[..end]
        .split('|')
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn quoted_words(source: &str) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    let mut rest = source;
    while let Some(start) = rest.find('"') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else { break };
        words.insert(rest[..end].to_ascii_lowercase());
        rest = &rest[end + 1..];
    }
    words
}

fn assert_surface_has_no_legacy(
    actual: &BTreeSet<String>,
    expected: &BTreeSet<String>,
    editor: &str,
) {
    for word in actual.difference(expected) {
        assert!(
            !LEGACY_SYNTAX_WORDS
                .iter()
                .any(|legacy| legacy.eq_ignore_ascii_case(word)),
            "pre-1.0.0 syntax token {word:?} remains in {editor} output"
        );
    }
    assert_eq!(actual, expected, "{editor} keyword inventory drift");
}

fn emacs_batch_highlight(root: &Path, source: &str) {
    let plugin = root.join("editors/emacs/orna-eglot.el");
    let fixture = temporary_fixture(source);
    let expression = format!(
        "(progn (load-file {}) (with-temp-buffer (insert-file-contents {}) (orna-mode) (font-lock-ensure) (goto-char (point-min)) (search-forward \"pub\") (unless (eq (get-text-property (- (point) 3) 'face) 'font-lock-keyword-face) (error \"pub is not highlighted as a keyword\"))))",
        elisp_string(&plugin.display().to_string()),
        elisp_string(&fixture.display().to_string()),
    );
    let result = match Command::new("emacs")
        .args(["--batch", "--quick", "--eval", &expression])
        .output()
    {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let _ = fs::remove_file(fixture);
            return;
        }
        Err(error) => panic!("start Emacs batch mode: {error}"),
    };
    let _ = fs::remove_file(fixture);
    assert!(
        result.status.success(),
        "Emacs failed to load/highlight the fixture (exit {:?}):\n{}\n{}",
        result.status.code(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
}

fn temporary_fixture(source: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("orna-ji3t0-{}.orna", std::process::id()));
    fs::write(&path, source).unwrap();
    path
}

fn elisp_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
