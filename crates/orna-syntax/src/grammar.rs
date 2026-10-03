//! Shared lexical and editor-presentation metadata for Orna syntax.
//!
//! The parser/highlighter remains the authority for accepted syntax and
//! contextual names. This module exposes its token vocabulary and presentation
//! map so editor artifacts and the LSP legend are derived from the same data.

use crate::{HighlightKind, KEYWORDS, SCALAR_TYPES};

/// Source comments beginning with this sequence run through the line ending.
pub const LINE_COMMENT_START: &str = "--";
/// Source block comments begin with this sequence.
pub const BLOCK_COMMENT_START: &str = "/*";
/// Source block comments end with this sequence.
pub const BLOCK_COMMENT_END: &str = "*/";
/// Single-quoted source strings use doubled delimiters to escape a quote.
pub const STRING_DELIMITER: char = '\'';
/// Double-quoted source identifiers use doubled delimiters to escape a quote.
pub const QUOTED_IDENTIFIER_DELIMITER: char = '"';

/// The language identifier used by editor packages and LSP clients.
pub const LANGUAGE_ID: &str = "orna";
/// The extension used for Orna source documents.
pub const SOURCE_EXTENSION: &str = "orna";
/// The editor package version, kept in step with this syntax crate.
pub const EDITOR_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The TextMate grammar scope shared by generated editor packages.
pub const TEXTMATE_SCOPE: &str = "source.orna";
/// Character pairs that editors may auto-close and surround.
pub const BRACKET_PAIRS: &[(&str, &str)] = &[("(", ")"), ("[", "]"), ("{", "}")];
/// TextMate fallback pattern for integer literals accepted by the lexer.
pub const NUMBER_PATTERN: &str = r"\b[0-9]+\b";
/// TextMate fallback pattern for identifiers accepted by the lexer.
pub const IDENTIFIER_PATTERN: &str = r"[\p{L}_][\p{L}\p{N}_]*";

/// Operator spellings classified by `orna-syntax`.
pub const OPERATORS: &[&str] = &[
    ":=", "=>", "=", "<>", "!=", "<", ">", "<=", ">=", "+", "-", "*", "/", "%", "||", "->", ":",
    "?",
];

/// Punctuation spellings emitted by the source highlighter as punctuation.
pub const PUNCTUATION: &[&str] = &["(", ")", ",", ";", ".", "[", "]", "{", "}"];

/// One `HighlightKind` presentation shared by semantic tokens and editor grammars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPresentation {
    /// The syntax classifier result represented by this entry.
    pub kind: HighlightKind,
    /// The TextMate scope used by the generated fallback grammar.
    pub textmate_scope: &'static str,
    /// The LSP semantic-token type, or `None` when the LSP intentionally omits it.
    pub semantic_token_type: Option<&'static str>,
}

/// Presentation metadata for every syntax classification.
pub const TOKEN_PRESENTATIONS: &[TokenPresentation] = &[
    TokenPresentation {
        kind: HighlightKind::Keyword,
        textmate_scope: "keyword.control.orna",
        semantic_token_type: Some("keyword"),
    },
    TokenPresentation {
        kind: HighlightKind::TypeName,
        textmate_scope: "storage.type.orna",
        semantic_token_type: Some("type"),
    },
    TokenPresentation {
        kind: HighlightKind::FunctionName,
        textmate_scope: "entity.name.function.orna",
        semantic_token_type: Some("function"),
    },
    TokenPresentation {
        kind: HighlightKind::VariableName,
        textmate_scope: "variable.other.orna",
        semantic_token_type: Some("variable"),
    },
    TokenPresentation {
        kind: HighlightKind::NamespaceName,
        textmate_scope: "entity.name.namespace.orna",
        semantic_token_type: Some("namespace"),
    },
    TokenPresentation {
        kind: HighlightKind::PropertyName,
        textmate_scope: "variable.other.property.orna",
        semantic_token_type: Some("property"),
    },
    TokenPresentation {
        kind: HighlightKind::StringLiteral,
        textmate_scope: "string.quoted.single.orna",
        semantic_token_type: Some("string"),
    },
    TokenPresentation {
        kind: HighlightKind::NumberLiteral,
        textmate_scope: "constant.numeric.orna",
        semantic_token_type: Some("number"),
    },
    TokenPresentation {
        kind: HighlightKind::Comment,
        textmate_scope: "comment.orna",
        semantic_token_type: Some("comment"),
    },
    TokenPresentation {
        kind: HighlightKind::Operator,
        textmate_scope: "keyword.operator.orna",
        semantic_token_type: Some("operator"),
    },
    TokenPresentation {
        kind: HighlightKind::Punctuation,
        textmate_scope: "punctuation.orna",
        semantic_token_type: None,
    },
    TokenPresentation {
        kind: HighlightKind::QuotedIdentifier,
        textmate_scope: "entity.name.quoted.orna",
        semantic_token_type: None,
    },
];

