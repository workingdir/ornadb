//! Expression inference.

use std::collections::{BTreeMap, BTreeSet};

use num_traits::{Signed, Zero};
use orna_syntax::{
    AssignmentOperator, AssignmentTarget, ControlKind, Expr, LambdaParameter, LiteralKind, Pattern,
    Statement, SyntaxSpan, TypeExpr,
};
use orna_value::Diagnostic;

use super::*;

impl<'a> ControlParts<'a> {
    pub(crate) fn new(
        binding: Option<&'a Pattern>,
        condition: Option<&'a Expr>,
        body: Option<&'a Expr>,
        arms: &'a [orna_syntax::CaseArm],
        alternate: Option<&'a Expr>,
    ) -> Self {
        Self {
            binding,
            condition,
            body,
            arms,
            alternate,
        }
    }
}

/// Routes one `Expr::Control` to its form's inference. The match is exhaustive
/// so a new control kind cannot fall through to a generic diagnostic.
pub(crate) fn infer_control(
    kind: ControlKind,
    parts: ControlParts<'_>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    match kind {
        ControlKind::If => infer_if(parts, None, scope, local, diagnostics),
        ControlKind::Case => infer_case(parts, scope, local, diagnostics),
        ControlKind::For => infer_for(parts, scope, local, diagnostics),
        ControlKind::While => infer_while(parts, scope, local, diagnostics),
        // `break` may carry a loop's value (ORNA-CFLOW-008) and this slice does
        // not track break values, so typing `loop` as `Unit` would claim more
        // than the slice can prove.
        ControlKind::Loop => unsupported_expression(diagnostics),
    }
}

pub(crate) fn unsupported_expression(diagnostics: &mut Vec<Diagnostic>) -> Inferred {
    diagnostics.push(diag(
        DIAG_UNSUPPORTED,
        "expression form is outside this semantic slice",
    ));
    Inferred::error()
}

/// Types one qualified field access or member reference.
pub(crate) fn infer_field(
    expr: &Expr,
    base: &Expr,
    name: &str,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Some(path) = qualified_path(expr)
        && let Some(inferred) = infer_system_path(&path, scope, diagnostics)
    {
        return inferred;
    }
    if let Some(projection) = infer_table_projection(expr, scope, local) {
        return projection;
    }
    if let Some(variant) = infer_enum_variant_reference(expr, scope, local, diagnostics) {
        return variant;
    }
    if let Some(path) = qualified_path(expr)
        && scope.modules.contains_key(path[0])
        && !local.contains_key(path[0])
    {
        if let Some(table) = table_symbol(expr, scope, local) {
            return Inferred {
                ty: Type::Relation(Box::new(table.ty.clone())),
                effects: EffectSummary {
                    effects: BTreeSet::from([EffectKind::TableRead]),
                    opaque: false,
                    may_fail: true,
                },
            };
        }
        return resolve_qualified_module_member(&path, scope, diagnostics);
    }
    if let Some(path) = qualified_path(expr)
        && path.first() == Some(&"sys")
        && path
            .get(1)
            .is_some_and(|member| matches!(*member, "runtime" | "runtime_info"))
    {
        diagnostics.push(diag(
            DIAG_LEGACY_SYS_RUNTIME,
            "`sys.runtime` was renamed to `sys.rt`",
        ));
        return Inferred::error();
    }
    let diagnostics_before_base = diagnostics.len();
    let base = infer(base, scope, local, diagnostics);
    if base.ty == Type::Error && diagnostics.len() == diagnostics_before_base {
        return Inferred::error_with(base.effects);
    }
    if let Some(inferred) = infer_historical_member(&base.ty, name, scope, diagnostics) {
        return Inferred {
            ty: inferred.ty,
            effects: {
                let mut effects = base.effects;
                effects.join(&inferred.effects);
                effects
            },
        };
    }
    if let Some(ty) = infer_system_member(&base.ty, name) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    if let Some(message) = legacy_sys_admin_message(&base.ty, name) {
        diagnostics.push(diag(DIAG_SYS_ADMIN_METHOD, message));
        return Inferred::error_with(base.effects);
    }
    if let Type::Named(system_type) = &base.ty
        && system_api::embedded_system_api().describes_type(system_type)
    {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable system member is described but not implemented by this semantic slice",
        ));
        return Inferred::error_with(base.effects);
    }
    if let Some(ty) = infer_text_member(&base.ty, name) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    if name == "display" && contains_secret_value_type(&base.ty) {
        diagnostics.push(diag(
            DIAG_SECRET_DISPLAY,
            "secret values cannot be displayed",
        ));
        return Inferred::error_with(base.effects);
    }
    if let Some(ty) = infer_numeric_member(&base.ty, name) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    if let Some(ty) = infer_currency_suffix(&base.ty, name, scope, diagnostics) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    if let Some(ty) = infer_numeric_postfix(&base.ty, name) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    if let Some(ty) = infer_unit_suffix(&base.ty, name, scope, diagnostics) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    if let Some(ty) = infer_refined_member(&base.ty, name, scope) {
        return Inferred {
            ty,
            effects: base.effects,
        };
    }
    let ty = match &base.ty {
        Type::Record(fields) => fields.get(name).cloned().unwrap_or_else(|| {
            diagnostics.push(diag(DIAG_UNRESOLVED, "record field cannot be resolved"));
            Type::Error
        }),
        Type::Named(nominal) => {
            if scope
                .nominal_private_fields
                .get(nominal)
                .is_some_and(|fields| fields.contains(name))
                && scope.private_field_owner.as_deref() != Some(nominal.as_str())
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "private nominal field is inaccessible outside its owning implementation",
                ));
                Type::Error
            } else {
                scope
                    .table_rows
                    .get(nominal)
                    .or_else(|| scope.nominal_rows.get(nominal))
                    .and_then(|row| match row {
                        Type::Record(fields) => fields.get(name).cloned(),
                        _ => None,
                    })
                    .or_else(|| error_value_field_type(nominal, name))
                    .unwrap_or_else(|| {
                        diagnostics.push(diag(DIAG_TYPE, "field access requires a record"));
                        Type::Error
                    })
            }
        }
        _ => {
            diagnostics.push(diag(DIAG_TYPE, "field access requires a record"));
            Type::Error
        }
    };
    Inferred {
        ty,
        effects: base.effects,
    }
}

