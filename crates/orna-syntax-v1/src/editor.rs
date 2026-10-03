//! Editor highlighting and generated lexical grammars for Orna 1.0.0.
//!
//! Every generated vocabulary is derived from this crate's lexer. In
//! particular, editor keyword lists come from `Keyword::ALL`, the table used
//! by `Keyword::from_text`; the legacy `orna-syntax` crate is not an input.

use std::ops::Range;

use crate::{
    Keyword, TokenKind,
    lexer::{
        BLOCK_COMMENT_END, BLOCK_COMMENT_START, LINE_COMMENT_START, OPERATORS, PUNCTUATION,
        lex_recovering,
    },
};

/// Shared editor pattern for the literal candidates emitted by the v1
/// numeric lexer. The Rust lexer remains authoritative for validation.
pub const NUMBER_PATTERN: &str = r"(?:[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]*)?(?:Z|[+-][0-9]{2}:[0-9]{2})|[0-9]{4}-[0-9]{2}-[0-9]{2}|0x[0-9A-Fa-f_]*|0b[01_]*|[0-9][0-9_]*(?:\.[0-9_]+)?(?:[eE][+-]?[0-9_]*)?f?)";
pub const BRACKET_PAIRS: &[(&str, &str)] = &[("(", ")"), ("[", "]"), ("{", "}")];

/// Stable editor-facing token classes from the 1.0.0 lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenClass {
    Keyword,
    Identifier,
    Number,
    String,
    Comment,
    Operator,
    Punctuation,
}

/// One classified source token using a half-open UTF-8 byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightToken {
    pub range: Range<usize>,
    pub class: TokenClass,
}

/// Presentation for one 1.0.0 lexical class across editor protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPresentation {
    pub class: TokenClass,
    pub textmate_scope: &'static str,
    pub semantic_token_type: Option<&'static str>,
}

/// Editor and semantic-token vocabulary. Contextual type/function/property
/// roles are intentionally absent because they are not lexer token classes.
pub const TOKEN_PRESENTATIONS: &[TokenPresentation] = &[
    TokenPresentation {
        class: TokenClass::Keyword,
        textmate_scope: "keyword.control.orna",
        semantic_token_type: Some("keyword"),
    },
    TokenPresentation {
        class: TokenClass::Identifier,
        textmate_scope: "variable.other.orna",
        semantic_token_type: Some("variable"),
    },
    TokenPresentation {
        class: TokenClass::Number,
        textmate_scope: "constant.numeric.orna",
        semantic_token_type: Some("number"),
    },
    TokenPresentation {
        class: TokenClass::String,
        textmate_scope: "string.quoted.double.orna",
        semantic_token_type: Some("string"),
    },
    TokenPresentation {
        class: TokenClass::Comment,
        textmate_scope: "comment.orna",
        semantic_token_type: Some("comment"),
    },
    TokenPresentation {
        class: TokenClass::Operator,
        textmate_scope: "keyword.operator.orna",
        semantic_token_type: Some("operator"),
    },
    TokenPresentation {
        class: TokenClass::Punctuation,
        textmate_scope: "punctuation.orna",
        semantic_token_type: None,
    },
];

/// Semantic-token legend in protocol index order.
pub fn semantic_token_types() -> impl Iterator<Item = &'static str> {
    TOKEN_PRESENTATIONS
        .iter()
        .filter_map(|item| item.semantic_token_type)
}

/// Semantic-token index for one v1 lexical class; punctuation is omitted.
pub fn semantic_token_index(class: TokenClass) -> Option<usize> {
    let mut index = 0;
    for item in TOKEN_PRESENTATIONS {
        if item.semantic_token_type.is_some() {
            if item.class == class {
                return Some(index);
            }
            index += 1;
        } else if item.class == class {
            return None;
        }
    }
    None
}

