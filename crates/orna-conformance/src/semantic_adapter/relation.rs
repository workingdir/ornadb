use super::*;

/// The metadata one relation-lowering pass threads through every
/// mutually recursive step: the module's table key, field and decimal-field
/// maps, its functions, the namespace and shadowed names of the function
/// being walked. Only a nested scope replaces the context, through
/// [`RelationLowering::with_shadowed`].
#[derive(Clone, Copy)]
pub(super) struct RelationLowering<'a> {
    pub(super) table_keys: &'a TableKeys,
    pub(super) float_fields: &'a TableFloatFields,
    pub(super) table_fields: &'a TableFields,
    pub(super) functions: &'a Functions,
    pub(super) namespace: Option<&'a str>,
    pub(super) shadowed: &'a BTreeSet<String>,
}

impl<'a> RelationLowering<'a> {
    /// The same lowering with a narrower set of shadowed names.
    pub(super) fn with_shadowed<'b>(self, shadowed: &'b BTreeSet<String>) -> RelationLowering<'b>
    where
        'a: 'b,
    {
        RelationLowering {
            table_keys: self.table_keys,
            float_fields: self.float_fields,
            table_fields: self.table_fields,
            functions: self.functions,
            namespace: self.namespace,
            shadowed,
        }
    }

    /// The terminal `rhs` names (`first`, `one()` or `count`), unless a binding
    /// shadows the root intrinsic.
    pub(super) fn terminal(&self, rhs: &Expr) -> Option<FilteredTerminal> {
        let unshadowed = |name: &str| self.root_relation_intrinsic_is_unshadowed(name);
        if relation_first_target(rhs) && unshadowed("first") {
            Some(FilteredTerminal::First)
        } else if relation_one_target(rhs) && unshadowed("one") {
            Some(FilteredTerminal::One)
        } else if self.relation_count_target(rhs) {
            Some(FilteredTerminal::Count)
        } else {
            None
        }
    }

    /// Whether the compared field can be served by this lowering. A first-match
    /// scan needs a stored decimal field. A count or a cardinality-checked
    /// `one` also accepts a key field, and a comparison written value-first
    /// still needs a decimal field.
    pub(super) fn accepts_field(
        &self,
        terminal: FilteredTerminal,
        filter: &EqualityFilter<'_>,
    ) -> bool {
        let stored = self
            .table_fields
            .get(filter.table)
            .is_some_and(|fields| fields.contains(filter.field));
        let decimal = self
            .float_fields
            .contains_decimal(filter.table, filter.field);
        match terminal {
            FilteredTerminal::First => stored && decimal,
            FilteredTerminal::One | FilteredTerminal::Count => {
                let keyed = self
                    .table_keys
                    .get(filter.table)
                    .is_some_and(|key| key.fields.iter().any(|name| name == filter.field));
                (stored || keyed) && (!filter.reversed || decimal)
            }
        }
    }

    pub(super) fn lower_relation_expression_with_resolution(&self, expression: &mut Expr) {
        if let Some(lowered) = self
            .relation_quantifier(expression)
            .or_else(|| self.relation_aggregate(expression))
            .or_else(|| self.relation_lookup(expression))
            .or_else(|| self.relation_filter_terminal(expression))
        {
            *expression = lowered;
            return;
        }
        self.lower_core_relation_input(expression);
        match expression {
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
                self.lower_relation_expression_with_resolution(rhs);
            }
            Expr::Range { lower, upper, .. } => {
                for endpoint in [lower, upper].into_iter().flatten() {
                    self.lower_relation_expression_with_resolution(endpoint);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.lower_relation_expression_with_resolution(lhs);
                self.lower_relation_expression_with_resolution(rhs);
            }
            Expr::Call {
                callee, arguments, ..
            } => {
                self.lower_relation_expression_with_resolution(callee);
                for argument in arguments {
                    self.lower_relation_expression_with_resolution(&mut argument.value);
                }
            }
            Expr::GenericCall {
                callee, arguments, ..
            } => {
                // Generic applications have the same expression children as an
                // ordinary call.  Their type arguments are syntax-only here;
                // semantic admission owns their validation, while this bounded
                // relation rewriter must still visit the callee and values in
                // source order without inventing evaluator support.
                self.lower_relation_expression_with_resolution(callee);
                for argument in arguments {
                    self.lower_relation_expression_with_resolution(&mut argument.value);
                }
            }
            Expr::Index { base, index, .. } => {
                self.lower_relation_expression_with_resolution(base);
                self.lower_relation_expression_with_resolution(index);
            }
            Expr::Field { base, .. } if matches!(base.as_ref(), Expr::Name { text, .. } if self.table_keys.contains_key(text)) =>
                {}
            Expr::Field { base, .. } => self.lower_relation_expression_with_resolution(base),
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    self.lower_relation_expression_with_resolution(element);
                }
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                for field in fields {
                    self.lower_relation_expression_with_resolution(&mut field.value);
                }
            }
            Expr::Lambda {
                parameters, body, ..
            } => {
                let mut shadowed = self.shadowed.clone();
                for parameter in parameters {
                    shadowed.extend(relation_pattern_names(&parameter.pattern));
                }
                self.with_shadowed(&shadowed)
                    .lower_relation_expression_with_resolution(body);
            }
            Expr::Block {
                statements, tail, ..
            } => {
                let mut shadowed = self.shadowed.clone();
                for statement in statements {
                    self.lower_relation_statement_with_resolution(statement, &mut shadowed);
                }
                if let Some(tail) = tail {
                    self.with_shadowed(&shadowed)
                        .lower_relation_expression_with_resolution(tail);
                }
            }
            Expr::Control {
                condition,
                body,
                alternate,
                ..
            } => {
                for expression in [condition, body, alternate].into_iter().flatten() {
                    self.lower_relation_expression_with_resolution(expression);
                }
            }
            Expr::Name { .. }
            | Expr::Literal { .. }
            | Expr::InterpolatedString { .. }
            | Expr::ReplBinding { .. } => {}
        }
    }

    pub(super) fn lower_core_relation_input(&self, expression: &mut Expr) {
        let table = match expression {
            Expr::Name { text, .. }
                if self.table_keys.contains_key(text)
                    && self.table_fields.contains_key(text)
                    && !self.shadowed.contains(text) =>
            {
                Some(text.clone())
            }
            _ => None,
        };
        if let Some(table) = table {
            let span = expression.span();
            *expression = relation_source_expression(&table, span);
            return;
        }
        match expression {
            Expr::Group { inner, .. } => self.lower_core_relation_input(inner),
            Expr::Binary { lhs, op, rhs, .. }
                if op == "|" && self.relation_stage_name(rhs).is_some() =>
            {
                self.lower_core_relation_input(lhs);
            }
            Expr::Call {
                callee, arguments, ..
            } if self.relation_stage_name(callee).is_some() => {
                let relation_index = arguments
                    .iter()
                    .position(|argument| argument.name.as_deref() == Some("rows"))
                    .or_else(|| (!arguments.is_empty()).then_some(0));
                if let Some(relation_index) = relation_index {
                    self.lower_core_relation_input(&mut arguments[relation_index].value);
                }
            }
            _ => {}
        }
    }

    pub(super) fn relation_stage_name<'e>(&self, expression: &'e Expr) -> Option<&'e str> {
        let Expr::Name { text, .. } = expression else {
            return None;
        };
        if !matches!(
            text.as_str(),
            "filter"
                | "map"
                | "flat_map"
                | "sort_by"
                | "distinct"
                | "take"
                | "drop"
                | "window"
                | "count"
                | "first"
                | "one"
                | "every"
                | "exists"
                | "sum"
                | "min"
                | "max"
        ) {
            return None;
        }
        self.root_relation_intrinsic_is_unshadowed(text)
            .then_some(text.as_str())
    }

    pub(super) fn root_relation_intrinsic_is_unshadowed(&self, name: &str) -> bool {
        !self.shadowed.contains(name)
            && !self.functions.contains_key(name)
            && self
                .namespace
                .map(|namespace| !self.functions.contains_key(&format!("{namespace}.{name}")))
                .unwrap_or(true)
    }

    /// Lowers ordinary `every`/`exists` calls to a lazy relation plan.  The
    /// negated/positive filter followed by `take(1)` preserves canonical scan
    /// order, decisive short-circuiting and predicate failures without general
    /// relation materialization.
    pub(super) fn relation_quantifier(&self, expression: &Expr) -> Option<Expr> {
        let (operation, table, predicate) = match expression {
            Expr::Call {
                callee, arguments, ..
            } => {
                let Expr::Name {
                    text: operation, ..
                } = callee.as_ref()
                else {
                    return None;
                };
                if !matches!(operation.as_str(), "every" | "exists")
                    || !self.root_relation_intrinsic_is_unshadowed(operation)
                {
                    return None;
                }
                let mut positional = 0usize;
                let mut table = None;
                let mut predicate = None;
                for argument in arguments {
                    let slot = match argument.name.as_deref() {
                        Some("rows") => &mut table,
                        Some("predicate") => &mut predicate,
                        Some(_) => return None,
                        None => {
                            let slot = match positional {
                                0 => &mut table,
                                1 => &mut predicate,
                                _ => return None,
                            };
                            positional += 1;
                            slot
                        }
                    };
                    if slot.replace(&argument.value).is_some() {
                        return None;
                    }
                }
                (operation.as_str(), table?, predicate?)
            }
            Expr::Binary { lhs, op, rhs, .. } if op == "|" => {
                let Expr::Call {
                    callee, arguments, ..
                } = rhs.as_ref()
                else {
                    return None;
                };
                let Expr::Name {
                    text: operation, ..
                } = callee.as_ref()
                else {
                    return None;
                };
                if !matches!(operation.as_str(), "every" | "exists")
                    || !self.root_relation_intrinsic_is_unshadowed(operation)
                {
                    return None;
                }
                let [argument] = arguments.as_slice() else {
                    return None;
                };
                if argument
                    .name
                    .as_deref()
                    .is_some_and(|name| name != "predicate")
                {
                    return None;
                }
                (operation.as_str(), lhs.as_ref(), &argument.value)
            }
            _ => return None,
        };
        let Expr::Name {
            text: table_name,
            span: table_span,
        } = table
        else {
            return None;
        };
        if !self.table_keys.contains_key(table_name)
            || self.shadowed.contains(table_name)
            || !self.root_relation_intrinsic_is_unshadowed("filter")
            || !self.root_relation_intrinsic_is_unshadowed("take")
            || !self.root_relation_intrinsic_is_unshadowed("count")
        {
            return None;
        }
        let Expr::Lambda {
            parameters,
            body,
            span: predicate_span,
        } = predicate
        else {
            return None;
        };
        if parameters.len() != 1 {
            return None;
        }
        let mut callback_shadowed = self.shadowed.clone();
        for parameter in parameters {
            callback_shadowed.extend(relation_pattern_names(&parameter.pattern));
        }
        let mut callback_body = body.as_ref().clone();
        self.with_shadowed(&callback_shadowed)
            .lower_relation_expression_with_resolution(&mut callback_body);
        let callback_body = if operation == "every" {
            Expr::Unary {
                op: "!".into(),
                rhs: Box::new(callback_body),
                span: body.span(),
            }
        } else {
            callback_body
        };
        let callback = Expr::Lambda {
            parameters: parameters.clone(),
            body: Box::new(callback_body),
            span: predicate_span.clone(),
        };
        let span = expression.span();
        let filter = Expr::Call {
            callee: Box::new(Expr::Name {
                text: "filter".into(),
                span: span.clone(),
            }),
            arguments: vec![orna_syntax_ast::Argument {
                name: None,
                value: callback,
                span: predicate_span.clone(),
            }],
            span: span.clone(),
        };
        let filtered = Expr::Binary {
            lhs: Box::new(relation_source_expression(table_name, table_span.clone())),
            op: "|".into(),
            rhs: Box::new(filter),
            span: span.clone(),
        };
        let take = Expr::Call {
            callee: Box::new(Expr::Name {
                text: "take".into(),
                span: span.clone(),
            }),
            arguments: vec![orna_syntax_ast::Argument {
                name: None,
                value: Expr::Literal {
                    text: "1".into(),
                    kind: orna_syntax_ast::LiteralKind::Integer,
                    span: span.clone(),
                },
                span: span.clone(),
            }],
            span: span.clone(),
        };
        let limited = Expr::Binary {
            lhs: Box::new(filtered),
            op: "|".into(),
            rhs: Box::new(take),
            span: span.clone(),
        };
        let counted = Expr::Binary {
            lhs: Box::new(limited),
            op: "|".into(),
            rhs: Box::new(Expr::Call {
                callee: Box::new(Expr::Name {
                    text: "count".into(),
                    span: span.clone(),
                }),
                arguments: Vec::new(),
                span: span.clone(),
            }),
            span: span.clone(),
        };
        Some(Expr::Binary {
            lhs: Box::new(counted),
            op: if operation == "every" {
                "==".into()
            } else {
                ">".into()
            },
            rhs: Box::new(Expr::Literal {
                text: "0".into(),
                kind: orna_syntax_ast::LiteralKind::Integer,
                span: span.clone(),
            }),
            span,
        })
    }

    pub(super) fn relation_aggregate(&self, expression: &Expr) -> Option<Expr> {
        let Expr::Binary { lhs, op, rhs, .. } = expression else {
            return None;
        };
        if op != "|" {
            return None;
        }
        let operation = self.relation_integer_aggregate_operation(rhs)?;
        let Expr::Binary {
            lhs: relation,
            op: map_op,
            rhs: map,
            ..
        } = lhs.as_ref()
        else {
            return None;
        };
        let Expr::Name {
            text: table,
            span: table_span,
        } = relation.as_ref()
        else {
            return None;
        };
        if map_op != "|" || !self.table_keys.contains_key(table) {
            return None;
        }
        let Expr::Call {
            callee, arguments, ..
        } = map.as_ref()
        else {
            return None;
        };
        if !matches!(callee.as_ref(), Expr::Name { text, .. } if text == "map")
            || !self.root_relation_intrinsic_is_unshadowed("map")
        {
            return None;
        }
        let [argument] = arguments.as_slice() else {
            return None;
        };
        if argument.name.is_some() {
            return None;
        }
        let Expr::Lambda {
            parameters, body, ..
        } = &argument.value
        else {
            return None;
        };
        let [parameter] = parameters.as_slice() else {
            return None;
        };
        let Pattern::Name(binding, _) = &parameter.pattern else {
            return None;
        };
        let Expr::Field {
            base, name: field, ..
        } = body.as_ref()
        else {
            return None;
        };
        if !matches!(base.as_ref(), Expr::Name { text, .. } if text == binding) {
            return None;
        }
        let kind = if operation == "sum"
            && self
                .float_fields
                .get(table)
                .is_some_and(|fields| fields.contains(field))
        {
            "float"
        } else if self.float_fields.contains_decimal(table, field) {
            "decimal"
        } else {
            "integer"
        };

        let table = Expr::Literal {
            text: format!("{table:?}"),
            kind: orna_syntax_ast::LiteralKind::String,
            span: table_span.clone(),
        };
        let field = Expr::Literal {
            text: format!("{field:?}"),
            kind: orna_syntax_ast::LiteralKind::String,
            span: body.span(),
        };
        let operation = Expr::Literal {
            text: format!("{operation:?}"),
            kind: orna_syntax_ast::LiteralKind::String,
            span: rhs.span(),
        };
        let kind = Expr::Literal {
            text: format!("{kind:?}"),
            kind: orna_syntax_ast::LiteralKind::String,
            span: body.span(),
        };
        Some(Expr::Call {
            callee: Box::new(Expr::Field {
                base: Box::new(Expr::ReplBinding {
                    text: "$__orna_relation".into(),
                    span: expression.span(),
                }),
                name: "aggregate".into(),
                span: expression.span(),
            }),
            arguments: vec![
                orna_syntax_ast::Argument {
                    name: None,
                    span: table.span(),
                    value: table,
                },
                orna_syntax_ast::Argument {
                    name: None,
                    span: field.span(),
                    value: field,
                },
                orna_syntax_ast::Argument {
                    name: None,
                    span: operation.span(),
                    value: operation,
                },
                orna_syntax_ast::Argument {
                    name: None,
                    span: kind.span(),
                    value: kind,
                },
            ],
            span: expression.span(),
        })
    }

    pub(super) fn relation_integer_aggregate_operation<'e>(
        &self,
        expression: &'e Expr,
    ) -> Option<&'e str> {
        match expression {
            Expr::Name { text, .. }
                if matches!(text.as_str(), "min" | "max" | "sum")
                    && self.root_relation_intrinsic_is_unshadowed(text) =>
            {
                Some(text)
            }
            Expr::Call {
                callee, arguments, ..
            } if arguments.is_empty() => match callee.as_ref() {
                Expr::Name { text, .. }
                    if matches!(text.as_str(), "min" | "max" | "sum")
                        && self.root_relation_intrinsic_is_unshadowed(text) =>
                {
                    Some(text)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Lowers one statement's expressions in the block scope `shadowed`, then
    /// extends that scope with the names the statement declares.
    pub(super) fn lower_relation_statement_with_resolution(
        &self,
        statement: &mut Statement,
        shadowed: &mut BTreeSet<String>,
    ) {
        match statement {
            Statement::Let { pattern, value, .. } => {
                self.with_shadowed(shadowed)
                    .lower_relation_expression_with_resolution(value);
                shadowed.extend(relation_pattern_names(pattern));
            }
            Statement::Assert { value, .. }
            | Statement::Assignment { value, .. }
            | Statement::Expression { value, .. }
            | Statement::Control { value, .. } => {
                self.with_shadowed(shadowed)
                    .lower_relation_expression_with_resolution(value);
            }
            Statement::Return { value, .. } | Statement::Break { value, .. } => {
                if let Some(value) = value {
                    self.with_shadowed(shadowed)
                        .lower_relation_expression_with_resolution(value);
                }
            }
            Statement::Continue { .. } => {}
        }
    }

    pub(super) fn relation_count_target(&self, expression: &Expr) -> bool {
        let name = match expression {
            Expr::Name { text, .. } => text,
            Expr::Call {
                callee, arguments, ..
            } => {
                if !arguments.is_empty() {
                    return false;
                }
                let Expr::Name { text, .. } = callee.as_ref() else {
                    return false;
                };
                text
            }
            _ => return false,
        };
        name == "count" && self.root_relation_intrinsic_is_unshadowed(name)
    }

    /// Recognises `<Table> | filter(|row| row.field == value)` over a known,
    /// unshadowed table, with the comparison written in either operand order.
    pub(super) fn equality_filter<'e>(&self, source: &'e Expr) -> Option<EqualityFilter<'e>> {
        let Expr::Binary {
            lhs: table,
            op: filter_op,
            rhs: filter,
            ..
        } = source
        else {
            return None;
        };
        let Expr::Name {
            text: table,
            span: table_span,
        } = table.as_ref()
        else {
            return None;
        };
        if filter_op != "|"
            || self.shadowed.contains(table)
            || !self.table_keys.contains_key(table)
            || !self.table_fields.contains_key(table)
            || !self.root_relation_intrinsic_is_unshadowed("filter")
        {
            return None;
        }
        let Expr::Call {
            callee, arguments, ..
        } = filter.as_ref()
        else {
            return None;
        };
        if !matches!(callee.as_ref(), Expr::Name { text, .. } if text == "filter") {
            return None;
        }
        let [argument] = arguments.as_slice() else {
            return None;
        };
        let Expr::Lambda {
            parameters, body, ..
        } = &argument.value
        else {
            return None;
        };
        let [parameter] = parameters.as_slice() else {
            return None;
        };
        let Pattern::Name(binding, _) = &parameter.pattern else {
            return None;
        };
        let Expr::Binary {
            lhs: predicate_lhs,
            op: predicate_op,
            rhs: predicate_rhs,
            ..
        } = body.as_ref()
        else {
            return None;
        };
        if predicate_op != "==" {
            return None;
        }
        let row_field = |operand: &'e Expr| match operand {
            Expr::Field { base, name, .. } if matches!(base.as_ref(), Expr::Name { text, .. } if text == binding) => {
                Some(name)
            }
            _ => None,
        };
        let (field_expr, field, expected, reversed) = match (
            row_field(predicate_lhs.as_ref()),
            row_field(predicate_rhs.as_ref()),
        ) {
            (Some(field), _) => (predicate_lhs.as_ref(), field, predicate_rhs.as_ref(), false),
            (None, Some(field)) => (predicate_rhs.as_ref(), field, predicate_lhs.as_ref(), true),
            (None, None) => return None,
        };
        Some(EqualityFilter {
            table,
            table_span,
            field,
            field_expr,
            expected,
            reversed,
        })
    }

    /// Lowers `<Table> | filter(|row| row.field == value) | first`, `| one()` and
    /// `| count` to one effect-boundary lookup that keeps the filtered relation's
    /// canonical scan order. The boundary can then stop after the first match, or
    /// after a second one to tell a zero-match from a multiple-match cardinality
    /// failure.
    pub(super) fn relation_filter_terminal(&self, expression: &Expr) -> Option<Expr> {
        let Expr::Binary { lhs, op, rhs, .. } = expression else {
            return None;
        };
        if op != "|" {
            return None;
        }
        let terminal = self.terminal(rhs)?;
        let filter = self.equality_filter(lhs)?;
        self.accepts_field(terminal, &filter)
            .then(|| filtered_relation_call(terminal.method(), &filter, expression))
    }

    pub(super) fn relation_lookup(&self, expression: &Expr) -> Option<Expr> {
        let Expr::Binary { lhs, op, rhs, .. } = expression else {
            return None;
        };
        if op != "|"
            || !relation_one_target(rhs)
            || !self.root_relation_intrinsic_is_unshadowed("one")
        {
            return None;
        }
        let Expr::Binary {
            lhs: table,
            op: filter_op,
            rhs: filter,
            ..
        } = lhs.as_ref()
        else {
            return None;
        };
        let Expr::Name {
            text: table,
            span: table_span,
        } = table.as_ref()
        else {
            return None;
        };
        let Expr::Call {
            callee, arguments, ..
        } = filter.as_ref()
        else {
            return None;
        };
        let [argument] = arguments.as_slice() else {
            return None;
        };
        let Expr::Name {
            text: filter_name, ..
        } = callee.as_ref()
        else {
            return None;
        };
        let Expr::Lambda {
            parameters, body, ..
        } = &argument.value
        else {
            return None;
        };
        let [parameter] = parameters.as_slice() else {
            return None;
        };
        let Pattern::Name(binding, _) = &parameter.pattern else {
            return None;
        };
        if filter_op != "|"
            || filter_name != "filter"
            || !self.root_relation_intrinsic_is_unshadowed("filter")
        {
            return None;
        }
        let mut arguments = Vec::new();
        let mut fields = Vec::new();
        relation_key_arguments(body, binding, &mut fields)?;
        let declared_keys = self.table_keys.get(table)?;
        if fields.len() != declared_keys.len()
            || fields
                .iter()
                .zip(&declared_keys.fields)
                .any(|((field, _), declared)| field != declared)
        {
            return None;
        }
        arguments.extend(fields.into_iter().map(|(_, value)| value));
        Some(Expr::Call {
            callee: Box::new(Expr::Field {
                base: Box::new(Expr::Name {
                    text: table.clone(),
                    span: table_span.clone(),
                }),
                name: "lookup".into(),
                span: expression.span(),
            }),
            arguments: arguments
                .into_iter()
                .map(|value| orna_syntax_ast::Argument {
                    name: None,
                    span: value.span(),
                    value,
                })
                .collect(),
            span: expression.span(),
        })
    }
}

