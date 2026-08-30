use super::*;

mod capabilities;
mod constructors;
mod control_flow;
mod resources;

pub(super) use capabilities::validate_client_capability;
use capabilities::{checked_client_capability, normalise_client_parameter_name};
use constructors::{check_action_constructor, check_inspect_call, check_resource_constructor};
use control_flow::{
    check_client_control_flow_body, is_closed_client_boolean_return,
    is_standard_client_boolean_return,
};
#[cfg(test)]
pub(super) use resources::ClientResourceTypeParser;
pub(super) use resources::{client_contract_identity, client_local_resource_type};
use resources::{
    client_local_resource_family, client_type_specification_from_source,
    reject_deferred_client_resource_descriptor, validate_registered_client_external_contract,
};

pub(super) fn resolve_client_function_headers<'a>(
    parse_report: &'a ParseReport,
    function_ids: &HashMap<QualifiedSemanticName, CheckedFunctionId>,
) -> Vec<ClientFunctionHeader<'a>> {
    let mut headers = Vec::new();
    for unit in parse_report.units() {
        for declaration in unit.parsed().client_functions() {
            let name = semantic_name(&declaration.name);
            if let Some(&id) = function_ids.get(&name) {
                headers.push(ClientFunctionHeader {
                    declaration,
                    logical_path: unit.logical_path(),
                    id,
                });
            }
        }
    }
    headers
}

pub(super) fn resolve_client_function_inputs<'a>(
    headers: &[ClientFunctionHeader<'a>],
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    base: &CatalogueSnapshot,
    assignments: &mut CheckAssignments,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    uses: &mut Vec<CheckedApplicationTypeUse>,
) -> Vec<ResolvedClientFunctionInput<'a>> {
    let mut inputs = Vec::with_capacity(headers.len());
    for header in headers {
        let declaration = header.declaration;
        let diagnostics_before = diagnostics.len();
        let name = semantic_name(&declaration.name);
        let base_function = base.function_by_name(&name);
        let expression_body = declaration.body.as_expression().is_some()
            || declaration.body.as_external_contract().is_some()
            || declaration.body.as_state_block().is_some();
        if !expression_body && !declaration.parameters.is_empty() {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "this CLIENT function cannot declare parameters yet",
                header.logical_path,
                &declaration.parameter_list_span,
            ));
        }
        if !expression_body
            && let (
                Some(standard),
                FunctionReturnType::Single(specification),
                Some((_, body_source)),
            ) = (
                standard,
                &declaration.return_type,
                declaration.body.as_boolean_literal(),
            )
            && is_standard_client_boolean_return(specification)
            && matches!(
                intrinsic_boolean_type(Some(standard)),
                IntrinsicBooleanType::Missing
            )
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "the checked standard library does not provide a Boolean value type",
                header.logical_path,
                &body_source.span,
            ));
        }
        let mut parameter_names = HashSet::new();
        let mut parameters = Vec::with_capacity(declaration.parameters.len());

        for parameter in &declaration.parameters {
            let parameter_name = semantic_part(&parameter.name);
            if !parameter_names.insert(parameter_name.clone()) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DuplicateDefinition,
                    format!("duplicate parameter definition {parameter_name} in {name}"),
                    header.logical_path,
                    &parameter.name.span,
                ));
                continue;
            }
            if let Some(default) = &parameter.default_expression {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT function parameters do not yet support default values",
                    header.logical_path,
                    &default.span,
                ));
                continue;
            }

            let Some(resolved_type) = resolve_application_type_with_named_standard(
                &parameter.type_specification,
                submitted_ids,
                header.logical_path,
                diagnostics,
                standard,
                true,
            ) else {
                continue;
            };
            let id = assignments.parameter_id(
                base_function
                    .and_then(|function| function.parameter_by_name(&parameter_name))
                    .map(|parameter| parameter.id()),
            );
            record_standard_type_use(
                uses,
                standard,
                CheckedTypeUseKind::Parameter {
                    owner: header.id,
                    parameter: id,
                },
                resolved_type,
                type_use_location(&parameter.type_specification, header.logical_path),
            );
            parameters.push(ResolvedServerFunctionParameter {
                id,
                name: parameter_name,
                ordinal: parameter.order as u32,
                semantic_type: resolved_type.semantic_type,
                standard_value_type: resolved_type.standard_value_type,
                name_span: parameter.name.span.clone(),
                location: location(header.logical_path, &parameter.span),
                reference_location: reference_location(
                    &parameter.type_specification,
                    header.logical_path,
                ),
            });
        }

        let return_type = match &declaration.return_type {
            FunctionReturnType::Single(specification) if !expression_body && standard.is_none() => {
                if is_closed_client_boolean_return(specification) {
                    Some(ResolvedApplicationType {
                        semantic_type: SemanticType::scalar(StandardScalar::Boolean),
                        standard_value_type: None,
                    })
                } else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "this CLIENT function must return BOOLEAN",
                        header.logical_path,
                        specification.span(),
                    ));
                    None
                }
            }
            FunctionReturnType::Single(specification) => {
                resolve_application_type_with_named_standard(
                    specification,
                    submitted_ids,
                    header.logical_path,
                    diagnostics,
                    standard,
                    true,
                )
            }
            FunctionReturnType::Stream { element, .. } if expression_body => {
                resolve_application_type_with_named_standard(
                    element,
                    submitted_ids,
                    header.logical_path,
                    diagnostics,
                    standard,
                    true,
                )
            }
            FunctionReturnType::Rows { span, .. } | FunctionReturnType::Stream { span, .. } => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    if expression_body {
                        "this CLIENT function must return one value"
                    } else {
                        "this CLIENT function must return BOOLEAN"
                    },
                    header.logical_path,
                    span,
                ));
                None
            }
        };

        if diagnostics.len() != diagnostics_before {
            continue;
        }
        let Some(return_type) = return_type else {
            continue;
        };
        let result_shape = match &declaration.return_type {
            FunctionReturnType::Stream { .. } => ClientExpressionResultShape::OptionalList,
            FunctionReturnType::Single(_) | FunctionReturnType::Rows { .. } => {
                ClientExpressionResultShape::Value
            }
        };
        let return_shape = match result_shape {
            ClientExpressionResultShape::Value => CheckedClientReturnShape::Single,
            ClientExpressionResultShape::OptionalList => CheckedClientReturnShape::Stream,
        };
        if !expression_body
            && return_type.semantic_type != SemanticType::scalar(StandardScalar::Boolean)
        {
            let span = match &declaration.return_type {
                FunctionReturnType::Single(specification) => specification.span(),
                FunctionReturnType::Rows { span, .. } | FunctionReturnType::Stream { span, .. } => {
                    span
                }
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function must return BOOLEAN",
                header.logical_path,
                span,
            ));
            continue;
        }
        if expression_body
            && !client_expression_type_is_evaluable(
                ClientExpressionType {
                    semantic_type: return_type.semantic_type,
                    standard_value_type: return_type.standard_value_type,
                    result_shape,
                },
                base,
                standard,
            )
        {
            let span = match &declaration.return_type {
                FunctionReturnType::Single(specification) => specification.span(),
                FunctionReturnType::Rows { span, .. } | FunctionReturnType::Stream { span, .. } => {
                    span
                }
            };
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function return type is not supported by the local evaluator",
                header.logical_path,
                span,
            ));
            continue;
        }
        if let FunctionReturnType::Single(specification)
        | FunctionReturnType::Stream {
            element: specification,
            ..
        } = &declaration.return_type
        {
            record_standard_type_use(
                uses,
                standard,
                CheckedTypeUseKind::Return {
                    owner: header.id,
                    ordinal: 0,
                },
                return_type,
                type_use_location(specification, header.logical_path),
            );
        }
        inputs.push(ResolvedClientFunctionInput {
            id: header.id,
            name,
            capabilities: &declaration.capabilities,
            parameters,
            return_type: return_type.semantic_type,
            standard_value_type: return_type.standard_value_type,
            result_shape,
            return_shape,
            body: &declaration.body,
            location: location(header.logical_path, &declaration.span),
            declaration_span: declaration.span.clone(),
            logical_path: header.logical_path,
            control_flow_required: client_body_requires_control_flow(&declaration.body),
        });
    }
    inputs
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ClientExpressionResultShape {
    Value,
    OptionalList,
}

#[derive(Clone, Copy)]
pub(super) struct ClientExpressionType {
    pub(super) semantic_type: SemanticType<CheckedTypeId>,
    pub(super) standard_value_type: Option<orna_core::TypeId>,
    pub(super) result_shape: ClientExpressionResultShape,
}

#[derive(Clone)]
struct ClientLocalBinding {
    checked: CheckedClientExpression,
    expression_type: ClientExpressionType,
    // Procedural locals are read by ordinal. Legacy state-block locals keep
    // their old substitution behaviour and therefore have no ordinal.
    ordinal: Option<u32>,
    kind: CheckedClientLocalKind,
}

type ClientLocalEnvironment = HashMap<String, ClientLocalBinding>;

#[derive(Clone)]
struct ClientExpressionParameter {
    id: CheckedParameterId,
    name: String,
    expression_type: ClientExpressionType,
}

#[derive(Clone)]
struct ClientExpressionTarget {
    id: CheckedFunctionId,
    parameters: Vec<ClientExpressionParameter>,
    return_type: ClientExpressionType,
}

#[derive(Clone)]
struct ClientActionTarget {
    domain: orna_artifact::client_plan::ActionTargetDomain,
    id: CheckedFunctionId,
    parameters: Vec<ClientExpressionParameter>,
    return_type: ClientExpressionType,
}

fn action_result_type_is_durable(
    result_type: ClientExpressionType,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    matches!(
        result_type.semantic_type,
        SemanticType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject,
        ) if result_type.standard_value_type.is_some()
    ) || matches!(result_type.semantic_type, SemanticType::Reference { .. })
        || matches!(
        result_type.semantic_type,
        SemanticType::Named(CheckedTypeId::Existing(type_id))
            if standard.is_some_and(|standard| {
                standard
                    .verified_snapshot()
                    .catalogue()
                    .value_type_by_id(type_id)
                    .is_some_and(|value_type| {
                        value_type.persistence() == ValueTypePersistence::Persistable
                            || type_id == STD_ACTION_TYPE_ID
                    })
            })
        )
}

fn client_action_result_type(
    result_type: ClientExpressionType,
    standard: Option<&CheckedStandardLibrary>,
) -> ClientExpressionType {
    if result_type.standard_value_type.is_some() {
        return result_type;
    }
    let SemanticType::Named(CheckedTypeId::Existing(type_id)) = result_type.semantic_type else {
        return result_type;
    };
    if standard.is_some_and(|standard| {
        standard
            .verified_snapshot()
            .catalogue()
            .value_type_by_id(type_id)
            .is_some()
    }) {
        ClientExpressionType {
            standard_value_type: Some(type_id),
            ..result_type
        }
    } else {
        result_type
    }
}