pub(crate) fn infer_lambda(
    parameters: &[LambdaParameter],
    body: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut locals = local.clone();
    let mut types = Vec::new();
    for parameter in parameters {
        let ty = parameter
            .annotation
            .as_ref()
            .map(type_of)
            .unwrap_or_else(|| {
                if let Pattern::Name(name, _) = &parameter.pattern
                    && let Some(ty) = inferred_lambda_parameter_type(body, name, scope, local)
                {
                    ty
                } else {
                    diagnostics.push(diag(
                        DIAG_ANNOTATION,
                        "lambda parameter needs a static annotation",
                    ));
                    Type::Error
                }
            });
        let ty = scope_snapshot_ref_parameter_pattern(&parameter.pattern, &ty);
        if ty == Type::Error {
            // Keep an underconstrained lambda parameter in scope as
            // an error-typed local. This lets the annotation
            // diagnostic point at the useful site without turning
            // every use of that parameter into an unrelated
            // unresolved-name diagnostic.
            if let Pattern::Name(name, _) = &parameter.pattern {
                insert_local_binding(name, Type::Error, &mut locals, diagnostics);
                if let Some(symbol) = locals.get_mut(name) {
                    symbol.kind = SymbolKind::Parameter;
                }
            }
        } else {
            bind_parameter_pattern(
                &parameter.pattern,
                ty.clone(),
                scope,
                &mut locals,
                diagnostics,
            );
        }
        types.push(ty);
    }
    let value = if matches!(
        body,
        Expr::Block {
            statements,
            tail: None,
            ..
        } if statements.is_empty()
    ) {
        Inferred::pure(Type::Record(BTreeMap::new()))
    } else {
        infer_function_body(body, None, scope, &locals, diagnostics)
    };
    Inferred {
        ty: Type::Function {
            parameters: types,
            default_parameters: BTreeSet::new(),
            parameter_names: parameters
                .iter()
                .map(|parameter| match &parameter.pattern {
                    Pattern::Name(name, _) => Some(name.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            result: Box::new(value.ty),
        },
        effects: value.effects,
    }
}

pub(crate) fn infer_call(
    callee: &Expr,
    arguments: &[orna_syntax::Argument],
    span: &SyntaxSpan,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if matches!(callee, Expr::Name { text, .. } if text == "fail")
        && !local.contains_key("fail")
        && !scope.names.contains_key("fail")
    {
        return infer_fail_call(arguments, scope, local, diagnostics);
    }
    if matches!(callee, Expr::Name { text, .. } if text == "error")
        && !local.contains_key("error")
        && !scope.names.contains_key("error")
    {
        return infer_error_call(arguments, scope, local, diagnostics);
    }
    if matches!(callee, Expr::Name { text, .. } if text == "Some")
        && !local.contains_key("Some")
        && !scope.names.contains_key("Some")
    {
        if let [argument] = arguments
            && argument.name.is_none()
        {
            let value = infer(&argument.value, scope, local, diagnostics);
            return Inferred {
                ty: Type::Optional(Box::new(value.ty)),
                effects: value.effects,
            };
        }
        let mut effects = EffectSummary::default();
        for argument in arguments {
            let value = infer(&argument.value, scope, local, diagnostics);
            effects.join(&value.effects);
        }
        diagnostics.push(diag(
            DIAG_TYPE,
            "Some requires exactly one positional value",
        ));
        return Inferred::error_with(effects);
    }
    if cross_database_write(callee, scope) {
        diagnostics.push(diag(
            DIAG_CROSS_DATABASE_WRITE,
            "cross-database writes are not atomic/supported",
        ));
        return Inferred::error();
    }
    if matches!(callee, Expr::Name { text, .. } if text == "parallel")
        && let [argument] = arguments
        && let Expr::List { elements, .. } = &argument.value
    {
        let mut consumers = BTreeSet::new();
        for element in elements {
            if let Some(path) = qualified_path(element)
                && !consumers.insert(path.join("."))
            {
                diagnostics.push(diag(
                    DIAG_AMBIGUOUS_DURABLE_CONSUMER,
                    "same durable source consumed twice without distinct consumer identity",
                ));
                break;
            }
        }
    }
    if matches!(callee, Expr::Name { text, .. } if text == "merge_streams")
        && let [argument] = arguments
        && let Expr::List { elements, .. } = &argument.value
        && elements.len() > 1
    {
        diagnostics.push(diag(
            DIAG_DURABLE_CONSUMER_SOURCE_ROOTS,
            "a durable consumer function may own only one checkpointed source root; extract separate named consumer functions",
        ));
    }
    if matches!(
        callee,
        Expr::Name { text, .. }
            if matches!(text.as_str(), "Ok" | "Err")
                && local.get(text).or_else(|| scope.names.get(text)).is_none()
    ) {
        diagnostics.push(diag(
            DIAG_LEGACY_RESULT,
            "Result/Ok/Err control plumbing was removed; return the success type directly",
        ));
        return Inferred::error();
    }
    if qualified_path(callee).as_deref() == Some(["sys", "storage"].as_slice()) {
        diagnostics.push(diag(
            DIAG_SYS_STORAGE_CALL,
            "`sys.storage` is a grouping namespace; use `sys.Storage` or `sys.admin` storage functions",
        ));
        return Inferred::error();
    }
    if qualified_path(callee).as_deref() == Some(["sys", "Commit", "insert"].as_slice()) {
        diagnostics.push(diag(DIAG_SYS_COMMIT_READ_ONLY, "sys.Commit is read-only"));
        return Inferred::error();
    }
    if let Some(path) = qualified_path(callee)
        && let Some(inferred) = infer_ui_action_call(&path, arguments, scope, local, diagnostics)
    {
        return inferred;
    }
    if let Some(path) = qualified_path(callee)
        && path.first() == Some(&"sys")
        && let Some(inferred) =
            infer_descriptor_system_call(&path, arguments, None, scope, local, diagnostics)
    {
        return inferred;
    }
    if let Some(currency) = money_constructor_currency(callee)
        && let Some(inferred) = infer_money_constructor(
            Type::Named(canonical_nominal_name(currency, scope)),
            arguments,
            scope,
            local,
            diagnostics,
        )
    {
        return inferred;
    }
    if let Some(inferred) = infer_stream_from_list(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) = infer_collection_call(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) =
        infer_nominal_conversion_call(callee, arguments, scope, local, diagnostics)
    {
        return inferred;
    }
    if let Some(inferred) = infer_statistics_call(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) = infer_parallel_call(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) = infer_race_call(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) = infer_timeout_call(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) = infer_relation_member_call(callee, arguments, scope, local, diagnostics)
    {
        return inferred;
    }
    if let Some(inferred) = infer_table_operation(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) = infer_codec_decode_call(callee, arguments, scope, local, diagnostics) {
        return inferred;
    }
    if let Some(inferred) =
        infer_local_generic_call(callee, None, arguments, scope, local, diagnostics)
    {
        return inferred;
    }
    let presentation_boundary =
        qualified_path(callee).is_some_and(|path| is_secret_sensitive_presentation_path(&path));
    let intrinsic = intrinsic_call_effects(callee);
    let callee = infer(callee, scope, local, diagnostics);
    let mut effects = callee.effects.clone();
    effects.join(&intrinsic);
    let call_parameters =
        callable_function_type(&callee.ty).and_then(|function_type| match function_type {
            Type::Function {
                parameters,
                parameter_names,
                ..
            } => Some((parameters.as_slice(), parameter_names.as_deref())),
            _ => None,
        });
    let values = arguments
        .iter()
        .enumerate()
        .map(|(index, a)| {
            let x = if a.name.as_deref() == Some("as") {
                infer_type_argument(&a.value)
                    .unwrap_or_else(|| infer(&a.value, scope, local, diagnostics))
            } else if let Some((parameters, parameter_names)) = call_parameters
                && let Some(expected) =
                    expected_call_parameter(parameters, parameter_names, arguments, index)
            {
                infer_contextual(&a.value, expected, scope, local, diagnostics)
            } else {
                infer(&a.value, scope, local, diagnostics)
            };
            effects.join(&x.effects);
            x.ty
        })
        .collect::<Vec<_>>();
    if presentation_boundary && values.iter().any(contains_secret_value_type) {
        diagnostics.push(diag(DIAG_TYPE, "secret values cannot be presented"));
        return Inferred::error_with(effects);
    }
    let historical_context = historical_callable_context(&callee.ty).cloned();
    match callable_function_type(&callee.ty).cloned() {
        Some(Type::Function {
            parameters,
            parameter_names,
            result,
            default_parameters,
        }) => {
            check_call_arguments(
                &parameters,
                parameter_names.as_deref(),
                &default_parameters,
                arguments,
                &values,
                None,
                diagnostics,
            );
            let historical_database =
                specialize_historical_database_result(&result, &values, arguments, local);
            let result = historical_database.unwrap_or_else(|| {
                specialize_dynamic_parameter_snapshot_contexts(
                    &result,
                    DynamicParameters {
                        formal_parameters: &parameters,
                        default_parameters: &default_parameters,
                        parameter_names: parameter_names.as_deref(),
                        arguments,
                        argument_types: &values,
                        local,
                        call_span: span,
                        historical_context: historical_context.as_ref(),
                    },
                )
            });
            Inferred {
                ty: historical_context.as_ref().map_or_else(
                    || result.clone(),
                    |snapshot| bind_historical_closure_context(snapshot, &result),
                ),
                effects,
            }
        }
        _ if callee.ty == Type::Error => Inferred::error_with(effects),
        _ => {
            diagnostics.push(diag(DIAG_TYPE, "call requires a statically known function"));
            Inferred::error_with(effects)
        }
    }
}

/// Whether `callee` is a table write reached through an attached database
/// alias. An attached database keeps its own transaction log, so Orna must not
/// claim the write is atomic (ORNA-ATTACH-002, source/08-relations.md:131).
pub(crate) fn cross_database_write(callee: &Expr, scope: &Scope) -> bool {
    let Some(path) = qualified_path(callee) else {
        return false;
    };
    let Some((member, target)) = path.split_last() else {
        return false;
    };
    // The alias roots a qualified table path, so a write through it has at
    // least an alias and a table before the write member.
    target.len() >= 2
        && scope.attached_databases.contains(target[0])
        && matches!(*member, "insert" | "upsert" | "update" | "delete" | "rekey")
}

pub(crate) fn infer_generic_call(
    callee: &Expr,
    type_arguments: &[TypeExpr],
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Some(path) = qualified_path(callee)
        && path.first() == Some(&"sys")
        && let Some(inferred) = infer_descriptor_system_call(
            &path,
            arguments,
            Some(type_arguments),
            scope,
            local,
            diagnostics,
        )
    {
        return inferred;
    }
    if let Some(currency) = money_generic_constructor_currency(callee, type_arguments, scope)
        && let Some(inferred) =
            infer_money_constructor(currency, arguments, scope, local, diagnostics)
    {
        return inferred;
    }
    if let Some(inferred) = infer_local_generic_call(
        callee,
        Some(type_arguments),
        arguments,
        scope,
        local,
        diagnostics,
    ) {
        return inferred;
    }
    let intrinsic = intrinsic_call_effects(callee);
    let mut effects = infer(callee, scope, local, diagnostics).effects;
    effects.join(&intrinsic);
    for argument in arguments {
        effects.join(&infer(&argument.value, scope, local, diagnostics).effects);
    }
    diagnostics.push(diag(
        DIAG_UNSUPPORTED,
        "generic calls are outside this semantic slice",
    ));
    Inferred::error_with(effects)
}

pub(crate) fn infer_range(
    lower: Option<&Expr>,
    upper: Option<&Expr>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let endpoints = [lower, upper]
        .into_iter()
        .flatten()
        .map(|endpoint| infer(endpoint, scope, local, diagnostics))
        .collect::<Vec<_>>();
    let mut effects = EffectSummary::default();
    for endpoint in &endpoints {
        effects.join(&endpoint.effects);
    }
    let ty = match endpoints.as_slice() {
        [] => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "an untyped range needs at least one endpoint or a range context",
            ));
            Type::Error
        }
        [one] => {
            if !is_ordered_range_bound(&one.ty) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "range bounds must have the same ordered type",
                ));
                Type::Error
            } else {
                Type::Range(Box::new(one.ty.clone()))
            }
        }
        [left, right] if left.ty == right.ty && is_ordered_range_bound(&left.ty) => {
            Type::Range(Box::new(left.ty.clone()))
        }
        _ => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "range bounds must have the same ordered type",
            ));
            Type::Error
        }
    };
    Inferred { ty, effects }
}

pub(crate) fn infer_binary(
    lhs: &Expr,
    op: &str,
    rhs: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if op == "|" {
        return infer_success_pipeline(lhs, rhs, scope, local, diagnostics);
    }
    if op == "|?" {
        return infer_recovery_pipeline(lhs, rhs, scope, local, diagnostics);
    }
    let equality = matches!(op, "==" | "!=");
    let (left, right) = if equality && is_empty_list_literal(lhs) && !is_empty_list_literal(rhs) {
        let right = infer(rhs, scope, local, diagnostics);
        let left = match &right.ty {
            Type::List(_) => infer_contextual(lhs, &right.ty, scope, local, diagnostics),
            _ => infer(lhs, scope, local, diagnostics),
        };
        (left, right)
    } else {
        let left = infer(lhs, scope, local, diagnostics);
        let right = if op == "??" {
            match &left.ty {
                Type::Optional(inner) => infer_contextual(rhs, inner, scope, local, diagnostics),
                _ => infer(rhs, scope, local, diagnostics),
            }
        } else if equality && is_empty_list_literal(rhs) && matches!(left.ty, Type::List(_)) {
            infer_contextual(rhs, &left.ty, scope, local, diagnostics)
        } else {
            infer(rhs, scope, local, diagnostics)
        };
        (left, right)
    };
    let mut effects = left.effects;
    effects.join(&right.effects);
    if matches!(op, "==" | "!=")
        && (matches!(left.ty, Type::Relation(_)) || matches!(right.ty, Type::Relation(_)))
    {
        diagnostics.push(diag(
            DIAG_RELATION_EQUALITY_AMBIGUOUS,
            "relation equality is ambiguous; choose sequence or row-set comparison explicitly",
        ));
        return Inferred::error_with(effects);
    }
    if matches!(op, ".." | "..=") {
        let ty = if left.ty == right.ty && is_ordered_range_bound(&left.ty) {
            Type::Range(Box::new(left.ty))
        } else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "range bounds must have the same ordered type",
            ));
            Type::Error
        };
        return Inferred { ty, effects };
    }
    if op == "in" {
        let ty = match &right.ty {
            Type::Range(element) => {
                require_same(element, &left.ty, diagnostics);
                Type::Bool
            }
            _ => {
                diagnostics.push(diag(DIAG_TYPE, "membership requires an ordered range"));
                Type::Error
            }
        };
        return Inferred { ty, effects };
    }
    if op == "??" {
        let ty = match (&left.ty, &right.ty) {
            (Type::Optional(_), Type::Error) | (Type::Error, _) => Type::Error,
            (Type::Optional(inner), right) if types_match(inner, right) => (**inner).clone(),
            _ => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "coalesce fallback must match the optional value type",
                ));
                Type::Error
            }
        };
        return Inferred { ty, effects };
    }
    if matches!(op, "&&" | "||") {
        require_same(&Type::Bool, &left.ty, diagnostics);
        require_same(&Type::Bool, &right.ty, diagnostics);
        return Inferred {
            ty: if left.ty == Type::Bool && right.ty == Type::Bool {
                Type::Bool
            } else {
                Type::Error
            },
            effects,
        };
    }
    let ty = if matches!(op, "==" | "!=") {
        if left.ty == Type::Error || right.ty == Type::Error {
            Type::Error
        } else if !types_match(&left.ty, &right.ty) && !types_match(&right.ty, &left.ty) {
            diagnostics.push(type_mismatch(
                DIAG_TYPE,
                "static types are incompatible",
                &left.ty,
                &right.ty,
            ));
            Type::Error
        } else {
            Type::Bool
        }
    } else if matches!(op, "<" | "<=" | ">" | ">=") {
        if left.ty == Type::Error || right.ty == Type::Error {
            Type::Error
        } else if left.ty != right.ty || !is_evaluator_ordered_comparison_type(&left.ty) {
            diagnostics.push(type_mismatch(
                DIAG_TYPE,
                "static types are incompatible",
                &left.ty,
                &right.ty,
            ));
            Type::Error
        } else {
            Type::Bool
        }
    } else {
        binary_arithmetic_type(op, &left.ty, &right.ty, scope, diagnostics)
    };
    Inferred { ty, effects }
}

pub(crate) fn infer_block(
    statements: &[Statement],
    tail: Option<&Expr>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut locals = local.clone();
    let mut effects = EffectSummary::default();
    let mut final_control = None;
    for (index, statement) in statements.iter().enumerate() {
        match statement {
            Statement::Let {
                pattern,
                annotation,
                value,
                ..
            } => {
                let x = if let Some(annotation) = annotation {
                    validate_type_annotation(annotation, scope, &BTreeSet::new(), diagnostics);
                    let expected = resolved_type_of(annotation, scope);
                    let x = infer_contextual(value, &expected, scope, &locals, diagnostics);
                    require_same(&expected, &x.ty, diagnostics);
                    x
                } else {
                    infer(value, scope, &locals, diagnostics)
                };
                effects.join(&x.effects);
                // A local alias of a typed pin keeps the source
                // parameter identity for later rebinding and capture.
                let ty = specialize_snapshot_ref_parameter(value, &x.ty, &locals);
                if ty == Type::Bottom {
                    final_control = Some(Inferred::pure(ty));
                    break;
                }
                bind_pattern(pattern, ty, scope, &mut locals, diagnostics);
            }
            Statement::Assert { value, .. } => {
                let x = infer(value, scope, &locals, diagnostics);
                effects.join(&x.effects);
                require_same(&Type::Bool, &x.ty, diagnostics);
            }
            Statement::Expression { value, .. } => {
                let x = infer(value, scope, &locals, diagnostics);
                effects.join(&x.effects);
                if x.ty == Type::Bottom {
                    final_control = Some(x);
                    break;
                }
            }
            Statement::Control { value, .. } => {
                let x = infer(value, scope, &locals, diagnostics);
                effects.join(&x.effects);
                if x.ty == Type::Bottom {
                    final_control = Some(x);
                    break;
                }
                if index + 1 == statements.len() && tail.is_none() {
                    final_control = Some(x);
                }
            }
            Statement::Assignment {
                target,
                operator,
                value,
                ..
            } => effects.join(&infer_assignment(
                target,
                operator,
                value,
                scope,
                &mut locals,
                diagnostics,
            )),
            Statement::Break { value, .. } => {
                if let Some(value) = value {
                    let x = infer(value, scope, &locals, diagnostics);
                    effects.join(&x.effects);
                }
            }
            Statement::Continue { .. } => {}
            Statement::Return { value, .. } => {
                let returned =
                    infer_return_statement(statement, value.as_ref(), scope, &locals, diagnostics);
                effects.join(&returned.effects);
                final_control = Some(Inferred {
                    ty: Type::Bottom,
                    effects: returned.effects,
                });
                break;
            }
        }
    }
    // A block without a final expression completes with `Unit`
    // (ORNA-CFLOW-001); `Null` is reserved for the `null` literal.
    let tail = final_control
        .or_else(|| tail.map(|x| infer(x, scope, &locals, diagnostics)))
        .unwrap_or(Inferred::pure(Type::Unit));
    effects.join(&tail.effects);
    Inferred {
        ty: tail.ty,
        effects,
    }
}