/// Classify the 1.0.0 lexer stream for semantic highlighting.
///
/// The scanner is error tolerant for editor buffers. It keeps tokens emitted
/// before and after a lexical error and recovers omitted comments from the
/// gaps between lexer tokens.
pub fn highlight(source: &str) -> Vec<HighlightToken> {
    let (tokens, _) = lex_recovering(source);
    let mut result = Vec::with_capacity(tokens.len());
    let mut cursor = 0;
    for token in tokens {
        let start = token.span.start;
        if start > cursor {
            comments_in_gap(source, cursor..start, &mut result);
        }
        let class = match token.kind {
            TokenKind::Keyword(_) => Some(TokenClass::Keyword),
            TokenKind::Identifier { .. } | TokenKind::ReplBinding => Some(TokenClass::Identifier),
            TokenKind::Integer
            | TokenKind::Decimal
            | TokenKind::Float
            | TokenKind::Date
            | TokenKind::Instant => Some(TokenClass::Number),
            TokenKind::String
            | TokenKind::StringStart
            | TokenKind::StringText
            | TokenKind::StringEnd => Some(TokenClass::String),
            TokenKind::InterpolationStart | TokenKind::InterpolationEnd => {
                Some(TokenClass::Punctuation)
            }
            TokenKind::Punct(spelling) if OPERATORS.contains(&spelling) => {
                Some(TokenClass::Operator)
            }
            TokenKind::Punct(spelling) if PUNCTUATION.contains(&spelling) => {
                Some(TokenClass::Punctuation)
            }
            TokenKind::Punct(_) | TokenKind::Eof => None,
        };
        if let Some(class) = class {
            result.push(HighlightToken {
                range: token.span.start..token.span.end,
                class,
            });
        }
        cursor = token.span.end.max(cursor);
    }
    if cursor < source.len() {
        comments_in_gap(source, cursor..source.len(), &mut result);
    }
    result.sort_by_key(|token| token.range.start);
    result
}

fn comments_in_gap(source: &str, gap: Range<usize>, out: &mut Vec<HighlightToken>) {
    let bytes = source.as_bytes();
    let mut at = gap.start;
    while at < gap.end {
        if source[at..gap.end].starts_with(LINE_COMMENT_START) {
            let start = at;
            at += 2;
            while at < gap.end && !matches!(bytes[at], b'\r' | b'\n') {
                at += source[at..].chars().next().expect("in bounds").len_utf8();
            }
            out.push(HighlightToken {
                range: start..at,
                class: TokenClass::Comment,
            });
        } else if source[at..gap.end].starts_with(BLOCK_COMMENT_START) {
            let start = at;
            at += 2;
            let mut depth = 1usize;
            while at < gap.end && depth > 0 {
                if source[at..gap.end].starts_with(BLOCK_COMMENT_START) {
                    depth += 1;
                    at += 2;
                } else if source[at..gap.end].starts_with(BLOCK_COMMENT_END) {
                    depth -= 1;
                    at += 2;
                } else {
                    at += source[at..].chars().next().expect("in bounds").len_utf8();
                }
            }
            out.push(HighlightToken {
                range: start..at,
                class: TokenClass::Comment,
            });
        } else {
            at += source[at..].chars().next().expect("in bounds").len_utf8();
        }
    }
}

/// One deterministic, repository-relative generated artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedArtifact {
    pub path: &'static str,
    pub contents: String,
}

pub fn generated_artifacts() -> Vec<GeneratedArtifact> {
    let textmate = render_textmate();
    vec![
        artifact("editors/textmate/orna.tmLanguage.json", textmate.clone()),
        artifact("editors/vscode/syntaxes/orna.tmLanguage.json", textmate),
        artifact(
            "editors/semantic-token-legend.json",
            render_semantic_legend(),
        ),
        artifact(
            "editors/vscode/language-configuration.json",
            render_vscode_language_configuration(),
        ),
        artifact(
            "editors/tree-sitter-orna/grammar.js",
            render_tree_sitter_grammar(),
        ),
        artifact(
            "editors/tree-sitter-orna/queries/highlights.scm",
            render_tree_sitter_query(),
        ),
        artifact("editors/vim/syntax/orna.vim", render_vim()),
        artifact("editors/emacs/orna-eglot.el", render_emacs()),
        artifact("editors/sublime/Orna.sublime-syntax", render_sublime()),
    ]
}

