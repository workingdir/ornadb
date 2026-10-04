//! End-to-end protocol and editor artifact proofs for the Orna 1.0 language.

use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use orna_syntax_v1::Keyword;
use serde_json::{Value, json};

const SOURCE: &str = include_str!("fixtures/ji3t0-lsp-v1-self-contained.orna");
const SEMANTIC_SOURCE: &str = include_str!("fixtures/editor-semantic-tokens.orna");
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
    let reference_locations = references.as_array().unwrap();
    assert_eq!(reference_locations.len(), 2, "{references}");
    let declaration_position = position_of(SOURCE, "pub fn add", "pub fn ".len());
    let call_reference_position = position_of(SOURCE, "add(value, 2)", 0);
    for expected in [&declaration_position, &call_reference_position] {
        assert!(
            reference_locations.iter().any(|location| {
                location["uri"] == uri && location["range"]["start"] == *expected
            }),
            "references omitted {expected}: {references}"
        );
    }
    let references_without_declaration = client.request(
        "textDocument/references",
        json!({
            "textDocument":{"uri":uri},
            "position":call_position,
            "context":{"includeDeclaration":false}
        }),
    );
    assert_eq!(references_without_declaration.as_array().unwrap().len(), 1);
    assert_eq!(
        references_without_declaration[0]["range"]["start"],
        call_reference_position
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
    let fixture_source = format!("{SOURCE}\n{SEMANTIC_SOURCE}\nCREATE SCHEMA old_syntax;\n");
    let semantic_source_line = SOURCE.lines().count() + 1;
    let fixture = temporary_fixture(&fixture_source);
    let script = temporary_path("lua");
    let result_path = temporary_path("result");
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
local legacy_group = syntax_group(7, 1)
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
local hover_contents = hover.result.contents
local hover_text = type(hover_contents) == "string" and hover_contents or hover_contents.value
assert(string.find(hover_text, "fn add(left: Int, right: Int): Int", 1, true), "unexpected hover: " .. hover_text)
assert(string.find(hover_text, "Add two integer values.", 1, true), "hover lost fixture documentation: " .. hover_text)

local completion, completion_error = client:request_sync("textDocument/completion", {
  textDocument = { uri = uri },
  position = vim.fn.json_decode(vim.env.ORNA_COMPLETION_POSITION),
}, 5000, bufnr)
assert(completion ~= nil and completion.err == nil, "Neovim completion request failed: " .. vim.inspect(completion_error or completion))
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
assert(add_completion.detail == "fn add(left: Int, right: Int): Int", "unexpected add completion detail: " .. vim.inspect(add_completion))
assert(add_completion.documentation == "Add two integer values.", "add completion lost fixture documentation: " .. vim.inspect(add_completion))
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

local semantic, semantic_error = client:request_sync("textDocument/semanticTokens/full", {
  textDocument = { uri = uri },
}, 5000, bufnr)
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
  if line == 1 and character == 0 and semantic.result.data[index + 3] == keyword_type then
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
local source_lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
local semantic_source_line = tonumber(vim.env.ORNA_SEMANTIC_SOURCE_LINE)
line, character = 0, 0
for index = 1, #semantic.result.data, 5 do
  local delta_line, delta_start = semantic.result.data[index], semantic.result.data[index + 1]
  if delta_line == 0 then character = character + delta_start else line, character = line + delta_line, delta_start end
  local token_length = semantic.result.data[index + 2]
  local token_type = token_types[semantic.result.data[index + 3] + 1]
  local token_text = string.sub(source_lines[line + 1], character + 1, character + token_length)
  if line >= semantic_source_line then
    if token_type == "keyword" and token_text == "let" then expected_classes.keyword = true end
    if token_type == "variable" and token_text == "total" then expected_classes.variable = true end
    if token_type == "number" and token_text == "12" then expected_classes.number = true end
    if token_type == "string" then expected_classes.string = true end
    if token_type == "comment" and string.find(token_text, "comment", 1, true) then expected_classes.comment = true end
    if token_type == "operator" and token_text == "+" then expected_classes.operator = true end
  end
end
for token_type, found in pairs(expected_classes) do
  assert(found, "Neovim LSP semantic tokens omitted the syntax-v1 " .. token_type .. " class")
end

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
        .env(
            "ORNA_SEMANTIC_SOURCE_LINE",
            semantic_source_line.to_string(),
        )
        .env("ORNA_PROBE_SCRIPT", &script)
        .env("ORNA_EDITOR_RESULT", &result_path)
        .output()
        .unwrap_or_else(|error| panic!("start Neovim at {}: {error}", neovim.display()));
    let editor_result = fs::read_to_string(&result_path);
    let _ = fs::remove_file(fixture);
    let _ = fs::remove_file(script);
    let _ = fs::remove_file(result_path);
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
    assert_eq!(fs::read_to_string(&hover_fixture).unwrap(), SOURCE);
    assert_eq!(
        fs::read_to_string(&semantic_fixture).unwrap(),
        SEMANTIC_SOURCE
    );
    let script = temporary_path("el");
    let plugin = root.join("editors/emacs/orna-eglot.el");
    let elisp = format!(
        r#";; -*- lexical-binding: t; -*-
(require 'package)
(package-initialize)
(require 'cl-lib)
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
(defun orna-test-location-key (location)
  (let* ((range (orna-test-get location "range"))
         (start (orna-test-get range "start")))
    (format "%s:%s" (orna-test-get start "line") (orna-test-get start "character"))))

(let ((hover-buffer (find-file-noselect {}))
      (semantic-buffer (find-file-noselect {})))
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
                 (references
                  (orna-test-list
                   (jsonrpc-request
                    server :textDocument/references
                    (append params (list :context (list :includeDeclaration t))))))
                 (expected
                  (sort
                   (mapcar #'orna-test-position-key
                           (list (orna-test-position "pub fn add" 7)
                                 (orna-test-position "add(value, 2)" 0)))
                   #'string<))
                 (actual
                  (sort (mapcar #'orna-test-location-key references) #'string<)))
            (unless (stringp uri)
              (error "Eglot did not provide a URI for the attached buffer: %S" params))
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
    (when (buffer-live-p hover-buffer) (kill-buffer hover-buffer))
    (when (buffer-live-p semantic-buffer) (kill-buffer semantic-buffer))))
"#,
        elisp_string(&plugin.display().to_string()),
        elisp_string(env!("CARGO_BIN_EXE_orna-lsp")),
        elisp_string(&hover_fixture.display().to_string()),
        elisp_string(&semantic_fixture.display().to_string()),
    );
    fs::write(&script, elisp).expect("write Emacs Eglot hover/token probe");
    let output = Command::new(&emacs)
        .args(["--batch", "--quick", "--script"])
        .arg(&script)
        .current_dir(root)
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
    for evidence in [
        "EMACS_LSP_ATTACHMENT=pass",
        "EMACS_LSP_HOVER=pass",
        "EMACS_LSP_REFERENCES=pass",
        "EMACS_LSP_RENAME=pass",
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
    Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling().to_owned())
        .collect()
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