/// Types a prefix operator. `!` requires `Bool` because Orna has no implicit
/// truthiness (ORNA-CFLOW-009); a numeric sign keeps its operand's type.
pub(crate) fn infer_unary_operator(
    op: &str,
    operand: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    if matches!(operand, Type::Error | Type::Bottom) {
        return operand.clone();
    }
    if op == "!" {
        require_same(&Type::Bool, operand, diagnostics);
        return if *operand == Type::Bool {
            Type::Bool
        } else {
            Type::Error
        };
    }
    let signed = matches!(op, "-" | "+")
        && (matches!(
            operand,
            Type::Int | Type::Decimal | Type::Float | Type::Quantity { affine: false, .. }
        ) || money_currency(operand).is_some()
            || unit_suffix_scalar(operand).is_some()
            // Negated elapsed time, and a negated instant used as a descending
            // sort key (`examples/valid/sys-run-history.orna`).
            || (op == "-"
                && (matches!(operand, Type::Instant)
                    || matches!(operand, Type::Named(name) if name == "std.DURATION"))));
    if signed {
        return operand.clone();
    }
    diagnostics.push(diag(
        DIAG_TYPE_MISMATCH,
        format!(
            "operator `{op}` is not defined for {}",
            diagnostic_type_name(operand)
        ),
    ));
    Type::Error
}

/// Types `+ - * / % ^` once both operands are inferred (ORNA-INFER-001,
/// ORNA-TYPE-008). Compound assignment applies the same table, because
/// `count += 1` means `count = count + 1` (ORNA-CFLOW-006). Orna has no
/// implicit numeric promotion and no string or calendar-date arithmetic, so
/// every pair the table does not admit is reported.
pub(crate) fn binary_arithmetic_type(
    op: &str,
    left: &Type,
    right: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    if matches!(left, Type::Error) || matches!(right, Type::Error) {
        return Type::Error;
    }
    // A diverging operand means the operation itself never completes.
    if matches!(left, Type::Bottom) || matches!(right, Type::Bottom) {
        return Type::Bottom;
    }
    if let Some(ty) = elapsed_time_operator_type(op, left, right) {
        return ty;
    }
    if op == "*" {
        if (is_binary_float(left) && is_money_rate(right))
            || (is_money_rate(left) && is_binary_float(right))
        {
            diagnostics.push(diag(
                DIAG_MONEY_BINARY_FLOAT,
                "binary Float cannot enter an exact Money calculation implicitly",
            ));
            return Type::Error;
        }
        if let Some(ty) = reduce_exact_money_rate(left, right) {
            return ty;
        }
    }
    if op == "+"
        && let Some((code, message)) = incompatible_addition_message(left, right)
    {
        diagnostics.push(diag(code, message));
        return Type::Error;
    }
    // Subtracting is adding the negation, so it needs the same conversion
    // (ORNA-MONEY-002).
    if op == "-"
        && let (Some(left_currency), Some(right_currency)) =
            (money_currency(left), money_currency(right))
        && left_currency != right_currency
        && !matches!(left_currency, Type::Error)
        && !matches!(right_currency, Type::Error)
    {
        diagnostics.push(diag(
            DIAG_CURRENCY_MISMATCH,
            "cannot subtract different currencies without conversion",
        ));
        return Type::Error;
    }
    if matches!(op, "+" | "-")
        && let Some(message) = temporal_quantity_mismatch(op, left, right, scope)
    {
        diagnostics.push(diag(DIAG_TIME_ENERGY_ADD, message));
        return Type::Error;
    }
    if let Some(ty) = arithmetic_operator_type(op, left, right, scope, diagnostics) {
        return ty;
    }
    diagnostics.push(diag(
        DIAG_TYPE_MISMATCH,
        format!(
            "operator `{op}` is not defined for {} and {}",
            diagnostic_type_name(left),
            diagnostic_type_name(right),
        ),
    ));
    Type::Error
}

/// Elapsed `Duration` arithmetic is distinct from calendar periods: it can
/// shift an `Instant` or combine with another `Duration` (ORNA-TIME-004).
pub(crate) fn elapsed_time_operator_type(op: &str, left: &Type, right: &Type) -> Option<Type> {
    let duration = Type::Named("std.DURATION".into());
    match (op, left, right) {
        ("+", Type::Instant, right) if right == &duration => Some(Type::Instant),
        ("+", left, Type::Instant) if left == &duration => Some(Type::Instant),
        ("-", Type::Instant, right) if right == &duration => Some(Type::Instant),
        ("-", Type::Instant, Type::Instant) => Some(duration),
        ("+" | "-", left, right) if left == &duration && right == &duration => Some(duration),
        // Duration scaling accepts exact numeric factors only; the runtime
        // rejects results finer than its nanosecond grid.
        ("*" | "/", left, Type::Int | Type::Decimal) if left == &duration => Some(duration),
        ("*", Type::Int | Type::Decimal, right) if right == &duration => Some(duration),
        // A look-back window such as `now() - 7.days`.
        ("-", Type::Instant, period) if is_calendar_period(period) => Some(Type::Instant),
        _ => None,
    }
}

/// The admitted arithmetic table. `None` is an undefined operand pair; a rule
/// with its own published diagnostic reports it and returns `Error`.
pub(crate) fn arithmetic_operator_type(
    op: &str,
    left: &Type,
    right: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    match (left, right) {
        // Int, Decimal and Float never mix implicitly (ORNA-NUM-001..003).
        (Type::Int, Type::Int) | (Type::Decimal, Type::Decimal) | (Type::Float, Type::Float) => {
            Some(left.clone())
        }
        _ if money_currency(left).is_some() || money_currency(right).is_some() => {
            money_operator_type(op, left, right, diagnostics)
        }
        (Type::MoneyPerUnit { .. }, Type::MoneyPerUnit { .. }) => {
            (matches!(op, "+" | "-") && left == right).then(|| left.clone())
        }
        // A rate scales by an exact number and stays a rate.
        (Type::MoneyPerUnit { .. }, Type::Int | Type::Decimal) if matches!(op, "*" | "/") => {
            Some(left.clone())
        }
        (Type::Int | Type::Decimal, Type::MoneyPerUnit { .. }) if op == "*" => Some(right.clone()),
        (Type::Quantity { .. }, _) | (_, Type::Quantity { .. }) => {
            quantity_operator_type(op, left, right, scope, diagnostics)
        }
        _ => unit_suffix_operator_type(op, left, right, diagnostics),
    }
}

/// Money keeps its currency and exact decimal semantics: same-currency
/// addition and exact scaling only (ORNA-MONEY-001..003).
pub(crate) fn money_operator_type(
    op: &str,
    left: &Type,
    right: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    if is_binary_float(left) || is_binary_float(right) {
        diagnostics.push(diag(
            DIAG_MONEY_BINARY_FLOAT,
            "binary Float cannot enter an exact Money calculation implicitly",
        ));
        return Some(Type::Error);
    }
    let money = |ty: &Type| money_currency(ty).is_some();
    let exact = |ty: &Type| matches!(ty, Type::Int | Type::Decimal);
    match op {
        "+" | "-"
            if money(left)
                && money(right)
                && (types_match(left, right) || types_match(right, left)) =>
        {
            Some(left.clone())
        }
        "*" | "/" if money(left) && exact(right) => Some(left.clone()),
        "*" if exact(left) && money(right) => Some(right.clone()),
        _ => None,
    }
}

/// Declared-unit quantities follow dimension-basis algebra (ORNA-UNIT-001,
/// ORNA-UNIT-004) and the affine point/vector rules (ORNA-UNIT-005).
pub(crate) fn quantity_operator_type(
    op: &str,
    left: &Type,
    right: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    match (op, left, right) {
        ("+" | "-", Type::Quantity { .. }, Type::Quantity { .. }) => {
            quantity_sum_type(op, left, right, scope, diagnostics)
        }
        ("*" | "/", Type::Quantity { .. }, Type::Quantity { .. }) => {
            quantity_product_type(op == "/", left, right)
        }
        ("%", Type::Quantity { affine: false, .. }, _) if left == right => Some(left.clone()),
        (
            "*" | "/",
            Type::Quantity {
                numeric,
                affine: false,
                ..
            },
            scalar,
        ) if numeric.as_ref() == scalar => Some(left.clone()),
        (
            "*",
            scalar,
            Type::Quantity {
                numeric,
                affine: false,
                ..
            },
        ) if numeric.as_ref() == scalar => Some(right.clone()),
        ("/", scalar, Type::Quantity { numeric, .. }) if numeric.as_ref() == scalar => {
            let dimensionless = Type::Quantity {
                numeric: numeric.clone(),
                dimension_basis: BTreeMap::new(),
                scale: UnitFraction::one().public(),
                offset: UnitFraction::zero().public(),
                affine: false,
            };
            quantity_product_type(true, &dimensionless, right)
        }
        _ => None,
    }
}

/// `+` and `-` over quantities (ORNA-UNIT-001, ORNA-UNIT-004). Dimensions
/// must agree; linear units of different scales meet exactly at their common
/// unit, so `1.km + 1.m` is `1001.m`. Absolute affine values follow the
/// point/vector algebra of ORNA-UNIT-005 within one unit.
pub(crate) fn quantity_sum_type(
    op: &str,
    left: &Type,
    right: &Type,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    let (
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            offset,
            affine,
        },
        Type::Quantity {
            numeric: right_numeric,
            dimension_basis: right_basis,
            scale: right_scale,
            offset: right_offset,
            affine: right_affine,
        },
    ) = (left, right)
    else {
        return None;
    };
    if dimension_basis != right_basis {
        diagnostics.push(diag(
            DIAG_INCOMPATIBLE_DIMENSIONS,
            incompatible_dimensions_message(
                op,
                &dimension_name(dimension_basis, scope),
                &dimension_name(right_basis, scope),
            ),
        ));
        return Some(Type::Error);
    }
    // Absolute<U> + Absolute<U> -> error, whatever the operands' numerics.
    if op == "+" && *affine && *right_affine {
        diagnostics.push(diag(
            DIAG_AFFINE_TEMPERATURE_ADD,
            "cannot add two absolute affine temperatures",
        ));
        return Some(Type::Error);
    }
    if numeric != right_numeric {
        return None;
    }
    let zero = UnitFraction::zero().public();
    if *affine || *right_affine || *offset != zero || *right_offset != zero {
        if scale != right_scale {
            return None;
        }
        return match (op, *affine, *right_affine) {
            // Absolute<U> - Absolute<U> -> Delta<U>
            ("-", true, true) if offset == right_offset => Some(Type::Quantity {
                numeric: numeric.clone(),
                dimension_basis: dimension_basis.clone(),
                scale: scale.clone(),
                offset: zero,
                affine: false,
            }),
            // Absolute<U> + Delta<U> and Absolute<U> - Delta<U> -> Absolute<U>
            (_, true, false) if *right_offset == zero => Some(left.clone()),
            // Delta<U> + Absolute<U> -> Absolute<U>
            ("+", false, true) if *offset == zero => Some(right.clone()),
            (_, false, false) if offset == right_offset => Some(left.clone()),
            _ => None,
        };
    }
    let scale = common_scale(&unit_fraction(scale)?, &unit_fraction(right_scale)?)?;
    Some(Type::Quantity {
        numeric: numeric.clone(),
        dimension_basis: dimension_basis.clone(),
        scale: scale.public(),
        offset: zero,
        affine: false,
    })
}