fn render_vscode_language_configuration() -> String {
    let brackets = BRACKET_PAIRS
        .iter()
        .map(|(open, close)| format!("    [{}, {}]", json_string(open), json_string(close)))
        .collect::<Vec<_>>()
        .join(",\n");
    let auto_closing = BRACKET_PAIRS
        .iter()
        .map(|(open, close)| {
            format!(
                "    {{ \"open\": {}, \"close\": {} }}",
                json_string(open),
                json_string(close)
            )
        })
        .chain([format!(
            "    {{ \"open\": {}, \"close\": {}, \"notIn\": [\"string\", \"comment\"] }}",
            json_string("\""),
            json_string("\"")
        )])
        .collect::<Vec<_>>()
        .join(",\n");
    let surrounding = BRACKET_PAIRS
        .iter()
        .map(|(open, close)| format!("    [{}, {}]", json_string(open), json_string(close)))
        .chain([format!("    [{}, {}]", json_string("\""), json_string("\""))])
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"comments\": {{\n    \"lineComment\": \"//\",\n    \"blockComment\": [\"/*\", \"*/\"]\n  }},\n  \"brackets\": [\n{brackets}\n  ],\n  \"autoClosingPairs\": [\n{auto_closing}\n  ],\n  \"surroundingPairs\": [\n{surrounding}\n  ]\n}}\n"
    )
}

fn artifact(path: &'static str, contents: String) -> GeneratedArtifact {
    GeneratedArtifact { path, contents }
}

fn keywords() -> Vec<&'static str> {
    Keyword::ALL.iter().map(|(word, _)| *word).collect()
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn regex_escape(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn regex_alternation(values: &[&str]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_by_key(|value| std::cmp::Reverse(value.len()));
    sorted.dedup();
    sorted
        .into_iter()
        .map(regex_escape)
        .collect::<Vec<_>>()
        .join("|")
}

fn render_textmate() -> String {
    let keyword_re = format!(
        r"(?<![\p{{L}}\p{{N}}_])(?:{})(?![\p{{L}}\p{{N}}_])",
        regex_alternation(&keywords())
    );
    let number_re = format!(
        r"(?<![\p{{L}}\p{{N}}_])(?:{NUMBER_PATTERN})(?![\p{{L}}\p{{N}}_])"
    );
    let operator_re = regex_alternation(OPERATORS);
    let punctuation_re = regex_alternation(PUNCTUATION);
    let patterns = format!(
        r##"{{
  "scopeName": "source.orna",
  "patterns": [
    {{ "include": "#comments" }},
    {{ "include": "#string" }},
    {{ "include": "#keyword" }},
    {{ "include": "#number" }},
    {{ "include": "#operator" }},
    {{ "include": "#punctuation" }},
    {{ "include": "#identifier" }}
  ],
  "repository": {{
    "comments": {{ "patterns": [
      {{ "name": "comment.line.double-slash.orna", "match": "//[^\\r\\n]*" }},
      {{ "name": "comment.block.orna", "begin": "/\\*", "end": "\\*/", "patterns": [{{ "include": "#comments" }}] }}
    ]}},
    "string": {{ "name": "string.quoted.double.orna", "begin": "\\\"", "end": "\\\"", "patterns": [
      {{ "name": "constant.character.escape.orna", "match": "\\\\(?:[\\\"\\\\nrt0]|u\\{{[0-9A-Fa-f]{{1,6}}\\}})" }},
      {{ "name": "meta.interpolation.orna", "begin": "(?<!\\\\)\\{{", "end": "\\}}", "patterns": [{{ "include": "#expressions" }}] }}
    ]}},
    "expressions": {{ "patterns": [{{ "include": "#comments" }}, {{ "include": "#string" }}, {{ "include": "#keyword" }}, {{ "include": "#number" }}, {{ "include": "#operator" }}, {{ "include": "#punctuation" }}, {{ "include": "#identifier" }}] }},
    "keyword": {{ "name": "keyword.control.orna", "match": {} }},
    "number": {{ "name": "constant.numeric.orna", "match": {} }},
    "operator": {{ "name": "keyword.operator.orna", "match": {} }},
    "punctuation": {{ "name": "punctuation.orna", "match": {} }},
    "identifier": {{ "name": "variable.other.orna", "match": "[_\\p{{L}}][_\\p{{L}}\\p{{N}}]*" }}
  }}
}}
"##,
        json_string(&keyword_re),
        json_string(&number_re),
        json_string(&operator_re),
        json_string(&punctuation_re)
    );
    format!("{patterns}\n")
}