/// Rewrites the narrow relation forms admitted by the durable transaction
/// seam.  Quantifiers remain lazy plans (`filter` + `take(1)` + `count`),
/// while aggregate, filtered-cardinality and lookup forms use their existing
/// candidate-relation effects.  Other relation operators stay unsupported
/// rather than being materialized by this seam.
/// Internal forms use a ReplBinding AST marker which ordinary source cannot
/// spell, rather than an undocumented table member.
pub(super) fn lower_relation_bindings(
    functions: &Functions,
    table_keys: &TableKeys,
    float_fields: &TableFloatFields,
    table_fields: &TableFields,
) -> Functions {
    let mut materialized = functions.clone();
    for (name, function) in materialized.local_iter_mut() {
        let mut shadowed = BTreeSet::new();
        for parameter in &function.parameters {
            shadowed.extend(relation_pattern_names(&parameter.pattern));
        }
        let lowering = RelationLowering {
            table_keys,
            float_fields,
            table_fields,
            functions,
            namespace: name.rsplit_once('.').map(|(namespace, _)| namespace),
            shadowed: &shadowed,
        };
        lowering.lower_relation_expression_with_resolution(&mut function.body);
    }
    materialized
}

pub(super) fn relation_source_expression(table: &str, span: orna_syntax_ast::SourceSpan) -> Expr {
    Expr::Call {
        callee: Box::new(Expr::Field {
            base: Box::new(Expr::ReplBinding {
                text: "$__orna_relation".into(),
                span: span.clone(),
            }),
            name: "source".into(),
            span: span.clone(),
        }),
        arguments: vec![orna_syntax_ast::Argument {
            name: None,
            value: Expr::Literal {
                text: format!("{table:?}"),
                kind: orna_syntax_ast::LiteralKind::String,
                span: span.clone(),
            },
            span: span.clone(),
        }],
        span,
    }
}

