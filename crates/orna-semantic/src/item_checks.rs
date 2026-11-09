//! Protocol surfaces and declaration validation.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    AssignmentTarget, Expr, LambdaParameter, Pattern, ProtocolMember, Statement, StringSegment,
    TypeExpr,
};
use orna_value::Diagnostic;

use super::*;

/// The specified member surface of a core protocol that source need not
/// declare.
pub(crate) fn core_protocol_surface(name: &str) -> Option<&'static [ProtocolMember]> {
    (name == "Currency").then(|| CORE_CURRENCY_SURFACE.as_slice())
}

/// Resolve a transparent local type alias to a locally declared, non-generic
/// protocol.  This intentionally does not cross imported module boundaries
/// or substitute generic protocol parameters: those identities remain the
/// documented residuals of this validator.
pub(crate) fn resolve_local_protocol_name(name: &str, scope: &Scope) -> Option<String> {
    let mut current = name.to_owned();
    let mut resolving = BTreeSet::new();
    loop {
        if scope.local_protocols.contains_key(&current) {
            return Some(current);
        }
        let Type::Named(next) = scope.type_aliases.get(&current)? else {
            return None;
        };
        if !resolving.insert(current) {
            return None;
        }
        current = next.clone();
    }
}

/// Check the member surface, signature/default contracts, and the available
/// contextual body shape of a nested implementation when its local protocol
/// imported-member conformance, complete callable semantics, and full runtime
/// conversion dispatch remain outside this validator. Unsupported generic,
/// qualified, imported, unresolved, and non-protocol identities fail closed.
/// The core `Currency` protocol is checked against its specified surface;
/// only the builtin Display/Present names retain a missing-member-surface
/// residual. Mixed protocols check generic and non-generic members alike.
/// `From<Source>` checks the closed source/member shape below, but does not
/// implement conversion selection or runtime dispatch.
/// Default compatibility uses the conservative proof described below for
/// expression behaviour; arbitrary semantic default equivalence is residual.
pub(crate) fn validate_nested_implementation_members(
    implementation: &orna_syntax::Implementation,
    local_protocols: &BTreeMap<String, Vec<ProtocolMember>>,
    scope: &Scope,
    target: &Type,
    target_shape: Option<&Type>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut owned_scope = scope.clone();
    if let Type::Named(target_name) = target {
        owned_scope.private_field_owner = Some(target_name.clone());
    }
    let scope = &owned_scope;
    let TypeExpr::Name {
        path, arguments, ..
    } = &implementation.protocol
    else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "protocol implementation identity cannot be resolved",
        ));
        return;
    };
    if path.as_slice() == ["From"] {
        validate_from_implementation(
            implementation,
            arguments,
            target,
            target_shape,
            scope,
            diagnostics,
        );
        return;
    }
    if !arguments.is_empty() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "generic protocol instantiation cannot be validated",
        ));
        return;
    }
    if path.len() != 1 {
        diagnostics.push(diag(
            DIAG_TYPE,
            "qualified protocol identity cannot be resolved",
        ));
        return;
    }
    if scope.generic_protocols.contains(&path[0]) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "generic protocol substitution cannot be validated",
        ));
        return;
    }
    let Some(required_members) =
        required_protocol_members(path, local_protocols, scope, diagnostics)
    else {
        return;
    };
    let mut implemented_names = BTreeSet::new();
    for member in &implementation.members {
        let name = match member {
            orna_syntax::ImplMember::Function { signature, .. } => &signature.name,
            orna_syntax::ImplMember::Static { name, .. } => name,
        };
        if !implemented_names.insert(name.clone()) {
            diagnostics.push(diag(DIAG_TYPE, "duplicate protocol implementation member"));
        }

        let required = required_members
            .iter()
            .find(|required| match (required, member) {
                (
                    ProtocolMember::Function { signature, .. },
                    orna_syntax::ImplMember::Function {
                        signature: implementation_signature,
                        ..
                    },
                ) => signature.name == implementation_signature.name,
                (
                    ProtocolMember::Static { name, .. },
                    orna_syntax::ImplMember::Static {
                        name: implementation_name,
                        ..
                    },
                ) => name == implementation_name,
                _ => false,
            });
        let Some(required) = required else {
            diagnostics.push(diag(DIAG_TYPE, "unknown protocol implementation member"));
            continue;
        };

        match (required, member) {
            (
                ProtocolMember::Function { signature, .. },
                orna_syntax::ImplMember::Function {
                    signature: implementation_signature,
                    body,
                    ..
                },
            ) => {
                if !compatible_protocol_function_signature(
                    signature,
                    implementation_signature,
                    scope,
                ) {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "protocol implementation function signature is incompatible",
                    ));
                } else {
                    if signature.generics.is_empty() {
                        validate_nested_implementation_function(
                            signature,
                            implementation_signature,
                            body,
                            scope,
                            target,
                            target_shape,
                            diagnostics,
                        );
                    }
                }
            }
            (
                ProtocolMember::Static { ty, .. },
                orna_syntax::ImplMember::Static {
                    ty: implementation_ty,
                    value,
                    ..
                },
            ) => {
                let expected = resolved_type_of(ty, scope);
                if implementation_ty.as_ref().is_some_and(|implementation_ty| {
                    resolved_type_of(implementation_ty, scope) != expected
                }) {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "protocol implementation static property type is incompatible",
                    ));
                }
                let inferred =
                    infer_contextual(value, &expected, scope, &BTreeMap::new(), diagnostics);
                require_same(&expected, &inferred.ty, diagnostics);
            }
            _ => {}
        }
    }

    for required in required_members {
        let implemented = implementation
            .members
            .iter()
            .any(|member| match (required, member) {
                (
                    ProtocolMember::Function { signature, .. },
                    orna_syntax::ImplMember::Function {
                        signature: implementation_signature,
                        ..
                    },
                ) => signature.name == implementation_signature.name,
                (
                    ProtocolMember::Static { name, .. },
                    orna_syntax::ImplMember::Static {
                        name: implementation_name,
                        ..
                    },
                ) => name == implementation_name,
                _ => false,
            });
        if !implemented {
            diagnostics.push(diag(DIAG_TYPE, "missing protocol implementation member"));
        }
    }
}