/// The largest scale that measures both scales a whole number of times, so
/// converting either operand to it is exact: `km` and `m` meet at `m`.
pub(crate) fn common_scale(left: &UnitFraction, right: &UnitFraction) -> Option<UnitFraction> {
    let mut divisor = (&left.numerator * &right.denominator).abs();
    let mut remainder = (&right.numerator * &left.denominator).abs();
    while !remainder.is_zero() {
        let next = &divisor % &remainder;
        divisor = remainder;
        remainder = next;
    }
    UnitFraction::new(divisor, &left.denominator * &right.denominator)
}

/// Elapsed `Duration` values and calendar periods measure intrinsic time; a
/// declared quantity of another dimension cannot be added to them
/// (ORNA-UNIT-004).
pub(crate) fn temporal_quantity_mismatch(
    op: &str,
    left: &Type,
    right: &Type,
    scope: &Scope,
) -> Option<String> {
    let (time, quantity, time_first) = match (left, right) {
        (
            time,
            Type::Quantity {
                dimension_basis, ..
            },
        ) if is_intrinsic_time(time) => (time, dimension_basis, true),
        (
            Type::Quantity {
                dimension_basis, ..
            },
            time,
        ) if is_intrinsic_time(time) => (time, dimension_basis, false),
        _ => return None,
    };
    debug_assert!(is_intrinsic_time(time));
    let quantity = dimension_name(quantity, scope);
    if quantity == "Time" {
        return None;
    }
    Some(if time_first {
        incompatible_dimensions_message(op, "Time", &quantity)
    } else {
        incompatible_dimensions_message(op, &quantity, "Time")
    })
}

pub(crate) fn is_intrinsic_time(ty: &Type) -> bool {
    matches!(ty, Type::Named(name) if name == "std.DURATION") || is_calendar_period(ty)
}

pub(crate) fn incompatible_dimensions_message(op: &str, left: &str, right: &str) -> String {
    if op == "+" {
        format!("cannot add {left} and {right}")
    } else {
        format!("cannot subtract {right} from {left}")
    }
}

/// Names a reduced dimension for diagnostics: the declared dimension with
/// exactly this basis (the current module's when several match), otherwise
/// its base dimensions, such as `Length / Time`.
pub(crate) fn dimension_name(basis: &DimensionBasis, scope: &Scope) -> String {
    let declared = scope
        .source_dimensions
        .iter()
        .filter(|(_, candidate)| *candidate == basis)
        .map(|(identity, _)| identity)
        .collect::<Vec<_>>();
    let local = declared
        .iter()
        .copied()
        .filter(|identity| scope.current_namespace.as_ref() == Some(&identity.namespace))
        .collect::<Vec<_>>();
    if let [identity] = declared.as_slice() {
        return identity.name.clone();
    }
    if let [identity] = local.as_slice() {
        return identity.name.clone();
    }
    let term = |dimension: &SourceDimensionIdentity, exponent: u64| {
        if exponent == 1 {
            dimension.name.clone()
        } else {
            format!("{}^{exponent}", dimension.name)
        }
    };
    let numerator = basis
        .iter()
        .filter(|(_, exponent)| **exponent > 0)
        .map(|(dimension, exponent)| term(dimension, exponent.unsigned_abs()))
        .collect::<Vec<_>>();
    let denominator = basis
        .iter()
        .filter(|(_, exponent)| **exponent < 0)
        .map(|(dimension, exponent)| term(dimension, exponent.unsigned_abs()))
        .collect::<Vec<_>>();
    match (numerator.is_empty(), denominator.is_empty()) {
        (true, true) => "a dimensionless quantity".into(),
        (false, true) => numerator.join(" * "),
        (true, false) => format!("1 / {}", denominator.join(" / ")),
        (false, false) => format!("{} / {}", numerator.join(" * "), denominator.join(" / ")),
    }
}

/// `*` and `/` over non-affine quantities add or subtract dimension exponents
/// and multiply or divide the exact unit scales.
pub(crate) fn quantity_product_type(divide: bool, left: &Type, right: &Type) -> Option<Type> {
    let (
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            affine: false,
            ..
        },
        Type::Quantity {
            numeric: right_numeric,
            dimension_basis: right_basis,
            scale: right_scale,
            affine: false,
            ..
        },
    ) = (left, right)
    else {
        return None;
    };
    if numeric != right_numeric {
        return None;
    }
    let basis = combine_dimension_bases(dimension_basis, right_basis, divide)?;
    let left_scale = unit_fraction(scale)?;
    let right_scale = unit_fraction(right_scale)?;
    let scale = if divide {
        left_scale.divide(&right_scale)?
    } else {
        left_scale.multiply(&right_scale)?
    };
    if basis.is_empty() && scale == UnitFraction::one() {
        return Some(numeric.as_ref().clone());
    }
    Some(Type::Quantity {
        numeric: numeric.clone(),
        dimension_basis: basis,
        scale: scale.public(),
        offset: UnitFraction::zero().public(),
        affine: false,
    })
}

pub(crate) fn unit_fraction(rational: &SourceUnitRational) -> Option<UnitFraction> {
    UnitFraction::new(
        rational.numerator.parse().ok()?,
        rational.denominator.parse().ok()?,
    )
}

/// Calendar periods such as `7.days` keep their unit through same-unit
/// addition and exact scalar scaling; they have no declared dimensions, so
/// products of two periods are unsupported.
pub(crate) fn unit_suffix_operator_type(
    op: &str,
    left: &Type,
    right: &Type,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Type> {
    let left_scalar = unit_suffix_scalar(left);
    let right_scalar = unit_suffix_scalar(right);
    let same_unit = types_match(left, right) || types_match(right, left);
    match (op, left_scalar, right_scalar) {
        ("+" | "-" | "%", Some(_), Some(_)) if same_unit => Some(left.clone()),
        ("/", Some(scalar), Some(_)) if left == right => Some(scalar),
        ("*" | "/", Some(scalar), None) if &scalar == right => Some(left.clone()),
        ("*", None, Some(scalar)) if &scalar == left => Some(right.clone()),
        ("*" | "/", Some(_), Some(_)) | ("^", Some(_), None) => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "dimensional algebra is outside this semantic slice",
            ));
            Some(Type::Error)
        }
        ("/", None, Some(scalar)) if &scalar == left => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "dimensional algebra is outside this semantic slice",
            ));
            Some(Type::Error)
        }
        _ => None,
    }
}

