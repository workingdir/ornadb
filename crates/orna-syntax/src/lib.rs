//! Lossless source parsing for the Orna language.
//!
//! This crate currently recognises only `CREATE SCHEMA qualified.name;`.
//! All source bytes remain in the CST, including whitespace and comments.

use std::{fmt, ops::Range};

use rowan::{GreenNode, GreenNodeBuilder, Language};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
enum SyntaxKind {
    Root,
    CreateSchemaStatement,
    QualifiedName,
    Whitespace,
    LineComment,
    BlockComment,
    Word,
    QuotedIdentifier,
    Dot,
    Semicolon,
    Other,
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        Self(kind as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum OrnaLanguage {}

impl Language for OrnaLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        match raw.0 {
            0 => SyntaxKind::Root,
            1 => SyntaxKind::CreateSchemaStatement,
            2 => SyntaxKind::QualifiedName,
            3 => SyntaxKind::Whitespace,
            4 => SyntaxKind::LineComment,
            5 => SyntaxKind::BlockComment,
            6 => SyntaxKind::Word,
            7 => SyntaxKind::QuotedIdentifier,
            8 => SyntaxKind::Dot,
            9 => SyntaxKind::Semicolon,
            10 => SyntaxKind::Other,
            _ => panic!("unknown Orna syntax kind"),
        }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

/// A byte range in the input source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    /// The first byte in the range, inclusive.
    pub start: usize,
    /// The first byte after the range, exclusive.
    pub end: usize,
}

impl SourceSpan {
    fn from_range(range: Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// A source error emitted by the lexer or the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// A stable category for programmatic handling.
    pub code: &'static str,
    /// A human-readable description of the error.
    pub message: String,
    /// The part of the source that caused the error.
    pub span: SourceSpan,
}

/// One identifier component in a qualified name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePart {
    /// The exact source spelling, including quotes when present.
    pub text: String,
    /// The byte range of this component.
    pub span: SourceSpan,
}

/// A qualified schema name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    /// The name components in source order.
    pub parts: Vec<NamePart>,
    /// The span from the first component through the final component.
    pub span: SourceSpan,
}

/// A parsed `CREATE SCHEMA` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDeclaration {
    /// The declared schema name.
    pub name: QualifiedName,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// A private wrapper around the lossless Rowan tree.
///
/// It intentionally exposes text, rather than Rowan types, as the public CST
/// boundary. This keeps the parser implementation replaceable.
#[derive(Clone)]
pub struct SyntaxTree {
    root: rowan::SyntaxNode<OrnaLanguage>,
}

impl SyntaxTree {
    /// Return the exact source text represented by this tree.
    pub fn text(&self) -> String {
        self.root.to_string()
    }
}

impl fmt::Debug for SyntaxTree {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SyntaxTree").finish_non_exhaustive()
    }
}

/// The output from parsing one source unit.
#[derive(Debug, Clone)]
pub struct Parse {
    syntax: SyntaxTree,
    diagnostics: Vec<Diagnostic>,
    schemas: Vec<SchemaDeclaration>,
}

impl Parse {
    /// Return the lossless CST.
    pub fn syntax(&self) -> &SyntaxTree {
        &self.syntax
    }

    /// Return all lexical and syntactic diagnostics in source order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Return successfully parsed schema declarations in source order.
    pub fn schemas(&self) -> &[SchemaDeclaration] {
        &self.schemas
    }
}

/// Parse one Orna source unit.
///
/// The parser recognises `CREATE SCHEMA qualified.name;` only. It keeps all
/// source bytes in its CST, including bytes in malformed statements.
pub fn parse(source: &str) -> Parse {
    Parser::new(source).parse()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Whitespace,
    LineComment,
    BlockComment,
    Word,
    QuotedIdentifier,
    Dot,
    Semicolon,
    Other,
}

impl TokenKind {
    fn syntax_kind(self) -> SyntaxKind {
        match self {
            Self::Whitespace => SyntaxKind::Whitespace,
            Self::LineComment => SyntaxKind::LineComment,
            Self::BlockComment => SyntaxKind::BlockComment,
            Self::Word => SyntaxKind::Word,
            Self::QuotedIdentifier => SyntaxKind::QuotedIdentifier,
            Self::Dot => SyntaxKind::Dot,
            Self::Semicolon => SyntaxKind::Semicolon,
            Self::Other => SyntaxKind::Other,
        }
    }

    fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment
        )
    }
}

#[derive(Debug, Clone)]
struct Token<'source> {
    kind: TokenKind,
    text: &'source str,
    range: Range<usize>,
}

impl<'source> Token<'source> {
    fn span(&self) -> SourceSpan {
        SourceSpan::from_range(self.range.clone())
    }

    fn is_word(&self, expected: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(expected)
    }

    fn is_identifier(&self) -> bool {
        matches!(self.kind, TokenKind::Word | TokenKind::QuotedIdentifier)
    }
}

