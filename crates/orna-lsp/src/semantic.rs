//! Semantic token computation from the Orna highlight classifier.
//!
//! The server advertises a fixed legend. The classifier output maps directly
//! onto it; punctuation is skipped because editors style it through their
//! own grammar.

use lsp_types::{Range, SemanticToken, SemanticTokenType};
use orna_syntax::SourceSpan;
use orna_syntax_v1::editor::{self, TokenClass};

use crate::documents::PositionMapper;

/// Build the LSP legend from the shared syntax presentation table.
pub fn legend() -> Vec<SemanticTokenType> {
    editor::semantic_token_types()
        .map(SemanticTokenType::new)
        .collect()
}

/// Returns the delta-encoded semantic tokens for one document.
///
/// When `range` is present, only token segments that intersect the range are
/// included, matching the `textDocument/semanticTokens/range` contract.
pub fn semantic_tokens(
    source: &str,
    mapper: &PositionMapper<'_>,
    range: Option<&Range>,
) -> Vec<SemanticToken> {
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in editor::highlight(source) {
        let span = SourceSpan {
            start: token.range.start,
            end: token.range.end,
        };
        let Some(index) = editor::semantic_token_index(token.class) else {
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