fn action_argument_type_is_orv3_encodable(
    expression_type: ClientExpressionType,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    matches!(
        expression_type.semantic_type,
        SemanticType::Scalar(
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ) if expression_type.standard_value_type.is_some()
    ) || matches!(
        expression_type.semantic_type,
        SemanticType::Reference { .. }
    ) || matches!(
        expression_type.semantic_type,
        SemanticType::Named(type_id)
            if match type_id {
                CheckedTypeId::Provisional(_) => true,
                CheckedTypeId::Existing(type_id) => {
                    base.enum_type_by_id(type_id).is_some()
                        || base.record_value_type_by_id(type_id).is_some()
                        || standard.is_some_and(|standard| {
                            let catalogue = standard.verified_snapshot().catalogue();
                            catalogue.enum_type_by_id(type_id).is_some()
                                || catalogue.record_value_type_by_id(type_id).is_some()
                        })
                }
            }
    )
}

fn client_expression_contains_await_or_resource(
    expression: &CheckedClientExpression,
    locals: &ClientLocalEnvironment,
) -> bool {
    match expression {
        CheckedClientExpression::Await { .. } | CheckedClientExpression::Resource { .. } => true,
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, argument)| client_expression_contains_await_or_resource(argument, locals)),
        CheckedClientExpression::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, argument)| client_expression_contains_await_or_resource(argument, locals)),
        CheckedClientExpression::Inspect { operation } => match operation {
            CheckedInspectOperation::Snapshot {
                target, options, ..
            } => {
                client_expression_contains_await_or_resource(target, locals)
                    || options.as_deref().is_some_and(|options| {
                        client_expression_contains_await_or_resource(options, locals)
                    })
            }
            CheckedInspectOperation::Projection { snapshot, .. } => {
                client_expression_contains_await_or_resource(snapshot, locals)
            }
        },
        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_await_or_resource(left, locals)
                || client_expression_contains_await_or_resource(right, locals)
        }
        CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_await_or_resource(expression, locals)
        }
        CheckedClientExpression::LocalRead { local, .. } => locals.values().any(|binding| {
            binding.ordinal == Some(*local)
                && matches!(binding.kind, CheckedClientLocalKind::Resource(_))
        }),
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::Evaluate { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}
fn client_expression_contains_inspect(expression: &CheckedClientExpression) -> bool {
    match expression {
        CheckedClientExpression::Inspect { .. } => true,
        CheckedClientExpression::Await { expression, .. } => {
            client_expression_contains_inspect(expression)
        }
        CheckedClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|(_, argument)| client_expression_contains_inspect(argument)),
        CheckedClientExpression::Resource { operation } => operation
            .arguments()
            .iter()
            .any(|(_, argument)| client_expression_contains_inspect(argument)),
        CheckedClientExpression::Action { operation } => operation
            .arguments()
            .iter()
            .any(|(_, argument)| client_expression_contains_inspect(argument)),

        CheckedClientExpression::Concat { left, right, .. }
        | CheckedClientExpression::Binary { left, right, .. } => {
            client_expression_contains_inspect(left) || client_expression_contains_inspect(right)
        }
        CheckedClientExpression::Unary { expression, .. }
        | CheckedClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_inspect(expression)
        }
        CheckedClientExpression::SourceIntrospection { .. }
        | CheckedClientExpression::Input { .. }
        | CheckedClientExpression::Evaluate { .. }
        | CheckedClientExpression::String { .. }
        | CheckedClientExpression::Integer { .. }
        | CheckedClientExpression::Boolean { .. }
        | CheckedClientExpression::ParameterRead { .. }
        | CheckedClientExpression::LocalRead { .. }
        | CheckedClientExpression::FieldPath { .. } => false,
    }
}

