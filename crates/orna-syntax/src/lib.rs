//! Lossless source parsing for the Orna language.
//!
//! This crate recognises schema declarations and object type declarations.
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
    CreateObjectTypeStatement,
    ObjectField,
    NamedTypeSpecification,
    ReferenceTypeSpecification,
    StringLiteral,
    LeftParenthesis,
    RightParenthesis,
    Comma,
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
            11 => SyntaxKind::CreateObjectTypeStatement,
            12 => SyntaxKind::ObjectField,
            13 => SyntaxKind::NamedTypeSpecification,
            14 => SyntaxKind::ReferenceTypeSpecification,
            15 => SyntaxKind::StringLiteral,
            16 => SyntaxKind::LeftParenthesis,
            17 => SyntaxKind::RightParenthesis,
            18 => SyntaxKind::Comma,
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

/// A qualified Orna name.
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

/// A type written in an object field declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeSpecification {
    /// A named type, including source spellings such as `TEXT` and `BOOL`.
    Named(QualifiedName),
    /// A typed reference to an object type.
    Reference {
        /// The object type named by the reference.
        target: QualifiedName,
        /// The span from `REF` through the target type name.
        span: SourceSpan,
    },
}

impl TypeSpecification {
    /// Return the span of the written type specification.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Named(name) => &name.span,
            Self::Reference { span, .. } => span,
        }
    }
}

/// A source slice retained for an expression that is not parsed in this slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSlice {
    /// The exact source text in the slice.
    pub text: String,
    /// The byte range of the slice.
    pub span: SourceSpan,
}

/// The action for a reference when its target is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDeletePolicy {
    /// Reject deletion while the reference exists.
    Restrict,
    /// Set the reference field to null when the target is deleted.
    SetNull,
    /// Delete the referencing object when the target is deleted.
    Cascade,
}

/// A field in an object type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectFieldDeclaration {
    /// The field name as written in source.
    pub name: NamePart,
    /// The zero-based source order of the field within its object type.
    pub order: usize,
    /// The type written for the field.
    pub type_specification: TypeSpecification,
    /// Whether the field may be null. Fields are nullable unless `NOT NULL` appears.
    pub nullable: bool,
    /// Whether the field has a uniqueness constraint.
    pub unique: bool,
    /// The unparsed default expression source, if one was declared.
    pub default_expression: Option<SourceSlice>,
    /// The reference delete action, if one was declared.
    pub on_delete: Option<OnDeletePolicy>,
    /// The span from the field name through its final modifier.
    pub span: SourceSpan,
}

/// A parsed `CREATE TYPE ... AS OBJECT` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTypeDeclaration {
    /// The declared object type name.
    pub name: QualifiedName,
    /// The object fields in source order.
    pub fields: Vec<ObjectFieldDeclaration>,
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
    object_types: Vec<ObjectTypeDeclaration>,
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

    /// Return successfully parsed object type declarations in source order.
    pub fn object_types(&self) -> &[ObjectTypeDeclaration] {
        &self.object_types
    }
}

