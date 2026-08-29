use super::*;

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
enum ClientCapabilityArgumentKind {
    PathScope,
    HostScope,
    SecretId,
}

impl ClientCapabilityArgumentKind {
    const fn label(self) -> &'static str {
        match self {
            Self::PathScope => "path-scope",
            Self::HostScope => "host-scope",
            Self::SecretId => "secret-id",
        }
    }
}

struct ClientCapabilityVocabularyEntry {
    parts: &'static [&'static str],
    argument_count: usize,
    argument_kind: ClientCapabilityArgumentKind,
}

const CLIENT_CAPABILITY_VOCABULARY: &[ClientCapabilityVocabularyEntry] = &[
    ClientCapabilityVocabularyEntry {
        parts: &["std", "fs", "read"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::PathScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "fs", "write"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::PathScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "net", "connect"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::HostScope,
    },
    ClientCapabilityVocabularyEntry {
        parts: &["std", "secret", "use"],
        argument_count: 1,
        argument_kind: ClientCapabilityArgumentKind::SecretId,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClientCapabilityArgument {
    TextLiteral,
    Parameter(String),
}

fn client_capability_entry(
    name: &QualifiedSemanticName,
) -> Option<&'static ClientCapabilityVocabularyEntry> {
    CLIENT_CAPABILITY_VOCABULARY.iter().find(|entry| {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(entry.parts.iter().copied())
    })
}

fn client_capability_argument_count(arguments: Option<&SourceSlice>) -> usize {
    let Some(arguments) = arguments else {
        return 0;
    };
    let text = arguments.text.trim();
    if text.is_empty() {
        return 0;
    }

    let mut count = 1;
    let mut parentheses = 0usize;
    let mut quote = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if let Some(quote_character) = quote {
            if character == quote_character {
                if characters.peek() == Some(&quote_character) {
                    characters.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '(' => parentheses += 1,
            ')' => parentheses = parentheses.saturating_sub(1),
            ',' if parentheses == 0 => count += 1,
            _ => {}
        }
    }
    count
}

fn parse_client_capability_argument(text: &str) -> Option<ClientCapabilityArgument> {
    let text = text.trim();
    if is_client_text_literal(text) {
        return Some(ClientCapabilityArgument::TextLiteral);
    }
    normalise_client_parameter_name(text).map(ClientCapabilityArgument::Parameter)
}

/// Records one validated capability requirement in the checked CLIENT model.
///
/// The checked name is the closed qualified vocabulary name and the argument
/// source is the declaration's literal scope value or parameter reference.
/// Validation has already run, so a non-vocabulary name, wrong argument
/// shape, or undeclared parameter cannot reach this conversion; unknown
/// forms map to `None` and are skipped.
fn checked_client_capability(
    capability: &CapabilitySpecification,
) -> Option<CheckedClientCapability> {
    let name = semantic_name(&capability.name);
    client_capability_entry(&name)?;
    let arguments = capability.arguments.as_ref()?;
    let argument = parse_client_capability_argument(&arguments.text)?;
    let argument = match argument {
        ClientCapabilityArgument::TextLiteral => {
            CheckedClientCapabilityArgument::Text(unquote_client_text_literal(&arguments.text)?)
        }
        ClientCapabilityArgument::Parameter(parameter) => {
            CheckedClientCapabilityArgument::Parameter(parameter)
        }
    };
    Some(CheckedClientCapability::new(name.to_string(), argument))
}

/// Unquotes one validated single-quoted CLIENT text literal.
///
/// A doubled quote inside the literal is a single literal quote, mirroring
/// `normalise_client_parameter_name`'s handling of quoted parameter names.
fn unquote_client_text_literal(text: &str) -> Option<String> {
    let text = text.trim();
    if !is_client_text_literal(text) {
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

fn is_client_text_literal(text: &str) -> bool {
    let mut characters = text.chars();
    if characters.next() != Some('\'') || !text.ends_with('\'') {
        return false;
    }

    let mut characters = text[1..].chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\'' {
            continue;
        }
        if characters.peek() == Some(&'\'') {
            characters.next();
        } else {
            return characters.peek().is_none();
        }
    }
    false
}

fn normalise_client_parameter_name(text: &str) -> Option<String> {
    if text.starts_with('"') {
        if !text.ends_with('"') || text.len() < 2 {
            return None;
        }
        let inner = &text[1..text.len() - 1];
        let mut characters = inner.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '"' && characters.peek() == Some(&'"') {
                characters.next();
            } else if character == '"' {
                return None;
            }
        }
        if inner.is_empty() {
            return None;
        }
        return Some(inner.replace("\"\"", "\""));
    }

    let mut characters = text.chars();
    let first = characters.next()?;
    if first != '_' && !first.is_alphabetic() {
        return None;
    }
    if characters.any(|character| character != '_' && !character.is_alphanumeric()) {
        return None;
    }
    Some(text.to_lowercase())
}

pub(super) fn validate_client_capability<'a>(
    capability: &CapabilitySpecification,
    declared_parameters: impl IntoIterator<Item = &'a str>,
    logical_path: &str,
    declaration_span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) {
    let name = semantic_name(&capability.name);
    let Some(entry) = client_capability_entry(&name) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!("unknown CLIENT capability {name}"),
            logical_path,
            declaration_span,
        ));
        return;
    };

    let argument_count = client_capability_argument_count(capability.arguments.as_ref());
    if argument_count != entry.argument_count {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} requires exactly {} {} argument",
                entry.argument_count,
                entry.argument_kind.label()
            ),
            logical_path,
            declaration_span,
        ));
        return;
    }

    let Some(arguments) = capability.arguments.as_ref() else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} requires one {} argument",
                entry.argument_kind.label()
            ),
            logical_path,
            declaration_span,
        ));
        return;
    };
    let Some(argument) = parse_client_capability_argument(&arguments.text) else {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} argument must be a text literal or declared parameter"
            ),
            logical_path,
            declaration_span,
        ));
        return;
    };
    if let ClientCapabilityArgument::Parameter(parameter) = argument
        && !declared_parameters
            .into_iter()
            .any(|declared| declared == parameter)
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::CapabilityRequirement,
            format!(
                "CLIENT capability {name} argument references undeclared parameter {parameter}"
            ),
            logical_path,
            declaration_span,
        ));
    }
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