/// Contextualize a nominal `From` result with its private representation row,
/// while retaining the target's nominal identity at the result boundary.
/// `infer_nominal` intentionally exposes a row shape to ordinary inference so
/// private field access remains usable; that structural view must not admit a
/// raw record as a conversion result.
pub(crate) fn infer_from_nominal_target(
    body: &orna_syntax::Expr,
    target: &Type,
    target_shape: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let expected_shape = target_shape.unwrap_or(target);
    let inferred = infer_contextual(body, expected_shape, scope, local, diagnostics);
    let Type::Named(_) = target else {
        return inferred;
    };
    let constructs_target = inferred.ty == *target;
    if !constructs_target {
        diagnostics.push(diag(
            DIAG_TYPE,
            "From implementation result must construct its nominal target",
        ));
        return Inferred::error_with(inferred.effects);
    }
    Inferred {
        ty: target.clone(),
        effects: inferred.effects,
    }
}

/// Resolve only the static type shapes this closed semantic scope can prove.
/// Unknown nominal names are not accepted merely because `type_of` preserves
/// them as `Type::Named`; imported/generic type details unavailable to this
/// scope therefore fail closed.
pub(crate) fn valid_static_type(type_expr: &TypeExpr, scope: &Scope) -> Option<Type> {
    let ty = resolved_type_of(type_expr, scope);
    if static_type_is_known(&ty, scope) {
        Some(ty)
    } else {
        None
    }
}

pub(crate) fn static_type_is_known(ty: &Type, scope: &Scope) -> bool {
    match ty {
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit => true,
        Type::Named(name) => {
            matches!(
                name.as_str(),
                "std.DURATION"
                    | "std.UUID"
                    | "std.TIME"
                    | "std.BINARY_LARGE_OBJECT"
                    | "std.Action"
                    | "std.Rows"
                    | "std.JsonValue"
                    | "std.Document"
                    | "std.ByteStream"
                    | "std.UI"
                    | "std.ui.Action"
                    | "Blob"
                    | "Error"
            ) || system_api::embedded_system_api().describes_type(name)
                || scope.generic_type_parameters.contains(name)
                || scope.nominal_rows.contains_key(name)
                || scope.names.get(name).is_some_and(|symbol| {
                    matches!(
                        symbol.kind,
                        SymbolKind::Type | SymbolKind::Enum | SymbolKind::Table
                    )
                })
        }
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => static_type_is_known(inner, scope),
        Type::Record(fields) => fields
            .values()
            .all(|field| static_type_is_known(field, scope)),
        Type::Tuple(elements) => elements
            .iter()
            .all(|element| static_type_is_known(element, scope)),
        Type::Applied { base, arguments } => {
            (matches!(
                base.as_str(),
                "List"
                    | "Range"
                    | "Relation"
                    | "Query"
                    | "Stream"
                    | "Money"
                    | "Float"
                    | "Decimal"
                    | "Quantity"
            ) || system_api::embedded_system_api().describes_type(base))
                && arguments
                    .iter()
                    .all(|argument| static_type_is_known(argument, scope))
        }
        Type::Quantity { numeric, .. } => static_type_is_known(numeric, scope),
        Type::MoneyPerUnit { currency, unit } => {
            static_type_is_known(currency, scope) && static_type_is_known(unit, scope)
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .all(|parameter| static_type_is_known(parameter, scope))
                && static_type_is_known(result, scope)
        }
        Type::Bottom | Type::Error => false,
    }
}