/// The semantic-token legend in exactly the order used for token indices.
pub fn semantic_token_types() -> impl Iterator<Item = &'static str> {
    TOKEN_PRESENTATIONS
        .iter()
        .filter_map(|presentation| presentation.semantic_token_type)
}

/// Return the LSP legend index for one syntax classification.
pub fn semantic_token_index(kind: HighlightKind) -> Option<usize> {
    let mut index = 0;
    for presentation in TOKEN_PRESENTATIONS {
        if presentation.semantic_token_type.is_some() {
            if presentation.kind == kind {
                return Some(index);
            }
            index += 1;
        } else if presentation.kind == kind {
            return None;
        }
    }
    None
}

/// One checked-in artifact rendered from this grammar metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedEditorArtifact {
    /// Repository-relative output path.
    pub path: &'static str,
    /// Deterministic generated contents.
    pub contents: String,
}

/// Render the TextMate grammar consumed by editors without semantic tokens.
pub fn render_textmate_grammar() -> String {
    let keyword_scope = scope_for(HighlightKind::Keyword);
    let type_scope = scope_for(HighlightKind::TypeName);
    let variable_scope = scope_for(HighlightKind::VariableName);
    let string_scope = scope_for(HighlightKind::StringLiteral);
    let number_scope = scope_for(HighlightKind::NumberLiteral);
    let comment_scope = scope_for(HighlightKind::Comment);
    let operator_scope = scope_for(HighlightKind::Operator);
    let punctuation_scope = scope_for(HighlightKind::Punctuation);
    let quoted_identifier_scope = scope_for(HighlightKind::QuotedIdentifier);

    render_template(
        TEXTMATE_TEMPLATE,
        &[
            ("@@LANGUAGE_NAME@@", json_string("Orna")),
            ("@@TEXTMATE_SCOPE@@", json_string(TEXTMATE_SCOPE)),
            ("@@SOURCE_EXTENSION@@", json_string(SOURCE_EXTENSION)),
            (
                "@@LINE_COMMENT_PATTERN@@",
                json_string(&format!("{}[^\\n]*", regex_escape(LINE_COMMENT_START))),
            ),
            ("@@COMMENT_SCOPE@@", json_string(comment_scope)),
            (
                "@@BLOCK_COMMENT_START@@",
                json_string(&regex_escape(BLOCK_COMMENT_START)),
            ),
            (
                "@@BLOCK_COMMENT_END@@",
                json_string(&regex_escape(BLOCK_COMMENT_END)),
            ),
            (
                "@@STRING_PATTERN@@",
                json_string(&quoted_delimited_pattern(STRING_DELIMITER)),
            ),
            ("@@STRING_SCOPE@@", json_string(string_scope)),
            (
                "@@QUOTED_IDENTIFIER_PATTERN@@",
                json_string(&quoted_delimited_pattern(QUOTED_IDENTIFIER_DELIMITER)),
            ),
            (
                "@@QUOTED_IDENTIFIER_SCOPE@@",
                json_string(quoted_identifier_scope),
            ),
            (
                "@@SCALAR_PATTERN@@",
                json_string(&regex_alternation(SCALAR_TYPES)),
            ),
            ("@@TYPE_SCOPE@@", json_string(type_scope)),
            (
                "@@KEYWORD_PATTERN@@",
                json_string(&regex_alternation(KEYWORDS)),
            ),
            ("@@KEYWORD_SCOPE@@", json_string(keyword_scope)),
            ("@@NUMBER_PATTERN@@", json_string(NUMBER_PATTERN)),
            ("@@NUMBER_SCOPE@@", json_string(number_scope)),
            (
                "@@OPERATOR_PATTERN@@",
                json_string(&regex_alternation_without_boundaries(OPERATORS)),
            ),
            ("@@OPERATOR_SCOPE@@", json_string(operator_scope)),
            (
                "@@PUNCTUATION_PATTERN@@",
                json_string(&regex_alternation_without_boundaries(PUNCTUATION)),
            ),
            ("@@PUNCTUATION_SCOPE@@", json_string(punctuation_scope)),
            ("@@IDENTIFIER_SCOPE@@", json_string(variable_scope)),
            ("@@IDENTIFIER_PATTERN@@", json_string(IDENTIFIER_PATTERN)),
        ],
    )
}