fn lex(source: &str) -> (Vec<Token<'_>>, Vec<Diagnostic>) {
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

struct Parser<'source> {
    tokens: Vec<Token<'source>>,
    index: usize,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<Diagnostic>,
    schemas: Vec<SchemaDeclaration>,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str) -> Self {
        let (tokens, diagnostics) = lex(source);
        Self {
            tokens,
            index: 0,
            builder: GreenNodeBuilder::new(),
            diagnostics,
            schemas: Vec::new(),
        }
    }

    fn parse(mut self) -> Parse {
        self.builder.start_node(SyntaxKind::Root.into());
        while self.current().is_some() {
            if self.current().is_some_and(|token| token.kind.is_trivia()) {
                self.bump();
            } else if self.current().is_some_and(|token| token.is_word("CREATE")) {
                self.parse_create_schema_statement();
            } else {
                self.error_current(
                    "ORNA0001",
                    "expected CREATE SCHEMA statement; only CREATE SCHEMA is supported",
                );
                self.bump();
            }
        }
        self.builder.finish_node();

        let green: GreenNode = self.builder.finish();
        Parse {
            syntax: SyntaxTree {
                root: rowan::SyntaxNode::new_root(green),
            },
            diagnostics: self.diagnostics,
            schemas: self.schemas,
        }
    }

    fn parse_create_schema_statement(&mut self) {
        let statement_start = self.current().expect("CREATE token exists").range.start;
        self.builder
            .start_node(SyntaxKind::CreateSchemaStatement.into());

        self.expect_word("CREATE");
        self.skip_trivia();
        if !self.expect_word("SCHEMA") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }

        self.skip_trivia();
        let name = self.parse_qualified_name();
        self.skip_trivia();
        let semicolon = self.expect_kind(TokenKind::Semicolon, "expected ';' after schema name");

        match (name, semicolon) {
            (Some(name), Some(semicolon)) => self.schemas.push(SchemaDeclaration {
                name,
                span: SourceSpan {
                    start: statement_start,
                    end: semicolon.end,
                },
            }),
            (_, None) => self.recover_statement(),
            (None, Some(_)) => {}
        }

        self.builder.finish_node();
    }

    fn parse_qualified_name(&mut self) -> Option<QualifiedName> {
        self.builder.start_node(SyntaxKind::QualifiedName.into());
        let first = self.expect_identifier("expected a schema name after CREATE SCHEMA");
        let mut parts = first.into_iter().collect::<Vec<_>>();

        loop {
            self.skip_trivia();
            if !self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Dot)
            {
                break;
            }
            self.bump();
            self.skip_trivia();
            match self.expect_identifier("expected an identifier after '.' in schema name") {
                Some(part) => parts.push(part),
                None => {
                    self.builder.finish_node();
                    return None;
                }
            }
        }
        self.builder.finish_node();

        let start = parts.first()?.span.start;
        let end = parts.last()?.span.end;
        Some(QualifiedName {
            parts,
            span: SourceSpan { start, end },
        })
    }

    fn expect_word(&mut self, expected: &str) -> bool {
        if self.current().is_some_and(|token| token.is_word(expected)) {
            self.bump();
            true
        } else {
            self.error_current("ORNA0001", &format!("expected keyword {expected}"));
            false
        }
    }

    fn expect_identifier(&mut self, message: &str) -> Option<NamePart> {
        let Some(token) = self.current().cloned() else {
            self.error_current("ORNA0001", message);
            return None;
        };
        if !token.is_identifier() {
            self.error_current("ORNA0001", message);
            return None;
        }
        self.bump();
        Some(NamePart {
            text: token.text.to_owned(),
            span: token.span(),
        })
    }

    fn expect_kind(&mut self, kind: TokenKind, message: &str) -> Option<SourceSpan> {
        if self.current().is_some_and(|token| token.kind == kind) {
            let span = self.current().expect("current token exists").span();
            self.bump();
            Some(span)
        } else {
            self.error_current("ORNA0001", message);
            None
        }
    }

    fn skip_trivia(&mut self) {
        while self.current().is_some_and(|token| token.kind.is_trivia()) {
            self.bump();
        }
    }

    fn recover_statement(&mut self) {
        while let Some(token) = self.current() {
            if token.kind == TokenKind::Semicolon {
                self.bump();
                break;
            }
            self.bump();
        }
    }

    fn error_current(&mut self, code: &'static str, message: &str) {
        let span = self.current().map_or_else(
            || {
                let end = self.tokens.last().map_or(0, |token| token.range.end);
                SourceSpan { start: end, end }
            },
            Token::span,
        );
        self.diagnostics.push(Diagnostic {
            code,
            message: message.to_owned(),
            span,
        });
    }

    fn current(&self) -> Option<&Token<'source>> {
        self.tokens.get(self.index)
    }

    fn bump(&mut self) {
        let token = self.current().expect("token exists").clone();
        self.builder
            .token(token.kind.syntax_kind().into(), token.text);
        self.index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn parses_schema_declarations_case_insensitively_without_rewriting_source() {
        let source = "cReAtE sChEmA crm.sales;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "crm");
        assert_eq!(parsed.schemas()[0].name.parts[1].text, "sales");
    }

    #[test]
    fn preserves_trivia_across_multiple_schema_declarations() {
        let source =
            "-- initial namespace\nCREATE SCHEMA people; /* task data */\nCREATE SCHEMA tasks;\n";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas().len(), 2);
        assert_eq!(parsed.schemas()[1].span.start, 59);
    }

    #[test]
    fn reports_malformed_schema_declarations_with_source_spans() {
        let source = "CREATE SCHEMA crm.;\nCREATE SCHEMA tasks";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas().len(), 0);
        assert_eq!(parsed.diagnostics().len(), 2);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(parsed.diagnostics()[0].span.start, 18);
        assert_eq!(parsed.diagnostics()[1].code, "ORNA0001");
        assert_eq!(parsed.diagnostics()[1].span.start, source.len());
    }

    #[test]
    fn reports_unterminated_comments_without_losing_source() {
        let source = "CREATE SCHEMA crm; /* unfinished";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0002");
        assert_eq!(parsed.diagnostics()[0].span.start, 19);
        assert_eq!(parsed.diagnostics()[0].span.end, source.len());
    }
}