fn client_expression_contains_action(expression: &ClientExpression) -> bool {
    let action_name =
        QualifiedSemanticName::new(["std", "action", "call"]).expect("std.action.call is valid");
    match expression {
        ClientExpression::Call {
            callee, arguments, ..
        } => {
            semantic_name(callee) == action_name
                || arguments
                    .iter()
                    .any(|argument| client_expression_contains_action(&argument.value))
        }
        ClientExpression::Await { expression, .. } => client_expression_contains_action(expression),
        ClientExpression::Concat { left, right, .. } => {
            client_expression_contains_action(left) || client_expression_contains_action(right)
        }
        ClientExpression::Binary(binary) => {
            client_expression_contains_action(&binary.left)
                || client_expression_contains_action(&binary.right)
        }
        ClientExpression::Unary(unary) => client_expression_contains_action(&unary.expression),
        ClientExpression::Parenthesized { expression, .. } => {
            client_expression_contains_action(expression)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => false,
    }
}

fn action_target_parameters(
    parameters: &[ResolvedServerFunctionParameter],
) -> Option<Vec<ClientExpressionParameter>> {
    parameters
        .iter()
        .map(|parameter| {
            Some(ClientExpressionParameter {
                id: parameter.id,
                name: parameter.name.clone(),
                expression_type: ClientExpressionType {
                    semantic_type: parameter.semantic_type,
                    standard_value_type: parameter.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            })
        })
        .collect()
}

#[derive(Clone)]
pub(super) struct ClientResourceTarget {
    kind: ResourceKind,
    id: CheckedFunctionId,
    parameters: Vec<ClientExpressionParameter>,
    result_type: ClientExpressionType,
}

fn standard_value_type_scalar(
    standard: Option<&CheckedStandardLibrary>,
    type_id: orna_core::TypeId,
) -> Option<StandardScalar> {
    standard.and_then(|standard| {
        standard
            .value_types()
            .iter()
            .find(|value_type| value_type.id() == type_id)
            .and_then(|value_type| {
                (value_type.kind() == ValueTypeKind::Primitive)
                    .then(|| compatibility_scalar(value_type.representation_contract()))
                    .flatten()
            })
    })
}

fn standard_scalar_type_id(
    standard: Option<&CheckedStandardLibrary>,
    scalar: StandardScalar,
) -> Option<orna_core::TypeId> {
    standard.and_then(|standard| {
        standard.value_types().iter().find_map(|value_type| {
            (value_type.kind() == ValueTypeKind::Primitive
                && compatibility_scalar(value_type.representation_contract()) == Some(scalar))
            .then_some(value_type.id())
        })
    })
}

fn client_expression_type_from_core(
    resolved_type: ResolvedType,
    standard: Option<&CheckedStandardLibrary>,
) -> Option<ClientExpressionType> {
    client_expression_type_from_core_with_shape(
        resolved_type,
        standard,
        ClientExpressionResultShape::Value,
    )
}

fn client_expression_type_from_core_with_shape(
    resolved_type: ResolvedType,
    standard: Option<&CheckedStandardLibrary>,
    result_shape: ClientExpressionResultShape,
) -> Option<ClientExpressionType> {
    let (semantic_type, standard_value_type) = match resolved_type {
        ResolvedType::Scalar(scalar) => (
            SemanticType::scalar(scalar),
            standard_scalar_type_id(standard, scalar),
        ),
        ResolvedType::Named(type_id) => {
            (SemanticType::Named(CheckedTypeId::Existing(type_id)), None)
        }
        ResolvedType::Reference { target } => (
            SemanticType::reference(CheckedTypeId::Existing(target)),
            None,
        ),
        ResolvedType::Value(type_id) => {
            let scalar = standard_value_type_scalar(standard, type_id);
            (
                scalar.map_or(
                    SemanticType::Named(CheckedTypeId::Existing(type_id)),
                    SemanticType::scalar,
                ),
                scalar.map(|_| type_id),
            )
        }
    };
    Some(ClientExpressionType {
        semantic_type,
        standard_value_type,
        result_shape,
    })
}

fn client_expression_targets(
    inputs: &[ResolvedClientFunctionInput<'_>],
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> HashMap<QualifiedSemanticName, ClientExpressionTarget> {
    let mut targets = HashMap::new();
    for input in inputs {
        targets.insert(
            input.name.clone(),
            ClientExpressionTarget {
                id: input.id,
                parameters: input
                    .parameters
                    .iter()
                    .map(|parameter| ClientExpressionParameter {
                        id: parameter.id,
                        name: parameter.name.clone(),
                        expression_type: ClientExpressionType {
                            semantic_type: parameter.semantic_type,
                            standard_value_type: parameter.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        },
                    })
                    .collect(),
                return_type: ClientExpressionType {
                    semantic_type: input.return_type,
                    standard_value_type: input.standard_value_type,
                    result_shape: input.result_shape,
                },
            },
        );
    }
    let standard_functions =
        standard.map(|standard| standard.verified_snapshot().catalogue().functions());
    // Application CLIENT declarations and functions take precedence over a
    // same-named standard target; the standard target is only a fallback.
    for functions in [Some(base.functions()), standard_functions] {
        let Some(functions) = functions else {
            continue;
        };
        for function in functions {
            if function.domain() != FunctionDomain::Client || targets.contains_key(function.name())
            {
                continue;
            }
            let Some(return_type) = (match function.return_type() {
                FunctionReturn::Single(resolved_type) => {
                    client_expression_type_from_core(*resolved_type, standard)
                }
                FunctionReturn::Stream(resolved_type) => {
                    client_expression_type_from_core_with_shape(
                        *resolved_type,
                        standard,
                        ClientExpressionResultShape::OptionalList,
                    )
                }
                FunctionReturn::Rows(_) => None,
            }) else {
                continue;
            };
            let Some(parameters) = function
                .parameters()
                .iter()
                .map(|parameter| {
                    client_expression_type_from_core(parameter.resolved_type(), standard).map(
                        |expression_type| ClientExpressionParameter {
                            id: CheckedParameterId::Existing(parameter.id()),
                            name: parameter.name().to_owned(),
                            expression_type,
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()
            else {
                // An unrepresentable parameter must not disappear from the
                // target signature and make an incomplete call look bound.
                continue;
            };
            targets.insert(
                function.name().clone(),
                ClientExpressionTarget {
                    id: CheckedFunctionId::Existing(function.id()),
                    parameters,
                    return_type,
                },
            );
        }
    }
    targets
}

fn client_action_targets(
    client_inputs: &[ResolvedClientFunctionInput<'_>],
    server_inputs: &[ResolvedServerFunctionInput<'_>],
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> HashMap<QualifiedSemanticName, ClientActionTarget> {
    let mut targets = HashMap::new();
    for input in client_inputs {
        // ADR 0079 defers stream actions. Client stream functions are valid
        // expression producers, but they are not action targets until the
        // action protocol has an explicit stream result contract.
        if input.result_shape != ClientExpressionResultShape::Value {
            continue;
        }
        let return_type = client_action_result_type(
            ClientExpressionType {
                semantic_type: input.return_type,
                standard_value_type: input.standard_value_type,
                result_shape: input.result_shape,
            },
            standard,
        );
        if !action_result_type_is_durable(return_type, standard) {
            continue;
        }
        let Some(parameters) = action_target_parameters(&input.parameters) else {
            continue;
        };
        targets.insert(
            input.name.clone(),
            ClientActionTarget {
                domain: orna_artifact::client_plan::ActionTargetDomain::Client,
                id: input.id,
                parameters,
                return_type,
            },
        );
    }
    for input in server_inputs {
        let return_type = client_action_result_type(
            match input.return_type {
                ResolvedServerFunctionReturn::Single {
                    semantic_type,
                    standard_value_type,
                    ..
                } => ClientExpressionType {
                    semantic_type,
                    standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
                ResolvedServerFunctionReturn::Rows { .. }
                | ResolvedServerFunctionReturn::Stream { .. } => continue,
            },
            standard,
        );
        if !action_result_type_is_durable(return_type, standard) {
            continue;
        }
        let Some(parameters) = action_target_parameters(&input.parameters) else {
            continue;
        };
        targets.insert(
            input.name.clone(),
            ClientActionTarget {
                domain: orna_artifact::client_plan::ActionTargetDomain::Server,
                id: input.id,
                parameters,
                return_type,
            },
        );
    }
    let standard_functions =
        standard.map(|standard| standard.verified_snapshot().catalogue().functions());
    // Keep application precedence so a target name resolves to one catalogue identity.
    for functions in [Some(base.functions()), standard_functions] {
        let Some(functions) = functions else {
            continue;
        };
        for function in functions {
            if targets.contains_key(function.name()) {
                continue;
            }
            let return_type = match function.return_type() {
                FunctionReturn::Single(resolved) => {
                    client_expression_type_from_core(*resolved, standard)
                }
                // Action execution rejects ROWS and STREAM results (ADR 0079),
                // including one-column ROWS that could otherwise look scalar.
                FunctionReturn::Rows(_) | FunctionReturn::Stream(_) => None,
            };
            let Some(return_type) = return_type
                .map(|value| client_action_result_type(value, standard))
                .filter(|value| action_result_type_is_durable(*value, standard))
            else {
                continue;
            };
            let Some(parameters) = function
                .parameters()
                .iter()
                .map(|parameter| {
                    client_expression_type_from_core(parameter.resolved_type(), standard).map(
                        |expression_type| ClientExpressionParameter {
                            id: CheckedParameterId::Existing(parameter.id()),
                            name: parameter.name().to_owned(),
                            expression_type,
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            targets.insert(
                function.name().clone(),
                ClientActionTarget {
                    domain: match function.domain() {
                        FunctionDomain::Client => {
                            orna_artifact::client_plan::ActionTargetDomain::Client
                        }
                        FunctionDomain::Server => {
                            orna_artifact::client_plan::ActionTargetDomain::Server
                        }
                    },
                    id: CheckedFunctionId::Existing(function.id()),
                    parameters,
                    return_type,
                },
            );
        }
    }
    targets
}

fn client_resource_call_site_id(
    location: &SourceLocation,
    owner: &QualifiedSemanticName,
) -> CallSiteId {
    let path = location.logical_path().as_bytes();
    let mut payload = Vec::with_capacity(path.len() + 32);
    payload.extend_from_slice(&(path.len() as u64).to_be_bytes());
    payload.extend_from_slice(path);
    payload.extend_from_slice(&(location.span().start() as u64).to_be_bytes());
    payload.extend_from_slice(&(location.span().end() as u64).to_be_bytes());
    // A call-site identifies the compiled source location in its owning
    // CLIENT function. The target and its revision are separate resource
    // identity fields, so retargeting does not change the call-site identity.
    let owner = owner.to_string();
    payload.extend_from_slice(&(owner.len() as u64).to_be_bytes());
    payload.extend_from_slice(owner.as_bytes());
    let digest = artifact_payload_digest(&payload).expect("resource call-site payload is bounded");
    CallSiteId::from_bytes(
        digest.to_bytes()[..16]
            .try_into()
            .expect("digest has 16-byte prefix"),
    )
}

/// Returns whether a STREAM item can be materialised as the runtime
/// canonical `OPTION<LIST<T>>` resource value.
///
/// The client runtime collection representation admits the six legacy scalar
/// values, active enum/record identities, and active object references.
/// Other scalar identities and opaque values may be valid function types but
/// cannot be represented inside the list descriptor used for stream batches.
pub(super) fn client_resource_stream_type_is_supported(
    expression_type: ClientExpressionType,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    match expression_type.semantic_type {
        SemanticType::Scalar(scalar) => matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ),
        SemanticType::Named(CheckedTypeId::Provisional(_)) => true,
        SemanticType::Reference {
            target: CheckedTypeId::Provisional(_),
        } => true,
        SemanticType::Named(CheckedTypeId::Existing(type_id)) => {
            base.enum_type_by_id(type_id).is_some()
                || base.record_value_type_by_id(type_id).is_some()
                || standard.is_some_and(|standard| {
                    let catalogue = standard.verified_snapshot().catalogue();
                    catalogue.enum_type_by_id(type_id).is_some()
                        || catalogue.record_value_type_by_id(type_id).is_some()
                })
        }
        SemanticType::Reference {
            target: CheckedTypeId::Existing(type_id),
        } => {
            base.object_type_by_id(type_id).is_some()
                || standard.is_some_and(|standard| {
                    standard
                        .verified_snapshot()
                        .catalogue()
                        .object_type_by_id(type_id)
                        .is_some()
                })
        }
    }
}

pub(super) fn client_resource_targets(
    inputs: &[ResolvedServerFunctionInput<'_>],
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> HashMap<QualifiedSemanticName, ClientResourceTarget> {
    let mut targets = HashMap::new();
    for input in inputs {
        let (kind, result_type) = match &input.return_type {
            ResolvedServerFunctionReturn::Single {
                semantic_type,
                standard_value_type,
                ..
            } => (
                ResourceKind::Scalar,
                ClientExpressionType {
                    semantic_type: *semantic_type,
                    standard_value_type: *standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ),
            ResolvedServerFunctionReturn::Stream {
                semantic_type,
                standard_value_type,
                ..
            } => (
                ResourceKind::Stream,
                ClientExpressionType {
                    semantic_type: *semantic_type,
                    standard_value_type: *standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ),
            ResolvedServerFunctionReturn::Rows { .. } => continue,
        };
        if kind == ResourceKind::Stream
            && !client_resource_stream_type_is_supported(result_type, base, standard)
        {
            continue;
        }
        targets.insert(
            input.name.clone(),
            ClientResourceTarget {
                kind,
                id: input.id,
                parameters: input
                    .parameters
                    .iter()
                    .map(|parameter| ClientExpressionParameter {
                        id: parameter.id,
                        name: parameter.name.clone(),
                        expression_type: ClientExpressionType {
                            semantic_type: parameter.semantic_type,
                            standard_value_type: parameter.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        },
                    })
                    .collect(),
                result_type,
            },
        );
    }
    let standard_functions =
        standard.map(|standard| standard.verified_snapshot().catalogue().functions());
    for (is_standard, functions) in [(false, Some(base.functions())), (true, standard_functions)] {
        let Some(functions) = functions else {
            continue;
        };
        for function in functions {
            // Standard resource execution is intentionally closed to the one
            // executor currently implemented by the client resource path.
            // Return shape alone must not admit presenters or future functions.
            if is_standard && function.id() != STD_INVOKE_ECHO_FUNCTION_ID {
                continue;
            }
            if function.domain() != FunctionDomain::Server || targets.contains_key(function.name())
            {
                continue;
            }
            let (kind, result_type) = match function.return_type() {
                FunctionReturn::Single(resolved) => (
                    ResourceKind::Scalar,
                    client_expression_type_from_core(*resolved, standard),
                ),
                FunctionReturn::Stream(resolved) => (
                    ResourceKind::Stream,
                    client_expression_type_from_core(*resolved, standard),
                ),
                FunctionReturn::Rows(_) => continue,
            };
            let Some(result_type) = result_type else {
                continue;
            };
            if kind == ResourceKind::Stream
                && !client_resource_stream_type_is_supported(result_type, base, standard)
            {
                continue;
            }
            let Some(parameters) = function
                .parameters()
                .iter()
                .map(|parameter| {
                    client_expression_type_from_core(parameter.resolved_type(), standard).map(
                        |expression_type| ClientExpressionParameter {
                            id: CheckedParameterId::Existing(parameter.id()),
                            name: parameter.name().to_owned(),
                            expression_type,
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()
            else {
                // An unrepresentable parameter must not disappear from the
                // target signature and make an incomplete call look bound.
                continue;
            };
            targets.insert(
                function.name().clone(),
                ClientResourceTarget {
                    kind,
                    id: CheckedFunctionId::Existing(function.id()),
                    parameters,
                    result_type,
                },
            );
        }
    }
    targets
}

fn client_expression_type_is_evaluable(
    expression_type: ClientExpressionType,
    base: &CatalogueSnapshot,
    standard: Option<&CheckedStandardLibrary>,
) -> bool {
    if expression_type.result_shape == ClientExpressionResultShape::OptionalList {
        return client_resource_stream_type_is_supported(expression_type, base, standard);
    }
    match expression_type.semantic_type {
        SemanticType::Scalar(scalar) => matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::Float
                | StandardScalar::CharacterLargeObject
                | StandardScalar::BinaryLargeObject
        ),
        SemanticType::Named(CheckedTypeId::Existing(type_id))
            if is_sealed_inspect_type_id(type_id)
                || type_id == SYS_SOURCE_FUNCTION_TYPE_ID
                || type_id == STD_UI_TYPE_ID =>
        {
            true
        }
        SemanticType::Named(CheckedTypeId::Existing(type_id)) => standard
            .and_then(|standard| {
                standard
                    .value_types()
                    .iter()
                    .find(|value_type| value_type.id() == type_id)
            })
            .is_some_and(|value_type| {
                value_type.kind() == ValueTypeKind::Opaque
                    || matches!(
                        value_type.representation_contract(),
                        "orna.kernel.value.boolean@1"
                            | "orna.kernel.value.integer@1"
                            | "orna.kernel.value.bigint@1"
                            | "orna.kernel.value.float@1"
                            | "orna.kernel.value.character-large-object@1"
                            | "orna.kernel.value.binary-large-object@1"
                    )
            }),
        SemanticType::Named(CheckedTypeId::Provisional(_)) | SemanticType::Reference { .. } => {
            false
        }
    }
}

fn client_expression_types_compatible(
    actual: ClientExpressionType,
    expected: ClientExpressionType,
) -> bool {
    let named_standard_alias = matches!(
        (
            actual.semantic_type,
            expected.semantic_type,
            actual.standard_value_type,
            expected.standard_value_type,
        ),
        (
            SemanticType::Named(CheckedTypeId::Existing(actual_id)),
            SemanticType::Scalar(_),
            None,
            Some(expected_id),
        ) if actual_id == expected_id
    );
    (actual.semantic_type == expected.semantic_type || named_standard_alias)
        && actual.result_shape == expected.result_shape
        && (expected.standard_value_type.is_none()
            || actual.standard_value_type == expected.standard_value_type
            || named_standard_alias)
}

fn resource_constructor_kind(name: &QualifiedSemanticName) -> Option<ResourceKind> {
    if name == &QualifiedSemanticName::new(["std", "data", "resource"]).ok()? {
        Some(ResourceKind::Scalar)
    } else if name == &QualifiedSemanticName::new(["std", "data", "stream_resource"]).ok()? {
        Some(ResourceKind::Stream)
    } else {
        None
    }
}

fn checked_client_unary_operator(
    operator: orna_syntax::ClientUnaryOperator,
) -> ControlFlowUnaryOperator {
    match operator {
        orna_syntax::ClientUnaryOperator::Plus => ControlFlowUnaryOperator::Plus,
        orna_syntax::ClientUnaryOperator::Minus => ControlFlowUnaryOperator::Minus,
        orna_syntax::ClientUnaryOperator::Not => ControlFlowUnaryOperator::Not,
    }
}

fn checked_client_binary_operator(
    operator: orna_syntax::ClientBinaryOperator,
) -> ControlFlowBinaryOperator {
    match operator {
        orna_syntax::ClientBinaryOperator::Add => ControlFlowBinaryOperator::Add,
        orna_syntax::ClientBinaryOperator::Subtract => ControlFlowBinaryOperator::Subtract,
        orna_syntax::ClientBinaryOperator::Multiply => ControlFlowBinaryOperator::Multiply,
        orna_syntax::ClientBinaryOperator::Divide => ControlFlowBinaryOperator::Divide,
        orna_syntax::ClientBinaryOperator::Modulo => ControlFlowBinaryOperator::Modulo,
        orna_syntax::ClientBinaryOperator::Equal => ControlFlowBinaryOperator::Equal,
        orna_syntax::ClientBinaryOperator::NotEqual => ControlFlowBinaryOperator::NotEqual,
        orna_syntax::ClientBinaryOperator::LessThan => ControlFlowBinaryOperator::LessThan,
        orna_syntax::ClientBinaryOperator::GreaterThan => ControlFlowBinaryOperator::GreaterThan,
        orna_syntax::ClientBinaryOperator::LessThanOrEqual => {
            ControlFlowBinaryOperator::LessThanOrEqual
        }
        orna_syntax::ClientBinaryOperator::GreaterThanOrEqual => {
            ControlFlowBinaryOperator::GreaterThanOrEqual
        }
        orna_syntax::ClientBinaryOperator::And => ControlFlowBinaryOperator::And,
        orna_syntax::ClientBinaryOperator::Or => ControlFlowBinaryOperator::Or,
    }
}

fn control_flow_supported_scalar(expression_type: ClientExpressionType) -> Option<StandardScalar> {
    if expression_type.result_shape != ClientExpressionResultShape::Value {
        return None;
    }
    match expression_type.semantic_type {
        SemanticType::Scalar(
            scalar @ (StandardScalar::Integer
            | StandardScalar::Boolean
            | StandardScalar::CharacterLargeObject),
        ) => Some(scalar),
        SemanticType::Scalar(_) | SemanticType::Named(_) | SemanticType::Reference { .. } => None,
    }
}

fn control_flow_types_match(left: ClientExpressionType, right: ClientExpressionType) -> bool {
    control_flow_supported_scalar(left).is_some()
        && control_flow_supported_scalar(left) == control_flow_supported_scalar(right)
        && left.standard_value_type == right.standard_value_type
}

#[allow(clippy::too_many_arguments)]
fn check_client_expression(
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
    let expression_location = || location(input.logical_path, expression.span());
    match expression {
        ClientExpression::Await { expression, span } => {
            let (checked_resource, result_type) = check_resource_constructor(
                expression,
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
            let resource_kind = match &checked_resource {
                CheckedClientExpression::Resource { operation } => Some(operation.kind()),
                CheckedClientExpression::LocalRead { .. } => match expression.as_ref() {
                    ClientExpression::LocalRead { local } => locals
                        .get(&semantic_part(local))
                        .and_then(|binding| match binding.kind {
                            CheckedClientLocalKind::Resource(kind) => Some(kind),
                            CheckedClientLocalKind::Value => None,
                        }),
                    _ => None,
                },
                _ => None,
            };
            let result_shape = if resource_kind == Some(ResourceKind::Stream) {
                ClientExpressionResultShape::OptionalList
            } else {
                ClientExpressionResultShape::Value
            };
            let result_type = ClientExpressionType {
                result_shape,
                ..result_type
            };
            Some((
                CheckedClientExpression::Await {
                    expression: Box::new(checked_resource),
                    location: location(input.logical_path, span),
                },
                result_type,
            ))
        }
        ClientExpression::StringLiteral { value, source } => {
            let expression_type = ClientExpressionType {
                semantic_type: SemanticType::scalar(StandardScalar::CharacterLargeObject),
                standard_value_type: standard_scalar_type_id(
                    standard,
                    StandardScalar::CharacterLargeObject,
                ),
                result_shape: ClientExpressionResultShape::Value,
            };
            Some((
                CheckedClientExpression::String {
                    value: value.clone(),
                    location: location(input.logical_path, &source.span),
                },
                expression_type,
            ))
        }
        ClientExpression::IntegerLiteral { value, source } => {
            if i32::try_from(*value).is_err() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT integer literal is outside the INTEGER range",
                    input.logical_path,
                    &source.span,
                ));
                return None;
            }
            let expression_type = ClientExpressionType {
                semantic_type: SemanticType::scalar(StandardScalar::Integer),
                standard_value_type: standard_scalar_type_id(standard, StandardScalar::Integer),
                result_shape: ClientExpressionResultShape::Value,
            };
            Some((
                CheckedClientExpression::Integer {
                    value: *value,
                    location: location(input.logical_path, &source.span),
                },
                expression_type,
            ))
        }
        ClientExpression::BooleanLiteral { value, source } => {
            let expression_type = ClientExpressionType {
                semantic_type: SemanticType::scalar(StandardScalar::Boolean),
                standard_value_type: standard_scalar_type_id(standard, StandardScalar::Boolean),
                result_shape: ClientExpressionResultShape::Value,
            };
            Some((
                CheckedClientExpression::Boolean {
                    value: *value,
                    location: location(input.logical_path, &source.span),
                },
                expression_type,
            ))
        }
        ClientExpression::ParameterRead { parameter } => {
            let name = semantic_part(parameter);
            if let Some(binding) = locals.get(&name) {
                return Some((
                    binding.ordinal.map_or_else(
                        || binding.checked.clone(),
                        |ordinal| CheckedClientExpression::LocalRead {
                            local: ordinal,
                            location: expression_location(),
                        },
                    ),
                    binding.expression_type,
                ));
            }
            let Some(parameter) = input
                .parameters
                .iter()
                .find(|parameter| parameter.name == name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT parameter {name}"),
                    input.logical_path,
                    &parameter.span,
                ));
                return None;
            };
            Some((
                CheckedClientExpression::ParameterRead {
                    parameter: parameter.id,
                    location: expression_location(),
                },
                ClientExpressionType {
                    semantic_type: parameter.semantic_type,
                    standard_value_type: parameter.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ))
        }
        ClientExpression::LocalRead { local } => {
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
            Some((
                binding.ordinal.map_or_else(
                    || binding.checked.clone(),
                    |ordinal| CheckedClientExpression::LocalRead {
                        local: ordinal,
                        location: expression_location(),
                    },
                ),
                binding.expression_type,
            ))
        }
        ClientExpression::FieldPath {
            root,
            members,
            span,
        } => {
            let root_name = semantic_part(root);
            let Some(parameter) = input
                .parameters
                .iter()
                .find(|parameter| parameter.name == root_name)
            else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT parameter {root_name}"),
                    input.logical_path,
                    &root.span,
                ));
                return None;
            };
            let SemanticType::Reference { target } = parameter.semantic_type else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT field paths require a REF parameter",
                    input.logical_path,
                    span,
                ));
                return None;
            };
            let mut owner = target;
            let mut fields = Vec::with_capacity(members.len());
            let mut expression_type = None;
            for (index, member) in members.iter().enumerate() {
                let field_name = semantic_part(member);
                let Some(field) =
                    QueryCatalogue::field_by_name(query_catalogue, owner, &field_name)
                else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::UnknownQualifiedName,
                        format!("unknown field {field_name} in CLIENT field path"),
                        input.logical_path,
                        &member.span,
                    ));
                    return None;
                };
                fields.push(field.id());
                expression_type = Some(ClientExpressionType {
                    semantic_type: field.semantic_type(),
                    standard_value_type: field.standard_value_type(),
                    result_shape: ClientExpressionResultShape::Value,
                });
                if let SemanticType::Reference { target: next } = field.semantic_type() {
                    owner = next;
                } else if index + 1 != members.len() {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "CLIENT field path continues through a non-reference field",
                        input.logical_path,
                        &member.span,
                    ));
                    return None;
                }
            }
            let Some(expression_type) = expression_type else {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT field path must select a field",
                    input.logical_path,
                    span,
                ));
                return None;
            };
            Some((
                CheckedClientExpression::FieldPath {
                    root: parameter.id,
                    fields,
                    location: location(input.logical_path, span),
                },
                expression_type,
            ))
        }
        ClientExpression::Unary(unary) => {
            let (checked_expression, expression_type) = check_client_expression(
                &unary.expression,
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
            let required = match unary.operator {
                orna_syntax::ClientUnaryOperator::Plus
                | orna_syntax::ClientUnaryOperator::Minus => StandardScalar::Integer,
                orna_syntax::ClientUnaryOperator::Not => StandardScalar::Boolean,
            };
            if control_flow_supported_scalar(expression_type) != Some(required) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT unary {} requires a {} expression",
                        unary.operator.as_str(),
                        match required {
                            StandardScalar::Integer => "INTEGER",
                            StandardScalar::Boolean => "BOOLEAN",
                            _ => "supported scalar",
                        }
                    ),
                    input.logical_path,
                    &unary.span,
                ));
                return None;
            }
            Some((
                CheckedClientExpression::Unary {
                    operator: checked_client_unary_operator(unary.operator),
                    expression: Box::new(checked_expression),
                    location: location(input.logical_path, &unary.span),
                },
                ClientExpressionType {
                    semantic_type: SemanticType::scalar(required),
                    standard_value_type: expression_type.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                },
            ))
        }
        ClientExpression::Binary(binary) => {
            let (left_checked, left_type) = check_client_expression(
                &binary.left,
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
            let (right_checked, right_type) = check_client_expression(
                &binary.right,
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
            let operator = binary.operator;
            let valid = match operator {
                orna_syntax::ClientBinaryOperator::Add
                | orna_syntax::ClientBinaryOperator::Subtract
                | orna_syntax::ClientBinaryOperator::Multiply
                | orna_syntax::ClientBinaryOperator::Divide
                | orna_syntax::ClientBinaryOperator::Modulo => {
                    control_flow_supported_scalar(left_type) == Some(StandardScalar::Integer)
                        && control_flow_supported_scalar(right_type)
                            == Some(StandardScalar::Integer)
                }
                orna_syntax::ClientBinaryOperator::And | orna_syntax::ClientBinaryOperator::Or => {
                    control_flow_supported_scalar(left_type) == Some(StandardScalar::Boolean)
                        && control_flow_supported_scalar(right_type)
                            == Some(StandardScalar::Boolean)
                }
                orna_syntax::ClientBinaryOperator::Equal
                | orna_syntax::ClientBinaryOperator::NotEqual
                | orna_syntax::ClientBinaryOperator::LessThan
                | orna_syntax::ClientBinaryOperator::GreaterThan
                | orna_syntax::ClientBinaryOperator::LessThanOrEqual
                | orna_syntax::ClientBinaryOperator::GreaterThanOrEqual => {
                    control_flow_types_match(left_type, right_type)
                }
            };
            if !valid {
                let message = match operator {
                    orna_syntax::ClientBinaryOperator::Add
                    | orna_syntax::ClientBinaryOperator::Subtract
                    | orna_syntax::ClientBinaryOperator::Multiply
                    | orna_syntax::ClientBinaryOperator::Divide
                    | orna_syntax::ClientBinaryOperator::Modulo => {
                        format!(
                            "CLIENT arithmetic operator {} requires INTEGER operands",
                            operator.as_str()
                        )
                    }
                    orna_syntax::ClientBinaryOperator::And
                    | orna_syntax::ClientBinaryOperator::Or => {
                        format!(
                            "CLIENT Boolean operator {} requires BOOLEAN operands",
                            operator.as_str()
                        )
                    }
                    _ => format!(
                        "CLIENT comparison {} requires operands of the same INTEGER, BOOLEAN, or TEXT type",
                        operator.as_str()
                    ),
                };
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    message,
                    input.logical_path,
                    &binary.span,
                ));
                return None;
            }
            let comparison = matches!(
                operator,
                orna_syntax::ClientBinaryOperator::Equal
                    | orna_syntax::ClientBinaryOperator::NotEqual
                    | orna_syntax::ClientBinaryOperator::LessThan
                    | orna_syntax::ClientBinaryOperator::GreaterThan
                    | orna_syntax::ClientBinaryOperator::LessThanOrEqual
                    | orna_syntax::ClientBinaryOperator::GreaterThanOrEqual
            );
            let result_scalar = if comparison
                || matches!(
                    operator,
                    orna_syntax::ClientBinaryOperator::And | orna_syntax::ClientBinaryOperator::Or
                ) {
                StandardScalar::Boolean
            } else {
                StandardScalar::Integer
            };
            Some((
                CheckedClientExpression::Binary {
                    operator: checked_client_binary_operator(operator),
                    left: Box::new(left_checked),
                    right: Box::new(right_checked),
                    location: location(input.logical_path, &binary.span),
                },
                ClientExpressionType {
                    semantic_type: SemanticType::scalar(result_scalar),
                    standard_value_type: standard_scalar_type_id(standard, result_scalar),
                    result_shape: ClientExpressionResultShape::Value,
                },
            ))
        }
        ClientExpression::Parenthesized { expression, span } => {
            let (checked_expression, expression_type) = check_client_expression(
                expression,
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
            Some((
                CheckedClientExpression::Parenthesized {
                    expression: Box::new(checked_expression),
                    location: location(input.logical_path, span),
                },
                expression_type,
            ))
        }

        ClientExpression::Concat { left, right, span } => {
            let (left_checked, left_type) = check_client_expression(
                left,
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
            let (right_checked, right_type) = check_client_expression(
                right,
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
            let text = SemanticType::scalar(StandardScalar::CharacterLargeObject);
            if left_type.semantic_type != text
                || right_type.semantic_type != text
                || left_type.result_shape != ClientExpressionResultShape::Value
                || right_type.result_shape != ClientExpressionResultShape::Value
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "CLIENT concatenation requires TEXT expressions",
                    input.logical_path,
                    span,
                ));
                return None;
            }
            Some((
                CheckedClientExpression::Concat {
                    left: Box::new(left_checked),
                    right: Box::new(right_checked),
                    location: location(input.logical_path, span),
                },
                ClientExpressionType {
                    semantic_type: text,
                    result_shape: ClientExpressionResultShape::Value,
                    standard_value_type: left_type.standard_value_type,
                },
            ))
        }
        ClientExpression::Call {
            callee,
            arguments,
            span,
        } => {
            let name = semantic_name(callee);
            if let Some(system_function) = orna_core::system::system_function_by_name(&name)
                && system_function.kind()
                    == orna_core::system::SystemFunctionKind::SourceIntrospection
            {
                if !arguments.is_empty() {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "sys.source.current takes no arguments",
                        input.logical_path,
                        span,
                    ));
                    return None;
                }
                return Some((
                    CheckedClientExpression::SourceIntrospection {
                        location: location(input.logical_path, span),
                    },
                    ClientExpressionType {
                        semantic_type: SemanticType::Named(CheckedTypeId::Existing(
                            SYS_SOURCE_FUNCTION_TYPE_ID,
                        )),
                        standard_value_type: None,
                        result_shape: ClientExpressionResultShape::Value,
                    },
                ));
            }
            if name
                == QualifiedSemanticName::new(["std", "cli", "input"])
                    .expect("std.cli.input is valid")
            {
                if !arguments.is_empty() {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "std.cli.input takes no arguments",
                        input.logical_path,
                        span,
                    ));
                    return None;
                }
                let text = SemanticType::scalar(StandardScalar::CharacterLargeObject);
                return Some((
                    CheckedClientExpression::Input {
                        location: location(input.logical_path, span),
                    },
                    ClientExpressionType {
                        semantic_type: text,
                        standard_value_type: standard_scalar_type_id(
                            standard,
                            StandardScalar::CharacterLargeObject,
                        ),
                        result_shape: ClientExpressionResultShape::Value,
                    },
                ));
            }
            if name
                == QualifiedSemanticName::new(["std", "cli", "evaluate"])
                    .expect("std.cli.evaluate is valid")
            {
                if arguments.len() != 1 {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "std.cli.evaluate requires one command expression",
                        input.logical_path,
                        span,
                    ));
                    return None;
                }
                let (command, command_type) = check_client_expression(
                    &arguments[0].value,
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
                let text = SemanticType::scalar(StandardScalar::CharacterLargeObject);
                if command_type.semantic_type != text
                    || command_type.result_shape != ClientExpressionResultShape::Value
                {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        "std.cli.evaluate requires a TEXT command expression",
                        input.logical_path,
                        arguments[0].value.span(),
                    ));
                    return None;
                }
                let ui_type =
                    client_expression_type_from_core(ResolvedType::value(STD_UI_TYPE_ID), standard)
                        .expect("std.ui.UI is representable as a CLIENT result");
                return Some((
                    CheckedClientExpression::Evaluate {
                        expression: Box::new(command),
                        location: location(input.logical_path, span),
                    },
                    ui_type,
                ));
            }
            if let Some(inspect) = check_inspect_call(
                expression,
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
            ) {
                return inspect;
            }
            if name
                == QualifiedSemanticName::new(["std", "action", "call"])
                    .expect("std.action.call is valid")
            {
                return check_action_constructor(
                    expression,
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
                );
            }
            if name
                == QualifiedSemanticName::new(["std", "action", "sequence"])
                    .expect("std.action.sequence is valid")
                || name
                    == QualifiedSemanticName::new(["std", "action", "parallel"])
                        .expect("std.action.parallel is valid")
            {
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    format!("unknown CLIENT function {name}"),
                    input.logical_path,
                    span,
                ));
                return None;
            }
            if resource_constructor_kind(&name).is_some() {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "resource constructors are only valid as an AWAIT operand",
                    input.logical_path,
                    span,
                ));
                return None;
            }
            let Some(target) = targets.get(&name) else {
                let message = if server_names.contains(&name)
                    || base
                        .function_by_name(&name)
                        .is_some_and(|function| function.domain() == FunctionDomain::Server)
                {
                    format!("CLIENT expression cannot call SERVER function {name}")
                } else {
                    format!("unknown CLIENT function {name}")
                };
                diagnostics.push(diagnostic(
                    DiagnosticCode::UnknownQualifiedName,
                    message,
                    input.logical_path,
                    span,
                ));
                return None;
            };
            if target.return_type.result_shape == ClientExpressionResultShape::OptionalList {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT STREAM function {name} cannot be used as an expression operand"
                    ),
                    input.logical_path,
                    span,
                ));
                return None;
            }
            used_capabilities.insert(name.clone());
            let mut bound = vec![false; target.parameters.len()];
            let mut positional = 0usize;
            let mut checked_argument_slots = vec![None; target.parameters.len()];
            for argument in arguments {
                let parameter_index = if let Some(name) = &argument.name {
                    let parameter_name = semantic_part(name);
                    let Some(index) = target
                        .parameters
                        .iter()
                        .position(|parameter| parameter.name == parameter_name)
                    else {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::UnknownQualifiedName,
                            format!("unknown CLIENT argument {parameter_name}"),
                            input.logical_path,
                            &input.declaration_span,
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
                            format!("too many arguments for CLIENT function {name}"),
                            input.logical_path,
                            &input.declaration_span,
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
                            "duplicate argument for CLIENT parameter {}",
                            target.parameters[parameter_index].name
                        ),
                        input.logical_path,
                        &input.declaration_span,
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
                            "argument does not match CLIENT parameter {}",
                            parameter.name
                        ),
                        input.logical_path,
                        &input.declaration_span,
                    ));
                    return None;
                }
                bound[parameter_index] = true;
                checked_argument_slots[parameter_index] = Some((parameter.id, checked));
            }
            if bound.iter().any(|bound| !bound) {
                diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!("missing argument for CLIENT function {name}"),
                    input.logical_path,
                    &input.declaration_span,
                ));
                return None;
            }
            let checked_arguments = checked_argument_slots
                .into_iter()
                .map(|argument| argument.expect("checked CLIENT argument slot is bound"))
                .collect::<Vec<_>>();
            references.push(CheckedDefinitionReference {
                target: CheckedDefinitionReferenceTarget::Function(target.id),
                kind: DefinitionReferenceKind::FunctionCall,
                location: location(input.logical_path, span),
            });
            Some((
                CheckedClientExpression::Call {
                    function: target.id,
                    arguments: checked_arguments,
                    location: location(input.logical_path, span),
                },
                target.return_type,
            ))
        }
    }
}