/// Render the LSP legend as a portable artifact for editor integrations.
pub fn render_semantic_token_legend() -> String {
    let token_types = semantic_token_types()
        .map(json_string)
        .map(|token_type| format!("    {token_type}"))
        .collect::<Vec<_>>()
        .join(",\n");
    let highlight_kinds = TOKEN_PRESENTATIONS
        .iter()
        .map(|presentation| {
            let semantic_type = presentation
                .semantic_token_type
                .map(json_string)
                .unwrap_or_else(|| "null".to_owned());
            format!(
                "    {{ \"kind\": {}, \"semanticTokenType\": {}, \"textmateScope\": {} }}",
                json_string(&format!("{:?}", presentation.kind)),
                semantic_type,
                json_string(presentation.textmate_scope),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    render_template(
        SEMANTIC_TOKEN_TEMPLATE,
        &[
            ("@@LANGUAGE_ID@@", json_string(LANGUAGE_ID)),
            ("@@TOKEN_TYPES@@", token_types),
            ("@@HIGHLIGHT_KINDS@@", highlight_kinds),
        ],
    )
}

/// Render file association and comment/bracket configuration for VS Code.
pub fn render_vscode_artifacts() -> (String, String) {
    let package = render_template(
        VSCODE_PACKAGE_TEMPLATE,
        &[
            ("@@LANGUAGE_ID@@", json_string(LANGUAGE_ID)),
            ("@@LANGUAGE_NAME@@", json_string("Orna")),
            ("@@PACKAGE_VERSION@@", json_string(EDITOR_PACKAGE_VERSION)),
            (
                "@@SOURCE_EXTENSION@@",
                json_string(&format!(".{SOURCE_EXTENSION}")),
            ),
            ("@@TEXTMATE_SCOPE@@", json_string(TEXTMATE_SCOPE)),
        ],
    );
    let brackets = BRACKET_PAIRS
        .iter()
        .map(|(open, close)| format!("      [{}, {}]", json_string(open), json_string(close)))
        .collect::<Vec<_>>()
        .join(",\n");
    let mut auto_closing = BRACKET_PAIRS
        .iter()
        .map(|(open, close)| {
            format!(
                "      {{ \"open\": {}, \"close\": {} }}",
                json_string(open),
                json_string(close)
            )
        })
        .collect::<Vec<_>>();
    for delimiter in [STRING_DELIMITER, QUOTED_IDENTIFIER_DELIMITER] {
        let delimiter = delimiter.to_string();
        auto_closing.push(format!(
            "      {{ \"open\": {}, \"close\": {}, \"notIn\": [\"string\", \"comment\"] }}",
            json_string(&delimiter),
            json_string(&delimiter),
        ));
    }
    let auto_closing = auto_closing.join(",\n");
    let mut surrounding_pairs = BRACKET_PAIRS
        .iter()
        .map(|(open, close)| format!("      [{}, {}]", json_string(open), json_string(close)))
        .collect::<Vec<_>>();
    for delimiter in [STRING_DELIMITER, QUOTED_IDENTIFIER_DELIMITER] {
        let delimiter = json_string(&delimiter.to_string());
        surrounding_pairs.push(format!("      [{delimiter}, {delimiter}]"));
    }
    let surrounding_pairs = surrounding_pairs.join(",\n");
    let language_configuration = render_template(
        VSCODE_LANGUAGE_CONFIGURATION_TEMPLATE,
        &[
            ("@@LINE_COMMENT@@", json_string(LINE_COMMENT_START)),
            ("@@BLOCK_COMMENT_START@@", json_string(BLOCK_COMMENT_START)),
            ("@@BLOCK_COMMENT_END@@", json_string(BLOCK_COMMENT_END)),
            ("@@BRACKETS@@", brackets),
            ("@@SURROUNDING_PAIRS@@", surrounding_pairs),
            ("@@AUTO_CLOSING@@", auto_closing),
        ],
    );
    (package, language_configuration)
}

/// Render all checked-in editor artifacts in deterministic order.
pub fn generated_editor_artifacts() -> Vec<GeneratedEditorArtifact> {
    let textmate = render_textmate_grammar();
    let (vscode_package, vscode_configuration) = render_vscode_artifacts();
    vec![
        GeneratedEditorArtifact {
            path: "editors/textmate/orna.tmLanguage.json",
            contents: textmate.clone(),
        },
        GeneratedEditorArtifact {
            path: "editors/vscode/syntaxes/orna.tmLanguage.json",
            contents: textmate,
        },
        GeneratedEditorArtifact {
            path: "editors/vscode/package.json",
            contents: vscode_package,
        },
        GeneratedEditorArtifact {
            path: "editors/vscode/language-configuration.json",
            contents: vscode_configuration,
        },
        GeneratedEditorArtifact {
            path: "editors/semantic-token-legend.json",
            contents: render_semantic_token_legend(),
        },
    ]
}

fn scope_for(kind: HighlightKind) -> &'static str {
    TOKEN_PRESENTATIONS
        .iter()
        .find(|presentation| presentation.kind == kind)
        .expect("every HighlightKind has editor presentation metadata")
        .textmate_scope
}

fn regex_alternation(values: &[&str]) -> String {
    format!(r"(?i:\b(?:{})\b)", regex_alternatives(values).join("|"))
}

fn regex_alternation_without_boundaries(values: &[&str]) -> String {
    format!("(?:{})", regex_alternatives(values).join("|"))
}

fn regex_alternatives(values: &[&str]) -> Vec<String> {
    let mut alternatives = values
        .iter()
        .map(|value| {
            value
                .split_whitespace()
                .map(regex_escape)
                .collect::<Vec<_>>()
                .join(r"\s+")
        })
        .collect::<Vec<_>>();
    alternatives.sort_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
    alternatives
}

fn regex_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if "\\.+*?()|[]{}^$".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn quoted_delimited_pattern(delimiter: char) -> String {
    let delimiter = regex_escape(&delimiter.to_string());
    format!("{delimiter}(?:{delimiter}{delimiter}|[^{delimiter}])*{delimiter}")
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character < '\u{20}' => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn render_template(template: &str, replacements: &[(&str, String)]) -> String {
    let mut rendered = template.to_owned();
    for (placeholder, replacement) in replacements {
        rendered = rendered.replace(placeholder, replacement);
    }
    debug_assert!(
        !rendered.contains("@@"),
        "editor artifact template has an unfilled placeholder"
    );
    rendered
}

const TEXTMATE_TEMPLATE: &str = r##"{
  "$schema": "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
  "name": @@LANGUAGE_NAME@@,
  "scopeName": @@TEXTMATE_SCOPE@@,
  "fileTypes": [@@SOURCE_EXTENSION@@],
  "patterns": [
    { "include": "#comments" },
    { "include": "#strings" },
    { "include": "#quoted-identifiers" },
    { "include": "#scalar-types" },
    { "include": "#keywords" },
    { "include": "#numbers" },
    { "include": "#operators" },
    { "include": "#punctuation" },
    { "include": "#identifiers" }
  ],
  "repository": {
    "comments": {
      "patterns": [
        { "match": @@LINE_COMMENT_PATTERN@@, "name": @@COMMENT_SCOPE@@ },
        { "begin": @@BLOCK_COMMENT_START@@, "end": @@BLOCK_COMMENT_END@@, "name": @@COMMENT_SCOPE@@ }
      ]
    },
    "strings": { "match": @@STRING_PATTERN@@, "name": @@STRING_SCOPE@@ },
    "quoted-identifiers": { "match": @@QUOTED_IDENTIFIER_PATTERN@@, "name": @@QUOTED_IDENTIFIER_SCOPE@@ },
    "scalar-types": { "match": @@SCALAR_PATTERN@@, "name": @@TYPE_SCOPE@@ },
    "keywords": { "match": @@KEYWORD_PATTERN@@, "name": @@KEYWORD_SCOPE@@ },
    "numbers": { "match": @@NUMBER_PATTERN@@, "name": @@NUMBER_SCOPE@@ },
    "operators": { "match": @@OPERATOR_PATTERN@@, "name": @@OPERATOR_SCOPE@@ },
    "punctuation": { "match": @@PUNCTUATION_PATTERN@@, "name": @@PUNCTUATION_SCOPE@@ },
    "identifiers": { "match": @@IDENTIFIER_PATTERN@@, "name": @@IDENTIFIER_SCOPE@@ }
  }
}
"##;

const SEMANTIC_TOKEN_TEMPLATE: &str = r#"{
  "language": @@LANGUAGE_ID@@,
  "tokenTypes": [
@@TOKEN_TYPES@@
  ],
  "tokenModifiers": [],
  "highlightKinds": [
@@HIGHLIGHT_KINDS@@
  ]
}
"#;

const VSCODE_PACKAGE_TEMPLATE: &str = r#"{
  "name": "orna-syntax",
  "displayName": "Orna Syntax",
  "description": "Generated TextMate syntax support for Orna source files",
  "version": @@PACKAGE_VERSION@@,
  "engines": { "vscode": "^1.80.0" },
  "categories": ["Programming Languages"],
  "contributes": {
    "languages": [
      {
        "id": @@LANGUAGE_ID@@,
        "aliases": [@@LANGUAGE_NAME@@],
        "extensions": [@@SOURCE_EXTENSION@@],
        "configuration": "./language-configuration.json"
      }
    ],
    "grammars": [
      {
        "language": @@LANGUAGE_ID@@,
        "scopeName": @@TEXTMATE_SCOPE@@,
        "path": "./syntaxes/orna.tmLanguage.json"
      }
    ]
  }
}
"#;

