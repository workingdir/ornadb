//! SQL query and data-mutation body parsing.

use super::*;
pub(super) struct QueryParseError {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) span: SourceSpan,
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
pub(super) fn no_input_parameter_identifier<'source>(
    tokens: &[Token<'source>],
) -> Option<Token<'source>> {
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

pub(super) fn parse_select_query(tokens: &[Token<'_>]) -> Result<SelectQuery, QueryParseError> {
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

pub(super) fn parse_sql_insert(tokens: &[Token<'_>]) -> Result<InsertStatement, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Insert).parse_insert()
}

pub(super) fn parse_sql_update(tokens: &[Token<'_>]) -> Result<UpdateStatement, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Update).parse_update()
}

pub(super) fn parse_sql_delete(tokens: &[Token<'_>]) -> Result<DeleteStatement, QueryParseError> {
    SqlBodyParser::new(tokens, SqlBodySyntax::Delete).parse_delete()
}
