//! Editor highlighting and generated lexical grammars for Orna 1.0.0.
//!
//! Every generated vocabulary is derived from this crate's lexer. In
//! particular, editor keyword lists come from `Keyword::ALL`, the table used
//! by `Keyword::from_text`; no earlier grammar contributes editor tokens.

use std::ops::Range;

use crate::{
    Keyword, TokenKind,
    lexer::{
        BLOCK_COMMENT_END, BLOCK_COMMENT_START, LINE_COMMENT_START, NUMBER_PATTERN, OPERATORS,
        PUNCTUATION, STRING_DELIMITER, lex_recovering,
    },
};

pub const BRACKET_PAIRS: &[(&str, &str)] = &[("(", ")"), ("[", "]"), ("{", "}")];
pub const LANGUAGE_ID: &str = "orna";
pub const SOURCE_EXTENSION: &str = "orna";
pub const SOURCE_SCOPE: &str = "source.orna";
pub const LANGUAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARTIFACT_MANIFEST_PATH: &str = "editors/generated-artifacts.json";
pub const MONACO_CONFIG_PATH: &str = "playground/web-ui/public/assets/orna-editor-config.json";
pub const EDITOR_EMITTERS: &[&str] = &[
    "TextMate grammar and VSCode extension",
    "Tree-sitter grammar and package metadata",
    "VSCode language configuration",
    "Vim syntax and filetype detection",
    "Vim LSP registration and omnifunc completion",
    "Neovim native LSP attachment",
    "Emacs major mode and Eglot attachment",
    "Sublime syntax",
    "Semantic-token legend",
    "Monaco language configuration and tokenizer",
];

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

/// Semantic-token types the LSP refines beyond the lexical classes. They are
/// declared here so every editor attach legend advertises the same list.
const REFINEMENT_TOKEN_TYPES: &[&str] = &[
    "type",
    "enum",
    "interface",
    "function",
    "parameter",
];

/// Semantic-token modifiers advertised to editors, in protocol bit order.
pub const TOKEN_MODIFIERS: &[&str] = &["declaration", "readonly", "modification"];

/// Renders the complete semantic-token legend as the JSON that editor
/// attachments and the `semantic-legend` command consume.
pub fn semantic_legend_json() -> String {
    let token_types = legend_token_types()
        .map(json_string)
        .collect::<Vec<_>>()
        .join(", ");
    let modifiers = TOKEN_MODIFIERS
        .iter()
        .map(|modifier| json_string(modifier))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{\n  \"tokenTypes\": [{token_types}],\n  \"tokenModifiers\": [{modifiers}]\n}}\n")
}

/// Renders the semantic-token legend as Markdown tables: token types by index,
/// and modifiers by the protocol bit each one sets.
pub fn semantic_legend_markdown() -> String {
    let mut out =
        String::from("### Token types\n\n| Index | Token type | Hover sample |\n| --- | --- | --- |\n");
    for (index, token_type) in legend_token_types().enumerate() {
        let sample = token_type_sample(token_type).unwrap_or("");
        out.push_str(&format!("| {index} | {token_type} | `{sample}` |\n"));
    }
    out.push_str("\n### Token modifiers\n\n| Bit | Token modifier |\n| --- | --- |\n");
    for (index, modifier) in TOKEN_MODIFIERS.iter().enumerate() {
        out.push_str(&format!("| 1 << {index} | {modifier} |\n"));
    }
    out
}

