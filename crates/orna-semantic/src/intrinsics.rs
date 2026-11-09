//! Registry-driven typing for the native collection, query and statistics
//! intrinsics.
//!
//! The `orna-intrinsics` catalogue is the single declaration site for every
//! native operation. This module maps its closed [`SigType`] language onto the
//! checker's [`Type`] with one total conversion, resolves a call spelling to an
//! [`IntrinsicId`], and types the call from the descriptor's [`Signature`]:
//! argument names, arity, pipeline insertion and the contextual-lambda rule all
//! come from the registry row rather than a hand-written table.
//!
//! The few rules a signature cannot express stay here as bare-minimum
//! refinements: lawful equality and total order, non-negative and positive
//! constant arguments, collection-kind agreement, calendar bucketing and the
//! quantity-aware statistics results.

use std::collections::BTreeMap;

use orna_foundation::Diagnostic;
use orna_intrinsics::{Intrinsic, IntrinsicId, Module, Param, SigType};
use orna_syntax::{Argument, Expr};

use super::*;

/// Whether a container-polymorphic intrinsic is applied to a finite list or a
/// relation. The catalogue declares both overloads as `[T]`; the input
/// argument selects the concrete one, which is the only rule the hand-written
/// tables implemented (`spec 09:21`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Container {
    List,
    Relation,
}

impl Container {
    fn of(ty: &Type) -> Option<Self> {
        match ty {
            Type::List(_) => Some(Self::List),
            Type::Relation(_) => Some(Self::Relation),
            _ => None,
        }
    }
}

/// Resolves `SigType` against the concrete types bound for a call.
///
/// `slot` marks a position that carries the input container kind: the
/// outermost `[T]` there becomes `Relation<T>` for the relation overload.
/// Nested lists (for example the groups inside `group_by`) stay finite lists.
fn signature_type(sig: SigType, subst: &[Option<Type>], container: Container, slot: bool) -> Type {
    match sig {
        SigType::Unit => Type::Unit,
        SigType::Bool => Type::Bool,
        SigType::Int => Type::Int,
        SigType::Float => Type::Float,
        SigType::Decimal => Type::Decimal,
        SigType::Str => Type::Text,
        SigType::Blob => Type::Named("Blob".into()),
        SigType::Path => Type::Named("Path".into()),
        SigType::Digest => Type::Named("Digest".into()),
        SigType::Uuid => Type::Named("std.Uuid".into()),
        SigType::Date => Type::Date,
        SigType::Instant => Type::Instant,
        SigType::LocalDateTime => Type::Named("std.LocalDateTime".into()),
        SigType::TimeOfDay => Type::Named("std.TimeOfDay".into()),
        SigType::Duration => Type::Named("std.DURATION".into()),
        SigType::TimeZone => Type::Named("std.TimeZone".into()),
        SigType::ZonedDateTime => Type::Named("std.ZonedDateTime".into()),
        SigType::Error => Type::Named("Error".into()),
        SigType::JsonValue => Type::Named("JsonValue".into()),
        SigType::Ui => Type::Named("std.Ui".into()),
        SigType::Money => Type::Applied {
            base: "Money".into(),
            arguments: vec![Type::Error],
        },
        SigType::List(element) if slot && container == Container::Relation => {
            Type::Relation(Box::new(signature_type(*element, subst, container, false)))
        }
        SigType::List(element) => {
            Type::List(Box::new(signature_type(*element, subst, container, false)))
        }
        SigType::Relation(element) => {
            Type::Relation(Box::new(signature_type(*element, subst, container, false)))
        }
        SigType::Query(element) => Type::Named(format!(
            "Query<{}>",
            type_label(&signature_type(*element, subst, container, false))
        )),
        SigType::Stream(element) => {
            Type::Stream(Box::new(signature_type(*element, subst, container, false)))
        }
        SigType::Option(element) => {
            Type::Optional(Box::new(signature_type(*element, subst, container, false)))
        }
        SigType::Range(element) => {
            Type::Range(Box::new(signature_type(*element, subst, container, false)))
        }
        SigType::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| signature_type(*element, subst, container, false))
                .collect(),
        ),
        SigType::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, ty)| {
                    (
                        (*name).to_owned(),
                        signature_type(*ty, subst, container, false),
                    )
                })
                .collect(),
        ),
        SigType::Nominal(name) => Type::Named((*name).to_owned()),
        SigType::Fn(parameters, result) => Type::Function {
            parameters: parameters
                .iter()
                .map(|parameter| signature_type(*parameter, subst, container, false))
                .collect(),
            parameter_names: None,
            default_parameters: std::collections::BTreeSet::new(),
            result: Box::new(signature_type(*result, subst, container, true)),
        },
        SigType::Param(index) => subst
            .get(usize::from(index))
            .and_then(Clone::clone)
            .unwrap_or(Type::Error),
    }
}

