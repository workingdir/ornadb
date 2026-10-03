//! Editor artifact metadata generated from the frozen Orna 1.0 lexer.
//!
//! The generated grammars intentionally consume only this crate's keyword and
//! delimiter vocabulary.  Type names remain identifiers because the language
//! permits user-defined types and does not have a reserved scalar-type list.

use crate::{Keyword, lexer};

/// Semantic token names shared by the LSP and generated legend.
pub const EDITOR_TOKEN_TYPES: [&str; 8] = [
    "keyword",
    "function",
    "type",
    "enum",
    "variable",
    "string",
    "number",
    "operator",
];

/// Operators accepted by the syntax-v1 lexer, in longest-token-first order.
pub const EDITOR_OPERATOR_SPELLINGS: &[&str] = lexer::OPERATOR_SPELLINGS;

/// One checked-in editor artifact rendered from syntax-v1 metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedEditorArtifact {
    /// Repository-relative output path.
    pub path: &'static str,
    /// Deterministic generated contents.
    pub contents: String,
}

/// Render the language keyword inventory from ORNA-LEX-007.
pub fn generated_editor_artifacts() -> Vec<GeneratedEditorArtifact> {
    let keywords = Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling())
        .collect::<Vec<_>>();
    let keyword_alternation = keywords
        .iter()
        .map(|keyword| regex_escape(keyword))
        .collect::<Vec<_>>()
        .join("|");
    let textmate = render_textmate(&keyword_alternation);
    let tree_sitter = render_tree_sitter(&keywords);
    let highlights = render_tree_sitter_highlights(&keywords);
    let token_types = EDITOR_TOKEN_TYPES
        .iter()
        .map(|token| format!("    {}", json_string(token)))
        .collect::<Vec<_>>()
        .join(",\n");
    let keywords_emacs = keywords
        .iter()
        .map(|keyword| json_string(keyword))
        .collect::<Vec<_>>()
        .join(" ");
    let keywords_sublime = keywords.join("|");
    let keywords_vim = keywords.join(" ");

    vec![
        artifact(
            "editors/semantic-token-legend.json",
            format!(
                "{{\n  \"language\": \"orna\",\n  \"tokenTypes\": [\n{token_types}\n  ],\n  \"tokenModifiers\": []\n}}\n"
            ),
        ),
        artifact("editors/tree-sitter-orna/grammar.js", tree_sitter),
        artifact(
            "editors/tree-sitter-orna/queries/highlights.scm",
            highlights,
        ),
        artifact(
            "editors/tree-sitter-orna/package.json",
            "{\n  \"name\": \"tree-sitter-orna-v1\",\n  \"version\": \"1.0.0\",\n  \"description\": \"Orna 1.0 syntax-v1 grammar\",\n  \"tree-sitter\": {\"scopes\": {\"source.orna\": \"orna\"}, \"file-types\": [\"orna\"], \"highlights\": [\"queries/highlights.scm\"]},\n  \"scripts\": {\"test\": \"tree-sitter test\"},\n  \"devDependencies\": {\"tree-sitter-cli\": \"^0.26.5\"}\n}\n",
        ),
        artifact(
            "editors/tree-sitter-orna/tree-sitter.json",
            "{\n  \"grammars\": [{\"name\": \"orna\", \"camelcase\": \"Orna\", \"scope\": \"source.orna\", \"path\": \".\", \"file-types\": [\"orna\"], \"highlights\": \"queries/highlights.scm\"}],\n  \"metadata\": {\"version\": \"1.0.0\", \"license\": \"MIT\", \"description\": \"Orna 1.0 syntax-v1 grammar\", \"authors\": [{\"name\": \"OrnaDB\"}], \"links\": {\"repository\": \"https://github.com/workingdir/ornadb\"}}\n}\n",
        ),
        artifact(
            "editors/emacs/orna-eglot.el",
            format!(
                ";;; Orna 1.0 syntax-v1 support -*- lexical-binding: t; -*-\n;; Generated from orna-syntax-v1.\n\n(defvar orna-v1-keywords '({keywords_emacs}))\n(defconst orna-v1-font-lock-keywords\n  `((,(regexp-opt orna-v1-keywords 'words) . font-lock-keyword-face)\n    (\"//.*$\" . font-lock-comment-face)\n    (\"/\\\\*\\\\(?:.\\\\|\\\\n\\\\)*?\\\\*/\" . font-lock-comment-face)\n    (\"\\\\\\\"\\\\(?:\\\\\\\\.\\\\|[^\\\\\\\"\\\\\\\\]\\\\)*\\\\\\\"\" . font-lock-string-face)\n    (\"[0-9]+\\\\(?:\\\\.[0-9]+\\\\)?\" . font-lock-constant-face)))\n(define-derived-mode orna-v1-mode prog-mode \"Orna\"\n  \"Major mode for Orna 1.0 source.\"\n  (setq-local comment-start \"// \")\n  (setq-local comment-end \"\")\n  (setq-local font-lock-defaults '(orna-v1-font-lock-keywords)))\n(add-to-list 'auto-mode-alist '(\"\\\\.orna\" . orna-v1-mode))\n(provide 'orna-eglot)\n;;; orna-eglot.el ends here\n"
            ),
        ),
        artifact(
            "editors/sublime/Orna.sublime-syntax",
            format!(
                "%YAML 1.2\n---\nname: Orna 1.0\nfile_extensions: [orna]\nscope: source.orna\ncontexts:\n  main:\n    - match: '//.*$'\n      scope: comment.line.orna\n    - match: '/\\*'\n      push: block-comment\n    - match: '\"(?:\\\\.|[^\"\\\\])*\"'\n      scope: string.quoted.double.orna\n    - match: '\\b(?:{keywords_sublime})\\b'\n      scope: keyword.control.orna\n    - match: '\\b[0-9]+(?:\\.[0-9]+)?\\b'\n      scope: constant.numeric.orna\n    - match: '[[:alpha:]_][[:alnum:]_]*'\n      scope: variable.other.orna\n    - match: '[+*/%^=!<>|?&.-]+'\n      scope: keyword.operator.orna\n  block-comment:\n    - meta_scope: comment.block.orna\n    - match: '\\*/'\n      pop: true\n    - match: '/\\*'\n      push: block-comment\n"
            ),
        ),
        artifact(
            "editors/textmate/orna.tmLanguage.json",
            textmate.clone(),
        ),
        artifact(
            "editors/vscode/syntaxes/orna.tmLanguage.json",
            textmate,
        ),
        artifact(
            "editors/vscode/language-configuration.json",
            "{\n  \"comments\": {\"lineComment\": \"//\", \"blockComment\": [\"/*\", \"*/\"]},\n  \"brackets\": [[\"(\", \")\"], [\"[\", \"]\"], [\"{\", \"}\"]],\n  \"autoClosingPairs\": [{\"open\": \"(\", \"close\": \")\"}, {\"open\": \"[\", \"close\": \"]\"}, {\"open\": \"{\", \"close\": \"}\"}, {\"open\": \"\\\"\", \"close\": \"\\\"\", \"notIn\": [\"string\", \"comment\"]}],\n  \"surroundingPairs\": [[\"(\", \")\"], [\"[\", \"]\"], [\"{\", \"}\"], [\"\\\"\", \"\\\"\"]]\n}\n",
        ),
        artifact(
            "editors/vscode/package.json",
            "{\n  \"name\": \"orna-language-v1\",\n  \"displayName\": \"Orna 1.0 Language\",\n  \"description\": \"Generated syntax-v1 highlighting for Orna source files\",\n  \"version\": \"1.0.0\",\n  \"engines\": {\"vscode\": \"^1.80.0\"},\n  \"categories\": [\"Programming Languages\"],\n  \"contributes\": {\"languages\": [{\"id\": \"orna\", \"aliases\": [\"Orna\"], \"extensions\": [\".orna\"], \"configuration\": \"./language-configuration.json\"}], \"grammars\": [{\"language\": \"orna\", \"scopeName\": \"source.orna\", \"path\": \"./syntaxes/orna.tmLanguage.json\"}]}\n}\n",
        ),
        artifact(
            "editors/vim/ftdetect/orna.vim",
            "au BufRead,BufNewFile *.orna setfiletype orna\n",
        ),
        artifact(
            "editors/vim/syntax/orna.vim",
            format!(
                r#"" Generated from orna-syntax-v1.
if exists("b:current_syntax") | finish | endif
syntax keyword ornaKeyword {keywords_vim}
syntax match ornaComment +//.*$+
syntax region ornaBlockComment start=+/\*+ end=+\*/+ fold contains=ornaBlockComment
syntax region ornaString start=+"+ skip=+\\.+ end=+"+
syntax match ornaNumber +\<\d\+\(\.\d\+\)\=\>+
syntax match ornaIdentifier +\<[[:alpha:]_][[:alnum:]_]*\>+
highlight default link ornaKeyword Keyword
highlight default link ornaComment Comment
highlight default link ornaBlockComment Comment
highlight default link ornaString String
highlight default link ornaNumber Number
highlight default link ornaIdentifier Identifier
let b:current_syntax = "orna"
"#
            ),
        ),
    ]
}