pub(super) fn relation_pattern_names(pattern: &Pattern) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    pub(super) fn collect(pattern: &Pattern, names: &mut BTreeSet<String>) {
        match pattern {
            Pattern::Name(name, _) => {
                if name != "_" {
                    names.insert(name.clone());
                }
            }
            Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
                for pattern in elements {
                    collect(pattern, names);
                }
            }
            Pattern::Record { fields, .. } => {
                for (name, pattern, _) in fields {
                    if let Some(pattern) = pattern {
                        collect(pattern, names);
                    } else {
                        names.insert(name.clone());
                    }
                }
            }
            Pattern::Constructor {
                arguments, fields, ..
            } => {
                for pattern in arguments {
                    collect(pattern, names);
                }
                for field in fields {
                    if let Some(pattern) = &field.pattern {
                        collect(pattern, names);
                    } else {
                        names.insert(field.name.clone());
                    }
                }
            }
            Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
        }
    }
    collect(pattern, &mut names);
    names
}

pub(super) fn relation_first_target(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Name { text, .. } if text == "first"
    ) || matches!(
        expression,
        Expr::Call {
            callee, arguments, ..
        } if arguments.is_empty() && matches!(callee.as_ref(), Expr::Name { text, .. } if text == "first")
    )
}

/// The terminal operation of a lowered equality-filter lookup.
#[derive(Clone, Copy)]
pub(super) enum FilteredTerminal {
    First,
    One,
    Count,
}