/// Version of the semantic-token legend, taken from this crate's version so
/// the legend and the crate that defines it change together.
pub const SEMANTIC_LEGEND_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Semantic legend as CSV with the columns `kind,index,name,sample`. Token
/// type rows use the protocol index; modifier rows use the bit position and
/// have an empty sample. Fields are quoted per RFC 4180 when needed.
pub fn semantic_legend_csv() -> String {
    let mut out = String::from("kind,index,name,sample\n");
    for (index, token_type) in legend_token_types().enumerate() {
        let sample = token_type_sample(token_type).unwrap_or("");
        out.push_str(&format!(
            "token_type,{index},{},{}\n",
            csv_field(token_type),
            csv_field(sample)
        ));
    }
    for (index, modifier) in TOKEN_MODIFIERS.iter().enumerate() {
        out.push_str(&format!("token_modifier,{index},{},\n", csv_field(modifier)));
    }
    out
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// Example lexeme shown as the hover sample for each legend token type.
const TOKEN_TYPE_SAMPLES: &[(&str, &str)] = &[
    ("keyword", "fn"),
    ("variable", "total"),
    ("number", "42"),
    ("string", "\"text\""),
    ("comment", "// note"),
    ("operator", "+"),
    ("type", "Int"),
    ("enum", "Ordering"),
    ("interface", "Printable"),
    ("function", "increment"),
    ("parameter", "value"),
];

/// Hover sample for one legend token type, if it has one.
pub fn token_type_sample(token_type: &str) -> Option<&'static str> {
    TOKEN_TYPE_SAMPLES
        .iter()
        .find(|(name, _)| *name == token_type)
        .map(|(_, sample)| *sample)
}

/// Complete semantic-token legend advertised to editors: the lexical classes
/// in protocol index order, then the LSP refinements.
pub fn legend_token_types() -> impl Iterator<Item = &'static str> {
    semantic_token_types().chain(REFINEMENT_TOKEN_TYPES.iter().copied())
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
    let mut artifacts = vec![
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
        artifact(MONACO_CONFIG_PATH, render_monaco_config()),
        artifact(
            "editors/tree-sitter-orna/grammar.js",
            render_tree_sitter_grammar(),
        ),
        artifact(
            "editors/tree-sitter-orna/queries/highlights.scm",
            render_tree_sitter_query(),
        ),
        artifact("editors/vim/syntax/orna.vim", render_vim()),
        artifact("editors/vim/ftdetect/orna.vim", render_vim_filetype()),
        artifact("editors/vim/plugin/orna-lsp.vim", render_vim_lsp()),
        artifact("editors/neovim/lua/orna/init.lua", render_neovim_lsp()),
        artifact("editors/emacs/orna-eglot.el", render_emacs()),
        artifact("editors/sublime/Orna.sublime-syntax", render_sublime()),
        artifact("editors/vscode/package.json", render_vscode_package()),
        artifact(
            "editors/tree-sitter-orna/package.json",
            render_tree_sitter_package(),
        ),
        artifact(
            "editors/tree-sitter-orna/tree-sitter.json",
            render_tree_sitter_metadata(),
        ),
    ];
    let mut paths = artifacts.iter().map(|item| item.path).collect::<Vec<_>>();
    paths.push(ARTIFACT_MANIFEST_PATH);
    artifacts.push(artifact(
        ARTIFACT_MANIFEST_PATH,
        render_artifact_manifest(&paths),
    ));
    artifacts
}

fn render_artifact_manifest(paths: &[&str]) -> String {
    let files = paths
        .iter()
        .map(|path| format!("    {}", json_string(path)))
        .collect::<Vec<_>>()
        .join(",\n");
    let emitters = EDITOR_EMITTERS
        .iter()
        .map(|emitter| format!("    {}", json_string(emitter)))
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"source\": \"orna-syntax-v1\",\n  \"languageVersion\": {},\n  \"emitters\": [\n{emitters}\n  ],\n  \"generatedFiles\": [\n{files}\n  ]\n}}\n",
        json_string(LANGUAGE_VERSION)
    )
}

fn render_vscode_package() -> String {
    format!(
        "{{\n  \"name\": \"orna-syntax\",\n  \"displayName\": \"Orna Syntax\",\n  \"description\": \"Generated TextMate syntax support for Orna source files\",\n  \"version\": {},\n  \"engines\": {{ \"vscode\": \"^1.80.0\" }},\n  \"categories\": [\"Programming Languages\"],\n  \"contributes\": {{\n    \"languages\": [{{\n      \"id\": {},\n      \"aliases\": [\"Orna\"],\n      \"extensions\": [\".{}\"],\n      \"configuration\": \"./language-configuration.json\"\n    }}],\n    \"grammars\": [{{\n      \"language\": {},\n      \"scopeName\": {},\n      \"path\": \"./syntaxes/orna.tmLanguage.json\"\n    }}]\n  }}\n}}\n",
        json_string(LANGUAGE_VERSION),
        json_string(LANGUAGE_ID),
        SOURCE_EXTENSION,
        json_string(LANGUAGE_ID),
        json_string(SOURCE_SCOPE)
    )
}