/// The scalar of a calendar period such as `Int<days>`.
pub(crate) fn unit_suffix_scalar(ty: &Type) -> Option<Type> {
    match ty {
        Type::Applied { base, arguments } if arguments.len() == 1 => match base.as_str() {
            "Int" => Some(Type::Int),
            "Decimal" => Some(Type::Decimal),
            "Float" => Some(Type::Float),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn lambda_numeric_parameter_usage(expression: &Expr, name: &str) -> bool {
    fn direct_name(expression: &Expr, name: &str) -> bool {
        match expression {
            Expr::Name { text, .. } => text == name,
            Expr::Group { inner, .. } => direct_name(inner, name),
            _ => false,
        }
    }
    fn visit(expression: &Expr, name: &str) -> bool {
        match expression {
            Expr::Binary { lhs, op, rhs, .. } => {
                (matches!(
                    op.as_str(),
                    "+" | "-" | "*" | "/" | "%" | "<" | "<=" | ">" | ">=" | "==" | "!="
                ) && (direct_name(lhs, name) || direct_name(rhs, name)))
                    || visit(lhs, name)
                    || visit(rhs, name)
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => visit(base, name),
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, name)
                    || arguments
                        .iter()
                        .any(|argument| visit(&argument.value, name))
            }
            Expr::GenericCall {
                callee, arguments, ..
            } => {
                visit(callee, name)
                    || arguments
                        .iter()
                        .any(|argument| visit(&argument.value, name))
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                elements.iter().any(|element| visit(element, name))
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                fields.iter().any(|field| visit(&field.value, name))
            }
            Expr::Lambda { body, .. } => visit(body, name),
            Expr::Block {
                statements, tail, ..
            } => {
                statements.iter().any(|statement| match statement {
                    Statement::Let { value, .. }
                    | Statement::Assert { value, .. }
                    | Statement::Expression { value, .. }
                    | Statement::Control { value, .. }
                    | Statement::Assignment { value, .. } => visit(value, name),
                    Statement::Return { value, .. } | Statement::Break { value, .. } => {
                        value.as_ref().is_some_and(|value| visit(value, name))
                    }
                    Statement::Continue { .. } => false,
                }) || tail.as_deref().is_some_and(|tail| visit(tail, name))
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
                    .is_some_and(|condition| visit(condition, name))
                    || body.as_deref().is_some_and(|body| visit(body, name))
                    || arms.iter().any(|arm| visit(&arm.body, name))
                    || alternate
                        .as_deref()
                        .is_some_and(|alternate| visit(alternate, name))
            }
            _ => false,
        }
    }
    visit(expression, name)
}

pub(crate) fn inferred_function_parameter_type(
    body: &Expr,
    parameter: &orna_syntax::Parameter,
) -> Option<Type> {
    let Pattern::Name(name, _) = &parameter.pattern else {
        return None;
    };
    if let Some(default) = &parameter.default {
        let mut diagnostics = Vec::new();
        let inferred = infer(
            default,
            &Scope::default(),
            &BTreeMap::new(),
            &mut diagnostics,
        );
        if diagnostics.is_empty() && inferred.ty != Type::Error {
            return Some(inferred.ty);
        }
    }
    if lambda_numeric_parameter_usage(body, name) {
        return Some(Type::Int);
    }
    inferred_relation_parameter_type(body, name)
        .or_else(|| parameter_comparison_usage(body, name).then_some(Type::Error))
}

pub(crate) fn inferred_lambda_parameter_type(
    body: &Expr,
    name: &str,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<Type> {
    inferred_comparison_parameter_type(body, name, scope, local)
        .or_else(|| lambda_numeric_parameter_usage(body, name).then_some(Type::Int))
        .or_else(|| {
            inferred_record_parameter_type(body, name).filter(|ty| !type_contains_error(ty))
        })
        .or_else(|| parameter_comparison_usage(body, name).then_some(Type::Error))
}

pub(crate) fn inferred_comparison_parameter_type(
    expression: &Expr,
    name: &str,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<Type> {
    fn constrained_type(
        expression: &Expr,
        scope: &Scope,
        local: &BTreeMap<String, Symbol>,
    ) -> Option<Type> {
        let mut diagnostics = Vec::new();
        let inferred = infer(expression, scope, local, &mut diagnostics);
        (diagnostics.is_empty() && inferred.ty != Type::Error).then_some(inferred.ty)
    }

    fn side_constraint(
        parameter_side: &Expr,
        other_side: &Expr,
        name: &str,
        scope: &Scope,
        local: &BTreeMap<String, Symbol>,
    ) -> Option<Type> {
        let other = constrained_type(other_side, scope, local)?;
        match unwrap_group(parameter_side) {
            Expr::Name { text, .. } if text == name => Some(other),
            Expr::Field {
                base, name: field, ..
            } if contains_direct_name(base, name) => {
                Some(Type::Record(BTreeMap::from([(field.clone(), other)])))
            }
            _ => None,
        }
    }

    match unwrap_group(expression) {
        Expr::Binary { lhs, op, rhs, .. }
            if matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") =>
        {
            side_constraint(lhs, rhs, name, scope, local)
                .or_else(|| side_constraint(rhs, lhs, name, scope, local))
                .or_else(|| inferred_comparison_parameter_type(lhs, name, scope, local))
                .or_else(|| inferred_comparison_parameter_type(rhs, name, scope, local))
        }
        Expr::Binary { lhs, rhs, .. } => {
            inferred_comparison_parameter_type(lhs, name, scope, local)
                .or_else(|| inferred_comparison_parameter_type(rhs, name, scope, local))
        }
        Expr::Unary { rhs, .. } => inferred_comparison_parameter_type(rhs, name, scope, local),
        Expr::Field { base, .. } | Expr::Index { base, .. } => {
            inferred_comparison_parameter_type(base, name, scope, local)
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => inferred_comparison_parameter_type(callee, name, scope, local).or_else(|| {
            arguments.iter().find_map(|argument| {
                inferred_comparison_parameter_type(&argument.value, name, scope, local)
            })
        }),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => elements
            .iter()
            .find_map(|element| inferred_comparison_parameter_type(element, name, scope, local)),
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => fields
            .iter()
            .find_map(|field| inferred_comparison_parameter_type(&field.value, name, scope, local)),
        Expr::Lambda { body, .. } => inferred_comparison_parameter_type(body, name, scope, local),
        Expr::Block {
            statements, tail, ..
        } => statements
            .iter()
            .find_map(|statement| match statement {
                Statement::Let { value, .. }
                | Statement::Assert { value, .. }
                | Statement::Expression { value, .. }
                | Statement::Control { value, .. }
                | Statement::Assignment { value, .. } => {
                    inferred_comparison_parameter_type(value, name, scope, local)
                }
                Statement::Return { value, .. } | Statement::Break { value, .. } => {
                    value.as_ref().and_then(|value| {
                        inferred_comparison_parameter_type(value, name, scope, local)
                    })
                }
                Statement::Continue { .. } => None,
            })
            .or_else(|| {
                tail.as_deref()
                    .and_then(|tail| inferred_comparison_parameter_type(tail, name, scope, local))
            }),
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => condition
            .as_deref()
            .and_then(|condition| inferred_comparison_parameter_type(condition, name, scope, local))
            .or_else(|| {
                body.as_deref()
                    .and_then(|body| inferred_comparison_parameter_type(body, name, scope, local))
            })
            .or_else(|| {
                arms.iter().find_map(|arm| {
                    inferred_comparison_parameter_type(&arm.body, name, scope, local)
                })
            })
            .or_else(|| {
                alternate.as_deref().and_then(|alternate| {
                    inferred_comparison_parameter_type(alternate, name, scope, local)
                })
            }),
        _ => None,
    }
}

pub(crate) fn type_contains_error(ty: &Type) -> bool {
    match ty {
        Type::Error => true,
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => type_contains_error(inner),
        Type::Tuple(elements) => elements.iter().any(type_contains_error),
        Type::Record(fields) => fields.values().any(type_contains_error),
        Type::Applied { arguments, .. } => arguments.iter().any(type_contains_error),
        Type::Quantity { numeric, .. } => type_contains_error(numeric),
        Type::MoneyPerUnit { currency, unit } => {
            type_contains_error(currency) || type_contains_error(unit)
        }
        Type::Function {
            parameters, result, ..
        } => parameters.iter().any(type_contains_error) || type_contains_error(result),
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Named(_)
        | Type::Bottom => false,
    }
}

pub(crate) fn inferred_relation_parameter_type(body: &Expr, name: &str) -> Option<Type> {
    let Expr::Binary { lhs, rhs, op, .. } = unwrap_group(body) else {
        return None;
    };
    if op != "|" || !contains_direct_name(lhs, name) {
        return None;
    }
    let Expr::Call {
        callee, arguments, ..
    } = unwrap_group(rhs)
    else {
        return None;
    };
    let Expr::Name { text, .. } = callee.as_ref() else {
        return None;
    };
    if !matches!(text.as_str(), "filter" | "map") || arguments.len() != 1 {
        return None;
    }
    let Expr::Lambda {
        parameters, body, ..
    } = unwrap_group(&arguments[0].value)
    else {
        return None;
    };
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    let Pattern::Name(parameter, _) = &parameter.pattern else {
        return None;
    };
    Some(Type::Relation(Box::new(inferred_record_type(
        body, parameter,
    ))))
}

pub(crate) fn inferred_record_parameter_type(body: &Expr, name: &str) -> Option<Type> {
    let fields = inferred_record_fields(body, name);
    (!fields.is_empty()).then_some(Type::Record(fields))
}

pub(crate) fn inferred_record_type(body: &Expr, name: &str) -> Type {
    Type::Record(inferred_record_fields(body, name))
}

pub(crate) fn inferred_record_fields(body: &Expr, name: &str) -> BTreeMap<String, Type> {
    fn visit(expression: &Expr, name: &str, fields: &mut BTreeMap<String, Type>) {
        match expression {
            Expr::Field {
                base, name: field, ..
            } if contains_direct_name(base, name) => {
                fields.entry(field.clone()).or_insert(Type::Error);
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, name, fields),
            Expr::Binary { lhs, rhs, .. } => {
                visit(lhs, name, fields);
                visit(rhs, name, fields);
            }
            Expr::Field { base, .. } | Expr::Index { base, .. } => visit(base, name, fields),
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, name, fields);
                for argument in arguments {
                    visit(&argument.value, name, fields);
                }
            }
            Expr::GenericCall {
                callee, arguments, ..
            } => {
                visit(callee, name, fields);
                for argument in arguments {
                    visit(&argument.value, name, fields);
                }
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                for element in elements {
                    visit(element, name, fields);
                }
            }
            Expr::Record { fields: values, .. } | Expr::Nominal { fields: values, .. } => {
                for field in values {
                    visit(&field.value, name, fields);
                }
            }
            Expr::Lambda { body, .. } => visit(body, name, fields),
            Expr::Block {
                statements, tail, ..
            } => {
                for statement in statements {
                    match statement {
                        Statement::Let { value, .. }
                        | Statement::Assert { value, .. }
                        | Statement::Expression { value, .. }
                        | Statement::Control { value, .. }
                        | Statement::Assignment { value, .. } => visit(value, name, fields),
                        Statement::Return { value, .. } | Statement::Break { value, .. } => {
                            if let Some(value) = value {
                                visit(value, name, fields);
                            }
                        }
                        Statement::Continue { .. } => {}
                    }
                }
                if let Some(tail) = tail {
                    visit(tail, name, fields);
                }
            }
            Expr::Control {
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                if let Some(condition) = condition {
                    visit(condition, name, fields);
                }
                if let Some(body) = body {
                    visit(body, name, fields);
                }
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        visit(guard, name, fields);
                    }
                    visit(&arm.body, name, fields);
                }
                if let Some(alternate) = alternate {
                    visit(alternate, name, fields);
                }
            }
            _ => {}
        }
    }
    let mut fields = BTreeMap::new();
    visit(body, name, &mut fields);
    fields
}

pub(crate) fn parameter_comparison_usage(expression: &Expr, name: &str) -> bool {
    fn visit(expression: &Expr, name: &str) -> bool {
        match expression {
            Expr::Binary { lhs, op, rhs, .. } => {
                (matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=")
                    && (contains_direct_name(lhs, name) || contains_direct_name(rhs, name)))
                    || visit(lhs, name)
                    || visit(rhs, name)
            }
            Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => visit(rhs, name),
            Expr::Field { base, .. } | Expr::Index { base, .. } => visit(base, name),
            Expr::Call {
                callee, arguments, ..
            } => {
                visit(callee, name)
                    || arguments
                        .iter()
                        .any(|argument| visit(&argument.value, name))
            }
            Expr::GenericCall {
                callee, arguments, ..
            } => {
                visit(callee, name)
                    || arguments
                        .iter()
                        .any(|argument| visit(&argument.value, name))
            }
            Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
                elements.iter().any(|element| visit(element, name))
            }
            Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
                fields.iter().any(|field| visit(&field.value, name))
            }
            Expr::Lambda { body, .. } => visit(body, name),
            Expr::Block { tail, .. } => tail.as_deref().is_some_and(|tail| visit(tail, name)),
            Expr::Control {
                condition,
                body,
                arms,
                alternate,
                ..
            } => {
                condition.as_deref().is_some_and(|value| visit(value, name))
                    || body.as_deref().is_some_and(|value| visit(value, name))
                    || arms.iter().any(|arm| visit(&arm.body, name))
                    || alternate.as_deref().is_some_and(|value| visit(value, name))
            }
            _ => false,
        }
    }
    visit(expression, name)
}

/// Whether the expression is the bare (possibly parenthesised) name `name`.
pub(crate) fn contains_direct_name(expression: &Expr, name: &str) -> bool {
    match unwrap_group(expression) {
        Expr::Name { text, .. } => text == name,
        _ => false,
    }
}

pub(crate) fn unwrap_group(expression: &Expr) -> &Expr {
    match expression {
        Expr::Group { inner, .. } => unwrap_group(inner),
        expression => expression,
    }
}

pub(crate) fn infer_while(
    parts: ControlParts<'_>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let ControlParts {
        binding,
        condition,
        body,
        arms,
        alternate,
    } = parts;
    if binding.is_some() || !arms.is_empty() || alternate.is_some() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed while control shape is outside this semantic slice",
        ));
    }
    let Some(condition) = condition else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "while control requires a condition"));
        return Inferred::error();
    };
    let condition = infer(condition, scope, local, diagnostics);
    require_same(&Type::Bool, &condition.ty, diagnostics);

    let Some(body) = body else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "while control requires a body"));
        return Inferred::error_with(condition.effects);
    };
    let body = infer(body, scope, local, diagnostics);
    let mut effects = condition.effects;
    effects.join(&body.effects);
    Inferred {
        ty: Type::Unit,
        effects,
    }
}

pub(crate) fn infer_assignment(
    target: &AssignmentTarget,
    operator: &AssignmentOperator,
    value: &Expr,
    scope: &Scope,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> EffectSummary {
    let value_expression = value;
    let value = infer(value_expression, scope, local, diagnostics);
    match (target, operator) {
        (AssignmentTarget::Name { name, .. }, AssignmentOperator::Set) => {
            let expected = local.get(name).map(|symbol| symbol.ty.clone());
            match expected {
                Some(expected)
                    if is_snapshot_ref_value(&expected) && is_snapshot_ref_value(&value.ty) =>
                {
                    // Rebinding changes the pin seen by later reads of this local;
                    // inferred closure types retain the previous snapshot value.
                    let rebound_type =
                        specialize_snapshot_ref_parameter(value_expression, &value.ty, local);
                    if let Some(symbol) = local.get_mut(name) {
                        // When a parameter is rebound from another snapshot
                        // parameter, preserve the source identity so returned
                        // closure chains specialize against that caller input.
                        symbol.ty = rebound_type;
                    }
                }
                Some(expected)
                    if !checkpoint_snapshot_maps_are_valid(&expected)
                        || !checkpoint_snapshot_maps_are_valid(&value.ty) =>
                {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "snapshot context maps must contain canonical selector sets",
                    ));
                }
                Some(expected) => {
                    if let Some(reset_type) = pinned_snapshot_reset_type(&expected, &value.ty) {
                        // A checkpoint reset replaces the local with the exact
                        // selected value. Do not merge it with intermediate
                        // rebinds: saved aliases keep their own identities, and
                        // a later reset must recover the source map unchanged.
                        if let Some(symbol) = local.get_mut(name) {
                            symbol.ty = reset_type;
                        }
                    } else if contains_type_error(&expected) || contains_type_error(&value.ty) {
                        // Error recovery cannot prove that a checkpoint source
                        // carries a stable pin map. Keep the last valid local
                        // identity and avoid cascading a second type diagnostic.
                    } else if type_contains_pinned_snapshot_identity(&expected)
                        && type_contains_pinned_snapshot_identity(&value.ty)
                    {
                        // The reference does not specify diagnostics for
                        // incompatible structured checkpoint resets. Keep the
                        // local unchanged and explain why this source cannot
                        // continue the pin-stable chain.
                        let message = if paired_dynamic_snapshot_reset(&expected, &value.ty) {
                            "paired checkpoint reset must preserve each omitted lane's snapshot identity"
                        } else {
                            "checkpoint reset source must preserve nested snapshot pin topology and callable contracts"
                        };
                        diagnostics.push(diag(DIAG_TYPE, message));
                    } else {
                        require_same(&expected, &value.ty, diagnostics);
                    }
                }
                None => diagnostics.push(diag(
                    DIAG_UNRESOLVED,
                    "assignment target cannot be resolved",
                )),
            }
        }
        (AssignmentTarget::Name { name, .. }, _) => match local.get(name) {
            Some(symbol) => {
                // `count += 1` is `count = count + 1` (ORNA-CFLOW-006).
                let op = match operator {
                    AssignmentOperator::Add => "+",
                    AssignmentOperator::Subtract => "-",
                    AssignmentOperator::Multiply => "*",
                    AssignmentOperator::Divide => "/",
                    AssignmentOperator::Set => unreachable!("`=` has its own assignment arm"),
                };
                let result = binary_arithmetic_type(op, &symbol.ty, &value.ty, scope, diagnostics);
                require_same(&symbol.ty, &result, diagnostics);
            }
            None => diagnostics.push(diag(
                DIAG_UNRESOLVED,
                "assignment target cannot be resolved",
            )),
        },
        _ => diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "only local-name assignment targets are supported in this semantic slice",
        )),
    }
    value.effects
}