pub(crate) fn validate_nested_implementation_function(
    required: &orna_syntax::FunctionSignature,
    implementation: &orna_syntax::FunctionSignature,
    body: &orna_syntax::Expr,
    scope: &Scope,
    target: &Type,
    target_shape: Option<&Type>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target_shape = target_shape.or_else(|| match target {
        Type::Named(name) => scope
            .nominal_rows
            .get(name)
            .or_else(|| scope.table_rows.get(name)),
        _ => None,
    });
    let mut local = BTreeMap::new();
    for (required, implementation) in required.parameters.iter().zip(&implementation.parameters) {
        let effective_type = required
            .annotation
            .as_ref()
            .map(|annotation| nested_member_type(annotation, scope, target, target_shape, true))
            .or_else(|| {
                implementation.annotation.as_ref().map(|annotation| {
                    nested_member_type(annotation, scope, target, target_shape, true)
                })
            });
        let protocol_default_type = required.default.as_ref().and_then(|default| {
            validate_default_expression(
                default,
                effective_type.as_ref(),
                scope,
                &local,
                diagnostics,
            )
        });
        let implementation_default_type = implementation.default.as_ref().and_then(|default| {
            validate_default_expression(
                default,
                effective_type.as_ref().or(protocol_default_type.as_ref()),
                scope,
                &local,
                diagnostics,
            )
        });
        if !is_self_pattern(&implementation.pattern)
            && required.annotation.is_none()
            && implementation.annotation.is_none()
            && required.default.is_none()
            && implementation.default.is_none()
        {
            // An unconstrained non-self parameter cannot be admitted merely
            // because a later annotated body happens to type-check. This is
            // the unresolved-variable case covered by ORNA-INFER-001/-007.
            diagnostics.push(diag(
                DIAG_TYPE,
                "nested implementation parameter is underconstrained",
            ));
        }
        let parameter_type = effective_type
            .or(implementation_default_type)
            .or(protocol_default_type)
            .or_else(|| {
                is_self_pattern(&implementation.pattern).then(|| {
                    target_shape
                        .filter(|shape| matches!(shape, Type::Record(_)))
                        .unwrap_or(target)
                        .clone()
                })
            })
            .unwrap_or(Type::Error);
        bind_pattern(
            &implementation.pattern,
            parameter_type,
            scope,
            &mut local,
            diagnostics,
        );
    }

    let effective_result = required
        .result
        .as_ref()
        .map(|result| nested_member_type(result, scope, target, target_shape, false))
        .or_else(|| {
            implementation
                .result
                .as_ref()
                .map(|result| nested_member_type(result, scope, target, target_shape, false))
        });
    if let Some(expected) = effective_result {
        let before = diagnostics.len();
        let inferred = infer_function_body(body, Some(&expected), scope, &local, diagnostics);
        if diagnostics.len() == before {
            require_same(&expected, &inferred.ty, diagnostics);
        }
    } else if local.values().any(|symbol| symbol.ty != Type::Error) {
        infer_function_body(body, None, scope, &local, diagnostics);
    }
}

pub(crate) fn nested_member_type(
    annotation: &TypeExpr,
    scope: &Scope,
    target: &Type,
    target_shape: Option<&Type>,
    structural_self: bool,
) -> Type {
    let self_type = if structural_self {
        target_shape
            .filter(|shape| matches!(shape, Type::Record(_)))
            .unwrap_or(target)
    } else {
        target
    };
    substitute_generic_type(
        &resolved_type_of(annotation, scope),
        &BTreeMap::from([("Self".into(), self_type.clone())]),
    )
}

pub(crate) fn validate_default_expression(
    expression: &orna_syntax::Expr,
    expected: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if let Some(expected) = expected {
        let before = diagnostics.len();
        let inferred = infer_contextual(expression, expected, scope, local, diagnostics);
        if diagnostics.len() == before {
            require_same(expected, &inferred.ty, diagnostics);
        }
        Some(expected.clone())
    } else {
        let inferred = infer(expression, scope, local, diagnostics);
        (inferred.ty != Type::Error).then_some(inferred.ty)
    }
}

pub(crate) fn is_self_pattern(pattern: &orna_syntax::Pattern) -> bool {
    matches!(pattern, orna_syntax::Pattern::Name(name, _) if name == "self")
}

pub(crate) fn compatible_protocol_function_signature(
    required: &orna_syntax::FunctionSignature,
    implementation: &orna_syntax::FunctionSignature,
    scope: &Scope,
) -> bool {
    if required.generics.len() != implementation.generics.len()
        || required.parameters.len() != implementation.parameters.len()
    {
        return false;
    }

    let mut generic_scope = scope.clone();
    for (index, (required_generic, implementation_generic)) in required
        .generics
        .iter()
        .zip(&implementation.generics)
        .enumerate()
    {
        let placeholder = format!("__protocol_member_generic_{index}");
        generic_scope.type_aliases.remove(&required_generic.name);
        generic_scope
            .type_aliases
            .remove(&implementation_generic.name);
        generic_scope
            .generic_type_parameters
            .insert(placeholder.clone());
        generic_scope.type_aliases.insert(
            required_generic.name.clone(),
            Type::Named(placeholder.clone()),
        );
        generic_scope.type_aliases.insert(
            implementation_generic.name.clone(),
            Type::Named(placeholder),
        );

        if required_generic.bounds.len() != implementation_generic.bounds.len()
            || !required_generic
                .bounds
                .iter()
                .zip(&implementation_generic.bounds)
                .all(|(required, implementation)| {
                    compatible_protocol_type_annotation(
                        Some(required),
                        Some(implementation),
                        &generic_scope,
                    )
                })
        {
            return false;
        }
    }

    if !compatible_protocol_type_annotation(
        required.result.as_ref(),
        implementation.result.as_ref(),
        &generic_scope,
    ) {
        return false;
    }
    required
        .parameters
        .iter()
        .zip(&implementation.parameters)
        .all(|(required, implementation)| {
            compatible_parameter_pattern(&required.pattern, &implementation.pattern)
                && compatible_protocol_type_annotation(
                    required.annotation.as_ref(),
                    implementation.annotation.as_ref(),
                    &generic_scope,
                )
                && compatible_protocol_defaults(
                    required.default.as_ref(),
                    implementation.default.as_ref(),
                )
        })
}

/// A default expression is part of the callable behaviour of a protocol
/// member. Defaults are accepted only when their ASTs have the same structure
/// and values after source spans are ignored. This is a conservative proof of
/// identical source-level behaviour, not a claim of arbitrary expression
/// equivalence; semantically equal but differently written expressions remain
/// rejected.
pub(crate) fn compatible_protocol_defaults(
    required: Option<&orna_syntax::Expr>,
    implementation: Option<&orna_syntax::Expr>,
) -> bool {
    match (required, implementation) {
        (None, None) => true,
        (Some(required), Some(implementation)) => same_default_expression(required, implementation),
        _ => false,
    }
}