fn render_semantic_legend() -> String {
    let token_types = semantic_token_types()
        .map(json_string)
        .collect::<Vec<_>>()
        .join(", ");
    let classes = TOKEN_PRESENTATIONS
        .iter()
        .map(|item| {
            format!(
                "    {{ \"class\": {}, \"semanticTokenType\": {}, \"textmateScope\": {} }}",
                json_string(&format!("{:?}", item.class)),
                item.semantic_token_type
                    .map(json_string)
                    .unwrap_or_else(|| "null".to_owned()),
                json_string(item.textmate_scope)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"language\": \"orna\",\n  \"tokenTypes\": [{token_types}],\n  \"tokenModifiers\": [],\n  \"lexicalClasses\": [\n{classes}\n  ]\n}}\n"
    )
}

fn render_tree_sitter_grammar() -> String {
    let keyword_choices = keywords()
        .iter()
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(", ");
    let operator_choices = OPERATORS
        .iter()
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(", ");
    let punctuation_choices = PUNCTUATION
        .iter()
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(", ");
    let number_pattern = NUMBER_PATTERN;
    format!(
        r#"// Generated from orna-syntax-v1 lexer token definitions; edit the Rust source instead.
module.exports = grammar({{
  name: 'orna',
  extras: $ => [/\s/],
  word: $ => $.identifier,
  rules: {{
    source_file: $ => repeat(choice($.comment, $.string, $.keyword, $.identifier, $.number, $.operator, $.punctuation)),
    comment: $ => choice($.line_comment, $.block_comment),
    line_comment: _ => token(seq('//', /[^\r\n]*/)),
    block_comment: $ => seq('/*', repeat(choice(/[^*/]+/, /\*[^/]/, /\/[^*]/, $.block_comment)), '*/'),
    string: _ => token(seq('"', repeat(choice(/[^"\\]/, /\\./)), '"')),
    keyword: _ => token(prec(2, choice({keyword_choices}))),
    identifier: _ => token(prec(1, /[_\p{{XID_Start}}][_\p{{XID_Continue}}]*/)),
    number: _ => token(/{number_pattern}/),
    operator: _ => token(choice({operator_choices})),
    punctuation: _ => token(choice({punctuation_choices}))
  }}
}});
"#
    )
}

fn render_tree_sitter_query() -> String {
    "; Generated from the orna-syntax-v1 lexical classes.\n(comment) @comment\n(string) @string\n(keyword) @keyword\n(identifier) @variable\n(number) @number\n(operator) @operator\n(punctuation) @punctuation\n".to_owned()
}

fn render_vim() -> String {
    let mut out = String::from(
        "\" Generated from orna-syntax-v1.\nif exists(\"b:current_syntax\") | finish | endif\nsyntax case match\n",
    );
    out.push_str(&format!(
        "syntax keyword ornaKeyword {}\n",
        keywords().join(" ")
    ));
    out.push_str("syntax region ornaString start=+\"+ skip=+\\\\.+ end=+\"+ contains=ornaInterpolation\nsyntax region ornaInterpolation start=+\\\\{+ end=+}+ contained\nsyntax match ornaComment +//.*$+\nsyntax region ornaComment start=+/\\*+ end=+\\*/+ contains=ornaComment\n");
    out.push_str(&format!(
        "syntax match ornaNumber /\\v{}/\n",
        vim_regex(NUMBER_PATTERN)
    ));
    out.push_str(&format!(
        "syntax match ornaOperator +\\({}\\)+\n",
        vim_alternation(OPERATORS)
    ));
    out.push_str(&format!(
        "syntax match ornaPunctuation +\\({}\\)+\n",
        vim_alternation(PUNCTUATION)
    ));
    out.push_str("syntax match ornaIdentifier +[_[:alpha:]][_[:alnum:]]*+\nhi def link ornaKeyword Statement\nhi def link ornaString String\nhi def link ornaComment Comment\nhi def link ornaNumber Number\nhi def link ornaOperator Operator\nhi def link ornaPunctuation Delimiter\nhi def link ornaIdentifier Identifier\nlet b:current_syntax = \"orna\"\n");
    out
}

fn vim_regex(pattern: &str) -> String {
    pattern.replace("(?:", "\\%(")
}

fn vim_alternation(values: &[&str]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_by_key(|item| std::cmp::Reverse(item.len()));
    sorted
        .into_iter()
        .map(|item| {
            item.chars()
                .map(|c| {
                    if r#"\\.^$~[]*""#.contains(c) {
                        format!("\\{c}")
                    } else {
                        c.to_string()
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\\|")
}

fn render_emacs() -> String {
    let word_list = keywords()
        .iter()
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let operators = OPERATORS
        .iter()
        .map(|word| format!("\"{}\"", word.replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ");
    let number_pattern = emacs_regex(NUMBER_PATTERN);
    format!(
        r#";;; orna-eglot.el --- Orna 1.0.0 lexical highlighting -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1. Install packaging is maintained separately.
(require 'eglot)
(defvar orna-keywords '({word_list}))
(defvar orna-operators '({operators}))
(defvar orna-font-lock-keywords
  `((,(regexp-opt orna-keywords 'words) . font-lock-keyword-face)
    ("//.*$" . font-lock-comment-face)
    ("/\\\\*\\\\(?:.\\\\|\\\\n\\\\)*?\\\\*/" . font-lock-comment-face)
    ("\\\"\\\\(?:\\\\\\\\.\\\\|[^\\\"\\\\]\\\\)*\\\"" . font-lock-string-face)
    ({number_pattern} . font-lock-constant-face)
    (,(regexp-opt orna-operators) . font-lock-builtin-face))
  "Lexical highlighting generated from the 1.0.0 lexer.")
(define-derived-mode orna-mode prog-mode "Orna"
  "Major mode for Orna source files."
  (setq-local comment-start "// ")
  (setq-local comment-end "")
  (setq-local font-lock-defaults '(orna-font-lock-keywords nil t)))
(add-to-list 'auto-mode-alist '("\\\\.orna\\\\'" . orna-mode))
(defun orna-setup-eglot ()
  "Register Orna buffers with the orna-lsp language server."
  (add-to-list 'eglot-server-programs (cons '(orna-mode) '("orna-lsp"))))
(provide 'orna-eglot)
"#
    )
}

fn emacs_regex(pattern: &str) -> String {
    let regex = pattern
        .replace("(?:", "\\(?:")
        .replace('{', "\\{")
        .replace('}', "\\}");
    format!("\"{}\"", regex.replace('\\', "\\\\"))
}

fn render_sublime() -> String {
    let keyword_pattern = regex_alternation(&keywords());
    let operator_pattern = regex_alternation(OPERATORS);
    let punctuation_pattern = regex_alternation(PUNCTUATION);
    let number_pattern = NUMBER_PATTERN;
    format!(
        r#"%YAML 1.2
---
name: Orna
file_extensions: [orna]
scope: source.orna
contexts:
  main:
    - match: '//.*$'
      scope: comment.line.orna
    - begin: '/\\*'
      end: '\\*/'
      scope: comment.block.orna
    - begin: '"'
      end: '"'
      scope: string.quoted.double.orna
      patterns:
        - match: '\\\\(?:["\\\\nrt0]|u\\{{[0-9A-Fa-f]{{1,6}}\\}})'
          scope: constant.character.escape.orna
        - begin: '(?<!\\\\)\\{{'
          end: '\\}}'
          scope: meta.interpolation.orna
          patterns:
            - include: main
    - match: '(?<![\\p{{L}}\\p{{N}}_])(?:{keyword_pattern})(?![\\p{{L}}\\p{{N}}_])'
      scope: keyword.control.orna
    - match: '{number_pattern}'
      scope: constant.numeric.orna
    - match: '(?:{operator_pattern})'
      scope: keyword.operator.orna
    - match: '(?:{punctuation_pattern})'
      scope: punctuation.orna
    - match: '[_\\p{{L}}][_\\p{{L}}\\p{{N}}]*'
      scope: variable.other.orna
"#
    )
}
