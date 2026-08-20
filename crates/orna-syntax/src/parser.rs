use rowan::{GreenNode, GreenNodeBuilder, Language};

use crate::{
    CapabilitySpecification, ClientCallArgument, ClientExpression, ClientFunctionBody,
    ClientFunctionDeclaration, ClientLocalBinding, ClientProceduralStatement,
    ClientAssignmentStatement, ClientLetStatement, ClientStateBlockBody,
    DeleteStatement, Diagnostic,
    EnumLabelDeclaration, EnumTypeDeclaration, FieldRenameDeclaration, FunctionReturnType,
    FunctionSecurity, FunctionTransaction, FunctionVolatility, InsertStatement, MutationValue,
    NamePart, NoInputParameterSelectBody, NullOrdering, ObjectFieldDeclaration, ObjectSource,
    ObjectTypeDeclaration, OnDeletePolicy, OpaqueValueTypeDeclaration, OptionTypeSpelling,
    OrderingDirection, OrderingExpression, Parse, PrimitiveValueTypeDeclaration,
    PrimitiveValueTypePersistence, QualifiedName, QueryExpression, RecordConstructor,
    RecordConstructorField, RecordConstructorFieldValue, RecordValueTypeDeclaration,
    RowsColumnDeclaration, SchemaDeclaration, SelectQuantifier, SelectQuery, ServerFunctionBody,
    ServerFunctionDeclaration, ServerFunctionParameter, SourceSlice, SourceSpan, SqlDeleteBody,
    SqlInsertBody, SqlQueryBody, SqlUpdateBody, StandardLargeObjectKind, StateDeclaration,
    StateDefault, StateScope, SyntaxTree, TypeExportDeclaration, TypeExportTarget,
    TypeSpecification, UpdateAssignment, UpdateStatement, ValueFieldDeclaration,
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
    CreateTypeStatement,
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
    StandardLargeObjectTypeSpecification,
    CapabilityClause,
    CapabilitySpecification,
    CapabilityArguments,
    SqlInsertBody,
    AlterTypeRenameFieldStatement,
    SqlUpdateBody,
    SqlDeleteBody,
    CreateClientFunctionStatement,
    ClientFunctionParameter,
    ClientFunctionReturnType,
    ClientBooleanReturnBody,
    ClientExpressionBody,
    ClientExternalContractBody,
    ClientCallExpression,
    ClientCallArgument,
    ClientAwaitExpression,
    ClientConcatExpression,
    NumberLiteral,
    ExportTypeStatement,
    ValueField,
    ListTypeSpecification,
    SetTypeSpecification,
    MapTypeSpecification,
    OptionTypeSpecification,
    StreamTypeSpecification,
    ClientStateBlockBody,
    ClientStateDeclaration,
    ClientStateReturnStatement,
    ClientLocalBinding,
    ClientProceduralStatement,
    ClientProceduralLetStatement,
    ClientAssignmentStatement,
    StreamReturnType,
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
            11 => SyntaxKind::CreateTypeStatement,
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
            24 => SyntaxKind::StandardLargeObjectTypeSpecification,
            25 => SyntaxKind::CapabilityClause,
            26 => SyntaxKind::CapabilitySpecification,
            27 => SyntaxKind::CapabilityArguments,
            28 => SyntaxKind::SqlInsertBody,
            29 => SyntaxKind::AlterTypeRenameFieldStatement,
            30 => SyntaxKind::SqlUpdateBody,
            31 => SyntaxKind::SqlDeleteBody,
            32 => SyntaxKind::CreateClientFunctionStatement,
            33 => SyntaxKind::ClientFunctionParameter,
            34 => SyntaxKind::ClientFunctionReturnType,
            35 => SyntaxKind::ClientBooleanReturnBody,
            36 => SyntaxKind::ClientExpressionBody,
            37 => SyntaxKind::ClientExternalContractBody,
            38 => SyntaxKind::ClientCallExpression,
            39 => SyntaxKind::ClientCallArgument,
            40 => SyntaxKind::ClientAwaitExpression,
            41 => SyntaxKind::ClientConcatExpression,
            42 => SyntaxKind::NumberLiteral,
            43 => SyntaxKind::ExportTypeStatement,
            44 => SyntaxKind::ValueField,
            45 => SyntaxKind::ListTypeSpecification,
            46 => SyntaxKind::SetTypeSpecification,
            47 => SyntaxKind::MapTypeSpecification,
            48 => SyntaxKind::OptionTypeSpecification,
            49 => SyntaxKind::StreamTypeSpecification,
            50 => SyntaxKind::ClientStateBlockBody,
            51 => SyntaxKind::ClientStateDeclaration,
            52 => SyntaxKind::ClientStateReturnStatement,
            53 => SyntaxKind::ClientLocalBinding,
            54 => SyntaxKind::ClientProceduralStatement,
            55 => SyntaxKind::ClientProceduralLetStatement,
            56 => SyntaxKind::ClientAssignmentStatement,
            57 => SyntaxKind::StreamReturnType,
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
    enum_types: Vec<EnumTypeDeclaration>,
    record_value_types: Vec<RecordValueTypeDeclaration>,
    primitive_value_types: Vec<PrimitiveValueTypeDeclaration>,
    opaque_value_types: Vec<OpaqueValueTypeDeclaration>,
    type_exports: Vec<TypeExportDeclaration>,
    field_renames: Vec<FieldRenameDeclaration>,
    server_functions: Vec<ServerFunctionDeclaration>,
    client_functions: Vec<ClientFunctionDeclaration>,
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
            enum_types: Vec::new(),
            record_value_types: Vec::new(),
            primitive_value_types: Vec::new(),
            opaque_value_types: Vec::new(),
            type_exports: Vec::new(),
            field_renames: Vec::new(),
            server_functions: Vec::new(),
            client_functions: Vec::new(),
        }
    }

    fn parse(mut self) -> Parse {
        self.builder.start_node(SyntaxKind::Root.into());
        while self.current().is_some() {
            if self.current().is_some_and(|token| token.kind.is_trivia()) {
                self.bump();
            } else if self.current().is_some_and(|token| token.is_word("CREATE")) {
                self.parse_create_statement();
            } else if self.current().is_some_and(|token| token.is_word("ALTER")) {
                self.parse_alter_type_rename_field_statement();
            } else if self.current().is_some_and(|token| token.is_word("EXPORT")) {
                self.parse_export_type_statement();
            } else {
                self.error_current(
                    "ORNA0001",
                    "expected a CREATE, ALTER, or EXPORT declaration",
                );
                self.recover_statement();
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
            enum_types: self.enum_types,
            record_value_types: self.record_value_types,
            primitive_value_types: self.primitive_value_types,
            opaque_value_types: self.opaque_value_types,
            type_exports: self.type_exports,
            field_renames: self.field_renames,
            server_functions: self.server_functions,
            client_functions: self.client_functions,
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
        } else if (self
            .peek_significant(1)
            .is_some_and(|token| token.is_word("CLIENT"))
            && self
                .peek_significant(2)
                .is_some_and(|token| token.is_word("FUNCTION")))
            || (self
                .peek_significant(1)
                .is_some_and(|token| token.is_word("EXTERNAL"))
                && self
                    .peek_significant(2)
                    .is_some_and(|token| token.is_word("CLIENT"))
                && self
                    .peek_significant(3)
                    .is_some_and(|token| token.is_word("FUNCTION")))
        {
            self.parse_create_client_function_statement();
        } else if self
            .peek_significant(1)
            .is_some_and(|token| token.is_word("TYPE"))
        {
            self.parse_create_type_statement();
        } else {
            self.parse_create_schema_statement();
        }
    }

    fn parse_alter_type_rename_field_statement(&mut self) {
        let statement_start = self.current().expect("ALTER token exists").range.start;
        self.builder
            .start_node(SyntaxKind::AlterTypeRenameFieldStatement.into());

        self.expect_word("ALTER");
        self.skip_trivia();
        if self.take_word("TYPE").is_none() {
            self.error_current("ORNA0001", "ALTER must be followed by TYPE");
            self.recover_statement();
            self.builder.finish_node();
            return;
        }

        self.skip_trivia();
        if self.current().is_some_and(|token| token.is_word("RENAME")) {
            self.error_current("ORNA0001", "expected the type name after ALTER TYPE");
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        let Some(type_name) = self.parse_qualified_name_with_messages(
            "expected the type name after ALTER TYPE",
            "expected the type name after '.'",
            Some("RENAME"),
        ) else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        if self.take_word("RENAME").is_none() {
            self.error_current("ORNA0001", "expected RENAME after the type name");
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        if self.take_word("FIELD").is_none() {
            let message = if self.current().is_some_and(|token| token.is_word("TO")) {
                "ALTER TYPE only supports RENAME FIELD"
            } else {
                "expected FIELD after RENAME"
            };
            self.error_current("ORNA0001", message);
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        if self.current().is_some_and(|token| token.is_word("TO")) {
            self.error_current("ORNA0001", "expected the old field name after RENAME FIELD");
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        let Some(old_field_name) =
            self.expect_identifier("expected the old field name after RENAME FIELD")
        else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        if self.take_word("TO").is_none() {
            self.error_current("ORNA0001", "expected TO after the old field name");
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        let Some(new_field_name) = self.expect_identifier("expected the new field name after TO")
        else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after field rename declaration",
        ) else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };

        self.field_renames.push(FieldRenameDeclaration {
            type_name,
            old_field_name,
            new_field_name,
            span: SourceSpan {
                start: statement_start,
                end: semicolon.end,
            },
        });
        self.builder.finish_node();
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
        let capabilities = if self
            .current()
            .is_some_and(|token| token.is_word("REQUIRES"))
        {
            match self.parse_capability_clause() {
                Some(capabilities) => capabilities,
                None => {
                    self.recover_statement();
                    self.builder.finish_node();
                    return;
                }
            }
        } else {
            Vec::new()
        };

        self.skip_trivia();
        if !self.expect_word("AS") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        let Some(body) = self.parse_server_function_body() else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        let terminator_error = if body.as_sql_insert().is_some() {
            "expected ';' after server function INSERT body"
        } else {
            "expected ';' after server function SQL query body"
        };
        let Some(semicolon) = self.expect_kind(TokenKind::Semicolon, terminator_error) else {
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
            capabilities,
            body,
            span: SourceSpan {
                start: statement_start,
                end: semicolon.end,
            },
        });
        self.builder.finish_node();
    }

    fn parse_create_client_function_statement(&mut self) {
        let statement_start = self.current().expect("CREATE token exists").range.start;
        self.builder
            .start_node(SyntaxKind::CreateClientFunctionStatement.into());

        self.expect_word("CREATE");
        self.skip_trivia();
        let external = if self
            .current()
            .is_some_and(|token| token.is_word("EXTERNAL"))
        {
            self.bump();
            self.skip_trivia();
            true
        } else {
            false
        };
        if !self.expect_word("CLIENT") {
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
            self.parse_qualified_name("expected a function name after CREATE CLIENT FUNCTION")
        else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        let Some(parameter_list_start) = self.expect_kind(
            TokenKind::LeftParenthesis,
            "expected '(' after client function name",
        ) else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        let Some((parameters, parameter_list_end)) = self.parse_client_function_parameters() else {
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
        self.builder
            .start_node(SyntaxKind::ClientFunctionReturnType.into());
        let return_type = self.parse_client_function_return_type();
        self.builder.finish_node();
        let Some(return_type) = return_type else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };

        self.skip_trivia();
        let runtime_contract = if external {
            let Some(contract) = self.parse_runtime_contract_clause() else {
                self.recover_statement();
                self.builder.finish_node();
                return;
            };
            Some(contract)
        } else {
            None
        };

        self.skip_trivia();
        let capabilities = if self
            .current()
            .is_some_and(|token| token.is_word("REQUIRES"))
        {
            match self.parse_capability_clause() {
                Some(capabilities) => capabilities,
                None => {
                    self.recover_statement();
                    self.builder.finish_node();
                    return;
                }
            }
        } else {
            Vec::new()
        };

        let body = if external {
            let Some(identity) = runtime_contract.clone() else {
                self.recover_statement();
                self.builder.finish_node();
                return;
            };
            ClientFunctionBody::ExternalContract { identity }
        } else {
            self.skip_trivia();
            let Some(body) = self.parse_client_function_body() else {
                self.recover_statement();
                self.builder.finish_node();
                return;
            };
            body
        };
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after CLIENT function body",
        ) else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };

        self.client_functions.push(ClientFunctionDeclaration {
            name,
            parameters,
            parameter_list_span: SourceSpan {
                start: parameter_list_start.start,
                end: parameter_list_end,
            },
            return_type,
            external,
            runtime_contract,
            capabilities,
            body,
            span: SourceSpan {
                start: statement_start,
                end: semicolon.end,
            },
        });
        self.builder.finish_node();
    }

    /// Parses one `RUNTIME CONTRACT '<identity>'` clause.
    ///
    /// The clause owns the `ClientExternalContractBody` node: the string is
    /// emitted here at its exact source position, before any capability
    /// clause, so the tree stays lossless regardless of clause order.
    fn parse_runtime_contract_clause(&mut self) -> Option<SourceSlice> {
        if !self.expect_word("RUNTIME") {
            self.error_current(
                "ORNA0001",
                "external CLIENT functions must declare RUNTIME CONTRACT '<identity>'",
            );
            return None;
        }
        self.skip_trivia();
        if !self.expect_word("CONTRACT") {
            self.error_current(
                "ORNA0001",
                "external CLIENT functions must declare RUNTIME CONTRACT '<identity>'",
            );
            return None;
        }
        self.skip_trivia();
        let Some(token) = self.current().cloned() else {
            self.error_current(
                "ORNA0001",
                "expected a contract identity string after RUNTIME CONTRACT",
            );
            return None;
        };
        if token.kind != TokenKind::StringLiteral {
            self.error_current(
                "ORNA0001",
                "expected a quoted contract identity after RUNTIME CONTRACT",
            );
            return None;
        }
        self.builder
            .start_node(SyntaxKind::ClientExternalContractBody.into());
        self.bump();
        self.builder.finish_node();
        Some(SourceSlice {
            text: token.text.to_owned(),
            span: token.span(),
        })
    }

    fn parse_client_function_parameters(
        &mut self,
    ) -> Option<(Vec<ServerFunctionParameter>, usize)> {
        self.parse_parenthesized_comma_list(
            "expected ')' to close CLIENT function parameters",
            "expected ',' or ')' after CLIENT function parameter",
            "trailing commas are not allowed in CLIENT function parameters",
            Self::parse_client_function_parameter,
        )
    }

    fn parse_client_function_parameter(&mut self, order: usize) -> Option<ServerFunctionParameter> {
        self.builder
            .start_node(SyntaxKind::ClientFunctionParameter.into());
        let result = self.parse_function_parameter(order, "CLIENT function");
        self.builder.finish_node();
        result
    }

    fn parse_client_function_body(&mut self) -> Option<ClientFunctionBody> {
        if self.current().is_some_and(|token| token.is_word("AS")) {
            self.bump();
            self.skip_trivia();
            self.builder
                .start_node(SyntaxKind::ClientExpressionBody.into());
            let expression = self.parse_client_expression();
            self.builder.finish_node();
            return expression.map(|expression| ClientFunctionBody::Expression { expression });
        }
        if self.current().is_some_and(|token| token.is_word("IS")) {
            return self.parse_client_state_block();
        }
        self.builder
            .start_node(SyntaxKind::ClientBooleanReturnBody.into());
        let result = (|| {
            if !self.current().is_some_and(|token| token.is_word("RETURN")) {
                self.error_current(
                    "ORNA0001",
                    "CLIENT functions use RETURN before their result value",
                );
                return None;
            }
            self.bump();
            self.skip_trivia();
            let Some(token) = self.current().cloned() else {
                self.error_current(
                    "ORNA0001",
                    "CLIENT RETURN currently supports only TRUE or FALSE",
                );
                return None;
            };
            let value = if token.is_word("TRUE") {
                true
            } else if token.is_word("FALSE") {
                false
            } else {
                self.error_current(
                    "ORNA0001",
                    "CLIENT RETURN currently supports only TRUE or FALSE",
                );
                return None;
            };
            self.bump();
            Some(ClientFunctionBody::BooleanLiteral {
                value,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            })
        })();
        self.builder.finish_node();
        result
    }

    /// Parses one closed CLIENT expression (ADR 0068).
    ///
    /// The expression surface is a call, a string/integer/Boolean literal, a
    /// parameter read, a field path from a parameter, `AWAIT` over one nested
    /// client expression, and left-associative `||` concatenation. This parser
    /// is deliberately closed: it never accepts arithmetic, comparisons,
    /// parentheses, or SQL fragments.
    fn parse_client_expression(&mut self) -> Option<ClientExpression> {
        let mut left = self.parse_client_primary_expression()?;
        loop {
            self.skip_trivia();
            if self.current().is_none_or(|token| token.text != "||") {
                break;
            }
            self.bump();
            self.skip_trivia();
            let right = self.parse_client_primary_expression()?;
            let span = SourceSpan {
                start: left.span().start,
                end: right.span().end,
            };
            left = ClientExpression::Concat {
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Some(left)
    }

    fn parse_client_primary_expression(&mut self) -> Option<ClientExpression> {
        let Some(token) = self.current().cloned() else {
            self.error_current("ORNA0001", "expected a CLIENT expression");
            return None;
        };
        if token.is_word("AWAIT") {
            return self.parse_client_await_expression();
        }
        if token.kind == TokenKind::StringLiteral {
            self.bump();
            return Some(ClientExpression::StringLiteral {
                value: Self::unquote_client_text_literal(token.text)?,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if token.kind == TokenKind::NumberLiteral {
            self.bump();
            let value = token.text.parse::<i64>().ok().or_else(|| {
                self.error_current(
                    "ORNA0001",
                    "CLIENT integer literals must fit in a signed 64-bit value",
                );
                None
            })?;
            return Some(ClientExpression::IntegerLiteral {
                value,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if token.is_word("TRUE") || token.is_word("FALSE") {
            self.bump();
            return Some(ClientExpression::BooleanLiteral {
                value: token.is_word("TRUE"),
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if !token.is_identifier() {
            self.error_current("ORNA0001", "expected a CLIENT expression");
            return None;
        }
        let root_start = token.span().start;
        // Collect one dotted name without emitting tokens yet: a qualified
        // callee is followed by `(`, otherwise the name is a parameter read
        // or field path. Emitting happens inside the owning node so the
        // highlighter sees the call's opening parenthesis after the callee.
        let mut parts = Vec::new();
        let mut end = root_start;
        let mut probe = self.index;
        while let Some(part) = self.tokens.get(probe).cloned() {
            if !part.is_identifier() {
                break;
            }
            parts.push(NamePart {
                text: part.text.to_owned(),
                span: part.span(),
            });
            end = part.span().end;
            probe += 1;
            while self
                .tokens
                .get(probe)
                .is_some_and(|token| token.kind.is_trivia())
            {
                probe += 1;
            }
            if self
                .tokens
                .get(probe)
                .is_some_and(|token| token.kind == TokenKind::Dot)
            {
                probe += 1;
                while self
                    .tokens
                    .get(probe)
                    .is_some_and(|token| token.kind.is_trivia())
                {
                    probe += 1;
                }
                if !self.tokens.get(probe).is_some_and(Token::is_identifier) {
                    self.index = probe;
                    self.error_current(
                        "ORNA0001",
                        "expected an identifier after a CLIENT expression dot",
                    );
                    return None;
                }
                continue;
            }
            break;
        }
        if parts.is_empty() {
            self.error_current("ORNA0001", "expected a CLIENT expression");
            return None;
        }
        if self
            .tokens
            .get(probe)
            .is_some_and(|token| token.kind == TokenKind::LeftParenthesis)
        {
            return self.parse_client_call(parts, root_start, end);
        }
        // Emit the collected name tokens for the non-call form.
        for part in &parts {
            self.skip_trivia();
            self.bump();
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Dot)
            {
                self.bump();
                self.skip_trivia();
            }
            let _ = part;
        }
        if parts.len() == 1 {
            let parameter = parts
                .into_iter()
                .next()
                .expect("a single-part name has one part");
            return Some(ClientExpression::ParameterRead { parameter });
        }
        let root = parts.remove(0);
        Some(ClientExpression::FieldPath {
            root,
            members: parts,
            span: SourceSpan {
                start: root_start,
                end,
            },
        })
    }

    fn parse_client_await_expression(&mut self) -> Option<ClientExpression> {
        let start = self.current()?.span().start;
        self.builder
            .start_node(SyntaxKind::ClientAwaitExpression.into());
        self.bump(); // consume AWAIT
        self.skip_trivia();
        let result = self.parse_client_expression().map(|expression| {
            let span = SourceSpan {
                start,
                end: expression.span().end,
            };
            ClientExpression::Await {
                expression: Box::new(expression),
                span,
            }
        });
        self.builder.finish_node();
        result
    }

    fn parse_client_call(
        &mut self,
        parts: Vec<NamePart>,
        start: usize,
        name_end: usize,
    ) -> Option<ClientExpression> {
        self.builder
            .start_node(SyntaxKind::ClientCallExpression.into());
        let result = self.parse_client_call_inner(parts, start, name_end);
        self.builder.finish_node();
        result
    }

    fn parse_client_call_inner(
        &mut self,
        parts: Vec<NamePart>,
        start: usize,
        name_end: usize,
    ) -> Option<ClientExpression> {
        let callee = QualifiedName {
            parts,
            span: SourceSpan {
                start,
                end: name_end,
            },
        };
        // Emit the callee qualified name inside the call node.
        self.builder.start_node(SyntaxKind::QualifiedName.into());
        for (index, _part) in callee.parts.iter().enumerate() {
            self.skip_trivia();
            self.bump();
            self.skip_trivia();
            if index + 1 < callee.parts.len()
                && self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::Dot)
            {
                self.bump();
                self.skip_trivia();
            }
        }
        self.builder.finish_node();
        self.bump(); // consume '('
        self.skip_trivia();
        let mut arguments = Vec::new();
        if !self
            .current()
            .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
        {
            loop {
                let argument = self.parse_client_call_argument()?;
                let span = argument.span;
                arguments.push(ClientCallArgument {
                    name: argument.name,
                    value: argument.value,
                    span,
                });
                self.skip_trivia();
                if self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::Comma)
                {
                    self.bump();
                    self.skip_trivia();
                    continue;
                }
                break;
            }
        }
        self.skip_trivia();
        self.expect_kind(
            TokenKind::RightParenthesis,
            "expected ')' to close the CLIENT call",
        )?;
        let end = self.tokens[self.index - 1].range.end;
        let span = SourceSpan {
            start: callee.span.start,
            end,
        };
        Some(ClientExpression::Call {
            callee,
            arguments,
            span,
        })
    }

    /// Unquotes one single-quoted text literal with doubled-quote escaping.
    fn unquote_client_text_literal(text: &str) -> Option<String> {
        let mut characters = text.chars();
        if characters.next() != Some('\'') || !text.ends_with('\'') {
            return None;
        }
        let inner = &text[1..text.len() - 1];
        let mut value = String::with_capacity(inner.len());
        let mut characters = inner.chars().peekable();
        while let Some(character) = characters.next() {
            value.push(character);
            if character == '\'' && characters.peek() == Some(&'\'') {
                characters.next();
            }
        }
        Some(value)
    }

    fn parse_client_call_argument(&mut self) -> Option<ClientCallArgument> {
        self.builder
            .start_node(SyntaxKind::ClientCallArgument.into());
        let result = (|| {
            let start = self.current()?.span().start;
            // A named argument is `identifier => expression`. Detect it with
            // a pure lookahead over the token vector (never emitting or
            // moving the parser index), then emit the identifier, the arrow,
            // and their intervening trivia exactly once.
            let named = if self.current().is_some_and(Token::is_identifier) {
                let name = self.current().cloned()?;
                let mut probe = self.index + 1;
                while self
                    .tokens
                    .get(probe)
                    .is_some_and(|token| token.kind.is_trivia())
                {
                    probe += 1;
                }
                if self
                    .tokens
                    .get(probe)
                    .is_some_and(|token| token.text == "=>")
                {
                    self.bump(); // identifier
                    self.skip_trivia();
                    self.bump(); // '=>'
                    self.skip_trivia();
                    Some(NamePart {
                        text: name.text.to_owned(),
                        span: name.span(),
                    })
                } else {
                    None
                }
            } else {
                None
            };
            let value = self.parse_client_expression()?;
            let end = value.span().end;
            Some((named, value, SourceSpan { start, end }))
        })();
        let argument = result.map(|(name, value, span)| ClientCallArgument { name, value, span });
        self.builder.finish_node();
        argument
    }

    /// Parses one closed CLIENT state block body (work ADR 0069).
    ///
    /// The accepted shape is exactly:
    ///
    /// ```text
    /// IS
    ///     { STATE identifier type_spec [SCOPE (LOCAL | SESSION | USER)]
    ///       [DEFAULT expression] ; }
    /// BEGIN
    ///     { LET identifier [type_spec] := expression ; }
    ///     { identifier := expression ; }
    ///     RETURN [ expression ] ;
    /// END
    /// ```
    ///
    /// The subset is deliberately closed: only `STATE` declarations or `LET`
    /// local bindings may precede `BEGIN`; after `BEGIN`, `LET` and simple-name
    /// assignments are retained before exactly one `RETURN`. Every other
    /// procedural statement is rejected with a closed diagnostic. On failure
    /// the caller recovers to the statement terminator.
    fn parse_client_state_block(&mut self) -> Option<ClientFunctionBody> {
        self.builder
            .start_node(SyntaxKind::ClientStateBlockBody.into());
        let result = (|| {
            let block_start = self.current().expect("IS token exists").range.start;
            self.bump();
            let mut states = Vec::new();
            let mut locals = Vec::new();
            let mut statements = Vec::new();
            loop {
                self.skip_trivia();
                if self.current().is_some_and(|token| token.is_word("BEGIN")) {
                    break;
                }
                if self.current().is_some_and(|token| token.is_word("STATE")) {
                    let state = self.parse_client_state_declaration()?;
                    states.push(state);
                    continue;
                }
                if self.current().is_some_and(|token| token.is_word("LET")) {
                    let local = self.parse_client_local_binding()?;
                    locals.push(local);
                    continue;
                }
                if self.current().is_none() {
                    self.error_current(
                        "ORNA0001",
                        "expected BEGIN after CLIENT block declarations",
                    );
                    return None;
                }
                self.error_current(
                    "ORNA0001",
                    "CLIENT blocks accept only STATE or LET declarations before BEGIN",
                );
                return None;
            }
            self.bump(); // BEGIN
            let mut local_names = locals
                .iter()
                .map(|local| local.name.clone())
                .collect::<Vec<_>>();
            loop {
                self.skip_trivia();
                if self.current().is_some_and(|token| token.is_word("RETURN")) {
                    break;
                }
                let Some(token) = self.current() else {
                    self.error_current(
                        "ORNA0001",
                        "CLIENT blocks require exactly one RETURN statement",
                    );
                    return None;
                };
                if token.is_word("LET") {
                    let Some(mut statement) = self.parse_client_let_statement() else {
                        return None;
                    };
                    statement.expression =
                        rewrite_client_local_name_references(statement.expression, &local_names);
                    local_names.push(statement.name.clone());
                    statements.push(ClientProceduralStatement::Let(statement));
                    continue;
                }
                if token.is_identifier()
                    && self
                        .peek_significant(1)
                        .is_some_and(|next| next.text == ":")
                {
                    let Some(mut statement) = self.parse_client_assignment_statement() else {
                        return None;
                    };
                    statement.expression =
                        rewrite_client_local_name_references(statement.expression, &local_names);
                    statements.push(ClientProceduralStatement::Assignment(statement));
                    continue;
                }
                self.error_current(
                    "ORNA0001",
                    "CLIENT blocks accept only a single RETURN statement",
                );
                return None;
            }
            let return_expression = self.parse_client_state_return()?;
            let return_expression = return_expression
                .map(|expression| rewrite_client_local_name_references(expression, &local_names));
            self.skip_trivia();
            if self.current().is_some_and(|token| token.is_word("RETURN")) {
                self.error_current(
                    "ORNA0001",
                    "CLIENT blocks accept only a single RETURN statement",
                );
                return None;
            }
            let end_token = self.expect_word_token("END")?;
            Some(ClientFunctionBody::StateBlock(ClientStateBlockBody {
                states,
                locals,
                statements,
                return_expression,
                span: SourceSpan {
                    start: block_start,
                    end: end_token.range.end,
                },
            }))
        })();
        self.builder.finish_node();
        result
    }

    /// Parses one procedural `LET name type := expression;` binding.
    fn parse_client_local_binding(&mut self) -> Option<ClientLocalBinding> {
        self.builder
            .start_node(SyntaxKind::ClientLocalBinding.into());
        let result = (|| {
            let let_token = self.take_word("LET")?;
            self.skip_trivia();
            let name = self.expect_identifier("expected a local name after LET")?;
            self.skip_trivia();
            let type_start = self.current()?.range.start;
            let mut type_end = type_start;
            while self
                .current()
                .is_some_and(|token| !(token.kind == TokenKind::Other && token.text == ":"))
            {
                if self.current().is_some_and(|token| token.kind == TokenKind::Semicolon) {
                    self.error_current(
                        "ORNA0001",
                        "expected ':=' before the CLIENT local initializer",
                    );
                    return None;
                }
                if self.current().is_some_and(|token| token.text == "=") {
                    self.error_current(
                        "ORNA0001",
                        "CLIENT local bindings require a declared type and ':=' initializer",
                    );
                    return None;
                }
                let token = self.current().expect("type token exists");
                if !token.kind.is_trivia() {
                    type_end = token.range.end;
                }
                self.bump();
            }
            self.take_symbol(":")?;
            if type_start == type_end {
                self.error_current("ORNA0001", "expected a local type before ':='");
                return None;
            }
            if self.take_symbol("=").is_none() {
                self.error_current(
                    "ORNA0001",
                    "expected '=' after ':' in the CLIENT local binding",
                );
                return None;
            }
            self.skip_trivia();
            let expression = self.parse_client_expression()?;
            self.skip_trivia();
            let semicolon = self.expect_kind(
                TokenKind::Semicolon,
                "expected ';' after the CLIENT local binding",
            )?;
            Some(ClientLocalBinding {
                name,
                type_source: SourceSlice {
                    text: self.source[type_start..type_end].to_owned(),
                    span: SourceSpan {
                        start: type_start,
                        end: type_end,
                    },
                },
                expression,
                span: SourceSpan {
                    start: let_token.range.start,
                    end: semicolon.end,
                },
            })
        })();
        self.builder.finish_node();
        result
    }

    /// Parses one procedural `LET name [type] := expression;` statement.
    fn parse_client_let_statement(&mut self) -> Option<ClientLetStatement> {
        self.builder
            .start_node(SyntaxKind::ClientProceduralStatement.into());
        self.builder
            .start_node(SyntaxKind::ClientProceduralLetStatement.into());
        let result = (|| {
            let let_token = self.take_word("LET")?;
            self.skip_trivia();
            let name = self.expect_identifier("expected a local name after LET")?;
            self.skip_trivia();
            let type_source = if self.current().is_some_and(|token| token.text == ":") {
                None
            } else {
                let type_start = self.current()?.range.start;
                let mut type_end = type_start;
                while self
                    .current()
                    .is_some_and(|token| !(token.kind == TokenKind::Other && token.text == ":"))
                {
                    if self.current().is_some_and(|token| token.kind == TokenKind::Semicolon) {
                        self.error_current(
                            "ORNA0001",
                            "expected ':=' before the CLIENT local initializer",
                        );
                        return None;
                    }
                    let token = self.current().expect("type token exists");
                    if !token.kind.is_trivia() {
                        type_end = token.range.end;
                    }
                    self.bump();
                }
                if type_start == type_end {
                    self.error_current("ORNA0001", "expected a local type before ':='");
                    return None;
                }
                Some(SourceSlice {
                    text: self.source[type_start..type_end].to_owned(),
                    span: SourceSpan {
                        start: type_start,
                        end: type_end,
                    },
                })
            };
            self.take_symbol(":")?;
            if self.take_symbol("=").is_none() {
                self.error_current(
                    "ORNA0001",
                    "expected '=' after ':' in the CLIENT local binding",
                );
                return None;
            }
            self.skip_trivia();
            let expression = self.parse_client_expression()?;
            self.skip_trivia();
            let semicolon = self.expect_kind(
                TokenKind::Semicolon,
                "expected ';' after the CLIENT local binding",
            )?;
            Some(ClientLetStatement {
                name,
                type_source,
                expression,
                span: SourceSpan {
                    start: let_token.range.start,
                    end: semicolon.end,
                },
            })
        })();
        self.builder.finish_node();
        self.builder.finish_node();
        result
    }

    /// Parses one procedural `name := expression;` assignment.
    fn parse_client_assignment_statement(&mut self) -> Option<ClientAssignmentStatement> {
        self.builder
            .start_node(SyntaxKind::ClientProceduralStatement.into());
        self.builder
            .start_node(SyntaxKind::ClientAssignmentStatement.into());
        let result = (|| {
            let target = self.expect_identifier("expected an assignment target")?;
            let target_start = target.span.start;
            self.skip_trivia();
            self.take_symbol(":")?;
            if self.take_symbol("=").is_none() {
                self.error_current(
                    "ORNA0001",
                    "expected '=' after ':' in the CLIENT assignment",
                );
                return None;
            }
            self.skip_trivia();
            let expression = self.parse_client_expression()?;
            self.skip_trivia();
            let semicolon = self.expect_kind(
                TokenKind::Semicolon,
                "expected ';' after the CLIENT assignment",
            )?;
            Some(ClientAssignmentStatement {
                target,
                expression,
                span: SourceSpan {
                    start: target_start,
                    end: semicolon.end,
                },
            })
        })();
        self.builder.finish_node();
        self.builder.finish_node();
        result
    }

    /// Parses one `STATE` declaration inside a CLIENT state block.
    fn parse_client_state_declaration(&mut self) -> Option<StateDeclaration> {
        self.builder
            .start_node(SyntaxKind::ClientStateDeclaration.into());
        let result = (|| {
            let Some(state_token) = self.take_word("STATE") else {
                self.error_current("ORNA0001", "expected STATE before a state declaration");
                return None;
            };
            let start = state_token.range.start;
            self.skip_trivia();
            let name = self.expect_identifier("expected a state name after STATE")?;
            self.skip_trivia();
            let type_specification = self.parse_type_specification_with_message(
                "expected a state type after the state name",
            )?;
            self.skip_trivia();
            let scope = if self.current().is_some_and(|token| token.is_word("SCOPE")) {
                self.bump();
                self.skip_trivia();
                let Some(token) = self.current().cloned() else {
                    self.error_current("ORNA0001", "expected LOCAL, SESSION, or USER after SCOPE");
                    return None;
                };
                let scope = if token.is_word("LOCAL") {
                    StateScope::Local
                } else if token.is_word("SESSION") {
                    StateScope::Session
                } else if token.is_word("USER") {
                    StateScope::User
                } else {
                    self.error_current(
                        "ORNA0001",
                        "CLIENT state scope must be LOCAL, SESSION, or USER",
                    );
                    return None;
                };
                self.bump();
                scope
            } else {
                StateScope::Local
            };
            self.skip_trivia();
            let default = if self.current().is_some_and(|token| token.is_word("DEFAULT")) {
                self.bump();
                self.skip_trivia();
                if self.current().is_some_and(|token| token.is_word("NULL")) {
                    self.bump();
                    StateDefault::Null
                } else {
                    StateDefault::Expression(self.parse_client_expression()?)
                }
            } else {
                StateDefault::Unset
            };
            self.skip_trivia();
            let Some(semicolon) = self.take_kind(TokenKind::Semicolon) else {
                self.error_current(
                    "ORNA0001",
                    "expected ';' after the CLIENT state declaration",
                );
                return None;
            };
            Some(StateDeclaration {
                name,
                type_specification,
                scope,
                default,
                span: SourceSpan {
                    start,
                    end: semicolon.range.end,
                },
            })
        })();
        self.builder.finish_node();
        result
    }

    /// Parses the single `RETURN [ expression ] ;` statement of a state
    /// block.
    ///
    /// The caller has verified that the current token is `RETURN`. The
    /// result is the parsed return expression, or `None` when the statement
    /// names no expression.
    fn parse_client_state_return(&mut self) -> Option<Option<ClientExpression>> {
        self.builder
            .start_node(SyntaxKind::ClientStateReturnStatement.into());
        let result = (|| {
            self.bump(); // RETURN
            self.skip_trivia();
            if self.current().is_some_and(|token| token.kind == TokenKind::Semicolon) {
                self.bump();
                return Some(None);
            }
            let expression = self.parse_client_expression()?;
            self.skip_trivia();
            self
                .expect_kind(
                    TokenKind::Semicolon,
                    "expected ';' after the CLIENT state block RETURN statement",
                )?;
            Some(Some(expression))
        })();
        self.builder.finish_node();
        result
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
        let result = self.parse_function_parameter(order, "server function");
        self.builder.finish_node();
        result
    }

    fn parse_function_parameter(
        &mut self,
        order: usize,
        subject: &str,
    ) -> Option<ServerFunctionParameter> {
        (|| {
            let name = self.expect_identifier(&format!("expected a {subject} parameter name"))?;
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
            self.skip_trivia();
            let documentation = if self
                .current()
                .is_some_and(|token| token.is_word("DOCUMENTATION"))
            {
                let documentation = self.parse_documentation_modifier()?;
                end = documentation.span.end;
                Some(documentation)
            } else {
                None
            };
            Some(ServerFunctionParameter {
                name,
                order,
                type_specification,
                default_expression,
                documentation,
                span: SourceSpan { start, end },
            })
        })()
    }

    fn parse_function_return_type(&mut self) -> Option<FunctionReturnType> {
        self.parse_function_return_type_with_messages(
            "RETURNS TABLE is not supported; use RETURNS ROWS (...) for query-producing functions",
            "RETURNS SET OF is not supported; use RETURNS ROWS (...) for query-producing functions",
            "expected a field type",
        )
    }

    fn parse_client_function_return_type(&mut self) -> Option<FunctionReturnType> {
        if self.current().is_some_and(|token| token.is_word("RETURN")) {
            self.error_current(
                "ORNA0001",
                "CLIENT functions must name one return type after RETURNS",
            );
            return None;
        }
        self.parse_function_return_type_with_messages(
            "CLIENT functions must name one return type after RETURNS",
            "CLIENT functions must name one return type after RETURNS",
            "CLIENT functions must name one return type after RETURNS",
        )
    }

    fn parse_function_return_type_with_messages(
        &mut self,
        table_message: &str,
        set_message: &str,
        type_message: &str,
    ) -> Option<FunctionReturnType> {
        if self.current().is_some_and(|token| token.is_word("TABLE")) {
            self.error_current("ORNA0001", table_message);
            return None;
        }
        if self.current().is_some_and(|token| token.is_word("SET"))
            && self
                .peek_significant(1)
                .is_some_and(|token| token.is_word("OF"))
        {
            self.error_current("ORNA0001", set_message);
            return None;
        }
        if self.current().is_none() {
            self.error_current("ORNA0001", type_message);
            return None;
        }
        if self.current().is_some_and(|token| token.is_word("STREAM")) {
            return self.parse_stream_return_type(type_message);
        }
        if !self.current().is_some_and(|token| token.is_word("ROWS")) {
            return self
                .parse_type_specification_with_message(type_message)
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

    fn parse_stream_return_type(&mut self, type_message: &str) -> Option<FunctionReturnType> {
        let (element, span) = self.parse_stream_type_parts(
            type_message,
            0,
            32,
            SyntaxKind::StreamReturnType,
        )?;
        Some(FunctionReturnType::Stream {
            element: *element,
            span,
        })
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

    fn parse_capability_clause(&mut self) -> Option<Vec<CapabilitySpecification>> {
        self.builder.start_node(SyntaxKind::CapabilityClause.into());
        let result = (|| {
            self.expect_word("REQUIRES");
            self.skip_trivia();
            if !self.expect_word("CAPABILITY") {
                return None;
            }
            self.skip_trivia();

            let mut capabilities = Vec::new();
            if self.current().is_none() || self.current().is_some_and(Self::ends_capability_clause)
            {
                self.error_current(
                    "ORNA0001",
                    "expected a capability after REQUIRES CAPABILITY",
                );
                return None;
            }
            capabilities.push(self.parse_capability_specification()?);

            loop {
                self.skip_trivia();
                if self
                    .current()
                    .is_some_and(|token| token.is_kind(TokenKind::Comma))
                {
                    self.bump();
                    self.skip_trivia();
                    if self.current().is_none()
                        || self.current().is_some_and(Self::ends_capability_clause)
                    {
                        self.error_current(
                            "ORNA0001",
                            "trailing commas are not allowed in capability requirements",
                        );
                        return None;
                    }
                    capabilities.push(self.parse_capability_specification()?);
                    continue;
                }
                if self.current().is_some_and(Self::ends_capability_clause) {
                    return Some(capabilities);
                }
                self.error_current(
                    "ORNA0001",
                    "expected ',' or a body keyword after a capability requirement",
                );
                return None;
            }
        })();
        self.builder.finish_node();
        result
    }

    /// Whether the current token ends a capability clause: the statement
    /// terminator or the keyword that introduces the function body.
    ///
    /// SERVER bodies follow `AS`; CLIENT bodies follow `RETURN` (or the
    /// documented `IS` long form).
    fn ends_capability_clause(token: &Token<'source>) -> bool {
        token.is_kind(TokenKind::Semicolon)
            || token.is_word("AS")
            || token.is_word("IS")
            || token.is_word("RETURN")
    }

    fn parse_capability_specification(&mut self) -> Option<CapabilitySpecification> {
        self.builder
            .start_node(SyntaxKind::CapabilitySpecification.into());
        let result = (|| {
            let name = self.parse_qualified_name("expected a capability name")?;
            let start = name.span.start;
            let mut end = name.span.end;

            self.skip_trivia();
            let arguments = if self
                .current()
                .is_some_and(|token| token.is_kind(TokenKind::LeftParenthesis))
            {
                Some(self.parse_capability_arguments()?)
            } else {
                None
            };
            if let Some((_, closing_end)) = &arguments {
                end = *closing_end;
            }

            Some(CapabilitySpecification {
                name,
                arguments: arguments.map(|(arguments, _)| arguments),
                span: SourceSpan { start, end },
            })
        })();
        self.builder.finish_node();
        result
    }

    fn parse_capability_arguments(&mut self) -> Option<(SourceSlice, usize)> {
        self.builder
            .start_node(SyntaxKind::CapabilityArguments.into());
        let result = (|| {
            let opening = self.current().cloned().expect("opening parenthesis exists");
            self.bump();
            let start = opening.range.end;
            let mut depth = 1usize;

            while let Some(token) = self.current().cloned() {
                match token.kind {
                    TokenKind::LeftParenthesis => {
                        depth += 1;
                        self.bump();
                    }
                    TokenKind::RightParenthesis => {
                        depth -= 1;
                        if depth == 0 {
                            let end = token.range.start;
                            self.bump();
                            return Some((
                                SourceSlice {
                                    text: self.source[start..end].to_owned(),
                                    span: SourceSpan { start, end },
                                },
                                token.range.end,
                            ));
                        }
                        self.bump();
                    }
                    TokenKind::Semicolon => {
                        self.error_current(
                            "ORNA0001",
                            "expected ')' to close capability arguments",
                        );
                        return None;
                    }
                    _ => self.bump(),
                }
            }

            self.error_current("ORNA0001", "expected ')' to close capability arguments");
            None
        })();
        self.builder.finish_node();
        result
    }

    fn collect_sql_body(
        &mut self,
        empty_body_message: &str,
    ) -> Option<(SourceSlice, (usize, usize))> {
        let first = self.current()?;
        if first.is_kind(TokenKind::Semicolon) {
            self.error_current("ORNA0001", empty_body_message);
            return None;
        }
        let start = first.range.start;
        let mut end = start;
        let body_token_start = self.index;

        while let Some(token) = self.current().cloned() {
            if token.is_kind(TokenKind::Semicolon) {
                break;
            }
            if !token.kind.is_trivia() {
                end = token.range.end;
            }
            self.bump();
        }
        if end == start {
            self.error_current("ORNA0001", empty_body_message);
            return None;
        }
        Some((
            SourceSlice {
                text: self.source[start..end].to_owned(),
                span: SourceSpan { start, end },
            },
            (body_token_start, self.index),
        ))
    }

    fn parse_server_function_body(&mut self) -> Option<ServerFunctionBody> {
        let is_insert = self.current().is_some_and(|token| token.is_word("INSERT"));
        let is_update = self.current().is_some_and(|token| token.is_word("UPDATE"));
        let is_delete = self.current().is_some_and(|token| token.is_word("DELETE"));
        let syntax_kind = if is_insert {
            SyntaxKind::SqlInsertBody
        } else if is_update {
            SyntaxKind::SqlUpdateBody
        } else if is_delete {
            SyntaxKind::SqlDeleteBody
        } else {
            SyntaxKind::SqlQueryBody
        };
        self.builder.start_node(syntax_kind.into());
        let result = (|| {
            let (source, (body_token_start, body_token_end)) =
                self.collect_sql_body(if is_insert {
                    "expected a SQL INSERT after AS"
                } else if is_update {
                    "expected an UPDATE after AS"
                } else if is_delete {
                    "expected a DELETE after AS"
                } else {
                    "expected a SQL query after AS"
                })?;
            let body_tokens = &self.tokens[body_token_start..body_token_end];
            if is_insert {
                let insert = match parse_sql_insert(body_tokens) {
                    Ok(insert) => insert,
                    Err(error) => {
                        self.diagnostics.push(Diagnostic {
                            code: error.code,
                            message: error.message,
                            span: error.span,
                        });
                        return None;
                    }
                };
                Some(ServerFunctionBody::SqlInsert(SqlInsertBody {
                    source,
                    insert,
                }))
            } else if is_update {
                let update = match parse_sql_update(body_tokens) {
                    Ok(update) => update,
                    Err(error) => {
                        self.diagnostics.push(Diagnostic {
                            code: error.code,
                            message: error.message,
                            span: error.span,
                        });
                        return None;
                    }
                };
                Some(ServerFunctionBody::SqlUpdate(SqlUpdateBody {
                    source,
                    update,
                }))
            } else if is_delete {
                let delete = match parse_sql_delete(body_tokens) {
                    Ok(delete) => delete,
                    Err(error) => {
                        self.diagnostics.push(Diagnostic {
                            code: error.code,
                            message: error.message,
                            span: error.span,
                        });
                        return None;
                    }
                };
                Some(ServerFunctionBody::SqlDelete(SqlDeleteBody {
                    source,
                    delete,
                }))
            } else if let Some(identifier) = no_input_parameter_identifier(body_tokens) {
                Some(ServerFunctionBody::NoInputParameterSelect(
                    NoInputParameterSelectBody {
                        source,
                        parameter: NamePart {
                            text: identifier.text.to_owned(),
                            span: identifier.span(),
                        },
                    },
                ))
            } else {
                let query = match parse_select_query(body_tokens) {
                    Ok(query) => query,
                    Err(error) => {
                        self.diagnostics.push(Diagnostic {
                            code: error.code,
                            message: error.message,
                            span: error.span,
                        });
                        return None;
                    }
                };
                Some(ServerFunctionBody::SqlQuery(SqlQueryBody { source, query }))
            }
        })();
        self.builder.finish_node();
        result
    }

    fn parse_create_type_statement(&mut self) {
        let statement_start = self.current().expect("CREATE token exists").range.start;
        self.builder
            .start_node(SyntaxKind::CreateTypeStatement.into());

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
        if self.take_word("OBJECT").is_some() {
            self.parse_create_object_type_body(statement_start, name);
        } else if self.take_word("ENUM").is_some() {
            self.parse_create_enum_type_body(statement_start, name);
        } else if self.take_word("VALUE").is_some() {
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::LeftParenthesis)
            {
                self.parse_create_record_value_type_body(statement_start, name);
            } else if self.current().is_some_and(|token| token.is_word("OPAQUE")) {
                self.parse_create_opaque_value_type_body(statement_start, name);
            } else {
                self.parse_create_primitive_value_type_body(statement_start, name);
            }
        } else {
            self.error_current("ORNA0001", "expected OBJECT, ENUM, or VALUE after AS");
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.builder.finish_node();
    }

    fn parse_create_object_type_body(
        &mut self,
        statement_start: usize,
        name: Option<QualifiedName>,
    ) {
        self.skip_trivia();
        if self
            .expect_kind(TokenKind::LeftParenthesis, "expected '(' after AS OBJECT")
            .is_none()
        {
            self.recover_statement();
            return;
        }

        let fields = self.parse_object_fields();
        self.skip_trivia();
        let mut final_type = false;
        let mut documentation = None;
        loop {
            self.skip_trivia();
            if self.take_word("FINAL").is_some() {
                final_type = true;
            } else if self
                .current()
                .is_some_and(|token| token.is_word("DOCUMENTATION"))
            {
                documentation = self.parse_documentation_modifier();
            } else {
                break;
            }
        }
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
                    final_type,
                    documentation,
                    span: SourceSpan {
                        start: statement_start,
                        end: semicolon.end,
                    },
                })
            }
            (_, _, None) => self.recover_statement(),
            _ => {}
        }
    }

    fn parse_create_enum_type_body(&mut self, statement_start: usize, name: Option<QualifiedName>) {
        self.skip_trivia();
        if self
            .expect_kind(TokenKind::LeftParenthesis, "expected '(' after AS ENUM")
            .is_none()
        {
            self.recover_statement();
            return;
        }

        let mut labels = Vec::new();
        loop {
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
            {
                let message = if labels.is_empty() {
                    "enum type must declare at least one label"
                } else {
                    "enum type cannot have a trailing comma"
                };
                self.error_current("ORNA0001", message);
                self.recover_statement();
                return;
            }
            let Some(label) = self
                .current()
                .cloned()
                .filter(|token| token.kind == TokenKind::StringLiteral)
            else {
                self.error_current("ORNA0001", "expected a string literal enum label");
                self.recover_statement();
                return;
            };
            self.bump();
            labels.push(EnumLabelDeclaration {
                literal: SourceSlice {
                    text: label.text.to_owned(),
                    span: label.span(),
                },
            });

            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
            {
                self.bump();
                break;
            }
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Comma)
            {
                self.bump();
                continue;
            }
            let message = if self.current().is_none()
                || self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::Semicolon)
            {
                "expected ')' after enum labels"
            } else {
                "expected ',' or ')' after enum label"
            };
            self.error_current("ORNA0001", message);
            self.recover_statement();
            return;
        }

        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after enum type declaration",
        ) else {
            self.recover_statement();
            return;
        };
        if let Some(name) = name {
            self.enum_types.push(EnumTypeDeclaration {
                name,
                labels,
                span: SourceSpan {
                    start: statement_start,
                    end: semicolon.end,
                },
            });
        }
    }

    fn parse_create_primitive_value_type_body(
        &mut self,
        statement_start: usize,
        name: Option<QualifiedName>,
    ) {
        self.skip_trivia();
        if !self.expect_word("PRIMITIVE") {
            self.recover_statement();
            return;
        }
        let Some((kernel_contract, kernel_contract_modifier_span)) =
            self.parse_kernel_contract("expected keyword KERNEL", "expected keyword CONTRACT")
        else {
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        if !self.expect_word("IMMUTABLE") {
            self.recover_statement();
            return;
        }
        self.skip_trivia();
        let (persistence, persistence_span) = if let Some(token) = self.take_word("PERSISTABLE") {
            (PrimitiveValueTypePersistence::Persistable, token.span())
        } else if let Some(token) = self.take_word("TRANSIENT") {
            (PrimitiveValueTypePersistence::Transient, token.span())
        } else {
            self.error_current(
                "ORNA0001",
                "expected PERSISTABLE or TRANSIENT after IMMUTABLE",
            );
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        let documentation = self.parse_documentation_modifier();
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after primitive value type declaration",
        ) else {
            self.recover_statement();
            return;
        };
        if let Some(name) = name {
            self.primitive_value_types
                .push(PrimitiveValueTypeDeclaration {
                    name,
                    kernel_contract,
                    kernel_contract_modifier_span,
                    persistence,
                    persistence_span,
                    documentation,
                    span: SourceSpan {
                        start: statement_start,
                        end: semicolon.end,
                    },
                });
        }
    }

    fn parse_create_opaque_value_type_body(
        &mut self,
        statement_start: usize,
        name: Option<QualifiedName>,
    ) {
        self.skip_trivia();
        let Some(opaque) = self.take_word("OPAQUE") else {
            self.error_current("ORNA0001", "expected OPAQUE after AS VALUE");
            self.recover_statement();
            return;
        };
        let Some((kernel_contract, kernel_contract_modifier_span)) = self.parse_kernel_contract(
            "expected KERNEL after OPAQUE",
            "expected CONTRACT after KERNEL",
        ) else {
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        let Some(immutable) = self.take_word("IMMUTABLE") else {
            self.error_current("ORNA0001", "expected IMMUTABLE after opaque codec contract");
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        let Some(transient) = self.take_word("TRANSIENT") else {
            self.error_current("ORNA0001", "expected TRANSIENT after IMMUTABLE");
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        let documentation = self.parse_documentation_modifier();
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after opaque value type declaration",
        ) else {
            self.recover_statement();
            return;
        };
        if let Some(name) = name {
            self.opaque_value_types.push(OpaqueValueTypeDeclaration {
                name,
                kernel_contract,
                opaque_span: opaque.span(),
                kernel_contract_modifier_span,
                immutable_span: immutable.span(),
                transient_span: transient.span(),
                documentation,
                span: SourceSpan {
                    start: statement_start,
                    end: semicolon.end,
                },
            });
        }
    }

    /// Parses one `DOCUMENTATION '...'` modifier and returns its string text.
    fn parse_documentation_modifier(&mut self) -> Option<SourceSlice> {
        self.skip_trivia();
        self.take_word("DOCUMENTATION")?;
        self.skip_trivia();
        let Some(literal) = self
            .current()
            .cloned()
            .filter(|token| token.kind == TokenKind::StringLiteral)
        else {
            self.error_current("ORNA0001", "expected a string literal after DOCUMENTATION");
            return None;
        };
        self.bump();
        Some(SourceSlice {
            text: literal.text.to_owned(),
            span: literal.span(),
        })
    }

    fn parse_kernel_contract(
        &mut self,
        missing_kernel: &str,
        missing_contract: &str,
    ) -> Option<(SourceSlice, SourceSpan)> {
        self.skip_trivia();
        let Some(kernel) = self.take_word("KERNEL") else {
            self.error_current("ORNA0001", missing_kernel);
            return None;
        };
        self.skip_trivia();
        let Some(contract) = self.take_word("CONTRACT") else {
            self.error_current("ORNA0001", missing_contract);
            return None;
        };
        let modifier_span = SourceSpan {
            start: kernel.range.start,
            end: contract.range.end,
        };
        self.skip_trivia();
        let Some(contract_literal) = self
            .current()
            .cloned()
            .filter(|token| token.kind == TokenKind::StringLiteral)
        else {
            self.error_current(
                "ORNA0001",
                "expected a string literal after KERNEL CONTRACT",
            );
            return None;
        };
        self.bump();
        Some((
            SourceSlice {
                text: contract_literal.text.to_owned(),
                span: contract_literal.span(),
            },
            modifier_span,
        ))
    }

    fn parse_create_record_value_type_body(
        &mut self,
        statement_start: usize,
        name: Option<QualifiedName>,
    ) {
        if self
            .expect_kind(TokenKind::LeftParenthesis, "expected '(' after AS VALUE")
            .is_none()
        {
            self.recover_statement();
            return;
        }
        let Some(fields) = self.parse_value_fields() else {
            self.recover_statement();
            return;
        };

        self.skip_trivia();
        let Some(immutable) = self.expect_word_token("IMMUTABLE") else {
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        let Some(persistable) = self.expect_word_token("PERSISTABLE") else {
            self.recover_statement();
            return;
        };
        self.skip_trivia();
        let documentation = self.parse_documentation_modifier();
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after record value type declaration",
        ) else {
            self.recover_statement();
            return;
        };

        if let Some(name) = name {
            self.record_value_types.push(RecordValueTypeDeclaration {
                name,
                fields,
                immutable_span: immutable.span(),
                persistable_span: persistable.span(),
                documentation,
                span: SourceSpan {
                    start: statement_start,
                    end: semicolon.end,
                },
            });
        }
    }

    fn parse_export_type_statement(&mut self) {
        let statement_start = self.current().expect("EXPORT token exists").range.start;
        self.builder
            .start_node(SyntaxKind::ExportTypeStatement.into());

        self.expect_word("EXPORT");
        self.skip_trivia();
        if !self.expect_word("TYPE") {
            self.recover_statement();
            self.builder.finish_node();
            return;
        }
        self.skip_trivia();
        let Some(source_type) = self.parse_qualified_name("expected a type name after EXPORT TYPE")
        else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        let target = if self.take_word("AS").is_some() {
            self.skip_trivia();
            self.parse_qualified_name("expected a qualified type name after AS")
                .map(|name| TypeExportTarget::Qualified { name })
        } else if let Some(to) = self.take_word("TO") {
            self.skip_trivia();
            let Some(prelude) = self.expect_word_token("PRELUDE") else {
                self.recover_statement();
                self.builder.finish_node();
                return;
            };
            let modifier_span = SourceSpan {
                start: to.range.start,
                end: prelude.range.end,
            };
            self.skip_trivia();
            if !self.expect_word("AS") {
                self.recover_statement();
                self.builder.finish_node();
                return;
            }
            self.skip_trivia();
            let mut words = Vec::new();
            while self.current().is_some_and(|token| {
                token.kind == TokenKind::Word
                    && !(token.is_word("CREATE")
                        || token.is_word("ALTER")
                        || token.is_word("EXPORT"))
            }) {
                let token = self.current().expect("word token exists").clone();
                self.bump();
                words.push(NamePart {
                    text: token.text.to_owned(),
                    span: token.span(),
                });
                self.skip_trivia();
            }
            let Some(first) = words.first() else {
                self.error_current(
                    "ORNA0001",
                    "expected an unquoted prelude type name after AS",
                );
                self.recover_statement();
                self.builder.finish_node();
                return;
            };
            let name_span = SourceSpan {
                start: first.span.start,
                end: words.last().expect("prelude word exists").span.end,
            };
            Some(TypeExportTarget::Prelude {
                words,
                name_span,
                modifier_span,
            })
        } else {
            self.error_current("ORNA0001", "expected AS or TO after exported type name");
            None
        };
        let Some(target) = target else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.skip_trivia();
        let Some(semicolon) = self.expect_kind(
            TokenKind::Semicolon,
            "expected ';' after type export declaration",
        ) else {
            self.recover_statement();
            self.builder.finish_node();
            return;
        };
        self.type_exports.push(TypeExportDeclaration {
            source_type,
            target,
            span: SourceSpan {
                start: statement_start,
                end: semicolon.end,
            },
        });
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

    fn parse_value_fields(&mut self) -> Option<Vec<ValueFieldDeclaration>> {
        let mut fields = Vec::new();

        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
        {
            self.error_current(
                "ORNA0001",
                "record value type must declare at least one field",
            );
            return None;
        }

        loop {
            let field = self.parse_value_field(fields.len())?;
            fields.push(field);
            self.skip_trivia();

            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
            {
                self.bump();
                return Some(fields);
            }
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Comma)
            {
                self.bump();
                self.skip_trivia();
                if self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
                {
                    self.bump();
                    return Some(fields);
                }
                continue;
            }

            let message = if self.current().is_none()
                || self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::Semicolon)
            {
                "expected ')' after record value fields"
            } else {
                "expected ',' or ')' after record value field"
            };
            self.error_current("ORNA0001", message);
            return None;
        }
    }

    fn parse_value_field(&mut self, order: usize) -> Option<ValueFieldDeclaration> {
        self.builder.start_node(SyntaxKind::ValueField.into());
        let result = (|| {
            let name = self.expect_identifier("expected a record value field name")?;
            let field_start = name.span.start;
            self.skip_trivia();
            if self.current().is_some_and(|token| token.is_word("REF")) {
                self.error_current("ORNA0001", "record value fields cannot use REF");
                return None;
            }
            let type_specification =
                self.parse_type_specification_with_message("expected a record value field type")?;
            let field_end = type_specification.span().end;
            self.skip_trivia();
            let documentation = self.parse_documentation_modifier();
            self.skip_trivia();
            if self.current().is_some_and(|token| {
                token.is_word("DEFAULT")
                    || token.is_word("CHECK")
                    || token.is_word("NULL")
                    || token.is_word("NOT")
                    || token.is_word("UNIQUE")
                    || token.is_word("ON")
                    || token.is_word("PRIMARY")
            }) {
                self.error_current("ORNA0001", "record value fields do not accept modifiers");
                return None;
            }
            let field_end = documentation
                .as_ref()
                .map_or(field_end, |documentation| documentation.span.end);
            Some(ValueFieldDeclaration {
                name,
                order,
                type_specification,
                documentation,
                span: SourceSpan {
                    start: field_start,
                    end: field_end,
                },
            })
        })();
        self.builder.finish_node();
        result
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
        let mut documentation = None;
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
            } else if token.is_word("DOCUMENTATION") {
                let Some(documentation_slice) = self.parse_documentation_modifier() else {
                    self.builder.finish_node();
                    return None;
                };
                field_end = documentation_slice.span.end;
                documentation = Some(documentation_slice);
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
            documentation,
            span: SourceSpan {
                start: field_start,
                end: field_end,
            },
        })
    }

    fn parse_type_specification(&mut self) -> Option<TypeSpecification> {
        self.parse_type_specification_with_message("expected a field type")
    }

    fn parse_type_specification_with_message(
        &mut self,
        named_type_message: &str,
    ) -> Option<TypeSpecification> {
        self.parse_type_specification_at_depth(named_type_message, 0)
    }

    fn parse_type_specification_at_depth(
        &mut self,
        named_type_message: &str,
        depth: usize,
    ) -> Option<TypeSpecification> {
        const MAXIMUM_DEPTH: usize = 32;

        if depth > MAXIMUM_DEPTH {
            self.error_current(
                "ORNA0001",
                "type specification exceeds the maximum depth of 32",
            );
            return None;
        }

        if self.current().is_some_and(|token| token.is_word("REF")) {
            self.builder
                .start_node(SyntaxKind::ReferenceTypeSpecification.into());
            let start = self.current().expect("REF token exists").range.start;
            self.bump();
            self.skip_trivia();
            let target = self.parse_type_specification_at_depth(
                "expected a type specification after REF",
                depth + 1,
            );
            self.builder.finish_node();
            let target = target?;
            let specification = TypeSpecification::Reference {
                span: SourceSpan {
                    start,
                    end: target.span().end,
                },
                target: Box::new(target),
            };
            return self.parse_postfix_options(specification, depth, MAXIMUM_DEPTH);
        }

        if self.current().is_some_and(|token| {
            token.is_word("LIST")
                || token.is_word("SET")
                || token.is_word("MAP")
                || token.is_word("OPTION")
                || token.is_word("STREAM")
        }) {
            let specification =
                self.parse_prefix_type_specification(named_type_message, depth, MAXIMUM_DEPTH)?;
            return self.parse_postfix_options(specification, depth, MAXIMUM_DEPTH);
        }

        let specification = if let Some(specification) = self.parse_multiword_standard_scalar() {
            specification
        } else {
            self.builder
                .start_node(SyntaxKind::NamedTypeSpecification.into());
            let specification = self
                .parse_qualified_name(named_type_message)
                .map(TypeSpecification::Named);
            self.builder.finish_node();
            specification?
        };
        self.parse_postfix_options(specification, depth, MAXIMUM_DEPTH)
    }

    fn parse_stream_type_parts(
        &mut self,
        named_type_message: &str,
        depth: usize,
        maximum_depth: usize,
        syntax_kind: SyntaxKind,
    ) -> Option<(Box<TypeSpecification>, SourceSpan)> {
        let constructor = self.current().expect("STREAM constructor exists").clone();
        self.builder.start_node(syntax_kind.into());
        let start = constructor.range.start;
        self.bump();
        self.skip_trivia();
        if self.take_symbol("<").is_none() {
            self.error_current("ORNA0001", "expected '<' after type constructor");
            self.builder.finish_node();
            return None;
        }
        self.skip_trivia();

        let Some(element) = self.parse_type_specification_at_depth(named_type_message, depth + 1)
        else {
            self.recover_type_constructor();
            self.builder.finish_node();
            return None;
        };
        self.skip_trivia();

        let Some(end) = self.take_symbol(">").map(|token| token.range.end) else {
            self.error_current("ORNA0001", "expected '>' to close type constructor");
            self.recover_type_constructor();
            self.builder.finish_node();
            return None;
        };
        self.builder.finish_node();
        let span = SourceSpan { start, end };
        debug_assert!(depth + type_specification_depth(&element) + 1 <= maximum_depth);
        Some((Box::new(element), span))
    }

    fn parse_prefix_type_specification(
        &mut self,
        named_type_message: &str,
        depth: usize,
        maximum_depth: usize,
    ) -> Option<TypeSpecification> {
        let constructor = self.current().expect("type constructor exists").clone();
        if constructor.is_word("STREAM") {
            let (element, span) = self.parse_stream_type_parts(
                named_type_message,
                depth,
                maximum_depth,
                SyntaxKind::StreamTypeSpecification,
            )?;
            return Some(TypeSpecification::Stream { element, span });
        }
        let syntax_kind = if constructor.is_word("LIST") {
            SyntaxKind::ListTypeSpecification
        } else if constructor.is_word("SET") {
            SyntaxKind::SetTypeSpecification
        } else if constructor.is_word("MAP") {
            SyntaxKind::MapTypeSpecification
        } else if constructor.is_word("OPTION") {
            SyntaxKind::OptionTypeSpecification
        } else {
            SyntaxKind::StreamTypeSpecification
        };
        self.builder.start_node(syntax_kind.into());
        let start = constructor.range.start;
        self.bump();
        self.skip_trivia();
        if self.take_symbol("<").is_none() {
            self.error_current("ORNA0001", "expected '<' after type constructor");
            self.builder.finish_node();
            return None;
        }
        self.skip_trivia();

        let Some(first) = self.parse_type_specification_at_depth(named_type_message, depth + 1)
        else {
            self.recover_type_constructor();
            self.builder.finish_node();
            return None;
        };
        self.skip_trivia();

        let second = if constructor.is_word("MAP") {
            if self.take_kind(TokenKind::Comma).is_none() {
                self.error_current("ORNA0001", "expected ',' between MAP key and value types");
                self.recover_type_constructor();
                self.builder.finish_node();
                return None;
            }
            self.skip_trivia();
            let Some(second) =
                self.parse_type_specification_at_depth(named_type_message, depth + 1)
            else {
                self.recover_type_constructor();
                self.builder.finish_node();
                return None;
            };
            self.skip_trivia();
            Some(second)
        } else {
            None
        };

        let Some(end) = self.take_symbol(">").map(|token| token.range.end) else {
            self.error_current("ORNA0001", "expected '>' to close type constructor");
            self.recover_type_constructor();
            self.builder.finish_node();
            return None;
        };
        self.builder.finish_node();
        let span = SourceSpan { start, end };
        let specification = if constructor.is_word("LIST") {
            TypeSpecification::List {
                element: Box::new(first),
                span,
            }
        } else if constructor.is_word("SET") {
            TypeSpecification::Set {
                element: Box::new(first),
                span,
            }
        } else if constructor.is_word("MAP") {
            TypeSpecification::Map {
                key: Box::new(first),
                value: Box::new(second.expect("MAP value exists")),
                span,
            }
        } else if constructor.is_word("OPTION") {
            TypeSpecification::Option {
                value: Box::new(first),
                spelling: OptionTypeSpelling::Prefix,
                span,
            }
        } else {
            TypeSpecification::Stream {
                element: Box::new(first),
                span,
            }
        };
        debug_assert!(depth + type_specification_depth(&specification) <= maximum_depth);
        Some(specification)
    }

    fn parse_postfix_options(
        &mut self,
        mut specification: TypeSpecification,
        enclosing_depth: usize,
        maximum_depth: usize,
    ) -> Option<TypeSpecification> {
        loop {
            self.skip_trivia();
            if !self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Other && token.text == "?")
            {
                return Some(specification);
            }
            if enclosing_depth + type_specification_depth(&specification) >= maximum_depth {
                self.error_current(
                    "ORNA0001",
                    "type specification exceeds the maximum depth of 32",
                );
                return None;
            }
            self.builder
                .start_node(SyntaxKind::OptionTypeSpecification.into());
            let end = self.current().expect("postfix option exists").range.end;
            self.bump();
            self.builder.finish_node();
            let start = specification.span().start;
            specification = TypeSpecification::Option {
                value: Box::new(specification),
                spelling: OptionTypeSpelling::Postfix,
                span: SourceSpan { start, end },
            };
        }
    }

    fn recover_type_constructor(&mut self) {
        let mut nested = 0usize;
        while let Some(token) = self.current().cloned() {
            if token.kind == TokenKind::Semicolon || token.kind == TokenKind::RightParenthesis {
                break;
            }
            if token.kind == TokenKind::Other && token.text == "<" {
                nested += 1;
            } else if token.kind == TokenKind::Other && token.text == ">" {
                self.bump();
                if nested == 0 {
                    break;
                }
                nested -= 1;
                continue;
            }
            self.bump();
        }
    }

    fn parse_multiword_standard_scalar(&mut self) -> Option<TypeSpecification> {
        let first = self.current()?.clone();
        let kind = if first.is_word("CHARACTER") {
            StandardLargeObjectKind::Character
        } else if first.is_word("BINARY") {
            StandardLargeObjectKind::Binary
        } else {
            return None;
        };
        if !self
            .peek_significant(1)
            .is_some_and(|token| token.is_word("LARGE"))
            || !self
                .peek_significant(2)
                .is_some_and(|token| token.is_word("OBJECT"))
        {
            return None;
        }

        self.builder
            .start_node(SyntaxKind::StandardLargeObjectTypeSpecification.into());
        let start = first.range.start;
        self.take_word(first.text)
            .expect("first scalar token exists");
        self.skip_trivia();
        self.take_word("LARGE").expect("LARGE scalar token exists");
        self.skip_trivia();
        let object = self
            .take_word("OBJECT")
            .expect("OBJECT scalar token exists");
        self.builder.finish_node();

        Some(TypeSpecification::StandardLargeObject {
            kind,
            source: SourceSlice {
                text: self.source[start..object.range.end].to_owned(),
                span: SourceSpan {
                    start,
                    end: object.range.end,
                },
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
        self.parse_qualified_name_with_messages(
            first_identifier_message,
            "expected a name after '.'",
            None,
        )
    }

    fn parse_qualified_name_with_messages(
        &mut self,
        first_identifier_message: &str,
        next_identifier_message: &str,
        stop_word: Option<&str>,
    ) -> Option<QualifiedName> {
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
            if stop_word.is_some_and(|word| self.current().is_some_and(|token| token.is_word(word)))
            {
                self.error_current("ORNA0001", next_identifier_message);
                self.builder.finish_node();
                return None;
            }
            match self.expect_identifier(next_identifier_message) {
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

    fn take_kind(&mut self, expected: TokenKind) -> Option<Token<'source>> {
        let token = self.current().cloned()?;
        if token.kind == expected {
            self.bump();
            Some(token)
        } else {
            None
        }
    }

    fn take_symbol(&mut self, expected: &str) -> Option<Token<'source>> {
        let token = self.current().cloned()?;
        if token.kind == TokenKind::Other && token.text == expected {
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
        if token.kind == TokenKind::QuotedIdentifier
            && self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ORNA0002" && diagnostic.span == token.span())
        {
            self.bump();
            return None;
        }
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
            if token.is_word("CREATE") || token.is_word("ALTER") || token.is_word("EXPORT") {
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

fn rewrite_client_local_name_references(
    expression: ClientExpression,
    locals: &[NamePart],
) -> ClientExpression {
    let semantic_name = |name: &NamePart| {
        if name.text.starts_with('"') {
            name.text[1..name.text.len() - 1].replace("\"\"", "\"")
        } else {
            name.text.to_lowercase()
        }
    };
    let is_local = |name: &NamePart| {
        locals
            .iter()
            .any(|local| semantic_name(local) == semantic_name(name))
    };
    match expression {
        ClientExpression::ParameterRead { parameter } if is_local(&parameter) => {
            ClientExpression::LocalRead { local: parameter }
        }
        ClientExpression::Call {
            callee,
            arguments,
            span,
        } => ClientExpression::Call {
            callee,
            arguments: arguments
                .into_iter()
                .map(|argument| ClientCallArgument {
                    value: rewrite_client_local_name_references(argument.value, locals),
                    ..argument
                })
                .collect(),
            span,
        },
        ClientExpression::Await { expression, span } => ClientExpression::Await {
            expression: Box::new(rewrite_client_local_name_references(*expression, locals)),
            span,
        },
        ClientExpression::Concat { left, right, span } => ClientExpression::Concat {
            left: Box::new(rewrite_client_local_name_references(*left, locals)),
            right: Box::new(rewrite_client_local_name_references(*right, locals)),
            span,
        },
        other => other,
    }
}

fn type_specification_depth(specification: &TypeSpecification) -> usize {
    match specification {
        TypeSpecification::Named(_) | TypeSpecification::StandardLargeObject { .. } => 0,
        TypeSpecification::Reference { target, .. } => type_specification_depth(target) + 1,
        TypeSpecification::List { element, .. }
        | TypeSpecification::Set { element, .. }
        | TypeSpecification::Stream { element, .. } => type_specification_depth(element) + 1,
        TypeSpecification::Map { key, value, .. } => {
            type_specification_depth(key).max(type_specification_depth(value)) + 1
        }
        TypeSpecification::Option { value, .. } => type_specification_depth(value) + 1,
    }
}

struct QueryParseError {
    code: &'static str,
    message: String,
    span: SourceSpan,
}

struct ParsedReference {
    alias: NamePart,
    span: SourceSpan,
}

struct ParsedIdentitySelector {
    alias: NamePart,
    parameter: NamePart,
    equality_span: SourceSpan,
    reference_span: SourceSpan,
}

#[derive(Debug, Clone, Copy)]
enum SqlBodySyntax {
    Select,
    Insert,
    Update,
    Delete,
}

struct SqlBodyParser<'tokens, 'source> {
    tokens: &'tokens [Token<'source>],
    index: usize,
    syntax: SqlBodySyntax,
}

impl<'tokens, 'source> SqlBodyParser<'tokens, 'source> {
    fn new(tokens: &'tokens [Token<'source>], syntax: SqlBodySyntax) -> Self {
        Self {
            tokens,
            index: 0,
            syntax,
        }
    }

    fn parse_select(mut self) -> Result<SelectQuery, QueryParseError> {
        let select = self
            .take_word("SELECT")
            .ok_or_else(|| self.implementation_gap("only SELECT query bodies", "a SELECT query"))?;

        self.skip_trivia();
        let quantifier = if let Some(distinct) = self.take_word("DISTINCT") {
            self.skip_trivia();
            if self.current().is_some_and(|token| token.is_word("ON")) {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: "DISTINCT ON is not supported; use SELECT DISTINCT followed by the result columns"
                        .to_owned(),
                    span: self.current_span(),
                });
            }
            SelectQuantifier::Distinct {
                source: SourceSlice {
                    text: distinct.text.to_owned(),
                    span: distinct.span(),
                },
            }
        } else if self.current().is_some_and(|token| token.is_word("ALL")) {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "SELECT ALL is not supported; omit ALL to preserve duplicate rows"
                    .to_owned(),
                span: self.current_span(),
            });
        } else {
            SelectQuantifier::All
        };
        let mut projections = vec![self.parse_expression(false)?];
        loop {
            self.skip_trivia();
            if self.take_kind(TokenKind::Comma).is_some() {
                self.skip_trivia();
                projections.push(self.parse_expression(false)?);
                continue;
            }
            break;
        }

        self.skip_trivia();
        if self.take_word("FROM").is_none() {
            if self.current().is_none()
                || self
                    .current()
                    .is_some_and(|token| token.is_word("WHERE") || token.is_word("ORDER"))
            {
                return Err(self.implementation_gap(
                    "SELECT query bodies without FROM",
                    "FROM followed by an aliased object source",
                ));
            }
            return Err(self.expected("FROM after the SELECT list"));
        }
        self.skip_trivia();
        let source_object = self.parse_object_source()?;

        self.skip_trivia();
        let predicate = if self.take_word("WHERE").is_some() {
            self.skip_trivia();
            if self.has_reversed_selector_operands() {
                return Err(self.implementation_gap(
                    "selector parameters on the left side of WHERE equality",
                    "WHERE REF(alias) = selector_parameter",
                ));
            }
            let predicate = self.parse_expression(true)?;
            if !supports_where_predicate(&quantifier, &predicate) {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message:
                        "WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate"
                            .to_owned(),
                    span: predicate.span().clone(),
                });
            }
            Some(predicate)
        } else {
            None
        };

        let has_identity_selector_parameter = matches!(
            &predicate,
            Some(QueryExpression::Equality { right, .. })
                if matches!(right.as_ref(), QueryExpression::ParameterRead { .. })
        );

        self.skip_trivia();
        let ordering = if let Some(order) = self.take_word("ORDER") {
            if matches!(quantifier, SelectQuantifier::Distinct { .. }) {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message:
                        "SELECT DISTINCT queries do not allow ORDER BY; remove the ORDER BY clause"
                            .to_owned(),
                    span: order.span(),
                });
            }
            if has_identity_selector_parameter {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: "identity-selected SELECT queries do not allow ORDER BY; remove the ORDER BY clause"
                        .to_owned(),
                    span: order.span(),
                });
            }
            self.skip_trivia();
            if self.take_word("BY").is_none() {
                return Err(self.expected("BY after ORDER"));
            }
            self.skip_trivia();
            self.parse_ordering()?
        } else {
            Vec::new()
        };

        self.skip_trivia();
        if self.current().is_some() {
            return Err(self.unsupported_remaining_query_syntax());
        }

        let end = ordering
            .last()
            .map(|ordering| ordering.span.end)
            .or_else(|| predicate.as_ref().map(|predicate| predicate.span().end))
            .unwrap_or(source_object.span.end);
        Ok(SelectQuery {
            quantifier,
            projections,
            source_object,
            predicate,
            ordering,
            span: SourceSpan {
                start: select.range.start,
                end,
            },
        })
    }

    fn parse_object_source(&mut self) -> Result<ObjectSource, QueryParseError> {
        let object_type = self.parse_qualified_name("an object type after FROM")?;
        self.skip_trivia();
        if self.take_word("AS").is_some() {
            self.skip_trivia();
        }
        if self.current().is_none()
            || self
                .current()
                .is_some_and(|token| token.is_word("WHERE") || token.is_word("ORDER"))
        {
            return Err(self.implementation_gap(
                "object sources without aliases",
                "an object source alias after FROM",
            ));
        }
        let alias = self.parse_name_part("an object source alias after FROM")?;
        Ok(ObjectSource {
            span: SourceSpan {
                start: object_type.span.start,
                end: alias.span.end,
            },
            object_type,
            alias,
        })
    }

    fn parse_ordering(&mut self) -> Result<Vec<OrderingExpression>, QueryParseError> {
        let mut ordering = Vec::new();
        loop {
            let expression = self.parse_expression(false)?;
            if !matches!(expression, QueryExpression::FieldPath { .. }) {
                return Err(self.implementation_gap(
                    "ORDER BY expressions other than field paths",
                    "a field path",
                ));
            }
            self.skip_trivia();
            let direction = if let Some(token) = self.take_word("ASC") {
                (OrderingDirection::Ascending, token.range.end)
            } else if let Some(token) = self.take_word("DESC") {
                (OrderingDirection::Descending, token.range.end)
            } else {
                (OrderingDirection::Unspecified, expression.span().end)
            };
            self.skip_trivia();
            if self.current().is_some_and(|token| token.is_word("NULLS")) {
                return Err(self.implementation_gap(
                    "explicit NULLS FIRST or NULLS LAST ordering",
                    "the end of this ordering expression",
                ));
            }
            ordering.push(OrderingExpression {
                span: SourceSpan {
                    start: expression.span().start,
                    end: direction.1,
                },
                expression,
                direction: direction.0,
                null_order: NullOrdering::Unspecified,
            });
            if self.take_kind(TokenKind::Comma).is_some() {
                self.skip_trivia();
                continue;
            }
            return Ok(ordering);
        }
    }

    fn parse_expression(
        &mut self,
        allow_selector_parameter: bool,
    ) -> Result<QueryExpression, QueryParseError> {
        let left = self.parse_primary_expression()?;
        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::Other && token.text == "=")
        {
            self.index += 1;
            self.skip_trivia();
            let is_selector_left = matches!(&left, QueryExpression::ObjectReference { .. })
                || matches!(&left, QueryExpression::FieldPath { members, .. } if members.len() == 1);
            let right = if allow_selector_parameter && is_selector_left {
                self.parse_selector_parameter_or_primary_expression()?
            } else {
                self.parse_primary_expression()?
            };
            let span = SourceSpan {
                start: left.span().start,
                end: right.span().end,
            };
            return Ok(QueryExpression::Equality {
                left: Box::new(left),
                right: Box::new(right),
                span,
            });
        }
        Ok(left)
    }

    fn parse_selector_parameter_or_primary_expression(
        &mut self,
    ) -> Result<QueryExpression, QueryParseError> {
        let Some(token) = self.current().cloned() else {
            return Err(self.expected("a query expression"));
        };
        if !token.is_identifier()
            || token.is_word("REF")
            || token.is_word("TRUE")
            || token.is_word("FALSE")
        {
            return self.parse_primary_expression();
        }
        if self
            .tokens
            .iter()
            .skip(self.index + 1)
            .find(|token| !token.kind.is_trivia())
            .is_some_and(|token| token.kind == TokenKind::Dot)
        {
            return self.parse_field_path();
        }

        self.index += 1;
        let parameter = NamePart {
            text: token.text.to_owned(),
            span: token.span(),
        };
        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::LeftParenthesis)
        {
            return Err(self.implementation_gap(
                "function calls as identity selector parameters",
                "a selector parameter name by itself",
            ));
        }
        Ok(QueryExpression::ParameterRead { parameter })
    }

    fn has_reversed_selector_operands(&self) -> bool {
        let mut significant = self
            .tokens
            .iter()
            .skip(self.index)
            .filter(|token| !token.kind.is_trivia());
        let Some(left) = significant.next() else {
            return false;
        };
        let Some(equals) = significant.next() else {
            return false;
        };
        let Some(right) = significant.next() else {
            return false;
        };
        left.is_identifier()
            && !left.is_word("REF")
            && !left.is_word("TRUE")
            && !left.is_word("FALSE")
            && equals.kind == TokenKind::Other
            && equals.text == "="
            && right.is_word("REF")
    }

    fn parse_primary_expression(&mut self) -> Result<QueryExpression, QueryParseError> {
        if let Some(reference) = self.take_word("REF") {
            self.skip_trivia();
            if self.take_kind(TokenKind::LeftParenthesis).is_none() {
                return Err(self.expected("'(' after REF"));
            }
            self.skip_trivia();
            let alias = self.parse_name_part("an alias inside REF(...)")?;
            self.skip_trivia();
            let close = self
                .take_kind(TokenKind::RightParenthesis)
                .ok_or_else(|| self.expected("')' after the REF alias"))?;
            return Ok(QueryExpression::ObjectReference {
                alias,
                span: SourceSpan {
                    start: reference.range.start,
                    end: close.range.end,
                },
            });
        }
        if let Some(literal) = self.take_word("TRUE") {
            return Ok(self.boolean_literal(literal, true));
        }
        if let Some(literal) = self.take_word("FALSE") {
            return Ok(self.boolean_literal(literal, false));
        }
        self.parse_field_path()
    }

    fn parse_field_path(&mut self) -> Result<QueryExpression, QueryParseError> {
        let root = self.parse_name_part("a query expression")?;
        self.skip_trivia();
        if self.take_kind(TokenKind::Dot).is_none() {
            return Err(self.implementation_gap("bare alias expressions", "a field path"));
        }
        self.skip_trivia();
        let mut members = Vec::new();
        loop {
            if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Other && token.text == "*")
            {
                return Err(
                    self.implementation_gap("wildcard field paths", "a field name after '.'")
                );
            }
            members.push(self.parse_name_part("a field name after '.'")?);
            self.skip_trivia();
            if self.take_kind(TokenKind::Dot).is_none() {
                break;
            }
            self.skip_trivia();
        }
        let end = members.last().expect("field path has a member").span.end;
        Ok(QueryExpression::FieldPath {
            root: root.clone(),
            members,
            span: SourceSpan {
                start: root.span.start,
                end,
            },
        })
    }

    fn boolean_literal(&self, token: Token<'source>, value: bool) -> QueryExpression {
        QueryExpression::BooleanLiteral {
            value,
            source: SourceSlice {
                text: token.text.to_owned(),
                span: token.span(),
            },
        }
    }

    fn unsupported_remaining_query_syntax(&self) -> QueryParseError {
        let feature = self
            .current()
            .map_or("this SELECT query syntax", |token| token.text);
        self.implementation_gap(feature, "the end of the implemented SELECT query slice")
    }
}