/// Parse one Orna source unit.
///
/// The parser recognises schema declarations and object type declarations. It
/// keeps all source bytes in its CST, including bytes in malformed statements.
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
    StringLiteral,
    LeftParenthesis,
    RightParenthesis,
    Comma,
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
            Self::StringLiteral => SyntaxKind::StringLiteral,
            Self::LeftParenthesis => SyntaxKind::LeftParenthesis,
            Self::RightParenthesis => SyntaxKind::RightParenthesis,
            Self::Comma => SyntaxKind::Comma,
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

    fn is_kind(&self, kind: TokenKind) -> bool {
        self.kind == kind
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

struct Parser<'source> {
    source: &'source str,
    tokens: Vec<Token<'source>>,
    index: usize,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<Diagnostic>,
    schemas: Vec<SchemaDeclaration>,
    object_types: Vec<ObjectTypeDeclaration>,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str) -> Self {
        let (tokens, diagnostics) = lex(source);
        Self {
            source,
            tokens,
            index: 0,
            builder: GreenNodeBuilder::new(),
            diagnostics,
            schemas: Vec::new(),
            object_types: Vec::new(),
        }
    }

    fn parse(mut self) -> Parse {
        self.builder.start_node(SyntaxKind::Root.into());
        while self.current().is_some() {
            if self.current().is_some_and(|token| token.kind.is_trivia()) {
                self.bump();
            } else if self.current().is_some_and(|token| token.is_word("CREATE")) {
                self.parse_create_statement();
            } else {
                self.error_current("ORNA0001", "expected a supported CREATE statement");
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
            object_types: self.object_types,
        }
    }

    fn parse_create_statement(&mut self) {
        if self
            .peek_significant(1)
            .is_some_and(|token| token.is_word("SCHEMA"))
        {
            self.parse_create_schema_statement();
        } else if self
            .peek_significant(1)
            .is_some_and(|token| token.is_word("TYPE"))
        {
            self.parse_create_object_type_statement();
        } else {
            self.parse_create_schema_statement();
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
        let name = self.parse_qualified_name("expected a schema name after CREATE SCHEMA");
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

    fn parse_create_object_type_statement(&mut self) {
        let statement_start = self.current().expect("CREATE token exists").range.start;
        self.builder
            .start_node(SyntaxKind::CreateObjectTypeStatement.into());

        self.expect_word("CREATE");
        self.skip_trivia();
        if !self.expect_word("TYPE") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }

        self.skip_trivia();
        let name = self.parse_qualified_name("expected a type name after CREATE TYPE");
        self.skip_trivia();
        if !self.expect_word("AS") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        if !self.expect_word("OBJECT") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        if self
            .expect_kind(TokenKind::LeftParenthesis, "expected '(' after AS OBJECT")
            .is_none()
        {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }

        let fields = self.parse_object_fields();
        self.skip_trivia();
        let semicolon = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after object type declaration",
        );

        match (name, fields, semicolon) {
            (Some(name), Some(fields), Some(semicolon)) => {
                self.object_types.push(ObjectTypeDeclaration {
                    name,
                    fields,
                    span: SourceSpan {
                        start: statement_start,
                        end: semicolon.end,
                    },
                })
            }
            (_, _, None) => self.recover_statement(),
            _ => {}
        }

        self.builder.finish_node();
    }

    fn parse_object_fields(&mut self) -> Option<Vec<ObjectFieldDeclaration>> {
        let mut fields = Vec::new();
        let mut valid = true;

        loop {
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::RightParenthesis))
            {
                self.bump();
                return valid.then_some(fields);
            }
            if self.current().is_none() {
                self.error_current("ORNA0001", "expected ')' to close object type fields");
                return None;
            }

            if self.current().is_some_and(|token| token.is_word("PRIMARY")) {
                self.reject_primary_key();
                valid = false;
            } else if let Some(field) = self.parse_object_field(fields.len()) {
                fields.push(field);
            } else {
                self.recover_field();
                valid = false;
            }

            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::Comma))
            {
                self.bump();
                continue;
            }
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::RightParenthesis))
            {
                self.bump();
                return valid.then_some(fields);
            }

            self.error_current("ORNA0001", "expected ',' or ')' after object field");
            self.recover_field();
            valid = false;
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::Comma))
            {
                self.bump();
            }
        }
    }

    fn parse_object_field(&mut self, order: usize) -> Option<ObjectFieldDeclaration> {
        self.builder.start_node(SyntaxKind::ObjectField.into());
        let Some(name) = self.expect_identifier("expected an object field name") else {
            self.builder.finish_node();
            return None;
        };
        let field_start = name.span.start;
        self.skip_trivia();
        let Some(type_specification) = self.parse_type_specification() else {
            self.builder.finish_node();
            return None;
        };
        let mut nullable = true;
        let mut unique = false;
        let mut default_expression = None;
        let mut on_delete = None;
        let mut field_end = type_specification.span().end;

        loop {
            self.skip_trivia();
            let Some(token) = self.current().cloned() else {
                break;
            };
            if token.is_kind(TokenKind::Comma) || token.is_kind(TokenKind::RightParenthesis) {
                break;
            }

            if token.is_word("NOT") {
                self.bump();
                self.skip_trivia();
                if let Some(null) = self.expect_word_token("NULL") {
                    nullable = false;
                    field_end = null.range.end;
                } else {
                    self.builder.finish_node();
                    return None;
                }
            } else if token.is_word("NULL") {
                self.bump();
                nullable = true;
                field_end = token.range.end;
            } else if token.is_word("UNIQUE") {
                self.bump();
                unique = true;
                field_end = token.range.end;
            } else if token.is_word("DEFAULT") {
                self.bump();
                self.skip_trivia();
                let Some(expression) = self.parse_default_expression() else {
                    self.builder.finish_node();
                    return None;
                };
                field_end = expression.span.end;
                default_expression = Some(expression);
            } else if token.is_word("ON") {
                self.bump();
                self.skip_trivia();
                if self.expect_word_token("DELETE").is_none() {
                    self.builder.finish_node();
                    return None;
                }
                self.skip_trivia();
                let Some((policy, policy_end)) = self.parse_on_delete_policy() else {
                    self.builder.finish_node();
                    return None;
                };
                on_delete = Some(policy);
                field_end = policy_end;
            } else if token.is_word("PRIMARY") {
                self.reject_primary_key();
                self.builder.finish_node();
                return None;
            } else {
                self.error_current("ORNA0001", "expected an object field modifier");
                self.builder.finish_node();
                return None;
            }
        }

        self.builder.finish_node();
        Some(ObjectFieldDeclaration {
            name,
            order,
            type_specification,
            nullable,
            unique,
            default_expression,
            on_delete,
            span: SourceSpan {
                start: field_start,
                end: field_end,
            },
        })
    }

    fn parse_type_specification(&mut self) -> Option<TypeSpecification> {
        if self.current().is_some_and(|token| token.is_word("REF")) {
            self.builder
                .start_node(SyntaxKind::ReferenceTypeSpecification.into());
            let start = self.current().expect("REF token exists").range.start;
            self.bump();
            self.skip_trivia();
            let target = self.parse_qualified_name("expected an object type name after REF");
            self.builder.finish_node();
            let target = target?;
            return Some(TypeSpecification::Reference {
                span: SourceSpan {
                    start,
                    end: target.span.end,
                },
                target,
            });
        }

        self.builder
            .start_node(SyntaxKind::NamedTypeSpecification.into());
        let name = self.parse_qualified_name("expected a field type");
        self.builder.finish_node();
        name.map(TypeSpecification::Named)
    }

    fn parse_default_expression(&mut self) -> Option<SourceSlice> {
        let Some(first) = self.current() else {
            self.error_current("ORNA0001", "expected an expression after DEFAULT");
            return None;
        };
        let start = first.range.start;
        let mut expression_end = start;
        let mut parenthesis_depth = 0usize;

        while let Some(token) = self.current().cloned() {
            if parenthesis_depth == 0
                && (token.is_kind(TokenKind::Comma) || token.is_kind(TokenKind::RightParenthesis))
            {
                break;
            }
            match token.kind {
                TokenKind::LeftParenthesis => parenthesis_depth += 1,
                TokenKind::RightParenthesis if parenthesis_depth > 0 => parenthesis_depth -= 1,
                _ => {}
            }
            if !token.kind.is_trivia() {
                expression_end = token.range.end;
            }
            self.bump();
        }

        if start == expression_end {
            self.error_current("ORNA0001", "expected an expression after DEFAULT");
            return None;
        }
        Some(SourceSlice {
            text: self.source[start..expression_end].to_owned(),
            span: SourceSpan {
                start,
                end: expression_end,
            },
        })
    }

    fn parse_on_delete_policy(&mut self) -> Option<(OnDeletePolicy, usize)> {
        let Some(token) = self.current().cloned() else {
            self.error_current(
                "ORNA0001",
                "expected RESTRICT, SET NULL, or CASCADE after ON DELETE",
            );
            return None;
        };
        if token.is_word("RESTRICT") {
            self.bump();
            Some((OnDeletePolicy::Restrict, token.range.end))
        } else if token.is_word("CASCADE") {
            self.bump();
            Some((OnDeletePolicy::Cascade, token.range.end))
        } else if token.is_word("SET") {
            self.bump();
            self.skip_trivia();
            let null = self.expect_word_token("NULL")?;
            Some((OnDeletePolicy::SetNull, null.range.end))
        } else {
            self.error_current(
                "ORNA0001",
                "expected RESTRICT, SET NULL, or CASCADE after ON DELETE",
            );
            None
        }
    }

    fn reject_primary_key(&mut self) {
        self.error_current(
            "ORNA0001",
            "PRIMARY KEY is not allowed in object types; use UNIQUE NOT NULL for a business identity",
        );
        self.bump();
        self.skip_trivia();
        if self.current().is_some_and(|token| token.is_word("KEY")) {
            self.bump();
        }
        self.recover_field();
    }

    fn parse_qualified_name(&mut self, first_identifier_message: &str) -> Option<QualifiedName> {
        self.builder.start_node(SyntaxKind::QualifiedName.into());
        let first = self.expect_identifier(first_identifier_message);
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
            match self.expect_identifier("expected an identifier after '.' in qualified name") {
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
        self.expect_word_token(expected).is_some()
    }

    fn expect_word_token(&mut self, expected: &str) -> Option<Token<'source>> {
        let Some(token) = self.current().cloned() else {
            self.error_current("ORNA0001", &format!("expected keyword {expected}"));
            return None;
        };
        if token.is_word(expected) {
            self.bump();
            Some(token)
        } else {
            self.error_current("ORNA0001", &format!("expected keyword {expected}"));
            None
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

    fn recover_field(&mut self) {
        while let Some(token) = self.current() {
            if matches!(
                token.kind,
                TokenKind::Comma | TokenKind::RightParenthesis | TokenKind::Semicolon
            ) {
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

    fn peek_significant(&self, offset: usize) -> Option<&Token<'source>> {
        self.tokens
            .iter()
            .skip(self.index)
            .filter(|token| !token.kind.is_trivia())
            .nth(offset)
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
    use super::{OnDeletePolicy, TypeSpecification, parse};

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

    #[test]
    fn parses_object_type_fields_without_rewriting_aliases_or_defaults() {
        let source = "CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT NOT NULL,\n\
            project REF tasks.project ON DELETE CASCADE,\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            object_id INT UNIQUE\n\
        );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.object_types().len(), 1);

        let object_type = &parsed.object_types()[0];
        assert_eq!(object_type.name.parts[0].text, "tasks");
        assert_eq!(object_type.name.parts[1].text, "task");
        assert_eq!(object_type.fields.len(), 4);

        let title = &object_type.fields[0];
        assert_eq!(title.name.text, "title");
        assert_eq!(title.order, 0);
        assert!(!title.nullable);
        assert!(!title.unique);
        assert_named_type(&title.type_specification, "TEXT");
        assert_eq!(
            title.span.start,
            source.find("title").expect("title exists")
        );
        assert_eq!(
            title.span.end,
            source.find("NOT NULL").expect("NOT NULL exists") + 8
        );

        let project = &object_type.fields[1];
        assert!(project.nullable);
        assert_eq!(project.on_delete, Some(OnDeletePolicy::Cascade));
        match &project.type_specification {
            TypeSpecification::Reference { target, .. } => {
                assert_eq!(target.parts[0].text, "tasks");
                assert_eq!(target.parts[1].text, "project");
            }
            TypeSpecification::Named(_) => panic!("project must be a reference"),
        }

        let completed = &object_type.fields[2];
        assert_named_type(&completed.type_specification, "BOOL");
        assert!(!completed.nullable);
        assert_eq!(
            completed
                .default_expression
                .as_ref()
                .map(|expression| expression.text.as_str()),
            Some("FALSE")
        );

        let object_id = &object_type.fields[3];
        assert_eq!(object_id.name.text, "object_id");
        assert_named_type(&object_id.type_specification, "INT");
        assert!(object_id.unique);
    }

    #[test]
    fn parses_each_supported_on_delete_policy() {
        let source = "CREATE TYPE crm.contact AS OBJECT (\n\
            restricted REF crm.person ON DELETE RESTRICT,\n\
            cleared REF crm.organisation ON DELETE SET NULL,\n\
            cascading REF crm.account ON DELETE CASCADE\n\
        );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let fields = &parsed.object_types()[0].fields;
        assert_eq!(fields[0].on_delete, Some(OnDeletePolicy::Restrict));
        assert_eq!(fields[1].on_delete, Some(OnDeletePolicy::SetNull));
        assert_eq!(fields[2].on_delete, Some(OnDeletePolicy::Cascade));
    }

    #[test]
    fn rejects_primary_keys_in_object_types_with_an_explanatory_diagnostic() {
        let source = "CREATE TYPE people.person AS OBJECT (id INT PRIMARY KEY);";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.object_types().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert!(
            parsed.diagnostics()[0]
                .message
                .contains("use UNIQUE NOT NULL for a business identity")
        );
        assert_eq!(
            parsed.diagnostics()[0].span.start,
            source.find("PRIMARY").expect("PRIMARY exists")
        );
    }

    fn assert_named_type(type_specification: &TypeSpecification, expected: &str) {
        match type_specification {
            TypeSpecification::Named(name) => {
                assert_eq!(name.parts.len(), 1);
                assert_eq!(name.parts[0].text, expected);
            }
            TypeSpecification::Reference { .. } => panic!("field must use a named type"),
        }
    }
}
