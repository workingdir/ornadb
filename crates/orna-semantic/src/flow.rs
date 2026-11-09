//! Function body and control-flow inference.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::{
    ControlKind, Declaration, Expr, Item, Pattern, Statement, StringSegment, TypeExpr,
};
use orna_value::Diagnostic;

use super::*;

/// Infers a function or anonymous-function body (ORNA-FN-009). With a known
/// result, each owned `return` value is checked where it occurs; a body that
/// completes only through `return` produces that result. Without one, the
/// result joins the tail with every reachable return path.
pub(crate) fn infer_function_body(
    body: &Expr,
    expected: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut owned = Vec::new();
    collect_owned_returns(body, &mut owned);
    if owned.is_empty() {
        return match expected {
            Some(expected) => infer_contextual(body, expected, scope, local, diagnostics),
            None => infer(body, scope, local, diagnostics),
        };
    }
    scope.return_frames.borrow_mut().push(ReturnFrame {
        owned,
        expected: expected.cloned(),
        returned: Vec::new(),
    });
    let inferred = match expected {
        Some(expected) => infer_contextual(body, expected, scope, local, diagnostics),
        None => infer(body, scope, local, diagnostics),
    };
    let frame = scope
        .return_frames
        .borrow_mut()
        .pop()
        .expect("function return frames are pushed and popped in pairs");
    if let Some(expected) = expected {
        if inferred.ty == Type::Bottom && !frame.returned.is_empty() {
            return Inferred {
                ty: expected.clone(),
                effects: inferred.effects,
            };
        }
        return inferred;
    }
    Inferred {
        ty: join_return_paths(inferred.ty, frame.returned, diagnostics),
        effects: inferred.effects,
    }
}

/// Records the `return` statements whose nearest enclosing function is the
/// body being walked. Anonymous-function bodies own their own returns.
pub(crate) fn collect_owned_returns(expression: &Expr, owned: &mut Vec<usize>) {
    match expression {
        Expr::Name { .. }
        | Expr::Literal { .. }
        | Expr::ReplBinding { .. }
        | Expr::Lambda { .. } => {}
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let StringSegment::Expression { value, .. } = segment {
                    collect_owned_returns(value, owned);
                }
            }
        }
        Expr::Unary { rhs: inner, .. }
        | Expr::Group { inner, .. }
        | Expr::Field { base: inner, .. } => collect_owned_returns(inner, owned),
        Expr::Binary { lhs, rhs, .. }
        | Expr::Index {
            base: lhs,
            index: rhs,
            ..
        } => {
            collect_owned_returns(lhs, owned);
            collect_owned_returns(rhs, owned);
        }
        Expr::Range { lower, upper, .. } => {
            for bound in [lower, upper].into_iter().flatten() {
                collect_owned_returns(bound, owned);
            }
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            collect_owned_returns(callee, owned);
            for argument in arguments {
                collect_owned_returns(&argument.value, owned);
            }
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for element in elements {
                collect_owned_returns(element, owned);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                collect_owned_returns(&field.value, owned);
            }
        }
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                match statement {
                    Statement::Return { value, .. } => {
                        owned.push(std::ptr::from_ref(statement).addr());
                        if let Some(value) = value {
                            collect_owned_returns(value, owned);
                        }
                    }
                    Statement::Break { value, .. } => {
                        if let Some(value) = value {
                            collect_owned_returns(value, owned);
                        }
                    }
                    Statement::Let { value, .. }
                    | Statement::Assert { value, .. }
                    | Statement::Assignment { value, .. }
                    | Statement::Expression { value, .. }
                    | Statement::Control { value, .. } => collect_owned_returns(value, owned),
                    Statement::Continue { .. } => {}
                }
            }
            if let Some(tail) = tail {
                collect_owned_returns(tail, owned);
            }
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            for part in [condition, body, alternate].into_iter().flatten() {
                collect_owned_returns(part, owned);
            }
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    collect_owned_returns(guard, owned);
                }
                collect_owned_returns(&arm.body, owned);
            }
        }
    }
}

