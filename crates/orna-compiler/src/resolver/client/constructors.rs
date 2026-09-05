use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn check_resource_constructor(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<(CheckedClientExpression, ClientExpressionType)> {
    if let ClientExpression::LocalRead { local } = expression {
        let name = semantic_part(local);
        let Some(binding) = locals.get(&name) else {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("unknown CLIENT local {name}"),
                input.logical_path,
                &local.span,
            ));
            return None;
        };
        if !matches!(binding.kind, CheckedClientLocalKind::Resource(..)) {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                format!("CLIENT local {name} is not a resource"),
                input.logical_path,
                &local.span,
            ));
            return None;
        }
        return Some((
            binding.ordinal.map_or_else(
                || binding.checked.clone(),
                |ordinal| CheckedClientExpression::LocalRead {
                    local: ordinal,
                    location: location(input.logical_path, &local.span),
                },
            ),
            binding.expression_type,
        ));
    }
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::DomainIncompatible,
            "AWAIT requires a std.data.resource or std.data.stream_resource constructor",
            input.logical_path,
            expression.span(),
        ));
        return None;
    };
    let constructor_name = semantic_name(callee);
    let Some(kind) = resource_constructor_kind(&constructor_name) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::DomainIncompatible,
            "AWAIT operand must be a resource constructor",
            input.logical_path,
            span,
        ));
        return None;
    };
    if arguments.len() != 2 {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource constructor requires exactly one target and one arguments value",
            input.logical_path,
            span,
        ));
        return None;
    }
    let mut target_expression = None;
    let mut arguments_expression = None;
    for argument in arguments {
        let Some(name) = &argument.name else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "resource constructor arguments must be named target and arguments",
                input.logical_path,
                &argument.span,
            ));
            return None;
        };
        match semantic_part(name).as_str() {
            "target" if target_expression.is_none() => target_expression = Some(&argument.value),
            "arguments" if arguments_expression.is_none() => {
                arguments_expression = Some(&argument.value)
            }
            "target" | "arguments" => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    "duplicate resource constructor argument",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            _ => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    "resource constructor accepts only target and arguments",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
        }
    }
    let (Some(target_expression), Some(arguments_expression)) =
        (target_expression, arguments_expression)
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource constructor requires both target and arguments",
            input.logical_path,
            span,
        ));
        return None;
    };
    let ClientExpression::FieldPath { root, members, .. } = target_expression else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource target must be a qualified SERVER function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    if members.is_empty() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource target must include a schema and function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let mut target_parts = Vec::with_capacity(members.len() + 1);
    target_parts.push(semantic_part(root));
    target_parts.extend(members.iter().map(semantic_part));
    let Ok(target_name) = QualifiedSemanticName::new(target_parts) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            "resource target must be a qualified SERVER function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    let Some(target) = resource_targets.get(&target_name) else {
        let message = if targets.contains_key(&target_name)
            || base
                .function_by_name(&target_name)
                .is_some_and(|function| function.domain() == FunctionDomain::Client)
        {
            format!("resource target {target_name} must be a SERVER function")
        } else {
            format!("unknown SERVER resource target {target_name}")
        };
        diagnostics.push(diagnostic(
            if targets.contains_key(&target_name)
                || base
                    .function_by_name(&target_name)
                    .is_some_and(|function| function.domain() == FunctionDomain::Client)
            {
                DiagnosticCode::DomainIncompatible
            } else {
                DiagnosticCode::UnknownQualifiedName
            },
            message,
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    if target.kind != kind {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("resource constructor kind does not match SERVER target {target_name}"),
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let ClientExpression::Call {
        callee: args_callee,
        arguments: target_arguments,
        ..
    } = arguments_expression
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    };
    if semantic_name(args_callee)
        != QualifiedSemanticName::new(["std", "call", "args"]).expect("std.call.args is valid")
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "resource arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    }
    let mut bound = vec![false; target.parameters.len()];
    let mut positional = 0usize;
    let mut checked_arguments = Vec::with_capacity(target_arguments.len());
    for argument in target_arguments {
        let parameter_index = if let Some(name) = &argument.name {
            let parameter_name = semantic_part(name);
            let Some(index) = target
                .parameters
                .iter()
                .position(|parameter| parameter.name == parameter_name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown SERVER resource parameter {parameter_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            };
            index
        } else {
            while positional < bound.len() && bound[positional] {
                positional += 1;
            }
            if positional >= bound.len() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("too many arguments for SERVER resource target {target_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            let index = positional;
            positional += 1;
            index
        };
        if bound[parameter_index] {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate SERVER resource parameter {}",
                    target.parameters[parameter_index].name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        let (checked, expression_type) = check_client_expression(
            &argument.value,
            input,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            references,
            used_capabilities,
            locals,
        )?;
        let parameter = &target.parameters[parameter_index];
        if !client_expression_types_compatible(expression_type, parameter.expression_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "resource argument does not match SERVER parameter {}",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        bound[parameter_index] = true;
        checked_arguments.push((parameter.id, checked));
    }
    if bound.iter().any(|bound| !bound) {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("missing argument for SERVER resource target {target_name}"),
            input.logical_path,
            span,
        ));
        return None;
    }
    checked_arguments.sort_by_key(|(parameter, _)| *parameter);
    let operation_location = location(input.logical_path, span);
    let call_site = client_resource_call_site_id(&operation_location, &input.name);
    references.push(CheckedDefinitionReference {
        target: CheckedDefinitionReferenceTarget::Function(target.id),
        kind: DefinitionReferenceKind::FunctionCall,
        location: operation_location.clone(),
    });
    let operation = CheckedResourceOperation {
        kind,
        target: target.id,
        call_site,
        arguments: checked_arguments,
        result_type: target.result_type.semantic_type,
        standard_result_type: target.result_type.standard_value_type,
        location: operation_location,
    };
    Some((
        CheckedClientExpression::Resource { operation },
        target.result_type,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_action_constructor(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<(CheckedClientExpression, ClientExpressionType)> {
    let action_name =
        QualifiedSemanticName::new(["std", "action", "call"]).expect("std.action.call is valid");
    let Some(action_type) = standard
        .and_then(|standard| {
            standard.value_types().iter().find(|value| {
                value.id() == STD_ACTION_TYPE_ID
                    && value.kind() == ValueTypeKind::Opaque
                    && value.representation_contract() == STD_ACTION_CONTRACT
            })
        })
        .map(|_| ClientExpressionType {
            semantic_type: SemanticType::Named(CheckedTypeId::Existing(STD_ACTION_TYPE_ID)),
            standard_value_type: Some(STD_ACTION_TYPE_ID),
            result_shape: ClientExpressionResultShape::Value,
        })
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "the checked standard library does not provide std.action.Action",
            input.logical_path,
            expression.span(),
        ));
        return None;
    };
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        return None;
    };
    if semantic_name(callee) != action_name {
        return None;
    }
    if arguments.len() != 2 {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call requires exactly one target and one arguments value",
            input.logical_path,
            span,
        ));
        return None;
    }
    let mut target_expression = None;
    let mut arguments_expression = None;
    for argument in arguments {
        let Some(name) = &argument.name else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "std.action.call arguments must be named target and arguments",
                input.logical_path,
                &argument.span,
            ));
            return None;
        };
        match semantic_part(name).as_str() {
            "target" if target_expression.is_none() => {
                target_expression = Some(&argument.value);
            }
            "arguments" if arguments_expression.is_none() => {
                arguments_expression = Some(&argument.value);
            }
            "target" | "arguments" => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    "duplicate std.action.call argument",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            _ => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    "std.action.call accepts only target and arguments",
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
        }
    }
    let (Some(target_expression), Some(arguments_expression)) =
        (target_expression, arguments_expression)
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call requires both target and arguments",
            input.logical_path,
            span,
        ));
        return None;
    };
    let ClientExpression::FieldPath { root, members, .. } = target_expression else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call target must be a qualified function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    if members.is_empty() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call target must include a schema and function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let mut target_parts = Vec::with_capacity(members.len() + 1);
    target_parts.push(semantic_part(root));
    target_parts.extend(members.iter().map(semantic_part));
    let Ok(target_name) = QualifiedSemanticName::new(target_parts) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            "std.action.call target must be a qualified function name",
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    let Some(target) = action_targets.get(&target_name) else {
        let message = if server_names.contains(&target_name)
            || base.function_by_name(&target_name).is_some()
        {
            format!("std.action.call target {target_name} does not return one durable value")
        } else {
            format!("unknown std.action.call target {target_name}")
        };
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            message,
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    };
    let ClientExpression::Call {
        callee: args_callee,
        arguments: target_arguments,
        ..
    } = arguments_expression
    else {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    };
    if semantic_name(args_callee)
        != QualifiedSemanticName::new(["std", "call", "args"]).expect("std.call.args is valid")
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "std.action.call arguments must be a std.call.args value",
            input.logical_path,
            arguments_expression.span(),
        ));
        return None;
    }
    if target.parameters.iter().any(|parameter| {
        !action_argument_type_is_orv3_encodable(parameter.expression_type, base, standard)
    }) {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!(
                "std.action.call target {target_name} has a parameter that is not ORV3-encodable"
            ),
            input.logical_path,
            target_expression.span(),
        ));
        return None;
    }
    let mut bound = vec![false; target.parameters.len()];
    let mut positional = 0usize;
    let mut checked_arguments = Vec::with_capacity(target_arguments.len());
    for argument in target_arguments {
        let parameter_index = if let Some(name) = &argument.name {
            let parameter_name = semantic_part(name);
            let Some(index) = target
                .parameters
                .iter()
                .position(|parameter| parameter.name == parameter_name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown std.action.call parameter {parameter_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            };
            index
        } else {
            while positional < bound.len() && bound[positional] {
                positional += 1;
            }
            if positional >= bound.len() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("too many arguments for std.action.call target {target_name}"),
                    input.logical_path,
                    &argument.span,
                ));
                return None;
            }
            let index = positional;
            positional += 1;
            index
        };
        if bound[parameter_index] {
            diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate std.action.call parameter {}",
                    target.parameters[parameter_index].name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        let (checked, expression_type) = check_client_expression(
            &argument.value,
            input,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            references,
            used_capabilities,
            locals,
        )?;
        let parameter = &target.parameters[parameter_index];
        if client_expression_contains_await_or_resource(&checked, locals) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "std.action.call argument for parameter {} is not ORV3-encodable",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        if !client_expression_types_compatible(expression_type, parameter.expression_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "std.action.call argument does not match parameter {}",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        if !action_argument_type_is_orv3_encodable(expression_type, base, standard) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "std.action.call argument for parameter {} is not ORV3-encodable",
                    parameter.name
                ),
                input.logical_path,
                &argument.span,
            ));
            return None;
        }
        bound[parameter_index] = true;
        checked_arguments.push((parameter.id, checked));
    }
    if bound.iter().any(|bound| !bound) {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("missing argument for std.action.call target {target_name}"),
            input.logical_path,
            span,
        ));
        return None;
    }
    checked_arguments.sort_by_key(|(parameter, _)| *parameter);
    let operation_location = location(input.logical_path, span);
    references.push(CheckedDefinitionReference {
        target: CheckedDefinitionReferenceTarget::Function(target.id),
        kind: DefinitionReferenceKind::FunctionCall,
        location: operation_location.clone(),
    });
    let operation = CheckedActionOperation {
        target_domain: target.domain,
        target: target.id,
        call_site: client_resource_call_site_id(&operation_location, &input.name),
        arguments: checked_arguments,
        result_type: target.return_type.semantic_type,
        standard_result_type: target.return_type.standard_value_type,
        location: operation_location,
    };
    Some((CheckedClientExpression::Action { operation }, action_type))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_inspect_call(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    references: &mut Vec<CheckedDefinitionReference>,
    used_capabilities: &mut HashSet<QualifiedSemanticName>,
    locals: &ClientLocalEnvironment,
) -> Option<Option<(CheckedClientExpression, ClientExpressionType)>> {
    let ClientExpression::Call {
        callee,
        arguments,
        span,
    } = expression
    else {
        return None;
    };
    let name = semantic_name(callee);
    let system = orna_core::system::system_function_by_name(&name)?;
    let Some(signature) = system.inspect_signature() else {
        return Some(None);
    };

    let projection = match system.id() {
        orna_core::system::SYS_INSPECT_SNAPSHOT_FUNCTION_ID => None,
        orna_core::system::SYS_INSPECT_INVOCATION_NODES_FUNCTION_ID => {
            Some(CheckedInspectProjection::InvocationNodes)
        }
        orna_core::system::SYS_INSPECT_CALLS_FUNCTION_ID => Some(CheckedInspectProjection::Calls),
        orna_core::system::SYS_INSPECT_RESOURCES_FUNCTION_ID => {
            Some(CheckedInspectProjection::Resources)
        }
        orna_core::system::SYS_INSPECT_STATE_CELLS_FUNCTION_ID => {
            Some(CheckedInspectProjection::StateCells)
        }
        orna_core::system::SYS_INSPECT_UI_NODES_FUNCTION_ID => {
            Some(CheckedInspectProjection::UiNodes)
        }
        orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_FUNCTION_ID => {
            Some(CheckedInspectProjection::PresentationCandidates)
        }
        orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_FUNCTION_ID => {
            Some(CheckedInspectProjection::RuntimeBindings)
        }
        orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_FUNCTION_ID => {
            Some(CheckedInspectProjection::SecurityDecisions)
        }
        _ => {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("sealed INSPECT function {name} is not an expression operation"),
                input.logical_path,
                span,
            ));
            return Some(None);
        }
    };

    let (target_argument, options_argument) = if projection.is_none() {
        if arguments.is_empty() || arguments.len() > 2 {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "sys.inspect.snapshot requires target and optionally p_options",
                input.logical_path,
                span,
            ));
            return Some(None);
        }
        let mut bound = [false; 2];
        let mut target_argument = None;
        let mut options_argument = None;
        for (position, argument) in arguments.iter().enumerate() {
            let index = match argument.name.as_ref().map(semantic_part) {
                Some(argument_name) if argument_name == "p_target" => 0,
                Some(argument_name) if argument_name == "p_options" => 1,
                Some(_) => {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::UnknownQualifiedName,
                        format!("{name} accepts only named arguments p_target and p_options"),
                        input.logical_path,
                        &argument.span,
                    ));
                    return Some(None);
                }
                None => position,
            };
            if bound[index] {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!(
                        "duplicate argument for sys.inspect.snapshot parameter {}",
                        if index == 0 { "p_target" } else { "p_options" }
                    ),
                    input.logical_path,
                    &argument.span,
                ));
                return Some(None);
            }
            bound[index] = true;
            if index == 0 {
                target_argument = Some(argument);
            } else {
                options_argument = Some(argument);
            }
        }
        let Some(target_argument) = target_argument else {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "sys.inspect.snapshot requires p_target",
                input.logical_path,
                span,
            ));
            return Some(None);
        };
        (target_argument, options_argument)
    } else {
        if arguments.len() != 1 {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "sys.inspect projection requires exactly one snapshot argument",
                input.logical_path,
                span,
            ));
            return Some(None);
        }
        let argument = &arguments[0];
        if let Some(argument_name) = &argument.name
            && semantic_part(argument_name) != "p_snapshot"
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::UnknownQualifiedName,
                format!("{name} accepts only named argument p_snapshot"),
                input.logical_path,
                &argument.span,
            ));
            return Some(None);
        }
        (argument, None)
    };

    let (checked, expression_type) = check_client_expression(
        &target_argument.value,
        input,
        targets,
        action_targets,
        resource_targets,
        query_catalogue,
        base,
        server_names,
        standard,
        diagnostics,
        references,
        used_capabilities,
        locals,
    )?;
    let expected_type = if projection.is_none() {
        SemanticType::reference(CheckedTypeId::Existing(
            orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID,
        ))
    } else {
        SemanticType::Named(CheckedTypeId::Existing(
            orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        ))
    };
    if expression_type.semantic_type != expected_type
        || expression_type.result_shape != ClientExpressionResultShape::Value
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            if projection.is_none() {
                "sys.inspect.snapshot target must be REF sys.inspect.invocation"
            } else {
                "sys.inspect projection argument must be sys.inspect.snapshot"
            },
            input.logical_path,
            target_argument.value.span(),
        ));
        return Some(None);
    }

    if let Some(options_argument) = options_argument {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            "sys.inspect.snapshot options are not supported in Inspector v1",
            input.logical_path,
            options_argument.value.span(),
        ));
        return Some(None);
    }
    let checked_options = None;

    // The registry signature remains authoritative for the sealed operation.
    let valid_signature = if projection.is_none() {
        signature.parameter_count() == 2
            && signature.parameter_type(0)
                == Some(orna_core::system::SYS_INSPECT_INVOCATION_TYPE_ID)
            && signature.parameter_type(1)
                == Some(orna_core::system::SYS_INSPECT_SNAPSHOT_OPTIONS_TYPE_ID)
            && signature.result_type() == Some(orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID)
    } else {
        signature.parameter_count() == 1
            && signature.parameter_type(0) == Some(orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID)
            && signature.result_type().is_some()
    };
    if !valid_signature {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("sealed INSPECT function {name} has an invalid registry signature"),
            input.logical_path,
            span,
        ));
        return Some(None);
    }

    let (operation, result_type) = if let Some(projection) = projection {
        let result_type = match projection {
            CheckedInspectProjection::InvocationNodes => {
                orna_core::system::SYS_INSPECT_INVOCATION_NODES_TYPE_ID
            }
            CheckedInspectProjection::Calls => orna_core::system::SYS_INSPECT_CALLS_TYPE_ID,
            CheckedInspectProjection::Resources => orna_core::system::SYS_INSPECT_RESOURCES_TYPE_ID,
            CheckedInspectProjection::StateCells => {
                orna_core::system::SYS_INSPECT_STATE_CELLS_TYPE_ID
            }
            CheckedInspectProjection::UiNodes => orna_core::system::SYS_INSPECT_UI_NODES_TYPE_ID,
            CheckedInspectProjection::PresentationCandidates => {
                orna_core::system::SYS_INSPECT_PRESENTATION_CANDIDATES_TYPE_ID
            }
            CheckedInspectProjection::RuntimeBindings => {
                orna_core::system::SYS_INSPECT_RUNTIME_BINDINGS_TYPE_ID
            }
            CheckedInspectProjection::SecurityDecisions => {
                orna_core::system::SYS_INSPECT_SECURITY_DECISIONS_TYPE_ID
            }
        };
        if signature.result_type() != Some(result_type) {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("sealed INSPECT function {name} has the wrong result carrier"),
                input.logical_path,
                span,
            ));
            return Some(None);
        }
        (
            CheckedInspectOperation::Projection {
                projection,

                snapshot: Box::new(checked),
                location: location(input.logical_path, span),
            },
            result_type,
        )
    } else {
        (
            CheckedInspectOperation::Snapshot {
                target: Box::new(checked),
                options: checked_options,
                location: location(input.logical_path, span),
            },
            orna_core::system::SYS_INSPECT_SNAPSHOT_TYPE_ID,
        )
    };
    references.push(CheckedDefinitionReference {
        target: CheckedDefinitionReferenceTarget::Function(CheckedFunctionId::Existing(
            system.id(),
        )),
        kind: DefinitionReferenceKind::FunctionCall,
        location: location(input.logical_path, span),
    });
    Some(Some((
        CheckedClientExpression::Inspect { operation },
        ClientExpressionType {
            semantic_type: SemanticType::Named(CheckedTypeId::Existing(result_type)),
            standard_value_type: None,
            result_shape: ClientExpressionResultShape::Value,
        },
    )))
}
