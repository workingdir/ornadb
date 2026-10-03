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

/// Operator spellings classified by `orna-syntax`.
pub const OPERATORS: &[&str] = &[
    ":=", "=>", "=", "<>", "!=", "<", ">", "<=", ">=", "+", "-", "*", "/", "%", "||", "->", ":",
    "?",
];

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