/// Types one `return` statement against the function body that owns it. A
/// bare `return;` completes with `Unit` (ORNA-CFLOW-001). A `return` outside
/// every inferred body, such as one in a nested `From` conversion, has no
/// result to check and is only inferred.
pub(crate) fn infer_return_statement(
    statement: &Statement,
    value: Option<&Expr>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let address = std::ptr::from_ref(statement).addr();
    let (owner, expected) = {
        let frames = scope.return_frames.borrow();
        match frames
            .iter()
            .rposition(|frame| frame.owned.contains(&address))
        {
            Some(index) => (Some(index), frames[index].expected.clone()),
            None => (None, None),
        }
    };
    let returned = match (value, &expected) {
        (Some(value), Some(expected)) => {
            infer_contextual(value, expected, scope, local, diagnostics)
        }
        (Some(value), None) => infer(value, scope, local, diagnostics),
        (None, _) => Inferred::pure(Type::Unit),
    };
    if let Some(index) = owner {
        if let Some(expected) = &expected {
            require_same(expected, &returned.ty, diagnostics);
        }
        scope.return_frames.borrow_mut()[index]
            .returned
            .push(returned.ty.clone());
    }
    returned
}

/// Joins the two value types a control-flow expression can produce
/// (ORNA-CFLOW-003 for `if` branches, ORNA-FN-009 for return paths).
///
/// `Bottom` contributes nothing: a branch that leaves through `return`,
/// `break` or `continue` never supplies a value. `null` is a member of `T?`
/// (ORNA-TYPE-001), so it joins an ordinary value as that value's optional.
/// Any other disagreement has no single static type and the caller reports it.
pub(crate) fn join_flow_types(left: &Type, right: &Type) -> Option<Type> {
    Some(match (left, right) {
        (Type::Error, _) | (_, Type::Error) => Type::Error,
        (Type::Bottom, right) => (*right).clone(),
        (left, Type::Bottom) => (*left).clone(),
        (left, right) if left == right => left.clone(),
        (Type::Null, Type::Optional(inner)) | (Type::Optional(inner), Type::Null) => {
            Type::Optional((*inner).clone())
        }
        (Type::Optional(inner), right) if types_match(inner, right) => {
            Type::Optional((*inner).clone())
        }
        (left, Type::Optional(inner)) if types_match(inner, left) => {
            Type::Optional((*inner).clone())
        }
        (Type::Null, right) if *right != Type::Unit => Type::Optional(Box::new((*right).clone())),
        (left, Type::Null) if *left != Type::Unit => Type::Optional(Box::new((*left).clone())),
        _ => return None,
    })
}

/// Joins two `if` branch types (ORNA-CFLOW-003). A pair that already fits its
/// sibling under [`types_match`] keeps the spelling this slice accepted
/// before, so the only widened forms are `null` with `T?` (ORNA-TYPE-001) and
/// an ordinary value with its optional.
pub(crate) fn join_conditional_branches(left: &Type, right: &Type) -> Option<Type> {
    if types_match(left, right) {
        return Some(left.clone());
    }
    if types_match(right, left) {
        return Some(right.clone());
    }
    join_flow_types(left, right)
}

/// Joins an unannotated function's tail with its return paths (ORNA-FN-009).
/// Each path joins through [`join_flow_types`]; a path that cannot join has no
/// single static result type and is reported.
pub(crate) fn join_return_paths(
    tail: Type,
    returned: Vec<Type>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let mut joined = tail;
    for path in returned {
        match join_flow_types(&joined, &path) {
            Some(next) => joined = next,
            None => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!(
                        "return paths have incompatible static types: {} and {}",
                        diagnostic_type_name(&joined),
                        diagnostic_type_name(&path),
                    ),
                ));
                joined = Type::Error;
            }
        }
    }
    joined
}

