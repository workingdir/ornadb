//! End-to-end protocol and editor artifact proofs for the Orna 1.0 language.

use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::{Value, json};

const SOURCE: &str = include_str!("fixtures/ji3t0-lsp-v1-self-contained.orna");

#[path = "support/completion_contract.rs"]
mod completion_contract;
#[path = "support/hover_semantic_contract.rs"]
mod hover_semantic_contract;
#[path = "support/syntax_v1_action_signature_contract.rs"]
mod syntax_v1_action_signature_contract;
#[path = "support/syntax_v1_depth_contract.rs"]
mod syntax_v1_depth_contract;
#[path = "support/syntax_v1_diagnostics_document_links_contract.rs"]
mod syntax_v1_diagnostics_document_links_contract;
#[path = "support/syntax_v1_document_highlight_code_lens_contract.rs"]
mod syntax_v1_document_highlight_code_lens_contract;
#[path = "support/syntax_v1_folding_selection_contract.rs"]
mod syntax_v1_folding_selection_contract;
#[path = "support/syntax_v1_workspace_hierarchy_contract.rs"]
mod syntax_v1_workspace_hierarchy_contract;

const SEMANTIC_SOURCE: &str = include_str!("fixtures/editor-semantic-tokens.orna");
const HINTS_SOURCE: &str = include_str!("fixtures/editor-lsp-hints.orna");
const SIGNATURE_SOURCE: &str = include_str!("fixtures/signature-actions-v1.orna");
const ACTION_SOURCE: &str = include_str!("fixtures/missing-semicolon-code-action-v1.orna");
const WORKSPACE_HIERARCHY_PROVIDER_SOURCE: &str =
    include_str!("fixtures/workspace-hierarchy-provider-v1.orna");
const WORKSPACE_HIERARCHY_CALLER_SOURCE: &str =
    include_str!("fixtures/workspace-hierarchy-caller-v1.orna");
const DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE: &str =
    include_str!("fixtures/document-highlight-code-lens-v1.orna");
const FOLDING_SELECTION_SOURCE: &str = include_str!("fixtures/folding-selection-v1.orna");
const DIAGNOSTIC_SOURCE: &str = include_str!("fixtures/incremental-malformed-v1.orna");
const DOCUMENT_LINKS_SOURCE: &str = include_str!("fixtures/document-links-v1.orna");
const DOCUMENT_LINK_TARGET_SOURCE: &str = include_str!("fixtures/library/math.orna");
const DOCUMENT_LINK_DIRECTORY_TARGET_SOURCE: &str = include_str!("fixtures/library/math/main.orna");
const INVALID_SOURCE: &str = include_str!("fixtures/ji3t0-invalid-v1.orna");

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
    assert!(
        capabilities["renameProvider"].as_bool() == Some(true)
            || capabilities["renameProvider"]["prepareProvider"] == true,
        "renameProvider must advertise rename support: {}",
        capabilities["renameProvider"]
    );
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
    hover_semantic_contract::assert_hover_contract(&hover, "LSP protocol");
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

    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":uri},
            "position":call_position,
            "context":{"includeDeclaration":true}
        }),
    );
    hover_semantic_contract::assert_references_contract(
        &[(uri, SOURCE)],
        &references,
        true,
        "LSP protocol",
    );
    let references_without_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":uri},
            "position":call_position,
            "context":{"includeDeclaration":false}
        }),
    );
    hover_semantic_contract::assert_references_contract(
        &[(uri, SOURCE)],
        &references_without_declaration,
        false,
        "LSP protocol without declaration",
    );

    let renamed = client.request(
        "textDocument/rename",
        json!({"textDocument":{"uri":uri},"position":call_position,"newName":"sum"}),
    );
    assert_no_legacy_words(&renamed, "rename response");
    hover_semantic_contract::assert_rename_contract(
        &[(uri, SOURCE)],
        &renamed,
        "sum",
        "LSP protocol",
    );

    let completion = client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":position_at(SOURCE,SOURCE.len())}),
    );
    assert_no_legacy_words(&completion, "completion response");
    completion_contract::assert_lsp_completion_contract(&completion, "LSP protocol");

    let semantic = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":uri}}),
    );
    assert_no_legacy_words(&semantic, "semantic-highlight response");
    let legends = capabilities["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .unwrap();
    hover_semantic_contract::assert_semantic_token_legend(
        &capabilities["semanticTokensProvider"]["legend"]["tokenTypes"],
        "LSP protocol",
    );
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

    let semantic_uri = "file:///workspace/editor-semantic-tokens.orna";
    let semantic_open = open(&mut client, semantic_uri, SEMANTIC_SOURCE);
    assert_no_legacy_words(&semantic_open, "semantic fixture diagnostics");
    let semantic_fixture = client.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":semantic_uri}}),
    );
    assert_no_legacy_words(&semantic_fixture, "semantic fixture response");
    hover_semantic_contract::assert_semantic_token_contract(
        SEMANTIC_SOURCE,
        &semantic_fixture,
        "LSP protocol",
    );

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

    let document_highlight_uri = "file:///workspace/document-highlight-code-lens-v1.orna";
    open(
        &mut client,
        document_highlight_uri,
        DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
    );
    let document_highlights = client.request(
        "textDocument/documentHighlight",
        json!({
            "textDocument":{"uri":document_highlight_uri},
            "position":syntax_v1_document_highlight_code_lens_contract::document_highlight_position(
                DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
            )
        }),
    );
    syntax_v1_document_highlight_code_lens_contract::assert_document_highlights_contract(
        DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
        &document_highlights,
        "LSP protocol",
    );

    let provider_uri = "file:///workspace/workspace-hierarchy-provider-v1.orna";
    let caller_uri = "file:///workspace/workspace-hierarchy-caller-v1.orna";
    open(
        &mut client,
        provider_uri,
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
    );
    open(&mut client, caller_uri, WORKSPACE_HIERARCHY_CALLER_SOURCE);
    let provider_code_lenses = client.request(
        "textDocument/codeLens",
        json!({"textDocument":{"uri":provider_uri}}),
    );
    let caller_code_lenses = client.request(
        "textDocument/codeLens",
        json!({"textDocument":{"uri":caller_uri}}),
    );
    syntax_v1_document_highlight_code_lens_contract::assert_code_lens_contract(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
        provider_uri,
        caller_uri,
        &provider_code_lenses,
        &caller_code_lenses,
        "LSP protocol",
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
fn neovim_loads_generated_v1_syntax_and_queries_the_lsp_server() {
    let Some(neovim) = neovim_binary() else {
        eprintln!("SKIP: Neovim is not installed; set ORNA_TEST_NEOVIM to its executable");
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates");
    let fixture_source = format!("{SOURCE}\nCREATE SCHEMA old_syntax;\n");
    let legacy_create_line = fixture_source
        .split_once("CREATE SCHEMA old_syntax;")
        .expect("legacy source row is in the generated fixture")
        .0
        .matches('\n')
        .count()
        + 1;
    let fixture = temporary_fixture(&fixture_source);
    let semantic_fixture = temporary_fixture(SEMANTIC_SOURCE);
    let hints_fixture = temporary_fixture(HINTS_SOURCE);
    let depth_ranges = syntax_v1_depth_contract::request_ranges(HINTS_SOURCE);
    let signature_fixture = temporary_fixture(SIGNATURE_SOURCE);
    let action_fixture = temporary_fixture(ACTION_SOURCE);
    let action_signature_requests =
        syntax_v1_action_signature_contract::request_data(SIGNATURE_SOURCE, ACTION_SOURCE);
    let workspace_provider_fixture =
        root.join("crates/orna-lsp/tests/fixtures/workspace-hierarchy-provider-v1.orna");
    let workspace_caller_fixture =
        root.join("crates/orna-lsp/tests/fixtures/workspace-hierarchy-caller-v1.orna");
    let document_highlight_fixture =
        root.join("crates/orna-lsp/tests/fixtures/document-highlight-code-lens-v1.orna");
    let folding_selection_fixture =
        root.join("crates/orna-lsp/tests/fixtures/folding-selection-v1.orna");
    assert_eq!(
        fs::read_to_string(&workspace_provider_fixture).unwrap(),
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&workspace_caller_fixture).unwrap(),
        WORKSPACE_HIERARCHY_CALLER_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_highlight_fixture).unwrap(),
        DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE
    );
    let workspace_hierarchy_requests = syntax_v1_workspace_hierarchy_contract::request_data(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
    );
    assert_eq!(
        fs::read_to_string(&folding_selection_fixture).unwrap(),
        FOLDING_SELECTION_SOURCE
    );
    let folding_selection_requests =
        syntax_v1_folding_selection_contract::request_data(FOLDING_SELECTION_SOURCE);
    let diagnostic_fixture =
        root.join("crates/orna-lsp/tests/fixtures/incremental-malformed-v1.orna");
    assert_eq!(
        fs::read_to_string(&diagnostic_fixture).unwrap(),
        DIAGNOSTIC_SOURCE
    );
    let diagnostic_recovery = DIAGNOSTIC_SOURCE
        .replace("value + ;", "value + 1;")
        .trim_end()
        .to_owned();
    assert!(
        orna_syntax_v1::parse_module(&diagnostic_recovery)
            .diagnostics
            .is_empty()
    );
    let document_links_fixture = root.join("crates/orna-lsp/tests/fixtures/document-links-v1.orna");
    let document_link_target_fixture =
        root.join("crates/orna-lsp/tests/fixtures/library/math.orna");
    let document_link_directory_target_fixture =
        root.join("crates/orna-lsp/tests/fixtures/library/math/main.orna");
    assert_eq!(
        fs::read_to_string(&document_links_fixture).unwrap(),
        DOCUMENT_LINKS_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_link_target_fixture).unwrap(),
        DOCUMENT_LINK_TARGET_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_link_directory_target_fixture).unwrap(),
        DOCUMENT_LINK_DIRECTORY_TARGET_SOURCE
    );
    let script = temporary_path("lua");
    let result_path = temporary_path("result");
    let completion_result_path = temporary_path("completion.json");
    let hover_semantic_result_path = temporary_path("hover-semantic.json");
    fs::write(
        &script,
        r#"
vim.opt.runtimepath:prepend(vim.env.ORNA_NEOVIM_RUNTIME)
vim.cmd("filetype on")
vim.cmd("syntax on")
require("orna").setup({
  cmd = { vim.env.ORNA_LSP_BIN },
  root_dir = vim.env.ORNA_PROJECT_ROOT,
})
vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_TEST_FIXTURE))
vim.cmd("source " .. vim.fn.fnameescape(vim.env.ORNA_VIM_SYNTAX))
local bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[bufnr].filetype == "orna", "generated ftdetect did not select the orna filetype")
assert(vim.v.errmsg == "", "generated syntax raised an editor error: " .. vim.v.errmsg)
local function syntax_group(line, column)
  return vim.fn.synIDattr(vim.fn.synID(line, column, 1), "name")
end
local pub_group = syntax_group(2, 1)
local fn_group = syntax_group(2, 5)
local legacy_group = syntax_group(tonumber(vim.env.ORNA_LEGACY_CREATE_LINE), 1)
assert(pub_group == "ornaKeyword", "pub has syntax group " .. pub_group)
assert(fn_group == "ornaKeyword", "fn has syntax group " .. fn_group)
assert(legacy_group == "ornaIdentifier", "legacy CREATE has syntax group " .. legacy_group)

local client
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = bufnr })) do
    if attached.name == "orna" and attached.initialized then
      client = attached
      return true
    end
  end
  return false