/// The words the grammar reserves as keywords anywhere in a statement.
///
/// A reserved word can never be the bare parameter identifier of a
/// `NoInputParameterSelect` body; such bodies keep their existing
/// implementation-gap diagnostics instead.
const RESERVED_WORDS: &[&str] = &[
    "ALL",
    "ALTER",
    "AS",
    "ASC",
    "ATOMIC",
    "BINARY",
    "BY",
    "CAPABILITY",
    "CASCADE",
    "CHARACTER",
    "CHECK",
    "CLIENT",
    "CONTRACT",
    "CREATE",
    "DEFAULT",
    "DEFINER",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DOCUMENTATION",
    "END",
    "ENUM",
    "EXPORT",
    "FALSE",
    "FIELD",
    "FROM",
    "FUNCTION",
    "IMMUTABLE",
    "INSERT",
    "INTO",
    "INVOKER",
    "IS",
    "KERNEL",
    "KEY",
    "LARGE",
    "LIST",
    "MANUAL",
    "MAP",
    "NOT",
    "NULL",
    "NULLS",
    "OBJECT",
    "OF",
    "ON",
    "ONLY",
    "OPAQUE",
    "OPTION",
    "ORDER",
    "PERSISTABLE",
    "PRIMARY",
    "PRIMITIVE",
    "READ",
    "REF",
    "RENAME",
    "REQUIRES",
    "RESTRICT",
    "RETURN",
    "RETURNING",
    "RETURNS",
    "ROWS",
    "SCHEMA",
    "SECURITY",
    "SELECT",
    "SERVER",
    "SET",
    "STABLE",
    "STREAM",
    "TABLE",
    "TO",
    "TRANSACTION",
    "TRANSIENT",
    "TRUE",
    "TYPE",
    "UNIQUE",
    "UPDATE",
    "VALUE",
    "VALUES",
    "VOLATILE",
    "VOLATILITY",
    "WHERE",
];