fn render_textmate(keyword_pattern: &str) -> String {
    let keyword_pattern = json_string(&format!("\\b(?:{keyword_pattern})\\b"));
    let line_comment = json_string(&format!("{}[^\\n\\r]*", regex_escape(lexer::LINE_COMMENT_START)));
    let block_start = json_string(lexer::BLOCK_COMMENT_START);
    let block_end = json_string(lexer::BLOCK_COMMENT_END);
    let string_pattern = json_string(r#""(?:\\.|[^"\\])*""#);
    let template = r##"{
  "$schema": "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
  "name": "Orna 1.0",
  "scopeName": "source.orna",
  "fileTypes": ["orna"],
  "patterns": [{"include": "#comments"}, {"include": "#strings"}, {"include": "#keywords"}, {"include": "#numbers"}, {"include": "#identifiers"}],
  "repository": {
    "comments": {"patterns": [{"begin": @@BLOCK_START@@, "end": @@BLOCK_END@@, "name": "comment.block.orna"}, {"match": @@LINE_COMMENT@@, "name": "comment.line.double-slash.orna"}]},
    "strings": {"name": "string.quoted.double.orna", "match": @@STRING_PATTERN@@},
    "keywords": {"match": @@KEYWORD_PATTERN@@, "name": "keyword.control.orna"},
    "numbers": {"match": "\\b[0-9]+(?:\\.[0-9]+)?\\b", "name": "constant.numeric.orna"},
    "identifiers": {"match": "[\\p{L}_][\\p{L}\\p{N}_]*", "name": "variable.other.orna"}
  }
}
"##;
    template
        .replace("@@BLOCK_START@@", &block_start)
        .replace("@@BLOCK_END@@", &block_end)
        .replace("@@LINE_COMMENT@@", &line_comment)
        .replace("@@STRING_PATTERN@@", &string_pattern)
        .replace("@@KEYWORD_PATTERN@@", &keyword_pattern)
}