end, 10), "generated Neovim setup did not attach orna-lsp")
local uri = vim.uri_from_bufnr(bufnr)
local hover, hover_error = client:request_sync("textDocument/hover", {
  textDocument = { uri = uri },
  position = { line = 1, character = 8 },
}, 5000, bufnr)
assert(hover ~= nil and hover.err == nil, "Neovim hover request failed: " .. vim.inspect(hover_error or hover))
local hover_result = hover.result
local hover_contents = hover.result.contents
local hover_text = type(hover_contents) == "string" and hover_contents or hover_contents.value
assert(string.find(hover_text, "fn add(left: Int, right: Int): Int", 1, true), "unexpected hover: " .. hover_text)
assert(string.find(hover_text, "Add two integer values.", 1, true), "hover lost fixture documentation: " .. hover_text)

local completion, completion_error = client:request_sync("textDocument/completion", {
  textDocument = { uri = uri },
  position = vim.fn.json_decode(vim.env.ORNA_COMPLETION_POSITION),
}, 5000, bufnr)
assert(completion ~= nil and completion.err == nil, "Neovim completion request failed: " .. vim.inspect(completion_error or completion))
vim.fn.writefile({ vim.fn.json_encode(completion.result) }, vim.env.ORNA_COMPLETION_RESULT)
local completion_items = completion.result.items or completion.result
local expected_keywords = vim.fn.json_decode(vim.env.ORNA_EXPECTED_KEYWORDS)
local expected_keyword_set = {}
for _, keyword in ipairs(expected_keywords) do
  expected_keyword_set[keyword] = true
end
local actual_keyword_set = {}
local actual_keyword_count = 0
local add_completion
for _, item in ipairs(completion_items) do
  if item.kind == 14 then
    actual_keyword_set[item.label] = true
    actual_keyword_count = actual_keyword_count + 1
    assert(expected_keyword_set[item.label], "unexpected completion keyword: " .. item.label)
  elseif item.label == "add" then
    add_completion = item
  end
end
assert(actual_keyword_count == #expected_keywords, "completion keyword inventory has " .. actual_keyword_count .. " entries, expected " .. #expected_keywords)
for _, keyword in ipairs(expected_keywords) do
  assert(actual_keyword_set[keyword], "completion keyword missing syntax-v1 token: " .. keyword)
end
assert(not actual_keyword_set.CREATE and not actual_keyword_set.SELECT, "pre-v1 keyword escaped into Neovim completions")
assert(add_completion ~= nil, "Neovim completion omitted fixture function add")
assert(add_completion.detail == "pub fn add(left: Int, right: Int): Int", "unexpected add completion detail: " .. vim.inspect(add_completion))
assert(add_completion.documentation.kind == "markdown", "add completion documentation is not Markdown: " .. vim.inspect(add_completion))
assert(add_completion.documentation.value == "Add two integer values.", "add completion lost fixture documentation: " .. vim.inspect(add_completion))
assert(add_completion.insertText == "add(${1:left}, ${2:right})", "unexpected add completion snippet: " .. vim.inspect(add_completion))
assert(add_completion.insertTextFormat == 2, "add completion did not advertise snippet formatting: " .. vim.inspect(add_completion))

local expected_reference_positions = vim.fn.json_decode(vim.env.ORNA_REFERENCE_POSITIONS)
local expected_reference_set = {}
for _, position in ipairs(expected_reference_positions) do
  expected_reference_set[tostring(position.line) .. ":" .. tostring(position.character)] = true
end
local references, references_error = client:request_sync("textDocument/references", {
  textDocument = { uri = uri },
  position = vim.fn.json_decode(vim.env.ORNA_RENAME_POSITION),
  context = { includeDeclaration = true },
}, 5000, bufnr)
assert(references ~= nil and references.err == nil, "Neovim references request failed: " .. vim.inspect(references_error or references))
assert(#references.result == 2, "Neovim references returned " .. #references.result .. " locations: " .. vim.inspect(references.result))
for _, location in ipairs(references.result) do
  assert(location.uri == uri, "Neovim references returned another document: " .. vim.inspect(location))
  local start = location.range.start
  local key = tostring(start.line) .. ":" .. tostring(start.character)
  assert(expected_reference_set[key], "Neovim references returned an unexpected symbol range: " .. vim.inspect(location))
  expected_reference_set[key] = nil
end
assert(next(expected_reference_set) == nil, "Neovim references omitted expected declaration or call sites: " .. vim.inspect(expected_reference_set))
local references_without_declaration, references_without_declaration_error = client:request_sync("textDocument/references", {
  textDocument = { uri = uri },
  position = vim.fn.json_decode(vim.env.ORNA_RENAME_POSITION),
  context = { includeDeclaration = false },
}, 5000, bufnr)
assert(references_without_declaration ~= nil and references_without_declaration.err == nil, "Neovim call-site references request failed: " .. vim.inspect(references_without_declaration_error or references_without_declaration))

local renamed, rename_error = client:request_sync("textDocument/rename", {
  textDocument = { uri = uri },
  position = vim.fn.json_decode(vim.env.ORNA_RENAME_POSITION),
  newName = "sum",
}, 5000, bufnr)
assert(renamed ~= nil and renamed.err == nil, "Neovim rename request failed: " .. vim.inspect(rename_error or renamed))
local rename_edits = renamed.result.changes[uri]
assert(rename_edits ~= nil and #rename_edits == 2, "Neovim rename returned unexpected edits: " .. vim.inspect(renamed.result))
local rename_edit_positions = {}
for _, edit in ipairs(rename_edits) do
  assert(edit.newText == "sum", "Neovim rename used unexpected replacement text: " .. vim.inspect(edit))
  local start = edit.range.start
  local key = tostring(start.line) .. ":" .. tostring(start.character)
  rename_edit_positions[key] = true
end
assert(#rename_edits == 2, "Neovim rename did not edit both declaration and call")
for _, position in ipairs(expected_reference_positions) do
  local key = tostring(position.line) .. ":" .. tostring(position.character)
  assert(rename_edit_positions[key], "Neovim rename omitted expected location " .. key)
end

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_SEMANTIC_FIXTURE))
local semantic_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[semantic_bufnr].filetype == "orna", "semantic fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = semantic_bufnr })) do
    if attached.name == "orna" and attached.initialized then
      client = attached
      return true
    end
  end
  return false
end, 10), "semantic fixture did not attach to orna-lsp")
local semantic, semantic_error = client:request_sync("textDocument/semanticTokens/full", {
  textDocument = { uri = vim.uri_from_bufnr(semantic_bufnr) },
}, 5000, semantic_bufnr)
assert(semantic ~= nil and semantic.err == nil, "Neovim semantic-token request failed: " .. vim.inspect(semantic_error or semantic))
local token_types = client.server_capabilities.semanticTokensProvider.legend.tokenTypes
local keyword_type
for index, token_type in ipairs(token_types) do
  if token_type == "keyword" then keyword_type = index - 1 end
end
assert(keyword_type ~= nil, "orna-lsp did not advertise the syntax-v1 keyword token")
local line, character = 0, 0
local found_pub = false
for index = 1, #semantic.result.data, 5 do
  local delta_line, delta_start = semantic.result.data[index], semantic.result.data[index + 1]
  if delta_line == 0 then character = character + delta_start else line, character = line + delta_line, delta_start end
  if line == 3 and character == 0 and semantic.result.data[index + 3] == keyword_type then
    found_pub = true
  end
end
assert(found_pub, "Neovim LSP semantic tokens did not mark pub as an Orna 1.0 keyword")
local expected_classes = {
  keyword = false,
  variable = false,
  number = false,
  string = false,
  comment = false,
  operator = false,
}
local source_lines = vim.api.nvim_buf_get_lines(semantic_bufnr, 0, -1, false)
line, character = 0, 0
for index = 1, #semantic.result.data, 5 do
  local delta_line, delta_start = semantic.result.data[index], semantic.result.data[index + 1]
  if delta_line == 0 then character = character + delta_start else line, character = line + delta_line, delta_start end
  local token_length = semantic.result.data[index + 2]
  local token_type = token_types[semantic.result.data[index + 3] + 1]
  local token_text = string.sub(source_lines[line + 1], character + 1, character + token_length)
  if token_type == "keyword" and token_text == "let" then expected_classes.keyword = true end
  if token_type == "variable" and token_text == "total" then expected_classes.variable = true end
  if token_type == "number" and token_text == "12" then expected_classes.number = true end
  if token_type == "string" then expected_classes.string = true end
  if token_type == "comment" and string.find(token_text, "comment", 1, true) then expected_classes.comment = true end
  if token_type == "operator" and token_text == "+" then expected_classes.operator = true end
end
for token_type, found in pairs(expected_classes) do
  assert(found, "Neovim LSP semantic tokens omitted the syntax-v1 " .. token_type .. " class")
end

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_HINTS_FIXTURE))
local hints_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[hints_bufnr].filetype == "orna", "hint fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = hints_bufnr })) do
    if attached.name == "orna" and attached.initialized then
      client = attached
      return true
    end
  end
  return false
end, 10), "hint fixture did not attach to orna-lsp")
local hints_uri = vim.uri_from_bufnr(hints_bufnr)
local depth_ranges = vim.fn.json_decode(vim.env.ORNA_DEPTH_RANGES)
local depth_semantic, depth_semantic_error = client:request_sync("textDocument/semanticTokens/full", {
  textDocument = { uri = hints_uri },
}, 5000, hints_bufnr)
assert(depth_semantic ~= nil and depth_semantic.err == nil, "Neovim depth semantic-token request failed: " .. vim.inspect(depth_semantic_error or depth_semantic))
local depth_semantic_range, depth_semantic_range_error = client:request_sync("textDocument/semanticTokens/range", {
  textDocument = { uri = hints_uri },
  range = depth_ranges.semantic,
}, 5000, hints_bufnr)
assert(depth_semantic_range ~= nil and depth_semantic_range.err == nil, "Neovim ranged semantic-token request failed: " .. vim.inspect(depth_semantic_range_error or depth_semantic_range))
local function request_hints(range_name)
  local response, request_error = client:request_sync("textDocument/inlayHint", {
    textDocument = { uri = hints_uri },
    range = depth_ranges[range_name],
  }, 5000, hints_bufnr)
  assert(response ~= nil and response.err == nil, "Neovim " .. range_name .. " inlay request failed: " .. vim.inspect(request_error or response))
  return response.result
end
local depth_hints_full = request_hints("hints_full")
local depth_hints_call = request_hints("hints_call")
local depth_hints_inferred = request_hints("hints_inferred")
local depth_hints_annotated = request_hints("hints_annotated")
local depth_hints_shadowed = request_hints("hints_shadowed")

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_SIGNATURE_FIXTURE))
local signature_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[signature_bufnr].filetype == "orna", "signature fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = signature_bufnr })) do
    if attached.name == "orna" and attached.initialized then
      client = attached
      return true
    end
  end
  return false
end, 10), "signature fixture did not attach to orna-lsp")
local signature_uri = vim.uri_from_bufnr(signature_bufnr)
local action_signature_requests = vim.fn.json_decode(vim.env.ORNA_ACTION_SIGNATURE_REQUESTS)
local function request_signature(request_name)
  local response, request_error = client:request_sync("textDocument/signatureHelp", {
    textDocument = { uri = signature_uri },
    position = action_signature_requests[request_name],
  }, 5000, signature_bufnr)
  assert(response ~= nil and response.err == nil, "Neovim " .. request_name .. " signature request failed: " .. vim.inspect(request_error or response))
  return response.result
end
local signature_nested_tuple = request_signature("signature_nested_tuple")
local signature_named_argument = request_signature("signature_named_argument")
local signature_nested_named_argument = request_signature("signature_nested_named_argument")
local signature_shadowed_call = request_signature("signature_shadowed_call")

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_ACTION_FIXTURE))
local action_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[action_bufnr].filetype == "orna", "code-action fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = action_bufnr })) do
    if attached.name == "orna" and attached.initialized then
      client = attached
      return true
    end
  end
  return false