impl CheckedClientStateSlot {
    pub(crate) fn location(&self) -> &SourceLocation {
        &self.location
    }
}

fn checked_state_slot_id(function: CheckedFunctionId, name: &str) -> CheckedStateSlotId {
    let mut payload = function.to_string().into_bytes();
    payload.push(0);
    payload.extend_from_slice(&(name.len() as u32).to_be_bytes());
    payload.extend_from_slice(name.as_bytes());
    let digest = artifact_payload_digest(&payload).expect("state-slot identity payload is bounded");
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.to_bytes()[..16]);
    let id = StateSlotId::from_bytes(bytes);
    if function.existing().is_some() {
        CheckedStateSlotId::Existing(id)
    } else {
        CheckedStateSlotId::Provisional(id)
    }
}
pub(crate) fn durable_state_slot_id(function: FunctionId, name: &str) -> StateSlotId {
    let mut payload = function.to_string().into_bytes();
    payload.push(0);
    payload.extend_from_slice(&(name.len() as u32).to_be_bytes());

    payload.extend_from_slice(name.as_bytes());
    let digest = artifact_payload_digest(&payload).expect("state-slot identity payload is bounded");
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.to_bytes()[..16]);
    StateSlotId::from_bytes(bytes)
}
fn client_body_requires_control_flow(body: &orna_syntax::ClientFunctionBody) -> bool {
    match body {
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => false,
        orna_syntax::ClientFunctionBody::Expression { expression }
        | orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            client_expression_requires_control_flow(expression)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => {
            block
                .locals
                .iter()
                .any(|local| client_expression_requires_control_flow(&local.expression))
                || block
                    .return_expression
                    .as_ref()
                    .is_some_and(client_expression_requires_control_flow)
                || block
                    .statements
                    .iter()
                    .any(client_statement_requires_control_flow)
        }
        _ => false,
    }
}