pub(crate) fn same_default_expression(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (Expr::Name { text: left, .. }, Expr::Name { text: right, .. })
        | (Expr::ReplBinding { text: left, .. }, Expr::ReplBinding { text: right, .. }) => {
            left == right
        }
        (
            Expr::Literal {
                text: left_text,
                kind: left_kind,
                ..
            },
            Expr::Literal {
                text: right_text,
                kind: right_kind,
                ..
            },
        ) => left_text == right_text && left_kind == right_kind,
        (
            Expr::InterpolatedString { segments: left, .. },
            Expr::InterpolatedString {
                segments: right, ..
            },
        ) => same_default_string_segments(left, right),
        (
            Expr::Unary {
                op: left_op,
                rhs: left_rhs,
                ..
            },
            Expr::Unary {
                op: right_op,
                rhs: right_rhs,
                ..
            },
        ) => left_op == right_op && same_default_expression(left_rhs, right_rhs),
        (
            Expr::Binary {
                lhs: left_lhs,
                op: left_op,
                rhs: left_rhs,
                ..
            },
            Expr::Binary {
                lhs: right_lhs,
                op: right_op,
                rhs: right_rhs,
                ..
            },
        ) => {
            left_op == right_op
                && same_default_expression(left_lhs, right_lhs)
                && same_default_expression(left_rhs, right_rhs)
        }
        (
            Expr::Range {
                lower: left_lower,
                operator: left_operator,
                upper: left_upper,
                ..
            },
            Expr::Range {
                lower: right_lower,
                operator: right_operator,
                upper: right_upper,
                ..
            },
        ) => {
            left_operator == right_operator
                && same_default_expression_options(left_lower.as_deref(), right_lower.as_deref())
                && same_default_expression_options(left_upper.as_deref(), right_upper.as_deref())
        }
        (
            Expr::Call {
                callee: left_callee,
                arguments: left_arguments,
                ..
            },
            Expr::Call {
                callee: right_callee,
                arguments: right_arguments,
                ..
            },
        ) => {
            same_default_expression(left_callee, right_callee)
                && same_default_arguments(left_arguments, right_arguments)
        }
        (
            Expr::GenericCall {
                callee: left_callee,
                type_arguments: left_type_arguments,
                arguments: left_arguments,
                ..
            },
            Expr::GenericCall {
                callee: right_callee,
                type_arguments: right_type_arguments,
                arguments: right_arguments,
                ..
            },
        ) => {
            same_default_expression(left_callee, right_callee)
                && same_default_type_expressions(left_type_arguments, right_type_arguments)
                && same_default_arguments(left_arguments, right_arguments)
        }
        (
            Expr::Index {
                base: left_base,
                index: left_index,
                ..
            },
            Expr::Index {
                base: right_base,
                index: right_index,
                ..
            },
        ) => {
            same_default_expression(left_base, right_base)
                && same_default_expression(left_index, right_index)
        }
        (
            Expr::Field {
                base: left_base,
                name: left_name,
                ..
            },
            Expr::Field {
                base: right_base,
                name: right_name,
                ..
            },
        ) => left_name == right_name && same_default_expression(left_base, right_base),
        (Expr::Group { inner: left, .. }, Expr::Group { inner: right, .. }) => {
            same_default_expression(left, right)
        }
        (
            Expr::Tuple { elements: left, .. },
            Expr::Tuple {
                elements: right, ..
            },
        )
        | (
            Expr::List { elements: left, .. },
            Expr::List {
                elements: right, ..
            },
        ) => same_default_expressions(left, right),
        (Expr::Record { fields: left, .. }, Expr::Record { fields: right, .. }) => {
            same_default_record_fields(left, right)
        }
        _ => same_default_compound_expression(left, right),
    }
}

/// The call, construction and control-flow forms: composite expressions whose
/// equality recurses through their parts.
pub(crate) fn same_default_compound_expression(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (
            Expr::Nominal {
                path: left_path,
                fields: left_fields,
                ..
            },
            Expr::Nominal {
                path: right_path,
                fields: right_fields,
                ..
            },
        ) => {
            left_path.len() == right_path.len()
                && left_path
                    .iter()
                    .zip(right_path)
                    .all(|(left, right)| left.text == right.text)
                && same_default_record_fields(left_fields, right_fields)
        }
        (
            Expr::Lambda {
                parameters: left_parameters,
                body: left_body,
                ..
            },
            Expr::Lambda {
                parameters: right_parameters,
                body: right_body,
                ..
            },
        ) => {
            left_parameters.len() == right_parameters.len()
                && left_parameters
                    .iter()
                    .zip(right_parameters)
                    .all(|(left, right)| same_default_lambda_parameter(left, right))
                && same_default_expression(left_body, right_body)
        }
        (
            Expr::Block {
                statements: left_statements,
                tail: left_tail,
                ..
            },
            Expr::Block {
                statements: right_statements,
                tail: right_tail,
                ..
            },
        ) => {
            left_statements.len() == right_statements.len()
                && left_statements
                    .iter()
                    .zip(right_statements)
                    .all(|(left, right)| same_default_statement(left, right))
                && same_default_expression_options(left_tail.as_deref(), right_tail.as_deref())
        }
        (
            Expr::Control {
                kind: left_kind,
                binding: left_binding,
                condition: left_condition,
                body: left_body,
                arms: left_arms,
                alternate: left_alternate,
                ..
            },
            Expr::Control {
                kind: right_kind,
                binding: right_binding,
                condition: right_condition,
                body: right_body,
                arms: right_arms,
                alternate: right_alternate,
                ..
            },
        ) => {
            left_kind == right_kind
                && same_default_pattern_options(left_binding.as_ref(), right_binding.as_ref())
                && same_default_expression_options(
                    left_condition.as_deref(),
                    right_condition.as_deref(),
                )
                && same_default_expression_options(left_body.as_deref(), right_body.as_deref())
                && left_arms.len() == right_arms.len()
                && left_arms
                    .iter()
                    .zip(right_arms)
                    .all(|(left, right)| same_default_case_arm(left, right))
                && same_default_expression_options(
                    left_alternate.as_deref(),
                    right_alternate.as_deref(),
                )
        }
        _ => false,
    }
}