/// Binds an unbound type parameter from an actual argument type.
fn bind_type(sig: SigType, actual: &Type, subst: &mut [Option<Type>], _container: Container) {
    match (sig, actual) {
        (SigType::Param(index), _) => {
            if let Some(slot) = subst.get_mut(usize::from(index))
                && slot.is_none()
            {
                *slot = Some(actual.clone());
            }
        }
        (SigType::List(expected), Type::List(actual) | Type::Relation(actual)) => {
            bind_type(*expected, actual.as_ref(), subst, _container);
        }
        (SigType::Relation(expected), Type::Relation(actual)) => {
            bind_type(*expected, actual.as_ref(), subst, _container);
        }
        (SigType::Option(expected), Type::Optional(actual)) => {
            bind_type(*expected, actual.as_ref(), subst, _container);
        }
        (SigType::Option(expected), actual) => bind_type(*expected, actual, subst, _container),
        (SigType::Range(expected), Type::Range(actual)) => {
            bind_type(*expected, actual.as_ref(), subst, _container);
        }
        (SigType::Stream(expected), Type::Stream(actual)) => {
            bind_type(*expected, actual.as_ref(), subst, _container);
        }
        (SigType::Tuple(expected), Type::Tuple(actual)) if expected.len() == actual.len() => {
            for (expected, actual) in expected.iter().zip(actual) {
                bind_type(*expected, actual, subst, _container);
            }
        }
        (
            SigType::Fn(expected_parameters, expected_result),
            Type::Function {
                parameters, result, ..
            },
        ) if expected_parameters.len() == parameters.len() => {
            for (expected, actual) in expected_parameters.iter().zip(parameters) {
                bind_type(*expected, actual, subst, _container);
            }
            bind_type(*expected_result, result.as_ref(), subst, _container);
        }
        _ => {}
    }
}

/// A resolved native callee spelling.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResolvedIntrinsic {
    pub(crate) id: IntrinsicId,
    pub(crate) root: bool,
}

impl ResolvedIntrinsic {
    pub(crate) fn descriptor(self) -> &'static Intrinsic {
        orna_intrinsics::by_id(self.id)
    }
}

/// The collection helpers that also work at the root.
///
/// The registry's root environment lists the `Core*` operations; the evaluator
/// additionally roots `pairs`, `rank` and `bucket_by`, and the owner accepted
/// their checker typing as a bug fix (decision D4). `project` is not an
/// intrinsic and stays rejected.
const ROOTED_COLLECTION_HELPERS: &[(&str, IntrinsicId)] = &[
    ("pairs", IntrinsicId::CollectionPairs),
    ("rank", IntrinsicId::CollectionRank),
    ("bucket_by", IntrinsicId::CollectionBucketBy),
    ("window", IntrinsicId::CollectionWindow),
];

/// Whether this stage types the id from the registry instead of the pinned
/// source declaration. Only the container-polymorphic families need it.
pub(crate) fn is_registry_typed(id: IntrinsicId) -> bool {
    matches!(
        orna_intrinsics::by_id(id).module,
        Module::Collection | Module::Query | Module::Stats
    )
}

fn is_stats_module(id: IntrinsicId) -> bool {
    orna_intrinsics::by_id(id).module == Module::Stats
}

fn is_collection_module(id: IntrinsicId) -> bool {
    matches!(
        orna_intrinsics::by_id(id).module,
        Module::Collection | Module::Query
    )
}