fn client_statement_requires_control_flow(
    statement: &orna_syntax::ClientProceduralStatement,
) -> bool {
    match statement {
        orna_syntax::ClientProceduralStatement::Let(statement) => {
            client_expression_requires_control_flow(&statement.expression)
        }
        orna_syntax::ClientProceduralStatement::Assignment(statement) => {
            client_expression_requires_control_flow(&statement.expression)
        }
        orna_syntax::ClientProceduralStatement::Return(_) => true,
        orna_syntax::ClientProceduralStatement::If(statement) => {
            client_expression_requires_control_flow(&statement.condition)
                || statement
                    .then_statements
                    .iter()
                    .any(client_statement_requires_control_flow)
                || statement.elsif_branches.iter().any(|branch| {
                    client_expression_requires_control_flow(&branch.condition)
                        || branch
                            .statements
                            .iter()
                            .any(client_statement_requires_control_flow)
                })
                || statement
                    .else_statements
                    .as_ref()
                    .is_some_and(|statements| {
                        statements
                            .iter()
                            .any(client_statement_requires_control_flow)
                    })
        }
        orna_syntax::ClientProceduralStatement::While(statement) => {
            client_expression_requires_control_flow(&statement.condition)
                || statement
                    .body
                    .iter()
                    .any(client_statement_requires_control_flow)
        }
    }
}