pub(crate) fn same_default_expression_options(left: Option<&Expr>, right: Option<&Expr>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same_default_expression(left, right),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) fn same_default_expressions(left: &[Expr], right: &[Expr]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_default_expression(left, right))
}

pub(crate) fn same_default_string_segments(
    left: &[StringSegment],
    right: &[StringSegment],
) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (
                    StringSegment::Text { text: left, .. },
                    StringSegment::Text { text: right, .. },
                ) => left == right,
                (
                    StringSegment::Expression { value: left, .. },
                    StringSegment::Expression { value: right, .. },
                ) => same_default_expression(left, right),
                _ => false,
            })
}

pub(crate) fn same_default_arguments(
    left: &[orna_syntax::Argument],
    right: &[orna_syntax::Argument],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.name == right.name && same_default_expression(&left.value, &right.value)
        })
}

pub(crate) fn same_default_record_fields(
    left: &[orna_syntax::RecordField],
    right: &[orna_syntax::RecordField],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.name == right.name && same_default_expression(&left.value, &right.value)
        })
}

pub(crate) fn same_default_lambda_parameter(
    left: &LambdaParameter,
    right: &LambdaParameter,
) -> bool {
    same_default_pattern(&left.pattern, &right.pattern)
        && same_default_type_options(left.annotation.as_ref(), right.annotation.as_ref())
}

pub(crate) fn same_default_type_options(left: Option<&TypeExpr>, right: Option<&TypeExpr>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => type_of(left) == type_of(right),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) fn same_default_type_expressions(left: &[TypeExpr], right: &[TypeExpr]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| type_of(left) == type_of(right))
}

pub(crate) fn same_default_pattern_options(
    left: Option<&Pattern>,
    right: Option<&Pattern>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same_default_pattern(left, right),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) fn same_default_pattern(left: &Pattern, right: &Pattern) -> bool {
    match (left, right) {
        (Pattern::Name(left, _), Pattern::Name(right, _)) => left == right,
        (Pattern::Wildcard(_), Pattern::Wildcard(_)) => true,
        (
            Pattern::Literal {
                text: left_text,
                kind: left_kind,
                ..
            },
            Pattern::Literal {
                text: right_text,
                kind: right_kind,
                ..
            },
        ) => left_text == right_text && left_kind == right_kind,
        (
            Pattern::Tuple { elements: left, .. },
            Pattern::Tuple {
                elements: right, ..
            },
        )
        | (
            Pattern::List { elements: left, .. },
            Pattern::List {
                elements: right, ..
            },
        ) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_default_pattern(left, right))
        }
        (Pattern::Record { fields: left, .. }, Pattern::Record { fields: right, .. }) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|((left_name, left, _), (right_name, right, _))| {
                        left_name == right_name
                            && same_default_pattern_options(left.as_ref(), right.as_ref())
                    })
        }
        (
            Pattern::Constructor {
                path: left_path,
                arguments: left_arguments,
                fields: left_fields,
                ..
            },
            Pattern::Constructor {
                path: right_path,
                arguments: right_arguments,
                fields: right_fields,
                ..
            },
        ) => {
            left_path.len() == right_path.len()
                && left_path
                    .iter()
                    .zip(right_path)
                    .all(|(left, right)| left.text == right.text)
                && left_arguments.len() == right_arguments.len()
                && left_arguments
                    .iter()
                    .zip(right_arguments)
                    .all(|(left, right)| same_default_pattern(left, right))
                && left_fields.len() == right_fields.len()
                && left_fields.iter().zip(right_fields).all(|(left, right)| {
                    left.name == right.name
                        && same_default_pattern_options(
                            left.pattern.as_ref(),
                            right.pattern.as_ref(),
                        )
                })
        }
        _ => false,
    }
}

pub(crate) fn same_default_case_arm(
    left: &orna_syntax::CaseArm,
    right: &orna_syntax::CaseArm,
) -> bool {
    same_default_pattern(&left.pattern, &right.pattern)
        && same_default_expression_options(left.guard.as_ref(), right.guard.as_ref())
        && same_default_expression(&left.body, &right.body)
}

