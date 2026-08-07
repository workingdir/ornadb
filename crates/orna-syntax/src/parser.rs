use rowan::{GreenNode, GreenNodeBuilder, Language};

use crate::{
    Diagnostic, FunctionReturnType, FunctionSecurity, FunctionTransaction, FunctionVolatility,
    NamePart, ObjectFieldDeclaration, ObjectTypeDeclaration, OnDeletePolicy, Parse, QualifiedName,
    RowsColumnDeclaration, SchemaDeclaration, ServerFunctionBody, ServerFunctionDeclaration,
    ServerFunctionParameter, SourceSlice, SourceSpan, SyntaxTree, TypeSpecification,
    lexer::{Token, TokenKind, lex},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub(crate) enum SyntaxKind {
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
    CreateServerFunctionStatement,
    FunctionParameter,
    RowsReturnType,
    RowsColumn,
    SqlQueryBody,
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        Self(kind as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum OrnaLanguage {}

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
            19 => SyntaxKind::CreateServerFunctionStatement,
            20 => SyntaxKind::FunctionParameter,
            21 => SyntaxKind::RowsReturnType,
            22 => SyntaxKind::RowsColumn,
            23 => SyntaxKind::SqlQueryBody,
            _ => panic!("unknown Orna syntax kind"),
        }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub(crate) fn parse(source: &str) -> Parse {
    Parser::new(source).parse()
}

struct Parser<'source> {
    source: &'source str,
    tokens: Vec<Token<'source>>,
    index: usize,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<Diagnostic>,
    schemas: Vec<SchemaDeclaration>,
    object_types: Vec<ObjectTypeDeclaration>,
    server_functions: Vec<ServerFunctionDeclaration>,
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
            server_functions: Vec::new(),
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
            server_functions: self.server_functions,
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
            .is_some_and(|token| token.is_word("SERVER"))
            && self
                .peek_significant(2)
                .is_some_and(|token| token.is_word("FUNCTION"))
        {
            self.parse_create_server_function_statement();
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

    fn parse_create_server_function_statement(&mut self) {
        let statement_start = self.current().expect("CREATE token exists").range.start;
        self.builder
            .start_node(SyntaxKind::CreateServerFunctionStatement.into());

        self.expect_word("CREATE");
        self.skip_trivia();
        if !self.expect_word("SERVER") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        if !self.expect_word("FUNCTION") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        let Some(name) =
            self.parse_qualified_name("expected a function name after CREATE SERVER FUNCTION")
        else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        if self
            .expect_kind(
                TokenKind::LeftParenthesis,
                "expected '(' after server function name",
            )
            .is_none()
        {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        let Some(parameters) = self.parse_server_function_parameters() else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        if !self.expect_word("RETURNS") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        let Some(return_type) = self.parse_function_return_type() else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };

        self.skip_trivia();
        let security = if self
            .current()
            .is_some_and(|token| token.is_word("SECURITY"))
        {
            let security = self.parse_function_security();
            if security.is_none() {
                self.recover_statement();
                self.builder.finish_node();
                return;
            }
            security
        } else {
            None
        };
        self.skip_trivia();
        let transaction = if self
            .current()
            .is_some_and(|token| token.is_word("TRANSACTION"))
        {
            let transaction = self.parse_function_transaction();
            if transaction.is_none() {
                self.recover_statement();
                self.builder.finish_node();
                return;
            }
            transaction
        } else {
            None
        };
        self.skip_trivia();
        let volatility = if self
            .current()
            .is_some_and(|token| token.is_word("VOLATILITY"))
        {
            let volatility = self.parse_function_volatility();
            if volatility.is_none() {
                self.recover_statement();
                self.builder.finish_node();
                return;
            }
            volatility
        } else {
            None
        };

        self.skip_trivia();
        if !self.expect_word("AS") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        let Some(body) = self.parse_sql_query_body() else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after server function SQL query body",
        ) else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };

        self.server_functions.push(ServerFunctionDeclaration {
            name,
            parameters,
            return_type,
            security,
            transaction,
            volatility,
            body,
            span: SourceSpan {
                start: statement_start,
                end: semicolon.end,
            },
        });
        self.builder.finish_node();
    }

    fn parse_server_function_parameters(&mut self) -> Option<Vec<ServerFunctionParameter>> {
        self.parse_parenthesized_comma_list(
            "expected ')' to close server function parameters",
            "expected ',' or ')' after server function parameter",
            "trailing commas are not allowed in server function parameters",
            Self::parse_server_function_parameter,
        )
        .map(|(parameters, _)| parameters)
    }

    fn parse_server_function_parameter(&mut self, order: usize) -> Option<ServerFunctionParameter> {
        self.builder
            .start_node(SyntaxKind::FunctionParameter.into());
        let result = (|| {
            let name = self.expect_identifier("expected a server function parameter name")?;
            let start = name.span.start;
            self.skip_trivia();
            let type_specification = self.parse_type_specification()?;
            let mut end = type_specification.span().end;
            self.skip_trivia();
            let default_expression = if self.current().is_some_and(|token| token.is_word("DEFAULT"))
            {
                self.bump();
                self.skip_trivia();
                let expression = self.parse_default_expression()?;
                end = expression.span.end;
                Some(expression)
            } else {
                None
            };
            Some(ServerFunctionParameter {
                name,
                order,
                type_specification,
                default_expression,
                span: SourceSpan { start, end },
            })
        })();
        self.builder.finish_node();
        result
    }

    fn parse_function_return_type(&mut self) -> Option<FunctionReturnType> {
        if self.current().is_some_and(|token| token.is_word("TABLE")) {
            self.error_current(
                "ORNA0001",
                "RETURNS TABLE is not supported; use RETURNS ROWS (...) for query-producing functions",
            );
            return None;
        }
        if self.current().is_some_and(|token| token.is_word("SET")) {
            self.error_current(
                "ORNA0001",
                "RETURNS SET OF is not supported; use RETURNS ROWS (...) for query-producing functions",
            );
            return None;
        }
        if !self.current().is_some_and(|token| token.is_word("ROWS")) {
            return self
                .parse_type_specification()
                .map(FunctionReturnType::Single);
        }

        self.builder.start_node(SyntaxKind::RowsReturnType.into());
        let start = self.current().expect("ROWS token exists").range.start;
        self.bump();
        self.skip_trivia();
        if self
            .expect_kind(
                TokenKind::LeftParenthesis,
                "expected '(' after RETURNS ROWS",
            )
            .is_none()
        {
            self.builder.finish_node();
            return None;
        }
        let result = self
            .parse_parenthesized_comma_list(
                "expected ')' to close RETURNS ROWS fields",
                "expected ',' or ')' after RETURNS ROWS field",
                "trailing commas are not allowed in RETURNS ROWS fields",
                Self::parse_rows_column,
            )
            .map(|(columns, end)| FunctionReturnType::Rows {
                columns,
                span: SourceSpan { start, end },
            });
        self.builder.finish_node();
        result
    }

    fn parse_rows_column(&mut self, order: usize) -> Option<RowsColumnDeclaration> {
        self.builder.start_node(SyntaxKind::RowsColumn.into());
        let result = (|| {
            let name = self.expect_identifier("expected a RETURNS ROWS field name")?;
            let start = name.span.start;
            self.skip_trivia();
            let type_specification = self.parse_type_specification()?;
            let end = type_specification.span().end;
            Some(RowsColumnDeclaration {
                name,
                order,
                type_specification,
                span: SourceSpan { start, end },
            })
        })();
        self.builder.finish_node();
        result
    }

    fn parse_parenthesized_comma_list<T>(
        &mut self,
        closing_message: &str,
        continuation_message: &str,
        trailing_comma_message: &str,
        parse_item: impl FnMut(&mut Self, usize) -> Option<T>,
    ) -> Option<(Vec<T>, usize)> {
        let mut items = Vec::new();
        let mut parse_item = parse_item;

        loop {
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::RightParenthesis))
            {
                let end = self.current().expect("right parenthesis exists").range.end;
                self.bump();
                return Some((items, end));
            }
            if self.current().is_none() {
                self.error_current("ORNA0001", closing_message);
                return None;
            }

            items.push(parse_item(self, items.len())?);
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::Comma))
            {
                self.bump();
                self.skip_trivia();
                if self
                    .current()
                    .is_some_and(|token| token.is_kind(TokenKind::RightParenthesis))
                {
                    self.error_current("ORNA0001", trailing_comma_message);
                    return None;
                }
                continue;
            }
            if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::RightParenthesis))
            {
                let end = self.current().expect("right parenthesis exists").range.end;
                self.bump();
                return Some((items, end));
            }

            self.error_current("ORNA0001", continuation_message);
            return None;
        }
    }

    fn parse_function_security(&mut self) -> Option<FunctionSecurity> {
        self.expect_word("SECURITY");
        self.skip_trivia();
        if self.take_word("INVOKER").is_some() {
            Some(FunctionSecurity::Invoker)
        } else if self.take_word("DEFINER").is_some() {
            Some(FunctionSecurity::Definer)
        } else {
            self.error_current("ORNA0001", "expected INVOKER or DEFINER after SECURITY");
            None
        }
    }

    fn parse_function_transaction(&mut self) -> Option<FunctionTransaction> {
        self.expect_word("TRANSACTION");
        self.skip_trivia();
        if self.take_word("ATOMIC").is_some() {
            Some(FunctionTransaction::Atomic)
        } else if self.take_word("MANUAL").is_some() {
            Some(FunctionTransaction::Manual)
        } else if self.take_word("READ").is_some() {
            self.skip_trivia();
            self.expect_word("ONLY")
                .then_some(FunctionTransaction::ReadOnly)
        } else {
            self.error_current(
                "ORNA0001",
                "expected ATOMIC, READ ONLY, or MANUAL after TRANSACTION",
            );
            None
        }
    }

    fn parse_function_volatility(&mut self) -> Option<FunctionVolatility> {
        self.expect_word("VOLATILITY");
        self.skip_trivia();
        if self.take_word("IMMUTABLE").is_some() {
            Some(FunctionVolatility::Immutable)
        } else if self.take_word("STABLE").is_some() {
            Some(FunctionVolatility::Stable)
        } else if self.take_word("VOLATILE").is_some() {
            Some(FunctionVolatility::Volatile)
        } else {
            self.error_current(
                "ORNA0001",
                "expected IMMUTABLE, STABLE, or VOLATILE after VOLATILITY",
            );
            None
        }
    }

    fn parse_sql_query_body(&mut self) -> Option<ServerFunctionBody> {
        self.builder.start_node(SyntaxKind::SqlQueryBody.into());
        let result = (|| {
            let first = self.current()?;
            if first.is_kind(TokenKind::Semicolon) {
                self.error_current("ORNA0001", "expected a SQL query after AS");
                return None;
            }
            let start = first.range.start;
            let mut end = start;
            let mut parenthesis_depth = 0usize;

            while let Some(token) = self.current().cloned() {
                if token.is_kind(TokenKind::Semicolon) && parenthesis_depth == 0 {
                    break;
                }
                match token.kind {
                    TokenKind::LeftParenthesis => parenthesis_depth += 1,
                    TokenKind::RightParenthesis if parenthesis_depth > 0 => parenthesis_depth -= 1,
                    _ => {}
                }
                if !token.kind.is_trivia() {
                    end = token.range.end;
                }
                self.bump();
            }
            if end == start {
                self.error_current("ORNA0001", "expected a SQL query after AS");
                return None;
            }
            Some(ServerFunctionBody::SqlQuery(SourceSlice {
                text: self.source[start..end].to_owned(),
                span: SourceSpan { start, end },
            }))
        })();
        self.builder.finish_node();
        result
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
        let name = self
            .parse_multiword_standard_scalar()
            .or_else(|| self.parse_qualified_name("expected a field type"));
        self.builder.finish_node();
        name.map(TypeSpecification::Named)
    }

    fn parse_multiword_standard_scalar(&mut self) -> Option<QualifiedName> {
        let first = self.current()?.clone();
        if !(first.is_word("CHARACTER") || first.is_word("BINARY"))
            || !self
                .peek_significant(1)
                .is_some_and(|token| token.is_word("LARGE"))
            || !self
                .peek_significant(2)
                .is_some_and(|token| token.is_word("OBJECT"))
        {
            return None;
        }

        self.builder.start_node(SyntaxKind::QualifiedName.into());
        let start = first.range.start;
        let first = self
            .take_word(first.text)
            .expect("first scalar token exists");
        self.skip_trivia();
        let large = self.take_word("LARGE").expect("LARGE scalar token exists");
        self.skip_trivia();
        let object = self
            .take_word("OBJECT")
            .expect("OBJECT scalar token exists");
        self.builder.finish_node();

        let text = format!("{} {} {}", first.text, large.text, object.text);
        Some(QualifiedName {
            parts: vec![NamePart {
                text,
                span: SourceSpan {
                    start,
                    end: object.range.end,
                },
            }],
            span: SourceSpan {
                start,
                end: object.range.end,
            },
        })
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

    fn take_word(&mut self, expected: &str) -> Option<Token<'source>> {
        let token = self.current().cloned()?;
        if token.is_word(expected) {
            self.bump();
            Some(token)
        } else {
            None
        }
    }

    fn expect_word_token(&mut self, expected: &str) -> Option<Token<'source>> {
        if let Some(token) = self.take_word(expected) {
            return Some(token);
        }
        if self.current().is_none() {
            self.error_current("ORNA0001", &format!("expected keyword {expected}"));
            return None;
        }
        self.error_current("ORNA0001", &format!("expected keyword {expected}"));
        None
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