#[allow(clippy::too_many_arguments)]
fn check_resource_constructor(
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
fn check_action_constructor(
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
fn check_inspect_call(
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
    Some(Some((
        CheckedClientExpression::Inspect { operation },
        ClientExpressionType {
            semantic_type: SemanticType::Named(CheckedTypeId::Existing(result_type)),
            standard_value_type: None,
            result_shape: ClientExpressionResultShape::Value,
        },
    )))
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

fn client_local_resource_family(source: &SourceSlice) -> Option<ResourceKind> {
    let mut parser = ClientResourceTypeParser::new(&source.text, source.span.start);
    let outer = parser.parse_qualified_name_parts()?;
    if outer.len() != 3
        || !outer[0].text.eq_ignore_ascii_case("std")
        || !outer[1].text.eq_ignore_ascii_case("data")
    {
        return None;
    }
    match outer[2].text.to_ascii_lowercase().as_str() {
        "resource" => Some(ResourceKind::Scalar),
        "streamresource" => Some(ResourceKind::Stream),
        _ => None,
    }
}

/// Parses a CLIENT resource declaration and returns its family plus inner descriptor.
///
/// The descriptor is resolved later against submitted and standard types; the SERVER
/// target remains authoritative for the resulting expression type.
pub(super) fn client_local_resource_type(
    source: &SourceSlice,
) -> Option<(ResourceKind, Option<TypeSpecification>)> {
    let mut parser = ClientResourceTypeParser::new(&source.text, source.span.start);
    let outer = parser.parse_qualified_name_parts()?;
    if outer.len() != 3
        || !outer[0].text.eq_ignore_ascii_case("std")
        || !outer[1].text.eq_ignore_ascii_case("data")
    {
        return None;
    }
    let kind = match outer[2].text.to_ascii_lowercase().as_str() {
        "resource" => ResourceKind::Scalar,
        "streamresource" => ResourceKind::Stream,
        _ => return None,
    };
    if !parser.consume(b'<') {
        return None;
    }
    let descriptor = if parser.consume_keyword("TABLE") || parser.consume_keyword("RECORD") {
        parser.parse_inline_record_shape(0)?;
        None
    } else {
        Some(parser.parse_type_specification(0)?)
    };
    if !parser.consume(b'>') || !parser.is_end() {
        return None;
    }
    Some((kind, descriptor))
}

fn reject_deferred_client_resource_descriptor(
    descriptor: Option<&TypeSpecification>,
    local_name: &str,
    input: &ResolvedClientFunctionInput<'_>,
    source: &SourceSlice,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    // A successful parse with no typed descriptor is the deferred inline row shape.
    if descriptor.is_some() {
        return false;
    }
    diagnostics.push(diagnostic(
        DiagnosticCode::TypeMismatch,
        format!(
            "CLIENT local {local_name} uses an inline TABLE/RECORD resource descriptor; row-resource transport is deferred"
        ),
        input.logical_path,
        &source.span,
    ));
    true
}

pub(super) struct ClientResourceTypeParser<'a> {
    text: &'a str,
    base: usize,
    offset: usize,
    invalid_trivia: bool,
}

impl<'a> ClientResourceTypeParser<'a> {
    pub(super) const MAX_TYPE_DEPTH: usize = 32;

    fn new(text: &'a str, base: usize) -> Self {
        Self {
            text,
            base,
            offset: 0,
            invalid_trivia: false,
        }
    }

    fn span(&self, start: usize, end: usize) -> SourceSpan {
        SourceSpan {
            start: self.base + start,
            end: self.base + end,
        }
    }

    fn source_slice(&self, start: usize, end: usize) -> SourceSlice {
        SourceSlice {
            text: self.text[start..end].to_owned(),
            span: self.span(start, end),
        }
    }

    fn is_end(&mut self) -> bool {
        self.skip_trivia();
        !self.invalid_trivia && self.offset == self.text.len()
    }

    fn skip_trivia(&mut self) {
        loop {
            while self
                .text
                .get(self.offset..)
                .and_then(|text| text.chars().next())
                .is_some_and(char::is_whitespace)
            {
                self.offset += self.text[self.offset..]
                    .chars()
                    .next()
                    .expect("character exists")
                    .len_utf8();
            }
            let Some(remaining) = self.text.get(self.offset..) else {
                return;
            };
            if remaining.starts_with("--") {
                self.offset += 2;
                while let Some(character) = self.text[self.offset..].chars().next() {
                    self.offset += character.len_utf8();
                    if character == '\n' {
                        break;
                    }
                }
                continue;
            }
            if let Some(comment) = remaining.strip_prefix("/*") {
                let Some(end) = comment.find("*/") else {
                    self.invalid_trivia = true;
                    self.offset = self.text.len();
                    return;
                };
                self.offset += end + 4;
                continue;
            }
            return;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        self.skip_trivia();
        if self.text.as_bytes().get(self.offset) == Some(&expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn parse_identifier_part(&mut self) -> Option<NamePart> {
        self.skip_trivia();
        let start = self.offset;
        if self.text.as_bytes().get(self.offset) == Some(&b'"') {
            self.offset += 1;
            while let Some(character) = self.text[self.offset..].chars().next() {
                self.offset += character.len_utf8();
                if character == '"' {
                    if self.text.as_bytes().get(self.offset) == Some(&b'"') {
                        self.offset += 1;
                    } else {
                        return Some(NamePart {
                            text: self.text[start..self.offset].to_owned(),
                            span: self.span(start, self.offset),
                        });
                    }
                }
            }
            return None;
        }
        let first = self.text[self.offset..].chars().next()?;
        if first != '_' && !first.is_alphabetic() {
            return None;
        }
        self.offset += first.len_utf8();
        while let Some(character) = self.text[self.offset..].chars().next() {
            if character != '_' && !character.is_alphabetic() && !character.is_numeric() {
                break;
            }
            self.offset += character.len_utf8();
        }
        Some(NamePart {
            text: self.text[start..self.offset].to_owned(),
            span: self.span(start, self.offset),
        })
    }

    fn parse_identifier(&mut self) -> Option<String> {
        self.parse_identifier_part().map(|part| part.text)
    }

    fn parse_qualified_name_parts(&mut self) -> Option<Vec<NamePart>> {
        let mut parts = vec![self.parse_identifier_part()?];
        while self.consume(b'.') {
            parts.push(self.parse_identifier_part()?);
        }
        Some(parts)
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_trivia();
        let saved = self.offset;
        if self.text.as_bytes().get(saved) == Some(&b'"') {
            return false;
        }
        let Some(identifier) = self.parse_identifier() else {
            return false;
        };
        if identifier.eq_ignore_ascii_case(keyword) {
            true
        } else {
            self.offset = saved;
            false
        }
    }

    fn parse_type_specification(&mut self, depth: usize) -> Option<TypeSpecification> {
        if depth > Self::MAX_TYPE_DEPTH {
            return None;
        }
        self.skip_trivia();
        let saved = self.offset;
        if self.consume_keyword("REF") {
            let target = self.parse_type_specification(depth + 1)?;
            let spec = TypeSpecification::Reference {
                span: self.span(saved, target.span().end - self.base),
                target: Box::new(target),
            };
            return self.parse_postfix_options(spec, depth);
        }
        for keyword in ["LIST", "SET", "MAP", "OPTION", "STREAM"] {
            self.offset = saved;
            if !self.consume_keyword(keyword) {
                continue;
            }
            if !self.consume(b'<') {
                return None;
            }
            let first = self.parse_type_specification(depth + 1)?;
            let second = if keyword == "MAP" {
                if !self.consume(b',') {
                    return None;
                }
                Some(self.parse_type_specification(depth + 1)?)
            } else {
                None
            };
            if !self.consume(b'>') {
                return None;
            }
            let spec = match keyword {
                "LIST" => TypeSpecification::List {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                "SET" => TypeSpecification::Set {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                "MAP" => TypeSpecification::Map {
                    span: self.span(saved, self.offset),
                    key: Box::new(first),
                    value: Box::new(second.expect("MAP value exists")),
                },
                "OPTION" => TypeSpecification::Option {
                    span: self.span(saved, self.offset),
                    value: Box::new(first),
                    spelling: OptionTypeSpelling::Prefix,
                },
                "STREAM" => TypeSpecification::Stream {
                    span: self.span(saved, self.offset),
                    element: Box::new(first),
                },
                _ => unreachable!(),
            };
            return self.parse_postfix_options(spec, depth);
        }
        self.offset = saved;
        if let Some(spec) = self.parse_standard_large_object_specification() {
            return self.parse_postfix_options(spec, depth);
        }
        self.offset = saved;
        let parts = self.parse_qualified_name_parts()?;
        let start = parts.first().expect("nonempty").span.start - self.base;
        let end = parts.last().expect("nonempty").span.end - self.base;
        self.parse_postfix_options(
            TypeSpecification::Named(QualifiedName {
                parts,
                span: self.span(start, end),
            }),
            depth,
        )
    }

    fn parse_inline_record_shape(&mut self, depth: usize) -> Option<()> {
        if depth > Self::MAX_TYPE_DEPTH || !self.consume(b'(') {
            return None;
        }
        if self.consume(b')') {
            return Some(());
        }
        loop {
            self.parse_identifier_part()?;
            self.parse_type_specification(depth + 1)?;
            if self.consume(b')') {
                return Some(());
            }
            if !self.consume(b',') {
                return None;
            }
        }
    }

    fn parse_standard_large_object_specification(&mut self) -> Option<TypeSpecification> {
        self.skip_trivia();
        let start = self.offset;
        let kind = if self.consume_keyword("CHARACTER") {
            StandardLargeObjectKind::Character
        } else {
            self.offset = start;
            if self.consume_keyword("BINARY") {
                StandardLargeObjectKind::Binary
            } else {
                self.offset = start;
                return None;
            }
        };
        if !self.consume_keyword("LARGE") || !self.consume_keyword("OBJECT") {
            self.offset = start;
            return None;
        }
        Some(TypeSpecification::StandardLargeObject {
            kind,
            source: self.source_slice(start, self.offset),
        })
    }

    fn parse_postfix_options(
        &mut self,
        mut spec: TypeSpecification,
        depth: usize,
    ) -> Option<TypeSpecification> {
        let mut option_depth = depth;
        loop {
            self.skip_trivia();
            if self.text.as_bytes().get(self.offset) != Some(&b'?') {
                return Some(spec);
            }
            if option_depth >= Self::MAX_TYPE_DEPTH {
                return None;
            }
            self.offset += 1;
            option_depth += 1;
            let start = spec.span().start - self.base;
            spec = TypeSpecification::Option {
                value: Box::new(spec),
                spelling: OptionTypeSpelling::Postfix,
                span: self.span(start, self.offset),
            };
        }
    }
}

fn client_type_specification_from_source(source: &SourceSlice) -> Option<TypeSpecification> {
    let text = source.text.trim();
    let normalized: String = text
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let large_object = match normalized.to_ascii_uppercase().as_str() {
        "CHARACTERLARGEOBJECT" => Some(StandardLargeObjectKind::Character),
        "BINARYLARGEOBJECT" => Some(StandardLargeObjectKind::Binary),
        _ => None,
    };
    if let Some(kind) = large_object {
        return Some(TypeSpecification::StandardLargeObject {
            kind,
            source: source.clone(),
        });
    }
    if text.is_empty()
        || text.split('.').any(|part| {
            part.is_empty()
                || part.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || character == '_' || character == '"')
                })
        })
    {
        return None;
    }
    let parts = text
        .split('.')
        .map(|part| orna_syntax::NamePart {
            text: part.to_owned(),
            span: source.span.clone(),
        })
        .collect::<Vec<_>>();
    Some(TypeSpecification::Named(QualifiedName {
        parts,
        span: source.span.clone(),
    }))
}
pub(super) fn client_contract_identity(source: &SourceSlice) -> Option<String> {
    let identity = decode_string_literal(source)?;
    let (name, version) = identity.rsplit_once('@')?;
    if version.is_empty()
        || version
            .parse::<u64>()
            .ok()
            .is_none_or(|version| version == 0)
        || name.contains('@')
    {
        return None;
    }
    let parts = name.split('.').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|part| normalise_client_parameter_name(part).is_none())
        || QualifiedSemanticName::new(parts).is_err()
    {
        return None;
    }
    Some(identity)
}
fn is_inspect_render_identity(identity: &str) -> bool {
    identity == "devtools.inspector_shell@1"
        || identity == INSPECT_RENDER_CONTRACT
        || identity.starts_with("std.inspect.render@")
}

#[allow(clippy::too_many_arguments)]
fn validate_registered_client_external_contract(
    _name: &QualifiedSemanticName,
    identity: &str,
    parameters: &[ResolvedServerFunctionParameter],
    return_type: ResolvedApplicationType,
    result_shape: ClientExpressionResultShape,
    logical_path: &str,
    declaration_span: &SourceSpan,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> bool {
    if !is_inspect_render_identity(identity) {
        return true;
    }
    if identity != INSPECT_RENDER_CONTRACT {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownQualifiedName,
            format!("unregistered CLIENT external contract {identity}"),
            logical_path,
            declaration_span,
        ));
        return false;
    }

    if parameters.len() != INSPECT_RENDER_CARRIER_SIGNATURE.len() {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("{INSPECT_RENDER_CONTRACT} requires exactly nine ordered carrier parameters"),
            logical_path,
            declaration_span,
        ));
        return false;
    }
    for (parameter, (expected_name, expected_id, _)) in
        parameters.iter().zip(INSPECT_RENDER_CARRIER_SIGNATURE)
    {
        if parameter.name != expected_name
            || parameter.semantic_type != SemanticType::Named(CheckedTypeId::Existing(expected_id))
        {
            diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!(
                    "{INSPECT_RENDER_CONTRACT} parameter {expected_name} must be {}",
                    expected_name.trim_start_matches("p_")
                ),
                logical_path,
                &parameter.name_span,
            ));
            return false;
        }
    }
    if result_shape != ClientExpressionResultShape::Value
        || return_type.semantic_type != SemanticType::Named(CheckedTypeId::Existing(STD_UI_TYPE_ID))
    {
        diagnostics.push(diagnostic(
            DiagnosticCode::TypeMismatch,
            format!("{INSPECT_RENDER_CONTRACT} must return std.ui.UI"),
            logical_path,
            declaration_span,
        ));
        return false;
    }
    true
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