pub(crate) fn check_function(
    item: &Item,
    symbols: &mut BTreeMap<String, Symbol>,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Declaration::Function { signature, body } = &item.declaration else {
        unreachable!("function checker called for a non-function declaration");
    };
    let mut function_scope = scope.clone();
    function_scope.generic_type_parameters = signature
        .generics
        .iter()
        .map(|generic| generic.name.clone())
        .collect();
    function_scope.generic_type_bounds = signature
        .generics
        .iter()
        .map(|generic| (generic.name.clone(), generic.bounds.clone()))
        .collect();
    // Bounds are in scope so `Money<C>` can accept a `C impl Currency`.
    validate_function_signature_annotations(signature, &function_scope, diagnostics);
    let mut local = BTreeMap::new();
    let mut default_effects = EffectSummary::default();
    for parameter in &signature.parameters {
        let ty = parameter
            .annotation
            .as_ref()
            .map(|annotation| resolved_type_of(annotation, &function_scope))
            .or_else(|| inferred_function_parameter_type(body, parameter));
        if ty.is_none() {
            diagnostics.push(diag(
                DIAG_ANNOTATION,
                "function parameter needs a static annotation",
            ));
        }
        if let Some(default) = &parameter.default {
            let inferred = infer_contextual(
                default,
                ty.as_ref().unwrap_or(&Type::Error),
                &function_scope,
                &local,
                diagnostics,
            );
            require_same(
                ty.as_ref().unwrap_or(&Type::Error),
                &inferred.ty,
                diagnostics,
            );
            default_effects.join(&inferred.effects);
        }
        let ty = ty.unwrap_or(Type::Error);
        if ty == Type::Error {
            // Preserve a simple parameter name after an invalid annotation so
            // references in the body do not add a misleading unresolved-name
            // cascade. The annotation diagnostic remains authoritative.
            if let Pattern::Name(name, _) = &parameter.pattern {
                insert_local_binding(name, Type::Error, &mut local, diagnostics);
                if let Some(symbol) = local.get_mut(name) {
                    symbol.kind = SymbolKind::Parameter;
                }
                continue;
            }
        }
        bind_parameter_pattern(&parameter.pattern, ty, scope, &mut local, diagnostics);
    }
    if let Some(expected) = signature
        .result
        .as_ref()
        .map(|result| resolved_type_of(result, &function_scope))
    {
        let mut inferred =
            infer_function_body(body, Some(&expected), &function_scope, &local, diagnostics);
        inferred.effects.join(&default_effects);
        require_same_in(&function_scope, &expected, &inferred.ty, diagnostics);
        if let Some(symbol) = symbols.get_mut(&signature.name) {
            symbol.effects = inferred.effects;
        }
    } else {
        let mut inferred = infer_function_body(body, None, &function_scope, &local, diagnostics);
        inferred.effects.join(&default_effects);
        if let Some(symbol) = symbols.get_mut(&signature.name) {
            symbol.effects = inferred.effects;
            if let Type::Function { result, .. } = &mut symbol.ty {
                **result = inferred.ty;
            }
        }
    }
    let parameter_names = local.keys().cloned().collect::<BTreeSet<_>>();
    let dependencies = tables_referenced(
        body,
        Some(&scope.table_rows),
        Some(scope),
        Some(&parameter_names),
    );
    scope
        .function_dependencies
        .borrow_mut()
        .insert(signature.name.clone(), dependencies);
    validate_loop_transfers(body, &function_scope, &local, &mut Vec::new(), diagnostics);
}

pub(crate) fn type_mentions_generic(ty: &Type, generic_names: &BTreeSet<String>) -> bool {
    match ty {
        Type::Named(name) => generic_names.contains(name),
        Type::List(inner)
        | Type::Range(inner)
        | Type::Relation(inner)
        | Type::Stream(inner)
        | Type::Optional(inner) => type_mentions_generic(inner, generic_names),
        Type::Tuple(elements) => elements
            .iter()
            .any(|element| type_mentions_generic(element, generic_names)),
        Type::Record(fields) => fields
            .values()
            .any(|field| type_mentions_generic(field, generic_names)),
        Type::Applied { arguments, .. } => arguments
            .iter()
            .any(|argument| type_mentions_generic(argument, generic_names)),
        Type::Quantity { numeric, .. } => type_mentions_generic(numeric, generic_names),
        Type::MoneyPerUnit { currency, unit } => {
            type_mentions_generic(currency, generic_names)
                || type_mentions_generic(unit, generic_names)
        }
        Type::Function {
            parameters, result, ..
        } => {
            parameters
                .iter()
                .any(|parameter| type_mentions_generic(parameter, generic_names))
                || type_mentions_generic(result, generic_names)
        }
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Bottom
        | Type::Error => false,
    }
}

pub(crate) fn function_parameters_are_concrete(
    ty: &Type,
    generic_names: &BTreeSet<String>,
) -> bool {
    let Type::Function { parameters, .. } = ty else {
        return false;
    };
    parameters
        .iter()
        .all(|parameter| !type_mentions_generic(parameter, generic_names))
}

