//! Semantic token computation from the Orna highlight classifier.
//!
//! The server advertises a fixed legend. The classifier output maps directly
//! onto it; punctuation is skipped because editors style it through their
//! own grammar.

use lsp_types::{Range, SemanticToken, SemanticTokenType};
use orna_syntax::{HighlightKind, Parse};

use crate::documents::PositionMapper;

/// The semantic token legend in legend-index order.
pub const LEGEND: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,
    SemanticTokenType::TYPE,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::NAMESPACE,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::STRING,
    SemanticTokenType::NUMBER,
    SemanticTokenType::COMMENT,
    SemanticTokenType::OPERATOR,
];

/// Maps one classifier kind to its legend index.
fn legend_index(kind: HighlightKind) -> Option<usize> {
    let index = match kind {
        HighlightKind::Keyword => 0,
        HighlightKind::TypeName => 1,
        HighlightKind::FunctionName => 2,
        HighlightKind::VariableName => 3,
        HighlightKind::NamespaceName => 4,
        HighlightKind::PropertyName => 5,
        HighlightKind::StringLiteral => 6,
        HighlightKind::NumberLiteral => 7,
        HighlightKind::Comment => 8,
        HighlightKind::Operator => 9,
        HighlightKind::Punctuation | HighlightKind::QuotedIdentifier => return None,
    };
    Some(index)
}

/// Returns the delta-encoded semantic tokens for one document.
///
/// When `range` is present, only token segments that intersect the range are
/// included, matching the `textDocument/semanticTokens/range` contract.
pub fn semantic_tokens(
    parse: &Parse,
    mapper: &PositionMapper<'_>,
    range: Option<&Range>,
) -> Vec<SemanticToken> {
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in parse.highlight() {
        let span = orna_syntax::SourceSpan {
            start: token.range.start,
            end: token.range.end,
        };
        let kind = if matches!(
            token.kind,
            orna_syntax::HighlightKind::PropertyName | orna_syntax::HighlightKind::QuotedIdentifier
        ) && crate::analysis::is_client_target_function_span(parse, &span)
        {
            orna_syntax::HighlightKind::FunctionName
        } else {
            token.kind
        };
        let Some(index) = legend_index(kind) else {
            continue;
        };
        for (position, length) in mapper.segments(&span) {
            if length == 0 {
                continue;
            }
            if let Some(range) = range {
                let segment_end = lsp_types::Position {
                    line: position.line,
                    character: position.character.saturating_add(length),
                };
                if segment_end <= range.start || position >= range.end {
                    continue;
                }
            }
            let line = position.line;
            let start = position.character;
            let (delta_line, delta_start) = if data.is_empty() {
                (line, start)
            } else if line == previous_line {
                (0, start - previous_start)
            } else {
                (line - previous_line, start)
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type: index as u32,
                token_modifiers_bitset: 0,
            });
            previous_line = line;
            previous_start = start;
        }
    }
    data
}