impl FilteredTerminal {
    /// The `$__orna_relation` method the effect boundary serves this terminal
    /// with.
    pub(super) fn method(self) -> &'static str {
        match self {
            Self::First => "filtered_first",
            Self::One => "filtered_one",
            Self::Count => "filtered_count",
        }
    }
}

/// The `<Table> | filter(|row| row.field == value)` shape that every filtered
/// relation lowering recognises.
pub(super) struct EqualityFilter<'a> {
    pub(super) table: &'a String,
    pub(super) table_span: &'a orna_syntax_ast::SourceSpan,
    pub(super) field: &'a String,
    /// The `row.field` side of the comparison, for its span.
    pub(super) field_expr: &'a Expr,
    pub(super) expected: &'a Expr,
    /// The expected value was written first: `value == row.field`.
    pub(super) reversed: bool,
}

/// `$__orna_relation.<method>("Table", "field", expected)`.
pub(super) fn filtered_relation_call(
    method: &str,
    filter: &EqualityFilter<'_>,
    expression: &Expr,
) -> Expr {
    let positional = |value: Expr| orna_syntax_ast::Argument {
        name: None,
        span: value.span(),
        value,
    };
    let table = Expr::Literal {
        text: format!("{:?}", filter.table),
        kind: orna_syntax_ast::LiteralKind::String,
        span: filter.table_span.clone(),
    };
    let field = Expr::Literal {
        text: format!("{:?}", filter.field),
        kind: orna_syntax_ast::LiteralKind::String,
        span: filter.field_expr.span(),
    };
    Expr::Call {
        callee: Box::new(Expr::Field {
            base: Box::new(Expr::ReplBinding {
                text: "$__orna_relation".into(),
                span: expression.span(),
            }),
            name: method.into(),
            span: expression.span(),
        }),
        arguments: vec![
            positional(table),
            positional(field),
            positional(filter.expected.clone()),
        ],
        span: expression.span(),
    }
}

pub(super) fn relation_one_target(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Call {
            callee, arguments, ..
        } if arguments.is_empty() && matches!(callee.as_ref(), Expr::Name { text, .. } if text == "one")
    )
}

pub(super) fn relation_key_arguments(
    expression: &Expr,
    binding: &str,
    output: &mut Vec<(String, Expr)>,
) -> Option<()> {
    if let Expr::Binary { lhs, op, rhs, .. } = expression
        && op == "&&"
    {
        relation_key_arguments(lhs, binding, output)?;
        return relation_key_arguments(rhs, binding, output);
    }
    let Expr::Binary { lhs, op, rhs, .. } = expression else {
        return None;
    };
    let Expr::Field { base, name, .. } = lhs.as_ref() else {
        return None;
    };
    if op != "==" || !matches!(base.as_ref(), Expr::Name { text, .. } if text == binding) {
        return None;
    }
    if output.iter().any(|(field, _)| field == name) {
        return None;
    }
    output.push((name.clone(), rhs.as_ref().clone()));
    Some(())
}