/// Resolves an unqualified root intrinsic name, honouring ordinary lexical
/// shadowing.
pub(crate) fn root_intrinsic_id(
    name: &str,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<IntrinsicId> {
    if local.contains_key(name) || scope.names.contains_key(name) {
        return None;
    }
    if let Some((_, id)) = ROOTED_COLLECTION_HELPERS
        .iter()
        .find(|(rooted, _)| *rooted == name)
    {
        return Some(*id);
    }
    orna_intrinsics::root_intrinsic(name).filter(|id| is_registry_typed(*id))
}

/// Resolves a qualified `std.<module>.<name>` or imported alias spelling.
fn module_intrinsic_id(namespace: &Namespace, name: &str, scope: &Scope) -> Option<IntrinsicId> {
    if name.starts_with("__") || namespace.0.first().map(String::as_str) != Some("std") {
        return None;
    }
    let module = Module::from_path(&namespace.0.join("."))?;
    let intrinsic = orna_intrinsics::lookup(module, name)?;
    if !is_registry_typed(intrinsic.id) || intrinsic.name != name {
        return None;
    }
    if is_stats_module(intrinsic.id) {
        return Some(intrinsic.id);
    }
    // A collection/query spelling is admitted only when the captured module
    // exports that declaration. The pinned source re-exports only the helpers,
    // so `std.collection.count` stays rejected until the std repo declares the
    // core re-exports (stage S4, decision D4).
    let declared = scope
        .available_modules
        .get(namespace)
        .and_then(|module| module.exports.get(name))
        .is_some_and(|symbol| symbol.kind == SymbolKind::Function);
    declared.then_some(intrinsic.id)
}

/// Resolves a call callee to a native intrinsic spelling.
pub(crate) fn resolve_intrinsic(
    callee: &Expr,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
) -> Option<ResolvedIntrinsic> {
    let path = qualified_path(callee)?;
    match path.as_slice() {
        [name] => Some(ResolvedIntrinsic {
            id: root_intrinsic_id(name, scope, local)?,
            root: true,
        }),
        ["std", module @ ("collection" | "query" | "stats"), name]
            if !local.contains_key("std") =>
        {
            let namespace = Namespace(vec!["std".into(), (*module).into()]);
            Some(ResolvedIntrinsic {
                id: module_intrinsic_id(&namespace, name, scope)?,
                root: false,
            })
        }
        [alias, name] if !local.contains_key(*alias) => {
            let namespace = scope.modules.get(*alias)?;
            Some(ResolvedIntrinsic {
                id: module_intrinsic_id(namespace, name, scope)?,
                root: false,
            })
        }
        _ => None,
    }
}

/// Types a direct call to a registry-typed collection or query intrinsic.
///
/// Returns `None` when the callee is not a registry-typed native, so ordinary
/// source resolution (and the shadowing rule) keeps its precedence.
pub(crate) fn infer_collection_call(
    callee: &Expr,
    arguments: &[Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    // The frozen parser represents `!exists(rows, predicate)` as a call whose
    // callee is unary `!exists`; it is still the root `exists` form, and its
    // result is already `Bool`.
    if let Expr::Unary { op, rhs, .. } = callee
        && op == "!"
        && let Some(resolved) = resolve_intrinsic(rhs, scope, local)
        && resolved.id == IntrinsicId::CoreExists
    {
        return Some(infer_intrinsic(
            resolved.id,
            None,
            arguments,
            scope,
            local,
            diagnostics,
        ));
    }
    let resolved = resolve_intrinsic(callee, scope, local)?;
    if !is_registry_typed(resolved.id) || is_stats_module(resolved.id) {
        return None;
    }
    let inferred = infer_intrinsic(resolved.id, None, arguments, scope, local, diagnostics);
    Some(check_declared_aggregate_element(
        callee,
        resolved,
        inferred,
        scope,
        diagnostics,
    ))
}

/// A pinned module may re-export an aggregate with a concrete element type
/// (`min(rows: [Decimal]): Decimal?`). The registry declares the generic form,
/// so a module spelling must still respect the declaration it re-exports.
pub(crate) fn check_declared_aggregate_element(
    callee: &Expr,
    resolved: ResolvedIntrinsic,
    mut inferred: Inferred,
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    if resolved.root
        || !matches!(
            resolved.id,
            IntrinsicId::CoreSum | IntrinsicId::CoreMin | IntrinsicId::CoreMax
        )
        || matches!(inferred.ty, Type::Error)
    {
        return inferred;
    }
    let name = orna_intrinsics::by_id(resolved.id).name;
    let Some(expected) = finite_list_aggregate_element(callee, name, scope) else {
        return inferred;
    };
    let actual = match &inferred.ty {
        Type::Optional(element) => element.as_ref().clone(),
        other => other.clone(),
    };
    if actual != expected {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!(
                "finite-list {name} requires a finite list with a supported {name} element type"
            ),
        ));
        inferred.ty = match resolved.id {
            IntrinsicId::CoreSum => expected,
            _ => Type::Optional(Box::new(expected)),
        };
    }
    inferred
}

