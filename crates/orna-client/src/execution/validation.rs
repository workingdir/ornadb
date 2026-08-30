use super::*;

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(super) fn validate_active_catalogue(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> Result<(), ClientExecutionError> {
    let canonical = catalogue_digest_with_context(
        active.catalogue_hash_context(),
        active.catalogue(),
        active.function_revisions(),
        active.expressions(),
        active.origins(),
        active.references(),
    )
    .map_err(|source| invalid_active_revision(active.pair(), function, source))?;
    if canonical != active.catalogue_hash() {
        return Err(ClientExecutionError::InvalidActiveRevision {
            pair: active.pair(),
            function,
            source: ClientActiveRevisionError::CatalogueHashMismatch,
        });
    }
    Ok(())
}

fn invalid_active_revision(
    pair: RevisionPair,
    function: FunctionId,
    source: CanonicalHashError,
) -> ClientExecutionError {
    ClientExecutionError::InvalidActiveRevision {
        pair,
        function,
        source: ClientActiveRevisionError::Canonical(source),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClientReturnShape {
    LegacyBoolean,
    StandardBoolean(TypeId),
    Opaque(TypeId),
    Expression(ResolvedType),
    StreamExpression(ResolvedType),
    State(ResolvedType),
    StreamState(ResolvedType),
    Resource(ResolvedType),
    StreamResource(ResolvedType),
    Procedural(ResolvedType),
    StreamProcedural(ResolvedType),
    ControlFlow(ResolvedType),
    StreamControlFlow(ResolvedType),
    Action(TypeId),
    Inspect(ResolvedType),
    Source(ResolvedType),
    OtherValue,
    Unsupported,
}

fn classify_client_return(
    active: &ActiveDatabaseRevision,
    return_type: &FunctionReturn,
    artifact_version: u32,
) -> ClientReturnShape {
    let expression_eligible = matches!(
        artifact_version,
        EXPRESSION_FORMAT_VERSION
            | STATE_FORMAT_VERSION
            | RESOURCE_FORMAT_VERSION
            | PROCEDURAL_FORMAT_VERSION
            | orna_artifact::client_plan::ACTION_FORMAT_VERSION
            | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
            | orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
    );
    let stream_expression_eligible = artifact_version == EXPRESSION_FORMAT_VERSION;
    let expression_shape = |resolved_type: ResolvedType| {
        if artifact_version == STATE_FORMAT_VERSION {
            ClientReturnShape::State(resolved_type)
        } else if artifact_version == RESOURCE_FORMAT_VERSION {
            ClientReturnShape::Resource(resolved_type)
        } else if artifact_version == PROCEDURAL_FORMAT_VERSION {
            ClientReturnShape::Procedural(resolved_type)
        } else if artifact_version == orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION {
            ClientReturnShape::ControlFlow(resolved_type)
        } else if artifact_version == orna_artifact::client_plan::INSPECT_FORMAT_VERSION {
            ClientReturnShape::Inspect(resolved_type)
        } else {
            ClientReturnShape::Expression(resolved_type)
        }
    };
    let resolved_type = match return_type {
        FunctionReturn::Single(resolved_type) => *resolved_type,
        FunctionReturn::Stream(resolved_type) if stream_expression_eligible => {
            return ClientReturnShape::StreamExpression(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type) if artifact_version == STATE_FORMAT_VERSION => {
            return ClientReturnShape::StreamState(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type) if artifact_version == RESOURCE_FORMAT_VERSION => {
            return ClientReturnShape::StreamResource(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type) if artifact_version == PROCEDURAL_FORMAT_VERSION => {
            return ClientReturnShape::StreamProcedural(*resolved_type);
        }
        FunctionReturn::Stream(resolved_type)
            if artifact_version == orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION =>
        {
            return ClientReturnShape::StreamControlFlow(*resolved_type);
        }
        FunctionReturn::Rows(_) | FunctionReturn::Stream(_) => {
            return ClientReturnShape::Unsupported;
        }
    };
    if let Some(scalar) = resolved_type.legacy_scalar() {
        return if scalar == StandardScalar::Boolean {
            if expression_eligible {
                expression_shape(resolved_type)
            } else {
                ClientReturnShape::LegacyBoolean
            }
        } else if expression_eligible
            && matches!(
                scalar,
                StandardScalar::Integer | StandardScalar::CharacterLargeObject
            )
        {
            expression_shape(resolved_type)
        } else {
            ClientReturnShape::Unsupported
        };
    }
    if resolved_type.reference_target().is_some() {
        return ClientReturnShape::Unsupported;
    }
    if resolved_type.named_type() == Some(SYS_SOURCE_FUNCTION_TYPE_ID) {
        return if matches!(
            artifact_version,
            EXPRESSION_FORMAT_VERSION
                | PROCEDURAL_FORMAT_VERSION
                | orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
                | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
        ) {
            ClientReturnShape::Source(resolved_type)
        } else {
            ClientReturnShape::Unsupported
        };
    }
    if let Some(type_id) = resolved_type.value_type() {
        if artifact_version == orna_artifact::client_plan::ACTION_FORMAT_VERSION
            && type_id == STD_ACTION_TYPE_ID
        {
            return ClientReturnShape::Action(type_id);
        }
        if artifact_version == orna_artifact::client_plan::INSPECT_FORMAT_VERSION
            && is_sealed_inspect_type(type_id)
        {
            return expression_shape(resolved_type);
        }
        let Some(definition) = active
            .catalogue_hash_context()
            .standard()
            .and_then(|standard| standard.catalogue().value_type_by_id(type_id))
        else {
            return ClientReturnShape::Unsupported;
        };
        if definition.representation_contract() == "orna.kernel.value.boolean@1" {
            return if expression_eligible {
                expression_shape(resolved_type)
            } else {
                ClientReturnShape::StandardBoolean(type_id)
            };
        }
        if definition.kind() == ValueTypeKind::Opaque {
            return if expression_eligible {
                expression_shape(resolved_type)
            } else {
                ClientReturnShape::Opaque(type_id)
            };
        }
        if expression_eligible
            && matches!(
                definition.representation_contract(),
                "orna.kernel.value.integer@1" | "orna.kernel.value.character-large-object@1"
            )
        {
            return expression_shape(resolved_type);
        }
        return ClientReturnShape::OtherValue;
    }
    ClientReturnShape::Unsupported
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(crate) fn validate_function_shape(
    active: &ActiveDatabaseRevision,
    definition: &orna_core::catalogue::FunctionDefinition,
    context: ClientExecutionContext,
    artifact_version: u32,
) -> Result<ClientReturnShape, ClientExecutionError> {
    if definition.domain() != FunctionDomain::Client {
        return Err(invalid_function(
            context,
            ClientExecutionRule::FunctionDomain,
        ));
    }
    if !matches!(
        artifact_version,
        EXPRESSION_FORMAT_VERSION
            | STATE_FORMAT_VERSION
            | RESOURCE_FORMAT_VERSION
            | PROCEDURAL_FORMAT_VERSION
            | orna_artifact::client_plan::ACTION_FORMAT_VERSION
            | orna_artifact::client_plan::INSPECT_FORMAT_VERSION
            | orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
    ) && !definition.parameters().is_empty()
    {
        return Err(invalid_function(context, ClientExecutionRule::Parameters));
    }
    let return_shape = classify_client_return(active, definition.return_type(), artifact_version);
    if matches!(return_shape, ClientReturnShape::Unsupported) {
        return Err(invalid_function(context, ClientExecutionRule::ReturnType));
    }
    if definition.security() != FunctionSecurity::Invoker {
        return Err(invalid_function(context, ClientExecutionRule::Security));
    }
    if definition.volatility() != FunctionVolatility::Immutable {
        return Err(invalid_function(context, ClientExecutionRule::Volatility));
    }
    Ok(return_shape)
}

pub(super) fn is_expression_reference_allowed(
    function: Option<&orna_core::catalogue::FunctionDefinition>,
    reference: &orna_core::revision::DefinitionReference,
) -> bool {
    match reference.kind() {
        DefinitionReferenceKind::FunctionCall
        | DefinitionReferenceKind::NamedType
        | DefinitionReferenceKind::ParameterRead
        | DefinitionReferenceKind::QueryField
        | DefinitionReferenceKind::Expression => true,
        DefinitionReferenceKind::ObjectReference => {
            let DefinitionReferenceTarget::ObjectType(target) = reference.target() else {
                return false;
            };
            function.is_some_and(|definition| {
                definition
                    .parameters()
                    .iter()
                    .any(|parameter| parameter.resolved_type().reference_target() == Some(target))
            })
        }
        _ => false,
    }
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(crate) fn validate_selected_references(
    active: &ActiveDatabaseRevision,
    references: &[orna_core::revision::DefinitionReference],
    function: &FunctionDefinition,
    semantic_hash_version: FunctionSemanticHashVersion,
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
) -> Result<(), ClientExecutionError> {
    let selected = references
        .iter()
        .filter(|reference| {
            reference.source_function() == context.function()
                && reference.source_revision() == context.function_revision()
        })
        .collect::<Vec<_>>();

    match active.catalogue_hash_context() {
        orna_core::revision::CatalogueHashContext::Version1 => {
            if return_shape != ClientReturnShape::LegacyBoolean
                || semantic_hash_version != FunctionSemanticHashVersion::Version1
                || !selected.is_empty()
            {
                return Err(invalid_function(context, ClientExecutionRule::References));
            }
        }
        orna_core::revision::CatalogueHashContext::Version2 { standard } => {
            if semantic_hash_version != FunctionSemanticHashVersion::Version2 {
                return Err(invalid_function(context, ClientExecutionRule::References));
            }
            if matches!(
                return_shape,
                ClientReturnShape::Expression(_)
                    | ClientReturnShape::StreamExpression(_)
                    | ClientReturnShape::State(_)
                    | ClientReturnShape::StreamState(_)
                    | ClientReturnShape::Resource(_)
                    | ClientReturnShape::StreamResource(_)
                    | ClientReturnShape::Procedural(_)
                    | ClientReturnShape::StreamProcedural(_)
                    | ClientReturnShape::ControlFlow(_)
                    | ClientReturnShape::StreamControlFlow(_)
                    | ClientReturnShape::Action(_)
                    | ClientReturnShape::Inspect(_)
                    | ClientReturnShape::Source(_)
            ) {
                if selected
                    .iter()
                    .any(|reference| !is_expression_reference_allowed(Some(function), reference))
                {
                    return Err(invalid_function(context, ClientExecutionRule::References));
                }
                return Ok(());
            }
            let Some(reference) = selected.first() else {
                return Err(invalid_function(context, ClientExecutionRule::References));
            };
            let valid = selected.len() == 1
                && reference.ordinal() == 0
                && reference.kind() == DefinitionReferenceKind::NamedType
                && match reference.target() {
                    DefinitionReferenceTarget::ValueType(type_id) => {
                        let definition = standard.catalogue().value_type_by_id(type_id);
                        match return_shape {
                            ClientReturnShape::LegacyBoolean => definition.is_some_and(|value| {
                                value.representation_contract() == "orna.kernel.value.boolean@1"
                            }),
                            ClientReturnShape::StandardBoolean(return_type) => {
                                return_type == type_id
                                    && definition.is_some_and(|value| {
                                        value.representation_contract()
                                            == "orna.kernel.value.boolean@1"
                                    })
                            }
                            ClientReturnShape::Opaque(return_type) => {
                                return_type == type_id
                                    && definition
                                        .is_some_and(|value| value.kind() == ValueTypeKind::Opaque)
                            }
                            ClientReturnShape::Action(return_type) => {
                                return_type == type_id
                                    && type_id == STD_ACTION_TYPE_ID
                                    && definition
                                        .is_some_and(|value| value.kind() == ValueTypeKind::Opaque)
                            }
                            ClientReturnShape::Source(_) => type_id == SYS_SOURCE_FUNCTION_TYPE_ID,
                            _ => false,
                        }
                    }
                    _ => false,
                };
            if !valid {
                return Err(invalid_function(context, ClientExecutionRule::References));
            }
        }
        _ => return Err(invalid_function(context, ClientExecutionRule::References)),
    }
    Ok(())
}

/// Checks that a decoded expression call targets one of the durable
/// `FunctionCall` references recorded for its owning revision.
///
/// The artifact payload is integrity checked, but its function IDs are still
/// untrusted input at this boundary. The compiler emits one resolved
/// `FunctionCall` reference for every call node; requiring the target to be in
/// that set prevents a validly encoded artifact from invoking an unrelated
/// function that was not part of the checked call graph.
pub(super) fn client_call_target_is_referenced(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    target: FunctionId,
) -> bool {
    let Some(owner) = resolve_client_function(active, context.function()) else {
        return false;
    };
    if owner.revision.id() != context.function_revision() {
        return false;
    }
    owner.references.iter().any(|reference| {
        reference.source_function() == context.function()
            && reference.source_revision() == context.function_revision()
            && reference.kind() == DefinitionReferenceKind::FunctionCall
            && reference.target() == DefinitionReferenceTarget::Function(target)
    })
}

/// Preflights every CLIENT call in one decoded version-3 expression plan.
///
/// The compiler records call references in postorder, so nested calls precede
/// their enclosing call. Matching that sequence against the owning revision's
/// durable references closes the gap left by a target-set-only check: target
/// substitutions, reordered/duplicated/missing calls, and malformed argument
/// bindings are all rejected before any expression is evaluated.
// ClientExecutionError or action errors retain their accepted diagnostic context and variants.
#[allow(clippy::result_large_err)]
pub(crate) fn preflight_client_expression_calls(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    collect_client_expression_call_targets(active, expression, context, &mut decoded_targets)?;

    preflight_client_call_targets(active, context, decoded_targets)
}
// ClientExecutionError or action errors retain their accepted diagnostic context and variants.
#[allow(clippy::result_large_err)]
fn preflight_client_call_targets(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    decoded_targets: Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    let Some(owner) = resolve_client_function(active, context.function()) else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    if owner.revision.id() != context.function_revision() {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    let mut durable_references = owner
        .references
        .iter()
        .filter(|reference| {
            reference.source_function() == context.function()
                && reference.source_revision() == context.function_revision()
                && reference.kind() == DefinitionReferenceKind::FunctionCall
        })
        .collect::<Vec<_>>();
    durable_references.sort_unstable_by_key(|reference| reference.ordinal());

    if durable_references.len() != decoded_targets.len()
        || durable_references
            .iter()
            .zip(decoded_targets)
            .any(|(reference, target)| {
                reference.target() != DefinitionReferenceTarget::Function(target)
            })
    {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    Ok(())
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(crate) fn preflight_client_state_calls(
    active: &ActiveDatabaseRevision,
    plan: &StateClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    for slot in plan.slots() {
        if let StateDefault::Expression(expression) = slot.default() {
            collect_client_expression_call_targets(
                active,
                expression,
                context,
                &mut decoded_targets,
            )?;
        }
    }
    collect_client_expression_call_targets(
        active,
        plan.expression(),
        context,
        &mut decoded_targets,
    )?;
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(crate) fn preflight_client_procedural_calls(
    active: &ActiveDatabaseRevision,
    plan: &ProceduralClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    for statement in plan.statements() {
        collect_client_expression_call_targets(
            active,
            statement.expression(),
            context,
            &mut decoded_targets,
        )?;
    }
    collect_client_expression_call_targets(
        active,
        plan.return_expression(),
        context,
        &mut decoded_targets,
    )?;
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError or action errors retain their accepted diagnostic context and variants.

#[allow(clippy::result_large_err)]
pub(crate) fn preflight_client_control_flow_calls(
    active: &ActiveDatabaseRevision,
    plan: &ControlFlowClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let mut decoded_targets = Vec::new();
    collect_control_flow_block_call_targets(
        active,
        plan.statements(),
        context,
        &mut decoded_targets,
    )?;
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn collect_control_flow_block_call_targets(
    active: &ActiveDatabaseRevision,
    statements: &[ControlFlowStatement],
    context: ClientExecutionContext,
    decoded_targets: &mut Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    for statement in statements {
        match statement {
            ControlFlowStatement::Let { expression, .. }
            | ControlFlowStatement::Assignment { expression, .. } => {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            ControlFlowStatement::Return(return_statement) => {
                if let Some(expression) = return_statement.expression() {
                    collect_client_expression_call_targets(
                        active,
                        expression,
                        context,
                        decoded_targets,
                    )?;
                }
            }
            ControlFlowStatement::If(if_statement) => {
                for branch in if_statement.branches() {
                    collect_client_expression_call_targets(
                        active,
                        branch.condition(),
                        context,
                        decoded_targets,
                    )?;
                    collect_control_flow_block_call_targets(
                        active,
                        branch.statements(),
                        context,
                        decoded_targets,
                    )?;
                }
                if let Some(statements) = if_statement.else_statements() {
                    collect_control_flow_block_call_targets(
                        active,
                        statements,
                        context,
                        decoded_targets,
                    )?;
                }
            }
            ControlFlowStatement::While(while_statement) => {
                collect_client_expression_call_targets(
                    active,
                    while_statement.condition(),
                    context,
                    decoded_targets,
                )?;
                collect_control_flow_block_call_targets(
                    active,
                    while_statement.statements(),
                    context,
                    decoded_targets,
                )?;
            }
        }
    }
    Ok(())
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
pub(crate) fn preflight_client_action_calls(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    validate_client_action_operation(active, operation, context)?;
    let mut decoded_targets = Vec::new();
    for (_, expression) in operation.arguments() {
        collect_client_expression_call_targets(active, expression, context, &mut decoded_targets)?;
    }
    decoded_targets.push(operation.target_function());
    preflight_client_call_targets(active, context, decoded_targets)
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
pub(crate) fn preflight_client_inner_plan_calls(
    active: &ActiveDatabaseRevision,
    plan: &InnerClientPlan,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    match plan {
        InnerClientPlan::Boolean(_) | InnerClientPlan::Opaque(_) => Ok(()),
        InnerClientPlan::Expression(inner) => {
            preflight_client_expression_calls(active, inner.expression(), context)
        }
        InnerClientPlan::State(inner) => preflight_client_state_calls(active, inner, context),
        InnerClientPlan::Resource(inner) => {
            preflight_client_expression_calls(active, inner.expression(), context)
        }
        InnerClientPlan::Procedural(inner) => {
            preflight_client_procedural_calls(active, inner, context)
        }
        InnerClientPlan::ControlFlow(inner) => {
            preflight_client_control_flow_calls(active, inner, context)
        }
        InnerClientPlan::Action(inner) => {
            preflight_client_action_calls(active, inner.operation(), context)
        }
    }
}

fn operation_arguments_match_definition(
    definition: &FunctionDefinition,
    arguments: &[(ParameterId, ClientExpressionNode)],
) -> bool {
    if arguments.len() != definition.parameters().len() {
        return false;
    }
    let mut expected = definition
        .parameters()
        .iter()
        .map(|parameter| parameter.id())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    arguments
        .iter()
        .map(|(parameter, _)| *parameter)
        .eq(expected)
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn validate_client_resource_operation(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    operation: &ResourceOperationNode,
) -> Result<(), ClientExecutionError> {
    let Some(resolved) = resolve_resource_operation_target(active, operation) else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    if resolved.definition.domain() != FunctionDomain::Server
        || !operation_arguments_match_definition(resolved.definition, operation.arguments())
    {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    let expected = match (operation.kind(), resolved.definition.return_type()) {
        (ResourceKind::Scalar, FunctionReturn::Single(result)) => *result,
        (ResourceKind::Stream, FunctionReturn::Stream(result)) => *result,
        _ => {
            return Err(expression_error(
                context,
                ClientExpressionError::InvalidCall,
            ));
        }
    };
    if !resource_type_matches_id(active, expected, operation.declared_result_type()) {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    Ok(())
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn validate_client_action_operation(
    active: &ActiveDatabaseRevision,
    operation: &orna_artifact::client_plan::ActionOperationNode,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    let raw_target =
        InvocationTarget::new(operation.target_function(), operation.target_revision());
    let Some(resolved) = resolve_unclassified_target(active, raw_target) else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    let expected_domain = match operation.domain() {
        ActionTargetDomain::Client => FunctionDomain::Client,
        ActionTargetDomain::Server => FunctionDomain::Server,
    };
    if resolved.definition.domain() != expected_domain
        || !operation_arguments_match_definition(resolved.definition, operation.arguments())
    {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    let FunctionReturn::Single(expected) = resolved.definition.return_type() else {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    };
    let expected = *expected;
    if !resource_type_matches_id(active, expected, operation.declared_result_type()) {
        return Err(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        ));
    }
    Ok(())
}

// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
fn collect_client_expression_call_targets(
    active: &ActiveDatabaseRevision,
    expression: &ClientExpressionNode,
    context: ClientExecutionContext,
    decoded_targets: &mut Vec<FunctionId>,
) -> Result<(), ClientExecutionError> {
    match expression {
        ClientExpressionNode::Await { expression } => {
            collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
        }
        ClientExpressionNode::Resource { operation } => {
            validate_client_resource_operation(active, context, operation)?;
            for (_, expression) in operation.arguments() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            decoded_targets.push(operation.target_function());
        }
        ClientExpressionNode::Action { operation } => {
            validate_client_action_operation(active, operation, context)?;
            for (_, expression) in operation.arguments() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            decoded_targets.push(operation.target_function());
        }
        ClientExpressionNode::Inspect { operation } => {
            if let Some(expression) = operation.target() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            if let Some(expression) = operation.options() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            if let Some(expression) = operation.snapshot_expression() {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
        }
        ClientExpressionNode::Call {
            function,
            arguments,
        } => {
            let Some(resolved) = resolve_client_function(active, *function) else {
                return Err(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                ));
            };
            let definition = resolved.definition;
            if arguments.len() != definition.parameters().len()
                || definition.parameters().iter().any(|parameter| {
                    arguments
                        .iter()
                        .filter(|(candidate, _)| *candidate == parameter.id())
                        .count()
                        != 1
                })
                || arguments.iter().any(|(parameter, _)| {
                    definition
                        .parameters()
                        .iter()
                        .all(|candidate| candidate.id() != *parameter)
                })
            {
                return Err(expression_error(
                    context,
                    ClientExpressionError::InvalidCall,
                ));
            }
            for (_, expression) in arguments {
                collect_client_expression_call_targets(
                    active,
                    expression,
                    context,
                    decoded_targets,
                )?;
            }
            decoded_targets.push(*function);
        }
        ClientExpressionNode::Concat { left, right }
        | ClientExpressionNode::Binary { left, right, .. } => {
            collect_client_expression_call_targets(active, left, context, decoded_targets)?;
            collect_client_expression_call_targets(active, right, context, decoded_targets)?;
        }
        ClientExpressionNode::Unary { expression, .. } => {
            collect_client_expression_call_targets(active, expression, context, decoded_targets)?;
        }
        ClientExpressionNode::Input | ClientExpressionNode::Evaluate { .. } => {}
        ClientExpressionNode::String { .. }
        | ClientExpressionNode::Integer { .. }
        | ClientExpressionNode::Boolean { .. }
        | ClientExpressionNode::ParameterRead { .. }
        | ClientExpressionNode::LocalRead { .. }
        | ClientExpressionNode::FieldPath { .. }
        | ClientExpressionNode::ExternalContract { .. }
        | ClientExpressionNode::SourceIntrospection => {}
    }
    Ok(())
}

/// Validates the saved artefact contract against the effective plan version.
///
/// For a version-5 capability envelope the effective version is the inner
/// plan version (the envelope decode already fixed the outer version); for
/// versions 1-4 it is the artefact's own version.
// ClientExecutionError retains its full public execution context and source error.
#[allow(clippy::result_large_err)]
pub(crate) fn validate_artifact(
    artifact: &orna_core::revision::ExecutableArtifact,
    language_version: &str,
    context: ClientExecutionContext,
    return_shape: ClientReturnShape,
    artifact_version: u32,
) -> Result<(), ClientExecutionError> {
    if artifact.format() != FORMAT_IDENTITY {
        return Err(invalid_function(
            context,
            ClientExecutionRule::ArtifactFormat,
        ));
    }
    let expected_version = match return_shape {
        ClientReturnShape::LegacyBoolean | ClientReturnShape::StandardBoolean(_) => FORMAT_VERSION,
        ClientReturnShape::Opaque(_) => OPAQUE_FORMAT_VERSION,
        ClientReturnShape::Expression(_) | ClientReturnShape::StreamExpression(_) => {
            EXPRESSION_FORMAT_VERSION
        }
        ClientReturnShape::Procedural(_) | ClientReturnShape::StreamProcedural(_) => {
            PROCEDURAL_FORMAT_VERSION
        }
        ClientReturnShape::ControlFlow(_) | ClientReturnShape::StreamControlFlow(_) => {
            orna_artifact::client_plan::CONTROL_FLOW_FORMAT_VERSION
        }
        ClientReturnShape::State(_) | ClientReturnShape::StreamState(_) => STATE_FORMAT_VERSION,
        ClientReturnShape::Resource(_) | ClientReturnShape::StreamResource(_) => {
            RESOURCE_FORMAT_VERSION
        }
        ClientReturnShape::Action(_) => orna_artifact::client_plan::ACTION_FORMAT_VERSION,
        ClientReturnShape::Inspect(_) => orna_artifact::client_plan::INSPECT_FORMAT_VERSION,
        ClientReturnShape::Source(_) => artifact_version,
        ClientReturnShape::OtherValue => unreachable!("definition references were validated"),
        ClientReturnShape::Unsupported => unreachable!("function shape was validated"),
    };
    if artifact_version != expected_version {
        return Err(invalid_function(
            context,
            ClientExecutionRule::ArtifactVersion,
        ));
    }
    if language_version != LANGUAGE_VERSION_IDENTITY {
        return Err(invalid_function(
            context,
            ClientExecutionRule::LanguageVersion,
        ));
    }
    Ok(())
}

/// Validates a CLIENT artifact's execution domain and canonical payload digest.
///
/// This check runs before plan decoding or evaluation. It proves payload
/// integrity only; provenance, signatures, sandbox policy, and host
/// capabilities remain separate contract surfaces.
pub fn validate_client_artifact_integrity(
    artifact: &orna_core::revision::ExecutableArtifact,
) -> Result<(), ClientArtifactIntegrityError> {
    if artifact.kind() != ExecutableArtifactKind::Client {
        return Err(ClientArtifactIntegrityError::WrongExecutionDomain);
    }
    let digest = artifact_payload_digest(artifact.payload())
        .map_err(|_| ClientArtifactIntegrityError::PayloadDigest)?;
    if digest != artifact.content_hash() {
        return Err(ClientArtifactIntegrityError::PayloadDigest);
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
pub(super) fn validate_artifact_identity(
    artifact: &orna_core::revision::ExecutableArtifact,
    context: ClientExecutionContext,
) -> Result<(), ClientExecutionError> {
    validate_client_artifact_integrity(artifact).map_err(|_| invalid_artifact(context))
}

fn invalid_artifact(context: ClientExecutionContext) -> ClientExecutionError {
    ClientExecutionError::InvalidArtifact {
        context,
        source: ClientPlanError::InvalidMagic,
    }
}

fn invalid_function(
    context: ClientExecutionContext,
    rule: ClientExecutionRule,
) -> ClientExecutionError {
    ClientExecutionError::InvalidFunction { context, rule }
}