pub(crate) fn same_default_statement(left: &Statement, right: &Statement) -> bool {
    match (left, right) {
        (
            Statement::Let {
                pattern: left_pattern,
                annotation: left_annotation,
                value: left_value,
                ..
            },
            Statement::Let {
                pattern: right_pattern,
                annotation: right_annotation,
                value: right_value,
                ..
            },
        ) => {
            same_default_pattern(left_pattern, right_pattern)
                && same_default_type_options(left_annotation.as_ref(), right_annotation.as_ref())
                && same_default_expression(left_value, right_value)
        }
        (Statement::Assert { value: left, .. }, Statement::Assert { value: right, .. })
        | (Statement::Expression { value: left, .. }, Statement::Expression { value: right, .. })
        | (Statement::Control { value: left, .. }, Statement::Control { value: right, .. }) => {
            same_default_expression(left, right)
        }
        (Statement::Return { value: left, .. }, Statement::Return { value: right, .. })
        | (Statement::Break { value: left, .. }, Statement::Break { value: right, .. }) => {
            same_default_expression_options(left.as_ref(), right.as_ref())
        }
        (Statement::Continue { .. }, Statement::Continue { .. }) => true,
        (
            Statement::Assignment {
                target: left_target,
                operator: left_operator,
                value: left_value,
                ..
            },
            Statement::Assignment {
                target: right_target,
                operator: right_operator,
                value: right_value,
                ..
            },
        ) => {
            left_operator == right_operator
                && same_default_assignment_target(left_target, right_target)
                && same_default_expression(left_value, right_value)
        }
        _ => false,
    }
}

pub(crate) fn same_default_assignment_target(
    left: &AssignmentTarget,
    right: &AssignmentTarget,
) -> bool {
    match (left, right) {
        (AssignmentTarget::Name { name: left, .. }, AssignmentTarget::Name { name: right, .. }) => {
            left == right
        }
        (
            AssignmentTarget::Field {
                base: left_base,
                name: left_name,
                ..
            },
            AssignmentTarget::Field {
                base: right_base,
                name: right_name,
                ..
            },
        ) => left_name == right_name && same_default_assignment_target(left_base, right_base),
        (
            AssignmentTarget::Index {
                base: left_base,
                index: left_index,
                ..
            },
            AssignmentTarget::Index {
                base: right_base,
                index: right_index,
                ..
            },
        ) => {
            same_default_assignment_target(left_base, right_base)
                && same_default_expression(left_index, right_index)
        }
        _ => false,
    }
}

pub(crate) fn compatible_protocol_type_annotation(
    required: Option<&TypeExpr>,
    implementation: Option<&TypeExpr>,
    scope: &Scope,
) -> bool {
    match (required, implementation) {
        (Some(required), Some(implementation)) => {
            let required = resolved_type_of(required, scope);
            let implementation = resolved_type_of(implementation, scope);
            required != Type::Error && implementation != Type::Error && required == implementation
        }
        (Some(required), None) => resolved_type_of(required, scope) != Type::Error,
        (None, Some(implementation)) => resolved_type_of(implementation, scope) != Type::Error,
        (None, None) => true,
    }
}

pub(crate) fn validate_implementation_restrictions(
    implementation: &orna_syntax::Implementation,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match &implementation.protocol {
        TypeExpr::Name {
            path, arguments, ..
        } if path.as_slice() == ["TryFrom"] && arguments.len() == 1 => {
            diagnostics.push(diag(
                DIAG_LEGACY_TRYFROM,
                "use From<Source>; From may fail in Orna",
            ));
        }
        TypeExpr::Name { path, .. }
            if matches!(path.as_slice(), [protocol] if protocol == "Display" || protocol == "Present")
                && implementation_has_write(implementation, scope) =>
        {
            diagnostics.push(diag(
                DIAG_DISPLAY_READ_ONLY,
                "Display and Present implementations must be read-only",
            ));
        }
        _ => {}
    }
}

pub(crate) fn compatible_parameter_pattern(
    required: &orna_syntax::Pattern,
    implementation: &orna_syntax::Pattern,
) -> bool {
    match (required, implementation) {
        (
            orna_syntax::Pattern::Name(required, _),
            orna_syntax::Pattern::Name(implementation, _),
        ) => required == implementation,
        (orna_syntax::Pattern::Wildcard(_), orna_syntax::Pattern::Wildcard(_)) => true,
        _ => false,
    }
}

/// Rejects duplicate protocol identities on one target.  Exact
/// protocol identity is the portion of overlap that this closed semantic
/// catalogue can prove without inventing generic dispatch or specialization.
pub(crate) fn validate_non_overlapping_implementations<'a>(
    implementations: impl IntoIterator<Item = &'a orna_syntax::Implementation>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen = Vec::new();
    for implementation in implementations {
        let protocol = resolved_type_of(&implementation.protocol, scope);
        if protocol == Type::Error {
            diagnostics.push(diag(
                DIAG_TYPE,
                "protocol implementation identity cannot be resolved",
            ));
            continue;
        }
        if seen.iter().any(|candidate| candidate == &protocol) {
            diagnostics.push(diag(
                DIAG_TYPE,
                "overlapping protocol implementations are invalid",
            ));
        } else {
            seen.push(protocol);
        }
    }
}

pub(crate) fn implementation_has_write(
    implementation: &orna_syntax::Implementation,
    scope: &Scope,
) -> bool {
    implementation.members.iter().any(|member| match member {
        orna_syntax::ImplMember::Function {
            signature, body, ..
        } => {
            let local = signature
                .parameters
                .iter()
                .filter_map(|parameter| match &parameter.pattern {
                    orna_syntax::Pattern::Name(name, _) => Some((
                        name.clone(),
                        Symbol {
                            table_schema: None,
                            kind: SymbolKind::Let,
                            ty: Type::Error,
                            public: false,
                            effects: EffectSummary::default(),
                            generic_parameters: Vec::new(),
                            enum_variants: BTreeSet::new(),
                        },
                    )),
                    _ => None,
                })
                .collect();
            expr_has_write_in_implementation_body(body, scope, &local)
        }
        orna_syntax::ImplMember::Static { value: body, .. } => {
            expr_has_write(body, scope, &BTreeMap::new())
        }
    })
}