/// Types a direct `std.stats` call. The relation row argument selects the
/// relation overload, exactly as the removed statistics name table did.
pub(crate) fn infer_statistics_call(
    callee: &Expr,
    arguments: &[Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let resolved = resolve_intrinsic(callee, scope, local)?;
    if !is_stats_module(resolved.id) {
        return None;
    }
    let row_name = orna_intrinsics::by_id(resolved.id).signature.params[0].name;
    let row = arguments
        .iter()
        .position(|argument| argument.name.as_deref() == Some(row_name))
        .or_else(|| {
            arguments
                .iter()
                .position(|argument| argument.name.is_none())
        })?;
    let mut probe_diagnostics = Vec::new();
    let probe = infer(&arguments[row].value, scope, local, &mut probe_diagnostics);
    if !matches!(probe.ty, Type::Relation(_)) {
        return None;
    }
    Some(infer_intrinsic(
        resolved.id,
        None,
        arguments,
        scope,
        local,
        diagnostics,
    ))
}

/// Whether the hand-written relation pipeline claimed this operation's shape.
///
/// Malformed shapes stay outside the semantic slice and keep the existing
/// `ORNA-S022-UNSUPPORTED` diagnostic, so the registry engine only claims the
/// argument lists the reference accepted.
pub(crate) fn relation_pipeline_shape_claimed(id: IntrinsicId, arguments: &[Argument]) -> bool {
    let named = |name: &str| {
        arguments
            .iter()
            .all(|argument| argument.name.is_none() || argument.name.as_deref() == Some(name))
    };
    match id {
        IntrinsicId::CoreSortBy => arguments.len() == 1 && named("key"),
        IntrinsicId::CoreEvery | IntrinsicId::CoreExists => {
            arguments.len() == 1 && named("predicate")
        }
        IntrinsicId::CollectionWindow => (1..=2).contains(&arguments.len()),
        IntrinsicId::CoreFirst => arguments.is_empty(),
        IntrinsicId::CoreUnion => arguments.len() == 1 && named("right"),
        IntrinsicId::CollectionRank => arguments.len() == 1 && named("key"),
        _ => false,
    }
}

/// Whether the hand-written list pipeline claimed this root operation's shape.
/// Malformed shapes keep their previous fall-through behaviour, so the registry
/// engine only claims the argument lists the reference accepted.
pub(crate) fn list_pipeline_shape_claimed(id: IntrinsicId, arguments: &[Argument]) -> bool {
    let named = |name: &str| {
        arguments
            .iter()
            .all(|argument| argument.name.is_none() || argument.name.as_deref() == Some(name))
    };
    match id {
        IntrinsicId::CoreFilter => arguments.len() == 1 && named("predicate"),
        IntrinsicId::CoreTake | IntrinsicId::CoreDrop => arguments.len() == 1 && named("count"),
        IntrinsicId::CoreDistinct | IntrinsicId::CoreLast => arguments.is_empty(),
        IntrinsicId::CoreUnion => arguments.len() == 1 && named("right"),
        // The finite-list path claimed these for any argument list.
        IntrinsicId::CoreMap
        | IntrinsicId::CoreFlatMap
        | IntrinsicId::CoreSortBy
        | IntrinsicId::CoreEvery
        | IntrinsicId::CoreExists
        | IntrinsicId::CoreFirst
        | IntrinsicId::CoreOne
        | IntrinsicId::CoreSum
        | IntrinsicId::CoreMin
        | IntrinsicId::CoreMax => true,
        // Decision D4: the evaluator-rooted helpers gain root typing.
        IntrinsicId::CollectionPairs
        | IntrinsicId::CollectionRank
        | IntrinsicId::CollectionBucketBy => true,
        _ => false,
    }
}

/// The message family the hand-written tables used for a list overload.
fn list_message_prefix(id: IntrinsicId) -> &'static str {
    match id {
        IntrinsicId::CoreFirst
        | IntrinsicId::CoreLast
        | IntrinsicId::CoreOne
        | IntrinsicId::CoreSortBy
        | IntrinsicId::CoreSum
        | IntrinsicId::CoreMin
        | IntrinsicId::CoreMax
        | IntrinsicId::CoreEvery
        | IntrinsicId::CoreExists => "finite-list",
        _ => "std.collection",
    }
}

fn relation_prefix(container: Container) -> &'static str {
    match container {
        Container::Relation => "relation ",
        Container::List => "",
    }
}

fn finiteness_message(id: IntrinsicId, name: &str) -> String {
    match id {
        IntrinsicId::CoreSum | IntrinsicId::CoreMin | IntrinsicId::CoreMax => format!(
            "finite-list {name} requires a finite list with a supported {name} element type"
        ),
        IntrinsicId::CoreFirst
        | IntrinsicId::CoreLast
        | IntrinsicId::CoreOne
        | IntrinsicId::CoreEvery
        | IntrinsicId::CoreExists
        | IntrinsicId::CoreDistinct
        | IntrinsicId::CoreTake
        | IntrinsicId::CoreDrop
        | IntrinsicId::CoreUnion
        | IntrinsicId::CoreSortBy => format!("finite-list {name} requires a finite list"),
        _ => format!("std.collection {name} requires a finite list"),
    }
}