fn is_reserved_word(token: &Token<'_>) -> bool {
    token.kind == TokenKind::Word
        && RESERVED_WORDS
            .iter()
            .any(|keyword| token.text.eq_ignore_ascii_case(keyword))
}

/// The identifier when `tokens` is exactly `SELECT <bare identifier>` with no
/// other clause, and `None` for every other body slice.
fn no_input_parameter_identifier<'source>(tokens: &[Token<'source>]) -> Option<Token<'source>> {
    let mut significant = tokens.iter().filter(|token| !token.kind.is_trivia());
    let select = significant.next()?;
    if !select.is_word("SELECT") {
        return None;
    }
    let identifier = significant.next()?;
    if !identifier.is_identifier() || is_reserved_word(identifier) {
        return None;
    }
    if significant.next().is_some() {
        return None;
    }
    Some(identifier.clone())
}

fn parse_select_query(tokens: &[Token<'_>]) -> Result<SelectQuery, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Select).parse_select()
}

fn supports_where_predicate(quantifier: &SelectQuantifier, predicate: &QueryExpression) -> bool {
    matches!(predicate, QueryExpression::Equality { .. })
        || matches!(
            (quantifier, predicate),
            (
                SelectQuantifier::All | SelectQuantifier::Distinct { .. },
                QueryExpression::FieldPath { .. } | QueryExpression::BooleanLiteral { .. },
            )
        )
}

impl<'tokens, 'source> SqlBodyParser<'tokens, 'source> {
    fn parse_insert(mut self) -> Result<InsertStatement, QueryParseError> {
        let insert = self
            .take_word("INSERT")
            .ok_or_else(|| self.expected("INSERT"))?;
        self.skip_trivia();
        if self.take_word("INTO").is_none() {
            return Err(self.expected("INTO after INSERT"));
        }
        self.skip_trivia();
        let (target_object, target_alias) =
            self.parse_aliased_mutation_target("INSERT", "INSERT INTO")?;

        self.skip_trivia();
        self.take_kind(TokenKind::LeftParenthesis)
            .ok_or_else(|| self.expected("'(' before the INSERT target fields"))?;
        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
        {
            return Err(self.expected("a non-empty INSERT target field list"));
        }
        let mut target_fields = Vec::new();
        loop {
            let field = self.parse_name_part("an unqualified INSERT target field")?;
            if target_fields
                .iter()
                .any(|existing| identifiers_equal(existing, &field))
            {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: format!(
                        "field {} appears more than once in this INSERT",
                        normalise_identifier(&field)
                    ),
                    span: field.span.clone(),
                });
            }
            self.skip_trivia();
            if let Some(dot) = self.take_kind(TokenKind::Dot) {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: "write only the field name in the INSERT field list; do not add an object or alias".to_owned(),
                    span: dot.span(),
                });
            }
            target_fields.push(field);
            self.skip_trivia();
            if self.take_kind(TokenKind::Comma).is_some() {
                self.skip_trivia();
                if self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
                {
                    return Err(self.expected("an INSERT target field after ','"));
                }
                continue;
            }
            self.take_kind(TokenKind::RightParenthesis)
                .ok_or_else(|| self.expected("',' or ')' after an INSERT target field"))?;
            break;
        }

        self.skip_trivia();
        if self.take_word("VALUES").is_none() {
            return Err(self.expected("VALUES after the INSERT target fields"));
        }
        self.skip_trivia();
        self.take_kind(TokenKind::LeftParenthesis)
            .ok_or_else(|| self.expected("'(' after VALUES"))?;
        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
        {
            return Err(self.expected("a non-empty VALUES row"));
        }
        let mut values = Vec::new();
        loop {
            values.push(self.parse_mutation_value(
                "use the declared parameter name by itself in VALUES; do not add an object or alias",
                "function calls in INSERT values",
                true,
            )?);
            self.skip_trivia();
            if self.take_kind(TokenKind::Comma).is_some() {
                self.skip_trivia();
                if self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::RightParenthesis)
                {
                    return Err(self.expected("an INSERT value after ','"));
                }
                continue;
            }
            let close = self
                .take_kind(TokenKind::RightParenthesis)
                .ok_or_else(|| self.expected("',' or ')' after an INSERT value"))?;
            if values.len() != target_fields.len() {
                let arity_span = values
                    .get(target_fields.len())
                    .map(MutationValue::span)
                    .cloned()
                    .unwrap_or_else(|| close.span());
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: format!(
                        "INSERT lists {} {} but {} {}; each field requires one value",
                        target_fields.len(),
                        if target_fields.len() == 1 {
                            "field"
                        } else {
                            "fields"
                        },
                        values.len(),
                        if values.len() == 1 { "value" } else { "values" }
                    ),
                    span: arity_span,
                });
            }
            break;
        }

        self.skip_trivia();
        if !self
            .current()
            .is_some_and(|token| token.is_word("RETURNING"))
        {
            if self.current().is_some_and(|token| {
                token.kind == TokenKind::LeftParenthesis || token.kind == TokenKind::Comma
            }) {
                return Err(self
                    .implementation_gap("multiple VALUES rows", "RETURNING after one VALUES row"));
            }
            return Err(self.expected("RETURNING after one VALUES row"));
        }
        let ParsedReference {
            alias: returning_alias,
            span: returning_ref_span,
        } = self.parse_returning_ref(&target_alias, "INSERT")?;
        let body_end = returning_ref_span.end;
        self.skip_trivia();
        if self.current().is_some() {
            return Err(self.implementation_gap(
                self.current().expect("current token exists").text,
                "the end of the INSERT body",
            ));
        }

        Ok(InsertStatement {
            target_object,
            target_alias,
            target_fields,
            values,
            returning_alias,
            returning_ref_span,
            span: SourceSpan {
                start: insert.range.start,
                end: body_end,
            },
        })
    }

    fn parse_mutation_value(
        &mut self,
        qualified_name_message: &str,
        function_call_feature: &str,
        permit_record_constructor: bool,
    ) -> Result<MutationValue, QueryParseError> {
        if let Some(token) = self.take_word("TRUE") {
            return Ok(MutationValue::BooleanLiteral {
                value: true,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if let Some(token) = self.take_word("FALSE") {
            return Ok(MutationValue::BooleanLiteral {
                value: false,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if let Some(token) = self.take_word("NULL") {
            return Ok(MutationValue::NullLiteral {
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }

        let first = self
            .parse_name_part("a declared parameter, TRUE, FALSE, NULL, or a record constructor")?;
        let mut parts = vec![first.clone()];
        let mut first_dot = None;
        loop {
            self.skip_trivia();
            let Some(dot) = self.take_kind(TokenKind::Dot) else {
                break;
            };
            first_dot.get_or_insert_with(|| dot.clone());
            self.skip_trivia();
            parts.push(self.parse_name_part("a record type name after '.'")?);
        }
        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::Other && token.text == "{")
        {
            if !permit_record_constructor {
                return Err(self.implementation_gap(
                    "record constructors in UPDATE values",
                    "a declared parameter name by itself",
                ));
            }
            let end = parts.last().expect("record type has a name part").span.end;
            return self
                .parse_record_constructor(QualifiedName {
                    parts,
                    span: SourceSpan {
                        start: first.span.start,
                        end,
                    },
                })
                .map(MutationValue::RecordConstructor);
        }
        if let Some(dot) = first_dot {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: qualified_name_message.to_owned(),
                span: dot.span(),
            });
        }
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::LeftParenthesis)
        {
            return Err(self
                .implementation_gap(function_call_feature, "a declared parameter name by itself"));
        }
        Ok(MutationValue::Parameter(first))
    }

    fn parse_record_constructor(
        &mut self,
        record_type: QualifiedName,
    ) -> Result<RecordConstructor, QueryParseError> {
        self.take_symbol("{")
            .ok_or_else(|| self.expected("'{' after the record type name"))?;
        self.skip_trivia();
        let mut fields = Vec::new();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::Other && token.text == "}")
        {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "record constructor must supply at least one field".to_owned(),
                span: self.current_span(),
            });
        }

        let close = loop {
            let name = self.parse_name_part("a record constructor field name")?;
            if fields
                .iter()
                .any(|field: &RecordConstructorField| identifiers_equal(&field.name, &name))
            {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: format!(
                        "record constructor field {} appears more than once",
                        normalise_identifier(&name)
                    ),
                    span: name.span.clone(),
                });
            }
            self.skip_trivia();
            self.take_symbol(":")
                .ok_or_else(|| self.expected("':' after a record constructor field name"))?;
            self.skip_trivia();
            let value = self.parse_record_constructor_field_value()?;
            let span = SourceSpan {
                start: name.span.start,
                end: value.span().end,
            };
            fields.push(RecordConstructorField { name, value, span });
            self.skip_trivia();
            if let Some(close) = self.take_symbol("}") {
                break close;
            }
            self.take_kind(TokenKind::Comma)
                .ok_or_else(|| self.expected("',' or '}' after a record constructor field"))?;
            self.skip_trivia();
            if let Some(close) = self.take_symbol("}") {
                break close;
            }
        };

        Ok(RecordConstructor {
            span: SourceSpan {
                start: record_type.span.start,
                end: close.range.end,
            },
            record_type,
            fields,
        })
    }

    fn parse_record_constructor_field_value(
        &mut self,
    ) -> Result<RecordConstructorFieldValue, QueryParseError> {
        if let Some(token) = self.take_word("TRUE") {
            return Ok(RecordConstructorFieldValue::BooleanLiteral {
                value: true,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if let Some(token) = self.take_word("FALSE") {
            return Ok(RecordConstructorFieldValue::BooleanLiteral {
                value: false,
                source: SourceSlice {
                    text: token.text.to_owned(),
                    span: token.span(),
                },
            });
        }
        if self.current().is_some_and(|token| token.is_word("NULL")) {
            return Err(QueryParseError {
                code: "ORNA0001",
                message:
                    "record constructor fields accept only a declared parameter, TRUE, or FALSE"
                        .to_owned(),
                span: self.current_span(),
            });
        }

        let parameter = self.parse_name_part(
            "a declared parameter, TRUE, or FALSE in a record constructor field",
        )?;
        self.skip_trivia();
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::LeftParenthesis)
        {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "record constructor fields do not support function calls".to_owned(),
                span: self.current_span(),
            });
        }
        if self.qualified_value_opens_constructor() {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "record constructor fields do not support nested record constructors"
                    .to_owned(),
                span: self
                    .tokens
                    .iter()
                    .skip(self.index)
                    .find(|token| {
                        !token.kind.is_trivia()
                            && token.kind == TokenKind::Other
                            && token.text == "{"
                    })
                    .map_or_else(|| self.current_span(), Token::span),
            });
        }
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::Dot)
        {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "record constructor fields do not support field paths or qualified values"
                    .to_owned(),
                span: self.current_span(),
            });
        }
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::Other && token.text == "{")
        {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "record constructor fields do not support nested record constructors"
                    .to_owned(),
                span: self.current_span(),
            });
        }
        Ok(RecordConstructorFieldValue::Parameter(parameter))
    }

    fn qualified_value_opens_constructor(&self) -> bool {
        let mut significant = self
            .tokens
            .iter()
            .skip(self.index)
            .filter(|token| !token.kind.is_trivia())
            .peekable();
        loop {
            let Some(dot) = significant.next() else {
                return false;
            };
            if dot.kind != TokenKind::Dot {
                return false;
            }
            let Some(name) = significant.next() else {
                return false;
            };
            if !name.is_identifier() {
                return false;
            }
            let Some(next) = significant.peek() else {
                return false;
            };
            if next.kind == TokenKind::Other && next.text == "{" {
                return true;
            }
            if next.kind != TokenKind::Dot {
                return false;
            }
        }
    }
}