fn client_expression_requires_control_flow(expression: &ClientExpression) -> bool {
    match expression {
        ClientExpression::Unary(_) | ClientExpression::Binary(_) => true,
        ClientExpression::Parenthesized { expression, .. }
        | ClientExpression::Await { expression, .. } => {
            client_expression_requires_control_flow(expression)
        }
        ClientExpression::Call { arguments, .. } => arguments
            .iter()
            .any(|argument| client_expression_requires_control_flow(&argument.value)),
        ClientExpression::Concat { left, right, .. } => {
            client_expression_requires_control_flow(left)
                || client_expression_requires_control_flow(right)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => false,
    }
}

fn validate_client_await_positions(
    expression: &ClientExpression,
    allow_await: bool,
    input: &ResolvedClientFunctionInput<'_>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    match expression {
        ClientExpression::Await { expression, span } => {
            if !allow_await {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "AWAIT is only valid as the CLIENT body return expression",
                    input.logical_path,
                    span,
                ));
            }
            // The resource constructor is a non-blocking value operation; an
            // AWAIT operand cannot itself contain another suspension.
            validate_client_await_positions(expression, false, input, diagnostics);
        }
        ClientExpression::Call { arguments, .. } => {
            for argument in arguments {
                validate_client_await_positions(&argument.value, false, input, diagnostics);
            }
        }
        ClientExpression::Concat { left, right, .. } => {
            validate_client_await_positions(left, false, input, diagnostics);
            validate_client_await_positions(right, false, input, diagnostics);
        }
        ClientExpression::Binary(binary) => {
            validate_client_await_positions(&binary.left, false, input, diagnostics);
            validate_client_await_positions(&binary.right, false, input, diagnostics);
        }
        ClientExpression::Unary(unary) => {
            validate_client_await_positions(&unary.expression, false, input, diagnostics);
        }
        ClientExpression::Parenthesized { expression, .. } => {
            validate_client_await_positions(expression, false, input, diagnostics);
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => {}
    }
}

