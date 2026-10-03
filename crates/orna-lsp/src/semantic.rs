//! Semantic token projection from the frozen 1.0.0 editor lexer.

use lsp_types::{Range, SemanticToken, SemanticTokenType};
use orna_syntax_v1::{SyntaxSpan, editor};

use crate::documents::PositionMapper;

/// LSP token legend supported by this server.
pub fn legend() -> Vec<SemanticTokenType> {
    editor::semantic_token_types()
        .map(SemanticTokenType::new)
        .collect()
}

/// Returns delta-encoded tokens projected from the v1 editor token classes.
pub fn semantic_tokens(
    text: &str,
    mapper: &PositionMapper<'_>,
    range: Option<&Range>,
) -> Vec<SemanticToken> {
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in editor::highlight(text) {
        let Some(token_type) = editor::semantic_token_index(token.class) else {
            continue;
        };
        let span = SyntaxSpan::new(token.range.start, token.range.end);
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
                token_type: token_type as u32,
                token_modifiers_bitset: 0,
            });
            previous_line = line;
            previous_start = start;
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = include_str!("../tests/fixtures/editor-semantic-tokens.orna");

    #[test]
    fn legend_and_emitted_tokens_follow_v1_editor_classes() {
        assert_eq!(
            legend(),
            [
                "keyword", "variable", "number", "string", "comment", "operator"
            ]
            .into_iter()
            .map(SemanticTokenType::new)
            .collect::<Vec<_>>()
        );

        let expected = editor::highlight(SOURCE)
            .into_iter()
            .filter_map(|token| editor::semantic_token_index(token.class))
            .map(|index| index as u32)
            .collect::<Vec<_>>();
        let mapper = PositionMapper::new(SOURCE);
        let actual = semantic_tokens(SOURCE, &mapper, None)
            .into_iter()
            .map(|token| token.token_type)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}