pub(crate) fn infer_for(
    parts: ControlParts<'_>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let ControlParts {
        binding,
        condition: iterable,
        body,
        arms,
        alternate,
    } = parts;
    let Some(binding) = binding else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "for control requires a binding"));
        return Inferred::error();
    };
    let Some(iterable) = iterable else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "for control requires an iterable"));
        return Inferred::error();
    };
    let Some(body) = body else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "for control requires a body"));
        return Inferred::error_with(infer(iterable, scope, local, diagnostics).effects);
    };
    if alternate.is_some()
        || arms.len() != 1
        || arms[0].guard.is_some()
        || arms[0].pattern != *binding
        || arms[0].body != *body
    {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed for control shape is outside this semantic slice",
        ));
        return Inferred::error_with(infer(iterable, scope, local, diagnostics).effects);
    }

    let iterable_expression = iterable;
    let iterable = infer(iterable_expression, scope, local, diagnostics);
    let Some(element) = finite_for_element_type(iterable_expression, &iterable.ty) else {
        if !matches!(iterable.ty, Type::Error) {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "for control supports only finite lists and integer ranges in this semantic slice",
            ));
        }
        return Inferred::error_with(iterable.effects);
    };
    let mut body_locals = local.clone();
    bind_pattern(binding, element, scope, &mut body_locals, diagnostics);
    let body = infer(body, scope, &body_locals, diagnostics);
    let mut effects = iterable.effects;
    effects.join(&body.effects);
    Inferred {
        ty: Type::Unit,
        effects,
    }
}

/// The runtime has a finite numeric-range contract only for canonical integer
/// ranges. Keep all other `Iterable` forms fail-closed until their iteration
/// and element-binding contracts are implemented.
pub(crate) fn finite_for_element_type(expression: &Expr, iterable: &Type) -> Option<Type> {
    match iterable {
        Type::List(element) => Some((**element).clone()),
        Type::Range(element)
            if element.as_ref() == &Type::Int && matches!(expression, Expr::Range { .. }) =>
        {
            Some(Type::Int)
        }
        _ => None,
    }
}

pub(crate) fn infer_if(
    parts: ControlParts<'_>,
    expected: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let ControlParts {
        binding,
        condition,
        body,
        arms,
        alternate,
    } = parts;
    if binding.is_some() || !arms.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed if control shape is outside this semantic slice",
        ));
    }
    let Some(condition) = condition else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "if expression requires a condition"));
        return Inferred::error();
    };
    let condition = infer(condition, scope, local, diagnostics);
    require_same(&Type::Bool, &condition.ty, diagnostics);

    let Some(body) = body else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "if expression requires a body"));
        return Inferred::error_with(condition.effects);
    };
    // A declared or contextual type is the conditional's own unification
    // target (ORNA-CFLOW-003), so each value-producing branch is checked
    // against it rather than against its sibling's spelling. `null` fits `T?`
    // there because `null` is a member of the optional (ORNA-TYPE-001).
    let contextual = expected.filter(|expected| !matches!(expected, Type::Error));
    let body = match contextual {
        Some(expected) => infer_contextual(body, expected, scope, local, diagnostics),
        None => infer(body, scope, local, diagnostics),
    };

    let Some(alternate) = alternate else {
        let mut effects = condition.effects;
        effects.join(&body.effects);
        // The untaken branch completes without a value (ORNA-CFLOW-001).
        if body.ty == Type::Bottom {
            return Inferred {
                ty: Type::Unit,
                effects,
            };
        }
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "if expression requires an else branch in this semantic slice",
        ));
        return Inferred::error_with(effects);
    };
    let alternate = match contextual {
        Some(expected) => infer_contextual(alternate, expected, scope, local, diagnostics),
        None => infer(alternate, scope, local, diagnostics),
    };
    let mut effects = condition.effects;
    effects.join(&body.effects);
    effects.join(&alternate.effects);
    let ty = match contextual {
        Some(expected) => {
            if body.ty == alternate.ty {
                // Both branches spell one type: report it once rather than
                // once per branch.
                require_same(expected, &body.ty, diagnostics);
            } else {
                // Distinct spellings can each fit the target on their own
                // terms, which is what lets `null` and `Some(value)` both fit
                // `Int?` (ORNA-TYPE-001), so check them separately and name
                // the branch that does not fit.
                require_same(expected, &body.ty, diagnostics);
                require_same(expected, &alternate.ty, diagnostics);
            }
            expected.clone()
        }
        None => match join_conditional_branches(&body.ty, &alternate.ty) {
            Some(joined) => joined,
            None => {
                diagnostics.push(type_mismatch(
                    DIAG_TYPE,
                    "static types are incompatible",
                    &body.ty,
                    &alternate.ty,
                ));
                Type::Error
            }
        },
    };
    Inferred { ty, effects }
}

pub(crate) fn check_call_arguments(
    parameters: &[Type],
    parameter_names: Option<&[String]>,
    default_parameters: &BTreeSet<usize>,
    arguments: &[orna_syntax::Argument],
    values: &[Type],
    input: Option<&Type>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let argument_count = values.len() + usize::from(input.is_some());
    let Some(parameter_names) = parameter_names.filter(|names| names.len() == parameters.len())
    else {
        if arguments.iter().any(|argument| argument.name.is_some()) {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "named arguments require declared parameter names",
            ));
        } else {
            if argument_count > parameters.len()
                || (argument_count..parameters.len())
                    .any(|index| !default_parameters.contains(&index))
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "function argument count does not match its static signature",
                ));
            }
            for (expected, actual) in parameters.iter().zip(input.into_iter().chain(values)) {
                require_same(expected, actual, diagnostics);
            }
        }
        return;
    };
    let mut seen = BTreeSet::new();
    let mut malformed = argument_count > parameters.len();
    let mut positional = 0usize;
    let mut named_started = false;
    let supplied = input.into_iter().map(|value| (None, value)).chain(
        arguments
            .iter()
            .zip(values)
            .map(|(argument, value)| (argument.name.as_deref(), value)),
    );
    for (name, actual) in supplied {
        let index = if let Some(name) = name {
            named_started = true;
            parameter_names
                .iter()
                .position(|parameter| parameter == name)
        } else if named_started {
            malformed = true;
            None
        } else {
            let index = (positional < parameters.len()).then_some(positional);
            positional += 1;
            index
        };
        let Some(index) = index else {
            malformed = true;
            continue;
        };
        if !seen.insert(index) {
            malformed = true;
            continue;
        }
        require_same(&parameters[index], actual, diagnostics);
    }
    if (0..parameters.len())
        .any(|index| !seen.contains(&index) && !default_parameters.contains(&index))
    {
        malformed = true;
    }
    if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            "named function arguments do not match its static signature",
        ));
    }
}

pub(crate) fn expected_call_parameter<'a>(
    parameters: &'a [Type],
    parameter_names: Option<&'a [String]>,
    arguments: &[orna_syntax::Argument],
    index: usize,
) -> Option<&'a Type> {
    let argument = arguments.get(index)?;
    if let Some(name) = argument.name.as_deref() {
        return parameter_names?
            .iter()
            .position(|parameter| parameter == name)
            .and_then(|position| parameters.get(position));
    }
    let positional = arguments[..index]
        .iter()
        .filter(|argument| argument.name.is_none())
        .count();
    parameters.get(positional)
}

pub(crate) fn infer_case(
    parts: ControlParts<'_>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let ControlParts {
        binding,
        condition,
        body,
        arms,
        alternate,
    } = parts;
    let mut effects = EffectSummary::default();
    if binding.is_some() || body.is_some() || alternate.is_some() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "malformed case control shape is outside this semantic slice",
        ));
    }
    if let Some(body) = body {
        effects.join(&infer(body, scope, local, diagnostics).effects);
    }
    if let Some(alternate) = alternate {
        effects.join(&infer(alternate, scope, local, diagnostics).effects);
    }
    let Some(condition) = condition else {
        diagnostics.push(diag(DIAG_UNSUPPORTED, "case expression requires a value"));
        let mut discarded_result = None;
        for arm in arms {
            infer_case_arm_body(
                arm,
                local,
                scope,
                diagnostics,
                &mut effects,
                &mut discarded_result,
            );
        }
        return Inferred::error_with(effects);
    };
    let scrutinee = infer(condition, scope, local, diagnostics);
    effects.join(&scrutinee.effects);
    if arms.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "case expression requires at least one arm",
        ));
        return Inferred::error_with(effects);
    }

    let mut result = None;
    match &scrutinee.ty {
        Type::Named(name) if scope.enum_variants.contains_key(name) => {
            let variants = &scope.enum_variants[name];
            let mut covered = BTreeSet::new();
            for arm in arms {
                let mut arm_locals = local.clone();
                if let Some(variant) = bind_enum_case_pattern(
                    &arm.pattern,
                    name,
                    variants,
                    &mut arm_locals,
                    diagnostics,
                ) {
                    // Guards can fail at runtime, so only an unguarded arm
                    // establishes coverage for a closed enum.
                    if arm.guard.is_none() && !covered.insert(variant) {
                        diagnostics.push(diag(DIAG_TYPE, "case enum variant is duplicated"));
                    }
                }
                infer_case_arm_body(
                    arm,
                    &arm_locals,
                    scope,
                    diagnostics,
                    &mut effects,
                    &mut result,
                );
            }
            if covered.len() != variants.len() {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "case expression does not cover every enum variant",
                ));
            }
        }
        Type::Optional(inner) => {
            let mut covered = BTreeSet::new();
            for arm in arms {
                let mut arm_locals = local.clone();
                if let Some(part) =
                    bind_optional_case_pattern(&arm.pattern, inner, &mut arm_locals, diagnostics)
                {
                    // A guarded Some/null arm is conditional coverage; a
                    // later unguarded arm may provide the exhaustive fallback.
                    if arm.guard.is_none() && !covered.insert(part) {
                        diagnostics.push(diag(DIAG_TYPE, "case optional arm is duplicated"));
                    }
                }
                infer_case_arm_body(
                    arm,
                    &arm_locals,
                    scope,
                    diagnostics,
                    &mut effects,
                    &mut result,
                );
            }
            if covered != BTreeSet::from(["Some", "null"]) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "case expression must cover Some and null exactly once",
                ));
            }
        }
        Type::Bool => {
            let mut covered = BTreeSet::new();
            for arm in arms {
                let mut arm_locals = local.clone();
                let (literal, catch_all) = match &arm.pattern {
                    Pattern::Literal {
                        text,
                        kind: LiteralKind::Boolean,
                        ..
                    } if text == "true" => (Some(true), false),
                    Pattern::Literal {
                        text,
                        kind: LiteralKind::Boolean,
                        ..
                    } if text == "false" => (Some(false), false),
                    Pattern::Wildcard(_) => (None, true),
                    Pattern::Name(name, _) => {
                        insert_case_binding(name, Type::Bool, &mut arm_locals, diagnostics);
                        (None, true)
                    }
                    _ => {
                        diagnostics.push(diag(
                            DIAG_TYPE,
                            "boolean case arm must match true, false, or bind the whole value",
                        ));
                        (None, false)
                    }
                };
                if arm.guard.is_none() {
                    if let Some(value) = literal {
                        if !covered.insert(value) {
                            diagnostics.push(diag(DIAG_TYPE, "boolean case arm is duplicated"));
                        }
                    } else if catch_all {
                        for value in [false, true] {
                            if !covered.insert(value) {
                                diagnostics.push(diag(DIAG_TYPE, "boolean case arm is duplicated"));
                            }
                        }
                    }
                }
                infer_case_arm_body(
                    arm,
                    &arm_locals,
                    scope,
                    diagnostics,
                    &mut effects,
                    &mut result,
                );
            }
            if covered != BTreeSet::from([false, true]) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "case expression does not cover true and false",
                ));
            }
        }
        Type::Error => {
            for arm in arms {
                infer_case_arm_body(arm, local, scope, diagnostics, &mut effects, &mut result);
            }
        }
        _ => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "case inference supports only local enums and optional values",
            ));
            for arm in arms {
                infer_case_arm_body(arm, local, scope, diagnostics, &mut effects, &mut result);
            }
        }
    }
    Inferred {
        ty: result.unwrap_or(Type::Error),
        effects,
    }
}