pub(crate) fn substitute_generic_type(ty: &Type, substitutions: &BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Named(name) => substitutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::List(inner) => Type::List(Box::new(substitute_generic_type(inner, substitutions))),
        Type::Range(inner) => Type::Range(Box::new(substitute_generic_type(inner, substitutions))),
        Type::Relation(inner) => {
            Type::Relation(Box::new(substitute_generic_type(inner, substitutions)))
        }
        Type::Stream(inner) => {
            Type::Stream(Box::new(substitute_generic_type(inner, substitutions)))
        }
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), substitute_generic_type(field, substitutions)))
                .collect(),
        ),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute_generic_type(element, substitutions))
                .collect(),
        ),
        Type::Optional(inner) => {
            Type::Optional(Box::new(substitute_generic_type(inner, substitutions)))
        }
        Type::Applied { base, arguments } => Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_generic_type(argument, substitutions))
                .collect(),
        },
        Type::Quantity {
            numeric,
            dimension_basis,
            scale,
            offset,
            affine,
        } => Type::Quantity {
            numeric: Box::new(substitute_generic_type(numeric, substitutions)),
            dimension_basis: dimension_basis.clone(),
            scale: scale.clone(),
            offset: offset.clone(),
            affine: *affine,
        },
        Type::MoneyPerUnit { currency, unit } => Type::MoneyPerUnit {
            currency: Box::new(substitute_generic_type(currency, substitutions)),
            unit: Box::new(substitute_generic_type(unit, substitutions)),
        },
        Type::Function {
            parameters,
            parameter_names,
            default_parameters,
            result,
        } => Type::Function {
            parameters: parameters
                .iter()
                .map(|parameter| substitute_generic_type(parameter, substitutions))
                .collect(),
            parameter_names: parameter_names.clone(),
            default_parameters: default_parameters.clone(),
            result: Box::new(substitute_generic_type(result, substitutions)),
        },
        Type::Int
        | Type::Decimal
        | Type::Float
        | Type::Date
        | Type::Instant
        | Type::Text
        | Type::Bool
        | Type::Null
        | Type::Unit
        | Type::Bottom
        | Type::Error => ty.clone(),
    }
}

