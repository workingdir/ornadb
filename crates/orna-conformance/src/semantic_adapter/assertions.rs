use super::*;

/// Checks every candidate row before the root activation can publish. This
/// deliberately admits only table-local `every` and projection-based
/// `all_unique`; module assertions remain outside this bounded source-execution
/// seam.
pub(super) fn validate_table_assertions(
    activation: &TransactionActivation<'_>,
    assertions: &TableAssertions,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
) -> Result<(), EvaluationError> {
    for (table, assertions) in assertions {
        for assertion in assertions {
            if table_assertion_function(&assertion.expression, functions).is_some() {
                let mut effects = ActivationRelationEffectHandler { activation };
                let value = evaluate_function_assertion(
                    table,
                    &assertion.expression,
                    functions,
                    limits,
                    budget,
                    &mut effects,
                )?;
                if !matches!(value.raw(), OvbRaw::Bool(true)) {
                    return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
                }
                continue;
            }
            let (kind, binding, predicate) = table_assertion_predicate(&assertion.expression)?;
            let mut projections = BTreeSet::new();
            let mut candidate_rows = 0usize;
            for (_, row) in activation
                .candidate_relation(table)
                .map_err(|error| transaction_error(table_error_code(error)))?
            {
                debit_host_step(budget)?;
                candidate_rows = candidate_rows
                    .checked_add(1)
                    .ok_or_else(|| transaction_error("ORNA-EVAL-LIMIT"))?;
                limits
                    .check_items(candidate_rows)
                    .map_err(|_| transaction_error("ORNA-EVAL-LIMIT"))?;
                let environment = Environment::from([(binding.to_owned(), row)]);
                let value = evaluate_with_functions_and_budget(
                    predicate,
                    &environment,
                    functions,
                    limits,
                    budget,
                )?;
                match kind {
                    TableAssertionKind::Every => match value.raw() {
                        OvbRaw::Bool(true) => {}
                        OvbRaw::Bool(false) => {
                            return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
                        }
                        _ => return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT")),
                    },
                    TableAssertionKind::AllUnique => {
                        let encoded = value
                            .encode()
                            .map_err(|_| transaction_error("ORNA-EVAL-TABLE-ASSERT"))?;
                        if !projections.insert(encoded) {
                            return Err(transaction_error("ORNA-EVAL-TABLE-ASSERT"));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Checks the exact module-level quantifier form already admitted by the
/// semantic checker. A quantifier names a declared table and binds one row;
/// its body may use ordinary pure expressions or another such quantifier. It
/// does not materialize a general relation value or expose query operators.
pub(super) fn validate_module_assertions(
    activation: &TransactionActivation<'_>,
    assertions: &ModuleAssertions,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
) -> Result<(), EvaluationError> {
    for assertion in assertions {
        let value = evaluate_module_assertion(
            activation,
            &assertion.expression,
            &ModuleAssertionScope::default(),
            functions,
            limits,
            budget,
            0,
        )?;
        if !matches!(value.raw(), OvbRaw::Bool(true)) {
            return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
        }
    }
    Ok(())
}

pub(super) fn module_assertion_child_depth(
    depth: usize,
    limits: EvaluatorLimits,
) -> Result<usize, EvaluationError> {
    // Section 9 specifies fold order and short-circuiting but no distinct
    // nesting bound, so use the evaluator's configured depth limit for this
    // recursive continuation walk.
    let child_depth = depth
        .checked_add(1)
        .ok_or_else(|| transaction_error("ORNA-EVAL-LIMIT"))?;
    if child_depth > limits.max_depth {
        Err(transaction_error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(child_depth)
    }
}

pub(super) fn evaluate_module_assertion(
    activation: &TransactionActivation<'_>,
    expression: &Expr,
    scope: &ModuleAssertionScope,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
    depth: usize,
) -> Result<Value, EvaluationError> {
    let Some((kind, table, predicate)) = module_assertion_quantifier(expression) else {
        return evaluate_module_assertion_continuation(
            activation, expression, scope, functions, limits, budget, depth,
        );
    };
    let table = table.to_owned();
    let relation = activation
        .candidate_relation(&table)
        .map_err(|error| transaction_error(table_error_code(error)))?;
    match kind {
        ModuleAssertionKind::Every => {
            for (_, row) in relation {
                debit_host_step(budget)?;
                let child_depth = module_assertion_child_depth(depth, limits)?;
                if !matches!(
                    evaluate_module_predicate(
                        activation,
                        &predicate,
                        row,
                        scope,
                        functions,
                        limits,
                        budget,
                        child_depth,
                    )?
                    .raw(),
                    OvbRaw::Bool(true)
                ) {
                    return canonical_bool(false);
                }
            }
            canonical_bool(true)
        }
        ModuleAssertionKind::Exists => {
            for (_, row) in relation {
                debit_host_step(budget)?;
                let child_depth = module_assertion_child_depth(depth, limits)?;
                if matches!(
                    evaluate_module_predicate(
                        activation,
                        &predicate,
                        row,
                        scope,
                        functions,
                        limits,
                        budget,
                        child_depth,
                    )?
                    .raw(),
                    OvbRaw::Bool(true)
                ) {
                    return canonical_bool(true);
                }
            }
            canonical_bool(false)
        }
    }
}

/// functions and the effect handler it runs through.
/// The inputs are one adapter call: the project unit, the retained standard
#[allow(clippy::too_many_arguments)]
pub(super) fn evaluate_module_predicate(
    activation: &TransactionActivation<'_>,
    predicate: &ModuleAssertionPredicate<'_>,
    row: Value,
    scope: &ModuleAssertionScope,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
    depth: usize,
) -> Result<Value, EvaluationError> {
    match predicate {
        ModuleAssertionPredicate::Inline { binding, body } => {
            let mut local = scope.clone();
            local.values.insert((*binding).to_owned(), row);
            local.continuations.remove(*binding);
            evaluate_module_assertion(activation, body, &local, functions, limits, budget, depth)
        }
        ModuleAssertionPredicate::Named(name) => {
            let closure = scope
                .continuations
                .get(*name)
                .ok_or_else(|| transaction_error("ORNA-EVAL-NAME"))?;
            let mut captured = closure.captured.clone();
            captured.values.insert(closure.parameter.clone(), row);
            captured.continuations.remove(&closure.parameter);
            evaluate_module_assertion(
                activation,
                &closure.body,
                &captured,
                functions,
                limits,
                budget,
                depth,
            )
        }
    }
}

pub(super) fn evaluate_module_assertion_continuation(
    activation: &TransactionActivation<'_>,
    expression: &Expr,
    scope: &ModuleAssertionScope,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
    depth: usize,
) -> Result<Value, EvaluationError> {
    match expression {
        Expr::Group { inner, .. } => evaluate_module_assertion(
            activation,
            inner,
            scope,
            functions,
            limits,
            budget,
            module_assertion_child_depth(depth, limits)?,
        ),
        Expr::Block {
            statements, tail, ..
        } => {
            let mut local = scope.clone();
            for statement in statements {
                match statement {
                    Statement::Let {
                        pattern,
                        value:
                            Expr::Lambda {
                                parameters, body, ..
                            },
                        ..
                    } => {
                        let [parameter] = parameters.as_slice() else {
                            return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
                        };
                        let Pattern::Name(parameter, _) = &parameter.pattern else {
                            return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
                        };
                        let Pattern::Name(name, _) = pattern else {
                            return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
                        };
                        local.values.remove(name);
                        local.continuations.insert(
                            name.clone(),
                            Arc::new(ModuleAssertionClosure {
                                parameter: parameter.clone(),
                                body: body.as_ref().clone(),
                                captured: local.clone(),
                            }),
                        );
                    }
                    Statement::Let { pattern, value, .. } => {
                        let value = evaluate_with_functions_and_budget(
                            value,
                            &local.values,
                            functions,
                            limits,
                            budget,
                        )?;
                        let Pattern::Name(name, _) = pattern else {
                            return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
                        };
                        local.continuations.remove(name);
                        local.values.insert(name.clone(), value);
                    }
                    Statement::Assert { value, .. } => {
                        let value = evaluate_module_assertion(
                            activation,
                            value,
                            &local,
                            functions,
                            limits,
                            budget,
                            module_assertion_child_depth(depth, limits)?,
                        )?;
                        if !matches!(value.raw(), OvbRaw::Bool(true)) {
                            return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
                        }
                    }
                    Statement::Expression { value, .. } | Statement::Control { value, .. } => {
                        let _ = evaluate_module_assertion(
                            activation,
                            value,
                            &local,
                            functions,
                            limits,
                            budget,
                            module_assertion_child_depth(depth, limits)?,
                        )?;
                    }
                    Statement::Return { .. }
                    | Statement::Break { .. }
                    | Statement::Continue { .. }
                    | Statement::Assignment { .. } => {
                        return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
                    }
                }
            }
            if let Some(tail) = tail {
                evaluate_module_assertion(
                    activation,
                    tail,
                    &local,
                    functions,
                    limits,
                    budget,
                    module_assertion_child_depth(depth, limits)?,
                )
            } else {
                canonical_bool(true)
            }
        }
        Expr::Unary { op, rhs, .. } if op == "!" => {
            let value = evaluate_module_assertion(
                activation,
                rhs,
                scope,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )?;
            match value.raw() {
                OvbRaw::Bool(value) => canonical_bool(!value),
                _ => Err(transaction_error("ORNA-EVAL-MODULE-ASSERT")),
            }
        }
        Expr::Binary { lhs, op, rhs, .. } if op == "&&" || op == "||" => {
            let left = evaluate_module_assertion(
                activation,
                lhs,
                scope,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )?;
            let OvbRaw::Bool(left) = left.raw() else {
                return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
            };
            if (op == "&&" && !left) || (op == "||" && *left) {
                return canonical_bool(*left);
            }
            let right = evaluate_module_assertion(
                activation,
                rhs,
                scope,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )?;
            match right.raw() {
                OvbRaw::Bool(right) => canonical_bool(*right),
                _ => Err(transaction_error("ORNA-EVAL-MODULE-ASSERT")),
            }
        }
        Expr::Control {
            kind: orna_syntax_ast::ControlKind::If,
            binding: None,
            condition: Some(condition),
            body: Some(body),
            arms,
            alternate: Some(alternate),
            ..
        } if arms.is_empty() => {
            let child_depth = module_assertion_child_depth(depth, limits)?;
            let condition = evaluate_module_assertion(
                activation,
                condition,
                scope,
                functions,
                limits,
                budget,
                child_depth,
            )?;
            match condition.raw() {
                OvbRaw::Bool(true) => evaluate_module_assertion(
                    activation,
                    body,
                    scope,
                    functions,
                    limits,
                    budget,
                    child_depth,
                ),
                OvbRaw::Bool(false) => evaluate_module_assertion(
                    activation,
                    alternate,
                    scope,
                    functions,
                    limits,
                    budget,
                    child_depth,
                ),
                _ => Err(transaction_error("ORNA-EVAL-MODULE-ASSERT")),
            }
        }
        Expr::Call {
            callee, arguments, ..
        } if let Expr::Name { text, .. } = callee.as_ref()
            && let Some(closure) = scope.continuations.get(text) =>
        {
            let [argument] = arguments.as_slice() else {
                return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
            };
            let value = evaluate_with_functions_and_budget(
                &argument.value,
                &scope.values,
                functions,
                limits,
                budget,
            )?;
            debit_host_step(budget)?;
            let mut captured = closure.captured.clone();
            captured.values.insert(closure.parameter.clone(), value);
            captured.continuations.remove(&closure.parameter);
            evaluate_module_assertion(
                activation,
                &closure.body,
                &captured,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )
        }
        _ => {
            evaluate_with_functions_and_budget(expression, &scope.values, functions, limits, budget)
        }
    }
}

pub(super) fn evaluate_module_assertion_rows(
    rows: &orna_runtime::RuntimeTableRows,
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
    depth: usize,
) -> Result<Value, EvaluationError> {
    let Some((kind, table, binding, body)) = module_assertion_inline_quantifier(expression) else {
        return evaluate_module_assertion_rows_continuation(
            rows,
            expression,
            environment,
            functions,
            limits,
            budget,
            depth,
        );
    };
    let relation = rows
        .get(table)
        .ok_or_else(|| transaction_error("ORNA-EVAL-MODULE-ASSERT"))?;
    match kind {
        ModuleAssertionKind::Every => {
            for (index, (_, encoded)) in relation.iter().enumerate() {
                debit_host_step(budget)?;
                limits
                    .check_items(index.saturating_add(1))
                    .map_err(|_| transaction_error("ORNA-EVAL-LIMIT"))?;
                let child_depth = module_assertion_child_depth(depth, limits)?;
                let row = Value::decode(encoded)
                    .map_err(|_| transaction_error("ORNA-EVAL-MODULE-ASSERT"))?;
                let mut local = environment.clone();
                local.insert(binding.to_owned(), row);
                if !matches!(
                    evaluate_module_assertion_rows(
                        rows,
                        body,
                        &local,
                        functions,
                        limits,
                        budget,
                        child_depth,
                    )?
                    .raw(),
                    OvbRaw::Bool(true)
                ) {
                    return canonical_bool(false);
                }
            }
            canonical_bool(true)
        }
        ModuleAssertionKind::Exists => {
            for (index, (_, encoded)) in relation.iter().enumerate() {
                debit_host_step(budget)?;
                limits
                    .check_items(index.saturating_add(1))
                    .map_err(|_| transaction_error("ORNA-EVAL-LIMIT"))?;
                let child_depth = module_assertion_child_depth(depth, limits)?;
                let row = Value::decode(encoded)
                    .map_err(|_| transaction_error("ORNA-EVAL-MODULE-ASSERT"))?;
                let mut local = environment.clone();
                local.insert(binding.to_owned(), row);
                if matches!(
                    evaluate_module_assertion_rows(
                        rows,
                        body,
                        &local,
                        functions,
                        limits,
                        budget,
                        child_depth,
                    )?
                    .raw(),
                    OvbRaw::Bool(true)
                ) {
                    return canonical_bool(true);
                }
            }
            canonical_bool(false)
        }
    }
}

pub(super) fn evaluate_module_assertion_rows_continuation(
    rows: &orna_runtime::RuntimeTableRows,
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    limits: EvaluatorLimits,
    budget: &mut StepBudget,
    depth: usize,
) -> Result<Value, EvaluationError> {
    match expression {
        Expr::Group { inner, .. } => evaluate_module_assertion_rows(
            rows,
            inner,
            environment,
            functions,
            limits,
            budget,
            module_assertion_child_depth(depth, limits)?,
        ),
        Expr::Block {
            statements,
            tail: Some(tail),
            ..
        } if statements.is_empty() => evaluate_module_assertion_rows(
            rows,
            tail,
            environment,
            functions,
            limits,
            budget,
            module_assertion_child_depth(depth, limits)?,
        ),
        Expr::Unary { op, rhs, .. } if op == "!" => {
            let value = evaluate_module_assertion_rows(
                rows,
                rhs,
                environment,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )?;
            match value.raw() {
                OvbRaw::Bool(value) => canonical_bool(!value),
                _ => Err(transaction_error("ORNA-EVAL-MODULE-ASSERT")),
            }
        }
        Expr::Binary { lhs, op, rhs, .. } if op == "&&" || op == "||" => {
            let left = evaluate_module_assertion_rows(
                rows,
                lhs,
                environment,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )?;
            let OvbRaw::Bool(left) = left.raw() else {
                return Err(transaction_error("ORNA-EVAL-MODULE-ASSERT"));
            };
            if (op == "&&" && !left) || (op == "||" && *left) {
                return canonical_bool(*left);
            }
            let right = evaluate_module_assertion_rows(
                rows,
                rhs,
                environment,
                functions,
                limits,
                budget,
                module_assertion_child_depth(depth, limits)?,
            )?;
            match right.raw() {
                OvbRaw::Bool(right) => canonical_bool(*right),
                _ => Err(transaction_error("ORNA-EVAL-MODULE-ASSERT")),
            }
        }
        Expr::Control {
            kind: orna_syntax_ast::ControlKind::If,
            binding: None,
            condition: Some(condition),
            body: Some(body),
            arms,
            alternate: Some(alternate),
            ..
        } if arms.is_empty() => {
            let child_depth = module_assertion_child_depth(depth, limits)?;
            let condition = evaluate_module_assertion_rows(
                rows,
                condition,
                environment,
                functions,
                limits,
                budget,
                child_depth,
            )?;
            match condition.raw() {
                OvbRaw::Bool(true) => evaluate_module_assertion_rows(
                    rows,
                    body,
                    environment,
                    functions,
                    limits,
                    budget,
                    child_depth,
                ),
                OvbRaw::Bool(false) => evaluate_module_assertion_rows(
                    rows,
                    alternate,
                    environment,
                    functions,
                    limits,
                    budget,
                    child_depth,
                ),
                _ => Err(transaction_error("ORNA-EVAL-MODULE-ASSERT")),
            }
        }
        _ => evaluate_with_functions_and_budget(expression, environment, functions, limits, budget),
    }
}
