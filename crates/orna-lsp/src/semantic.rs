//! Semantic token projection from the frozen 1.0.0 lexer.

use lsp_types::{Range, SemanticToken, SemanticTokenType};
use orna_syntax_v1::{Parse, SyntaxTree, TokenKind, lex};

use crate::documents::PositionMapper;

/// LSP token legend supported by this server.
pub fn legend() -> Vec<SemanticTokenType> {
    [
        SemanticTokenType::KEYWORD,
        SemanticTokenType::FUNCTION,
        SemanticTokenType::TYPE,
        SemanticTokenType::ENUM,
        SemanticTokenType::VARIABLE,
        SemanticTokenType::STRING,
        SemanticTokenType::NUMBER,
        SemanticTokenType::OPERATOR,
    ]
    .into_iter()
    .collect()
}

/// Returns delta-encoded tokens from `orna-syntax-v1` lexer spans.
pub fn semantic_tokens(
    parse: &Parse<SyntaxTree>,
    text: &str,
    mapper: &PositionMapper<'_>,
    range: Option<&Range>,
) -> Vec<SemanticToken> {
    let Ok(tokens) = lex(text) else {
        return Vec::new();
    };
    let symbols = crate::analysis::declaration_symbols(parse, text);
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for token in tokens {
        let Some(token_type) = token_type(&token.kind, &token.text, &token.span, &symbols) else {
            continue;
        };
        for (position, length) in mapper.segments(&token.span) {
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
                token_type,
                token_modifiers_bitset: 0,
            });
            previous_line = line;
            previous_start = start;
        }
    }
    data
}

fn token_type(
    kind: &TokenKind,
    text: &str,
    span: &orna_syntax_v1::SyntaxSpan,
    symbols: &[crate::analysis::EditorSymbol],
) -> Option<u32> {
    let index = match kind {
        TokenKind::Keyword(_) => 0,
        TokenKind::Identifier { .. } => symbols
            .iter()
            .find(|symbol| symbol.selection == *span)
            .map(|symbol| match symbol.kind {
                crate::analysis::EditorSymbolKind::Function => 1,
                crate::analysis::EditorSymbolKind::Type => 2,
                crate::analysis::EditorSymbolKind::Enum => 3,
                crate::analysis::EditorSymbolKind::Table
                | crate::analysis::EditorSymbolKind::Protocol
                | crate::analysis::EditorSymbolKind::Other => 4,
            })
            .or(Some(4))?,
        TokenKind::String
        | TokenKind::StringStart
        | TokenKind::StringText
        | TokenKind::StringEnd => 5,
        TokenKind::Integer
        | TokenKind::Decimal
        | TokenKind::Float
        | TokenKind::Date
        | TokenKind::Instant => 6,
        TokenKind::Punct(value) if is_operator(value) => 7,
        TokenKind::Punct(_)
        | TokenKind::InterpolationStart
        | TokenKind::InterpolationEnd
        | TokenKind::Eof
        | TokenKind::ReplBinding => return None,
    };
    let _ = text;
    Some(index)
}

fn is_operator(value: &str) -> bool {
    matches!(
        value,
        "!" | "?"
            | "??"
            | "|?"
            | "|"
            | "||"
            | "&&"
            | "=="
            | "!="
            | "<"
            | "<="
            | ">"
            | ">="
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "=>"
            | ".."
            | "..="
    )
}