fn unsupported_client_state_reference(
    expression: &ClientExpression,
    input: &ResolvedClientFunctionInput<'_>,
    state_names: &HashSet<String>,
) -> Option<SourceSpan> {
    let parameter_name = |name: &orna_syntax::NamePart| semantic_part(name);
    let is_state = |name: &orna_syntax::NamePart| {
        let name = parameter_name(name);
        state_names.contains(&name)
            && !input
                .parameters
                .iter()
                .any(|parameter| parameter.name == name)
    };
    match expression {
        ClientExpression::ParameterRead { parameter } if is_state(parameter) => {
            Some(parameter.span.clone())
        }
        ClientExpression::FieldPath { root, .. } if is_state(root) => Some(root.span.clone()),

        ClientExpression::Await { expression, .. } => {
            unsupported_client_state_reference(expression, input, state_names)
        }
        ClientExpression::Call { arguments, .. } => arguments.iter().find_map(|argument| {
            unsupported_client_state_reference(&argument.value, input, state_names)
        }),

        ClientExpression::Concat { left, right, .. } => {
            unsupported_client_state_reference(left, input, state_names)
                .or_else(|| unsupported_client_state_reference(right, input, state_names))
        }
        ClientExpression::Binary(binary) => {
            unsupported_client_state_reference(&binary.left, input, state_names)
                .or_else(|| unsupported_client_state_reference(&binary.right, input, state_names))
        }
        ClientExpression::Unary(unary) => {
            unsupported_client_state_reference(&unary.expression, input, state_names)
        }
        ClientExpression::Parenthesized { expression, .. } => {
            unsupported_client_state_reference(expression, input, state_names)
        }
        ClientExpression::StringLiteral { .. }
        | ClientExpression::IntegerLiteral { .. }
        | ClientExpression::BooleanLiteral { .. }
        | ClientExpression::ParameterRead { .. }
        | ClientExpression::LocalRead { .. }
        | ClientExpression::FieldPath { .. } => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_client_functions(
    inputs: &[ResolvedClientFunctionInput<'_>],
    server_inputs: &[ResolvedServerFunctionInput<'_>],
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    server_names: &[QualifiedSemanticName],
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    base: &CatalogueSnapshot,
    diagnostics: &mut Vec<CompilerDiagnostic>,
    standard: Option<&CheckedStandardLibrary>,
    uses: &mut Vec<CheckedApplicationTypeUse>,
) -> Vec<CheckedClientFunction> {
    let targets = client_expression_targets(inputs, base, standard);
    let action_targets = client_action_targets(inputs, server_inputs, base, standard);
    inputs
        .iter()
        .filter_map(|input| {
            for capability in input.capabilities {
                validate_client_capability(
                    capability,
                    input
                        .parameters
                        .iter()
                        .map(|parameter| parameter.name.as_str()),
                    input.logical_path,
                    &input.declaration_span,
                    diagnostics,
                );
            }
            let (body, body_type, body_location, mut references) =
                if input.control_flow_required {
                    check_client_control_flow_body(
                        input,
                        submitted_ids,
                        &targets,
                        &action_targets,
                        resource_targets,
                        query_catalogue,
                        base,
                        server_names,
                        standard,
                        diagnostics,
                    )?
                } else if let Some((value, body_source)) = input.body.as_boolean_literal() {
                    if !input.capabilities.is_empty() {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::CapabilityRequirement,
                            "accepted CLIENT function bodies must not declare capabilities",
                            input.logical_path,
                            &input.declaration_span,
                        ));
                        return None;
                    }
                    (
                        CheckedClientFunctionBody::BooleanLiteral {
                            value,
                            location: location(input.logical_path, &body_source.span),
                        },
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                        location(input.logical_path, &body_source.span),
                        Vec::new(),
                    )
                } else if let Some(expression) = input.body.as_expression().or_else(|| {
                    input
                        .body
                        .as_state_block()
                        .filter(|block| {
                            block.states.is_empty()
                                && block.locals.is_empty()
                                && block.statements.is_empty()
                        })
                        .and_then(|block| block.return_expression.as_ref())
                }) {
                    if matches!(
                        input.body,
                        orna_syntax::ClientFunctionBody::Expression { .. }
                    ) && input.return_type
                        == SemanticType::Named(CheckedTypeId::Existing(STD_UI_TYPE_ID))
                    {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT UI functions must use explicit RETURN instead of AS expression",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    let diagnostics_before = diagnostics.len();
                    validate_client_await_positions(
                        expression,
                        !matches!(input.body, orna_syntax::ClientFunctionBody::Expression { .. }),
                        input,
                        diagnostics,
                    );
                    if diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let mut references = Vec::new();
                    let mut used_capabilities = HashSet::new();
                    let locals = ClientLocalEnvironment::new();
                    let (checked, expression_type) = check_client_expression(
                        expression,
                        input,
                        &targets,
                        &action_targets,
                        resource_targets,
                        query_catalogue,
                        base,
                        server_names,
                        standard,
                        diagnostics,
                        &mut references,
                        &mut used_capabilities,
                        &locals,
                    )?;
                    if !client_expression_types_compatible(
                        expression_type,
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                    ) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "this CLIENT function must return the declared value type",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    for capability in input.capabilities {
                        let capability_name = semantic_name(&capability.name);
                        if !used_capabilities.contains(&capability_name) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::CapabilityRequirement,
                                format!(
                                    "declared CLIENT capability {capability_name} is not exercised"
                                ),
                                input.logical_path,
                                &input.declaration_span,
                            ));
                            return None;
                        }
                    }
                    (
                        CheckedClientFunctionBody::Expression {
                            expression: checked,
                        },
                        expression_type,
                        location(input.logical_path, expression.span()),
                        references,
                    )
                } else if let Some(block) = input.body.as_state_block().filter(|block| block.states.is_empty()) {
    let mut references = Vec::new();
    let mut used_capabilities = HashSet::new();
    let mut locals = ClientLocalEnvironment::new();
    let mut checked_locals = Vec::new();
    let mut statements = Vec::new();
    let mut next_ordinal = 0_u32;

    for local in &block.locals {
        let local_name = semantic_part(&local.name);
        if locals.contains_key(&local_name) {
            diagnostics.push(diagnostic(DiagnosticCode::DuplicateDefinition, format!("duplicate CLIENT local definition {local_name} in {}", input.name), input.logical_path, &local.name.span));
            return None;
        }
        let diagnostics_before = diagnostics.len();
        validate_client_await_positions(&local.expression, true, input, diagnostics);
        if diagnostics.len() != diagnostics_before { return None; }
        let direct_resource = matches!(
            &local.expression,
            ClientExpression::Call { callee, .. }
                if resource_constructor_kind(&semantic_name(callee)).is_some()
        );
        let (checked, expression_type, kind) =
            if client_local_resource_family(&local.type_source).is_some() || direct_resource {
                let Some((kind, descriptor)) = client_local_resource_type(&local.type_source) else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"
                        ),
                        input.logical_path,
                        &local.type_source.span,
                    ));
                    return None;
                };
                if reject_deferred_client_resource_descriptor(
                    descriptor.as_ref(),
                    &local_name,
                    input,
                    &local.type_source,
                    diagnostics,
                ) {
                    return None;
                }
                let expected_type = match descriptor.as_ref() {
                    Some(descriptor) => {
                        let resolved = resolve_application_type_with_named_standard(
                            descriptor,
                            submitted_ids,
                            input.logical_path,
                            diagnostics,
                            standard,
                            true,
                        )?;
                        Some(ClientExpressionType {
                            semantic_type: resolved.semantic_type,
                            standard_value_type: resolved.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        })
                    }
                    None => None,
                };
                let (checked, expression_type) = check_resource_constructor(
                    &local.expression,
                    input,
                    &targets,
                    &action_targets,
                    resource_targets,
                    query_catalogue,
                    base,
                    server_names,
                    standard,
                    diagnostics,
                    &mut references,
                    &mut used_capabilities,
                    &locals,
                )?;
                let actual_kind = match &checked {
                    CheckedClientExpression::Resource { operation } => operation.kind,
                    _ => unreachable!("resource constructor checker returns a resource"),
                };
                if actual_kind != kind {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!("CLIENT local {local_name} type does not match its resource constructor"),
                        input.logical_path,
                        &local.type_source.span,
                    ));
                    return None;
                }
                if let Some(expected_type) = expected_type
                    && !client_expression_types_compatible(expression_type, expected_type)
                {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                        ),
                        input.logical_path,
                        &local.type_source.span,
                    ));
                    return None;
                }
                (checked, expression_type, CheckedClientLocalKind::Resource(kind))
            } else {
            let (checked, expression_type) = check_client_expression(&local.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
            if !client_expression_type_is_evaluable(expression_type, base, standard) {
                diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, "this CLIENT local type is not supported by the local evaluator", input.logical_path, &local.span));
                return None;
            }
            let Some(specification) = client_type_specification_from_source(&local.type_source) else {
                diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("unsupported CLIENT local type for {local_name}"), input.logical_path, &local.type_source.span));
                return None;
            };
            let resolved = resolve_application_type_with_named_standard(&specification, submitted_ids, input.logical_path, diagnostics, standard, true)?;
            let expected = ClientExpressionType { semantic_type: resolved.semantic_type, standard_value_type: resolved.standard_value_type, result_shape: ClientExpressionResultShape::Value };
            if !client_expression_types_compatible(expression_type, expected) {
                diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} initializer does not match its declared type"), input.logical_path, &local.span));
                return None;
            }
            (checked, expression_type, CheckedClientLocalKind::Value)
        };
        let ordinal = next_ordinal; next_ordinal += 1;
        checked_locals.push(CheckedClientLocal { ordinal, name: local_name.clone(), semantic_type: expression_type.semantic_type, standard_value_type: expression_type.standard_value_type, kind, location: location(input.logical_path, &local.span) });
        locals.insert(local_name, ClientLocalBinding { checked: checked.clone(), expression_type, ordinal: Some(ordinal), kind });
        statements.push(CheckedClientStatement::Let { local: ordinal, expression: checked });
    }

    for statement in &block.statements {
        match statement {
            orna_syntax::ClientProceduralStatement::Let(statement) => {
                let local_name = semantic_part(&statement.name);
                if locals.contains_key(&local_name) {
                    diagnostics.push(diagnostic(DiagnosticCode::DuplicateDefinition, format!("duplicate CLIENT local definition {local_name} in {}", input.name), input.logical_path, &statement.name.span));
                    return None;
                }
                let diagnostics_before = diagnostics.len();
                validate_client_await_positions(&statement.expression, true, input, diagnostics);
                if diagnostics.len() != diagnostics_before { return None; }
                let declared_resource_family = statement
                    .type_source
                    .as_ref()
                    .and_then(client_local_resource_family);
                let direct_resource = matches!(&statement.expression, ClientExpression::Call { callee, .. } if resource_constructor_kind(&semantic_name(callee)).is_some());
                let (checked, expression_type, kind) = if declared_resource_family.is_some() || direct_resource {
                    if !direct_resource {
                        diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} resource type requires a resource constructor"), input.logical_path, &statement.span));
                        return None;
                    }
                    let (checked, expression_type) = check_resource_constructor(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
                    let actual_kind = match &checked {
                        CheckedClientExpression::Resource { operation } => operation.kind,
                        _ => unreachable!("resource constructor checker returns a resource"),
                    };
                    if let Some(source) = &statement.type_source {
                        let Some((expected_kind, descriptor)) = client_local_resource_type(source) else {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"), input.logical_path, &source.span));
                            return None;
                        };
                        if reject_deferred_client_resource_descriptor(
                            descriptor.as_ref(),
                            &local_name,
                            input,
                            source,
                            diagnostics,
                        ) {
                            return None;
                        }
                        let expected_type = match descriptor.as_ref() {
                            Some(descriptor) => {
                                let resolved = resolve_application_type_with_named_standard(
                                    descriptor,
                                    submitted_ids,
                                    input.logical_path,
                                    diagnostics,
                                    standard,
                                    true,
                                )?;
                                Some(ClientExpressionType {
                                    semantic_type: resolved.semantic_type,
                                    standard_value_type: resolved.standard_value_type,
                                    result_shape: ClientExpressionResultShape::Value,
                                })
                            }
                            None => None,
                        };
                        if actual_kind != expected_kind {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} type does not match its resource constructor"), input.logical_path, &source.span));
                            return None;
                        }
                        if let Some(expected_type) = expected_type
                            && !client_expression_types_compatible(expression_type, expected_type)
                        {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} descriptor does not match its SERVER resource result"), input.logical_path, &source.span));
                            return None;
                        }
                    }
                    (checked, expression_type, CheckedClientLocalKind::Resource(actual_kind))
                } else {
                    let (checked, expression_type) = check_client_expression(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
                    if !client_expression_type_is_evaluable(expression_type, base, standard) {
                        diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, "this CLIENT local type is not supported by the local evaluator", input.logical_path, &statement.span));
                        return None;
                    }
                    if let Some(source) = &statement.type_source {
                        let Some(specification) = client_type_specification_from_source(source) else {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("unsupported CLIENT local type for {local_name}"), input.logical_path, &source.span));
                            return None;
                        };
                        let resolved = resolve_application_type_with_named_standard(&specification, submitted_ids, input.logical_path, diagnostics, standard, true)?;
                        let expected = ClientExpressionType { semantic_type: resolved.semantic_type, standard_value_type: resolved.standard_value_type, result_shape: ClientExpressionResultShape::Value };
                        if !client_expression_types_compatible(expression_type, expected) {
                            diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT local {local_name} initializer does not match its declared type"), input.logical_path, &statement.span));
                            return None;
                        }
                    }
                    (checked, expression_type, CheckedClientLocalKind::Value)
                };
                let ordinal = next_ordinal; next_ordinal += 1;
                checked_locals.push(CheckedClientLocal { ordinal, name: local_name.clone(), semantic_type: expression_type.semantic_type, standard_value_type: expression_type.standard_value_type, kind, location: location(input.logical_path, &statement.span) });
                locals.insert(local_name, ClientLocalBinding { checked: checked.clone(), expression_type, ordinal: Some(ordinal), kind });
                statements.push(CheckedClientStatement::Let { local: ordinal, expression: checked });
            }
            orna_syntax::ClientProceduralStatement::Assignment(statement) => {
                let local_name = semantic_part(&statement.target);
                let Some(binding) = locals.get(&local_name).cloned() else {
                    diagnostics.push(diagnostic(DiagnosticCode::UnknownQualifiedName, format!("unknown CLIENT local {local_name}"), input.logical_path, &statement.target.span));
                    return None;
                };
                let diagnostics_before = diagnostics.len();
                validate_client_await_positions(&statement.expression, true, input, diagnostics);
                if diagnostics.len() != diagnostics_before { return None; }
                let direct_resource = matches!(&statement.expression, ClientExpression::Call { callee, .. } if resource_constructor_kind(&semantic_name(callee)).is_some());
                let (checked, expression_type) = if matches!(binding.kind, CheckedClientLocalKind::Resource(_)) && direct_resource {
                    check_resource_constructor(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?
                } else {
                    check_client_expression(&statement.expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?
                };
                if !client_expression_types_compatible(expression_type, binding.expression_type) || (matches!(binding.kind, CheckedClientLocalKind::Resource(_)) != matches!(checked, CheckedClientExpression::Resource { .. })) {
                    diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, format!("CLIENT assignment to local {local_name} does not match its declared type"), input.logical_path, &statement.span));
                    return None;
                }
                statements.push(CheckedClientStatement::Assignment { local: binding.ordinal.expect("procedural local has ordinal"), expression: checked.clone() });
                if let Some(binding) = locals.get_mut(&local_name) { binding.checked = checked; }
            }
            orna_syntax::ClientProceduralStatement::Return(_)
            | orna_syntax::ClientProceduralStatement::If(_)
            | orna_syntax::ClientProceduralStatement::While(_) => {
                diagnostics.push(diagnostic(
                    DiagnosticCode::DomainIncompatible,
                    "CLIENT procedural statements require the control-flow plan",
                    input.logical_path,
                    &input.declaration_span,
                ));
                return None;
            }

        }
    }
    let Some(expression) = block.return_expression.as_ref() else {
        diagnostics.push(diagnostic(DiagnosticCode::DomainIncompatible, "CLIENT procedural bodies must return an expression", input.logical_path, &block.span));
        return None;
    };
    let diagnostics_before = diagnostics.len();
    validate_client_await_positions(expression, true, input, diagnostics);
    if diagnostics.len() != diagnostics_before { return None; }
    let (checked_return, return_type) = check_client_expression(expression, input, &targets, &action_targets, resource_targets, query_catalogue, base, server_names, standard, diagnostics, &mut references, &mut used_capabilities, &locals)?;
    if !client_expression_types_compatible(return_type, ClientExpressionType { semantic_type: input.return_type, standard_value_type: input.standard_value_type, result_shape: input.result_shape }) {
        diagnostics.push(diagnostic(DiagnosticCode::TypeMismatch, "this CLIENT function must return the declared value type", input.logical_path, expression.span()));
        return None;
    }
    for capability in input.capabilities {
        let capability_name = semantic_name(&capability.name);
        if !used_capabilities.contains(&capability_name) {
            diagnostics.push(diagnostic(DiagnosticCode::CapabilityRequirement, format!("declared CLIENT capability {capability_name} is not exercised"), input.logical_path, &input.declaration_span));
            return None;
        }
    }
    (CheckedClientFunctionBody::Procedural { locals: checked_locals, statements, return_expression: checked_return }, return_type, location(input.logical_path, expression.span()), references)
} else if let Some(block) = input.body.as_state_block() {
                    if !block.locals.is_empty() || !block.statements.is_empty() {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks do not support procedural statements",
                            input.logical_path,
                            &block.span,
                        ));
                        return None;
                    }
                    let mut references = Vec::new();
                    let mut used_capabilities = HashSet::new();
                    let mut state_names = HashSet::new();
                    let mut states = Vec::with_capacity(block.states.len());
                    for (ordinal, state) in block.states.iter().enumerate() {
                        let state_name = semantic_part(&state.name);
                        if !state_names.insert(state_name.clone()) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::DuplicateDefinition,
                                format!("duplicate state definition {state_name} in {}", input.name),
                                input.logical_path,
                                &state.name.span,
                            ));
                            return None;
                        }
                        let resolved = resolve_application_type_with_named_standard(
                            &state.type_specification,
                            submitted_ids,
                            input.logical_path,
                            diagnostics,
                            standard,
                            true,
                        )?;
                        record_standard_type_use(
                            uses,
                            standard,
                            CheckedTypeUseKind::State {
                                owner: input.id,
                                ordinal: ordinal as u32,
                            },
                            resolved,
                            type_use_location(&state.type_specification, input.logical_path),
                        );
                        if let SemanticType::Named(CheckedTypeId::Existing(type_id)) =
                            resolved.semantic_type
                        {
                            if is_sealed_inspect_type_id(type_id) {
                                diagnostics.push(diagnostic(
                                    DiagnosticCode::DomainIncompatible,
                                    "sealed sys.inspect carriers are transient and cannot be stored in CLIENT state",
                                    input.logical_path,
                                    state.type_specification.span(),
                                ));
                                return None;
                            }
                            if standard.is_some_and(|standard| {
                                standard.value_types().iter().any(|value_type| {
                                    value_type.id() == type_id
                                        && value_type.kind() == ValueTypeKind::Opaque
                                })
                            }) {
                                diagnostics.push(diagnostic(
                                    DiagnosticCode::DomainIncompatible,
                                    "opaque CLIENT values are transient and cannot be stored in state",
                                    input.logical_path,
                                    state.type_specification.span(),
                                ));
                                return None;
                            }
                        }
                        let state_type = ClientExpressionType {
                            semantic_type: resolved.semantic_type,
                            standard_value_type: resolved.standard_value_type,
                            result_shape: ClientExpressionResultShape::Value,
                        };
                        if !client_expression_type_is_evaluable(state_type, base, standard) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "this CLIENT state type is not supported by the local evaluator",
                                input.logical_path,
                                state.type_specification.span(),
                            ));
                            return None;
                        }
                        let default = match &state.default {
                            StateDefault::Unset => CheckedStateDefault::Unset,
                            StateDefault::Null => CheckedStateDefault::Null,
                            StateDefault::Expression(expression) => {
                                let diagnostics_before = diagnostics.len();
                                validate_client_await_positions(expression, false, input, diagnostics);
                                if diagnostics.len() != diagnostics_before {
                                    return None;
                                }
                                if client_expression_contains_action(expression) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::DomainIncompatible,
                                        "CLIENT state defaults do not support action expressions",
                                        input.logical_path,
                                        expression.span(),
                                    ));
                                    return None;
                                }
                                if let Some(span) = unsupported_client_state_reference(
                                    expression,
                                    input,
                                    &state_names,
                                ) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::DomainIncompatible,
                                        "CLIENT state references are not supported in expressions",
                                        input.logical_path,
                                        &span,
                                    ));
                                    return None;
                                }
                                let (checked, expression_type) = check_client_expression(
                                    expression,
                                    input,
                                    &targets,
                                    &action_targets,
                                    resource_targets,
                                    query_catalogue,
                                    base,
                                    server_names,
                                    standard,
                                    diagnostics,
                                    &mut references,
                                    &mut used_capabilities,
                                    &ClientLocalEnvironment::new(),
                                )?;
                                if client_expression_contains_inspect(&checked) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::DomainIncompatible,
                                        "CLIENT state defaults do not support Inspector expressions",
                                        input.logical_path,
                                        expression.span(),
                                    ));
                                    return None;
                                }
                                if !client_expression_types_compatible(expression_type, state_type) {
                                    diagnostics.push(diagnostic(
                                        DiagnosticCode::TypeMismatch,
                                        "this CLIENT state default must have the declared state type",
                                        input.logical_path,
                                        expression.span(),
                                    ));
                                    return None;
                                }
                                CheckedStateDefault::Expression(checked)
                            }
                        };
                        let scope = match state.scope {
                            StateScope::Local => CheckedStateScope::Local,
                            StateScope::Session => CheckedStateScope::Session,
                            StateScope::User => CheckedStateScope::User,
                        };
                        states.push(CheckedClientStateSlot {
                            id: checked_state_slot_id(input.id, &state_name),
                            name: state_name,
                            ordinal: ordinal as u32,
                            semantic_type: resolved.semantic_type,
                            standard_value_type: resolved.standard_value_type,
                            scope,
                            default,
                            location: location(input.logical_path, &state.span),
                        });
                    }
                    let mut locals = ClientLocalEnvironment::new();
                    for local in &block.locals {
                        let local_name = semantic_part(&local.name);
                        if locals.contains_key(&local_name) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::DuplicateDefinition,
                                format!("duplicate CLIENT local definition {local_name} in {}", input.name),
                                input.logical_path,
                                &local.name.span,
                            ));
                            return None;
                        }
                        let diagnostics_before = diagnostics.len();
                        validate_client_await_positions(&local.expression, false, input, diagnostics);
                        if diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        if let Some(span) = unsupported_client_state_reference(
                            &local.expression,
                            input,
                            &state_names,
                        ) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::DomainIncompatible,
                                "CLIENT state references are not supported in expressions",
                                input.logical_path,
                                &span,
                            ));
                            return None;
                        }
                        let (checked, expression_type) = check_resource_constructor(
                            &local.expression,
                            input,
                            &targets,
                            &action_targets,
                            resource_targets,
                            query_catalogue,
                            base,
                            server_names,
                            standard,
                            diagnostics,
                            &mut references,
                            &mut used_capabilities,
                            &locals,
                        )?;
                        let Some((expected_kind, descriptor)) = client_local_resource_type(&local.type_source) else {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!("CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"),
                                input.logical_path,
                                &local.type_source.span,
                            ));
                            return None;
                        };
                        if reject_deferred_client_resource_descriptor(
                            descriptor.as_ref(),
                            &local_name,
                            input,
                            &local.type_source,
                            diagnostics,
                        ) {
                            return None;
                        }
                        let expected_type = match descriptor.as_ref() {
                            Some(descriptor) => {
                                let resolved = resolve_application_type_with_named_standard(
                                    descriptor,
                                    submitted_ids,
                                    input.logical_path,
                                    diagnostics,
                                    standard,
                                    true,
                                )?;
                                Some(ClientExpressionType {
                                    semantic_type: resolved.semantic_type,
                                    standard_value_type: resolved.standard_value_type,
                                    result_shape: ClientExpressionResultShape::Value,
                                })
                            }
                            None => None,
                        };
                        let actual_kind = match &checked {
                            CheckedClientExpression::Resource { operation } => operation.kind,
                            _ => unreachable!("resource constructor checker returns a resource"),
                        };
                        if actual_kind != expected_kind {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!("CLIENT local {local_name} type does not match its resource constructor"),
                                input.logical_path,
                                &local.type_source.span,
                            ));
                            return None;
                        }
                        if let Some(expected_type) = expected_type
                            && !client_expression_types_compatible(expression_type, expected_type)
                        {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!(
                                    "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                                ),
                                input.logical_path,
                                &local.type_source.span,
                            ));
                            return None;
                        }
                        locals.insert(local_name, ClientLocalBinding { checked, expression_type, ordinal: None, kind: CheckedClientLocalKind::Resource(actual_kind) });
                    }
                    let Some(expression) = block.return_expression.as_ref() else {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks must return an expression",
                            input.logical_path,
                            &block.span,
                        ));
                        return None;
                    };
                    if let Some(span) =
                        unsupported_client_state_reference(expression, input, &state_names)
                    {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state references are not supported in expressions",
                            input.logical_path,
                            &span,
                        ));
                        return None;
                    }
                    let diagnostics_before = diagnostics.len();
                    validate_client_await_positions(expression, false, input, diagnostics);
                    if diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    if client_expression_contains_action(expression) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks do not support action expressions",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    let (checked_return, return_type) = check_client_expression(
                        expression,
                        input,
                        &targets,
                        &action_targets,
                        resource_targets,
                        query_catalogue,
                        base,
                        server_names,
                        standard,
                        diagnostics,
                        &mut references,
                        &mut used_capabilities,
                        &locals,
                    )?;
                    if client_expression_contains_inspect(&checked_return) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::DomainIncompatible,
                            "CLIENT state blocks do not support Inspector expressions",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    if !client_expression_types_compatible(
                        return_type,
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                    ) {
                        diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "this CLIENT function must return the declared value type",
                            input.logical_path,
                            expression.span(),
                        ));
                        return None;
                    }
                    for capability in input.capabilities {
                        let capability_name = semantic_name(&capability.name);
                        if !used_capabilities.contains(&capability_name) {
                            diagnostics.push(diagnostic(
                                DiagnosticCode::CapabilityRequirement,
                                format!(
                                    "declared CLIENT capability {capability_name} is not exercised"
                                ),
                                input.logical_path,
                                &input.declaration_span,
                            ));
                            return None;
                        }
                    }
                    (
                        CheckedClientFunctionBody::StateBlock {
                            states,
                            return_expression: checked_return,
                        },
                        return_type,
                        location(input.logical_path, expression.span()),
                        references,
                    )
                } else if let Some(contract) = input.body.as_external_contract() {
                    let Some(identity) = client_contract_identity(contract) else {
                        diagnostics.push(diagnostic(
                        DiagnosticCode::DomainIncompatible,
                        "RUNTIME CONTRACT identity must be '<qualified-name>@<positive-version>'",
                        input.logical_path,
                        &input.declaration_span,
                    ));
                        return None;
                    };
                    if !validate_registered_client_external_contract(
                        &input.name,
                        &identity,
                        &input.parameters,
                        ResolvedApplicationType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                        },
                        input.result_shape,
                        input.logical_path,
                        &input.declaration_span,
                        diagnostics,
                    ) {
                        return None;
                    }
                    (
                        CheckedClientFunctionBody::ExternalContract {
                            identity,
                            location: location(input.logical_path, &contract.span),
                        },
                        ClientExpressionType {
                            semantic_type: input.return_type,
                            standard_value_type: input.standard_value_type,
                            result_shape: input.result_shape,
                        },
                        location(input.logical_path, &contract.span),
                        Vec::new(),
                    )
                } else {
                    diagnostics.push(diagnostic(
                        DiagnosticCode::DomainIncompatible,
                        "CLIENT function body is not supported",
                        input.logical_path,
                        &input.declaration_span,
                    ));
                    return None;
                };
            references.extend(parameter_references(&input.parameters));
            match &body {
                CheckedClientFunctionBody::ExternalContract { .. } => {}
                #[cfg(test)]
                CheckedClientFunctionBody::Unsupported => {}
                _ => {
                    let resolved = ResolvedApplicationType {
                        semantic_type: body_type.semantic_type,
                        standard_value_type: body_type.standard_value_type,
                    };
                    let mut recorder = StandardTypeUseRecorder::new(
                        uses,
                        standard,
                        input.id,
                        input.logical_path,
                    );
                    recorder.record_client_body(resolved, body_location);
                }
            }
            Some(CheckedClientFunction {
                id: input.id,
                name: input.name.clone(),
                domain: FunctionDomain::Client,
                parameters: input
                    .parameters
                    .iter()
                    .map(|parameter| CheckedServerFunctionParameter {
                        id: parameter.id,
                        name: parameter.name.clone(),
                        ordinal: parameter.ordinal,
                        semantic_type: parameter.semantic_type,
                        location: parameter.location.clone(),
                    })
                    .collect(),
                return_type: input.return_type,
                return_shape: input.return_shape,
                security: CatalogueFunctionSecurity::Invoker,
                transaction: None,
                volatility: CatalogueFunctionVolatility::Immutable,
                location: input.location.clone(),
                body,
                references,
                capabilities: input
                    .capabilities
                    .iter()
                    .filter_map(checked_client_capability)
                    .collect(),
            })
        })
        .collect()
}