struct ClientControlFlowChecker<'a, 'b> {
    input: &'a ResolvedClientFunctionInput<'b>,
    submitted_ids: &'a HashMap<QualifiedSemanticName, SubmittedType>,
    targets: &'a HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &'a HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &'a HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &'a CatalogueSnapshot,
    server_names: &'a [QualifiedSemanticName],
    standard: Option<&'a CheckedStandardLibrary>,
    diagnostics: &'a mut Vec<CompilerDiagnostic>,
    locals: ClientLocalEnvironment,
    checked_locals: Vec<CheckedClientLocal>,
    references: Vec<CheckedDefinitionReference>,
    used_capabilities: HashSet<QualifiedSemanticName>,
    next_ordinal: u32,
    _source_lifetime: std::marker::PhantomData<&'b ()>,
}

impl<'a, 'b> ClientControlFlowChecker<'a, 'b> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        input: &'a ResolvedClientFunctionInput<'b>,
        submitted_ids: &'a HashMap<QualifiedSemanticName, SubmittedType>,
        targets: &'a HashMap<QualifiedSemanticName, ClientExpressionTarget>,
        action_targets: &'a HashMap<QualifiedSemanticName, ClientActionTarget>,
        resource_targets: &'a HashMap<QualifiedSemanticName, ClientResourceTarget>,
        query_catalogue: &'a ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
        base: &'a CatalogueSnapshot,
        server_names: &'a [QualifiedSemanticName],
        standard: Option<&'a CheckedStandardLibrary>,
        diagnostics: &'a mut Vec<CompilerDiagnostic>,
    ) -> Self {
        Self {
            input,
            submitted_ids,
            targets,
            action_targets,
            resource_targets,
            query_catalogue,
            base,
            server_names,
            standard,
            diagnostics,
            locals: ClientLocalEnvironment::new(),
            checked_locals: Vec::new(),
            references: Vec::new(),
            used_capabilities: HashSet::new(),
            next_ordinal: 0,
            _source_lifetime: std::marker::PhantomData,
        }
    }

    fn expression(
        &mut self,
        expression: &ClientExpression,
    ) -> Option<(CheckedClientExpression, ClientExpressionType)> {
        check_client_expression(
            expression,
            self.input,
            self.targets,
            self.action_targets,
            self.resource_targets,
            self.query_catalogue,
            self.base,
            self.server_names,
            self.standard,
            self.diagnostics,
            &mut self.references,
            &mut self.used_capabilities,
            &self.locals,
        )
    }

    fn declare_local(
        &mut self,
        name: &NamePart,
        type_source: Option<&SourceSlice>,
        expression: &ClientExpression,
        span: &SourceSpan,
        pre_begin_resource: bool,
    ) -> Option<(u32, CheckedClientExpression)> {
        let local_name = semantic_part(name);
        if self.locals.contains_key(&local_name) {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DuplicateDefinition,
                format!(
                    "duplicate CLIENT local definition {local_name} in {}",
                    self.input.name
                ),
                self.input.logical_path,
                &name.span,
            ));
            return None;
        }
        let diagnostics_before = self.diagnostics.len();
        validate_client_await_positions(expression, true, self.input, self.diagnostics);
        if self.diagnostics.len() != diagnostics_before {
            return None;
        }

        let declared_resource_family = type_source.and_then(client_local_resource_family);
        let direct_resource = matches!(
            expression,
            ClientExpression::Call { callee, .. }
                if resource_constructor_kind(&semantic_name(callee)).is_some()
        );
        if pre_begin_resource && !direct_resource {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                format!("CLIENT local {local_name} requires a resource constructor initializer"),
                self.input.logical_path,
                span,
            ));
            return None;
        }

        let (checked, expression_type, kind) = if declared_resource_family.is_some()
            || direct_resource
        {
            if !direct_resource {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "CLIENT local {local_name} resource type requires a resource constructor"
                    ),
                    self.input.logical_path,
                    span,
                ));
                return None;
            }
            let (checked, expression_type) = check_resource_constructor(
                expression,
                self.input,
                self.targets,
                self.action_targets,
                self.resource_targets,
                self.query_catalogue,
                self.base,
                self.server_names,
                self.standard,
                self.diagnostics,
                &mut self.references,
                &mut self.used_capabilities,
                &self.locals,
            )?;
            let actual_kind = match &checked {
                CheckedClientExpression::Resource { operation } => operation.kind,
                _ => unreachable!("resource constructor checker returns a resource"),
            };
            if let Some(source) = type_source {
                let Some((expected_kind, descriptor)) = client_local_resource_type(source) else {
                    self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!(
                                "CLIENT local {local_name} must declare std.data.Resource<T> or std.data.StreamResource<T>"
                            ),
                            self.input.logical_path,
                            &source.span,
                        ));
                    return None;
                };
                if reject_deferred_client_resource_descriptor(
                    descriptor.as_ref(),
                    &local_name,
                    self.input,
                    source,
                    self.diagnostics,
                ) {
                    return None;
                }
                if actual_kind != expected_kind {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} type does not match its resource constructor"
                        ),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                }
                if let Some(descriptor) = descriptor {
                    let resolved = resolve_application_type_with_named_standard(
                        &descriptor,
                        self.submitted_ids,
                        self.input.logical_path,
                        self.diagnostics,
                        self.standard,
                        true,
                    )?;
                    let expected_type = ClientExpressionType {
                        semantic_type: resolved.semantic_type,
                        standard_value_type: resolved.standard_value_type,
                        result_shape: ClientExpressionResultShape::Value,
                    };
                    if !client_expression_types_compatible(expression_type, expected_type) {
                        self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                format!(
                                    "CLIENT local {local_name} descriptor does not match its SERVER resource result"
                                ),
                                self.input.logical_path,
                                &source.span,
                            ));
                        return None;
                    }
                }
            }
            (
                checked,
                expression_type,
                CheckedClientLocalKind::Resource(actual_kind),
            )
        } else {
            let (checked, expression_type) = self.expression(expression)?;
            if !client_expression_type_is_evaluable(expression_type, self.base, self.standard) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "this CLIENT local type is not supported by the local evaluator",
                    self.input.logical_path,
                    span,
                ));
                return None;
            }
            if let Some(source) = type_source {
                let Some(specification) = client_type_specification_from_source(source) else {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!("unsupported CLIENT local type for {local_name}"),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                };
                let resolved = resolve_application_type_with_named_standard(
                    &specification,
                    self.submitted_ids,
                    self.input.logical_path,
                    self.diagnostics,
                    self.standard,
                    true,
                )?;
                let expected_type = ClientExpressionType {
                    semantic_type: resolved.semantic_type,
                    standard_value_type: resolved.standard_value_type,
                    result_shape: ClientExpressionResultShape::Value,
                };
                if !client_expression_types_compatible(expression_type, expected_type) {
                    self.diagnostics.push(diagnostic(
                        DiagnosticCode::TypeMismatch,
                        format!(
                            "CLIENT local {local_name} initializer does not match its declared type"
                        ),
                        self.input.logical_path,
                        &source.span,
                    ));
                    return None;
                }
            }
            (checked, expression_type, CheckedClientLocalKind::Value)
        };

        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.checked_add(1)?;
        self.checked_locals.push(CheckedClientLocal {
            ordinal,
            name: local_name.clone(),
            semantic_type: expression_type.semantic_type,
            standard_value_type: expression_type.standard_value_type,
            kind,
            location: location(self.input.logical_path, span),
        });
        self.locals.insert(
            local_name,
            ClientLocalBinding {
                checked: checked.clone(),
                expression_type,
                ordinal: Some(ordinal),
                kind,
            },
        );
        Some((ordinal, checked))
    }

    fn statements(
        &mut self,
        statements: &[orna_syntax::ClientProceduralStatement],
    ) -> Option<(Vec<CheckedClientControlFlowStatement>, bool)> {
        let mut checked = Vec::with_capacity(statements.len());
        let mut guaranteed_return = false;
        for statement in statements {
            let (statement, statement_returns) = match statement {
                orna_syntax::ClientProceduralStatement::Let(statement) => {
                    let (local, expression) = self.declare_local(
                        &statement.name,
                        statement.type_source.as_ref(),
                        &statement.expression,
                        &statement.span,
                        false,
                    )?;
                    (
                        CheckedClientControlFlowStatement::Let {
                            local,
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
                orna_syntax::ClientProceduralStatement::Assignment(statement) => {
                    let local_name = semantic_part(&statement.target);
                    let Some(binding) = self.locals.get(&local_name).cloned() else {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::UnknownQualifiedName,
                            format!("unknown CLIENT local {local_name}"),
                            self.input.logical_path,
                            &statement.target.span,
                        ));
                        return None;
                    };
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.expression,
                        true,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let direct_resource = matches!(
                        &statement.expression,
                        ClientExpression::Call { callee, .. }
                            if resource_constructor_kind(&semantic_name(callee)).is_some()
                    );
                    let (expression, expression_type) =
                        if matches!(binding.kind, CheckedClientLocalKind::Resource(_))
                            && direct_resource
                        {
                            check_resource_constructor(
                                &statement.expression,
                                self.input,
                                self.targets,
                                self.action_targets,
                                self.resource_targets,
                                self.query_catalogue,
                                self.base,
                                self.server_names,
                                self.standard,
                                self.diagnostics,
                                &mut self.references,
                                &mut self.used_capabilities,
                                &self.locals,
                            )?
                        } else {
                            self.expression(&statement.expression)?
                        };
                    if !client_expression_types_compatible(expression_type, binding.expression_type)
                        || (matches!(binding.kind, CheckedClientLocalKind::Resource(_))
                            != matches!(expression, CheckedClientExpression::Resource { .. }))
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            format!(
                                "CLIENT assignment to local {local_name} does not match its declared type"
                            ),
                            self.input.logical_path,
                            &statement.span,
                        ));
                        return None;
                    }
                    if let Some(binding) = self.locals.get_mut(&local_name) {
                        binding.checked = expression.clone();
                    }
                    (
                        CheckedClientControlFlowStatement::Assignment {
                            local: binding.ordinal.expect("control-flow local has ordinal"),
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
                orna_syntax::ClientProceduralStatement::Return(statement) => {
                    let expression = if let Some(expression) = statement.expression.as_ref() {
                        let diagnostics_before = self.diagnostics.len();
                        validate_client_await_positions(
                            expression,
                            true,
                            self.input,
                            self.diagnostics,
                        );
                        if self.diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        let (checked, expression_type) = self.expression(expression)?;
                        let expected = ClientExpressionType {
                            semantic_type: self.input.return_type,
                            standard_value_type: self.input.standard_value_type,
                            result_shape: self.input.result_shape,
                        };
                        if !client_expression_types_compatible(expression_type, expected) {
                            self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "this CLIENT RETURN expression does not match the declared value type",
                                self.input.logical_path,
                                expression.span(),
                            ));
                            return None;
                        }
                        Some(checked)
                    } else if self.input.return_type == SemanticType::scalar(StandardScalar::Void) {
                        None
                    } else {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT RETURN without an expression requires a VOID return type",
                            self.input.logical_path,
                            &statement.span,
                        ));
                        return None;
                    };
                    (
                        CheckedClientControlFlowStatement::Return {
                            expression,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        true,
                    )
                }
                orna_syntax::ClientProceduralStatement::If(statement) => {
                    let incoming = self.locals.clone();
                    let mut branches = Vec::with_capacity(1 + statement.elsif_branches.len());
                    let mut all_return = true;

                    self.locals = incoming.clone();
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.condition,
                        false,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let (condition, condition_type) = self.expression(&statement.condition)?;
                    if control_flow_supported_scalar(condition_type)
                        != Some(StandardScalar::Boolean)
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT IF condition must be BOOLEAN",
                            self.input.logical_path,
                            statement.condition.span(),
                        ));
                        return None;
                    }
                    self.locals = incoming.clone();
                    let (then_statements, then_returns) =
                        self.statements(&statement.then_statements)?;
                    all_return &= then_returns;
                    branches.push(CheckedClientControlFlowBranch {
                        condition,
                        statements: then_statements,
                        location: location(self.input.logical_path, &statement.span),
                    });

                    for branch in &statement.elsif_branches {
                        self.locals = incoming.clone();
                        let diagnostics_before = self.diagnostics.len();
                        validate_client_await_positions(
                            &branch.condition,
                            false,
                            self.input,
                            self.diagnostics,
                        );
                        if self.diagnostics.len() != diagnostics_before {
                            return None;
                        }
                        let (condition, condition_type) = self.expression(&branch.condition)?;
                        if control_flow_supported_scalar(condition_type)
                            != Some(StandardScalar::Boolean)
                        {
                            self.diagnostics.push(diagnostic(
                                DiagnosticCode::TypeMismatch,
                                "CLIENT ELSIF condition must be BOOLEAN",
                                self.input.logical_path,
                                branch.condition.span(),
                            ));
                            return None;
                        }
                        self.locals = incoming.clone();
                        let (branch_statements, branch_returns) =
                            self.statements(&branch.statements)?;
                        all_return &= branch_returns;
                        branches.push(CheckedClientControlFlowBranch {
                            condition,
                            statements: branch_statements,
                            location: location(self.input.logical_path, &branch.span),
                        });
                    }

                    let (else_statements, else_returns) =
                        if let Some(statements) = statement.else_statements.as_ref() {
                            self.locals = incoming.clone();
                            let (statements, returns) = self.statements(statements)?;
                            (Some(statements), returns)
                        } else {
                            (None, false)
                        };
                    all_return &= else_returns;
                    self.locals = incoming;
                    (
                        CheckedClientControlFlowStatement::If {
                            branches,
                            else_statements,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        all_return,
                    )
                }
                orna_syntax::ClientProceduralStatement::While(statement) => {
                    let diagnostics_before = self.diagnostics.len();
                    validate_client_await_positions(
                        &statement.condition,
                        false,
                        self.input,
                        self.diagnostics,
                    );
                    if self.diagnostics.len() != diagnostics_before {
                        return None;
                    }
                    let incoming = self.locals.clone();
                    let (condition, condition_type) = self.expression(&statement.condition)?;
                    if control_flow_supported_scalar(condition_type)
                        != Some(StandardScalar::Boolean)
                    {
                        self.diagnostics.push(diagnostic(
                            DiagnosticCode::TypeMismatch,
                            "CLIENT WHILE condition must be BOOLEAN",
                            self.input.logical_path,
                            statement.condition.span(),
                        ));
                        return None;
                    }
                    self.locals = incoming.clone();
                    let (statements, _) = self.statements(&statement.body)?;
                    self.locals = incoming;
                    (
                        CheckedClientControlFlowStatement::While {
                            condition,
                            statements,
                            location: location(self.input.logical_path, &statement.span),
                        },
                        false,
                    )
                }
            };
            guaranteed_return |= statement_returns;
            checked.push(statement);
        }
        Some((checked, guaranteed_return))
    }

    fn finish_capabilities(&mut self) -> bool {
        for capability in self.input.capabilities {
            let capability_name = semantic_name(&capability.name);
            if !self.used_capabilities.contains(&capability_name) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::CapabilityRequirement,
                    format!("declared CLIENT capability {capability_name} is not exercised"),
                    self.input.logical_path,
                    &self.input.declaration_span,
                ));
                return false;
            }
        }
        true
    }

    fn finish_direct_expression(
        mut self,
        expression: &ClientExpression,
        allow_await: bool,
    ) -> Option<(
        CheckedClientFunctionBody,
        ClientExpressionType,
        SourceLocation,
        Vec<CheckedDefinitionReference>,
    )> {
        let diagnostics_before = self.diagnostics.len();
        validate_client_await_positions(expression, allow_await, self.input, self.diagnostics);
        if self.diagnostics.len() != diagnostics_before {
            return None;
        }
        let (checked, expression_type) = self.expression(expression)?;
        let expected = ClientExpressionType {
            semantic_type: self.input.return_type,
            standard_value_type: self.input.standard_value_type,
            result_shape: self.input.result_shape,
        };
        if !client_expression_types_compatible(expression_type, expected) {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::TypeMismatch,
                "this CLIENT function must return the declared value type",
                self.input.logical_path,
                expression.span(),
            ));
            return None;
        }
        if !self.finish_capabilities() {
            return None;
        }
        let location = location(self.input.logical_path, expression.span());
        Some((
            CheckedClientFunctionBody::ControlFlow {
                locals: Vec::new(),
                statements: vec![CheckedClientControlFlowStatement::Return {
                    expression: Some(checked),
                    location: location.clone(),
                }],
            },
            expression_type,
            location,
            self.references,
        ))
    }

    fn finish_block(
        mut self,
        block: &orna_syntax::ClientStateBlockBody,
    ) -> Option<(
        CheckedClientFunctionBody,
        ClientExpressionType,
        SourceLocation,
        Vec<CheckedDefinitionReference>,
    )> {
        if !block.states.is_empty() {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "CLIENT state blocks cannot contain programmable control flow",
                self.input.logical_path,
                &block.span,
            ));
            return None;
        }

        let mut checked_statements = Vec::new();
        for local in &block.locals {
            let (ordinal, expression) = self.declare_local(
                &local.name,
                Some(&local.type_source),
                &local.expression,
                &local.span,
                false,
            )?;
            checked_statements.push(CheckedClientControlFlowStatement::Let {
                local: ordinal,
                expression,
                location: location(self.input.logical_path, &local.span),
            });
        }
        let (statements, mut guaranteed_return) = self.statements(&block.statements)?;
        checked_statements.extend(statements);

        let mut body_type = ClientExpressionType {
            semantic_type: self.input.return_type,
            standard_value_type: self.input.standard_value_type,
            result_shape: self.input.result_shape,
        };
        if let Some(expression) = block.return_expression.as_ref() {
            let diagnostics_before = self.diagnostics.len();
            validate_client_await_positions(expression, true, self.input, self.diagnostics);
            if self.diagnostics.len() != diagnostics_before {
                return None;
            }
            let (checked, expression_type) = self.expression(expression)?;
            if !client_expression_types_compatible(
                expression_type,
                ClientExpressionType {
                    semantic_type: self.input.return_type,
                    standard_value_type: self.input.standard_value_type,
                    result_shape: self.input.result_shape,
                },
            ) {
                self.diagnostics.push(diagnostic(
                    DiagnosticCode::TypeMismatch,
                    "this CLIENT function must return the declared value type",
                    self.input.logical_path,
                    expression.span(),
                ));
                return None;
            }
            body_type = expression_type;
            checked_statements.push(CheckedClientControlFlowStatement::Return {
                expression: Some(checked),
                location: location(self.input.logical_path, expression.span()),
            });
            guaranteed_return = true;
        }
        if !guaranteed_return {
            self.diagnostics.push(diagnostic(
                DiagnosticCode::DomainIncompatible,
                "CLIENT control-flow blocks must return on every path",
                self.input.logical_path,
                &block.span,
            ));
            return None;
        }
        if !self.finish_capabilities() {
            return None;
        }
        Some((
            CheckedClientFunctionBody::ControlFlow {
                locals: self.checked_locals,
                statements: checked_statements,
            },
            body_type,
            location(self.input.logical_path, &block.span),
            self.references,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn check_client_control_flow_body(
    input: &ResolvedClientFunctionInput<'_>,
    submitted_ids: &HashMap<QualifiedSemanticName, SubmittedType>,
    targets: &HashMap<QualifiedSemanticName, ClientExpressionTarget>,
    action_targets: &HashMap<QualifiedSemanticName, ClientActionTarget>,
    resource_targets: &HashMap<QualifiedSemanticName, ClientResourceTarget>,
    query_catalogue: &ResolutionCatalogue<CheckedTypeId, CheckedFieldId>,
    base: &CatalogueSnapshot,
    server_names: &[QualifiedSemanticName],
    standard: Option<&CheckedStandardLibrary>,
    diagnostics: &mut Vec<CompilerDiagnostic>,
) -> Option<(
    CheckedClientFunctionBody,
    ClientExpressionType,
    SourceLocation,
    Vec<CheckedDefinitionReference>,
)> {
    let checker = ClientControlFlowChecker::new(
        input,
        submitted_ids,
        targets,
        action_targets,
        resource_targets,
        query_catalogue,
        base,
        server_names,
        standard,
        diagnostics,
    );
    match input.body {
        orna_syntax::ClientFunctionBody::Expression { expression } => {
            checker.finish_direct_expression(expression, false)
        }
        orna_syntax::ClientFunctionBody::ReturnExpression { expression } => {
            checker.finish_direct_expression(expression, true)
        }
        orna_syntax::ClientFunctionBody::StateBlock(block) => checker.finish_block(block),
        orna_syntax::ClientFunctionBody::BooleanLiteral { .. }
        | orna_syntax::ClientFunctionBody::ExternalContract { .. } => None,
        _ => None,
    }
}

fn is_closed_client_boolean_return(specification: &TypeSpecification) -> bool {
    let TypeSpecification::Named(name) = specification else {
        return false;
    };
    if name.parts.len() != 1 || name.parts[0].text.starts_with('"') {
        return false;
    }
    let spelling = &name.parts[0].text;
    spelling.eq_ignore_ascii_case("BOOLEAN") || spelling.eq_ignore_ascii_case("BOOL")
}

fn is_standard_client_boolean_return(specification: &TypeSpecification) -> bool {
    if is_closed_client_boolean_return(specification) {
        return true;
    }
    let TypeSpecification::Named(name) = specification else {
        return false;
    };
    match semantic_name(name).parts() {
        [schema, value_type] => schema == "std" && value_type == "boolean",
        [schema, types, value_type] => {
            schema == "std" && types == "types" && value_type == "boolean"
        }
        _ => false,
    }
}