const VSCODE_LANGUAGE_CONFIGURATION_TEMPLATE: &str = r#"{
  "comments": {
    "lineComment": @@LINE_COMMENT@@,
    "blockComment": [@@BLOCK_COMMENT_START@@, @@BLOCK_COMMENT_END@@]
  },
  "brackets": [
@@BRACKETS@@
  ],
  "autoClosingPairs": [
@@AUTO_CLOSING@@
  ],
  "surroundingPairs": [
@@SURROUNDING_PAIRS@@
  ]
}
"#;

/// The keyword spellings used by the parser's highlighting classifier.
pub const fn keywords() -> &'static [&'static str] {
    KEYWORDS
}

/// The scalar type spellings used by the parser's highlighting classifier.
pub const fn scalar_types() -> &'static [&'static str] {
    SCALAR_TYPES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_token_indices_follow_the_shared_presentation_order() {
        let all_kinds = [
            HighlightKind::Keyword,
            HighlightKind::TypeName,
            HighlightKind::FunctionName,
            HighlightKind::VariableName,
            HighlightKind::NamespaceName,
            HighlightKind::PropertyName,
            HighlightKind::StringLiteral,
            HighlightKind::NumberLiteral,
            HighlightKind::Comment,
            HighlightKind::Operator,
            HighlightKind::Punctuation,
            HighlightKind::QuotedIdentifier,
        ];
        assert_eq!(TOKEN_PRESENTATIONS.len(), all_kinds.len());
        for kind in all_kinds {
            assert_eq!(
                TOKEN_PRESENTATIONS
                    .iter()
                    .filter(|presentation| presentation.kind == kind)
                    .count(),
                1,
                "missing or duplicate presentation for {kind:?}"
            );
        }

        let kinds = TOKEN_PRESENTATIONS
            .iter()
            .filter(|presentation| presentation.semantic_token_type.is_some())
            .collect::<Vec<_>>();
        let names = semantic_token_types().collect::<Vec<_>>();

        assert_eq!(names.len(), kinds.len());
        for (index, presentation) in kinds.into_iter().enumerate() {
            assert_eq!(semantic_token_index(presentation.kind), Some(index));
        }
        assert_eq!(semantic_token_index(HighlightKind::Punctuation), None);
        assert_eq!(semantic_token_index(HighlightKind::QuotedIdentifier), None);
    }
}