impl<'tokens, 'source> SqlBodyParser<'tokens, 'source> {
    fn parse_update(mut self) -> Result<UpdateStatement, QueryParseError> {
        let update = self
            .take_word("UPDATE")
            .ok_or_else(|| self.expected("UPDATE"))?;
        self.skip_trivia();
        let (target_object, target_alias) =
            self.parse_aliased_mutation_target("UPDATE", "UPDATE")?;
        self.skip_trivia();
        if self.take_word("SET").is_none() {
            return Err(self.expected("SET after the UPDATE target alias"));
        }
        self.skip_trivia();

        let mut assignments = Vec::new();
        loop {
            if self.current().is_some_and(|token| token.is_word("WHERE")) {
                return Err(self.expected("at least one field assignment after SET"));
            }
            let target_field = self.parse_name_part("a field name after SET")?;
            if assignments.iter().any(|assignment: &UpdateAssignment| {
                identifiers_equal(&assignment.target_field, &target_field)
            }) {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: format!(
                        "field {} appears more than once in this UPDATE",
                        normalise_identifier(&target_field)
                    ),
                    span: target_field.span.clone(),
                });
            }
            self.skip_trivia();
            if let Some(dot) = self.take_kind(TokenKind::Dot) {
                return Err(QueryParseError {
                    code: "ORNA0001",
                    message: "write only the field name in SET; do not add an object or alias"
                        .to_owned(),
                    span: dot.span(),
                });
            }
            if self.take_symbol("=").is_none() {
                return Err(self.expected("'=' after the UPDATE field name"));
            }
            self.skip_trivia();
            let value = self.parse_mutation_value(
                "use the declared parameter name by itself after '='; do not add an object or alias",
                "function calls in UPDATE values",
                false,
            )?;
            let assignment_span = SourceSpan {
                start: target_field.span.start,
                end: value.span().end,
            };
            assignments.push(UpdateAssignment {
                target_field,
                value,
                span: assignment_span,
            });
            self.skip_trivia();
            if self.take_kind(TokenKind::Comma).is_some() {
                self.skip_trivia();
                if self.current().is_some_and(|token| token.is_word("WHERE")) {
                    return Err(self.expected("a field assignment after ','"));
                }
                continue;
            }
            break;
        }

        if self.take_word("WHERE").is_none() {
            return Err(self.expected("WHERE after the UPDATE assignments"));
        }
        self.skip_trivia();
        let ParsedIdentitySelector {
            alias: selector_alias,
            parameter: selector_parameter,
            equality_span: selector_equality_span,
            reference_span: selector_ref_span,
        } = self.parse_identity_selector(&target_alias, "UPDATE")?;

        if !self
            .current()
            .is_some_and(|token| token.is_word("RETURNING"))
        {
            return Err(self.expected("RETURNING after the UPDATE selector"));
        }
        let ParsedReference {
            alias: returning_alias,
            span: returning_ref_span,
        } = self.parse_returning_ref(&target_alias, "UPDATE")?;
        let body_end = returning_ref_span.end;
        self.skip_trivia();
        if self.current().is_some() {
            return Err(self.implementation_gap(
                self.current().expect("current token exists").text,
                "the end of the UPDATE body",
            ));
        }

        Ok(UpdateStatement {
            target_object,
            target_alias,
            assignments,
            selector_alias,
            selector_parameter,
            selector_equality_span,
            selector_ref_span,
            returning_alias,
            returning_ref_span,
            span: SourceSpan {
                start: update.range.start,
                end: body_end,
            },
        })
    }
}