fn render_tree_sitter(keywords: &[&str]) -> String {
    let keyword_rows = keywords
        .iter()
        .map(|keyword| format!("    {},", js_string(keyword)))
        .collect::<Vec<_>>()
        .join("\n");
    let operators = EDITOR_OPERATOR_SPELLINGS
        .iter()
        .map(|operator| js_string(operator))
        .collect::<Vec<_>>()
        .join(", ");
    let template = r#"// Generated from orna-syntax-v1; keywords are the ORNA-LEX-007 inventory.
const KEYWORDS = [
@@KEYWORD_ROWS@@
];
const keywordRules = {};
for (const word of KEYWORDS) keywordRules['kw_' + word] = ($) => word;

module.exports = grammar({
  name: 'orna',
  extras: ($) => [/\s/, $.line_comment, $.block_comment],
  word: ($) => $.identifier,
  rules: Object.assign({
    source_file: ($) => repeat(choice($._declaration, $._statement)),
    _declaration: ($) => choice($.function_declaration, $.type_declaration, $.enum_declaration,
      $.table_declaration, $.protocol_declaration, $.unit_declaration, $.use_declaration,
      $.implementation_declaration),
    function_declaration: ($) => seq(optional($.kw_pub), $.kw_fn, field('name', $.identifier),
      optional($.generic_parameters), $.parameter_list, optional(seq(':', $.type)), '=', $.expression, ';'),
    type_declaration: ($) => seq(optional($.kw_pub), $.kw_type, field('name', $.identifier), '=', $.type, ';'),
    enum_declaration: ($) => seq(optional($.kw_pub), $.kw_enum, field('name', $.identifier), '{',
      commaSep1($.enum_variant), optional(','), '}'),
    enum_variant: ($) => seq($.identifier, optional($.record_type)),
    record_type: ($) => seq('{', commaSep1(seq($.identifier, ':', $.type)), optional(','), '}'),
    table_declaration: ($) => seq(optional($.kw_pub), $.kw_table, field('name', $.identifier),
      optional($.parameter_list), '{', repeat(choice($.field_declaration, $.assertion)), '}'),
    field_declaration: ($) => seq($.identifier, ':', $.type, optional(seq('=', $.expression)), ','),
    assertion: ($) => seq($.kw_assert, $.expression, ';'),
    protocol_declaration: ($) => seq(optional($.kw_pub), $.kw_protocol, $.identifier,
      '{', repeat($.function_signature), '}'),
    function_signature: ($) => seq($.kw_fn, $.identifier, $.parameter_list, optional(seq(':', $.type)), ';'),
    unit_declaration: ($) => seq(optional($.kw_pub), $.kw_unit, $.identifier, '=', $.expression, ';'),
    use_declaration: ($) => seq($.kw_use, $.qualified_name, optional(seq($.kw_as, $.identifier)), ';'),
    implementation_declaration: ($) => seq($.kw_impl, $.type, '{', repeat($.function_declaration), '}'),
    generic_parameters: ($) => seq('<', commaSep1($.identifier), optional(','), '>'),
    parameter_list: ($) => seq('(', commaSep($.parameter), ')'),
    parameter: ($) => seq($.identifier, ':', $.type, optional(seq('=', $.expression))),
    type: ($) => choice($.qualified_name, seq($.identifier, '<', commaSep1($.type), '>'),
      seq('[', $.type, ']'), seq('(', commaSep($.type), ')', '->', $.type)),
    qualified_name: ($) => seq($.identifier, repeat(seq('.', $.identifier))),
    _statement: ($) => choice($.let_statement, $.return_statement, $.while_statement, $.expression_statement),
    let_statement: ($) => seq($.kw_let, $.identifier, optional(seq(':', $.type)), '=', $.expression, ';'),
    return_statement: ($) => seq($.kw_return, optional($.expression), ';'),
    while_statement: ($) => seq($.kw_while, $.expression, $.block),
    expression_statement: ($) => seq($.expression, ';'),
    block: ($) => seq('{', repeat($._statement), '}'),
    expression: ($) => choice($.literal, $.qualified_name, $.call_expression, $.binary_expression,
      $.unary_expression, $.lambda_expression, $.parenthesized_expression, $.record_expression),
    call_expression: ($) => prec(2, seq($.expression, '(', commaSep($.expression), ')')),
    binary_expression: ($) => prec.left(1, seq($.expression, $._operator, $.expression)),
    unary_expression: ($) => prec(3, seq(choice('!', '-', '+'), $.expression)),
    lambda_expression: ($) => prec.right(0, seq('|', commaSep($.identifier), '|', $.expression)),
    parenthesized_expression: ($) => seq('(', $.expression, ')'),
    record_expression: ($) => seq('{', commaSep(seq($.identifier, ':', $.expression)), '}'),
    literal: ($) => choice($.string, $.number, $.kw_true, $.kw_false, $.kw_null),
    string: ($) => seq('"', repeat(choice(/[^"\\]+/, /\\./, seq('{', $.expression, '}'))), '"'),
    number: ($) => /[0-9]+(?:\.[0-9]+)?/,
    identifier: ($) => /[_\p{L}][_\p{L}\p{N}]*/,
    line_comment: ($) => token(seq('//', /[^\n\r]*/)),
    block_comment: ($) => seq('/*', repeat(choice($.block_comment, /[^*]+|\*+[^*/]/)), '*/'),
    _operator: ($) => choice(@@OPERATORS@@),
  }, keywordRules),
});

function commaSep(rule) { return optional(commaSep1(rule)); }
function commaSep1(rule) { return seq(rule, repeat(seq(',', rule))); }
"#;
    template
        .replace("@@KEYWORD_ROWS@@", &keyword_rows)
        .replace("@@OPERATORS@@", &operators)
}

fn render_tree_sitter_highlights(keywords: &[&str]) -> String {
    let keyword_captures = keywords
        .iter()
        .map(|keyword| format!("(kw_{keyword}) @keyword"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "(line_comment) @comment\n(block_comment) @comment\n(string) @string\n(number) @number\n(identifier) @variable\n{keyword_captures}\n"
    )
}

fn artifact(path: &'static str, contents: impl Into<String>) -> GeneratedEditorArtifact {
    GeneratedEditorArtifact {
        path,
        contents: contents.into(),
    }
}

fn regex_escape(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        if matches!(character, '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

fn js_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}