pub(crate) fn constrain_generic_type(
    expected: &Type,
    actual: &Type,
    generic_names: &BTreeSet<String>,
    substitutions: &mut BTreeMap<String, Type>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Type::Named(name) = expected
        && generic_names.contains(name)
    {
        if let Some(previous) = substitutions.get(name) {
            if previous != actual && previous != &Type::Error && actual != &Type::Error {
                diagnostics.push(diag(DIAG_TYPE, "generic argument types are inconsistent"));
            }
        } else {
            substitutions.insert(name.clone(), actual.clone());
        }
        return;
    }
    match (expected, actual) {
        (Type::List(expected), Type::List(actual))
        | (Type::Range(expected), Type::Range(actual))
        | (Type::Relation(expected), Type::Relation(actual))
        | (Type::Stream(expected), Type::Stream(actual))
        | (Type::Optional(expected), Type::Optional(actual)) => {
            constrain_generic_type(expected, actual, generic_names, substitutions, diagnostics)
        }
        (
            Type::Function {
                parameters: expected_parameters,
                result: expected_result,
                ..
            },
            Type::Function {
                parameters: actual_parameters,
                result: actual_result,
                ..
            },
        ) if expected_parameters.len() == actual_parameters.len() => {
            for (expected, actual) in expected_parameters.iter().zip(actual_parameters) {
                constrain_generic_type(expected, actual, generic_names, substitutions, diagnostics);
            }
            constrain_generic_type(
                expected_result,
                actual_result,
                generic_names,
                substitutions,
                diagnostics,
            );
        }
        (
            Type::Applied {
                base: expected_base,
                arguments: expected_arguments,
            },
            Type::Applied {
                base: actual_base,
                arguments: actual_arguments,
            },
        ) if expected_base == actual_base && expected_arguments.len() == actual_arguments.len() => {
            for (expected, actual) in expected_arguments.iter().zip(actual_arguments) {
                constrain_generic_type(expected, actual, generic_names, substitutions, diagnostics);
            }
        }
        (Type::Tuple(expected), Type::Tuple(actual)) if expected.len() == actual.len() => {
            for (expected, actual) in expected.iter().zip(actual) {
                constrain_generic_type(expected, actual, generic_names, substitutions, diagnostics);
            }
        }
        (Type::Record(expected), Type::Record(actual)) => {
            for (name, expected) in expected {
                if let Some(actual) = actual.get(name) {
                    constrain_generic_type(
                        expected,
                        actual,
                        generic_names,
                        substitutions,
                        diagnostics,
                    );
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn bound_protocol_type(bound: &TypeExpr, scope: &Scope) -> Option<Type> {
    let Type::Named(name) = resolved_type_of(bound, scope) else {
        return None;
    };
    if scope.local_protocols.contains_key(&name)
        || scope
            .names
            .get(&name)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Protocol)
        || matches!(name.as_str(), "Display" | "Present")
    {
        Some(Type::Named(name))
    } else {
        None
    }
}

pub(crate) fn type_implements_protocol(ty: &Type, protocol: &Type, scope: &Scope) -> bool {
    let Type::Named(name) = ty else {
        return false;
    };
    if scope
        .generic_type_bounds
        .get(name)
        .into_iter()
        .flatten()
        .filter_map(|bound| bound_protocol_type(bound, scope))
        .any(|bound| bound == *protocol)
    {
        return true;
    }
    scope
        .protocol_implementations
        .get(name)
        .is_some_and(|implementations| {
            implementations
                .iter()
                .any(|candidate| candidate == protocol)
        })
}

pub(crate) fn resolved_generic_parameters(
    callee: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<Vec<GenericParameterMetadata>> {
    if let Expr::Name { text, .. } = callee {
        if local.contains_key(text) {
            return None;
        }
        if let Some(symbol) = scope.names.get(text)
            && !symbol.generic_parameters.is_empty()
        {
            return Some(symbol.generic_parameters.clone());
        }
        return scope.generic_functions.get(text).map(|signature| {
            signature
                .generics
                .iter()
                .map(|generic| GenericParameterMetadata {
                    name: generic.name.clone(),
                    bounds: generic.bounds.iter().map(type_of).collect(),
                })
                .collect()
        });
    }
    let path = qualified_path(callee)?;
    if path.len() < 2 || local.contains_key(path[0]) {
        return None;
    }
    let namespace = module_member_namespace(&path, scope)?;
    scope
        .available_modules
        .get(&namespace)
        .and_then(|module| module.generic_functions.get(path[path.len() - 1]))
        .cloned()
}

pub(crate) fn generic_bound_protocol(bound: &Type, scope: &Scope) -> Option<Type> {
    let Type::Named(name) = bound else {
        return None;
    };
    if scope.local_protocols.contains_key(name)
        || scope
            .names
            .get(name)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Protocol)
        || qualified_export(scope, name).is_some_and(|symbol| symbol.kind == SymbolKind::Protocol)
        || matches!(name.as_str(), "Display" | "Present")
    {
        Some(bound.clone())
    } else {
        None
    }
}

pub(crate) fn qualified_export<'a>(scope: &'a Scope, name: &str) -> Option<&'a Symbol> {
    let parts = name.split('.').collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }
    let direct_namespace = Namespace(
        parts[..parts.len() - 1]
            .iter()
            .map(|part| (*part).to_owned())
            .collect(),
    );
    if let Some(module) = scope.available_modules.get(&direct_namespace)
        && let Some(symbol) = module.exports.get(parts[parts.len() - 1])
    {
        return Some(symbol);
    }
    let root = scope.modules.get(parts[0])?;
    let namespace = Namespace(
        root.0
            .iter()
            .cloned()
            .chain(
                parts[1..parts.len() - 1]
                    .iter()
                    .map(|part| (*part).to_owned()),
            )
            .collect(),
    );
    scope
        .available_modules
        .get(&namespace)
        .and_then(|module| module.exports.get(parts[parts.len() - 1]))
}

/// Typecheck a generic call from declaration metadata carried by its resolved
/// function symbol.
pub(crate) fn infer_local_generic_call(
    callee: &Expr,
    explicit_type_arguments: Option<&[TypeExpr]>,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let symbol = resolved_callable_symbol(callee, scope, local)?;
    let generic_parameters = resolved_generic_parameters(callee, scope, local)?;
    if symbol.kind != SymbolKind::Function || generic_parameters.is_empty() {
        return None;
    }
    let Type::Function {
        parameters: raw_parameters,
        parameter_names,
        default_parameters,
        result,
    } = &symbol.ty
    else {
        return None;
    };
    let generic_names = generic_parameters
        .iter()
        .map(|generic| generic.name.clone())
        .collect::<BTreeSet<_>>();
    let bounded_generic_names = generic_parameters
        .iter()
        .filter(|generic| !generic.bounds.is_empty())
        .map(|generic| generic.name.clone())
        .collect::<BTreeSet<_>>();
    let mut substitutions = BTreeMap::new();
    let mut valid_substitution = true;
    if let Some(type_arguments) = explicit_type_arguments {
        if type_arguments.len() != generic_parameters.len() {
            diagnostics.push(diag(
                DIAG_TYPE,
                "generic argument count does not match the function signature",
            ));
            valid_substitution = false;
        }
        for (generic, argument) in generic_parameters.iter().zip(type_arguments) {
            let Some(argument) = valid_static_type(argument, scope) else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "generic type argument is not a valid static type",
                ));
                valid_substitution = false;
                continue;
            };
            substitutions.insert(generic.name.clone(), argument);
        }
    }
    let mut effects = intrinsic_call_effects(callee);
    effects.join(&symbol.effects);
    let mut values = vec![Type::Error; arguments.len()];
    let mut argument_diagnostics = vec![Vec::new(); arguments.len()];
    // Generic value arguments establish callback input types. Infer them
    // before lambdas so named argument order cannot leave a predicate's
    // parameter unbound when its source value appears later in the call.
    let inference_order = (0..arguments.len())
        .filter(|index| !matches!(arguments[*index].value, Expr::Lambda { .. }))
        .chain(
            (0..arguments.len())
                .filter(|index| matches!(arguments[*index].value, Expr::Lambda { .. })),
        );
    for index in inference_order {
        let argument = &arguments[index];
        let expected =
            expected_call_parameter(raw_parameters, parameter_names.as_deref(), arguments, index);
        let contextual_expected =
            expected.map(|expected| substitute_generic_type(expected, &substitutions));
        let explicit_context = explicit_type_arguments
            .is_some()
            .then(|| contextual_expected.clone())
            .flatten();
        let local_constructor_scope = expected.is_some_and(|expected| {
            matches!(expected, Type::Named(name) if bounded_generic_names.contains(name))
        });
        let contextual_callback = matches!(argument.value, Expr::Lambda { .. })
            && contextual_expected
                .as_ref()
                .is_some_and(|expected| function_parameters_are_concrete(expected, &generic_names));
        let diagnostics_for_argument = &mut argument_diagnostics[index];
        let inferred = if let Some(expected) = explicit_context.as_ref() {
            infer_contextual(
                &argument.value,
                expected,
                scope,
                local,
                diagnostics_for_argument,
            )
        } else if let (true, Some(contextual_expected)) =
            (expected.is_some(), contextual_expected.as_ref())
        {
            if type_mentions_generic(contextual_expected, &generic_names) {
                if local_constructor_scope {
                    let mut argument_scope = scope.clone();
                    argument_scope.allow_local_private_nominal_construction = true;
                    infer(
                        &argument.value,
                        &argument_scope,
                        local,
                        diagnostics_for_argument,
                    )
                } else if contextual_callback {
                    // The callback's input can be concrete while its result
                    // still determines another generic. Leave only those
                    // result variables open for inference after contextualizing.
                    let unresolved = generic_names
                        .iter()
                        .filter(|name| !substitutions.contains_key(*name))
                        .map(|name| (name.clone(), Type::Error))
                        .collect::<BTreeMap<_, _>>();
                    let callback_context =
                        substitute_generic_type(contextual_expected, &unresolved);
                    infer_contextual(
                        &argument.value,
                        &callback_context,
                        scope,
                        local,
                        diagnostics_for_argument,
                    )
                } else {
                    infer(&argument.value, scope, local, diagnostics_for_argument)
                }
            } else {
                infer_contextual(
                    &argument.value,
                    contextual_expected,
                    scope,
                    local,
                    diagnostics_for_argument,
                )
            }
        } else {
            infer(&argument.value, scope, local, diagnostics_for_argument)
        };
        effects.join(&inferred.effects);
        if let Some(expected) = expected {
            constrain_generic_type(
                expected,
                &inferred.ty,
                &generic_names,
                &mut substitutions,
                diagnostics_for_argument,
            );
        }
        values[index] = inferred.ty;
    }
    for diagnostics_for_argument in argument_diagnostics {
        diagnostics.extend(diagnostics_for_argument);
    }
    for generic in &generic_parameters {
        let Some(actual) = substitutions.get(&generic.name) else {
            diagnostics.push(diag(DIAG_TYPE, "generic type argument cannot be inferred"));
            valid_substitution = false;
            continue;
        };
        for bound in &generic.bounds {
            let Some(protocol) = generic_bound_protocol(bound, scope) else {
                diagnostics.push(diag(DIAG_TYPE, "generic bound must name a protocol"));
                valid_substitution = false;
                continue;
            };
            if !type_implements_protocol(actual, &protocol, scope) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "generic type argument does not satisfy its protocol bound",
                ));
                valid_substitution = false;
            }
        }
    }
    let parameters = raw_parameters
        .iter()
        .map(|parameter| substitute_generic_type(parameter, &substitutions))
        .collect::<Vec<_>>();
    check_call_arguments(
        &parameters,
        parameter_names.as_deref(),
        default_parameters,
        arguments,
        &values,
        None,
        diagnostics,
    );
    Some(Inferred {
        ty: if valid_substitution {
            substitute_generic_type(result, &substitutions)
        } else {
            Type::Error
        },
        effects,
    })
}