fn render_tree_sitter_package() -> String {
    format!(
        "{{\n  \"name\": \"tree-sitter-orna\",\n  \"version\": {},\n  \"description\": \"Generated Tree-sitter grammar for Orna\",\n  \"tree-sitter\": {{\n    \"scopes\": {{ {}: \"orna\" }},\n    \"file-types\": [\"{}\"],\n    \"highlights\": [\"queries/highlights.scm\"]\n  }},\n  \"scripts\": {{ \"test\": \"tree-sitter test\" }},\n  \"devDependencies\": {{ \"tree-sitter-cli\": \"^0.26.5\" }}\n}}\n",
        json_string(LANGUAGE_VERSION),
        json_string(SOURCE_SCOPE),
        SOURCE_EXTENSION
    )
}

fn render_tree_sitter_metadata() -> String {
    format!(
        "{{\n  \"grammars\": [{{\n    \"name\": {},\n    \"camelcase\": \"Orna\",\n    \"scope\": {},\n    \"path\": \".\",\n    \"file-types\": [\"{}\"],\n    \"highlights\": \"queries/highlights.scm\"\n  }}],\n  \"metadata\": {{\n    \"version\": {},\n    \"license\": \"MIT\",\n    \"description\": \"Generated Tree-sitter grammar for Orna\",\n    \"authors\": [{{ \"name\": \"OrnaDB\" }}],\n    \"links\": {{ \"repository\": \"https://github.com/workingdir/ornadb\" }}\n  }}\n}}\n",
        json_string(LANGUAGE_ID),
        json_string(SOURCE_SCOPE),
        SOURCE_EXTENSION,
        json_string(LANGUAGE_VERSION)
    )
}

fn presentation(class: TokenClass) -> &'static TokenPresentation {
    TOKEN_PRESENTATIONS
        .iter()
        .find(|item| item.class == class)
        .expect("every lexical class has a presentation")
}

fn class_name(class: TokenClass) -> &'static str {
    match class {
        TokenClass::Keyword => "keyword",
        TokenClass::Identifier => "identifier",
        TokenClass::Number => "number",
        TokenClass::String => "string",
        TokenClass::Comment => "comment",
        TokenClass::Operator => "operator",
        TokenClass::Punctuation => "punctuation",
    }
}

fn capture_name(class: TokenClass) -> &'static str {
    presentation(class)
        .semantic_token_type
        .unwrap_or("punctuation")
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
        .chain([format!(
            "    [{}, {}]",
            json_string("\""),
            json_string("\"")
        )])
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"comments\": {{\n    \"lineComment\": {},\n    \"blockComment\": [{}, {}]\n  }},\n  \"brackets\": [\n{brackets}\n  ],\n  \"autoClosingPairs\": [\n{auto_closing}\n  ],\n  \"surroundingPairs\": [\n{surrounding}\n  ]\n}}\n",
        json_string(LINE_COMMENT_START),
        json_string(BLOCK_COMMENT_START),
        json_string(BLOCK_COMMENT_END)
    )
}

fn artifact(path: &'static str, contents: String) -> GeneratedArtifact {
    GeneratedArtifact { path, contents }
}

fn keywords() -> Vec<&'static str> {
    Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling())
        .collect()
}