impl<'tokens, 'source> SqlBodyParser<'tokens, 'source> {
    fn parse_delete(mut self) -> Result<DeleteStatement, QueryParseError> {
        let delete = self
            .take_word("DELETE")
            .ok_or_else(|| self.expected("DELETE"))?;
        self.skip_trivia();
        if self.take_word("FROM").is_none() {
            return Err(self.expected("FROM after DELETE"));
        }
        self.skip_trivia();
        let (target_object, target_alias) =
            self.parse_aliased_mutation_target("DELETE", "DELETE FROM")?;
        self.skip_trivia();
        if self.take_word("WHERE").is_none() {
            return Err(self.expected("WHERE after the DELETE target alias"));
        }
        self.skip_trivia();
        let ParsedIdentitySelector {
            alias: selector_alias,
            parameter: selector_parameter,
            equality_span: selector_equality_span,
            reference_span: selector_ref_span,
        } = self.parse_identity_selector(&target_alias, "DELETE")?;

        if self.take_word("RETURNING").is_none() {
            return Err(self.expected("RETURNING after the DELETE selector"));
        }
        self.skip_trivia();
        let returned = self
            .take_word("TRUE")
            .ok_or_else(|| self.expected("TRUE after RETURNING"))?;
        let returning_true = SourceSlice {
            text: returned.text.to_owned(),
            span: returned.span(),
        };
        self.skip_trivia();
        if self.current().is_some() {
            return Err(self.implementation_gap(
                self.current().expect("current token exists").text,
                "the end of the DELETE body",
            ));
        }

        Ok(DeleteStatement {
            target_object,
            target_alias,
            selector_alias,
            selector_parameter,
            selector_equality_span,
            selector_ref_span,
            returning_true,
            span: SourceSpan {
                start: delete.range.start,
                end: returned.range.end,
            },
        })
    }
}

