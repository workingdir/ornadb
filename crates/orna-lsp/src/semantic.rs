//! Semantic token projection from the frozen 1.0.0 lexer.

use lsp_types::{Range, SemanticToken, SemanticTokenType};
use orna_syntax_v1::{
    BLOCK_COMMENT_END, BLOCK_COMMENT_START, EDITOR_OPERATOR_SPELLINGS, EDITOR_TOKEN_TYPES,
    LINE_COMMENT_START, Parse, SyntaxSpan, SyntaxTree, TokenKind, lex,
};

use crate::documents::PositionMapper;

/// LSP token legend supported by this server.
pub fn legend() -> Vec<SemanticTokenType> {
    EDITOR_TOKEN_TYPES
        .iter()
        .map(|token_type| SemanticTokenType::new(token_type))
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
    let mut projected = tokens
        .iter()
        .filter_map(|token| {
            token_type(&token.kind, &token.text, &token.span, &symbols)
                .map(|token_type| (token.span.clone(), token_type))
        })
        .collect::<Vec<_>>();
    if let Some(comment_type) = EDITOR_TOKEN_TYPES
        .iter()
        .position(|token_type| *token_type == "comment")
    {
        projected.extend(
            comment_spans(text, &tokens)
                .into_iter()
                .map(|span| (span, comment_type as u32)),
        );
    }
    projected.sort_by_key(|(span, _)| span.start);
    let mut data = Vec::new();
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    for (span, token_type) in projected {
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
    EDITOR_OPERATOR_SPELLINGS.contains(&value)
}

fn comment_spans(text: &str, tokens: &[orna_syntax_v1::Token]) -> Vec<SyntaxSpan> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    for token in tokens {
        let start = token.span.start.min(text.len());
        if cursor < start {
            scan_comments(text, cursor, start, &mut spans);
        }
        cursor = cursor.max(token.span.end.min(text.len()));
    }
    if cursor < text.len() {
        scan_comments(text, cursor, text.len(), &mut spans);
    }
    spans
}

fn scan_comments(text: &str, mut at: usize, end: usize, spans: &mut Vec<SyntaxSpan>) {
    while at < end {
        let rest = &text[at..end];
        if rest.starts_with(LINE_COMMENT_START) {
            let comment_start = at;
            at += LINE_COMMENT_START.len();
            while at < end && !matches!(text.as_bytes()[at], b'\n' | b'\r') {
                at += 1;
            }
            spans.push(SyntaxSpan::new(comment_start, at));
        } else if rest.starts_with(BLOCK_COMMENT_START) {
            let comment_start = at;
            at += BLOCK_COMMENT_START.len();
            let mut depth = 1usize;
            while at < end && depth != 0 {
                let remaining = &text[at..end];
                if remaining.starts_with(BLOCK_COMMENT_START) {
                    depth += 1;
                    at += BLOCK_COMMENT_START.len();
                } else if remaining.starts_with(BLOCK_COMMENT_END) {
                    depth -= 1;
                    at += BLOCK_COMMENT_END.len();
                } else {
                    at += remaining.chars().next().map_or(1, char::len_utf8);
                }
            }
            spans.push(SyntaxSpan::new(comment_start, at));
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
}