pub(crate) fn bind_enum_case_pattern(
    pattern: &Pattern,
    enum_name: &str,
    variants: &BTreeMap<String, BTreeMap<String, Type>>,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    let Pattern::Constructor {
        path,
        arguments,
        fields,
        ..
    } = pattern
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "enum case arm requires a qualified constructor pattern",
        ));
        return None;
    };
    let [owner, variant] = path.as_slice() else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "enum case constructor must be exactly Enum.variant",
        ));
        return None;
    };
    if owner.text != enum_name {
        diagnostics.push(diag(
            DIAG_TYPE,
            "enum case constructor does not match the scrutinee type",
        ));
        return None;
    }
    let Some(expected) = variants.get(&variant.text) else {
        diagnostics.push(diag(
            DIAG_UNRESOLVED,
            "enum case variant cannot be resolved",
        ));
        return None;
    };
    if !arguments.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "enum case payload requires named record fields",
        ));
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut valid = true;
    for field in fields {
        let Some(ty) = expected.get(&field.name) else {
            diagnostics.push(diag(
                DIAG_UNRESOLVED,
                "enum case payload field cannot be resolved",
            ));
            valid = false;
            continue;
        };
        if !seen.insert(field.name.as_str()) {
            diagnostics.push(diag(DIAG_TYPE, "enum case payload field is duplicated"));
            valid = false;
            continue;
        }
        match &field.pattern {
            None => insert_case_binding(&field.name, ty.clone(), local, diagnostics),
            Some(Pattern::Name(name, _)) => {
                insert_case_binding(name, ty.clone(), local, diagnostics)
            }
            Some(Pattern::Wildcard(_)) => {}
            Some(_) => {
                diagnostics.push(diag(
                    DIAG_UNSUPPORTED,
                    "nested enum payload patterns are outside this semantic slice",
                ));
                valid = false;
            }
        }
    }
    if seen.len() != expected.len() {
        diagnostics.push(diag(
            DIAG_TYPE,
            "enum case payload fields do not match the declared variant",
        ));
        valid = false;
    }
    valid.then(|| variant.text.clone())
}

pub(crate) fn bind_optional_case_pattern(
    pattern: &Pattern,
    inner: &Type,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'static str> {
    match pattern {
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } if path.len() == 1 && path[0].text == "Some" && fields.is_empty() => {
            let [binding] = arguments.as_slice() else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "Some case pattern requires exactly one payload binding",
                ));
                return None;
            };
            match binding {
                Pattern::Name(name, _) => {
                    insert_case_binding(name, inner.clone(), local, diagnostics)
                }
                Pattern::Wildcard(_) => {}
                _ => {
                    diagnostics.push(diag(
                        DIAG_UNSUPPORTED,
                        "nested optional payload patterns are outside this semantic slice",
                    ));
                    return None;
                }
            }
            Some("Some")
        }
        Pattern::Literal {
            kind: LiteralKind::Null,
            ..
        } => Some("null"),
        _ => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "optional case arm must be Some(binding) or null",
            ));
            None
        }
    }
}

pub(crate) fn insert_case_binding(
    name: &str,
    ty: Type,
    local: &mut BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    insert_local_binding(name, ty, local, diagnostics);
}

pub(crate) fn infer_case_arm_body(
    arm: &orna_syntax::CaseArm,
    local: &BTreeMap<String, Symbol>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
    effects: &mut EffectSummary,
    result: &mut Option<Type>,
) {
    if let Some(guard) = &arm.guard {
        let guard = infer(guard, scope, local, diagnostics);
        effects.join(&guard.effects);
        require_same(&Type::Bool, &guard.ty, diagnostics);
    }
    let body = infer(&arm.body, scope, local, diagnostics);
    effects.join(&body.effects);
    // ORNA-CASE-004 joins reachable bodies with the same rule as `if` branches
    // (ORNA-CFLOW-003), so `Int` with `Int?` in either order unifies to `Int?`.
    match result.as_ref() {
        None => *result = Some(body.ty),
        Some(joined) => match join_conditional_branches(joined, &body.ty) {
            Some(next) => *result = Some(next),
            // A failed join reports once, naming the arm that does not fit.
            None => require_same(joined, &body.ty, diagnostics),
        },
    }
}

pub(crate) fn merge_list_element_types(left: &Type, right: &Type) -> Option<Type> {
    if !checkpoint_snapshot_maps_are_valid(left) || !checkpoint_snapshot_maps_are_valid(right) {
        return None;
    }
    if left == right {
        return Some(left.clone());
    }
    if is_terminal_historical_callable(left) && is_terminal_historical_callable(right) {
        // Completed zero-argument closures have no continuation boundary to
        // keep paired lane identity. Preserve their exact pins so a list
        // cannot silently combine terminal values from separate lanes.
        return None;
    }
    if (type_contains_pinned_checkpoint_tuple(left) || type_contains_pinned_checkpoint_tuple(right))
        && (!checkpoint_pin_map_widths_match(left, right)
            || !checkpoint_value_pin_identity_topology_matches(left, right)
            || !nested_checkpoint_compaction_fold_preserves_pin_identity(left, right))
    {
        return None;
    }
    if let (Type::Tuple(left), Type::Tuple(right)) = (left, right)
        && !tuple_checkpoint_promotion_matches(left, right)
    {
        return None;
    }
    if let (Type::Record(left), Type::Record(right)) = (left, right)
        && !record_checkpoint_promotion_matches(left, right)
    {
        return None;
    }
    merge_checkpoint_field_map(left, right)
}

pub(crate) fn is_terminal_historical_callable(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Applied { base, arguments }
            if base == "sys.HistoricalCallable"
                && matches!(
                    arguments.as_slice(),
                    [_, Type::Function { parameters, .. }] if parameters.is_empty()
                )
    )
}

pub(crate) fn merge_checkpoint_field_map(left: &Type, right: &Type) -> Option<Type> {
    merge_checkpoint_field_map_inner(left, right, false)
}

/// Multi-parent folds validate source rows against the first accepted parent
/// before merging. The accumulated value may therefore have wider context
/// maps than the next row; retain structural and binder checks while allowing
/// those already-validated identities to grow through nested callable results.
pub(crate) fn merge_checkpoint_field_map_multi_parent(left: &Type, right: &Type) -> Option<Type> {
    merge_checkpoint_field_map_inner(left, right, true)
}

/// A rejected paired-pin scope must not discard unrelated checkpoint values
/// from the same sparse row. Descend through a record only when it directly
/// contains paired tuples and an independent sibling; deeper paired records
/// stay atomic while their tuple rollback identity is still being recovered.
pub(crate) fn merge_unpaired_checkpoint_record_siblings(
    accumulated: &Type,
    parent: &Type,
) -> Option<Type> {
    let (Type::Record(accumulated), Type::Record(parent)) = (accumulated, parent) else {
        return None;
    };
    let mut merged = BTreeMap::new();
    for (name, accumulated_value) in accumulated {
        let Some(parent_value) = parent.get(name) else {
            merged.insert(name.clone(), accumulated_value.clone());
            continue;
        };
        let contains_pinned_tuple = type_contains_pinned_checkpoint_tuple(accumulated_value)
            || type_contains_pinned_checkpoint_tuple(parent_value);
        let value = match (accumulated_value, parent_value) {
            (Type::Record(_), Type::Record(_))
                if contains_pinned_tuple
                    && record_has_direct_pinned_tuple_and_unpaired_sibling(
                        accumulated_value,
                        parent_value,
                    ) =>
            {
                merge_unpaired_checkpoint_record_siblings(accumulated_value, parent_value)
                    .unwrap_or_else(|| accumulated_value.clone())
            }
            _ if contains_pinned_tuple => accumulated_value.clone(),
            _ => merge_checkpoint_field_map_multi_parent(accumulated_value, parent_value)
                .unwrap_or_else(|| accumulated_value.clone()),
        };
        merged.insert(name.clone(), value);
    }
    Some(Type::Record(merged))
}

pub(crate) fn record_has_direct_pinned_tuple_and_unpaired_sibling(
    accumulated: &Type,
    parent: &Type,
) -> bool {
    let (Type::Record(accumulated), Type::Record(parent)) = (accumulated, parent) else {
        return false;
    };
    let direct_pinned_tuple = accumulated.iter().any(|(name, value)| {
        matches!(value, Type::Tuple(_))
            && type_contains_pinned_checkpoint_tuple(value)
            && parent.get(name).is_some_and(|parent| {
                matches!(parent, Type::Tuple(_)) && type_contains_pinned_checkpoint_tuple(parent)
            })
    });
    let unpaired_sibling = accumulated.iter().any(|(name, value)| {
        parent.get(name).is_some_and(|parent| {
            !type_contains_pinned_checkpoint_tuple(value)
                && !type_contains_pinned_checkpoint_tuple(parent)
        })
    });
    direct_pinned_tuple && unpaired_sibling
}