impl<'tokens, 'source> SqlBodyParser<'tokens, 'source> {
    fn parse_identity_selector(
        &mut self,
        target_alias: &NamePart,
        statement: &str,
    ) -> Result<ParsedIdentitySelector, QueryParseError> {
        let reference = self
            .take_word("REF")
            .ok_or_else(|| self.expected("REF(target_alias) after WHERE"))?;
        self.skip_trivia();
        self.take_kind(TokenKind::LeftParenthesis)
            .ok_or_else(|| self.expected("'(' after WHERE REF"))?;
        self.skip_trivia();
        let selector_alias = self.parse_name_part("the alias inside WHERE REF(...)")?;
        if !identifiers_equal(target_alias, &selector_alias) {
            return Err(alias_mismatch(
                "WHERE REF",
                target_alias,
                &selector_alias,
                statement,
            ));
        }
        self.skip_trivia();
        let close = self
            .take_kind(TokenKind::RightParenthesis)
            .ok_or_else(|| self.expected("')' after the WHERE REF alias"))?;
        self.skip_trivia();
        if self.take_symbol("=").is_none() {
            return Err(self.expected("'=' after WHERE REF(target_alias)"));
        }
        self.skip_trivia();
        if self.current().is_some_and(|token| {
            token.is_word("TRUE") || token.is_word("FALSE") || token.is_word("NULL")
        }) {
            return Err(self.expected("a declared REF parameter after '='"));
        }
        let selector_parameter = self.parse_name_part("a declared REF parameter after '='")?;
        self.skip_trivia();
        if let Some(dot) = self.take_kind(TokenKind::Dot) {
            return Err(QueryParseError {
                code: "ORNA0001",
                message: "use the selector parameter name by itself after '='; do not add an object or alias"
                    .to_owned(),
                span: dot.span(),
            });
        }
        if self
            .current()
            .is_some_and(|token| token.kind == TokenKind::LeftParenthesis)
        {
            return Err(self.implementation_gap(
                &format!("function calls as {statement} selectors"),
                "a declared REF parameter name by itself",
            ));
        }
        self.skip_trivia();
        Ok(ParsedIdentitySelector {
            alias: selector_alias,
            equality_span: SourceSpan {
                start: reference.range.start,
                end: selector_parameter.span.end,
            },
            parameter: selector_parameter,
            reference_span: SourceSpan {
                start: reference.range.start,
                end: close.range.end,
            },
        })
    }