/// Codec decoders take their generic result type through the language's
/// `as: T` type-witness argument. That argument is syntax, not a runtime
/// value, so remove it before checking the codec's ordinary data/options
/// parameters and use it as the call result type.
pub(crate) fn infer_codec_decode_call(
    callee: &Expr,
    arguments: &[orna_syntax::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let path = qualified_path(callee)?;
    let root = *path.first()?;
    if local.contains_key(root) {
        return None;
    }
    let resolved = if root == "std" {
        if !scope.modules.contains_key("std") {
            return None;
        }
        path.iter()
            .map(|part| (*part).to_owned())
            .collect::<Vec<_>>()
    } else {
        let namespace = scope.modules.get(root)?;
        namespace
            .0
            .iter()
            .cloned()
            .chain(path[1..].iter().map(|part| (*part).to_owned()))
            .collect::<Vec<_>>()
    };
    let operation = match resolved.join(".").as_str() {
        "std.encoding.json.decode" => (Type::Text, false),
        "std.encoding.json.decode_with_options" => (Type::Text, true),
        "std.encoding.orna.decode" => (Type::Text, false),
        "std.encoding.ovb.decode" => (Type::Named("std.BINARY_LARGE_OBJECT".into()), false),
        _ => return None,
    };

    let mut witness = None;
    let mut input = None;
    let mut saw_options = false;
    let mut positional_seen = false;
    let mut effects = EffectSummary::default();
    for argument in arguments {
        match argument.name.as_deref() {
            Some("as") if witness.is_none() => {
                witness = start_type_witness(&argument.value, scope);
                if witness.is_none() {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "codec result type witness is not a known static type",
                    ));
                }
            }
            Some("input") if input.is_none() => {
                let inferred =
                    infer_contextual(&argument.value, &operation.0, scope, local, diagnostics);
                effects.join(&inferred.effects);
                input = Some(inferred.ty);
            }
            Some("ignore_unknown_fields") if operation.1 && !saw_options => {
                let inferred =
                    infer_contextual(&argument.value, &Type::Bool, scope, local, diagnostics);
                effects.join(&inferred.effects);
                saw_options = true;
            }
            None if input.is_none() && !positional_seen => {
                let inferred =
                    infer_contextual(&argument.value, &operation.0, scope, local, diagnostics);
                effects.join(&inferred.effects);
                input = Some(inferred.ty);
                positional_seen = true;
            }
            _ => diagnostics.push(diag(
                DIAG_TYPE,
                "codec decode arguments do not match the documented signature",
            )),
        }
    }
    let Some(witness) = witness else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "codec decode requires an explicit `as: T` type witness",
        ));
        return Some(Inferred::error_with(effects));
    };
    if input.is_none() {
        diagnostics.push(diag(DIAG_TYPE, "codec decode requires an input value"));
    }
    Some(Inferred {
        ty: witness,
        effects,
    })
}

