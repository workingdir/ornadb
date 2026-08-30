use super::*;

impl<'source> Parser<'source> {
    pub(super) fn parse_create_client_function_statement(&mut self) {
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
                self.recover_client_function_statement();
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
        let is_boolean_return = self.current().is_some_and(|token| token.is_word("RETURN"))
            && self
                .peek_significant(1)
                .is_some_and(|token| token.is_word("TRUE") || token.is_word("FALSE"))
            && self
                .peek_significant(2)
                .is_none_or(|token| token.kind == TokenKind::Semicolon);
        self.builder.start_node(
            if is_boolean_return {
                SyntaxKind::ClientBooleanReturnBody
            } else {
                SyntaxKind::ClientExpressionBody
            }
            .into(),
        );
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
                self.error_current("ORNA0001", "expected a CLIENT expression");
                return None;
            };
            if is_boolean_return {
                let value = token.is_word("TRUE");
                self.bump();
                return Some(ClientFunctionBody::BooleanLiteral {
                    value,
                    source: SourceSlice {
                        text: token.text.to_owned(),
                        span: token.span(),
                    },
                });
            }
            let expression = self.parse_client_suspending_expression()?;
            Some(ClientFunctionBody::ReturnExpression { expression })
        })();
        self.builder.finish_node();
        result
    }

    /// Parses one CLIENT expression in a suspension-capable statement position.
    ///
    /// `AWAIT` is accepted only as the leading expression form in the
    /// procedural LET, assignment, and RETURN positions that call this helper.
    fn parse_client_suspending_expression(&mut self) -> Option<ClientExpression> {
        if self.current().is_some_and(|token| token.is_word("AWAIT")) {
            self.parse_client_await_expression()
        } else {
            self.parse_client_expression()
        }
    }

    /// Parses the typed CLIENT expression surface with conventional
    /// precedence. Binary operators are left-associative and parentheses are
    /// represented explicitly so every expression keeps an exact source span.
    fn parse_client_expression(&mut self) -> Option<ClientExpression> {
        self.parse_client_binary_expression(1)
    }

    fn parse_client_binary_expression(
        &mut self,
        minimum_precedence: u8,
    ) -> Option<ClientExpression> {
        let mut left = self.parse_client_unary_expression()?;
        loop {
            self.skip_trivia();
            if self.current().is_some_and(|token| token.text == "||") {
                if minimum_precedence > 4 {
                    break;
                }
                self.bump();
                self.skip_trivia();
                let right = self.parse_client_binary_expression(5)?;
                let span = SourceSpan {
                    start: left.span().start,
                    end: right.span().end,
                };
                left = ClientExpression::Concat {
                    left: Box::new(left),
                    right: Box::new(right),
                    span,
                };
                continue;
            }
            let Some((operator, precedence, width)) = self.client_binary_operator() else {
                break;
            };
            if precedence < minimum_precedence {
                break;
            }
            self.consume_client_binary_operator(width);
            self.skip_trivia();
            let right = self.parse_client_binary_expression(precedence + 1)?;
            let span = SourceSpan {
                start: left.span().start,
                end: right.span().end,
            };
            left = ClientExpression::Binary(ClientBinaryExpression {
                left: Box::new(left),
                operator,
                right: Box::new(right),
                span,
            });
        }
        Some(left)
    }

    fn parse_client_unary_expression(&mut self) -> Option<ClientExpression> {
        self.skip_trivia();
        let Some(token) = self.current().cloned() else {
            self.error_current("ORNA0001", "expected a CLIENT expression");
            return None;
        };
        let operator = if token.text == "+" {
            Some(ClientUnaryOperator::Plus)
        } else if token.text == "-" {
            Some(ClientUnaryOperator::Minus)
        } else if token.is_word("NOT") {
            Some(ClientUnaryOperator::Not)
        } else {
            None
        };
        let Some(operator) = operator else {
            return self.parse_client_primary_expression();
        };
        let start = token.range.start;
        self.builder
            .start_node(SyntaxKind::ClientUnaryExpression.into());
        self.bump();
        self.skip_trivia();
        let expression = self.parse_client_unary_expression();
        let result = expression.map(|expression| {
            let span = SourceSpan {
                start,
                end: expression.span().end,
            };
            ClientExpression::Unary(ClientUnaryExpression {
                operator,
                expression: Box::new(expression),
                span,
            })
        });
        self.builder.finish_node();
        result
    }

    fn client_binary_operator(&self) -> Option<(ClientBinaryOperator, u8, usize)> {
        let token = self.current()?;
        let next = self.tokens.get(self.index + 1);
        let adjacent_next = next.is_some_and(|next| next.range.start == token.range.end);
        if token.is_word("OR") {
            Some((ClientBinaryOperator::Or, 1, 1))
        } else if token.is_word("AND") {
            Some((ClientBinaryOperator::And, 2, 1))
        } else if token.text == "=" {
            Some((ClientBinaryOperator::Equal, 3, 1))
        } else if adjacent_next
            && ((token.text == "!" && next.is_some_and(|next| next.text == "="))
                || (token.text == "<" && next.is_some_and(|next| next.text == ">")))
        {
            Some((ClientBinaryOperator::NotEqual, 3, 2))
        } else if token.text == "<" && adjacent_next && next.is_some_and(|next| next.text == "=") {
            Some((ClientBinaryOperator::LessThanOrEqual, 3, 2))
        } else if token.text == ">" && adjacent_next && next.is_some_and(|next| next.text == "=") {
            Some((ClientBinaryOperator::GreaterThanOrEqual, 3, 2))
        } else if token.text == "<" {
            Some((ClientBinaryOperator::LessThan, 3, 1))
        } else if token.text == ">" {
            Some((ClientBinaryOperator::GreaterThan, 3, 1))
        } else if token.text == "+" {
            Some((ClientBinaryOperator::Add, 5, 1))
        } else if token.text == "-" {
            Some((ClientBinaryOperator::Subtract, 5, 1))
        } else if token.text == "*" {
            Some((ClientBinaryOperator::Multiply, 6, 1))
        } else if token.text == "/" {
            Some((ClientBinaryOperator::Divide, 6, 1))
        } else if token.text == "%" {
            Some((ClientBinaryOperator::Modulo, 6, 1))
        } else {
            None
        }
    }

    fn consume_client_binary_operator(&mut self, width: usize) {
        self.bump();
        if width == 2 {
            self.bump();
        }
    }
    fn parse_client_primary_expression(&mut self) -> Option<ClientExpression> {
        let Some(token) = self.current().cloned() else {
            self.error_current("ORNA0001", "expected a CLIENT expression");
            return None;
        };
        if token.is_word("FOR") {
            self.error_current(
                "ORNA0001",
                "CLIENT FOR loops are deferred until their collection/range contract exists",
            );
            return None;
        }
        if token.is_word("AWAIT") {
            self.error_current("ORNA0001", "expected a CLIENT expression");
            return None;
        }
        if token.kind == TokenKind::LeftParenthesis {
            let start = token.range.start;
            self.builder
                .start_node(SyntaxKind::ClientParenthesizedExpression.into());
            self.bump();
            self.skip_trivia();
            let expression = self.parse_client_expression();
            self.skip_trivia();
            let end = self
                .expect_kind(
                    TokenKind::RightParenthesis,
                    "expected ')' to close the CLIENT expression",
                )
                .map(|span| span.end);
            let result = match (expression, end) {
                (Some(expression), Some(end)) => Some(ClientExpression::Parenthesized {
                    expression: Box::new(expression),
                    span: SourceSpan { start, end },
                }),
                _ => None,
            };
            self.builder.finish_node();
            return result;
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
            let value = match token.text.parse::<i64>() {
                Ok(value) => value,
                Err(_) => {
                    self.diagnostics.push(Diagnostic {
                        code: "ORNA0001",
                        message: "CLIENT integer literals must fit in a signed 64-bit value"
                            .to_owned(),
                        span: token.span(),
                    });
                    return None;
                }
            };
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
        if Self::is_client_expression_keyword(&token) || !token.is_identifier() {
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
        for _part in &parts {
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

    fn is_client_expression_keyword(token: &Token<'_>) -> bool {
        token.is_word("AND")
            || token.is_word("OR")
            || token.is_word("NOT")
            || token.is_word("THEN")
            || token.is_word("LOOP")
            || token.is_word("ELSIF")
            || token.is_word("ELSE")
            || token.is_word("END")
            || token.is_word("IF")
            || token.is_word("WHILE")
            || token.is_word("RETURN")
            || token.is_word("LET")
            || token.is_word("STATE")
            || token.is_word("BEGIN")
            || token.is_word("FOR")
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

    /// Parses one CLIENT state/procedural block body.
    ///
    /// Existing simple blocks retain their final top-level `RETURN` in
    /// `return_expression`. Control-flow statements retain nested and early
    /// returns in `ClientProceduralStatement::Return`.
    fn parse_client_state_block(&mut self) -> Option<ClientFunctionBody> {
        self.builder
            .start_node(SyntaxKind::ClientStateBlockBody.into());
        let result = (|| {
            let block_start = self.current().expect("IS token exists").range.start;
            self.bump(); // IS
            let mut states = Vec::new();
            let mut locals = Vec::new();
            loop {
                self.skip_trivia();
                if self.current().is_some_and(|token| token.is_word("BEGIN")) {
                    break;
                }
                if self.current().is_some_and(|token| token.is_word("STATE")) {
                    if !locals.is_empty() {
                        self.error_current(
                            "ORNA0001",
                            "CLIENT state blocks cannot contain pre-BEGIN LET locals",
                        );
                        return None;
                    }
                    states.push(self.parse_client_state_declaration()?);
                    continue;
                }
                if self.current().is_some_and(|token| token.is_word("LET")) {
                    if !states.is_empty() {
                        self.error_current(
                            "ORNA0001",
                            "CLIENT state blocks cannot contain pre-BEGIN LET locals",
                        );
                        return None;
                    }
                    locals.push(self.parse_client_local_binding()?);
                    continue;
                }
                self.error_current(
                    "ORNA0001",
                    "CLIENT blocks accept only STATE or LET declarations before BEGIN",
                );
                return None;
            }
            self.bump(); // BEGIN

            let visible_locals = locals
                .iter()
                .map(|local| local.name.clone())
                .collect::<Vec<_>>();
            let mut statements =
                self.parse_client_statement_block(&visible_locals, &["END"], states.is_empty())?;
            if !states.is_empty()
                && (statements.is_empty()
                    || statements.iter().any(|statement| {
                        !matches!(statement, ClientProceduralStatement::Return(_))
                    }))
            {
                self.error_current(
                    "ORNA0001",
                    "CLIENT state blocks accept only a single RETURN statement",
                );
                return None;
            }
            if !states.is_empty()
                && statements
                    .get(..statements.len().saturating_sub(1))
                    .is_some_and(|prefix| {
                        prefix.iter().any(|statement| {
                            matches!(statement, ClientProceduralStatement::Return(_))
                        })
                    })
            {
                let span = statements
                    .iter()
                    .rev()
                    .find_map(|statement| match statement {
                        ClientProceduralStatement::Return(return_statement) => {
                            let start = return_statement.span.start;
                            Some(SourceSpan {
                                start,
                                end: start + "RETURN".len(),
                            })
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        self.current().map(Token::span).unwrap_or(SourceSpan {
                            start: block_start,
                            end: block_start,
                        })
                    });
                self.diagnostics.push(Diagnostic {
                    code: "ORNA0001",
                    message: "CLIENT blocks accept only a single RETURN statement".to_owned(),
                    span,
                });
                return None;
            }
            let return_expression = match statements.last() {
                Some(ClientProceduralStatement::Return(return_statement)) => {
                    let return_expression = return_statement.expression.clone();
                    statements.pop();
                    return_expression
                }
                _ => None,
            };
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

    /// Parses procedural statements until one of the supplied word terminators.
    fn parse_client_statement_block(
        &mut self,
        outer_locals: &[NamePart],
        terminators: &[&str],
        allow_await: bool,
    ) -> Option<Vec<ClientProceduralStatement>> {
        let mut local_names = outer_locals.to_vec();
        let mut statements = Vec::new();
        loop {
            self.skip_trivia();
            if self
                .current()
                .is_some_and(|token| terminators.iter().any(|word| token.is_word(word)))
            {
                return Some(statements);
            }
            let Some(token) = self.current().cloned() else {
                self.error_current("ORNA0001", "expected a CLIENT block terminator");
                return None;
            };
            if token.is_word("EXCEPTION") && terminators.contains(&"END") {
                return Some(statements);
            }
            if token.is_word("LET") {
                let mut statement = self.parse_client_let_statement()?;
                statement.expression =
                    rewrite_client_local_name_references(statement.expression, &local_names);
                local_names.push(statement.name.clone());
                statements.push(ClientProceduralStatement::Let(statement));
                continue;
            }
            if token.is_word("RETURN") {
                let mut return_statement = self.parse_client_return_statement(allow_await)?;
                return_statement.expression = return_statement.expression.map(|expression| {
                    rewrite_client_local_name_references(expression, &local_names)
                });
                statements.push(ClientProceduralStatement::Return(return_statement));
                continue;
            }
            if token.is_word("IF") {
                statements.push(ClientProceduralStatement::If(
                    self.parse_client_if_statement(&local_names, allow_await)?,
                ));
                continue;
            }
            if token.is_word("WHILE") {
                statements.push(ClientProceduralStatement::While(
                    self.parse_client_while_statement(&local_names, allow_await)?,
                ));
                continue;
            }
            if token.is_identifier()
                && self
                    .peek_significant(1)
                    .is_some_and(|next| next.text == ":")
            {
                let mut statement = self.parse_client_assignment_statement()?;
                statement.expression =
                    rewrite_client_local_name_references(statement.expression, &local_names);
                statements.push(ClientProceduralStatement::Assignment(statement));
                continue;
            }
            self.error_current(
                "ORNA0001",
                "CLIENT blocks accept LET, assignment, IF, WHILE, or RETURN statements",
            );
            return None;
        }
    }

    fn parse_client_return_statement(
        &mut self,
        allow_await: bool,
    ) -> Option<ClientReturnStatement> {
        self.builder
            .start_node(SyntaxKind::ClientReturnStatement.into());
        let result = (|| {
            let return_token = self.current().cloned()?;
            self.bump(); // RETURN
            self.skip_trivia();
            if !allow_await && self.current().is_some_and(|token| token.is_word("AWAIT")) {
                self.error_current("ORNA0001", "expected a CLIENT expression");
                return None;
            }
            let expression = if self
                .current()
                .is_some_and(|token| token.kind == TokenKind::Semicolon)
            {
                None
            } else {
                Some(self.parse_client_suspending_expression()?)
            };
            self.skip_trivia();
            let semicolon =
                self.expect_kind(TokenKind::Semicolon, "expected ';' after CLIENT RETURN")?;
            Some(ClientReturnStatement {
                expression,
                span: SourceSpan {
                    start: return_token.range.start,
                    end: semicolon.end,
                },
            })
        })();
        self.builder.finish_node();
        result
    }
    /// Parses one nested CLIENT `IF` statement and its branch bodies.
    fn parse_client_if_statement(
        &mut self,
        outer_locals: &[NamePart],
        allow_await: bool,
    ) -> Option<ClientIfStatement> {
        self.builder
            .start_node(SyntaxKind::ClientIfStatement.into());
        let result = (|| {
            let if_token = self.current().cloned()?;
            self.bump(); // IF
            self.skip_trivia();
            let condition =
                rewrite_client_local_name_references(self.parse_client_expression()?, outer_locals);
            self.skip_trivia();
            self.expect_word_token("THEN")?;
            let then_statements = self.parse_client_statement_block(
                outer_locals,
                &["ELSIF", "ELSE", "END"],
                allow_await,
            )?;
            let mut elsif_branches = Vec::new();
            while self.current().is_some_and(|token| token.is_word("ELSIF")) {
                self.builder.start_node(SyntaxKind::ClientIfBranch.into());
                let branch = (|| {
                    let branch_start = self.current().expect("ELSIF token exists").range.start;
                    self.bump(); // ELSIF
                    self.skip_trivia();
                    let condition = rewrite_client_local_name_references(
                        self.parse_client_expression()?,
                        outer_locals,
                    );
                    self.skip_trivia();
                    self.expect_word_token("THEN")?;
                    let statements = self.parse_client_statement_block(
                        outer_locals,
                        &["ELSIF", "ELSE", "END"],
                        allow_await,
                    )?;
                    let end = self.previous_significant_end(condition.span().end);
                    Some(ClientIfBranch {
                        condition,
                        statements,
                        span: SourceSpan {
                            start: branch_start,
                            end,
                        },
                    })
                })();
                self.builder.finish_node();
                elsif_branches.push(branch?);
            }
            let else_statements = if self.current().is_some_and(|token| token.is_word("ELSE")) {
                self.bump();
                Some(self.parse_client_statement_block(outer_locals, &["END"], allow_await)?)
            } else {
                None
            };
            let end_token = self.expect_word_token("END")?;
            self.skip_trivia();
            if self.take_word("IF").is_none() {
                self.diagnostics.push(Diagnostic {
                    code: "ORNA0001",
                    message: "expected keyword IF".to_owned(),
                    span: end_token.span(),
                });
                return None;
            }
            self.skip_trivia();
            let semicolon = self.expect_kind(TokenKind::Semicolon, "expected ';' after END IF")?;
            Some(ClientIfStatement {
                condition,
                then_statements,
                elsif_branches,
                else_statements,
                span: SourceSpan {
                    start: if_token.range.start,
                    end: semicolon.end.max(end_token.range.end),
                },
            })
        })();
        self.builder.finish_node();
        result
    }

    /// Parses one nested CLIENT `WHILE` statement and its body.
    fn parse_client_while_statement(
        &mut self,
        outer_locals: &[NamePart],
        allow_await: bool,
    ) -> Option<ClientWhileStatement> {
        self.builder
            .start_node(SyntaxKind::ClientWhileStatement.into());
        let result = (|| {
            let while_token = self.current().cloned()?;
            self.bump(); // WHILE
            self.skip_trivia();
            let condition =
                rewrite_client_local_name_references(self.parse_client_expression()?, outer_locals);
            self.skip_trivia();
            self.expect_word_token("LOOP")?;
            let body = self.parse_client_statement_block(outer_locals, &["END"], allow_await)?;
            let end_token = self.expect_word_token("END")?;
            self.skip_trivia();
            self.expect_word_token("LOOP")?;
            self.skip_trivia();
            let semicolon =
                self.expect_kind(TokenKind::Semicolon, "expected ';' after END LOOP")?;
            Some(ClientWhileStatement {
                condition,
                body,
                span: SourceSpan {
                    start: while_token.range.start,
                    end: semicolon.end.max(end_token.range.end),
                },
            })
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
                if self
                    .current()
                    .is_some_and(|token| token.kind == TokenKind::Semicolon)
                {
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
            let expression = self.parse_client_suspending_expression()?;
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
                    if self
                        .current()
                        .is_some_and(|token| token.kind == TokenKind::Semicolon)
                    {
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
            let expression = self.parse_client_suspending_expression()?;
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
            let expression = self.parse_client_suspending_expression()?;
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
}