pub(crate) fn merge_checkpoint_field_map_inner(
    left: &Type,
    right: &Type,
    source_topology_validated: bool,
) -> Option<Type> {
    if !checkpoint_snapshot_maps_are_valid(left) || !checkpoint_snapshot_maps_are_valid(right) {
        return None;
    }
    // Bottom contributes no value to a collection fold. Keep the concrete
    // sibling type when the other row omitted that slot; tuple promotion has
    // already checked that doing so cannot collapse distinct pin identities.
    if matches!(left, Type::Bottom) {
        return Some(right.clone());
    }
    if matches!(right, Type::Bottom) {
        return Some(left.clone());
    }
    if is_snapshot_context_type(left) || is_snapshot_context_type(right) {
        if !is_snapshot_context_map_shape(left) || !is_snapshot_context_map_shape(right) {
            return None;
        }
        return merge_snapshot_context_map(left, right);
    }
    if left == right {
        return Some(left.clone());
    }
    match (left, right) {
        (
            Type::Applied {
                base: left_base,
                arguments: left_arguments,
            },
            Type::Applied {
                base: right_base,
                arguments: right_arguments,
            },
        ) if left_base == "sys.HistoricalCallable" && right_base == "sys.HistoricalCallable" => {
            match (left_arguments.as_slice(), right_arguments.as_slice()) {
                ([left_snapshot, left_callable], [right_snapshot, right_callable]) => {
                    Some(historical_callable_type(
                        &merge_snapshot_context_map(left_snapshot, right_snapshot)?,
                        &merge_checkpoint_field_map_inner(
                            left_callable,
                            right_callable,
                            source_topology_validated,
                        )?,
                    ))
                }
                _ => None,
            }
        }
        (
            Type::Function {
                parameters: left_parameters,
                parameter_names: left_names,
                result: left_result,
                default_parameters: left_defaults,
            },
            Type::Function {
                parameters: right_parameters,
                parameter_names: right_names,
                result: right_result,
                default_parameters: right_defaults,
            },
        ) => {
            if left_parameters.len() != right_parameters.len() {
                return None;
            }
            if !source_topology_validated
                && !function_snapshot_pin_identity_topology_matches(
                    left_parameters,
                    left_result,
                    right_parameters,
                    right_result,
                )
            {
                return None;
            }
            // Distinct checkpoint closures can have the same callable shape
            // while their SnapshotRef parameters carry different lexical
            // binder IDs. Merge only those pinned parameter shapes, retaining
            // their context map; keep all other parameter contracts exact.
            let parameters = left_parameters
                .iter()
                .zip(right_parameters)
                .map(|(left, right)| {
                    if left == right {
                        Some(left.clone())
                    } else if type_contains_pinned_snapshot_identity(left)
                        && type_contains_pinned_snapshot_identity(right)
                        && pinned_snapshot_shape_matches(left, right)
                    {
                        merge_checkpoint_field_map_inner(left, right, source_topology_validated)
                    } else {
                        None
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Type::Function {
                parameters,
                default_parameters: left_defaults
                    .intersection(right_defaults)
                    .copied()
                    .collect(),
                parameter_names: (left_names == right_names)
                    .then(|| left_names.clone())
                    .flatten(),
                result: Box::new(merge_checkpoint_field_map_inner(
                    left_result,
                    right_result,
                    source_topology_validated,
                )?),
            })
        }
        (Type::List(left), Type::List(right)) => {
            if !source_topology_validated
                && (type_contains_pinned_checkpoint_tuple(left)
                    || type_contains_pinned_checkpoint_tuple(right))
                && (!checkpoint_pin_map_widths_match(left, right)
                    || !checkpoint_value_pin_identity_topology_matches(left, right)
                    || !nested_checkpoint_compaction_fold_preserves_pin_identity(left, right))
            {
                return None;
            }
            // Tuple shape is a list-element promotion contract: otherwise a
            // later row with narrower checkpoint maps can widen the already
            // inferred tuple slots. Keep this check at collection promotion
            // so ordinary tuple rebinding (including Bottom recovery) retains
            // its existing binder semantics.
            if !source_topology_validated
                && let (Type::Tuple(left), Type::Tuple(right)) = (left.as_ref(), right.as_ref())
                && !tuple_checkpoint_promotion_matches(left, right)
            {
                return None;
            }
            if !source_topology_validated
                && let (Type::Record(left), Type::Record(right)) = (left.as_ref(), right.as_ref())
                && !record_checkpoint_promotion_matches(left, right)
            {
                return None;
            }
            Some(Type::List(Box::new(merge_checkpoint_field_map_inner(
                left,
                right,
                source_topology_validated,
            )?)))
        }
        (Type::Range(left), Type::Range(right)) => Some(Type::Range(Box::new(
            merge_checkpoint_field_map_inner(left, right, source_topology_validated)?,
        ))),
        (Type::Relation(left), Type::Relation(right)) => Some(Type::Relation(Box::new(
            merge_checkpoint_field_map_inner(left, right, source_topology_validated)?,
        ))),
        (Type::Stream(left), Type::Stream(right)) => Some(Type::Stream(Box::new(
            merge_checkpoint_field_map_inner(left, right, source_topology_validated)?,
        ))),
        (Type::Optional(left), Type::Optional(right)) => Some(Type::Optional(Box::new(
            merge_checkpoint_field_map_inner(left, right, source_topology_validated)?,
        ))),
        (Type::Record(left), Type::Record(right)) if left.len() == right.len() => {
            Some(Type::Record(
                left.iter()
                    .map(|(name, left)| {
                        Some((
                            name.clone(),
                            merge_checkpoint_field_map_inner(
                                left,
                                right.get(name)?,
                                source_topology_validated,
                            )?,
                        ))
                    })
                    .collect::<Option<BTreeMap<_, _>>>()?,
            ))
        }
        (Type::Tuple(left), Type::Tuple(right)) if left.len() == right.len() => Some(Type::Tuple(
            left.iter()
                .zip(right)
                .map(|(left, right)| {
                    merge_checkpoint_field_map_inner(left, right, source_topology_validated)
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        (
            Type::Applied {
                base: left_base,
                arguments: left_arguments,
            },
            Type::Applied {
                base: right_base,
                arguments: right_arguments,
            },
        ) if left_base == right_base && left_arguments.len() == right_arguments.len() => {
            Some(Type::Applied {
                base: left_base.clone(),
                arguments: left_arguments
                    .iter()
                    .zip(right_arguments)
                    .map(|(left, right)| {
                        merge_checkpoint_field_map_inner(left, right, source_topology_validated)
                    })
                    .collect::<Option<Vec<_>>>()?,
            })
        }
        (
            Type::MoneyPerUnit {
                currency: left_currency,
                unit: left_unit,
            },
            Type::MoneyPerUnit {
                currency: right_currency,
                unit: right_unit,
            },
        ) => Some(Type::MoneyPerUnit {
            currency: Box::new(merge_checkpoint_field_map_inner(
                left_currency,
                right_currency,
                source_topology_validated,
            )?),
            unit: Box::new(merge_checkpoint_field_map_inner(
                left_unit,
                right_unit,
                source_topology_validated,
            )?),
        }),
        _ => None,
    }
}

/// Keep distinct pins in matching fields when homogeneous lists collect
/// checkpoints from different snapshots. The reference defines pin identity
/// for historical reads but is silent on local checkpoint collections.
pub(crate) fn merge_snapshot_context_map(left: &Type, right: &Type) -> Option<Type> {
    const CONTEXT_MAP: &str = "semantic.SnapshotContextMap";

    if !is_snapshot_context_map_shape(left) || !is_snapshot_context_map_shape(right) {
        return None;
    }

    fn collect_contexts(ty: &Type, contexts: &mut BTreeSet<String>) {
        match ty {
            Type::Applied { base, arguments } if base == "sys.SnapshotRefContext" => {
                if let [Type::Named(selector)] = arguments.as_slice()
                    && is_snapshot_selector_context(selector)
                {
                    contexts.insert(selector.clone());
                }
            }
            Type::Applied { base, arguments } if base == CONTEXT_MAP => {
                contexts.extend(arguments.iter().filter_map(|argument| match argument {
                    Type::Named(selector) if is_snapshot_selector_context(selector) => {
                        Some(selector.clone())
                    }
                    _ => None,
                }));
            }
            _ => {}
        }
    }

    let mut contexts = BTreeSet::new();
    collect_contexts(left, &mut contexts);
    collect_contexts(right, &mut contexts);
    match contexts.len() {
        0 => None,
        1 => contexts
            .into_iter()
            .next()
            .map(|selector| contextual_snapshot_ref(&selector)),
        _ => Some(Type::Applied {
            base: CONTEXT_MAP.into(),
            arguments: contexts.into_iter().map(Type::Named).collect(),
        }),
    }
}

pub(crate) fn is_snapshot_context_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Applied { base, .. }
            if base == "sys.SnapshotRefContext" || base == "semantic.SnapshotContextMap"
    )
}

/// Synthetic checkpoint maps only represent canonical snapshot selectors.
/// The reference does not specify local checkpoint-map merging, so reject
/// malformed map identities instead of letting equality or an enclosing
/// product hide them during a rebind storm.
pub(crate) fn checkpoint_snapshot_maps_are_valid(ty: &Type) -> bool {
    match ty {
        Type::Applied { base, .. } if base == "sys.SnapshotRefContext" => {
            is_contextual_snapshot_ref(ty)
        }
        Type::Applied { base, .. } if base == "semantic.SnapshotContextMap" => {
            is_snapshot_context_map_shape(ty)
        }
        Type::Applied { arguments, .. } => arguments.iter().all(checkpoint_snapshot_maps_are_valid),
        Type::List(element)
        | Type::Range(element)
        | Type::Relation(element)
        | Type::Stream(element)
        | Type::Optional(element) => checkpoint_snapshot_maps_are_valid(element),
        Type::Record(fields) => fields.values().all(checkpoint_snapshot_maps_are_valid),
        Type::Tuple(elements) => elements.iter().all(checkpoint_snapshot_maps_are_valid),
        Type::Function {
            parameters, result, ..
        } => {
            parameters.iter().all(checkpoint_snapshot_maps_are_valid)
                && checkpoint_snapshot_maps_are_valid(result)
        }
        Type::Quantity { numeric, .. } => checkpoint_snapshot_maps_are_valid(numeric),
        Type::MoneyPerUnit { currency, unit } => {
            checkpoint_snapshot_maps_are_valid(currency) && checkpoint_snapshot_maps_are_valid(unit)
        }
        _ => true,
    }
}

pub(crate) fn infer_nominal(
    path: &[orna_syntax::NameSegment],
    fields: &[orna_syntax::RecordField],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if let Some(variant) = infer_enum_payload_construction(path, fields, scope, local, diagnostics)
    {
        return variant;
    }
    let mut actual = BTreeMap::new();
    let mut supplied = Vec::with_capacity(fields.len());
    let mut effects = EffectSummary::default();
    let mut duplicate_field = false;
    for field in fields {
        let value = infer(&field.value, scope, local, diagnostics);
        effects.join(&value.effects);
        supplied.push((field.name.clone(), value.ty.clone()));
        if actual.insert(field.name.clone(), value.ty).is_some() {
            duplicate_field = true;
            diagnostics.push(diag(DIAG_DUPLICATE, "duplicate nominal constructor field"));
        }
    }
    let path_text = path
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>();
    let (symbol, expected, constructor_type) = if let [name] = path {
        let symbol = scope
            .names
            .get(&name.text)
            .filter(|symbol| symbol.kind == SymbolKind::Type);
        let identity = canonical_nominal_name(&name.text, scope);
        let local_expected = scope.nominal_rows.get(&identity).and_then(|ty| match ty {
            Type::Record(fields) => Some(fields),
            _ => None,
        });
        let exported_expected = symbol.and_then(Symbol::table_fields);
        let expected = if scope.local_nominal_names.contains(&name.text) {
            local_expected
        } else {
            exported_expected.or(local_expected)
        };
        (symbol, expected, Type::Named(identity))
    } else {
        let symbol = if path_text
            .first()
            .is_some_and(|root| local.contains_key(*root))
        {
            None
        } else {
            qualified_module_symbol(&path_text, scope)
                .filter(|symbol| symbol.kind == SymbolKind::Type)
        };
        let expected = symbol.and_then(Symbol::table_fields);
        let constructor_type = symbol
            .map(|symbol| symbol.ty.clone())
            .unwrap_or(Type::Error);
        (symbol, expected, constructor_type)
    };
    let (Some(symbol), Some(expected)) = (symbol, expected) else {
        diagnostics.push(diag(
            DIAG_UNRESOLVED,
            "nominal record type cannot be resolved",
        ));
        return Inferred::error_with(effects);
    };
    let schema = symbol.table_schema.as_ref();
    let admission = schema.and_then(|schema| schema.admission.as_ref());
    let public_fields = schema.map(|schema| &schema.fields);
    let owning_private = match (&constructor_type, path) {
        (Type::Named(identity), [name]) => {
            scope.private_field_owner.as_deref() == Some(identity)
                || (scope.allow_local_private_nominal_construction
                    && (scope.private_field_owner.is_none()
                        || scope.private_field_owner.as_deref() == Some(identity))
                    && scope.local_nominal_names.contains(&name.text)
                    && scope
                        .nominal_identities
                        .get(&name.text)
                        .is_some_and(|local_identity| local_identity == identity))
        }
        _ => false,
    };
    let private_field_supplied = match &constructor_type {
        Type::Named(identity) => scope
            .nominal_private_fields
            .get(identity)
            .is_some_and(|private| actual.keys().any(|field| private.contains(field))),
        _ => false,
    };
    let invalid_fields = actual.keys().any(|field| !expected.contains_key(field));
    let missing_public_required = admission.is_some_and(|admission| {
        admission
            .required
            .iter()
            .any(|field| !actual.contains_key(field))
    });
    let missing_private_required = admission.is_some_and(|admission| {
        if !admission.private_required || owning_private {
            return false;
        }
        let Some(public_fields) = public_fields else {
            return true;
        };
        let mut hidden_fields = expected
            .keys()
            .filter(|field| !public_fields.contains_key(*field));
        if hidden_fields.next().is_some() {
            expected
                .keys()
                .any(|field| !public_fields.contains_key(field) && !actual.contains_key(field))
        } else {
            // Imported schemas intentionally omit private field names. A
            // private-required flag therefore proves that the constructor
            // cannot be complete in this scope, even when all public fields
            // were supplied.
            true
        }
    });
    let legacy_shape_mismatch = admission.is_none() && expected.keys().ne(actual.keys());
    if !duplicate_field
        && (invalid_fields
            || missing_public_required
            || missing_private_required
            || (private_field_supplied && !owning_private)
            || legacy_shape_mismatch)
    {
        diagnostics.push(diag(
            DIAG_TYPE,
            "nominal constructor fields do not match the declared row",
        ));
    }
    for (name, actual) in supplied {
        if let Some(expected) = expected.get(&name) {
            require_same(expected, &actual, diagnostics);
        }
    }
    Inferred {
        // A nominal constructor carries its declared identity. The private
        // row is used only to validate constructor fields; returning it here
        // would make an inferred factory result structurally accessible from
        // unrelated modules.
        ty: constructor_type,
        effects,
    }
}