/// Validate transfer statements after ordinary expression inference. `for` and
/// `while` are the only loop forms this semantic slice gives a result type, so
/// their nearest-loop break value is statically constrained to that result.
/// A lambda begins a new function boundary and therefore cannot transfer to a
/// loop surrounding its creation site.
pub(crate) fn validate_loop_transfers(
    expression: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    loops: &mut Vec<LoopTransferContext>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match expression {
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                if let StringSegment::Expression { value, .. } = segment {
                    validate_loop_transfers(value, scope, local, loops, diagnostics);
                }
            }
        }
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
            validate_loop_transfers(rhs, scope, local, loops, diagnostics);
        }
        Expr::Binary { lhs, rhs, .. } => {
            validate_loop_transfers(lhs, scope, local, loops, diagnostics);
            validate_loop_transfers(rhs, scope, local, loops, diagnostics);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(lower) = lower {
                validate_loop_transfers(lower, scope, local, loops, diagnostics);
            }
            if let Some(upper) = upper {
                validate_loop_transfers(upper, scope, local, loops, diagnostics);
            }
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            validate_loop_transfers(callee, scope, local, loops, diagnostics);
            for argument in arguments {
                validate_loop_transfers(&argument.value, scope, local, loops, diagnostics);
            }
        }
        Expr::GenericCall {
            callee, arguments, ..
        } => {
            validate_loop_transfers(callee, scope, local, loops, diagnostics);
            for argument in arguments {
                validate_loop_transfers(&argument.value, scope, local, loops, diagnostics);
            }
        }
        Expr::Index { base, index, .. } => {
            validate_loop_transfers(base, scope, local, loops, diagnostics);
            validate_loop_transfers(index, scope, local, loops, diagnostics);
        }
        Expr::Field { base, .. } => {
            validate_loop_transfers(base, scope, local, loops, diagnostics);
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for element in elements {
                validate_loop_transfers(element, scope, local, loops, diagnostics);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            for field in fields {
                validate_loop_transfers(&field.value, scope, local, loops, diagnostics);
            }
        }
        Expr::Lambda {
            parameters, body, ..
        } => {
            let mut lambda_locals = local.clone();
            for parameter in parameters {
                bind_pattern(
                    &parameter.pattern,
                    parameter
                        .annotation
                        .as_ref()
                        .map(type_of)
                        .unwrap_or(Type::Error),
                    scope,
                    &mut lambda_locals,
                    diagnostics,
                );
            }
            validate_loop_transfers(body, scope, &lambda_locals, &mut Vec::new(), diagnostics);
        }
        Expr::Block {
            statements, tail, ..
        } => {
            let mut block_locals = local.clone();
            let mut terminated = false;
            for statement in statements {
                match statement {
                    Statement::Let {
                        pattern,
                        annotation,
                        value,
                        ..
                    } => {
                        validate_loop_transfers(value, scope, &block_locals, loops, diagnostics);
                        let ty = annotation.as_ref().map(type_of).unwrap_or_else(|| {
                            infer(value, scope, &block_locals, &mut Vec::new()).ty
                        });
                        bind_pattern(pattern, ty, scope, &mut block_locals, diagnostics);
                    }
                    Statement::Assert { value, .. }
                    | Statement::Expression { value, .. }
                    | Statement::Control { value, .. }
                    | Statement::Assignment { value, .. } => {
                        validate_loop_transfers(value, scope, &block_locals, loops, diagnostics)
                    }
                    Statement::Break { value, .. } => match loops.last() {
                        Some(LoopTransferContext::Typed(expected)) => {
                            let actual = value.as_ref().map_or(Type::Unit, |value| {
                                infer(value, scope, &block_locals, &mut Vec::new()).ty
                            });
                            require_same(expected, &actual, diagnostics);
                        }
                        // `loop` has no inferred result contract yet. Reject
                        // transfers explicitly instead of silently accepting a
                        // value whose type cannot be checked.
                        Some(LoopTransferContext::Unsupported) => diagnostics.push(diag(
                            DIAG_UNSUPPORTED,
                            "break transfer requires a loop result contract",
                        )),
                        None => diagnostics.push(diag(
                            DIAG_UNSUPPORTED,
                            "break transfer requires an enclosing supported loop",
                        )),
                    },
                    Statement::Continue { .. } => {
                        if loops.is_empty() {
                            diagnostics.push(diag(
                                DIAG_UNSUPPORTED,
                                "continue transfer requires an enclosing supported loop",
                            ));
                        }
                    }
                    Statement::Return { value, .. } => {
                        if let Some(value) = value {
                            validate_loop_transfers(
                                value,
                                scope,
                                &block_locals,
                                loops,
                                diagnostics,
                            );
                        }
                        terminated = true;
                        break;
                    }
                }
            }
            if let Some(tail) = tail
                && !terminated
            {
                validate_loop_transfers(tail, scope, &block_locals, loops, diagnostics);
            }
        }
        Expr::Control {
            kind,
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if let Some(condition) = condition {
                validate_loop_transfers(condition, scope, local, loops, diagnostics);
            }
            let loop_context = match kind {
                ControlKind::For | ControlKind::While => {
                    Some(LoopTransferContext::Typed(Type::Unit))
                }
                ControlKind::Loop => Some(LoopTransferContext::Unsupported),
                ControlKind::If | ControlKind::Case => None,
            };
            let enters_loop = loop_context.is_some();
            let mut body_locals = local.clone();
            if let (ControlKind::For, Some(binding), Some(iterable)) = (kind, binding, condition)
                && let Some(element) = finite_for_element_type(
                    iterable,
                    &infer(iterable, scope, local, &mut Vec::new()).ty,
                )
            {
                bind_pattern(binding, element, scope, &mut body_locals, &mut Vec::new());
            }
            if let Some(loop_context) = loop_context {
                loops.push(loop_context);
            }
            if let Some(body) = body {
                validate_loop_transfers(body, scope, &body_locals, loops, diagnostics);
            }
            if !matches!(kind, ControlKind::For) {
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        validate_loop_transfers(guard, scope, local, loops, diagnostics);
                    }
                    validate_loop_transfers(&arm.body, scope, local, loops, diagnostics);
                }
            }
            if let Some(alternate) = alternate {
                validate_loop_transfers(alternate, scope, local, loops, diagnostics);
            }
            if enters_loop {
                loops.pop();
            }
        }
        Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
    }
}