pub(crate) fn resolved_callable_symbol<'a>(
    callee: &Expr,
    scope: &'a Scope,
    local: &'a BTreeMap<String, Symbol>,
) -> Option<&'a Symbol> {
    if let Expr::Name { text, .. } = callee {
        return local.get(text).or_else(|| scope.names.get(text));
    }
    let path = qualified_path(callee)?;
    if path.len() < 2 || local.contains_key(path[0]) {
        return None;
    }
    qualified_module_symbol(&path, scope)
}

pub(crate) fn call_may_write(
    callee: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> bool {
    if let Expr::Field { base, name, .. } = callee
        && table_symbol(base, scope, local).is_some()
        && matches!(
            name.as_str(),
            "insert" | "upsert" | "update" | "delete" | "rekey"
        )
    {
        return true;
    }
    if let Some(symbol) = resolved_callable_symbol(callee, scope, local) {
        return symbol.kind != SymbolKind::Function || symbol.effects.is_effectful();
    }
    // A callable that is not represented by a resolved, effect-free function
    // summary is not proven pure. This intentionally rejects unknown indirect
    // calls, while the resolved-symbol path avoids spelling-based rejection of
    // a pure local function named `insert`.
    true
}

pub(crate) fn expr_has_write(expr: &Expr, scope: &Scope, local: &BTreeMap<String, Symbol>) -> bool {
    expr_has_write_with_return_termination(expr, scope, local, false)
}

pub(crate) fn expr_has_write_in_implementation_body(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> bool {
    expr_has_write_with_return_termination(expr, scope, local, true)
}

pub(crate) fn expr_has_write_with_return_termination(
    expr: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    terminate_after_direct_return: bool,
) -> bool {
    match expr {
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            call_may_write(callee, scope, local)
                || expr_has_write(callee, scope, local)
                || arguments
                    .iter()
                    .any(|argument| expr_has_write(&argument.value, scope, local))
        }
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
            expr_has_write(rhs, scope, local)
        }
        Expr::Binary { lhs, rhs, .. } => {
            expr_has_write(lhs, scope, local) || expr_has_write(rhs, scope, local)
        }
        Expr::Range { lower, upper, .. } => {
            lower
                .as_deref()
                .is_some_and(|lower| expr_has_write(lower, scope, local))
                || upper
                    .as_deref()
                    .is_some_and(|upper| expr_has_write(upper, scope, local))
        }
        Expr::Index { base, index, .. } => {
            expr_has_write(base, scope, local) || expr_has_write(index, scope, local)
        }
        Expr::Field { base, .. } => expr_has_write(base, scope, local),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => elements
            .iter()
            .any(|element| expr_has_write(element, scope, local)),
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => fields
            .iter()
            .any(|field| expr_has_write(&field.value, scope, local)),
        Expr::Lambda { body, .. } => expr_has_write(body, scope, local),
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                if statement_has_write(statement, scope, local) {
                    return true;
                }
                if terminate_after_direct_return && matches!(statement, Statement::Return { .. }) {
                    return false;
                }
            }
            tail.as_deref()
                .is_some_and(|tail| expr_has_write(tail, scope, local))
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            condition
                .as_deref()
                .is_some_and(|condition| expr_has_write(condition, scope, local))
                || body
                    .as_deref()
                    .is_some_and(|body| expr_has_write(body, scope, local))
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_has_write(guard, scope, local))
                        || expr_has_write(&arm.body, scope, local)
                })
                || alternate
                    .as_deref()
                    .is_some_and(|alternate| expr_has_write(alternate, scope, local))
        }
        _ => false,
    }
}

pub(crate) fn statement_has_write(
    statement: &Statement,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> bool {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. }
        | Statement::Assignment { value, .. } => expr_has_write(value, scope, local),
        Statement::Return { value, .. } | Statement::Break { value, .. } => value
            .as_ref()
            .is_some_and(|value| expr_has_write(value, scope, local)),
        Statement::Continue { .. } => false,
    }
}

pub(crate) fn validate_function_signature_annotations(
    signature: &orna_syntax::FunctionSignature,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let generic_names = signature
        .generics
        .iter()
        .map(|generic| generic.name.clone())
        .collect::<BTreeSet<_>>();
    for generic in &signature.generics {
        for bound in &generic.bounds {
            validate_type_annotation(bound, scope, &generic_names, diagnostics);
            if bound_protocol_type(bound, scope).is_none() {
                diagnostics.push(diag(DIAG_TYPE, "generic bound must name a protocol"));
            }
        }
    }
    for parameter in &signature.parameters {
        if let Some(annotation) = &parameter.annotation {
            validate_type_annotation(annotation, scope, &generic_names, diagnostics);
        }
    }
    if let Some(result) = &signature.result {
        validate_type_annotation(result, scope, &generic_names, diagnostics);
    }
}