    fn parse_aliased_mutation_target(
        &mut self,
        statement: &str,
        after: &str,
    ) -> Result<(QualifiedName, NamePart), QueryParseError> {
        let target_object = self.parse_qualified_name(&format!("an object type after {after}"))?;
        self.skip_trivia();
        if self.take_word("AS").is_none() {
            return Err(self.expected(&format!("AS before the {statement} target alias")));
        }
        self.skip_trivia();
        let target_alias = self.parse_name_part(&format!("a {statement} target alias after AS"))?;
        Ok((target_object, target_alias))
    }

    fn parse_returning_ref(
        &mut self,
        target_alias: &NamePart,
        statement: &str,
    ) -> Result<ParsedReference, QueryParseError> {
        self.take_word("RETURNING")
            .ok_or_else(|| self.expected("RETURNING"))?;
        self.skip_trivia();
        let reference = self
            .take_word("REF")
            .ok_or_else(|| self.expected("REF in the RETURNING expression"))?;
        self.skip_trivia();
        self.take_kind(TokenKind::LeftParenthesis)
            .ok_or_else(|| self.expected("'(' after RETURNING REF"))?;
        self.skip_trivia();
        let returning_alias = self.parse_name_part("the alias inside RETURNING REF(...)")?;
        if !identifiers_equal(target_alias, &returning_alias) {
            return Err(alias_mismatch(
                "RETURNING REF",
                target_alias,
                &returning_alias,
                statement,
            ));
        }
        self.skip_trivia();
        let close = self
            .take_kind(TokenKind::RightParenthesis)
            .ok_or_else(|| self.expected("')' after the RETURNING REF alias"))?;
        Ok(ParsedReference {
            alias: returning_alias,
            span: SourceSpan {
                start: reference.range.start,
                end: close.range.end,
            },
        })
    }