fn render_monaco_config() -> String {
    let keyword_pattern = format!(r"\b(?:{})\b", regex_alternation(&keywords()));
    let operator_pattern = format!("(?:{})", regex_alternation(OPERATORS));
    let punctuation_pattern = format!("(?:{})", regex_alternation(PUNCTUATION));
    let keywords_json = json_string_array(&keywords());
    let operators_json = json_string_array(OPERATORS);
    let language_configuration = render_vscode_language_configuration();
    let root_rules = [
        monarch_token(r"\s+", "white"),
        monarch_token(
            &format!("{}.*$", regex_escape(LINE_COMMENT_START)),
            "comment",
        ),
        format!(
            "[{}, {{ \"token\": \"comment\", \"next\": \"@comment\" }}]",
            json_string(&regex_escape(BLOCK_COMMENT_START))
        ),
        format!(
            "[{}, {{ \"token\": \"string.quote\", \"next\": \"@string\" }}]",
            json_string(&STRING_DELIMITER.to_string())
        ),
        monarch_token(&keyword_pattern, "keyword"),
        monarch_token(NUMBER_PATTERN, "number"),
        format!(
            "[{{ \"pattern\": {}, \"flags\": \"u\" }}, \"function\"]",
            json_string(r"[\p{L}_$][\p{L}\p{N}_$]*(?=\s*\()")
        ),
        format!(
            "[{{ \"pattern\": {}, \"flags\": \"u\" }}, \"identifier\"]",
            json_string(r"[\p{L}_$][\p{L}\p{N}_$]*")
        ),
        monarch_token(&operator_pattern, "operator"),
        monarch_token(&punctuation_pattern, "delimiter"),
    ]
    .join(",\n        ");
    let comment_rules = [
        monarch_token("[^*/]+", "comment"),
        format!(
            "[{}, {{ \"token\": \"comment\", \"next\": \"@pop\" }}]",
            json_string(&regex_escape(BLOCK_COMMENT_END))
        ),
        monarch_token(r"[*/]", "comment"),
    ]
    .join(",\n        ");
    let string_rules = [
        monarch_token(r#"[^\\"]+"#, "string"),
        monarch_token(r"\\.", "string.escape"),
        format!(
            "[{}, {{ \"token\": \"string.quote\", \"next\": \"@pop\" }}]",
            json_string(&STRING_DELIMITER.to_string())
        ),
    ]
    .join(",\n        ");

    format!(
        "{{\n  \"language\": {{ \"id\": {}, \"extensions\": [{}], \"aliases\": [\"Orna\", \"orna\"] }},\n  \"languageConfiguration\": {language_configuration},\n  \"monarchLanguage\": {{\n    \"defaultToken\": \"\",\n    \"tokenPostfix\": \".orna\",\n    \"keywords\": {keywords_json},\n    \"operators\": {operators_json},\n    \"tokenizer\": {{\n      \"root\": [\n        {root_rules}\n      ],\n      \"comment\": [\n        {comment_rules}\n      ],\n      \"string\": [\n        {string_rules}\n      ]\n    }}\n  }},\n  \"editorOptions\": {{ \"theme\": \"vs\", \"fontSize\": 14, \"tabSize\": 4, \"insertSpaces\": true, \"lineNumbers\": \"on\", \"minimap\": {{ \"enabled\": false }}, \"scrollBeyondLastLine\": false, \"wordWrap\": \"off\" }}\n}}\n",
        json_string(LANGUAGE_ID),
        json_string(&format!(".{SOURCE_EXTENSION}")),
    )
}

fn monarch_token(pattern: &str, token: &str) -> String {
    format!("[{}, {}]", json_string(pattern), json_string(token))
}

fn json_string_array(values: &[&str]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
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
    let number_re = format!(r"(?<![\p{{L}}\p{{N}}_])(?:{NUMBER_PATTERN})(?![\p{{L}}\p{{N}}_])");
    let operator_re = regex_alternation(OPERATORS);
    let punctuation_re = regex_alternation(PUNCTUATION);
    let line_comment_re = json_string(&format!("{}[^\\r\\n]*", regex_escape(LINE_COMMENT_START)));
    let block_begin_re = json_string(&regex_escape(BLOCK_COMMENT_START));
    let block_end_re = json_string(&regex_escape(BLOCK_COMMENT_END));
    let string_delimiter_re = json_string(&regex_escape(&STRING_DELIMITER.to_string()));
    let patterns = format!(
        r##"{{
  "scopeName": {},
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
      {{ "name": {}, "match": {} }},
      {{ "name": {}, "begin": {}, "end": {}, "patterns": [{{ "include": "#comments" }}] }}
    ]}},
    "string": {{ "name": {}, "begin": {}, "end": {}, "patterns": [
      {{ "name": "constant.character.escape.orna", "match": "\\\\(?:[\\\"\\\\nrt0]|u\\{{[0-9A-Fa-f]{{1,6}}\\}})" }},
      {{ "name": "meta.interpolation.orna", "begin": "(?<!\\\\)\\{{", "end": "\\}}", "patterns": [{{ "include": "#expressions" }}] }}
    ]}},
    "expressions": {{ "patterns": [{{ "include": "#comments" }}, {{ "include": "#string" }}, {{ "include": "#keyword" }}, {{ "include": "#number" }}, {{ "include": "#operator" }}, {{ "include": "#punctuation" }}, {{ "include": "#identifier" }}] }},
    "keyword": {{ "name": {}, "match": {} }},
    "number": {{ "name": {}, "match": {} }},
    "operator": {{ "name": {}, "match": {} }},
    "punctuation": {{ "name": {}, "match": {} }},
    "identifier": {{ "name": {}, "match": "[_\\p{{L}}][_\\p{{L}}\\p{{N}}]*" }}
  }}
}}
"##,
        json_string(SOURCE_SCOPE),
        json_string(presentation(TokenClass::Comment).textmate_scope),
        line_comment_re,
        json_string(presentation(TokenClass::Comment).textmate_scope),
        block_begin_re,
        block_end_re,
        json_string(presentation(TokenClass::String).textmate_scope),
        string_delimiter_re,
        string_delimiter_re,
        json_string(presentation(TokenClass::Keyword).textmate_scope),
        json_string(&keyword_re),
        json_string(presentation(TokenClass::Number).textmate_scope),
        json_string(&number_re),
        json_string(presentation(TokenClass::Operator).textmate_scope),
        json_string(&operator_re),
        json_string(presentation(TokenClass::Punctuation).textmate_scope),
        json_string(&punctuation_re),
        json_string(presentation(TokenClass::Identifier).textmate_scope)
    );
    format!("{patterns}\n")
}

