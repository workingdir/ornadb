use super::*;

#[derive(Clone, Debug)]
struct ControlFlowReturnValue {
    value: RuntimeValue,
    stream: bool,
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
pub(super) fn evaluate_control_flow_plan(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    expected: ResolvedType,
    stream_result: bool,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<RuntimeValue, ClientExecutionError> {
    let returned = evaluate_control_flow_block(
        active,
        plan,
        plan.statements(),
        context,
        lineage,
        arguments,
        declarations,
        grants,
        state,
        depth,
        principal,
        executor,
        local_environment,
        fuel,
    )?
    .ok_or_else(|| expression_error(context, ClientExpressionError::MissingReturn))?;

    let matches = if stream_result {
        returned.stream && runtime_stream_value_matches(active, &returned.value, expected)
    } else {
        !returned.stream && runtime_value_matches(active, &returned.value, expected)
    };
    if matches {
        Ok(returned.value)
    } else {
        Err(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        ))
    }
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_control_flow_block(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    statements: &[ControlFlowStatement],
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<Option<ControlFlowReturnValue>, ClientExecutionError> {
    for statement in statements {
        fuel.consume(context)?;
        if let Some(returned) = evaluate_control_flow_statement(
            active,
            plan,
            statement,
            context,
            lineage,
            arguments,
            declarations,
            grants,
            state,
            depth,
            principal,
            executor,
            local_environment,
            fuel,
        )? {
            return Ok(Some(returned));
        }
    }
    Ok(None)
}

// Evaluator calls retain explicit context and state parameters for recursive semantics.
#[allow(clippy::too_many_arguments)]
// ClientExecutionError retains its accepted public context and source diagnostics.
#[allow(clippy::result_large_err)]
fn evaluate_control_flow_statement(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    statement: &ControlFlowStatement,
    context: ClientExecutionContext,
    lineage: ObserverLineage,
    arguments: &[(ParameterId, RuntimeValue)],
    declarations: &[capability::LocalCapabilityDeclaration],
    grants: &capability::LocalCapabilityGrantSet,
    state: &mut ClientStateStore,
    depth: usize,
    principal: PrincipalId,
    executor: &mut Option<&mut dyn ClientResourceExecutor>,
    local_environment: &mut ClientLocalEnvironment,
    fuel: &mut ClientExecutionFuel,
) -> Result<Option<ControlFlowReturnValue>, ClientExecutionError> {
    match statement {
        ControlFlowStatement::Let { local, expression }
        | ControlFlowStatement::Assignment { local, expression } => {
            let Some(declaration) = plan
                .locals()
                .iter()
                .find(|candidate| candidate.local_id() == *local)
            else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::ParameterNotBound,
                ));
            };
            if matches!(statement, ControlFlowStatement::Assignment { .. })
                && !local_environment.contains_key(local)
            {
                return Err(expression_error(
                    context,
                    ClientExpressionError::ParameterNotBound,
                ));
            }
            let binding = evaluate_procedural_local_with_fuel(
                active,
                declaration,
                expression,
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            // A validated plan has one declaration per local identity. A LET
            // inside a repeated block reinitialises that declaration each time.
            local_environment.insert(*local, binding);
            Ok(None)
        }
        ControlFlowStatement::Return(return_statement) => {
            let Some(expression) = return_statement.expression() else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ));
            };
            let stream = expression_returns_stream(active, expression, local_environment);
            let value = evaluate_expression_with_fuel(
                active,
                expression,
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            Ok(Some(ControlFlowReturnValue { value, stream }))
        }
        ControlFlowStatement::If(if_statement) => {
            for branch in if_statement.branches() {
                fuel.consume(context)?;
                let condition = evaluate_expression_with_fuel(
                    active,
                    branch.condition(),
                    context,
                    &lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )?;
                let RuntimeValue::Boolean(condition) = condition else {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::TypeMismatch,
                    ));
                };
                if condition {
                    return evaluate_control_flow_block(
                        active,
                        plan,
                        branch.statements(),
                        context,
                        lineage,
                        arguments,
                        declarations,
                        grants,
                        state,
                        depth,
                        principal,
                        executor,
                        local_environment,
                        fuel,
                    );
                }
            }
            if let Some(statements) = if_statement.else_statements() {
                evaluate_control_flow_block(
                    active,
                    plan,
                    statements,
                    context,
                    lineage,
                    arguments,
                    declarations,
                    grants,
                    state,
                    depth,
                    principal,
                    executor,
                    local_environment,
                    fuel,
                )
            } else {
                Ok(None)
            }
        }
        ControlFlowStatement::While(while_statement) => loop {
            fuel.consume(context)?;
            fuel.consume(context)?;
            let condition = evaluate_expression_with_fuel(
                active,
                while_statement.condition(),
                context,
                &lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )?;
            let RuntimeValue::Boolean(condition) = condition else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::TypeMismatch,
                ));
            };
            if !condition {
                return Ok(None);
            }
            if let Some(returned) = evaluate_control_flow_block(
                active,
                plan,
                while_statement.statements(),
                context,
                lineage,
                arguments,
                declarations,
                grants,
                state,
                depth,
                principal,
                executor,
                local_environment,
                fuel,
            )? {
                return Ok(Some(returned));
            }
        },
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(super) fn validate_control_flow_plan_types(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    validate_control_flow_statements_types(active, plan, plan.statements(), context)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_control_flow_statements_types(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    statements: &[orna_artifact::client_plan::ControlFlowStatement],
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    for statement in statements {
        match statement {
            orna_artifact::client_plan::ControlFlowStatement::Let { expression, .. }
            | orna_artifact::client_plan::ControlFlowStatement::Assignment { expression, .. } => {
                validate_control_flow_expression_type(active, plan, expression, context)?;
            }
            orna_artifact::client_plan::ControlFlowStatement::Return(return_statement) => {
                if let Some(expression) = return_statement.expression() {
                    validate_control_flow_expression_type(active, plan, expression, context)?;
                }
            }
            orna_artifact::client_plan::ControlFlowStatement::If(if_statement) => {
                for branch in if_statement.branches() {
                    if validate_control_flow_expression_type(
                        active,
                        plan,
                        branch.condition(),
                        context,
                    )? != Some(StandardScalar::Boolean)
                    {
                        return Err(expression_error(
                            context,
                            ClientExpressionError::TypeMismatch,
                        ));
                    }
                    validate_control_flow_statements_types(
                        active,
                        plan,
                        branch.statements(),
                        context,
                    )?;
                }
                if let Some(statements) = if_statement.else_statements() {
                    validate_control_flow_statements_types(active, plan, statements, context)?;
                }
            }
            orna_artifact::client_plan::ControlFlowStatement::While(while_statement) => {
                if validate_control_flow_expression_type(
                    active,
                    plan,
                    while_statement.condition(),
                    context,
                )? != Some(StandardScalar::Boolean)
                {
                    return Err(expression_error(
                        context,
                        ClientExpressionError::TypeMismatch,
                    ));
                }
                validate_control_flow_statements_types(
                    active,
                    plan,
                    while_statement.statements(),
                    context,
                )?;
            }
        }
    }
    Ok(())
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
fn validate_control_flow_expression_type(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
) -> Result<Option<StandardScalar>, ClientExecutionError> {
    let mismatch = || expression_error(context, ClientExpressionError::TypeMismatch);
    match expression {
        ClientExpressionNode::String { .. } => Ok(Some(StandardScalar::CharacterLargeObject)),
        ClientExpressionNode::Integer { value } => {
            i32::try_from(*value).map_err(|_| mismatch())?;
            Ok(Some(StandardScalar::Integer))
        }
        ClientExpressionNode::Boolean { .. } => Ok(Some(StandardScalar::Boolean)),
        ClientExpressionNode::ParameterRead { parameter } => {
            Ok(resolve_client_function(active, context.function())
                .and_then(|resolved| resolved.definition.parameter_by_id(*parameter))
                .and_then(|parameter| {
                    static_control_flow_scalar_for_type(active, parameter.resolved_type())
                }))
        }
        ClientExpressionNode::LocalRead { local } => {
            let Some(declaration) = plan
                .locals()
                .iter()
                .find(|candidate| candidate.local() == *local)
            else {
                return Err(mismatch());
            };
            if declaration.kind() == ClientLocalKind::Value {
                let Some(resolved) = resolve_client_local_type(active, declaration.type_id())
                else {
                    return Err(mismatch());
                };
                Ok(static_control_flow_scalar_for_type(active, resolved))
            } else {
                Ok(None)
            }
        }
        ClientExpressionNode::FieldPath { root, fields } => {
            let Some(mut resolved) = resolve_client_function(active, context.function())
                .and_then(|function| function.definition.parameter_by_id(*root))
                .map(|parameter| parameter.resolved_type())
            else {
                return Ok(None);
            };
            for field in fields {
                let Some(target) = resolved.reference_target() else {
                    return Ok(None);
                };
                let Some(definition) = active.catalogue().object_type_by_id(target).or_else(|| {
                    active
                        .catalogue_hash_context()
                        .standard()
                        .and_then(|standard| standard.catalogue().object_type_by_id(target))
                }) else {
                    return Ok(None);
                };
                let Some(field) = definition.field_by_id(*field) else {
                    return Ok(None);
                };
                resolved = field.resolved_type();
            }
            Ok(static_control_flow_scalar_for_type(active, resolved))
        }
        ClientExpressionNode::Concat { left, right } => {
            let left = validate_control_flow_expression_type(active, plan, left, context)?;
            let right = validate_control_flow_expression_type(active, plan, right, context)?;
            if left != Some(StandardScalar::CharacterLargeObject)
                || right != Some(StandardScalar::CharacterLargeObject)
            {
                return Err(mismatch());
            }
            Ok(Some(StandardScalar::CharacterLargeObject))
        }
        ClientExpressionNode::Unary {
            operator,
            expression,
        } => {
            let operand = validate_control_flow_expression_type(active, plan, expression, context)?;
            let expected = match operator {
                ControlFlowUnaryOperator::Plus | ControlFlowUnaryOperator::Minus => {
                    StandardScalar::Integer
                }
                ControlFlowUnaryOperator::Not => StandardScalar::Boolean,
            };
            if operand != Some(expected) {
                return Err(mismatch());
            }
            Ok(Some(expected))
        }
        ClientExpressionNode::Binary {
            operator,
            left,
            right,
        } => {
            let left = validate_control_flow_expression_type(active, plan, left, context)?;
            let right = validate_control_flow_expression_type(active, plan, right, context)?;
            match operator {
                ControlFlowBinaryOperator::And | ControlFlowBinaryOperator::Or => {
                    if left != Some(StandardScalar::Boolean)
                        || right != Some(StandardScalar::Boolean)
                    {
                        return Err(mismatch());
                    }
                    Ok(Some(StandardScalar::Boolean))
                }
                ControlFlowBinaryOperator::Add
                | ControlFlowBinaryOperator::Subtract
                | ControlFlowBinaryOperator::Multiply
                | ControlFlowBinaryOperator::Divide
                | ControlFlowBinaryOperator::Modulo => {
                    if left != Some(StandardScalar::Integer)
                        || right != Some(StandardScalar::Integer)
                    {
                        return Err(mismatch());
                    }
                    Ok(Some(StandardScalar::Integer))
                }
                ControlFlowBinaryOperator::Equal
                | ControlFlowBinaryOperator::NotEqual
                | ControlFlowBinaryOperator::LessThan
                | ControlFlowBinaryOperator::GreaterThan
                | ControlFlowBinaryOperator::LessThanOrEqual
                | ControlFlowBinaryOperator::GreaterThanOrEqual => {
                    let supported = |scalar| {
                        matches!(
                            scalar,
                            Some(
                                StandardScalar::Integer
                                    | StandardScalar::Boolean
                                    | StandardScalar::CharacterLargeObject
                            )
                        )
                    };
                    if !supported(left) || left != right {
                        return Err(mismatch());
                    }
                    Ok(Some(StandardScalar::Boolean))
                }
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            for (_, argument) in arguments {
                validate_control_flow_expression_type(active, plan, argument, context)?;
            }
            Ok(
                resolve_client_function(active, *function).and_then(|resolved| {
                    let FunctionReturn::Single(return_type) = resolved.definition.return_type()
                    else {
                        return None;
                    };
                    static_control_flow_scalar_for_type(active, *return_type)
                }),
            )
        }
        ClientExpressionNode::Await { expression } => {
            validate_control_flow_expression_type(active, plan, expression, context)?;
            let type_id = match expression.as_ref() {
                ClientExpressionNode::Resource { operation } => operation.declared_result_type(),
                ClientExpressionNode::LocalRead { local } => {
                    let Some(declaration) = plan
                        .locals()
                        .iter()
                        .find(|candidate| candidate.local() == *local)
                    else {
                        return Err(mismatch());
                    };
                    if !matches!(declaration.kind(), ClientLocalKind::Resource(_)) {
                        return Err(mismatch());
                    }
                    declaration.type_id()
                }
                _ => return Err(mismatch()),
            };
            Ok(static_control_flow_scalar_for_type_id(active, type_id))
        }
        ClientExpressionNode::Resource { operation } => {
            for (_, argument) in operation.arguments() {
                validate_control_flow_expression_type(active, plan, argument, context)?;
            }
            Ok(None)
        }
        ClientExpressionNode::Action { operation } => {
            for (_, argument) in operation.arguments() {
                validate_control_flow_expression_type(active, plan, argument, context)?;
            }
            Ok(static_control_flow_scalar_for_type_id(
                active,
                operation.declared_result_type(),
            ))
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(target) = operation.target() {
                validate_control_flow_expression_type(active, plan, target, context)?;
            }
            if let Some(options) = operation.options() {
                validate_control_flow_expression_type(active, plan, options, context)?;
            }
            if let Some(snapshot) = operation.snapshot_expression() {
                validate_control_flow_expression_type(active, plan, snapshot, context)?;
            }
            Ok(None)
        }
        ClientExpressionNode::SourceIntrospection
        | ClientExpressionNode::Input
        | ClientExpressionNode::Evaluate { .. } => Ok(None),
        ClientExpressionNode::ExternalContract { .. } => Ok(None),
    }
}

fn static_control_flow_scalar_for_type_id(
    active: &ActiveDatabaseRevision,
    type_id: TypeId,
) -> Option<StandardScalar> {
    resolve_client_local_type(active, type_id)
        .and_then(|resolved| static_control_flow_scalar_for_type(active, resolved))
}

fn static_control_flow_scalar_for_type(
    active: &ActiveDatabaseRevision,
    resolved: ResolvedType,
) -> Option<StandardScalar> {
    match ClientResourceValueKind::from_active(active, resolved) {
        ClientResourceValueKind::Scalar(scalar) => Some(scalar),
        _ => None,
    }
}
