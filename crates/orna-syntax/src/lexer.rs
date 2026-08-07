use std::ops::Range;

use crate::{Diagnostic, SourceSpan, parser::SyntaxKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    Word,
    QuotedIdentifier,
    Dot,
    Semicolon,
    StringLiteral,
    LeftParenthesis,
    RightParenthesis,
    Comma,
    Other,
}

impl TokenKind {
    pub(crate) fn syntax_kind(self) -> SyntaxKind {
        match self {
            Self::Whitespace => SyntaxKind::Whitespace,
            Self::LineComment => SyntaxKind::LineComment,
            Self::BlockComment => SyntaxKind::BlockComment,
            Self::Word => SyntaxKind::Word,
            Self::QuotedIdentifier => SyntaxKind::QuotedIdentifier,
            Self::Dot => SyntaxKind::Dot,
            Self::Semicolon => SyntaxKind::Semicolon,
            Self::StringLiteral => SyntaxKind::StringLiteral,
            Self::LeftParenthesis => SyntaxKind::LeftParenthesis,
            Self::RightParenthesis => SyntaxKind::RightParenthesis,
            Self::Comma => SyntaxKind::Comma,
            Self::Other => SyntaxKind::Other,
        }
    }

    pub(crate) fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Token<'source> {
    pub(crate) kind: TokenKind,
    pub(crate) text: &'source str,
    pub(crate) range: Range<usize>,
}

impl<'source> Token<'source> {
    pub(crate) fn span(&self) -> SourceSpan {
        SourceSpan::from_range(self.range.clone())
    }

    pub(crate) fn is_word(&self, expected: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(expected)
    }

    pub(crate) fn is_identifier(&self) -> bool {
        matches!(self.kind, TokenKind::Word | TokenKind::QuotedIdentifier)
    }

    pub(crate) fn is_kind(&self, kind: TokenKind) -> bool {
        self.kind == kind
    }
}

pub(crate) fn lex(source: &str) -> (Vec<Token<'_>>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut diagnostics = Vec::new();
    let mut offset = 0;

    while offset < source.len() {
        let rest = &source[offset..];
        let (kind, width) = if let Some(character) = rest.chars().next() {
            if character.is_whitespace() {
                let width = rest
                    .char_indices()
                    .find(|(_, current)| !current.is_whitespace())
                    .map_or(rest.len(), |(index, _)| index);
                (TokenKind::Whitespace, width)
            } else if rest.starts_with("--") {
                let width = rest.find('\n').unwrap_or(rest.len());
                (TokenKind::LineComment, width)
            } else if rest.starts_with("/*") {
                match rest.find("*/") {
                    Some(end) => (TokenKind::BlockComment, end + 2),
                    None => {
                        diagnostics.push(Diagnostic {
                            code: "ORNA0002",
                            message: "unterminated block comment".to_owned(),
                            span: SourceSpan {
                                start: offset,
                                end: source.len(),
                            },
                        });
                        (TokenKind::BlockComment, rest.len())
                    }
                }
            } else if character == '"' {
                let mut index = character.len_utf8();
                let mut terminated = false;
                while index < rest.len() {
                    let current = rest[index..].chars().next().expect("valid UTF-8");
                    index += current.len_utf8();
                    if current == '"' {
                        if rest[index..].starts_with('"') {
                            index += '"'.len_utf8();
                        } else {
                            terminated = true;
                            break;
                        }
                    }
                }
                if !terminated {
                    diagnostics.push(Diagnostic {
                        code: "ORNA0002",
                        message: "unterminated quoted identifier".to_owned(),
                        span: SourceSpan {
                            start: offset,
                            end: source.len(),
                        },
                    });
                }
                (TokenKind::QuotedIdentifier, index)
            } else if character == '\'' {
                let mut index = character.len_utf8();
                let mut terminated = false;
                while index < rest.len() {
                    let current = rest[index..].chars().next().expect("valid UTF-8");
                    index += current.len_utf8();
                    if current == '\'' {
                        if rest[index..].starts_with('\'') {
                            index += '\''.len_utf8();
                        } else {
                            terminated = true;
                            break;
                        }
                    }
                }
                if !terminated {
                    diagnostics.push(Diagnostic {
                        code: "ORNA0002",
                        message: "unterminated string literal".to_owned(),
                        span: SourceSpan {
                            start: offset,
                            end: source.len(),
                        },
                    });
                }
                (TokenKind::StringLiteral, index)
            } else if is_identifier_start(character) {
                let width = rest
                    .char_indices()
                    .find(|(_, current)| !is_identifier_continue(*current))
                    .map_or(rest.len(), |(index, _)| index);
                (TokenKind::Word, width)
            } else {
                let kind = match character {
                    '.' => TokenKind::Dot,
                    ';' => TokenKind::Semicolon,
                    '(' => TokenKind::LeftParenthesis,
                    ')' => TokenKind::RightParenthesis,
                    ',' => TokenKind::Comma,
                    _ => TokenKind::Other,
                };
                (kind, character.len_utf8())
            }
        } else {
            break;
        };

        tokens.push(Token {
            kind,
            text: &source[offset..offset + width],
            range: offset..offset + width,
        });
        offset += width;
    }

    (tokens, diagnostics)
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character) || character.is_numeric()
}