fn render_semantic_legend() -> String {
    let token_types = legend_token_types()
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
    let source_choices = TOKEN_PRESENTATIONS
        .iter()
        .map(|item| format!("$.{}", class_name(item.class)))
        .collect::<Vec<_>>()
        .join(", ");
    let number_pattern = NUMBER_PATTERN;
    let line_comment = json_string(LINE_COMMENT_START);
    let block_start = json_string(BLOCK_COMMENT_START);
    let block_end = json_string(BLOCK_COMMENT_END);
    let string_delimiter = json_string(&STRING_DELIMITER.to_string());
    format!(
        r#"// Generated from orna-syntax-v1 lexer token definitions; edit the Rust source instead.
module.exports = grammar({{
  name: {language},
  extras: $ => [/\s/],
  word: $ => $.identifier,
  rules: {{
    source_file: $ => repeat(choice({source_choices})),
    comment: $ => choice($.line_comment, $.block_comment),
    line_comment: _ => token(seq({line_comment}, /[^\r\n]*/)),
    block_comment: $ => seq({block_start}, repeat(choice(/[^*/]+/, /\*[^/]/, /\/[^*]/, $.block_comment)), {block_end}),
    string: _ => token(seq({string_delimiter}, repeat(choice(/[^"\\]/, /\\./)), {string_delimiter})),
    keyword: _ => token(prec(2, choice({keyword_choices}))),
    identifier: _ => token(prec(1, /[_\p{{XID_Start}}][_\p{{XID_Continue}}]*/)),
    number: _ => token(/{number_pattern}/),
    operator: _ => token(choice({operator_choices})),
    punctuation: _ => token(choice({punctuation_choices}))
  }}
}});
"#,
        language = json_string(LANGUAGE_ID),
        source_choices = source_choices,
        line_comment = line_comment,
        block_start = block_start,
        block_end = block_end,
        string_delimiter = string_delimiter,
        keyword_choices = keyword_choices,
        number_pattern = number_pattern,
        operator_choices = operator_choices,
        punctuation_choices = punctuation_choices
    )
}

fn render_tree_sitter_query() -> String {
    let captures = TOKEN_PRESENTATIONS
        .iter()
        .map(|item| format!("({}) @{}", class_name(item.class), capture_name(item.class)))
        .collect::<Vec<_>>()
        .join("\n");
    format!("; Generated from the orna-syntax-v1 lexical classes.\n{captures}\n")
}

fn render_vim() -> String {
    let mut out = String::from(
        "\" Generated from orna-syntax-v1.\nif exists(\"b:current_syntax\") | finish | endif\nsyntax case match\n",
    );
    out.push_str(&format!(
        "syntax keyword ornaKeyword {}\n",
        keywords().join(" ")
    ));
    let string_delimiter = STRING_DELIMITER.to_string();
    let block_comment_start = vim_regex_literal(BLOCK_COMMENT_START);
    let block_comment_end = vim_regex_literal(BLOCK_COMMENT_END);
    out.push_str(&format!(
        "syntax region ornaString start=+{string_delimiter}+ skip=+\\\\.+ end=+{string_delimiter}+ contains=ornaInterpolation\nsyntax region ornaInterpolation start=+\\\\{{+ end=+}}+ contained\nsyntax match ornaComment +{}.*$+\nsyntax region ornaComment start=+{block_comment_start}+ end=+{block_comment_end}+ contains=ornaComment\n",
        LINE_COMMENT_START
    ));
    out.push_str(&format!(
        "syntax match ornaNumber /\\v{}/\n",
        vim_regex(NUMBER_PATTERN)
    ));
    out.push_str(&format!(
        "syntax match ornaOperator @\\({}\\)@\n",
        vim_alternation(OPERATORS)
    ));
    out.push_str(&format!(
        "syntax match ornaPunctuation +\\({}\\)+\n",
        vim_alternation(PUNCTUATION)
    ));
    out.push_str("syntax match ornaIdentifier +[_[:alpha:]][_[:alnum:]]*+\n");
    for (class, group, face) in [
        (TokenClass::Keyword, "ornaKeyword", "Statement"),
        (TokenClass::String, "ornaString", "String"),
        (TokenClass::Comment, "ornaComment", "Comment"),
        (TokenClass::Number, "ornaNumber", "Number"),
        (TokenClass::Operator, "ornaOperator", "Operator"),
        (TokenClass::Punctuation, "ornaPunctuation", "Delimiter"),
        (TokenClass::Identifier, "ornaIdentifier", "Identifier"),
    ] {
        debug_assert!(
            presentation(class).semantic_token_type.is_some() || class == TokenClass::Punctuation
        );
        out.push_str(&format!("hi def link {group} {face}\n"));
    }
    out.push_str("let b:current_syntax = \"orna\"\n");
    out
}

fn render_vim_filetype() -> String {
    format!(
        "\" Generated from orna-syntax-v1 language metadata.\naugroup {LANGUAGE_ID}_filetype\n    au!\n    au BufRead,BufNewFile *.{SOURCE_EXTENSION} setfiletype {LANGUAGE_ID}\naugroup END\n"
    )
}

fn render_vim_lsp() -> String {
    format!(
        r#"" Generated from orna-syntax-v1 language metadata.
if !exists('g:orna_lsp_command')
    let g:orna_lsp_command = ['orna-lsp']
endif
function! s:on_lsp_buffer_enabled() abort
    if &l:filetype ==# '{LANGUAGE_ID}'
        setlocal omnifunc=lsp#complete
    endif
endfunction
augroup orna_lsp
    au!
    au User lsp_setup call lsp#register_server({{
        \ 'name': '{LANGUAGE_ID}',
        \ 'cmd': {{server_info -> copy(g:orna_lsp_command)}},
        \ 'allowlist': ['{LANGUAGE_ID}'],
        \ }})
    au User lsp_buffer_enabled call <SID>on_lsp_buffer_enabled()
augroup END
"#
    )
}

fn render_neovim_lsp() -> String {
    format!(
        r#"-- Generated from orna-syntax-v1 language metadata.
local M = {{}}

function M.setup(options)
  options = options or {{}}
  local command = options.cmd or {{ "orna-lsp" }}
  vim.filetype.add({{ extension = {{ orna = "orna" }} }})
  local group = vim.api.nvim_create_augroup("orna_lsp", {{ clear = true }})

  vim.api.nvim_create_autocmd("FileType", {{
    group = group,
    pattern = "{LANGUAGE_ID}",
    callback = function(event)
      local root_dir = options.root_dir
      if type(root_dir) == "function" then
        root_dir = root_dir(event.buf)
      end
      vim.lsp.start({{
        name = "{LANGUAGE_ID}",
        cmd = command,
        root_dir = root_dir or vim.fn.getcwd(),
      }}, {{ bufnr = event.buf }})
    end,
  }})
end

return M
"#
    )
}

fn vim_regex(pattern: &str) -> String {
    pattern.replace("(?:", "\\%(")
}

fn vim_regex_literal(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if r#"\\.^$~[]*"#.contains(character) {
                format!("\\{character}")
            } else {
                character.to_string()
            }
        })
        .collect()
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
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(" ");
    let operators = OPERATORS
        .iter()
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(" ");
    let punctuation = PUNCTUATION
        .iter()
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(" ");
    let number_pattern = emacs_regex(NUMBER_PATTERN);
    let line_comment_pattern = emacs_regex(&format!("{}.*$", regex_escape(LINE_COMMENT_START)));
    let block_comment_pattern = emacs_regex(&format!(
        "{}(?:.|\\n)*?{}",
        regex_escape(BLOCK_COMMENT_START),
        regex_escape(BLOCK_COMMENT_END)
    ));
    let delimiter = STRING_DELIMITER.to_string();
    let string_pattern = emacs_regex(&format!(
        "{delimiter}(?:\\\\.|[^{delimiter}\\\\])*{delimiter}"
    ));
    let identifier_pattern = emacs_regex("[_[:alpha:]][_[:alnum:]]*");
    let mut rules = Vec::new();
    for item in TOKEN_PRESENTATIONS {
        let face = emacs_face(item.class);
        match item.class {
            TokenClass::Keyword => {
                rules.push(format!("(,(regexp-opt '({word_list}) 'words) . {face})"))
            }
            TokenClass::Identifier => {
                rules.push(format!("({identifier_pattern} . {face})"));
            }
            TokenClass::Number => rules.push(format!("({number_pattern} . {face})")),
            TokenClass::String => rules.push(format!("({string_pattern} . {face})")),
            TokenClass::Comment => {
                rules.push(format!("({line_comment_pattern} . {face})"));
                rules.push(format!("({block_comment_pattern} . {face})"));
            }
            TokenClass::Operator => rules.push(format!("(,(regexp-opt '({operators})) . {face})")),
            TokenClass::Punctuation => {
                rules.push(format!("(,(regexp-opt '({punctuation})) . {face})"))
            }
        }
    }
    let rules = rules.join("\n    ");
    let comment_start = json_string(&format!("{LINE_COMMENT_START} "));
    let extension_pattern = emacs_regex(&format!(r"\.{}\'", SOURCE_EXTENSION));
    format!(
        r#";;; orna-eglot.el --- Orna {LANGUAGE_VERSION} lexical highlighting -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1.
(require 'eglot)
(defvar orna-eglot-server-command '("orna-lsp")
  "Command used by Eglot to start the Orna language server.")
(defvar orna-font-lock-keywords
  `(
    {rules}
  )
  "Lexical highlighting generated from the v1 lexer.")
(define-derived-mode orna-mode prog-mode "Orna"
  "Major mode for Orna source files."
  (setq-local comment-start {comment_start})
  (setq-local comment-end "")
  (setq-local font-lock-defaults '(orna-font-lock-keywords nil nil)))
(add-to-list 'auto-mode-alist '({extension_pattern} . orna-mode))
(defun orna-setup-eglot ()
  "Attach Orna buffers to the configured language server with Eglot."
  (add-to-list 'eglot-server-programs
               (cons '(orna-mode) orna-eglot-server-command))
  (add-hook 'orna-mode-hook #'eglot-ensure))
(provide 'orna-eglot)
"#,
        rules = rules,
        comment_start = comment_start,
        extension_pattern = extension_pattern
    )
}

fn emacs_face(class: TokenClass) -> &'static str {
    match class {
        TokenClass::Keyword => "font-lock-keyword-face",
        TokenClass::Identifier => "font-lock-variable-name-face",
        TokenClass::Number => "font-lock-constant-face",
        TokenClass::String => "font-lock-string-face",
        TokenClass::Comment => "font-lock-comment-face",
        TokenClass::Operator => "font-lock-builtin-face",
        TokenClass::Punctuation => "font-lock-delimiter-face",
    }
}

fn emacs_regex(pattern: &str) -> String {
    let mut source = pattern.chars().peekable();
    let mut regex = String::new();
    let mut in_class = false;
    while let Some(ch) = source.next() {
        if ch == '\\' {
            regex.push(ch);
            if let Some(escaped) = source.next() {
                regex.push(escaped);
            }
            continue;
        }
        match ch {
            '[' => {
                in_class = true;
                regex.push(ch);
            }
            ']' => {
                in_class = false;
                regex.push(ch);
            }
            '(' if !in_class => {
                let mut lookahead = source.clone();
                if lookahead.next() == Some('?') && lookahead.next() == Some(':') {
                    source.next();
                    source.next();
                    regex.push_str("\\(?:");
                } else {
                    regex.push_str("\\(");
                }
            }
            ')' if !in_class => regex.push_str("\\)"),
            '|' | '?' | '+' if !in_class => {
                regex.push('\\');
                regex.push(ch);
            }
            '{' | '}' if !in_class => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => regex.push(ch),
        }
    }
    let mut literal = String::from("\"");
    for ch in regex.chars() {
        match ch {
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            _ => literal.push(ch),
        }
    }
    literal.push('"');
    literal
}

fn render_sublime() -> String {
    let keyword_pattern = regex_alternation(&keywords());
    let operator_pattern = regex_alternation(OPERATORS);
    let punctuation_pattern = regex_alternation(PUNCTUATION);
    let number_pattern = NUMBER_PATTERN;
    let line_comment_match = yaml_string(&format!("{}.*$", regex_escape(LINE_COMMENT_START)));
    let block_comment_start = yaml_string(&regex_escape(BLOCK_COMMENT_START));
    let block_comment_end = yaml_string(&regex_escape(BLOCK_COMMENT_END));
    let string_delimiter = yaml_string(&regex_escape(&STRING_DELIMITER.to_string()));
    let comment_scope = yaml_string(presentation(TokenClass::Comment).textmate_scope);
    let string_scope = yaml_string(presentation(TokenClass::String).textmate_scope);
    let keyword_scope = yaml_string(presentation(TokenClass::Keyword).textmate_scope);
    let number_scope = yaml_string(presentation(TokenClass::Number).textmate_scope);
    let operator_scope = yaml_string(presentation(TokenClass::Operator).textmate_scope);
    let punctuation_scope = yaml_string(presentation(TokenClass::Punctuation).textmate_scope);
    let identifier_scope = yaml_string(presentation(TokenClass::Identifier).textmate_scope);
    format!(
        r#"%YAML 1.2
---
name: Orna
file_extensions: [{SOURCE_EXTENSION}]
scope: source.orna
contexts:
  main:
    - match: {line_comment_match}
      scope: {comment_scope}
    - begin: {block_comment_start}
      end: {block_comment_end}
      scope: {comment_scope}
    - begin: {string_delimiter}
      end: {string_delimiter}
      scope: {string_scope}
      patterns:
        - match: '\\\\(?:["\\\\nrt0]|u\\{{[0-9A-Fa-f]{{1,6}}\\}})'
          scope: constant.character.escape.orna
        - begin: '(?<!\\\\)\\{{'
          end: '\\}}'
          scope: meta.interpolation.orna
          patterns:
            - include: main
    - match: '(?<![\\p{{L}}\\p{{N}}_])(?:{keyword_pattern})(?![\\p{{L}}\\p{{N}}_])'
      scope: {keyword_scope}
    - match: '{number_pattern}'
      scope: {number_scope}
    - match: '(?:{operator_pattern})'
      scope: {operator_scope}
    - match: '(?:{punctuation_pattern})'
      scope: {punctuation_scope}
    - match: '[_\\p{{L}}][_\\p{{L}}\\p{{N}}]*'
      scope: {identifier_scope}
"#,
        line_comment_match = line_comment_match,
        comment_scope = comment_scope,
        block_comment_start = block_comment_start,
        block_comment_end = block_comment_end,
        string_delimiter = string_delimiter,
        string_scope = string_scope,
        keyword_pattern = keyword_pattern,
        keyword_scope = keyword_scope,
        number_pattern = number_pattern,
        number_scope = number_scope,
        operator_pattern = operator_pattern,
        operator_scope = operator_scope,
        punctuation_pattern = punctuation_pattern,
        punctuation_scope = punctuation_scope,
        identifier_scope = identifier_scope
    )
}

fn yaml_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