/// The value an intrinsic call contributes to its surrounding expression.
pub(crate) fn infer_intrinsic(
    id: IntrinsicId,
    input: Option<Inferred>,
    arguments: &[Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let intrinsic = orna_intrinsics::by_id(id);
    let signature = intrinsic.signature;
    let params = signature.params;
    let pipeline = input.is_some();
    let stats = is_stats_module(id);
    let mut subst: Vec<Option<Type>> = vec![None; signature.type_params.len()];
    let mut effects = input
        .as_ref()
        .map(|input| input.effects.clone())
        .unwrap_or_default();
    let mut slots: Vec<Option<usize>> = vec![None; params.len()];
    let mut values: Vec<Option<Inferred>> = vec![None; arguments.len()];
    let mut malformed = false;
    let mut named_started = false;
    let mut positional = usize::from(pipeline);
    if pipeline {
        slots[0] = Some(usize::MAX);
    }
    for (index, argument) in arguments.iter().enumerate() {
        match argument.name.as_deref() {
            Some(name) => {
                named_started = true;
                match params.iter().position(|param| param.name == name) {
                    Some(slot) => {
                        if pipeline && params[slot].pipeline || slots[slot].replace(index).is_some()
                        {
                            malformed = true;
                        }
                    }
                    None if name == "rows" && is_collection_module(id) => {
                        if pipeline || slots[0].replace(index).is_some() {
                            malformed = true;
                        }
                    }
                    None => malformed = true,
                }
            }
            None if named_started || positional >= params.len() => malformed = true,
            None => {
                slots[positional] = Some(index);
                positional += 1;
            }
        }
    }
    for (slot, param) in params.iter().enumerate() {
        if !param.has_default && slots[slot].is_none() {
            malformed = true;
        }
    }
    if !signature.arities.is_empty()
        && !signature
            .arities
            .contains(&(arguments.len() + usize::from(pipeline)))
    {
        malformed = true;
    }

    // Infer the input container first: it binds the element type and selects
    // the finite-list or relation overload (`spec 09:21`).
    let row = if let Some(input) = &input {
        Some(input.ty.clone())
    } else if let Some(index) = slots[0] {
        let expression = &arguments[index].value;
        let value = if matches!(
            id,
            IntrinsicId::CoreSum | IntrinsicId::CoreMin | IntrinsicId::CoreMax
        ) && is_empty_list_literal(expression)
        {
            infer_finite_list_aggregate_rows(expression, None, scope, local, diagnostics)
        } else {
            infer(expression, scope, local, diagnostics)
        };
        effects.join(&value.effects);
        values[index] = Some(value.clone());
        Some(value.ty)
    } else {
        None
    };
    let container = row
        .as_ref()
        .and_then(Container::of)
        .unwrap_or(Container::List);
    if let Some(row) = &row {
        bind_type(params[0].ty, row, &mut subst, container);
    }
    if !stats {
        let missing_container = row
            .as_ref()
            .is_none_or(|row| !matches!(row, Type::List(_) | Type::Relation(_) | Type::Error));
        if missing_container {
            diagnostics.push(diag(DIAG_TYPE, finiteness_message(id, intrinsic.name)));
        }
    }

    // Infer every remaining argument once, in written order. Callbacks are
    // contextually typed after their source argument is bound.
    for (index, argument) in arguments.iter().enumerate() {
        if values[index].is_some() {
            continue;
        }
        let callback = slots.iter().enumerate().any(|(slot, assigned)| {
            *assigned == Some(index) && matches!(params[slot].ty, SigType::Fn(_, _))
        });
        if callback {
            continue;
        }
        // The finite-list mapper path skipped unbound lambdas in a malformed
        // call; the predicate path inferred them and reported their shape.
        if malformed
            && matches!(
                id,
                IntrinsicId::CoreFilter
                    | IntrinsicId::CoreMap
                    | IntrinsicId::CoreFlatMap
                    | IntrinsicId::CoreSortBy
            )
            && matches!(argument.value, Expr::Lambda { .. })
        {
            continue;
        }
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        values[index] = Some(value);
    }

    let mut invalid = false;

    // Bind and check the value parameters against the descriptor.
    for (slot, param) in params.iter().enumerate() {
        if slot == 0 || matches!(param.ty, SigType::Fn(_, _)) {
            continue;
        }
        let Some(index) = slots[slot] else {
            continue;
        };
        let Some(value) = values[index].as_ref() else {
            continue;
        };
        bind_type(param.ty, &value.ty, &mut subst, container);
        if stats || (id == IntrinsicId::CollectionBucketBy && slot >= 1) {
            continue;
        }
        check_value_parameter(
            id,
            intrinsic.name,
            slot,
            *param,
            &subst,
            &value.ty,
            &arguments[index].value,
            container,
            pipeline,
            &mut invalid,
            diagnostics,
        );
    }

    // Contextually type callbacks. A parameter of type `Fn(Param(j), _)` is
    // checked against the substituted `j`, which is how `partition`, `rank`,
    // `asof_join` and every future intrinsic gain callback typing without a
    // new table.
    for (slot, param) in params.iter().enumerate() {
        let SigType::Fn(parameters, result) = param.ty else {
            continue;
        };
        let Some(index) = slots[slot] else {
            continue;
        };
        let callback_element = parameters
            .first()
            .map(|parameter| signature_type(*parameter, &subst, container, false))
            .unwrap_or(Type::Error);
        let callback_result = if id == IntrinsicId::CoreFlatMap {
            Type::Error
        } else {
            signature_type(*result, &subst, container, false)
        };
        let callback = if container == Container::Relation {
            infer_relation_callback(
                &arguments[index].value,
                callback_element,
                callback_result,
                scope,
                local,
                diagnostics,
            )
        } else if matches!(
            id,
            IntrinsicId::CoreOne | IntrinsicId::CoreEvery | IntrinsicId::CoreExists
        ) {
            infer_finite_list_predicate(
                intrinsic.name,
                &arguments[index].value,
                callback_element,
                scope,
                local,
                diagnostics,
            )
        } else {
            if id == IntrinsicId::CoreSortBy
                && let Expr::Lambda { parameters, .. } = &arguments[index].value
                && let [parameter] = parameters.as_slice()
                && let Some(annotation) = &parameter.annotation
            {
                require_same(
                    &callback_element,
                    &resolved_type_of(annotation, scope),
                    diagnostics,
                );
            }
            infer_finite_list_callback(
                &arguments[index].value,
                callback_element,
                callback_result,
                scope,
                local,
                diagnostics,
            )
        };
        effects.join(&callback.effects);
        // The callback helpers normalise a lambda and a function value to the
        // callback's result type, so the declared `Fn` result binds here.
        bind_type(*result, &callback.ty, &mut subst, container);
        if id == IntrinsicId::CoreFlatMap {
            let inner = match &callback.ty {
                Type::List(element) => Some(element.as_ref().clone()),
                Type::Relation(element) if container == Container::Relation => {
                    Some(element.as_ref().clone())
                }
                Type::Error => Some(Type::Error),
                _ => None,
            };
            match inner {
                Some(inner) => {
                    if let Some(second) = subst.get_mut(1) {
                        second.get_or_insert(inner);
                    }
                }
                None => {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        match container {
                            Container::List => {
                                "std.collection flat_map transform must return a finite list"
                            }
                            Container::Relation => {
                                "relation flat_map transform must return a finite collection"
                            }
                        },
                    ));
                    invalid = true;
                }
            }
        }
        values[index] = Some(callback);
    }

    // A missing relation callback has a dedicated shape diagnostic, matching
    // the hand-written relation path.
    let missing_callback = params.iter().enumerate().find_map(|(slot, param)| {
        (!param.has_default && matches!(param.ty, SigType::Fn(_, _)) && slots[slot].is_none())
            .then_some(param.name)
    });
    if let Some(name) = missing_callback
        && container == Container::Relation
        && slots[0].is_some()
    {
        diagnostics.push(diag(
            DIAG_TYPE,
            format!("relation {} requires rows and {name}", intrinsic.name),
        ));
        malformed = true;
    } else if malformed {
        diagnostics.push(diag(
            DIAG_TYPE,
            if stats {
                format!(
                    "std.stats.{} relation arguments do not match its static signature",
                    intrinsic.name
                )
            } else {
                match container {
                    Container::Relation => format!(
                        "relation {} arguments do not match its static signature",
                        intrinsic.name
                    ),
                    Container::List => format!(
                        "{} {} arguments do not match its static signature",
                        list_message_prefix(id),
                        intrinsic.name
                    ),
                }
            },
        ));
    }

    if !stats {
        check_intrinsic_constraints(
            id,
            intrinsic.name,
            container,
            pipeline,
            &subst,
            &slots,
            &values,
            arguments,
            &mut invalid,
            diagnostics,
        );
    }

    let ty = if stats {
        infer_statistics_result(
            id,
            &slots,
            &values,
            input.as_ref(),
            scope,
            &mut invalid,
            diagnostics,
        )
    } else {
        signature_type(signature.result, &subst, container, true)
    };
    match id {
        IntrinsicId::CoreOne | IntrinsicId::CollectionZipExact => effects.may_fail = true,
        _ if stats => effects.may_fail = true,
        _ => {}
    }
    Inferred {
        ty: if malformed || invalid {
            Type::Error
        } else {
            ty
        },
        effects,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_value_parameter(
    id: IntrinsicId,
    name: &str,
    slot: usize,
    param: Param,
    subst: &[Option<Type>],
    actual: &Type,
    expression: &Expr,
    container: Container,
    pipeline: bool,
    invalid: &mut bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let prefix = relation_prefix(container);
    match id {
        IntrinsicId::CoreTake | IntrinsicId::CoreDrop if slot == 1 => {
            let integer_range = pipeline
                && matches!(actual, Type::Range(element) if element.as_ref() == &Type::Int);
            if *actual != Type::Int && !integer_range {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    match (container, pipeline) {
                        (Container::Relation, _) => {
                            format!("relation {name} count must be an Int")
                        }
                        (Container::List, true) => format!("{name} requires an integer count"),
                        (Container::List, false) => format!("{name} count must be an Int"),
                    },
                ));
                *invalid = true;
            } else if *actual == Type::Int && is_negative_integer_constant(expression) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("{prefix}{name} count must be nonnegative"),
                ));
                *invalid = true;
            }
        }
        IntrinsicId::CollectionWindow if matches!(slot, 1 | 2) => {
            if matches!(actual, Type::Error) {
                return;
            }
            let label = if slot == 1 { "size" } else { "step" };
            if *actual != Type::Int {
                diagnostics.push(diag(DIAG_TYPE, format!("window {label} must be an Int")));
                *invalid = true;
            } else if is_non_positive_integer_constant(expression) {
                diagnostics.push(diag(DIAG_TYPE, format!("window {label} must be positive")));
                *invalid = true;
            }
        }
        IntrinsicId::CollectionChunk if slot == 1 => {
            require_same(&Type::Int, actual, diagnostics);
            if is_non_positive_integer_constant(expression) {
                diagnostics.push(diag(DIAG_TYPE, "chunk size must be positive"));
                *invalid = true;
            }
        }
        _ => {
            if matches!(param.ty, SigType::Param(_))
                || matches!(param.ty, SigType::List(_) | SigType::Relation(_))
            {
                return;
            }
            let expected = signature_type(param.ty, subst, container, true);
            if !matches!(expected, Type::Error) {
                require_same(&expected, actual, diagnostics);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_intrinsic_constraints(
    id: IntrinsicId,
    name: &str,
    container: Container,
    pipeline: bool,
    subst: &[Option<Type>],
    slots: &[Option<usize>],
    values: &[Option<Inferred>],
    arguments: &[Argument],
    invalid: &mut bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let element = subst.first().and_then(Clone::clone).unwrap_or(Type::Error);
    let argument_type = |slot: usize| {
        slots
            .get(slot)
            .and_then(|index| *index)
            .and_then(|index| values.get(index))
            .and_then(Option::as_ref)
            .map(|value| &value.ty)
    };
    match id {
        IntrinsicId::CoreDistinct if is_default_float_equality_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "distinct requires a lawful equality key; default Float equality is unavailable",
            ));
            *invalid = true;
        }
        IntrinsicId::CollectionUnique if is_default_float_equality_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                "unique requires lawful equality; default Float equality is unavailable",
            ));
            *invalid = true;
        }
        IntrinsicId::CoreSum if !is_sum_element_type(&element) => {
            if is_absolute_affine(&element) {
                diagnostics.push(diag(
                    DIAG_ABSOLUTE_QUANTITY_SUM,
                    format!("cannot {name} absolute affine quantities"),
                ));
            } else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    match container {
                        Container::List => format!(
                            "finite-list {name} requires a finite list with a supported {name} element type"
                        ),
                        Container::Relation => {
                            format!("relation {name} requires a supported element type")
                        }
                    },
                ));
            }
            *invalid = true;
        }
        IntrinsicId::CoreMin | IntrinsicId::CoreMax if !is_sort_key_type(&element) => {
            diagnostics.push(diag(
                DIAG_TYPE,
                match container {
                    Container::List => format!(
                        "finite-list {name} requires a finite list with a supported {name} element type"
                    ),
                    Container::Relation => {
                        format!("relation {name} requires a supported element type")
                    }
                },
            ));
            *invalid = true;
        }
        IntrinsicId::CoreSortBy => {
            let key = subst.get(1).and_then(Clone::clone).unwrap_or(Type::Error);
            if matches!(key, Type::Error) || is_sort_key_type(&key) {
                return;
            }
            if container == Container::List {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "finite-list sort_by key must have a total order",
                ));
            }
            *invalid = true;
        }
        IntrinsicId::CollectionGroupBy | IntrinsicId::CollectionRank => {
            let key = subst.get(1).and_then(Clone::clone).unwrap_or(Type::Error);
            if !matches!(key, Type::Error) && !is_evaluator_ordered_comparison_type(&key) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("std.collection {name} key must have a lawful total order"),
                ));
                *invalid = true;
            }
        }
        IntrinsicId::CollectionAsofJoin => {
            if let Some(right) = argument_type(1) {
                if Container::of(right).is_some_and(|kind| kind != container) {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "std.collection asof_join inputs must use the same collection kind",
                    ));
                    *invalid = true;
                } else if !matches!(right, Type::Error) {
                    let right_element = match right {
                        Type::List(element) | Type::Relation(element) => element.as_ref().clone(),
                        _ => Type::Error,
                    };
                    require_same(&element, &right_element, diagnostics);
                }
            }
            let time = subst.get(1).and_then(Clone::clone).unwrap_or(Type::Error);
            if !matches!(time, Type::Error) && !is_evaluator_ordered_comparison_type(&time) {
                diagnostics.push(diag(DIAG_TYPE, "asof_join time key must be ordered"));
                *invalid = true;
            }
        }
        IntrinsicId::CollectionZip | IntrinsicId::CollectionZipExact => {
            if let Some(right) = argument_type(1)
                && Container::of(right).is_some_and(|kind| kind != container)
            {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    format!("std.collection {name} inputs must use the same collection kind"),
                ));
                *invalid = true;
            }
        }
        IntrinsicId::CoreUnion => {
            if let Some(right) = argument_type(1) {
                if !matches!(right, Type::Error)
                    && Container::of(right).is_none_or(|kind| kind != container)
                {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        match container {
                            Container::List => {
                                "union requires a list with a compatible element type"
                            }
                            Container::Relation => {
                                "relation union requires a relation with a compatible element type"
                            }
                        },
                    ));
                    *invalid = true;
                } else if let Type::List(right_element) | Type::Relation(right_element) = right
                    && !matches!(right_element.as_ref(), Type::Error)
                    && !types_match(&element, right_element)
                {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        match (container, pipeline) {
                            (Container::Relation, _) => {
                                "relation union requires relations with compatible element types"
                            }
                            (Container::List, true) => {
                                "union requires lists with compatible element types"
                            }
                            (Container::List, false) => {
                                "finite-list union requires lists with compatible element types"
                            }
                        },
                    ));
                    *invalid = true;
                }
            }
        }
        IntrinsicId::CollectionBucketBy if container == Container::Relation => {
            check_bucket_by(&element, slots, values, arguments, invalid, diagnostics);
        }
        IntrinsicId::CollectionBucketBy => {
            // The finite-list overload only constrains the row shape; the
            // zone/period contract belongs to the relation overload, which is
            // where the pinned source documents calendar bucketing.
            let instant = element == Type::Instant
                || matches!(&element, Type::Record(fields) if fields.get("time") == Some(&Type::Instant));
            if !matches!(element, Type::Error) && !instant {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "bucket_by requires Instant values or rows with an Instant time field",
                ));
                *invalid = true;
            }
        }
        _ => {}
    }
}