    fn parse_qualified_name(&mut self, expected: &str) -> Result<QualifiedName, QueryParseError> {
        let first = self.parse_name_part(expected)?;
        let mut parts = vec![first.clone()];
        let after_dot = match self.syntax {
            SqlBodySyntax::Select => "an identifier after '.' in an object type",
            SqlBodySyntax::Insert => "an identifier after '.' in the INSERT target",
            SqlBodySyntax::Update => "an identifier after '.' in the UPDATE target",
            SqlBodySyntax::Delete => "an identifier after '.' in the DELETE target",
        };
        loop {
            self.skip_trivia();
            if self.take_kind(TokenKind::Dot).is_none() {
                break;
            }
            self.skip_trivia();
            parts.push(self.parse_name_part(after_dot)?);
        }
        let end = parts.last().expect("qualified name has a part").span.end;
        Ok(QualifiedName {
            parts,
            span: SourceSpan {
                start: first.span.start,
                end,
            },
        })
    }

    fn parse_name_part(&mut self, expected: &str) -> Result<NamePart, QueryParseError> {
        let Some(token) = self.current().cloned() else {
            return Err(self.expected(expected));
        };
        if !token.is_identifier() {
            return Err(self.expected(expected));
        }
        self.index += 1;
        Ok(NamePart {
            text: token.text.to_owned(),
            span: token.span(),
        })
    }

    fn take_word(&mut self, expected: &str) -> Option<Token<'source>> {
        let token = self.current().cloned()?;
        if token.is_word(expected) {
            self.index += 1;
            Some(token)
        } else {
            None
        }
    }

    fn take_kind(&mut self, kind: TokenKind) -> Option<Token<'source>> {
        let token = self.current().cloned()?;
        if token.kind == kind {
            self.index += 1;
            Some(token)
        } else {
            None
        }
    }

    fn take_symbol(&mut self, symbol: &str) -> Option<Token<'source>> {
        let token = self.current().cloned()?;
        if token.kind == TokenKind::Other && token.text == symbol {
            self.index += 1;
            Some(token)
        } else {
            None
        }
    }

    fn skip_trivia(&mut self) {
        while self.current().is_some_and(|token| token.kind.is_trivia()) {
            self.index += 1;
        }
    }

    fn current(&self) -> Option<&Token<'source>> {
        self.tokens.get(self.index)
    }

    fn current_span(&self) -> SourceSpan {
        self.current().map_or_else(
            || {
                let end = self.tokens.last().map_or(0, |token| token.range.end);
                SourceSpan { start: end, end }
            },
            Token::span,
        )
    }

    fn expected(&self, expected: &str) -> QueryParseError {
        let context = match self.syntax {
            SqlBodySyntax::Select => "SELECT query",
            SqlBodySyntax::Insert => "SQL INSERT body",
            SqlBodySyntax::Update => "UPDATE body",
            SqlBodySyntax::Delete => "DELETE body",
        };
        QueryParseError {
            code: "ORNA0001",
            message: format!("expected {expected} in {context}"),
            span: self.current_span(),
        }
    }

    fn implementation_gap(&self, feature: &str, expected: &str) -> QueryParseError {
        let message = match self.syntax {
            SqlBodySyntax::Select => format!(
                "the current Orna SELECT parser does not yet implement {feature}; expected {expected}"
            ),
            SqlBodySyntax::Insert => {
                format!("this INSERT does not support {feature}; expected {expected}")
            }
            SqlBodySyntax::Update => {
                format!("this UPDATE does not support {feature}; expected {expected}")
            }
            SqlBodySyntax::Delete => {
                format!("this DELETE does not support {feature}; expected {expected}")
            }
        };
        QueryParseError {
            code: "ORNA0001",
            message,
            span: self.current_span(),
        }
    }
}

fn identifiers_equal(left: &NamePart, right: &NamePart) -> bool {
    normalise_identifier(left) == normalise_identifier(right)
}

fn alias_mismatch(
    expression: &str,
    expected: &NamePart,
    actual: &NamePart,
    statement: &str,
) -> QueryParseError {
    QueryParseError {
        code: "ORNA0001",
        message: format!(
            "{expression} must use the {statement} target alias {}, not {}",
            normalise_identifier(expected),
            normalise_identifier(actual)
        ),
        span: actual.span.clone(),
    }
}

fn normalise_identifier(identifier: &NamePart) -> String {
    if let Some(quoted) = identifier
        .text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
    {
        return quoted.replace("\"\"", "\"");
    }
    identifier.text.to_lowercase()
}

fn parse_sql_insert(tokens: &[Token<'_>]) -> Result<InsertStatement, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Insert).parse_insert()
}

fn parse_sql_update(tokens: &[Token<'_>]) -> Result<UpdateStatement, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Update).parse_update()
}

fn parse_sql_delete(tokens: &[Token<'_>]) -> Result<DeleteStatement, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Delete).parse_delete()
}