end, 10), "code-action fixture did not attach to orna-lsp")
local action_uri = vim.uri_from_bufnr(action_bufnr)
local function request_action(range_name, only_kind)
  local response, request_error = client:request_sync("textDocument/codeAction", {
    textDocument = { uri = action_uri },
    range = action_signature_requests[range_name],
    context = { diagnostics = {}, only = { only_kind } },
  }, 5000, action_bufnr)
  assert(response ~= nil and response.err == nil, "Neovim code-action request failed: " .. vim.inspect(request_error or response))
  return response.result
end
local code_action_quickfix = request_action("code_action_full_range", "quickfix")
local code_action_wrong_kind = request_action("code_action_full_range", "refactor")
local code_action_outside_range = request_action("code_action_outside_range", "quickfix")

local workspace_hierarchy_requests = vim.fn.json_decode(vim.env.ORNA_WORKSPACE_HIERARCHY_REQUESTS)
local workspace_provider_fixture = vim.env.ORNA_WORKSPACE_PROVIDER_FIXTURE
vim.cmd("edit " .. vim.fn.fnameescape(workspace_provider_fixture))
local workspace_provider_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[workspace_provider_bufnr].filetype == "orna", "workspace provider fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = workspace_provider_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "workspace provider fixture did not attach to orna-lsp")
local workspace_provider_uri = vim.uri_from_bufnr(workspace_provider_bufnr)
vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_WORKSPACE_CALLER_FIXTURE))
local workspace_caller_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[workspace_caller_bufnr].filetype == "orna", "workspace caller fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = workspace_caller_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "workspace caller fixture did not attach to orna-lsp")
local workspace_caller_uri = vim.uri_from_bufnr(workspace_caller_bufnr)
local function hierarchy_request(method, params, bufnr)
  local response, request_error = client:request_sync(method, params, 5000, bufnr)
  assert(response ~= nil and response.err == nil, "Neovim " .. method .. " request failed: " .. vim.inspect(request_error or response))
  return response.result
end
local workspace_mid = hierarchy_request("workspace/symbol", { query = "mid" }, workspace_caller_bufnr)
local workspace_mid_repeat = hierarchy_request("workspace/symbol", { query = "mid" }, workspace_caller_bufnr)
local workspace_rem = hierarchy_request("workspace/symbol", { query = "rem" }, workspace_caller_bufnr)
local workspace_remi = hierarchy_request("workspace/symbol", { query = "remi" }, workspace_caller_bufnr)
local call_root_item = hierarchy_request("textDocument/prepareCallHierarchy", {
  textDocument = { uri = workspace_provider_uri },
  position = workspace_hierarchy_requests.root_definition,
}, workspace_provider_bufnr)
local call_root_outgoing = hierarchy_request("callHierarchy/outgoingCalls", { item = call_root_item[1] }, workspace_provider_bufnr)
local call_root_incoming = hierarchy_request("callHierarchy/incomingCalls", { item = call_root_item[1] }, workspace_provider_bufnr)
local call_seed_item = hierarchy_request("textDocument/prepareCallHierarchy", {
  textDocument = { uri = workspace_provider_uri },
  position = workspace_hierarchy_requests.seed_definition,
}, workspace_provider_bufnr)
local call_seed_incoming = hierarchy_request("callHierarchy/incomingCalls", { item = call_seed_item[1] }, workspace_provider_bufnr)
local call_shadowed_item = hierarchy_request("textDocument/prepareCallHierarchy", {
  textDocument = { uri = workspace_provider_uri },
  position = workspace_hierarchy_requests.shadowed_definition,
}, workspace_provider_bufnr)
local call_shadowed_outgoing = hierarchy_request("callHierarchy/outgoingCalls", { item = call_shadowed_item[1] }, workspace_provider_bufnr)
local call_unresolved_item = hierarchy_request("textDocument/prepareCallHierarchy", {
  textDocument = { uri = workspace_provider_uri },
  position = workspace_hierarchy_requests.unresolved_definition,
}, workspace_provider_bufnr)
local call_unresolved_outgoing = hierarchy_request("callHierarchy/outgoingCalls", { item = call_unresolved_item[1] }, workspace_provider_bufnr)
local call_root_reference = hierarchy_request("textDocument/prepareCallHierarchy", {
  textDocument = { uri = workspace_caller_uri },
  position = workspace_hierarchy_requests.root_reference,
}, workspace_caller_bufnr)
local call_ambiguous_reference = hierarchy_request("textDocument/prepareCallHierarchy", {
  textDocument = { uri = workspace_caller_uri },
  position = workspace_hierarchy_requests.ambiguous_reference,
}, workspace_caller_bufnr)
local workspace_provider_code_lenses = hierarchy_request("textDocument/codeLens", {
  textDocument = { uri = workspace_provider_uri },
}, workspace_provider_bufnr)
local workspace_caller_code_lenses = hierarchy_request("textDocument/codeLens", {
  textDocument = { uri = workspace_caller_uri },
}, workspace_caller_bufnr)

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_DOCUMENT_HIGHLIGHT_FIXTURE))
local document_highlight_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[document_highlight_bufnr].filetype == "orna", "document-highlight fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = document_highlight_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "document-highlight fixture did not attach to orna-lsp")
local document_highlight_uri = vim.uri_from_bufnr(document_highlight_bufnr)
local document_highlights = hierarchy_request("textDocument/documentHighlight", {
  textDocument = { uri = document_highlight_uri },
  position = vim.fn.json_decode(vim.env.ORNA_DOCUMENT_HIGHLIGHT_POSITION),
}, document_highlight_bufnr)

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_FOLDING_SELECTION_FIXTURE))
local folding_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[folding_bufnr].filetype == "orna", "folding/selection fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = folding_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "folding/selection fixture did not attach to orna-lsp")
local folding_uri = vim.uri_from_bufnr(folding_bufnr)
local folding_ranges = hierarchy_request("textDocument/foldingRange", {
  textDocument = { uri = folding_uri },
}, folding_bufnr)
local folding_selection_requests = vim.fn.json_decode(vim.env.ORNA_FOLDING_SELECTION_REQUESTS)
local selection_ranges = hierarchy_request("textDocument/selectionRange", {
  textDocument = { uri = folding_uri },
  positions = folding_selection_requests.selection_positions,
}, folding_bufnr)

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_DIAGNOSTIC_FIXTURE))
local diagnostic_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[diagnostic_bufnr].filetype == "orna", "diagnostic fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = diagnostic_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "diagnostic fixture did not attach to orna-lsp")
local diagnostic_uri = vim.uri_from_bufnr(diagnostic_bufnr)
local diagnostic_invalid_result = hierarchy_request("textDocument/diagnostic", {
  textDocument = { uri = diagnostic_uri },
}, diagnostic_bufnr)
local diagnostic_invalid = diagnostic_invalid_result.items
vim.api.nvim_buf_set_lines(diagnostic_bufnr, 0, -1, false, { vim.env.ORNA_DIAGNOSTIC_RECOVERY_SOURCE })
local diagnostic_recovered_result = hierarchy_request("textDocument/diagnostic", {
  textDocument = { uri = diagnostic_uri },
}, diagnostic_bufnr)
local diagnostic_recovered = diagnostic_recovered_result.items

vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_DOCUMENT_LINKS_FIXTURE))
local document_links_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[document_links_bufnr].filetype == "orna", "document-links fixture did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = document_links_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "document-links fixture did not attach to orna-lsp")
local document_links_uri = vim.uri_from_bufnr(document_links_bufnr)
local document_links_unresolved = hierarchy_request("textDocument/documentLink", {
  textDocument = { uri = document_links_uri },
}, document_links_bufnr)
vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_DOCUMENT_LINK_TARGET_FIXTURE))
local document_link_target_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[document_link_target_bufnr].filetype == "orna", "document-link target did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = document_link_target_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "document-link target did not attach to orna-lsp")
local document_links_target_uri = vim.uri_from_bufnr(document_link_target_bufnr)
local document_links_resolved = hierarchy_request("textDocument/documentLink", {
  textDocument = { uri = document_links_uri },
}, document_links_bufnr)
vim.cmd("edit " .. vim.fn.fnameescape(vim.env.ORNA_DOCUMENT_LINK_DIRECTORY_FIXTURE))
local document_link_directory_bufnr = vim.api.nvim_get_current_buf()
assert(vim.bo[document_link_directory_bufnr].filetype == "orna", "ambiguous document-link target did not select the orna filetype")
assert(vim.wait(5000, function()
  for _, attached in ipairs(vim.lsp.get_clients({ bufnr = document_link_directory_bufnr })) do
    if attached.name == "orna" and attached.initialized then client = attached; return true end
  end
  return false
end, 10), "ambiguous document-link target did not attach to orna-lsp")
local document_links_ambiguous = hierarchy_request("textDocument/documentLink", {
  textDocument = { uri = document_links_uri },
}, document_links_bufnr)
vim.fn.writefile({ vim.fn.json_encode({
  uri = uri,
  hover = hover_result,
  semantic = semantic.result,
  legend = token_types,
  depth_semantic = depth_semantic.result,
  depth_semantic_range = depth_semantic_range.result,
  depth_hints_full = depth_hints_full,
  depth_hints_call = depth_hints_call,
  depth_hints_inferred = depth_hints_inferred,
  depth_hints_annotated = depth_hints_annotated,
  depth_hints_shadowed = depth_hints_shadowed,
  signature_nested_tuple = signature_nested_tuple,
  signature_named_argument = signature_named_argument,
  signature_nested_named_argument = signature_nested_named_argument,
  signature_shadowed_call = signature_shadowed_call,
  code_action_quickfix = code_action_quickfix,
  code_action_wrong_kind = code_action_wrong_kind,
  code_action_outside_range = code_action_outside_range,
  action_uri = action_uri,
  workspace_mid = workspace_mid,
  workspace_mid_repeat = workspace_mid_repeat,
  workspace_rem = workspace_rem,
  workspace_remi = workspace_remi,
  call_root_item = call_root_item,
  call_root_outgoing = call_root_outgoing,
  call_root_incoming = call_root_incoming,
  call_seed_item = call_seed_item,
  call_seed_incoming = call_seed_incoming,
  call_shadowed_outgoing = call_shadowed_outgoing,
  call_unresolved_outgoing = call_unresolved_outgoing,
  call_root_reference = call_root_reference,
  call_ambiguous_reference = call_ambiguous_reference,
  workspace_provider_uri = workspace_provider_uri,
  workspace_caller_uri = workspace_caller_uri,
  workspace_provider_code_lenses = workspace_provider_code_lenses,
  workspace_caller_code_lenses = workspace_caller_code_lenses,
  document_highlights = document_highlights,
  folding_ranges = folding_ranges,
  selection_ranges = selection_ranges,
  folding_uri = folding_uri,
  diagnostic_invalid = diagnostic_invalid,
  diagnostic_recovered = diagnostic_recovered,
  diagnostic_uri = diagnostic_uri,
  document_links_unresolved = document_links_unresolved,
  document_links_resolved = document_links_resolved,
  document_links_ambiguous = document_links_ambiguous,
  document_links_uri = document_links_uri,
  document_links_target_uri = document_links_target_uri,
  references = references.result,
  references_without_declaration = references_without_declaration.result,
  rename = renamed.result,
}) }, vim.env.ORNA_HOVER_SEMANTIC_RESULT)