/// Check type names at declaration boundaries instead of treating every
/// syntactic name as a valid nominal type. Unknown names must remain an
/// unresolved diagnostic; accepting them would let a public signature export
/// an unusable type and would make later inference appear to succeed.
pub(crate) fn validate_type_annotation(
    type_expr: &TypeExpr,
    scope: &Scope,
    generic_names: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match type_expr {
        TypeExpr::Name {
            path, arguments, ..
        } => {
            // Numeric arguments name units rather than types; `Money` takes a
            // currency type (ORNA-MONEY-004).
            let numeric_constructor =
                path.len() == 1 && matches!(path[0].as_str(), "Int" | "Decimal" | "Float");
            let money_constructor = path.len() == 1 && path[0] == "Money";
            if !annotation_type_name_is_declared(path, scope, generic_names) {
                diagnostics.push(diag(
                    if path.len() == 1 && path[0] == "Result" {
                        DIAG_LEGACY_RESULT
                    } else {
                        DIAG_UNRESOLVED
                    },
                    if path.len() == 1 && path[0] == "Result" {
                        "Result/Ok/Err control plumbing was removed; return the success type directly"
                    } else {
                        "type name cannot be resolved"
                    },
                ));
            }
            if numeric_constructor && !arguments.is_empty() {
                if let [unit] = arguments.as_slice() {
                    validate_unit_annotation(unit, scope, diagnostics);
                } else {
                    diagnostics.push(diag(DIAG_UNRESOLVED, "type name cannot be resolved"));
                }
            } else if path.len() == 1 && path[0] == "Quantity" {
                // `Quantity<N, U>` is the spelled-out form of `N<U>`.
                if let [numeric, unit] = arguments.as_slice()
                    && matches!(type_of(numeric), Type::Int | Type::Decimal | Type::Float)
                {
                    validate_unit_annotation(unit, scope, diagnostics);
                } else {
                    diagnostics.push(diag(DIAG_UNRESOLVED, "type name cannot be resolved"));
                }
            } else if money_constructor && !arguments.is_empty() {
                validate_money_currency(arguments, scope, generic_names, diagnostics);
            } else {
                for argument in arguments {
                    validate_type_annotation(argument, scope, generic_names, diagnostics);
                }
            }
        }
        TypeExpr::Optional { inner, .. } | TypeExpr::List { inner, .. } => {
            validate_type_annotation(inner, scope, generic_names, diagnostics);
        }
        // A product type is a money rate such as `Money<GBP> / kWh`, whose
        // divisor is a declared unit (ORNA-MONEY-003). Any other product is
        // rejected rather than becoming the internal error sentinel, which
        // would silently bypass later checks.
        TypeExpr::Product { lhs, rhs, .. } => {
            validate_type_annotation(lhs, scope, generic_names, diagnostics);
            if matches!(type_of(type_expr), Type::MoneyPerUnit { .. }) {
                validate_unit_annotation(rhs, scope, diagnostics);
            } else {
                validate_type_annotation(rhs, scope, generic_names, diagnostics);
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "type product is not a supported static type",
                ));
            }
        }
        TypeExpr::Record { fields, .. } => {
            for (_, field, _) in fields {
                validate_type_annotation(field, scope, generic_names, diagnostics);
            }
        }
        TypeExpr::Tuple { elements, .. } => {
            for element in elements {
                validate_type_annotation(element, scope, generic_names, diagnostics);
            }
        }
        TypeExpr::Function {
            parameters, result, ..
        } => {
            for parameter in parameters {
                validate_type_annotation(parameter, scope, generic_names, diagnostics);
            }
            validate_type_annotation(result, scope, generic_names, diagnostics);
        }
    }
}

/// Stored fields (no initialiser, or a `=` default) persist their value, so
/// their type must lie in the serialisable subset (29-formats). Secrets are
/// never committed (ORNA-SECRET-001), and a live relation, stream or function
/// is not a finite stored value. Computed (`=>`) fields are never stored and
/// are not checked here.
pub(crate) fn validate_stored_field_type(
    type_expr: &TypeExpr,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let resolved = resolved_type_of(type_expr, scope);
    if contains_secret_value_type_in(
        &resolved,
        &scope.nominal_rows,
        &scope.type_aliases,
        &scope.secret_types,
        &mut BTreeSet::new(),
    ) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "secret values cannot be stored in table fields",
        ));
    } else if contains_non_serialisable_type(&resolved) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "stored table field type must be serialisable",
        ));
    }
}

pub(crate) fn contains_non_serialisable_type(ty: &Type) -> bool {
    match ty {
        Type::Relation(_) | Type::Stream(_) | Type::Function { .. } => true,
        Type::List(inner) | Type::Range(inner) | Type::Optional(inner) => {
            contains_non_serialisable_type(inner)
        }
        Type::Record(fields) => fields.values().any(contains_non_serialisable_type),
        Type::Tuple(values) => values.iter().any(contains_non_serialisable_type),
        Type::Applied { base, arguments } => {
            matches!(base.as_str(), "Stream" | "Relation" | "Query")
                || arguments.iter().any(contains_non_serialisable_type)
        }
        _ => false,
    }
}

/// `Money<C>` takes one currency: a nominal type that implements the core
/// `Currency` protocol, or a generic parameter bounded by it
/// (ORNA-MONEY-004). An unbound name stays unresolved.
pub(crate) fn validate_money_currency(
    arguments: &[TypeExpr],
    scope: &Scope,
    generic_names: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let [
        currency @ TypeExpr::Name {
            path,
            arguments: currency_arguments,
            ..
        },
    ] = arguments
    else {
        diagnostics.push(diag(DIAG_UNRESOLVED, "type name cannot be resolved"));
        return;
    };
    if !currency_arguments.is_empty()
        || !annotation_type_name_is_declared(path, scope, generic_names)
    {
        diagnostics.push(diag(DIAG_UNRESOLVED, "type name cannot be resolved"));
    } else if !is_currency_type(&resolved_type_of(currency, scope), scope) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "Money currency must be a nominal type that implements Currency",
        ));
    }
}