fn check_bucket_by(
    element: &Type,
    slots: &[Option<usize>],
    values: &[Option<Inferred>],
    arguments: &[Argument],
    invalid: &mut bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let instant = element == &Type::Instant
        || matches!(element, Type::Record(fields) if fields.get("time") == Some(&Type::Instant));
    let period = slots
        .get(1)
        .and_then(|slot| *slot)
        .and_then(|index| values.get(index))
        .and_then(Option::as_ref);
    let zone = arguments
        .iter()
        .position(|argument| argument.name.as_deref() == Some("zone"))
        .and_then(|index| values.get(index))
        .and_then(Option::as_ref);
    let positional_period = slots
        .get(1)
        .and_then(|slot| *slot)
        .is_some_and(|index| arguments[index].name.is_none());
    let calendar = period.is_some_and(|value| is_calendar_period(&value.ty));
    let elapsed = period.is_some_and(|value| value.ty == Type::Named("std.DURATION".into()));
    let named_zone = zone.is_some_and(|value| value.ty == Type::Named("std.TimeZone".into()));
    let message = if !instant {
        Some((
            DIAG_TYPE,
            "bucket_by requires Instant values or rows with an Instant time field",
        ))
    } else if !positional_period || !(calendar || elapsed) {
        Some((
            DIAG_TYPE,
            "bucket_by requires an elapsed Duration or calendar-day period",
        ))
    } else if calendar && zone.is_none() {
        Some((
            DIAG_CALENDAR_BUCKET_ZONE,
            "calendar bucketing of Instant requires a time zone",
        ))
    } else if !(named_zone || elapsed && zone.is_none()) {
        Some((DIAG_TYPE, "bucket_by requires a named TimeZone argument"))
    } else {
        None
    };
    if let Some((code, message)) = message {
        diagnostics.push(diag(code, message));
        *invalid = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn infer_statistics_result(
    id: IntrinsicId,
    slots: &[Option<usize>],
    values: &[Option<Inferred>],
    input: Option<&Inferred>,
    scope: &Scope,
    invalid: &mut bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    let parameters = orna_intrinsics::by_id(id).signature.params;
    let names = parameters
        .iter()
        .map(|parameter| parameter.name)
        .collect::<Vec<_>>();
    let mut bound: Vec<Option<Inferred>> = vec![None; parameters.len()];
    if let Some(input) = input {
        bound[0] = Some(input.clone());
    }
    for (slot, assigned) in slots.iter().enumerate() {
        let Some(index) = *assigned else {
            continue;
        };
        if index == usize::MAX {
            continue;
        }
        if let Some(value) = values.get(index).and_then(Clone::clone) {
            bound[slot] = Some(value);
        }
    }
    let element = bound
        .first()
        .and_then(Option::as_ref)
        .map(|value| match &value.ty {
            Type::List(element) | Type::Relation(element) => element.as_ref().clone(),
            _ => Type::Error,
        })
        .unwrap_or(Type::Error);
    statistics_outcome(
        StatisticsOutcome {
            operation: statistics_operation_name(id),
            parameters: &names,
            values: &bound,
            scope,
        },
        element,
        invalid,
        diagnostics,
    )
}

fn statistics_operation_name(id: IntrinsicId) -> &'static str {
    match id {
        IntrinsicId::StatsMean => "mean",
        IntrinsicId::StatsMedian => "median",
        IntrinsicId::StatsPercentile => "percentile",
        IntrinsicId::StatsSum => "sum",
        IntrinsicId::StatsMin => "min",
        IntrinsicId::StatsMax => "max",
        IntrinsicId::StatsRange => "range",
        IntrinsicId::StatsMode => "mode",
        IntrinsicId::StatsVariance => "variance",
        IntrinsicId::StatsStandardDeviation => "standard_deviation",
        IntrinsicId::StatsHistogram => "histogram",
        IntrinsicId::StatsRate => "rate",
        IntrinsicId::StatsDerivative => "derivative",
        IntrinsicId::StatsIntegrate => "integrate",
        _ => "statistic",
    }
}