vim.fn.writefile({
  "FILETYPE=" .. vim.bo[bufnr].filetype,
  "PUB=" .. pub_group,
  "FN=" .. fn_group,
  "LEGACY_CREATE=" .. legacy_group,
  "LSP_ATTACHMENT=pass",
  "LSP_HOVER=pass",
  "LSP_COMPLETION_KEYWORDS=pass",
  "LSP_COMPLETION_ADD=pass",
  "LSP_REFERENCES=pass",
  "LSP_RENAME=pass",
  "LSP_SEMANTIC_PUB=pass",
  "LSP_SEMANTIC_CLASSES=pass",
  "LSP_SIGNATURE_HELP=pass",
  "LSP_CODE_ACTION=pass",
  "LSP_WORKSPACE_SYMBOL=pass",
  "LSP_CALL_HIERARCHY=pass",
  "LSP_DOCUMENT_HIGHLIGHT=pass",
  "LSP_CODE_LENS=pass",
  "LSP_FOLDING_RANGE=pass",
  "LSP_SELECTION_RANGE=pass",
  "LSP_DIAGNOSTICS=pass",
  "LSP_DOCUMENT_LINKS=pass",
}, vim.env.ORNA_EDITOR_RESULT)
client:stop(true)
vim.cmd("qa!")
"#,
    )
    .expect("write Neovim integration script");
    let output = Command::new(&neovim)
        .arg("--headless")
        .arg("-u")
        .arg("NONE")
        .arg("-i")
        .arg("NONE")
        .arg("-n")
        .arg("-c")
        .arg("lua dofile(vim.env.ORNA_PROBE_SCRIPT)")
        .arg("-c")
        .arg("qa!")
        .current_dir(root)
        .env("ORNA_VIM_SYNTAX", root.join("editors/vim/syntax/orna.vim"))
        .env("ORNA_NEOVIM_RUNTIME", root.join("editors/neovim"))
        .env("ORNA_TEST_FIXTURE", &fixture)
        .env("ORNA_SEMANTIC_FIXTURE", &semantic_fixture)
        .env("ORNA_HINTS_FIXTURE", &hints_fixture)
        .env("ORNA_SIGNATURE_FIXTURE", &signature_fixture)
        .env("ORNA_ACTION_FIXTURE", &action_fixture)
        .env(
            "ORNA_WORKSPACE_PROVIDER_FIXTURE",
            &workspace_provider_fixture,
        )
        .env("ORNA_WORKSPACE_CALLER_FIXTURE", &workspace_caller_fixture)
        .env(
            "ORNA_DOCUMENT_HIGHLIGHT_FIXTURE",
            &document_highlight_fixture,
        )
        .env("ORNA_FOLDING_SELECTION_FIXTURE", &folding_selection_fixture)
        .env("ORNA_DIAGNOSTIC_FIXTURE", &diagnostic_fixture)
        .env("ORNA_DIAGNOSTIC_RECOVERY_SOURCE", &diagnostic_recovery)
        .env("ORNA_DOCUMENT_LINKS_FIXTURE", &document_links_fixture)
        .env(
            "ORNA_DOCUMENT_LINK_TARGET_FIXTURE",
            &document_link_target_fixture,
        )
        .env(
            "ORNA_DOCUMENT_LINK_DIRECTORY_FIXTURE",
            &document_link_directory_target_fixture,
        )
        .env(
            "ORNA_DEPTH_RANGES",
            serde_json::to_string(&depth_ranges).unwrap(),
        )
        .env(
            "ORNA_ACTION_SIGNATURE_REQUESTS",
            serde_json::to_string(&action_signature_requests).unwrap(),
        )
        .env(
            "ORNA_WORKSPACE_HIERARCHY_REQUESTS",
            serde_json::to_string(&workspace_hierarchy_requests).unwrap(),
        )
        .env(
            "ORNA_DOCUMENT_HIGHLIGHT_POSITION",
            serde_json::to_string(
                &syntax_v1_document_highlight_code_lens_contract::document_highlight_position(
                    DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
                ),
            )
            .unwrap(),
        )
        .env(
            "ORNA_FOLDING_SELECTION_REQUESTS",
            serde_json::to_string(&folding_selection_requests).unwrap(),
        )
        .env("ORNA_LSP_BIN", env!("CARGO_BIN_EXE_orna-lsp"))
        .env("ORNA_PROJECT_ROOT", root)
        .env(
            "ORNA_COMPLETION_POSITION",
            serde_json::to_string(&position_at(SOURCE, SOURCE.find("add(value, 2)").unwrap()))
                .unwrap(),
        )
        .env(
            "ORNA_RENAME_POSITION",
            serde_json::to_string(&position_at(
                SOURCE,
                SOURCE.find("add(value, 2)").unwrap() + 1,
            ))
            .unwrap(),
        )
        .env(
            "ORNA_REFERENCE_POSITIONS",
            serde_json::to_string(&[
                position_of(SOURCE, "pub fn add", "pub fn ".len()),
                position_of(SOURCE, "add(value, 2)", 0),
            ])
            .unwrap(),
        )
        .env(
            "ORNA_EXPECTED_KEYWORDS",
            serde_json::to_string(&expected_keywords().into_iter().collect::<Vec<_>>()).unwrap(),
        )
        .env("ORNA_LEGACY_CREATE_LINE", legacy_create_line.to_string())
        .env("ORNA_COMPLETION_RESULT", &completion_result_path)
        .env("ORNA_HOVER_SEMANTIC_RESULT", &hover_semantic_result_path)
        .env("ORNA_PROBE_SCRIPT", &script)
        .env("ORNA_EDITOR_RESULT", &result_path)
        .output()
        .unwrap_or_else(|error| panic!("start Neovim at {}: {error}", neovim.display()));
    let editor_result = fs::read_to_string(&result_path);
    let completion_result = fs::read_to_string(&completion_result_path);
    let hover_semantic_result = fs::read_to_string(&hover_semantic_result_path);
    let _ = fs::remove_file(fixture);
    let _ = fs::remove_file(semantic_fixture);
    let _ = fs::remove_file(hints_fixture);
    let _ = fs::remove_file(signature_fixture);
    let _ = fs::remove_file(action_fixture);
    let _ = fs::remove_file(script);
    let _ = fs::remove_file(result_path);
    let _ = fs::remove_file(&completion_result_path);
    let _ = fs::remove_file(&hover_semantic_result_path);
    let editor_result = editor_result.unwrap_or_else(|error| {
        panic!(
            "Neovim did not write integration evidence (exit {:?}): {error}\n{}\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    });
    assert!(
        output.status.success(),
        "Neovim integration failed (exit {:?}):\n{}\n{}\n{editor_result}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let completion_result = completion_result.expect("Neovim completion result JSON");
    let completion_result: Value =
        serde_json::from_str(&completion_result).expect("decode Neovim completion result");
    completion_contract::assert_lsp_completion_contract(&completion_result, "Neovim LSP");
    let hover_semantic_result = hover_semantic_result.expect("Neovim hover and semantic JSON");
    let hover_semantic_result: Value = serde_json::from_str(&hover_semantic_result)
        .expect("decode Neovim hover and semantic result");
    hover_semantic_contract::assert_hover_contract(&hover_semantic_result["hover"], "Neovim");
    hover_semantic_contract::assert_semantic_token_legend(
        &hover_semantic_result["legend"],
        "Neovim",
    );
    hover_semantic_contract::assert_semantic_token_contract(
        SEMANTIC_SOURCE,
        &hover_semantic_result["semantic"],
        "Neovim",
    );
    syntax_v1_depth_contract::assert_semantic_depth_contract(
        HINTS_SOURCE,
        &hover_semantic_result["depth_semantic"],
        &hover_semantic_result["depth_semantic_range"],
        "Neovim",
    );
    syntax_v1_depth_contract::assert_inlay_hint_depth_contract(
        HINTS_SOURCE,
        &hover_semantic_result["depth_hints_full"],
        &hover_semantic_result["depth_hints_call"],
        &hover_semantic_result["depth_hints_inferred"],
        &hover_semantic_result["depth_hints_annotated"],
        &hover_semantic_result["depth_hints_shadowed"],
        "Neovim",
    );
    syntax_v1_action_signature_contract::assert_signature_help_contract(
        &hover_semantic_result["signature_nested_tuple"],
        &hover_semantic_result["signature_named_argument"],
        &hover_semantic_result["signature_nested_named_argument"],
        &hover_semantic_result["signature_shadowed_call"],
        "Neovim",
    );
    syntax_v1_action_signature_contract::assert_code_action_contract(
        ACTION_SOURCE,
        hover_semantic_result["action_uri"]
            .as_str()
            .expect("Neovim code-action document URI"),
        &hover_semantic_result["code_action_quickfix"],
        &hover_semantic_result["code_action_wrong_kind"],
        &hover_semantic_result["code_action_outside_range"],
        "Neovim",
    );
    syntax_v1_workspace_hierarchy_contract::assert_contract(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
        hover_semantic_result["workspace_provider_uri"]
            .as_str()
            .expect("Neovim workspace provider URI"),
        hover_semantic_result["workspace_caller_uri"]
            .as_str()
            .expect("Neovim workspace caller URI"),
        &hover_semantic_result,
        "Neovim",
    );
    syntax_v1_document_highlight_code_lens_contract::assert_document_highlights_contract(
        DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
        &hover_semantic_result["document_highlights"],
        "Neovim",
    );
    syntax_v1_document_highlight_code_lens_contract::assert_code_lens_contract(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
        hover_semantic_result["workspace_provider_uri"]
            .as_str()
            .expect("Neovim workspace provider URI"),
        hover_semantic_result["workspace_caller_uri"]
            .as_str()
            .expect("Neovim workspace caller URI"),
        &hover_semantic_result["workspace_provider_code_lenses"],
        &hover_semantic_result["workspace_caller_code_lenses"],
        "Neovim",
    );
    syntax_v1_folding_selection_contract::assert_contract(
        FOLDING_SELECTION_SOURCE,
        &hover_semantic_result["folding_ranges"],
        &hover_semantic_result["selection_ranges"],
        "Neovim",
    );
    syntax_v1_diagnostics_document_links_contract::assert_diagnostics(
        DIAGNOSTIC_SOURCE,
        &hover_semantic_result["diagnostic_invalid"],
        "Neovim",
    );
    syntax_v1_diagnostics_document_links_contract::assert_diagnostics_cleared_items(
        &hover_semantic_result["diagnostic_recovered"],
        "Neovim",
    );
    syntax_v1_diagnostics_document_links_contract::assert_no_document_links(
        &hover_semantic_result["document_links_unresolved"],
        "Neovim without open targets",
    );
    syntax_v1_diagnostics_document_links_contract::assert_document_link(
        DOCUMENT_LINKS_SOURCE,
        &hover_semantic_result["document_links_resolved"],
        hover_semantic_result["document_links_target_uri"]
            .as_str()
            .expect("Neovim document-link target URI"),
        "Neovim",
    );
    syntax_v1_diagnostics_document_links_contract::assert_no_document_links(
        &hover_semantic_result["document_links_ambiguous"],
        "Neovim with ambiguous targets",
    );
    let attached_uri = hover_semantic_result["uri"]
        .as_str()
        .expect("Neovim attached document URI");
    hover_semantic_contract::assert_references_contract(
        &[(attached_uri, SOURCE)],
        &hover_semantic_result["references"],
        true,
        "Neovim",
    );
    hover_semantic_contract::assert_references_contract(
        &[(attached_uri, SOURCE)],
        &hover_semantic_result["references_without_declaration"],
        false,
        "Neovim without declaration",
    );
    hover_semantic_contract::assert_rename_contract(
        &[(attached_uri, SOURCE)],
        &hover_semantic_result["rename"],
        "sum",
        "Neovim",
    );
    println!("Neovim integration evidence:\n{editor_result}");
    assert_eq!(
        editor_result.lines().collect::<Vec<_>>(),
        [
            "FILETYPE=orna",
            "PUB=ornaKeyword",
            "FN=ornaKeyword",
            "LEGACY_CREATE=ornaIdentifier",
            "LSP_ATTACHMENT=pass",
            "LSP_HOVER=pass",
            "LSP_COMPLETION_KEYWORDS=pass",
            "LSP_COMPLETION_ADD=pass",
            "LSP_REFERENCES=pass",
            "LSP_RENAME=pass",
            "LSP_SEMANTIC_PUB=pass",
            "LSP_SEMANTIC_CLASSES=pass",
            "LSP_SIGNATURE_HELP=pass",
            "LSP_CODE_ACTION=pass",
            "LSP_WORKSPACE_SYMBOL=pass",
            "LSP_CALL_HIERARCHY=pass",
            "LSP_DOCUMENT_HIGHLIGHT=pass",
            "LSP_CODE_LENS=pass",
            "LSP_FOLDING_RANGE=pass",
            "LSP_SELECTION_RANGE=pass",
            "LSP_DIAGNOSTICS=pass",
            "LSP_DOCUMENT_LINKS=pass",
        ]
    );
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
fn emacs_eglot_attaches_and_proves_hover_rename_references_and_semantic_tokens() {
    let Some(emacs) = emacs_binary() else {
        eprintln!("SKIP: Emacs is not installed; set ORNA_TEST_EMACS to its executable");
        return;
    };
    let availability = Command::new(&emacs)
        .args([
            "--batch",
            "--quick",
            "--eval",
            "(progn (require 'package) (package-initialize) (princ (format \"eglot=%s;semtok=%s\" (if (require 'eglot nil t) \"available\" \"unavailable\") (if (fboundp 'eglot-semantic-tokens-mode) \"available\" \"unavailable\"))))",
        ])
        .output()
        .unwrap_or_else(|error| panic!("probe Eglot through Emacs: {error}"));
    assert!(
        availability.status.success(),
        "Emacs Eglot capability probe failed (exit {:?}):\n{}",
        availability.status.code(),
        String::from_utf8_lossy(&availability.stderr),
    );
    let availability = String::from_utf8_lossy(&availability.stdout);
    if !availability.contains("eglot=available") {
        eprintln!("SKIP: Eglot is unavailable in this Emacs installation");
        return;
    }
    let semantic_tokens_available = availability.contains("semtok=available");
    if !semantic_tokens_available {
        eprintln!("SKIP: Eglot semantic-token fontification is unavailable in this version");
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("orna-lsp is under crates");
    let hover_fixture =
        root.join("crates/orna-lsp/tests/fixtures/ji3t0-lsp-v1-self-contained.orna");
    let semantic_fixture = root.join("crates/orna-lsp/tests/fixtures/editor-semantic-tokens.orna");
    let hints_fixture = root.join("crates/orna-lsp/tests/fixtures/editor-lsp-hints.orna");
    let signature_fixture = root.join("crates/orna-lsp/tests/fixtures/signature-actions-v1.orna");
    let action_fixture =
        root.join("crates/orna-lsp/tests/fixtures/missing-semicolon-code-action-v1.orna");
    let workspace_provider_fixture =
        root.join("crates/orna-lsp/tests/fixtures/workspace-hierarchy-provider-v1.orna");
    let workspace_caller_fixture =
        root.join("crates/orna-lsp/tests/fixtures/workspace-hierarchy-caller-v1.orna");
    let document_highlight_fixture =
        root.join("crates/orna-lsp/tests/fixtures/document-highlight-code-lens-v1.orna");
    let folding_selection_fixture =
        root.join("crates/orna-lsp/tests/fixtures/folding-selection-v1.orna");
    let diagnostic_fixture =
        root.join("crates/orna-lsp/tests/fixtures/incremental-malformed-v1.orna");
    let diagnostic_recovery = DIAGNOSTIC_SOURCE
        .replace("value + ;", "value + 1;")
        .trim_end()
        .to_owned();
    assert!(
        orna_syntax_v1::parse_module(&diagnostic_recovery)
            .diagnostics
            .is_empty()
    );
    let document_links_fixture = root.join("crates/orna-lsp/tests/fixtures/document-links-v1.orna");
    let document_link_target_fixture =
        root.join("crates/orna-lsp/tests/fixtures/library/math.orna");
    let document_link_directory_target_fixture =
        root.join("crates/orna-lsp/tests/fixtures/library/math/main.orna");
    assert_eq!(fs::read_to_string(&hover_fixture).unwrap(), SOURCE);
    assert_eq!(
        fs::read_to_string(&semantic_fixture).unwrap(),
        SEMANTIC_SOURCE
    );
    assert_eq!(fs::read_to_string(&hints_fixture).unwrap(), HINTS_SOURCE);
    let depth_ranges = syntax_v1_depth_contract::request_ranges(HINTS_SOURCE);
    assert_eq!(
        fs::read_to_string(&signature_fixture).unwrap(),
        SIGNATURE_SOURCE
    );
    assert_eq!(fs::read_to_string(&action_fixture).unwrap(), ACTION_SOURCE);
    let action_signature_requests =
        syntax_v1_action_signature_contract::request_data(SIGNATURE_SOURCE, ACTION_SOURCE);
    assert_eq!(
        fs::read_to_string(&workspace_provider_fixture).unwrap(),
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&workspace_caller_fixture).unwrap(),
        WORKSPACE_HIERARCHY_CALLER_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_highlight_fixture).unwrap(),
        DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE
    );
    let workspace_hierarchy_requests = syntax_v1_workspace_hierarchy_contract::request_data(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
    );
    assert_eq!(
        fs::read_to_string(&folding_selection_fixture).unwrap(),
        FOLDING_SELECTION_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&diagnostic_fixture).unwrap(),
        DIAGNOSTIC_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_links_fixture).unwrap(),
        DOCUMENT_LINKS_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_link_target_fixture).unwrap(),
        DOCUMENT_LINK_TARGET_SOURCE
    );
    assert_eq!(
        fs::read_to_string(&document_link_directory_target_fixture).unwrap(),
        DOCUMENT_LINK_DIRECTORY_TARGET_SOURCE
    );
    let folding_selection_requests =
        syntax_v1_folding_selection_contract::request_data(FOLDING_SELECTION_SOURCE);
    let script = temporary_path("el");
    let hover_semantic_result_path = temporary_path("hover-semantic.json");
    let plugin = root.join("editors/emacs/orna-eglot.el");
    let elisp = format!(
        r#";; -*- lexical-binding: t; -*-
(require 'package)
(package-initialize)
(require 'cl-lib)
(require 'json)
(require 'eglot)
(load-file {})
(setq orna-eglot-server-command (list {}))
(orna-setup-eglot)

(defun orna-test-wait-managed (buffer)
  (with-current-buffer buffer
    (let ((deadline (+ (float-time) 12.0)))
      (while (and (not (eglot-managed-p)) (< (float-time) deadline))
        (accept-process-output nil 0.05)))
    (unless (eglot-managed-p) (error "Eglot did not attach the Orna buffer"))))

(defun orna-test-face-p (needle face)
  (goto-char (point-min))
  (unless (search-forward needle nil t) (error "fixture omitted %s" needle))
  (let* ((position (- (point) (length needle)))
         (faces (get-text-property position 'face)))
    (unless (or (eq faces face) (and (listp faces) (memq face faces)))
      (error "%s lacks semantic face %s (actual face %S)" needle face faces))))
(defun orna-test-get (object key)
  (let ((keyword (intern (concat ":" key)))
        (symbol (intern key)))
    (cond
     ((hash-table-p object) (or (gethash key object) (gethash keyword object) (gethash symbol object)))
     ((and (listp object) (keywordp (car-safe object))) (plist-get object keyword))
     ((listp object) (or (cdr (assoc-string key object)) (cdr (assq symbol object)))))))
(defun orna-test-list (value)
  (cond ((stringp value) nil)
        ((vectorp value) (append value nil))
        ((listp value) value)
        (t nil)))
(defun orna-test-position (needle offset)
  (goto-char (point-min))
  (search-forward needle)
  (backward-char (- (length needle) offset))
  (list (1- (line-number-at-pos)) (current-column)))
(defun orna-test-position-key (position)
  (format "%s:%s" (car position) (cadr position)))
(defun orna-test-depth-range (name)
  (let* ((ranges (json-parse-string (getenv "ORNA_DEPTH_RANGES") :object-type 'hash-table))
         (range (gethash name ranges))
         (start (gethash "start" range))
         (end (gethash "end" range)))
    (list :start (list :line (gethash "line" start)
                       :character (gethash "character" start))
          :end (list :line (gethash "line" end)
                     :character (gethash "character" end)))))
(defun orna-test-action-signature-value (name)
  (gethash name
           (json-parse-string (getenv "ORNA_ACTION_SIGNATURE_REQUESTS")
                              :object-type 'hash-table)))
(defun orna-test-action-signature-position (name)
  (let ((position (orna-test-action-signature-value name)))
    (list :line (gethash "line" position)
          :character (gethash "character" position))))
(defun orna-test-action-signature-range (name)
  (let* ((range (orna-test-action-signature-value name))
         (start (gethash "start" range))
         (end (gethash "end" range)))
    (list :start (list :line (gethash "line" start)
                       :character (gethash "character" start))
          :end (list :line (gethash "line" end)
                     :character (gethash "character" end)))))
(defun orna-test-workspace-hierarchy-position (name)
  (let* ((requests (json-parse-string (getenv "ORNA_WORKSPACE_HIERARCHY_REQUESTS")
                                      :object-type 'hash-table))
         (position (gethash name requests)))
    (list :line (gethash "line" position)
          :character (gethash "character" position))))
(defun orna-test-folding-selection-positions ()
  (let ((positions (json-parse-string (getenv "ORNA_FOLDING_SELECTION_REQUESTS")
                                      :array-type 'list :object-type 'hash-table)))
    (mapcar (lambda (position)
              (list :line (gethash "line" position)
                    :character (gethash "character" position)))
            (gethash "selection_positions" positions))))
(defun orna-test-location-key (location)
  (let* ((range (orna-test-get location "range"))
         (start (orna-test-get range "start")))
    (format "%s:%s" (orna-test-get start "line") (orna-test-get start "character"))))

(let ((hover-buffer (find-file-noselect {}))
      (semantic-buffer nil)
      (depth-buffer nil)
      (signature-buffer nil)
      (action-buffer nil)
      (workspace-provider-buffer nil)
      (workspace-caller-buffer nil)
      (document-highlight-buffer nil)
      (folding-selection-buffer nil)
      (diagnostic-buffer nil)
      (document-links-buffer nil)
      (document-link-target-buffer nil)
      (document-link-directory-buffer nil)
      (hover-response nil)
      (semantic-response nil)
      (depth-semantic-response nil)
      (depth-semantic-range-response nil)
      (depth-hints-full nil)
      (depth-hints-call nil)
      (depth-hints-inferred nil)
      (depth-hints-annotated nil)
      (depth-hints-shadowed nil)
      (signature-nested-tuple nil)
      (signature-named-argument nil)
      (signature-nested-named-argument nil)
      (signature-shadowed-call nil)
      (code-action-quickfix nil)
      (code-action-wrong-kind nil)
      (code-action-outside-range nil)
      (action-uri nil)
      (workspace-provider-uri nil)
      (workspace-caller-uri nil)
      (workspace-server nil)
      (workspace-evidence (make-hash-table :test 'equal))
      (folding-ranges nil)
      (selection-ranges nil)
      (folding-uri nil)
      (diagnostic-invalid nil)
      (diagnostic-recovered nil)
      (diagnostic-uri nil)
      (document-links-unresolved nil)
      (document-links-resolved nil)
      (document-links-ambiguous nil)
      (document-links-uri nil)
      (document-links-target-uri nil)
      (references-response nil)
      (references-without-declaration-response nil)
      (rename-response nil)
      (attached-uri nil))
  (unwind-protect
      (progn
        (with-current-buffer hover-buffer
          (unless (eq major-mode 'orna-mode) (error "Orna major mode did not load"))
          (orna-test-wait-managed hover-buffer)
          (unless (memq #'eglot-hover-eldoc-function eldoc-documentation-functions)
            (error "Eglot did not install hover in ElDoc"))
          (goto-char (point-min))
          (search-forward "add(value, 2)")
          (backward-char 11)
          (switch-to-buffer hover-buffer)
          (let ((hover-info nil)
                (hover-done nil)
                (deadline (+ (float-time) 12.0)))
            (unless (eglot-hover-eldoc-function
                     (lambda (info &rest _ignored)
                       (setq hover-info info hover-done t)))
              (error "Eglot declined the hover request"))
            (while (and (not hover-done) (< (float-time) deadline))
              (accept-process-output nil 0.05))
            (unless hover-done (error "Eglot hover request timed out"))
            (unless (and (stringp hover-info)
                         (string-match-p "fn add(left: Int, right: Int): Int" hover-info)
                         (string-match-p "Add two integer values\\." hover-info))
              (error "Eglot hover omitted the fixture signature or documentation: %S" hover-info)))
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri"))
                 (raw-references
                  (jsonrpc-request
                   server :textDocument/references
                   (append params (list :context (list :includeDeclaration t)))))
                 (raw-references-without-declaration
                  (jsonrpc-request
                   server :textDocument/references
                   (append params (list :context (list :includeDeclaration :json-false)))))
                 (references
                  (orna-test-list raw-references))
                 (expected
                  (sort
                   (mapcar #'orna-test-position-key
                           (list (orna-test-position "pub fn add" 7)
                                 (orna-test-position "add(value, 2)" 0)))
                   #'string<))
                 (actual
                  (sort (mapcar #'orna-test-location-key references) #'string<)))
            (setq hover-response (jsonrpc-request server :textDocument/hover params))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the attached buffer: %S" params))
            (setq attached-uri uri)
            (setq references-response raw-references)
            (setq references-without-declaration-response
                  raw-references-without-declaration)
            (unless (= (length references) 2)
              (error "Eglot references returned %d locations: %S" (length references) references))
            (unless (equal actual expected)
              (error "Eglot references differ from declaration and call locations: %S" references))
            (unless (cl-every (lambda (location)
                                (equal (orna-test-get location "uri") uri))
                              references)
              (error "Eglot references returned a location outside the attached buffer: %S" references))
            (let* ((rename-params (append params (list :newName "sum")))
                   (workspace-edit
                    (jsonrpc-request server :textDocument/rename rename-params))
                   (changes (orna-test-get workspace-edit "changes"))
                   (edits (orna-test-list (orna-test-get changes uri)))
                   (edit-positions
                    (sort (mapcar #'orna-test-location-key edits) #'string<)))
              (setq rename-response workspace-edit)
              (unless (= (length edits) 2)
                (error "Eglot rename returned %d edits: %S" (length edits) workspace-edit))
              (unless (cl-every (lambda (edit) (equal (orna-test-get edit "newText") "sum")) edits)
                (error "Eglot rename returned an unexpected replacement: %S" edits))
              (unless (equal edit-positions expected)
                (error "Eglot rename omitted declaration or call edits: %S" edits)))
          (princ "EMACS_LSP_REFERENCES=pass\n")
          (princ "EMACS_LSP_RENAME=pass\n"))
          (princ "EMACS_LSP_ATTACHMENT=pass\n")
          (princ "EMACS_LSP_HOVER=pass\n"))
        (setq semantic-buffer (find-file-noselect {}))
        (when (fboundp 'eglot-semantic-tokens-mode)
         (with-current-buffer semantic-buffer
          (unless (eq major-mode 'orna-mode) (error "Orna semantic fixture did not load"))
          (orna-test-wait-managed semantic-buffer)
          (unless (bound-and-true-p eglot-semantic-tokens-mode)
            (error "Eglot semantic-token mode is not active"))
          (switch-to-buffer semantic-buffer)
          (font-lock-mode 1)
          (font-lock-ensure)
          (let ((deadline (+ (float-time) 12.0)))
            (while (and (not (get-text-property (save-excursion
                                                   (goto-char (point-min))
                                                   (search-forward "total")
                                                   (- (point) (length "total")))
                                                 'eglot--semtok-names))
                        (< (float-time) deadline))
              (font-lock-ensure)
              (accept-process-output nil 0.05)))
          (font-lock-ensure)
          (orna-test-face-p "let" 'eglot-semantic-keyword)
          (orna-test-face-p "total" 'eglot-semantic-variable)
          (orna-test-face-p "12" 'eglot-semantic-number)
          (orna-test-face-p "hello" 'eglot-semantic-string)
          (orna-test-face-p "comment" 'eglot-semantic-comment)
          (orna-test-face-p "+" 'eglot-semantic-operator)
          (princ "EMACS_LSP_SEMANTIC_CLASSES=pass\n")))
        (with-current-buffer semantic-buffer
          (orna-test-wait-managed semantic-buffer)
          (let* ((params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (setq semantic-response
                  (jsonrpc-request (eglot-current-server)
                                   :textDocument/semanticTokens/full
                                   (list :textDocument (list :uri uri))))))
        (setq depth-buffer (find-file-noselect {}))
        (with-current-buffer depth-buffer
          (orna-test-wait-managed depth-buffer)
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (setq depth-semantic-response
                  (jsonrpc-request server :textDocument/semanticTokens/full
                                   (list :textDocument (list :uri uri))))
            (setq depth-semantic-range-response
                  (jsonrpc-request server :textDocument/semanticTokens/range
                                   (list :textDocument (list :uri uri)
                                         :range (orna-test-depth-range "semantic"))))
            (setq depth-hints-full
                  (jsonrpc-request server :textDocument/inlayHint
                                   (list :textDocument (list :uri uri)
                                         :range (orna-test-depth-range "hints_full"))))
            (setq depth-hints-call
                  (jsonrpc-request server :textDocument/inlayHint
                                   (list :textDocument (list :uri uri)
                                         :range (orna-test-depth-range "hints_call"))))
            (setq depth-hints-inferred
                  (jsonrpc-request server :textDocument/inlayHint
                                   (list :textDocument (list :uri uri)
                                         :range (orna-test-depth-range "hints_inferred"))))
            (setq depth-hints-annotated
                  (jsonrpc-request server :textDocument/inlayHint
                                   (list :textDocument (list :uri uri)
                                         :range (orna-test-depth-range "hints_annotated"))))
            (setq depth-hints-shadowed
                  (jsonrpc-request server :textDocument/inlayHint
                                   (list :textDocument (list :uri uri)
                                         :range (orna-test-depth-range "hints_shadowed"))))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the depth fixture: %S" params))))
        (setq signature-buffer (find-file-noselect {}))
        (with-current-buffer signature-buffer
          (orna-test-wait-managed signature-buffer)
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (setq signature-nested-tuple
                  (jsonrpc-request
                   server :textDocument/signatureHelp
                   (list :textDocument (list :uri uri)
                         :position (orna-test-action-signature-position "signature_nested_tuple"))))
            (setq signature-named-argument
                  (jsonrpc-request
                   server :textDocument/signatureHelp
                   (list :textDocument (list :uri uri)
                         :position (orna-test-action-signature-position "signature_named_argument"))))
            (setq signature-nested-named-argument
                  (jsonrpc-request
                   server :textDocument/signatureHelp
                   (list :textDocument (list :uri uri)
                         :position (orna-test-action-signature-position "signature_nested_named_argument"))))
            (setq signature-shadowed-call
                  (jsonrpc-request
                   server :textDocument/signatureHelp
                   (list :textDocument (list :uri uri)
                         :position (orna-test-action-signature-position "signature_shadowed_call"))))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the signature fixture: %S" params))))
        (setq action-buffer (find-file-noselect {}))
        (with-current-buffer action-buffer
          (orna-test-wait-managed action-buffer)
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (setq action-uri uri)
            (setq code-action-quickfix
                  (jsonrpc-request
                   server :textDocument/codeAction
                   (list :textDocument (list :uri uri)
                         :range (orna-test-action-signature-range "code_action_full_range")
                         :context (list :diagnostics [] :only ["quickfix"]))))
            (setq code-action-wrong-kind
                  (jsonrpc-request
                   server :textDocument/codeAction
                   (list :textDocument (list :uri uri)
                         :range (orna-test-action-signature-range "code_action_full_range")
                         :context (list :diagnostics [] :only ["refactor"]))))
            (setq code-action-outside-range
                  (jsonrpc-request
                   server :textDocument/codeAction
                   (list :textDocument (list :uri uri)
                         :range (orna-test-action-signature-range "code_action_outside_range")
                         :context (list :diagnostics [] :only ["quickfix"]))))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the code-action fixture: %S" params))))
        (princ "EMACS_LSP_SIGNATURE_HELP=pass\n")
        (princ "EMACS_LSP_CODE_ACTION=pass\n")
        (setq workspace-provider-buffer
              (find-file-noselect (getenv "ORNA_WORKSPACE_PROVIDER_FIXTURE")))
        (with-current-buffer workspace-provider-buffer
          (orna-test-wait-managed workspace-provider-buffer)
          (let* ((params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the workspace provider: %S" params))
            (setq workspace-provider-uri uri
                  workspace-server (eglot-current-server))))
        (setq workspace-caller-buffer
              (find-file-noselect (getenv "ORNA_WORKSPACE_CALLER_FIXTURE")))
        (with-current-buffer workspace-caller-buffer
          (orna-test-wait-managed workspace-caller-buffer)
          (let* ((params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the workspace caller: %S" params))
            (setq workspace-caller-uri uri
                  workspace-server (eglot-current-server))))
        (puthash "workspace_provider_code_lenses"
                 (jsonrpc-request
                  workspace-server :textDocument/codeLens
                  (list :textDocument (list :uri workspace-provider-uri)))
                 workspace-evidence)
        (puthash "workspace_caller_code_lenses"
                 (jsonrpc-request
                  workspace-server :textDocument/codeLens
                  (list :textDocument (list :uri workspace-caller-uri)))
                 workspace-evidence)
        (setq document-highlight-buffer
              (find-file-noselect (getenv "ORNA_DOCUMENT_HIGHLIGHT_FIXTURE")))
        (with-current-buffer document-highlight-buffer
          (orna-test-wait-managed document-highlight-buffer)
          (let* ((params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri"))
                 (position (orna-test-position "count =" 1)))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the document-highlight fixture: %S" params))
            (puthash "document_highlights"
                     (jsonrpc-request
                      (eglot-current-server) :textDocument/documentHighlight
                      (list :textDocument (list :uri uri)
                            :position (list :line (car position)
                                            :character (cadr position))))
                     workspace-evidence)))
        (puthash "workspace_mid"
                 (jsonrpc-request workspace-server :workspace/symbol (list :query "mid"))
                 workspace-evidence)
        (puthash "workspace_mid_repeat"
                 (jsonrpc-request workspace-server :workspace/symbol (list :query "mid"))
                 workspace-evidence)
        (puthash "workspace_rem"
                 (jsonrpc-request workspace-server :workspace/symbol (list :query "rem"))
                 workspace-evidence)
        (puthash "workspace_remi"
                 (jsonrpc-request workspace-server :workspace/symbol (list :query "remi"))
                 workspace-evidence)
        (let* ((root-item-response
                (jsonrpc-request
                 workspace-server :textDocument/prepareCallHierarchy
                 (list :textDocument (list :uri workspace-provider-uri)
                       :position (orna-test-workspace-hierarchy-position "root_definition"))))
               (root-item (car (orna-test-list root-item-response))))
          (puthash "call_root_item" root-item-response workspace-evidence)
          (puthash "call_root_outgoing"
                   (jsonrpc-request workspace-server :callHierarchy/outgoingCalls
                                    (list :item root-item))
                   workspace-evidence)
          (puthash "call_root_incoming"
                   (jsonrpc-request workspace-server :callHierarchy/incomingCalls
                                    (list :item root-item))
                   workspace-evidence))
        (let* ((seed-item-response
                (jsonrpc-request
                 workspace-server :textDocument/prepareCallHierarchy
                 (list :textDocument (list :uri workspace-provider-uri)
                       :position (orna-test-workspace-hierarchy-position "seed_definition"))))
               (seed-item (car (orna-test-list seed-item-response))))
          (puthash "call_seed_item" seed-item-response workspace-evidence)
          (puthash "call_seed_incoming"
                   (jsonrpc-request workspace-server :callHierarchy/incomingCalls
                                    (list :item seed-item))
                   workspace-evidence))
        (let* ((shadowed-item-response
                (jsonrpc-request
                 workspace-server :textDocument/prepareCallHierarchy
                 (list :textDocument (list :uri workspace-provider-uri)
                       :position (orna-test-workspace-hierarchy-position "shadowed_definition"))))
               (shadowed-item (car (orna-test-list shadowed-item-response))))
          (puthash "call_shadowed_outgoing"
                   (jsonrpc-request workspace-server :callHierarchy/outgoingCalls
                                    (list :item shadowed-item))
                   workspace-evidence))
        (let* ((unresolved-item-response
                (jsonrpc-request
                 workspace-server :textDocument/prepareCallHierarchy
                 (list :textDocument (list :uri workspace-provider-uri)
                       :position (orna-test-workspace-hierarchy-position "unresolved_definition"))))
               (unresolved-item (car (orna-test-list unresolved-item-response))))
          (puthash "call_unresolved_outgoing"
                   (jsonrpc-request workspace-server :callHierarchy/outgoingCalls
                                    (list :item unresolved-item))
                   workspace-evidence))
        (puthash "call_root_reference"
                 (jsonrpc-request
                  workspace-server :textDocument/prepareCallHierarchy
                  (list :textDocument (list :uri workspace-caller-uri)
                        :position (orna-test-workspace-hierarchy-position "root_reference")))
                 workspace-evidence)
        (puthash "call_ambiguous_reference"
                 (jsonrpc-request
                  workspace-server :textDocument/prepareCallHierarchy
                  (list :textDocument (list :uri workspace-caller-uri)
                        :position (orna-test-workspace-hierarchy-position "ambiguous_reference")))
                 workspace-evidence)
        (setq folding-selection-buffer
              (find-file-noselect (getenv "ORNA_FOLDING_SELECTION_FIXTURE")))
        (with-current-buffer folding-selection-buffer
          (orna-test-wait-managed folding-selection-buffer)
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for folding/selection fixture: %S" params))
            (setq folding-uri uri
                  folding-ranges
                  (jsonrpc-request server :textDocument/foldingRange
                                   (list :textDocument (list :uri uri)))
                  selection-ranges
                  (jsonrpc-request server :textDocument/selectionRange
                                   (list :textDocument (list :uri uri)
                                         :positions (orna-test-folding-selection-positions))))))
        (setq diagnostic-buffer
              (find-file-noselect (getenv "ORNA_DIAGNOSTIC_FIXTURE")))
        (with-current-buffer diagnostic-buffer
          (orna-test-wait-managed diagnostic-buffer)
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the diagnostic fixture: %S" params))
            (setq diagnostic-uri uri
                  diagnostic-invalid
                  (orna-test-get
                   (jsonrpc-request server :textDocument/diagnostic
                                    (list :textDocument (list :uri uri)))
                   "items"))
            (let ((inhibit-read-only t))
              (erase-buffer)
              (insert (getenv "ORNA_DIAGNOSTIC_RECOVERY_SOURCE")))
            (setq diagnostic-recovered
                  (orna-test-get
                   (jsonrpc-request server :textDocument/diagnostic
                                    (list :textDocument (list :uri uri)))
                   "items")))
        (setq document-links-buffer
              (find-file-noselect (getenv "ORNA_DOCUMENT_LINKS_FIXTURE")))
        (with-current-buffer document-links-buffer
          (orna-test-wait-managed document-links-buffer)
          (let* ((server (eglot-current-server))
                 (params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the document-links fixture: %S" params))
            (setq document-links-uri uri
                  workspace-server server
                  document-links-unresolved
                  (jsonrpc-request server :textDocument/documentLink
                                   (list :textDocument (list :uri uri))))))
        (setq document-link-target-buffer
              (find-file-noselect (getenv "ORNA_DOCUMENT_LINK_TARGET_FIXTURE")))
        (with-current-buffer document-link-target-buffer
          (orna-test-wait-managed document-link-target-buffer)
          (let* ((params (eglot--TextDocumentPositionParams))
                 (uri (orna-test-get (orna-test-get params "textDocument") "uri")))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the document-link target: %S" params))
            (setq document-links-target-uri uri)))
        (setq document-links-resolved
              (jsonrpc-request workspace-server :textDocument/documentLink
                               (list :textDocument (list :uri document-links-uri))))
        (setq document-link-directory-buffer
              (find-file-noselect (getenv "ORNA_DOCUMENT_LINK_DIRECTORY_FIXTURE")))
        (with-current-buffer document-link-directory-buffer
          (orna-test-wait-managed document-link-directory-buffer))
        (setq document-links-ambiguous
              (jsonrpc-request workspace-server :textDocument/documentLink
                               (list :textDocument (list :uri document-links-uri))))
        (princ "EMACS_LSP_WORKSPACE_SYMBOL=pass\n")
        (princ "EMACS_LSP_CALL_HIERARCHY=pass\n")
        (princ "EMACS_LSP_DOCUMENT_HIGHLIGHT=pass\n")
        (princ "EMACS_LSP_CODE_LENS=pass\n")
        (princ "EMACS_LSP_FOLDING_RANGE=pass\n")
        (princ "EMACS_LSP_SELECTION_RANGE=pass\n")
        (princ "EMACS_LSP_DIAGNOSTICS=pass\n")
        (princ "EMACS_LSP_DOCUMENT_LINKS=pass\n")
        (let ((evidence (make-hash-table :test 'equal)))
          (puthash "uri" attached-uri evidence)
          (puthash "hover" hover-response evidence)
          (puthash "semantic" semantic-response evidence)
          (puthash "depth_semantic" depth-semantic-response evidence)
          (puthash "depth_semantic_range" depth-semantic-range-response evidence)
          (puthash "depth_hints_full" depth-hints-full evidence)
          (puthash "depth_hints_call" depth-hints-call evidence)
          (puthash "depth_hints_inferred" depth-hints-inferred evidence)
          (puthash "depth_hints_annotated" depth-hints-annotated evidence)
          (puthash "depth_hints_shadowed" depth-hints-shadowed evidence)
          (puthash "signature_nested_tuple" signature-nested-tuple evidence)
          (puthash "signature_named_argument" signature-named-argument evidence)
          (puthash "signature_nested_named_argument" signature-nested-named-argument evidence)
          (puthash "signature_shadowed_call" signature-shadowed-call evidence)
          (puthash "code_action_quickfix" code-action-quickfix evidence)
          (puthash "code_action_wrong_kind" code-action-wrong-kind evidence)
          (puthash "code_action_outside_range" code-action-outside-range evidence)
          (puthash "action_uri" action-uri evidence)
          (puthash "workspace_provider_uri" workspace-provider-uri evidence)
          (puthash "workspace_caller_uri" workspace-caller-uri evidence)
          (puthash "folding_uri" folding-uri evidence)
          (puthash "folding_ranges" folding-ranges evidence)
          (puthash "selection_ranges" selection-ranges evidence)
          (puthash "diagnostic_invalid" diagnostic-invalid evidence)
          (puthash "diagnostic_recovered" diagnostic-recovered evidence)
          (puthash "diagnostic_uri" diagnostic-uri evidence)
          (puthash "document_links_unresolved" document-links-unresolved evidence)
          (puthash "document_links_resolved" document-links-resolved evidence)
          (puthash "document_links_ambiguous" document-links-ambiguous evidence)
          (puthash "document_links_uri" document-links-uri evidence)
          (puthash "document_links_target_uri" document-links-target-uri evidence)
          (maphash (lambda (key value) (puthash key value evidence)) workspace-evidence)
          (puthash "references" references-response evidence)
          (puthash "references_without_declaration"
                   references-without-declaration-response evidence)
          (puthash "rename" rename-response evidence)
          (with-temp-file (getenv "ORNA_HOVER_SEMANTIC_RESULT")
            (insert (json-encode evidence)))))
    (when (buffer-live-p hover-buffer) (kill-buffer hover-buffer))
    (when (buffer-live-p semantic-buffer) (kill-buffer semantic-buffer))
    (when (buffer-live-p depth-buffer) (kill-buffer depth-buffer))
    (when (buffer-live-p signature-buffer) (kill-buffer signature-buffer))
    (when (buffer-live-p action-buffer) (kill-buffer action-buffer))
    (when (buffer-live-p workspace-provider-buffer) (kill-buffer workspace-provider-buffer))
    (when (buffer-live-p workspace-caller-buffer) (kill-buffer workspace-caller-buffer))
    (when (buffer-live-p folding-selection-buffer) (kill-buffer folding-selection-buffer))
    (when (buffer-live-p diagnostic-buffer) (kill-buffer diagnostic-buffer))
    (when (buffer-live-p document-links-buffer) (kill-buffer document-links-buffer))
    (when (buffer-live-p document-link-target-buffer) (kill-buffer document-link-target-buffer))
    (when (buffer-live-p document-highlight-buffer) (kill-buffer document-highlight-buffer))
    (when (buffer-live-p document-link-directory-buffer) (kill-buffer document-link-directory-buffer))))
"#,
        elisp_string(&plugin.display().to_string()),
        elisp_string(env!("CARGO_BIN_EXE_orna-lsp")),
        elisp_string(&hover_fixture.display().to_string()),
        elisp_string(&semantic_fixture.display().to_string()),
        elisp_string(&hints_fixture.display().to_string()),
        elisp_string(&signature_fixture.display().to_string()),
        elisp_string(&action_fixture.display().to_string()),
    );
    fs::write(&script, elisp).expect("write Emacs Eglot hover/token probe");
    let output = Command::new(&emacs)
        .args(["--batch", "--quick", "--script"])
        .arg(&script)
        .current_dir(root)
        .env("ORNA_HOVER_SEMANTIC_RESULT", &hover_semantic_result_path)
        .env(
            "ORNA_DEPTH_RANGES",
            serde_json::to_string(&depth_ranges).unwrap(),
        )
        .env(
            "ORNA_ACTION_SIGNATURE_REQUESTS",
            serde_json::to_string(&action_signature_requests).unwrap(),
        )
        .env(
            "ORNA_WORKSPACE_PROVIDER_FIXTURE",
            &workspace_provider_fixture,
        )
        .env("ORNA_WORKSPACE_CALLER_FIXTURE", &workspace_caller_fixture)
        .env(
            "ORNA_DOCUMENT_HIGHLIGHT_FIXTURE",
            &document_highlight_fixture,
        )
        .env("ORNA_FOLDING_SELECTION_FIXTURE", &folding_selection_fixture)
        .env("ORNA_DIAGNOSTIC_FIXTURE", &diagnostic_fixture)
        .env("ORNA_DIAGNOSTIC_RECOVERY_SOURCE", &diagnostic_recovery)
        .env("ORNA_DOCUMENT_LINKS_FIXTURE", &document_links_fixture)
        .env(
            "ORNA_DOCUMENT_LINK_TARGET_FIXTURE",
            &document_link_target_fixture,
        )
        .env(
            "ORNA_DOCUMENT_LINK_DIRECTORY_FIXTURE",
            &document_link_directory_target_fixture,
        )
        .env(
            "ORNA_WORKSPACE_HIERARCHY_REQUESTS",
            serde_json::to_string(&workspace_hierarchy_requests).unwrap(),
        )
        .env(
            "ORNA_FOLDING_SELECTION_REQUESTS",
            serde_json::to_string(&folding_selection_requests).unwrap(),
        )
        .output()
        .unwrap_or_else(|error| panic!("start Emacs at {}: {error}", emacs.display()));
    let _ = fs::remove_file(script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Emacs Eglot hover/token proof failed (exit {:?}):\n{stdout}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let hover_semantic_result = fs::read_to_string(&hover_semantic_result_path)
        .expect("Emacs Eglot hover and semantic-token result");
    let _ = fs::remove_file(&hover_semantic_result_path);
    let hover_semantic_result: Value = serde_json::from_str(&hover_semantic_result)
        .expect("decode Emacs Eglot hover and semantic-token result");
    hover_semantic_contract::assert_hover_contract(&hover_semantic_result["hover"], "Emacs Eglot");
    hover_semantic_contract::assert_semantic_token_contract(
        SEMANTIC_SOURCE,
        &hover_semantic_result["semantic"],
        "Emacs Eglot",
    );
    syntax_v1_action_signature_contract::assert_signature_help_contract(
        &hover_semantic_result["signature_nested_tuple"],
        &hover_semantic_result["signature_named_argument"],
        &hover_semantic_result["signature_nested_named_argument"],
        &hover_semantic_result["signature_shadowed_call"],
        "Emacs Eglot",
    );
    syntax_v1_action_signature_contract::assert_code_action_contract(
        ACTION_SOURCE,
        hover_semantic_result["action_uri"]
            .as_str()
            .expect("Emacs Eglot code-action document URI"),
        &hover_semantic_result["code_action_quickfix"],
        &hover_semantic_result["code_action_wrong_kind"],
        &hover_semantic_result["code_action_outside_range"],
        "Emacs Eglot",
    );
    syntax_v1_workspace_hierarchy_contract::assert_contract(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
        hover_semantic_result["workspace_provider_uri"]
            .as_str()
            .expect("Emacs Eglot workspace provider URI"),
        hover_semantic_result["workspace_caller_uri"]
            .as_str()
            .expect("Emacs Eglot workspace caller URI"),
        &hover_semantic_result,
        "Emacs Eglot",
    );
    syntax_v1_document_highlight_code_lens_contract::assert_document_highlights_contract(
        DOCUMENT_HIGHLIGHT_CODE_LENS_SOURCE,
        &hover_semantic_result["document_highlights"],
        "Emacs Eglot",
    );
    syntax_v1_document_highlight_code_lens_contract::assert_code_lens_contract(
        WORKSPACE_HIERARCHY_PROVIDER_SOURCE,
        WORKSPACE_HIERARCHY_CALLER_SOURCE,
        hover_semantic_result["workspace_provider_uri"]
            .as_str()
            .expect("Emacs Eglot workspace provider URI"),
        hover_semantic_result["workspace_caller_uri"]
            .as_str()
            .expect("Emacs Eglot workspace caller URI"),
        &hover_semantic_result["workspace_provider_code_lenses"],
        &hover_semantic_result["workspace_caller_code_lenses"],
        "Emacs Eglot",
    );
    syntax_v1_folding_selection_contract::assert_contract(
        FOLDING_SELECTION_SOURCE,
        &hover_semantic_result["folding_ranges"],
        &hover_semantic_result["selection_ranges"],
        "Emacs Eglot",
    );
    syntax_v1_diagnostics_document_links_contract::assert_diagnostics(
        DIAGNOSTIC_SOURCE,
        &hover_semantic_result["diagnostic_invalid"],
        "Emacs Eglot",
    );
    syntax_v1_diagnostics_document_links_contract::assert_diagnostics_cleared_items(
        &hover_semantic_result["diagnostic_recovered"],
        "Emacs Eglot",
    );
    syntax_v1_diagnostics_document_links_contract::assert_no_document_links(
        &hover_semantic_result["document_links_unresolved"],
        "Emacs Eglot without open targets",
    );
    syntax_v1_diagnostics_document_links_contract::assert_document_link(
        DOCUMENT_LINKS_SOURCE,
        &hover_semantic_result["document_links_resolved"],
        hover_semantic_result["document_links_target_uri"]
            .as_str()
            .expect("Emacs document-link target URI"),
        "Emacs Eglot",
    );
    syntax_v1_diagnostics_document_links_contract::assert_no_document_links(
        &hover_semantic_result["document_links_ambiguous"],
        "Emacs Eglot with ambiguous targets",
    );
    syntax_v1_depth_contract::assert_semantic_depth_contract(
        HINTS_SOURCE,
        &hover_semantic_result["depth_semantic"],
        &hover_semantic_result["depth_semantic_range"],
        "Emacs Eglot",
    );
    syntax_v1_depth_contract::assert_inlay_hint_depth_contract(
        HINTS_SOURCE,
        &hover_semantic_result["depth_hints_full"],
        &hover_semantic_result["depth_hints_call"],
        &hover_semantic_result["depth_hints_inferred"],
        &hover_semantic_result["depth_hints_annotated"],
        &hover_semantic_result["depth_hints_shadowed"],
        "Emacs Eglot",
    );
    let attached_uri = hover_semantic_result["uri"]
        .as_str()
        .expect("Emacs Eglot attached buffer URI");
    hover_semantic_contract::assert_references_contract(
        &[(attached_uri, SOURCE)],
        &hover_semantic_result["references"],
        true,
        "Emacs Eglot",
    );
    hover_semantic_contract::assert_references_contract(
        &[(attached_uri, SOURCE)],
        &hover_semantic_result["references_without_declaration"],
        false,
        "Emacs Eglot without declaration",
    );
    hover_semantic_contract::assert_rename_contract(
        &[(attached_uri, SOURCE)],
        &hover_semantic_result["rename"],
        "sum",
        "Emacs Eglot",
    );
    for evidence in [
        "EMACS_LSP_ATTACHMENT=pass",
        "EMACS_LSP_HOVER=pass",
        "EMACS_LSP_REFERENCES=pass",
        "EMACS_LSP_RENAME=pass",
        "EMACS_LSP_SIGNATURE_HELP=pass",
        "EMACS_LSP_CODE_ACTION=pass",
        "EMACS_LSP_WORKSPACE_SYMBOL=pass",
        "EMACS_LSP_CALL_HIERARCHY=pass",
        "EMACS_LSP_DOCUMENT_HIGHLIGHT=pass",
        "EMACS_LSP_CODE_LENS=pass",
        "EMACS_LSP_FOLDING_RANGE=pass",
        "EMACS_LSP_SELECTION_RANGE=pass",
        "EMACS_LSP_DIAGNOSTICS=pass",
        "EMACS_LSP_DOCUMENT_LINKS=pass",
    ] {
        assert!(
            stdout.contains(evidence),
            "Emacs omitted {evidence}: {stdout}"
        );
    }
    if semantic_tokens_available {
        assert!(
            stdout.contains("EMACS_LSP_SEMANTIC_CLASSES=pass"),
            "Emacs omitted semantic-token proof: {stdout}"
        );
    }
    println!("Emacs Eglot hover/rename/references evidence:\n{stdout}");
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
    completion_contract::expected_keywords()
}

fn neovim_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ORNA_TEST_NEOVIM") {
        return Some(PathBuf::from(path));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("nvim"))
        .find(|candidate| candidate.is_file())
}

fn emacs_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ORNA_TEST_EMACS") {
        return Some(PathBuf::from(path));
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("emacs"))
        .find(|candidate| candidate.is_file())
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
        grammar["repository"]["keyword"]["name"],
        "keyword.control.orna"
    );
    let pattern = grammar["repository"]["keyword"]["match"].as_str().unwrap();
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
        .find("(regexp-opt '")
        .expect("Emacs keyword regexp definition");
    let end = source[start..]
        .find(") 'words)")
        .expect("Emacs keyword regexp terminator")
        + start;
    let words = quoted_words(&source[start..end]);
    assert!(
        source.contains("(define-derived-mode orna-mode"),
        "Emacs mode is not defined"
    );
    assert!(source.contains("font-lock-keyword-face"));
    assert!(
        !source.contains("(setq-local case-fold-search t)"),
        "Orna v1 font-lock must not fold keyword case"
    );
    assert!(
        source.contains("(orna-font-lock-keywords nil nil)"),
        "Orna v1 keywords are case-sensitive"
    );
    assert!(source.contains("auto-mode-alist") && source.contains(".orna"));
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
        .position(|line| line.contains("scope: 'keyword.control.orna'"))
        .expect("Sublime keyword scope");
    let pattern_line = lines[..keyword_line]
        .iter()
        .rev()
        .find(|line| line.trim_start().starts_with("- match:"))
        .expect("Sublime keyword matcher");
    let pattern = pattern_line
        .split_once('\'')
        .expect("Sublime single-quoted generated matcher")
        .1
        .rsplit_once('\'')
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
    let end = [")\\b", ")(?!"]
        .iter()
        .filter_map(|delimiter| remainder.find(delimiter))
        .min()
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
    let emacs = std::env::var_os("ORNA_TEST_EMACS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("emacs"));
    let eglot = Command::new(&emacs)
        .args([
            "--batch",
            "--quick",
            "--eval",
            "(require 'package)(package-initialize)(princ (if (require 'eglot nil t) \"available\" \"unavailable\"))",
        ])
        .output();
    match eglot {
        Ok(result)
            if result.status.success()
                && String::from_utf8_lossy(&result.stdout).contains("available") => {}
        Ok(_) => {
            eprintln!(
                "SKIP: Emacs Eglot is unavailable; install or enable Eglot for the host probe"
            );
            let _ = fs::remove_file(fixture);
            return;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIP: Emacs is not installed; set ORNA_TEST_EMACS to its executable");
            let _ = fs::remove_file(fixture);
            return;
        }
        Err(error) => panic!("probe Emacs at {}: {error}", emacs.display()),
    }
    let expression = format!(
        "(progn (require 'package) (package-initialize) (load-file {}) (with-temp-buffer (insert-file-contents {}) (orna-mode) (font-lock-ensure) (goto-char (point-min)) (search-forward \"pub\") (unless (eq (get-text-property (- (point) 3) 'face) 'font-lock-keyword-face) (error \"pub is not highlighted as a keyword\"))))",
        elisp_string(&plugin.display().to_string()),
        elisp_string(&fixture.display().to_string()),
    );
    let result = match Command::new(&emacs)
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

static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

fn temporary_path(extension: &str) -> PathBuf {
    let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "orna-ji3t0-{}-{id}.{extension}",
        std::process::id()
    ))
}

fn temporary_fixture(source: &str) -> PathBuf {
    let path = temporary_path("orna");
    fs::write(&path, source).unwrap();
    path
}

fn elisp_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
