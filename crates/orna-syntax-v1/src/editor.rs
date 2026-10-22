//! Editor highlighting and generated lexical grammars for Orna 1.0.0.
//!
//! Every generated vocabulary is derived from this crate's lexer. In
//! particular, editor keyword lists come from `Keyword::ALL`, the table used
//! by `Keyword::from_text`; no earlier grammar contributes editor tokens.
//! Token patterns come from one lexical model (`editor::lexical`) rendered
//! per editor dialect.

mod lexical;

use std::ops::Range;

use lexical::{Dialect, Re};

use crate::{
    TokenKind,
    lexer::{
        BLOCK_COMMENT_END, BLOCK_COMMENT_START, LINE_COMMENT_START, OPERATORS, PUNCTUATION,
        STRING_DELIMITER, lex_recovering,
    },
};

pub const BRACKET_PAIRS: &[(&str, &str)] = &[("(", ")"), ("[", "]"), ("{", "}")];
pub const LANGUAGE_ID: &str = "orna";
pub const SOURCE_EXTENSION: &str = "orna";
pub const SOURCE_SCOPE: &str = "source.orna";
pub const LANGUAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARTIFACT_MANIFEST_PATH: &str = "editors/generated-artifacts.json";
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

/// Semantic legend as an aligned plain-text table with the columns
/// `Index`, `Token type` and `Hover sample`. Columns are padded to the widest
/// cell so the table reads cleanly in a terminal.
pub fn semantic_legend_table() -> String {
    let rows = legend_token_types()
        .enumerate()
        .map(|(index, token_type)| {
            (
                index.to_string(),
                token_type.to_owned(),
                token_type_sample(token_type).unwrap_or("").to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let index_width = rows.iter().map(|row| row.0.len()).max().unwrap_or(0).max("Index".len());
    let type_width = rows.iter().map(|row| row.1.len()).max().unwrap_or(0).max("Token type".len());
    let mut out = format!(
        "{:<index_width$}  {:<type_width$}  {}\n",
        "Index", "Token type", "Hover sample"
    );
    for (index, token_type, sample) in &rows {
        out.push_str(&format!(
            "{index:<index_width$}  {token_type:<type_width$}  {sample}\n"
        ));
    }
    out
}

/// Number of advertised token types and token modifiers.
pub fn semantic_legend_counts() -> (usize, usize) {
    (legend_token_types().count(), TOKEN_MODIFIERS.len())
}

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

/// Oniguruma token patterns shared by the TextMate and Sublime grammars.
struct OnigurumaRules {
    keyword: String,
    contextual_name: String,
    number: String,
    operator: String,
    punctuation: String,
    identifier: String,
    escape: String,
    line_comment: String,
    block_comment_start: String,
    block_comment_end: String,
    quote: String,
    open_brace: String,
    close_brace: String,
}

impl OnigurumaRules {
    fn new() -> Self {
        let render = |re: &Re| re.render(Dialect::Oniguruma);
        let continuation = render(&Re::IdentifierContinue);
        let not_after_name = format!("(?<!{continuation})");
        let not_before_name = format!("(?!{continuation})");
        let keywords = render(&lexical::spellings(&lexical::keywords()));
        let labels = render(&lexical::spellings(&lexical::label_keywords()));
        Self {
            keyword: format!("{not_after_name}{keywords}{not_before_name}"),
            // "Contextual names and construction": after a single `.` a reserved
            // word is a member, variant or qualified name, and before `:` it
            // labels a record field or a named argument. `..` is the range
            // operator, and `true`/`false`/`null` before `:` are case-arm
            // literal patterns.
            contextual_name: format!(
                r"(?:(?<=[^.]\.)|(?<=^\.)){keywords}{not_before_name}|{not_after_name}{labels}{not_before_name}(?=[ \t]*:)"
            ),
            number: format!(
                "{not_after_name}{}{not_before_name}",
                render(&lexical::number())
            ),
            operator: render(&lexical::operators()),
            punctuation: render(&lexical::punctuation()),
            identifier: render(&lexical::identifier()),
            escape: render(&lexical::escape_sequence()),
            line_comment: format!("{}.*", render(&Re::text(LINE_COMMENT_START))),
            block_comment_start: render(&Re::text(BLOCK_COMMENT_START)),
            block_comment_end: render(&Re::text(BLOCK_COMMENT_END)),
            quote: render(&Re::text(&STRING_DELIMITER.to_string())),
            open_brace: render(&Re::text("{")),
            close_brace: render(&Re::text("}")),
        }
    }
}

/// TextMate grammar for VS Code. A block comment nests only block comments
/// (ORNA-LEX-004), so `//` inside one is comment text. An interpolation
/// keeps nested braces and strings balanced, because interpolation
/// expressions use the ordinary lexer and nested-delimiter rules (ORNA-LIT-004,
/// "String escapes").
fn render_textmate() -> String {
    let rules = OnigurumaRules::new();
    let scope = |class| json_string(presentation(class).textmate_scope);
    let punctuation_scope = scope(TokenClass::Punctuation);
    let identifier_scope = scope(TokenClass::Identifier);
    let braces = format!(
        "\"begin\": {}, \"end\": {}, \"beginCaptures\": {{ \"0\": {{ \"name\": {punctuation_scope} }} }}, \"endCaptures\": {{ \"0\": {{ \"name\": {punctuation_scope} }} }}, \"patterns\": [{{ \"include\": \"#expressions\" }}]",
        json_string(&rules.open_brace),
        json_string(&rules.close_brace)
    );
    let includes = |names: &[&str]| {
        names
            .iter()
            .map(|name| format!("{{ \"include\": \"#{name}\" }}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tokens = [
        "contextual_name",
        "keyword",
        "number",
        "operator",
        "punctuation",
        "identifier",
    ];
    let top_level = includes(&[&["comments", "string"][..], &tokens[..]].concat());
    let expressions = includes(&[&["comments", "string", "braces"][..], &tokens[..]].concat());
    format!(
        r##"{{
  "scopeName": {scope_name},
  "patterns": [{top_level}],
  "repository": {{
    "comments": {{ "patterns": [{{ "include": "#line_comment" }}, {{ "include": "#block_comment" }}] }},
    "line_comment": {{ "name": {comment_scope}, "match": {line_comment} }},
    "block_comment": {{ "name": {comment_scope}, "begin": {block_start}, "end": {block_end}, "patterns": [{{ "include": "#block_comment" }}] }},
    "string": {{ "name": {string_scope}, "begin": {quote}, "end": {quote}, "patterns": [
      {{ "name": "constant.character.escape.orna", "match": {escape} }},
      {{ "name": "meta.interpolation.orna", {braces} }}
    ]}},
    "braces": {{ {braces} }},
    "expressions": {{ "patterns": [{expressions}] }},
    "contextual_name": {{ "name": {identifier_scope}, "match": {contextual_name} }},
    "keyword": {{ "name": {keyword_scope}, "match": {keyword} }},
    "number": {{ "name": {number_scope}, "match": {number} }},
    "operator": {{ "name": {operator_scope}, "match": {operator} }},
    "punctuation": {{ "name": {punctuation_scope}, "match": {punctuation} }},
    "identifier": {{ "name": {identifier_scope}, "match": {identifier} }}
  }}
}}
"##,
        scope_name = json_string(SOURCE_SCOPE),
        comment_scope = scope(TokenClass::Comment),
        line_comment = json_string(&rules.line_comment),
        block_start = json_string(&rules.block_comment_start),
        block_end = json_string(&rules.block_comment_end),
        string_scope = scope(TokenClass::String),
        quote = json_string(&rules.quote),
        escape = json_string(&rules.escape),
        contextual_name = json_string(&rules.contextual_name),
        keyword_scope = scope(TokenClass::Keyword),
        keyword = json_string(&rules.keyword),
        number_scope = scope(TokenClass::Number),
        number = json_string(&rules.number),
        operator_scope = scope(TokenClass::Operator),
        operator = json_string(&rules.operator),
        punctuation = json_string(&rules.punctuation),
        identifier = json_string(&rules.identifier),
    )
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

/// Tree-sitter lexical highlighter: the token classes of the lexical
/// structure, not the `orna.ebnf` productions. Braces pair up, so an
/// interpolation keeps nested braces and strings balanced (ORNA-LIT-004). A
/// block comment nests and closes at any run of `*` followed by `/`
/// (ORNA-LEX-004). Reserved words are extracted from identifiers through the
/// `word` rule (ORNA-LEX-007); after `.`, or as a label before `:`, they are
/// contextual names and parse as identifiers.
fn render_tree_sitter_grammar() -> String {
    let render = |re: &Re| re.render(Dialect::TreeSitter);
    let strings = |values: &[&str]| {
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        r#"// Generated from orna-syntax-v1 lexical rules; edit crates/orna-syntax-v1/src/editor.rs instead.
// A lexical highlighter: token classes only, not the orna.ebnf productions.
module.exports = grammar({{
  name: {language},
  extras: $ => [/\s/],
  word: $ => $.identifier,
  rules: {{
    source_file: $ => repeat($._token),
    _token: $ => choice($.comment, $.string, $.block, $.member, $.label, $.keyword, $.number, $.operator, $.punctuation, $.identifier),
    block: $ => seq("{{", repeat($._token), "}}"),
    member: $ => prec(1, seq(".", choice($.identifier, alias($.keyword, $.identifier)))),
    label: $ => prec(1, seq(alias($._label_keyword, $.identifier), ":")),
    _label_keyword: _ => prec(1, choice({label_keywords})),
    comment: $ => choice($.line_comment, $.block_comment),
    line_comment: _ => token(prec(1, seq({line_comment}, /[^\r\n]*/))),
    block_comment: $ => seq(/\/+\*/, repeat(choice(/[^*/]+/, /\*+[^*/]/, /\/+[^*/]/, $.block_comment)), /\*+\//),
    string: $ => seq({quote}, repeat(choice($.string_content, $.escape_sequence, $.interpolation)), token.immediate({quote})),
    string_content: _ => token.immediate(prec(1, /[^"\\{{]+/)),
    escape_sequence: _ => token.immediate(/{escape}/),
    interpolation: $ => seq(token.immediate("{{"), repeat($._token), "}}"),
    keyword: _ => choice({keywords}),
    identifier: _ => /{identifier}/,
    number: _ => token(/{number}/),
    operator: _ => token(choice({operators})),
    punctuation: _ => choice({punctuation})
  }}
}});
"#,
        language = json_string(LANGUAGE_ID),
        label_keywords = strings(&lexical::label_keywords()),
        line_comment = json_string(LINE_COMMENT_START),
        quote = json_string(&STRING_DELIMITER.to_string()),
        escape = render(&lexical::escape_sequence()),
        keywords = strings(&lexical::keywords()),
        identifier = render(&lexical::identifier()),
        number = render(&lexical::number()),
        operators = strings(OPERATORS),
        punctuation = strings(&lexical::unpaired_punctuation()),
    )
}

fn render_tree_sitter_query() -> String {
    let captures = TOKEN_PRESENTATIONS
        .iter()
        .map(|item| format!("({}) @{}", class_name(item.class), capture_name(item.class)))
        .chain([
            "(escape_sequence) @string.escape".to_owned(),
            format!(
                "[\"{{\" \"}}\" \".\" \":\"] @{}",
                capture_name(TokenClass::Punctuation)
            ),
        ])
        .collect::<Vec<_>>()
        .join("\n");
    format!("; Generated from the orna-syntax-v1 lexical classes.\n{captures}\n")
}

/// Vim syntax over the shared lexical model in very-magic form. Where two
/// items start in the same column the one defined later wins, so contextual
/// names, strings and comments come after the token matches they override.
fn render_vim() -> String {
    let render = |re: &Re| re.render(Dialect::VimVeryMagic);
    let keywords = render(&lexical::spellings(&lexical::keywords()));
    let labels = render(&lexical::spellings(&lexical::label_keywords()));
    let dot = render(&Re::text("."));
    let mut lines = vec![
        "\" Generated from orna-syntax-v1; edit crates/orna-syntax-v1/src/editor.rs instead."
            .to_owned(),
        "if exists(\"b:current_syntax\") | finish | endif".to_owned(),
        "syntax case match".to_owned(),
        vim_match(
            "ornaIdentifier",
            // No word-end anchor: a combining mark ends no Vim word, and the
            // greedy match already stops at the first non-identifier byte.
            &format!(r"\v<{}", render(&lexical::identifier())),
            "",
        ),
        format!(
            "syntax keyword ornaKeyword {}",
            lexical::keywords().join(" ")
        ),
        vim_match(
            "ornaPunctuation",
            &format!(r"\v{}", render(&lexical::punctuation())),
            "",
        ),
        vim_match(
            "ornaOperator",
            &format!(r"\v{}", render(&lexical::operators())),
            "",
        ),
        vim_match(
            "ornaNumber",
            &format!(r"\v<{}>", render(&lexical::number())),
            "",
        ),
        // A reserved word after a single `.`, or labelling a record field or
        // named argument, is a contextual name. These matches start at the
        // `.` or the delimiter, ahead of the keyword item they replace.
        vim_match(
            "ornaContextualName",
            &format!(r"\v%({dot})@<!{dot}{keywords}>"),
            " contains=ornaPunctuation",
        ),
        vim_match(
            "ornaContextualName",
            &format!(r"\v%(^\s+|[(,{{]\s*){labels}>\ze\s*:"),
            " contains=ornaPunctuation",
        ),
        format!(
            "syntax region ornaString start=+{quote}+ skip=+\\\\.+ end=+{quote}+ contains=ornaEscape,ornaInterpolation",
            quote = STRING_DELIMITER
        ),
        vim_match(
            "ornaEscape",
            &format!(r"\v{}", render(&lexical::escape_sequence())),
            " contained",
        ),
        "syntax region ornaInterpolation matchgroup=ornaPunctuation start=+{+ end=+}+ contained contains=@ornaExpression".to_owned(),
        "syntax region ornaBraces matchgroup=ornaPunctuation start=+{+ end=+}+ contained contains=@ornaExpression".to_owned(),
        "syntax cluster ornaExpression contains=ornaIdentifier,ornaKeyword,ornaPunctuation,ornaOperator,ornaNumber,ornaContextualName,ornaString,ornaBraces,ornaLineComment,ornaBlockComment".to_owned(),
        vim_match(
            "ornaLineComment",
            &format!(r"\v{}.*$", render(&Re::text(LINE_COMMENT_START))),
            "",
        ),
        format!(
            "syntax region ornaBlockComment start=+\\v{}+ end=+\\v{}+ contains=ornaBlockComment",
            render(&Re::text(BLOCK_COMMENT_START)),
            render(&Re::text(BLOCK_COMMENT_END))
        ),
    ];
    for (group, face) in [
        ("ornaKeyword", "Statement"),
        ("ornaString", "String"),
        ("ornaEscape", "SpecialChar"),
        ("ornaLineComment", "Comment"),
        ("ornaBlockComment", "Comment"),
        ("ornaNumber", "Number"),
        ("ornaOperator", "Operator"),
        ("ornaPunctuation", "Delimiter"),
        ("ornaIdentifier", "Identifier"),
        ("ornaContextualName", "Identifier"),
    ] {
        lines.push(format!("hi def link {group} {face}"));
    }
    lines.push("let b:current_syntax = \"orna\"".to_owned());
    lines.join("\n") + "\n"
}

/// One `syntax match` line, delimited by a character the pattern lacks.
fn vim_match(group: &str, pattern: &str, suffix: &str) -> String {
    let delimiter = ['/', '+', '#', '!', '~', ';', '&', '=']
        .into_iter()
        .find(|candidate| !pattern.contains(*candidate))
        .expect("a Vim pattern leaves one delimiter free");
    format!("syntax match {group} {delimiter}{pattern}{delimiter}{suffix}")
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

/// Emacs support code shared by every generated `orna-mode`.
///
/// The syntax table makes `//` a line comment and `/* */` a nesting block
/// comment (ORNA-LEX-003/004), so comment and string text is fontified
/// syntactically before any token rule runs. A string nested in an
/// interpolation would otherwise end the enclosing string at its first quote;
/// `orna-syntax-propertize` gives such quotes punctuation syntax, and the
/// interpolation matcher re-highlights the code between the braces
/// (ORNA-LIT-004).
const EMACS_SUPPORT: &str = r#"(defvar orna-punctuation-face
  (if (facep 'font-lock-delimiter-face) 'font-lock-delimiter-face 'default)
  "Face for Orna punctuation; `font-lock-delimiter-face' needs Emacs 29.")
(defvar orna-mode-syntax-table
  (let ((table (make-syntax-table)))
    (modify-syntax-entry ?/ ". 124b" table)
    (modify-syntax-entry ?* ". 23n" table)
    (modify-syntax-entry ?\n "> b" table)
    (modify-syntax-entry ?\" "\"" table)
    (modify-syntax-entry ?\\ "\\" table)
    (modify-syntax-entry ?_ "w" table)
    (dolist (char '(?+ ?- ?= ?< ?> ?! ?? ?| ?& ?% ?^ ?. ?, ?\; ?:))
      (modify-syntax-entry char "." table))
    table)
  "Syntax table for `orna-mode': line and nesting block comments, strings.")
(defconst orna--nested-quote-syntax (string-to-syntax ".")
  "Syntax of a quote that delimits a string nested in an interpolation.")
(defvar orna--mark-nested-quotes nil
  "Non-nil while `orna-syntax-propertize' scans strings.")
(defun orna--skip-block-comment ()
  "Move past the nesting block comment that starts at point."
  (forward-char 2)
  (let ((depth 1))
    (while (and (> depth 0) (not (eobp)))
      (cond ((looking-at "/\\*") (setq depth (1+ depth)) (forward-char 2))
            ((looking-at "\\*/") (setq depth (1- depth)) (forward-char 2))
            (t (forward-char 1))))))
(defun orna--mark-nested-quote ()
  "Give the quote at point punctuation syntax while propertizing."
  (when orna--mark-nested-quotes
    (put-text-property (point) (1+ (point)) 'syntax-table orna--nested-quote-syntax)))
(defun orna--scan-string (nested)
  "Move past the string whose opening quote is at point.
NESTED means the string sits inside an interpolation."
  (when nested (orna--mark-nested-quote))
  (forward-char 1)
  (catch 'done
    (while (not (eobp))
      (let ((char (char-after)))
        (cond ((looking-at "\\\\u{")
               (skip-chars-forward "^}")
               (unless (eobp) (forward-char 1)))
              ((eq char ?\\) (forward-char (min 2 (- (point-max) (point)))))
              ((eq char ?\")
               (when nested (orna--mark-nested-quote))
               (forward-char 1)
               (throw 'done t))
              ((eq char ?{) (forward-char 1) (orna--scan-interpolation))
              (t (forward-char 1)))))))
(defun orna--scan-interpolation ()
  "Move past the interpolation code at point and its closing brace."
  (let ((depth 0))
    (catch 'done
      (while (not (eobp))
        (let ((char (char-after)))
          (cond ((eq char ?\") (orna--scan-string t))
                ((looking-at "//") (forward-line 1))
                ((looking-at "/\\*") (orna--skip-block-comment))
                ((eq char ?{) (setq depth (1+ depth)) (forward-char 1))
                ((eq char ?})
                 (forward-char 1)
                 (if (= depth 0) (throw 'done t) (setq depth (1- depth))))
                (t (forward-char 1))))))))
(defun orna-syntax-propertize (start end)
  "Keep quotes of strings nested in interpolations within the region
from ending the enclosing string."
  (goto-char start)
  (let ((state (syntax-ppss)))
    (when (nth 3 state) (goto-char (nth 8 state))))
  (let ((orna--mark-nested-quotes t))
    (while (re-search-forward "\"" end t)
      (let ((open (match-beginning 0)))
        (unless (nth 8 (save-excursion (syntax-ppss open)))
          (goto-char open)
          (orna--scan-string nil))))))
(defvar orna--interpolation-start nil
  "Start of the interpolation code `orna--match-interpolation' found.")
(defvar orna--interpolation-end nil
  "End of the interpolation code `orna--match-interpolation' found.")
(defun orna--escaped-brace-p (open)
  "Non-nil when the brace at OPEN belongs to an escape such as `\\u{7b}'."
  (save-excursion
    (goto-char open)
    (when (eq (char-before) ?u) (backward-char))
    (= 1 (% (- (skip-chars-backward "\\\\")) 2))))
(defun orna--match-interpolation (limit)
  "Match the opening brace of the next string interpolation before limit."
  (let (found)
    (while (and (not found) (re-search-forward "{" limit t))
      (let ((open (match-beginning 0)))
        (when (and (nth 3 (save-excursion (syntax-ppss open)))
                   (not (orna--escaped-brace-p open)))
          (save-excursion
            (goto-char (1+ open))
            (orna--scan-interpolation)
            (setq orna--interpolation-start (1+ open)
                  orna--interpolation-end (point)))
          (set-match-data (list open (1+ open)))
          (setq found t))))
    found))
"#;

/// Emacs major mode with Eglot attachment. Faces are quoted symbols, and
/// `font-lock-delimiter-face` falls back to `default` before Emacs 29.
fn render_emacs() -> String {
    let render = |re: &Re| re.render(Dialect::Emacs);
    let keywords = render(&lexical::spellings(&lexical::keywords()));
    let labels = render(&lexical::spellings(&lexical::label_keywords()));
    let keyword_list = lexical::keywords()
        .iter()
        .map(|word| json_string(word))
        .collect::<Vec<_>>()
        .join(" ");
    let keyword = format!("(regexp-opt '({keyword_list}) 'words)");
    // A reserved word after a single `.`, or labelling a record field or
    // named argument, is a contextual name ("Contextual names and
    // construction"); these rules run before the keyword rule.
    let member = elisp_string(&format!(r"\(?:^\|[^.]\)\.\({keywords}\)\_>"));
    let label = elisp_string(&format!(r"\_<\({labels}\)[[:blank:]]*:"));
    let number = elisp_string(&format!(r"\_<{}\_>", render(&lexical::number())));
    let identifier = elisp_string(&format!(r"\_<{}", render(&lexical::identifier())));
    let operator = elisp_string(&render(&lexical::operators()));
    let punctuation = elisp_string(&render(&lexical::punctuation()));
    let nested_string = elisp_string(r#""\(?:\\.\|[^"\\]\)*""#);
    let face = |class| emacs_face(class);
    let top_level = [
        format!("({member} 1 {})", face(TokenClass::Identifier)),
        format!("({label} 1 {})", face(TokenClass::Identifier)),
        format!("(,{keyword} . {})", face(TokenClass::Keyword)),
        format!("({number} . {})", face(TokenClass::Number)),
        format!("({identifier} . {})", face(TokenClass::Identifier)),
        format!("({operator} . {})", face(TokenClass::Operator)),
        format!("({punctuation} . {})", face(TokenClass::Punctuation)),
    ];
    // Inside an interpolation the string face is replaced token by token;
    // later rules override earlier ones, and nested strings come last.
    let interpolation = [
        (identifier.clone(), 0, face(TokenClass::Identifier)),
        (number.clone(), 0, face(TokenClass::Number)),
        (format!(",{keyword}"), 0, face(TokenClass::Keyword)),
        (member.clone(), 1, face(TokenClass::Identifier)),
        (label.clone(), 1, face(TokenClass::Identifier)),
        (operator.clone(), 0, face(TokenClass::Operator)),
        (punctuation.clone(), 0, face(TokenClass::Punctuation)),
        (nested_string, 0, face(TokenClass::String)),
    ]
    .into_iter()
    .map(|(pattern, group, face)| {
        format!(
            "({pattern} orna--interpolation-end (goto-char orna--interpolation-start) ({group} {face} t))"
        )
    })
    .collect::<Vec<_>>()
    .join("\n     ");
    let rules = top_level.join("\n    ");
    let comment_start = json_string(&format!("{LINE_COMMENT_START} "));
    let extension_pattern = elisp_string(&format!(r"\.{SOURCE_EXTENSION}\'"));
    format!(
        r#";;; orna-eglot.el --- Orna {LANGUAGE_VERSION} lexical highlighting -*- lexical-binding: t; -*-
;; Generated from orna-syntax-v1; edit crates/orna-syntax-v1/src/editor.rs instead.
(require 'eglot)
(defvar orna-eglot-server-command '("orna-lsp")
  "Command used by Eglot to start the Orna language server.")
{EMACS_SUPPORT}(defvar orna-font-lock-keywords
  `(
    {rules}
    (orna--match-interpolation
     (0 orna-punctuation-face t)
     {interpolation})
  )
  "Lexical highlighting generated from the v1 lexer.")
(define-derived-mode orna-mode prog-mode "Orna"
  "Major mode for Orna source files."
  (setq-local comment-start {comment_start})
  (setq-local comment-end "")
  (setq-local syntax-propertize-function #'orna-syntax-propertize)
  (setq-local font-lock-defaults '(orna-font-lock-keywords nil nil)))
(add-to-list 'auto-mode-alist '({extension_pattern} . orna-mode))
(defun orna-setup-eglot ()
  "Attach Orna buffers to the configured language server with Eglot."
  (add-to-list 'eglot-server-programs
               (cons '(orna-mode) orna-eglot-server-command))
  (add-hook 'orna-mode-hook #'eglot-ensure))
(provide 'orna-eglot)
"#
    )
}

/// The face form for one lexical class. Font-lock evaluates the form, so each
/// face is a quoted symbol; punctuation names the variable that guards
/// `font-lock-delimiter-face`.
fn emacs_face(class: TokenClass) -> &'static str {
    match class {
        TokenClass::Keyword => "'font-lock-keyword-face",
        TokenClass::Identifier => "'font-lock-variable-name-face",
        TokenClass::Number => "'font-lock-constant-face",
        TokenClass::String => "'font-lock-string-face",
        TokenClass::Comment => "'font-lock-comment-face",
        TokenClass::Operator => "'font-lock-builtin-face",
        TokenClass::Punctuation => "orna-punctuation-face",
    }
}

/// An Emacs Lisp string literal.
fn elisp_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('"');
    out
}

/// Sublime Text syntax with real `push`/`pop` contexts. Regexes are written
/// in single-backslash form inside YAML single quotes, where a backslash is
/// literal.
fn render_sublime() -> String {
    let rules = OnigurumaRules::new();
    let scope = |class| yaml_string(presentation(class).textmate_scope);
    format!(
        r#"%YAML 1.2
---
# Generated from orna-syntax-v1; edit crates/orna-syntax-v1/src/editor.rs instead.
name: Orna
file_extensions: [{SOURCE_EXTENSION}]
scope: {SOURCE_SCOPE}
contexts:
  main:
    - include: comments
    - include: strings
    - include: tokens
  comments:
    - match: {line_comment}
      scope: {comment_scope}
    - match: {block_start}
      push: block_comment
  block_comment:
    - meta_scope: {comment_scope}
    - match: {block_start}
      push: block_comment
    - match: {block_end}
      pop: true
  strings:
    - match: {quote}
      push: string
  string:
    - meta_scope: {string_scope}
    - match: {quote}
      pop: true
    - match: {escape}
      scope: 'constant.character.escape.orna'
    - match: {open_brace}
      scope: {punctuation_scope}
      push: interpolation
  interpolation:
    - clear_scopes: 1
    - meta_scope: 'meta.interpolation.orna'
    - match: {close_brace}
      scope: {punctuation_scope}
      pop: true
    - include: expressions
  braces:
    - match: {close_brace}
      scope: {punctuation_scope}
      pop: true
    - include: expressions
  expressions:
    - include: comments
    - include: strings
    - match: {open_brace}
      scope: {punctuation_scope}
      push: braces
    - include: tokens
  tokens:
    - match: {contextual_name}
      scope: {identifier_scope}
    - match: {keyword}
      scope: {keyword_scope}
    - match: {number}
      scope: {number_scope}
    - match: {operator}
      scope: {operator_scope}
    - match: {punctuation}
      scope: {punctuation_scope}
    - match: {identifier}
      scope: {identifier_scope}
"#,
        line_comment = yaml_string(&rules.line_comment),
        comment_scope = scope(TokenClass::Comment),
        block_start = yaml_string(&rules.block_comment_start),
        block_end = yaml_string(&rules.block_comment_end),
        quote = yaml_string(&rules.quote),
        string_scope = scope(TokenClass::String),
        escape = yaml_string(&rules.escape),
        open_brace = yaml_string(&rules.open_brace),
        close_brace = yaml_string(&rules.close_brace),
        punctuation_scope = scope(TokenClass::Punctuation),
        contextual_name = yaml_string(&rules.contextual_name),
        identifier_scope = scope(TokenClass::Identifier),
        keyword = yaml_string(&rules.keyword),
        keyword_scope = scope(TokenClass::Keyword),
        number = yaml_string(&rules.number),
        number_scope = scope(TokenClass::Number),
        operator = yaml_string(&rules.operator),
        operator_scope = scope(TokenClass::Operator),
        punctuation = yaml_string(&rules.punctuation),
        identifier = yaml_string(&rules.identifier),
    )
}

fn yaml_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
