//! Descriptor-driven system call inference.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use orna_syntax_ast::{Expr, Pattern, TypeExpr};
use orna_value::Diagnostic;

use super::*;

/// Resolves only the JSON-described, read-only descriptor surface. The
/// descriptor is metadata, not a runtime-value factory: a schema-known member
/// that has no implementation route fails explicitly instead of acquiring a
/// guessed type or effect, and a resolved member without a linked production
/// provider is rejected with `sys.version.incompatible`.
pub(crate) fn infer_descriptor_system_path(
    path: &[&str],
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> DescriptorPathInference {
    use system_api::{PathResolution, SystemMember};

    let api = system_api::embedded_system_api();
    let name = path.join(".");
    if let Some(relation) = api.historical_relation(path) {
        let view = name.strip_suffix(".as_of").unwrap_or(&name);
        require_linked_system_member(SystemMember::HistoricalView(view), scope);
        return DescriptorPathInference::Resolved(Inferred {
            ty: function(
                vec![Type::Named("sys.SnapshotRef".into())],
                Type::Relation(Box::new(Type::Named(relation.name.clone()))),
            ),
            effects: descriptor_effects(system_api::SystemEffect::Read),
        });
    }
    if path == ["sys", "database", "as_of"] {
        require_linked_system_member(SystemMember::HistoricalView("sys.database"), scope);
        return DescriptorPathInference::Resolved(Inferred {
            ty: function(
                vec![Type::Named("sys.SnapshotRef".into())],
                historical_database_type(Type::Named("sys.SnapshotRef".into())),
            ),
            effects: descriptor_effects(system_api::SystemEffect::Read),
        });
    }
    if let Some(singleton) = api.historical_singleton(path) {
        require_linked_system_member(SystemMember::HistoricalView(&singleton.name), scope);
        return DescriptorPathInference::Resolved(Inferred {
            ty: function(
                vec![Type::Named("sys.SnapshotRef".into())],
                descriptor_type(&singleton.ty),
            ),
            effects: descriptor_effects(system_api::SystemEffect::Read),
        });
    }
    if let Some(function) = api.function(&name).and_then(|functions| {
        functions
            .iter()
            .find(|function| descriptor_function_is_staticly_supported(function))
    }) && let Some(ty) = descriptor_function_type(function)
    {
        require_linked_system_member(SystemMember::Callable(&function.label), scope);
        return DescriptorPathInference::Resolved(Inferred {
            ty,
            effects: descriptor_effects(function.effect),
        });
    }
    if let Some(relation) = api.relation(&name) {
        require_linked_system_member(SystemMember::Relation(&name), scope);
        return DescriptorPathInference::Resolved(Inferred {
            ty: Type::Relation(Box::new(Type::Named(relation.name.clone()))),
            effects: descriptor_effects(system_api::SystemEffect::Read),
        });
    }
    if let Some(enum_name) = api.enum_value(path) {
        return DescriptorPathInference::Resolved(Inferred::pure(Type::Named(enum_name)));
    }
    match api.resolve(path) {
        PathResolution::ReadOnly(ty) => {
            if let Some(singleton) = api.singleton_view(path) {
                require_linked_system_member(SystemMember::Singleton(singleton), scope);
            }
            DescriptorPathInference::Resolved(Inferred::pure(descriptor_type(ty)))
        }
        PathResolution::Removed(removed) => {
            diagnostics.push(diag(
                if removed.diagnostic == DIAG_LEGACY_SYS_RUNTIME {
                    DIAG_LEGACY_SYS_RUNTIME
                } else {
                    DIAG_UNSUPPORTED
                },
                if removed.diagnostic == DIAG_LEGACY_SYS_RUNTIME {
                    "`sys.runtime` was renamed to `sys.rt`"
                } else {
                    "removed portable system name; use the documented replacement"
                },
            ));
            DescriptorPathInference::Rejected
        }
        PathResolution::KnownUnsupported | PathResolution::Unknown => {
            diagnostics.push(diag(
                DIAG_UNSUPPORTED,
                "portable system member is described but not implemented by this semantic slice",
            ));
            DescriptorPathInference::Rejected
        }
    }
}

/// Admission requires a linked production provider for every portable member
/// a program uses. The member keeps its descriptor type and inference carries
/// on unchanged; the record only feeds `report_unlinked_system_members`.
pub(crate) fn require_linked_system_member(member: system_api::SystemMember<'_>, scope: &Scope) {
    if let Some(record) = &scope.unlinked_system_members
        && !system_api::embedded_system_api().provider_linked(member)
    {
        record.borrow_mut().insert(format!(
            "{member} has no provider linked in this implementation"
        ));
    }
}

/// Reports each member in an admission record that has no linked production
/// provider once, as `sys.version.incompatible`. The program is rejected
/// before evaluation can perform writes (ORNA-SYS-008) instead of failing
/// mid-evaluation.
pub(crate) fn report_unlinked_system_members(
    record: &RefCell<BTreeSet<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    diagnostics.extend(
        record
            .take()
            .into_iter()
            .map(|message| diag(DIAG_SYS_VERSION_INCOMPATIBLE, message)),
    );
}

pub(crate) fn descriptor_function_is_staticly_supported(
    function: &system_api::FunctionDescriptor,
) -> bool {
    // Semantic admission can type every concrete portable signature and its
    // declared effect. Generic functions remain explicitly unsupported unless
    // a dedicated descriptor-driven admission path handles their type args.
    // Typing is not execution: each selected overload must still pass
    // `require_linked_system_member`.
    function.type_parameters.is_empty()
}

pub(crate) fn descriptor_function_type(function: &system_api::FunctionDescriptor) -> Option<Type> {
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| descriptor_call_type(&parameter.ty, &function.type_parameters))
        .collect::<Option<Vec<_>>>()?;
    let names = function
        .parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    let default_parameters = function
        .parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| parameter.has_default.then_some(index))
        .collect();
    Some(Type::Function {
        parameters,
        parameter_names: Some(names),
        default_parameters,
        result: Box::new(descriptor_call_type(
            &function.result,
            &function.type_parameters,
        )?),
    })
}

/// Calls to the portable surface select one JSON descriptor overload before
/// argument validation. This prevents a broad handwritten callable from
/// accepting a mixture of arguments that no portable overload permits.
pub(crate) fn infer_descriptor_system_call(
    path: &[&str],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let functions = system_api::embedded_system_api().function(&path.join("."))?;
    if path == ["sys", "explain"]
        && let Some(explained) = infer_query_explain_system_call(
            functions,
            arguments,
            type_arguments,
            None,
            scope,
            local,
            diagnostics,
        )
    {
        return Some(explained);
    }
    if path == ["sys", "meta"] {
        return Some(infer_meta_system_call(
            functions,
            arguments,
            type_arguments,
            None,
            scope,
            local,
            diagnostics,
        ));
    }
    if path == ["sys", "await"] {
        return Some(infer_await_system_call(
            functions,
            arguments,
            type_arguments,
            None,
            scope,
            local,
            diagnostics,
        ));
    }
    if path == ["sys", "cancel"] {
        return Some(infer_cancel_system_call(
            functions,
            arguments,
            type_arguments,
            None,
            scope,
            local,
            diagnostics,
        ));
    }
    if path == ["sys", "invoke"]
        && (type_arguments.is_some()
            || arguments
                .iter()
                .any(|argument| argument.name.as_deref() == Some("as")))
    {
        return Some(infer_invoke_system_call(
            functions,
            arguments,
            type_arguments,
            None,
            scope,
            local,
            diagnostics,
        ));
    }
    if path == ["sys", "start"]
        && (type_arguments.is_some()
            || arguments
                .iter()
                .any(|argument| argument.name.as_deref() == Some("as")))
    {
        return Some(infer_start_system_call(
            functions,
            arguments,
            type_arguments,
            None,
            scope,
            local,
            diagnostics,
        ));
    }
    if let Some(type_arguments) = type_arguments {
        return Some(infer_unsupported_generic_system_call(
            functions,
            arguments,
            type_arguments,
            scope,
            local,
            diagnostics,
        ));
    }
    let supported = functions
        .iter()
        .filter(|function| descriptor_function_is_staticly_supported(function))
        .collect::<Vec<_>>();
    if supported.is_empty() {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable system function is described but not implemented by this semantic slice",
        ));
        return Some(Inferred::error());
    }
    let mut effects = EffectSummary::default();
    let values = arguments
        .iter()
        .map(|argument| {
            let value = if argument.name.as_deref() == Some("as") {
                infer_type_argument(&argument.value)
                    .unwrap_or_else(|| infer(&argument.value, scope, local, diagnostics))
            } else {
                infer(&argument.value, scope, local, diagnostics)
            };
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();
    let Some(function) = supported
        .iter()
        .copied()
        .find(|function| descriptor_arguments_match(function, arguments, &values))
    else {
        let code = if supported.is_empty() {
            DIAG_UNSUPPORTED
        } else {
            DIAG_TYPE
        };
        diagnostics.push(diag(
            code,
            if code == DIAG_UNSUPPORTED {
                "portable generic system function is described but not implemented by this semantic slice"
            } else {
                "arguments do not match a portable system function overload"
            },
        ));
        return Some(Inferred::error_with(effects));
    };
    require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
    effects.join(&descriptor_effects(function.effect));
    let result = descriptor_call_type(&function.result, &function.type_parameters)
        .expect("supported descriptor functions have concrete types");
    let result = if path == ["sys", "snapshot"] {
        arguments
            .first()
            .map(|argument| snapshot_selector_context(&argument.value, values.first(), local))
            .unwrap_or(result)
    } else {
        result
    };
    Some(Inferred {
        ty: result,
        effects,
    })
}

/// `sys.invoke<T>` is admitted only through its explicit `as: T` witness. The
/// function reference remains opaque at this layer; the descriptor shape,
/// named/default argument rules, result substitution and invoke effect are
/// nevertheless checked before any runtime activation can occur.
pub(crate) fn infer_invoke_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    input: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let input_offset = usize::from(input.is_some());
    let mut witness_index = None;
    let mut values = input.into_iter().cloned().collect::<Vec<_>>();
    values.extend(arguments.iter().enumerate().map(|(index, argument)| {
        if argument.name.as_deref() == Some("as") {
            if witness_index.replace(input_offset + index).is_some() {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.invoke requires exactly one explicit as: T witness",
                ));
            }
            let Some(ty) = start_type_witness(&argument.value, scope) else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.invoke as: witness must name a known static type",
                ));
                return Type::Error;
            };
            return ty;
        }
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        value.ty
    }));

    let Some(function) = functions
        .iter()
        .find(|function| invoke_typed_descriptor(function).is_some())
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function is described but not implemented by this semantic slice",
        ));
        return Inferred::error_with(effects);
    };
    require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
    let parameter = invoke_typed_descriptor(function)
        .expect("the selected descriptor is a typed sys.invoke overload");

    let Some(witness_index) = witness_index else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "typed sys.invoke requires an explicit as: T witness",
        ));
        return Inferred::error_with(effects);
    };
    if values.iter().any(|value| matches!(value, Type::Error)) {
        return Inferred::error_with(effects);
    }
    let witness = values[witness_index].clone();

    if let Some(type_arguments) = type_arguments {
        if type_arguments.len() != 1 {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.invoke requires exactly one explicit type argument when generic arguments are supplied",
            ));
            return Inferred::error_with(effects);
        }
        let Some(type_argument) = valid_static_type(&type_arguments[0], scope) else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.invoke explicit type argument must name a known static type",
            ));
            return Inferred::error_with(effects);
        };
        if type_argument != witness {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.invoke explicit type argument must match the as: witness",
            ));
            return Inferred::error_with(effects);
        }
    }

    let parameters = function
        .parameters
        .iter()
        .map(|descriptor| substitute_descriptor_type(&descriptor.ty, parameter, &witness))
        .collect::<Option<Vec<_>>>();
    let Some(parameters) = parameters else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function has an unsupported type shape",
        ));
        return Inferred::error_with(effects);
    };
    let names = function
        .parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let defaults = function
        .parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| parameter.has_default.then_some(index))
        .collect::<BTreeSet<_>>();
    if arguments.iter().enumerate().any(|(index, argument)| {
        argument.name.as_deref() == Some("as") && input_offset + index != witness_index
    }) || !descriptor_arguments_match_types(
        &parameters,
        &names,
        &defaults,
        arguments,
        &values[input_offset..],
        input,
    ) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "arguments do not match the portable sys.invoke<T> signature",
        ));
        return Inferred::error_with(effects);
    }

    effects.join(&descriptor_effects(function.effect));
    Inferred {
        ty: substitute_descriptor_type(&function.result, parameter, &witness)
            .unwrap_or(Type::Error),
        effects,
    }
}

pub(crate) fn invoke_typed_descriptor(function: &system_api::FunctionDescriptor) -> Option<&str> {
    if function.name != "sys.invoke"
        || function.effect != system_api::SystemEffect::Invoke
        || function.type_parameters.len() != 1
        || function.parameters.len() != 6
    {
        return None;
    }
    let parameter = function.type_parameters.iter().next()?;
    let [target, arguments, witness, at, transaction, idempotency_key] =
        function.parameters.as_slice()
    else {
        return None;
    };
    let named = |value: &system_api::SystemType, expected: &str| matches!(value, system_api::SystemType::Named(name) if name == expected);
    if target.name != "function"
        || !named(&target.ty, "sys.FunctionRef")
        || target.has_default
        || arguments.name != "arguments"
        || !named(&arguments.ty, "sys.ArgumentMap")
        || arguments.has_default
        || witness.name != "as"
        || witness.ty != system_api::SystemType::Named(parameter.to_owned())
        || witness.has_default
        || at.name != "at"
        || at.ty
            != system_api::SystemType::Optional(Box::new(system_api::SystemType::Named(
                "sys.SnapshotRef".into(),
            )))
        || !at.has_default
        || transaction.name != "transaction"
        || !named(&transaction.ty, "sys.InvokeTransaction")
        || !transaction.has_default
        || idempotency_key.name != "idempotency_key"
        || idempotency_key.ty
            != system_api::SystemType::Optional(Box::new(system_api::SystemType::Named(
                "Str".into(),
            )))
        || !idempotency_key.has_default
        || function.result != system_api::SystemType::Named(parameter.to_owned())
    {
        return None;
    }
    Some(parameter)
}

/// `sys.start<T>` is admitted only through its explicit `as: T` witness. The
/// opaque `sys.FunctionRef` target is resolved later by the invocation
/// boundary, so this semantic slice validates the closed descriptor shape and
/// carries the witness into the returned handle type.
pub(crate) fn infer_start_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    input: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let input_offset = usize::from(input.is_some());
    let mut witness_index = None;
    let mut values = input.into_iter().cloned().collect::<Vec<_>>();
    values.extend(arguments.iter().enumerate().map(|(index, argument)| {
        if argument.name.as_deref() == Some("as") {
            if witness_index.replace(input_offset + index).is_some() {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.start requires exactly one explicit as: T witness",
                ));
            }
            let Some(ty) = start_type_witness(&argument.value, scope) else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.start as: witness must name a known static type",
                ));
                return Type::Error;
            };
            return ty;
        }
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        value.ty
    }));

    let Some(function) = functions
        .iter()
        .find(|function| start_typed_descriptor(function).is_some())
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function is described but not implemented by this semantic slice",
        ));
        return Inferred::error_with(effects);
    };
    require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
    let parameter = start_typed_descriptor(function)
        .expect("the selected descriptor is a typed sys.start overload");

    let Some(witness_index) = witness_index else {
        diagnostics.push(diag(
            DIAG_TYPE,
            "typed sys.start requires an explicit as: T witness",
        ));
        return Inferred::error_with(effects);
    };
    if values.iter().any(|value| matches!(value, Type::Error)) {
        return Inferred::error_with(effects);
    }
    let witness = values[witness_index].clone();

    if let Some(type_arguments) = type_arguments {
        if type_arguments.len() != 1 {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.start requires exactly one explicit type argument when generic arguments are supplied",
            ));
            return Inferred::error_with(effects);
        }
        let Some(type_argument) = valid_static_type(&type_arguments[0], scope) else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.start explicit type argument must name a known static type",
            ));
            return Inferred::error_with(effects);
        };
        if type_argument != witness {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.start explicit type argument must match the as: witness",
            ));
            return Inferred::error_with(effects);
        }
    }

    let parameters = function
        .parameters
        .iter()
        .map(|descriptor| substitute_descriptor_type(&descriptor.ty, parameter, &witness))
        .collect::<Option<Vec<_>>>();
    let Some(parameters) = parameters else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function has an unsupported type shape",
        ));
        return Inferred::error_with(effects);
    };
    let names = function
        .parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let defaults = function
        .parameters
        .iter()
        .enumerate()
        .filter_map(|(index, parameter)| parameter.has_default.then_some(index))
        .collect::<BTreeSet<_>>();
    if arguments.iter().enumerate().any(|(index, argument)| {
        argument.name.as_deref() == Some("as") && input_offset + index != witness_index
    }) || !descriptor_arguments_match_types(
        &parameters,
        &names,
        &defaults,
        arguments,
        &values[input_offset..],
        input,
    ) {
        diagnostics.push(diag(
            DIAG_TYPE,
            "arguments do not match the portable sys.start<T> signature",
        ));
        return Inferred::error_with(effects);
    }

    effects.join(&descriptor_effects(function.effect));
    Inferred {
        ty: substitute_descriptor_type(&function.result, parameter, &witness)
            .unwrap_or(Type::Error),
        effects,
    }
}

pub(crate) fn start_type_witness(expression: &Expr, scope: &Scope) -> Option<Type> {
    let path = qualified_path(expression)?;
    let name = path.join(".");
    let ty = primitive(&name).unwrap_or(Type::Named(name));
    let ty = resolve_type_aliases(&ty, &scope.type_aliases, &mut BTreeSet::new());
    let ty = canonicalize_type(&ty, scope);
    static_type_is_known(&ty, scope).then_some(ty)
}

pub(crate) fn start_typed_descriptor(function: &system_api::FunctionDescriptor) -> Option<&str> {
    if function.name != "sys.start"
        || function.effect != system_api::SystemEffect::Invoke
        || function.type_parameters.len() != 1
        || function.parameters.len() != 6
    {
        return None;
    }
    let parameter = function.type_parameters.iter().next()?;
    let [target, arguments, witness, at, transaction, idempotency_key] =
        function.parameters.as_slice()
    else {
        return None;
    };
    let named = |value: &system_api::SystemType, expected: &str| matches!(value, system_api::SystemType::Named(name) if name == expected);
    if target.name != "function"
        || !named(&target.ty, "sys.FunctionRef")
        || target.has_default
        || arguments.name != "arguments"
        || !named(&arguments.ty, "sys.ArgumentMap")
        || arguments.has_default
        || witness.name != "as"
        || witness.ty != system_api::SystemType::Named(parameter.to_owned())
        || witness.has_default
        || at.name != "at"
        || at.ty
            != system_api::SystemType::Optional(Box::new(system_api::SystemType::Named(
                "sys.SnapshotRef".into(),
            )))
        || !at.has_default
        || transaction.name != "transaction"
        || !named(&transaction.ty, "sys.InvokeTransaction")
        || !transaction.has_default
        || idempotency_key.name != "idempotency_key"
        || idempotency_key.ty
            != system_api::SystemType::Optional(Box::new(system_api::SystemType::Named(
                "Str".into(),
            )))
        || !idempotency_key.has_default
    {
        return None;
    }
    let system_api::SystemType::Applied { base, arguments } = &function.result else {
        return None;
    };
    if base != "sys.InvocationHandle"
        || arguments.as_slice() != [system_api::SystemType::Named(parameter.to_owned())]
    {
        return None;
    }
    Some(parameter)
}

pub(crate) fn descriptor_arguments_match_types(
    parameters: &[Type],
    parameter_names: &[String],
    default_parameters: &BTreeSet<usize>,
    arguments: &[orna_syntax_ast::Argument],
    values: &[Type],
    input: Option<&Type>,
) -> bool {
    if parameters.len() != parameter_names.len() || arguments.len() != values.len() {
        return false;
    }
    let mut seen = BTreeSet::new();
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
            None
        } else {
            let index = (positional < parameters.len()).then_some(positional);
            positional += 1;
            index
        };
        let Some(index) = index else {
            return false;
        };
        if !seen.insert(index) || !types_match(&parameters[index], actual) {
            return false;
        }
    }
    (0..parameters.len()).all(|index| seen.contains(&index) || default_parameters.contains(&index))
}

/// Generic system operations outside the explicitly implemented subset still
/// have to validate the source expressions that feed them.  This keeps nested
/// diagnostics and effects visible while the operation itself remains closed
/// with an unsupported result.
pub(crate) fn infer_unsupported_generic_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: &[TypeExpr],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let values = arguments
        .iter()
        .map(|argument| {
            let value = if argument.name.as_deref() == Some("as") {
                infer_type_argument(&argument.value)
                    .unwrap_or_else(|| infer(&argument.value, scope, local, diagnostics))
            } else {
                infer(&argument.value, scope, local, diagnostics)
            };
            effects.join(&value.effects);
            value.ty
        })
        .collect::<Vec<_>>();

    if let Some(function) = functions
        .iter()
        .find(|function| !function.type_parameters.is_empty())
    {
        let substitutions = type_arguments
            .iter()
            .map(|type_argument| valid_static_type(type_argument, scope))
            .collect::<Option<Vec<_>>>()
            .filter(|arguments| arguments.len() == function.type_parameters.len())
            .map(|arguments| {
                function
                    .type_parameters
                    .iter()
                    .cloned()
                    .zip(arguments)
                    .collect::<BTreeMap<_, _>>()
            });
        if let Some(substitutions) = substitutions {
            let parameters = function
                .parameters
                .iter()
                .map(|parameter| substitute_generic_descriptor_type(&parameter.ty, &substitutions))
                .collect::<Option<Vec<_>>>();
            if let Some(parameters) = parameters {
                let names = function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect::<Vec<_>>();
                let defaults = function
                    .parameters
                    .iter()
                    .enumerate()
                    .filter_map(|(index, parameter)| parameter.has_default.then_some(index))
                    .collect();
                check_call_arguments(
                    &parameters,
                    Some(&names),
                    &defaults,
                    arguments,
                    &values,
                    None,
                    diagnostics,
                );
            }
        }
        effects.join(&descriptor_effects(function.effect));
    }
    diagnostics.push(diag(
        DIAG_UNSUPPORTED,
        "explicit generic arguments are not implemented for this portable system function",
    ));
    Inferred::error_with(effects)
}

pub(crate) fn substitute_generic_descriptor_type(
    ty: &system_api::SystemType,
    substitutions: &BTreeMap<String, Type>,
) -> Option<Type> {
    match ty {
        system_api::SystemType::Named(name) => substitutions
            .get(name)
            .cloned()
            .or_else(|| Some(descriptor_type(ty))),
        system_api::SystemType::Applied { base, arguments }
            if matches!(base.as_str(), "Relation" | "Query") =>
        {
            let element = arguments.first()?;
            Some(Type::Relation(Box::new(
                substitute_generic_descriptor_type(element, substitutions)?,
            )))
        }
        system_api::SystemType::Applied { base, arguments } => Some(Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_generic_descriptor_type(argument, substitutions))
                .collect::<Option<Vec<_>>>()?,
        }),
        system_api::SystemType::List(element) => Some(Type::List(Box::new(
            substitute_generic_descriptor_type(element, substitutions)?,
        ))),
        system_api::SystemType::Optional(element) => Some(Type::Optional(Box::new(
            substitute_generic_descriptor_type(element, substitutions)?,
        ))),
    }
}

/// `sys.explain(Query<T>)` is the generic planner overload. The v1 evaluator
/// currently represents a typed query handle as `Relation<T>`, so the
/// semantic layer specializes this descriptor without exposing a second
/// runtime type. The actual planner adapter still resolves the query against
/// the pinned catalogue snapshot.
pub(crate) fn infer_query_explain_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    input: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let query_descriptor = functions.iter().find(|function| {
        let [parameter] = function.parameters.as_slice() else {
            return false;
        };
        if function.type_parameters.len() != 1 {
            return false;
        }
        let generic = function
            .type_parameters
            .iter()
            .next()
            .expect("one type parameter was checked above");
        function.name == "sys.explain"
            && function.label == "sys.explain(Query)"
            && function.effect == system_api::SystemEffect::Read
            && parameter.name == "query"
            && !parameter.has_default
            && parameter.ty
                == system_api::SystemType::Applied {
                    base: "Query".into(),
                    arguments: vec![system_api::SystemType::Named(generic.clone())],
                }
            && function.result == system_api::SystemType::Named("sys.Plan".into())
    })?;

    let has_input = input.is_some();
    let mut effects = EffectSummary::default();
    let mut values = Vec::with_capacity(arguments.len() + usize::from(has_input));
    if let Some(input) = input {
        values.push(input.clone());
    }
    for argument in arguments {
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        values.push(value.ty);
    }

    let query_shape = if has_input {
        arguments.is_empty()
    } else {
        arguments.len() == 1
            && arguments[0]
                .name
                .as_deref()
                .is_none_or(|name| name == "query")
    };
    let generic_requested = type_arguments.is_some_and(|arguments| !arguments.is_empty());
    let query_named = arguments
        .iter()
        .any(|argument| argument.name.as_deref() == Some("query"));
    let query_value = values.first();
    let is_query = query_value.is_some_and(|ty| match ty {
        Type::Relation(_) => true,
        Type::Applied { base, .. } => base == "Query",
        _ => false,
    });
    let wants_query = query_shape && (query_named || is_query || generic_requested);

    if wants_query {
        require_linked_system_member(
            system_api::SystemMember::Callable(&query_descriptor.label),
            scope,
        );
        effects.join(&descriptor_effects(query_descriptor.effect));
        if query_value.is_some_and(|ty| matches!(ty, Type::Error)) {
            return Some(Inferred::error_with(effects));
        }
        let query_element = match query_value.expect("query shape contains a value") {
            Type::Relation(element) => element.as_ref(),
            Type::Applied { base, arguments } if base == "Query" => {
                let Some(element) = arguments.first() else {
                    diagnostics.push(diag(
                        DIAG_TYPE,
                        "sys.explain(Query) requires a typed query value",
                    ));
                    return Some(Inferred::error_with(effects));
                };
                element
            }
            _ => {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.explain(Query) requires a typed query value",
                ));
                return Some(Inferred::error_with(effects));
            }
        };

        if let Some(type_arguments) = type_arguments.filter(|arguments| !arguments.is_empty()) {
            let explicit = type_arguments
                .iter()
                .map(|argument| valid_static_type(argument, scope))
                .collect::<Option<Vec<_>>>();
            let Some([explicit]) = explicit.as_deref() else {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.explain requires exactly one valid explicit query element type",
                ));
                return Some(Inferred::error_with(effects));
            };
            if !types_match(explicit, query_element) {
                diagnostics.push(diag(
                    DIAG_TYPE,
                    "sys.explain explicit query element type must match the query value",
                ));
                return Some(Inferred::error_with(effects));
            }
        }

        let generic = query_descriptor
            .type_parameters
            .iter()
            .next()
            .expect("validated query descriptor has one type parameter");
        let substitutions = BTreeMap::from([(generic.clone(), query_element.clone())]);
        return Some(Inferred {
            ty: substitute_generic_descriptor_type(&query_descriptor.result, &substitutions)
                .unwrap_or(Type::Error),
            effects,
        });
    }

    if values.iter().any(|ty| matches!(ty, Type::Error)) {
        return Some(Inferred::error_with(effects));
    }

    if has_input {
        if arguments.is_empty()
            && let Some(function) = functions.iter().find(|function| {
                function.name == "sys.explain"
                    && function.label != "sys.explain(Query)"
                    && function.type_parameters.is_empty()
                    && function.parameters.len() == 1
                    && types_match(&descriptor_type(&function.parameters[0].ty), &values[0])
            })
        {
            require_linked_system_member(
                system_api::SystemMember::Callable(&function.label),
                scope,
            );
            effects.join(&descriptor_effects(function.effect));
            return Some(Inferred {
                ty: descriptor_type(&function.result),
                effects,
            });
        }
    } else if type_arguments.is_none()
        && let Some(function) = functions
            .iter()
            .filter(|function| {
                function.name == "sys.explain"
                    && descriptor_function_is_staticly_supported(function)
            })
            .find(|function| descriptor_arguments_match(function, arguments, &values))
    {
        require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
        effects.join(&descriptor_effects(function.effect));
        return Some(Inferred {
            ty: descriptor_type(&function.result),
            effects,
        });
    }

    diagnostics.push(diag(
        DIAG_TYPE,
        "arguments do not match a portable sys.explain overload",
    ));
    Some(Inferred::error_with(effects))
}

/// `sys.meta<T>` is the one generic system operation whose type parameter is
/// determined directly by its value argument.  Keep this substitution local
/// to the reflective metadata call; invocation generics additionally require
/// runtime result witnesses and therefore remain outside this semantic slice.
pub(crate) fn infer_meta_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    input: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let has_input = input.is_some();
    let mut values = input.into_iter().cloned().collect::<Vec<_>>();
    values.extend(arguments.iter().map(|argument| {
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        value.ty
    }));
    let valid_shape = if has_input {
        arguments.is_empty()
    } else {
        arguments.len() == 1
            && arguments[0]
                .name
                .as_deref()
                .is_none_or(|name| name == "value")
    };
    let Some(function) = functions.iter().find(|function| {
        function.type_parameters.len() == 1
            && function.parameters.len() == 1
            && function.parameters[0].name == "value"
            && function.parameters[0].ty
                == system_api::SystemType::Named(
                    function.type_parameters.iter().next().unwrap().clone(),
                )
    }) else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function is described but not implemented by this semantic slice",
        ));
        return Inferred::error_with(effects);
    };
    require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
    if !valid_shape {
        diagnostics.push(diag(
            DIAG_TYPE,
            "arguments do not match a portable system function overload",
        ));
        return Inferred::error_with(effects);
    }
    if values.iter().any(|value| matches!(value, Type::Error)) {
        return Inferred::error_with(effects);
    }
    effects.join(&descriptor_effects(function.effect));
    let parameter = function.type_parameters.iter().next().unwrap();
    let value_type = values.first().expect("valid sys.meta shape");
    if let Some(type_arguments) = type_arguments {
        let Some(type_argument) = type_arguments
            .iter()
            .map(|type_argument| valid_static_type(type_argument, scope))
            .collect::<Option<Vec<_>>>()
            .and_then(|arguments| (arguments.len() == 1).then(|| arguments[0].clone()))
        else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.meta requires exactly one explicit type argument when generic arguments are supplied",
            ));
            return Inferred::error_with(effects);
        };
        if type_argument != *value_type {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.meta explicit type argument must match the value type",
            ));
            return Inferred::error_with(effects);
        }
    }
    let result = substitute_descriptor_type(&function.result, parameter, value_type);
    Inferred {
        ty: result.unwrap_or(Type::Error),
        effects,
    }
}

pub(crate) fn infer_descriptor_system_call_with_input(
    path: &[&str],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: &[TypeExpr],
    input: &Type,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let functions = system_api::embedded_system_api().function(&path.join("."))?;
    match path {
        ["sys", "explain"] => infer_query_explain_system_call(
            functions,
            arguments,
            (!type_arguments.is_empty()).then_some(type_arguments),
            Some(input),
            scope,
            local,
            diagnostics,
        ),
        ["sys", "meta"] => Some(infer_meta_system_call(
            functions,
            arguments,
            Some(type_arguments),
            Some(input),
            scope,
            local,
            diagnostics,
        )),
        ["sys", "await"] => Some(infer_await_system_call(
            functions,
            arguments,
            Some(type_arguments),
            Some(input),
            scope,
            local,
            diagnostics,
        )),
        ["sys", "cancel"] => Some(infer_cancel_system_call(
            functions,
            arguments,
            Some(type_arguments),
            Some(input),
            scope,
            local,
            diagnostics,
        )),
        ["sys", "invoke"] => Some(infer_invoke_system_call(
            functions,
            arguments,
            Some(type_arguments),
            Some(input),
            scope,
            local,
            diagnostics,
        )),
        ["sys", "start"] => Some(infer_start_system_call(
            functions,
            arguments,
            Some(type_arguments),
            Some(input),
            scope,
            local,
            diagnostics,
        )),
        _ => None,
    }
}

pub(crate) fn cancel_descriptor(function: &system_api::FunctionDescriptor) -> bool {
    function.name == "sys.cancel"
        && function.effect == system_api::SystemEffect::Invoke
        && function.type_parameters == BTreeSet::from(["T".to_owned()])
        && function.parameters.len() == 2
        && function.parameters[0].name == "invocation"
        && function.parameters[0].ty
            == system_api::SystemType::Applied {
                base: "sys.InvocationHandle".into(),
                arguments: vec![system_api::SystemType::Named("T".into())],
            }
        && !function.parameters[0].has_default
        && function.parameters[1].name == "reason"
        && function.parameters[1].ty
            == system_api::SystemType::Optional(Box::new(system_api::SystemType::Named(
                "Str".into(),
            )))
        && function.parameters[1].has_default
        && function.result == system_api::SystemType::Named("Bool".into())
}

/// `sys.cancel<T>` infers `T` from its invocation handle and optionally checks
/// an explicit witness. The descriptor is matched exactly so a future JSON
/// overload cannot silently acquire this semantic admission path.
pub(crate) fn infer_cancel_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    input: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let has_input = input.is_some();
    let input_offset = usize::from(has_input);
    let mut values = input.into_iter().cloned().collect::<Vec<_>>();
    values.extend(arguments.iter().map(|argument| {
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        value.ty
    }));
    let Some(function) = functions
        .iter()
        .find(|function| cancel_descriptor(function))
    else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function is described but not implemented by this semantic slice",
        ));
        return Inferred::error_with(effects);
    };
    require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
    if values.iter().any(|value| matches!(value, Type::Error)) {
        return Inferred::error_with(effects);
    }

    let mut used = vec![false; function.parameters.len()];
    let mut next_positional = 0usize;
    let mut named_started = false;
    let mut invocation_type = None;
    let mut valid = true;
    let supplied = input.into_iter().map(|value| (None, value)).chain(
        arguments
            .iter()
            .zip(values.iter().skip(input_offset))
            .map(|(argument, value)| (argument.name.as_deref(), value)),
    );
    for (name, value) in supplied {
        let index = if let Some(name) = name {
            named_started = true;
            function
                .parameters
                .iter()
                .position(|parameter| parameter.name == name)
        } else if named_started {
            None
        } else {
            while used.get(next_positional) == Some(&true) {
                next_positional += 1;
            }
            let index = (next_positional < function.parameters.len()).then_some(next_positional);
            next_positional += 1;
            index
        };
        let Some(index) = index else {
            valid = false;
            continue;
        };
        if used[index] {
            valid = false;
            continue;
        }
        used[index] = true;
        match index {
            0 => match value {
                Type::Applied { base, arguments }
                    if base == "sys.InvocationHandle" && arguments.len() == 1 =>
                {
                    invocation_type = arguments.first().cloned();
                }
                _ => valid = false,
            },
            1 => {
                let expected = descriptor_type(&function.parameters[index].ty);
                if !types_match(&expected, value)
                    && !matches!(&expected, Type::Optional(inner) if inner.as_ref() == value)
                {
                    valid = false;
                }
            }
            _ => valid = false,
        }
    }
    if !used[0] || !valid {
        diagnostics.push(diag(
            DIAG_TYPE,
            "arguments do not match a portable system function overload",
        ));
        return Inferred::error_with(effects);
    }
    let result_type = invocation_type.expect("valid invocation handle supplies its type");
    if let Some(type_arguments) = type_arguments {
        let Some(type_argument) = type_arguments
            .iter()
            .map(|type_argument| valid_static_type(type_argument, scope))
            .collect::<Option<Vec<_>>>()
            .and_then(|arguments| (arguments.len() == 1).then(|| arguments[0].clone()))
        else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.cancel requires exactly one explicit type argument when generic arguments are supplied",
            ));
            return Inferred::error_with(effects);
        };
        if type_argument != result_type {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.cancel explicit type argument must match the invocation handle result type",
            ));
            return Inferred::error_with(effects);
        }
    }
    effects.join(&descriptor_effects(function.effect));
    Inferred {
        ty: substitute_descriptor_type(&function.result, "T", &result_type).unwrap_or(Type::Error),
        effects,
    }
}

/// `sys.await<T>` binds its result type from the typed invocation handle.
pub(crate) fn infer_await_system_call(
    functions: &[system_api::FunctionDescriptor],
    arguments: &[orna_syntax_ast::Argument],
    type_arguments: Option<&[TypeExpr]>,
    input: Option<&Type>,
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inferred {
    let mut effects = EffectSummary::default();
    let has_input = input.is_some();
    let mut values = input.into_iter().cloned().collect::<Vec<_>>();
    values.extend(arguments.iter().map(|argument| {
        let value = infer(&argument.value, scope, local, diagnostics);
        effects.join(&value.effects);
        value.ty
    }));
    let Some(function) = functions.iter().find(|function| {
        function.type_parameters == BTreeSet::from(["T".to_owned()])
            && function.parameters.len() == 2
            && function.parameters[0].name == "invocation"
            && function.parameters[0].ty
                == system_api::SystemType::Applied {
                    base: "sys.InvocationHandle".into(),
                    arguments: vec![system_api::SystemType::Named("T".into())],
                }
            && !function.parameters[0].has_default
            && function.parameters[1].name == "timeout"
            && function.parameters[1].ty
                == system_api::SystemType::Optional(Box::new(system_api::SystemType::Named(
                    "Duration".into(),
                )))
            && function.parameters[1].has_default
            && function.result
                == system_api::SystemType::Applied {
                    base: "sys.InvocationResult".into(),
                    arguments: vec![system_api::SystemType::Named("T".into())],
                }
            && function.effect == system_api::SystemEffect::Invoke
    }) else {
        diagnostics.push(diag(
            DIAG_UNSUPPORTED,
            "portable generic system function is described but not implemented by this semantic slice",
        ));
        return Inferred::error_with(effects);
    };
    require_linked_system_member(system_api::SystemMember::Callable(&function.label), scope);
    if values.iter().any(|value| matches!(value, Type::Error)) {
        return Inferred::error_with(effects);
    }

    let mut used = vec![false; function.parameters.len()];
    let mut next_positional = 0usize;
    let mut invocation_type = None;
    let mut valid = true;
    let mut named_started = false;
    let input_offset = usize::from(has_input);
    let supplied = input.into_iter().map(|value| (None, value)).chain(
        arguments
            .iter()
            .zip(values.iter().skip(input_offset))
            .map(|(argument, value)| (argument.name.as_deref(), value)),
    );
    for (name, value) in supplied {
        let index = if let Some(name) = name {
            named_started = true;
            function
                .parameters
                .iter()
                .position(|parameter| parameter.name == *name)
        } else if named_started {
            None
        } else {
            while used.get(next_positional) == Some(&true) {
                next_positional += 1;
            }
            let index = (next_positional < function.parameters.len()).then_some(next_positional);
            next_positional += 1;
            index
        };
        let Some(index) = index else {
            valid = false;
            continue;
        };
        if used[index] {
            valid = false;
            continue;
        }
        used[index] = true;
        match index {
            0 => match value {
                Type::Applied { base, arguments }
                    if base == "sys.InvocationHandle" && arguments.len() == 1 =>
                {
                    invocation_type = arguments.first().cloned();
                }
                _ => valid = false,
            },
            1 => {
                let expected = descriptor_type(&function.parameters[index].ty);
                if !await_timeout_type_matches(&expected, value) {
                    valid = false;
                }
            }
            _ => valid = false,
        }
    }
    if !used[0] || !valid {
        diagnostics.push(diag(
            DIAG_TYPE,
            "arguments do not match a portable system function overload",
        ));
        return Inferred::error_with(effects);
    }
    let result_type = invocation_type.expect("valid invocation handle supplies its type");
    if let Some(type_arguments) = type_arguments {
        let Some(type_argument) = type_arguments
            .iter()
            .map(|type_argument| valid_static_type(type_argument, scope))
            .collect::<Option<Vec<_>>>()
            .and_then(|arguments| (arguments.len() == 1).then(|| arguments[0].clone()))
        else {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.await requires exactly one explicit type argument when generic arguments are supplied",
            ));
            return Inferred::error_with(effects);
        };
        if type_argument != result_type {
            diagnostics.push(diag(
                DIAG_TYPE,
                "sys.await explicit type argument must match the invocation handle result type",
            ));
            return Inferred::error_with(effects);
        }
    }
    effects.join(&descriptor_effects(function.effect));
    Inferred {
        ty: substitute_descriptor_type(&function.result, "T", &result_type).unwrap_or(Type::Error),
        effects,
    }
}

pub(crate) fn await_timeout_type_matches(expected: &Type, actual: &Type) -> bool {
    types_match(expected, actual)
        || matches!(
            actual,
            Type::Applied { base, arguments }
                if matches!(base.as_str(), "Int" | "Decimal" | "Float")
                    && matches!(arguments.as_slice(), [Type::Named(unit)] if unit == "s")
        )
}

pub(crate) fn substitute_descriptor_type(
    ty: &system_api::SystemType,
    parameter: &str,
    value: &Type,
) -> Option<Type> {
    match ty {
        system_api::SystemType::Named(name) if name == parameter => Some(value.clone()),
        system_api::SystemType::Named(_) => Some(descriptor_type(ty)),
        system_api::SystemType::Applied { base, arguments } => Some(Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_descriptor_type(argument, parameter, value))
                .collect::<Option<Vec<_>>>()?,
        }),
        system_api::SystemType::List(element) => Some(Type::List(Box::new(
            substitute_descriptor_type(element, parameter, value)?,
        ))),
        system_api::SystemType::Optional(element) => Some(Type::Optional(Box::new(
            substitute_descriptor_type(element, parameter, value)?,
        ))),
    }
}

pub(crate) fn descriptor_arguments_match(
    function: &system_api::FunctionDescriptor,
    arguments: &[orna_syntax_ast::Argument],
    values: &[Type],
) -> bool {
    let mut used = vec![false; function.parameters.len()];
    let mut next_positional = 0usize;
    let mut named_started = false;
    for (argument, value) in arguments.iter().zip(values) {
        let index = if let Some(name) = &argument.name {
            named_started = true;
            function
                .parameters
                .iter()
                .position(|parameter| parameter.name == *name)
        } else if named_started {
            None
        } else {
            while used.get(next_positional) == Some(&true) {
                next_positional += 1;
            }
            let index = (next_positional < function.parameters.len()).then_some(next_positional);
            next_positional += 1;
            index
        };
        let Some(index) = index else {
            return false;
        };
        if used[index]
            || !descriptor_call_type(&function.parameters[index].ty, &function.type_parameters)
                .is_some_and(|expected| types_match(&expected, value))
        {
            return false;
        }
        used[index] = true;
    }
    function
        .parameters
        .iter()
        .enumerate()
        .all(|(index, parameter)| used[index] || parameter.has_default)
}

pub(crate) fn descriptor_call_type(
    ty: &system_api::SystemType,
    type_parameters: &BTreeSet<String>,
) -> Option<Type> {
    match ty {
        system_api::SystemType::Named(name) if type_parameters.contains(name) => None,
        system_api::SystemType::Named(_) => Some(descriptor_type(ty)),
        system_api::SystemType::Applied { base, arguments } if base == "Relation" => {
            // Source annotations normalize Relation<T> to this semantic type.
            let [element] = arguments.as_slice() else {
                return None;
            };
            Some(Type::Relation(Box::new(descriptor_call_type(
                element,
                type_parameters,
            )?)))
        }
        system_api::SystemType::Applied { base, arguments } => Some(Type::Applied {
            base: base.clone(),
            arguments: arguments
                .iter()
                .map(|argument| descriptor_call_type(argument, type_parameters))
                .collect::<Option<Vec<_>>>()?,
        }),
        system_api::SystemType::List(element) => Some(Type::List(Box::new(descriptor_call_type(
            element,
            type_parameters,
        )?))),
        system_api::SystemType::Optional(element) => Some(Type::Optional(Box::new(
            descriptor_call_type(element, type_parameters)?,
        ))),
    }
}

pub(crate) fn descriptor_effects(effect: system_api::SystemEffect) -> EffectSummary {
    match effect {
        system_api::SystemEffect::Read => EffectSummary {
            effects: BTreeSet::from([EffectKind::TableRead]),
            opaque: false,
            may_fail: true,
        },
        system_api::SystemEffect::Invoke => EffectSummary {
            effects: BTreeSet::new(),
            opaque: true,
            may_fail: true,
        },
        system_api::SystemEffect::Admin => EffectSummary {
            effects: BTreeSet::new(),
            opaque: true,
            may_fail: true,
        },
    }
}

pub(crate) fn descriptor_type(ty: &system_api::SystemType) -> Type {
    match ty {
        system_api::SystemType::Named(name) => match name.as_str() {
            "Int" => Type::Int,
            "Decimal" => Type::Decimal,
            "Float" => Type::Float,
            "Date" => Type::Date,
            "Instant" => Type::Instant,
            "Str" => Type::Text,
            "Bool" => Type::Bool,
            "Null" => Type::Null,
            "Duration" => Type::Named("std.DURATION".into()),
            other => Type::Named(other.into()),
        },
        system_api::SystemType::Applied { base, arguments }
            if matches!(base.as_str(), "Relation" | "Query") =>
        {
            Type::Relation(Box::new(
                arguments
                    .first()
                    .map(descriptor_type)
                    .unwrap_or(Type::Error),
            ))
        }
        system_api::SystemType::Applied { base, arguments } if base == "Stream" => {
            Type::Stream(Box::new(
                arguments
                    .first()
                    .map(descriptor_type)
                    .unwrap_or(Type::Error),
            ))
        }
        system_api::SystemType::Applied { base, arguments } => Type::Applied {
            base: base.clone(),
            arguments: arguments.iter().map(descriptor_type).collect(),
        },
        system_api::SystemType::List(element) => Type::List(Box::new(descriptor_type(element))),
        system_api::SystemType::Optional(element) => {
            Type::Optional(Box::new(descriptor_type(element)))
        }
    }
}

pub(crate) fn infer_system_path(
    path: &[&str],
    scope: &Scope,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    if path.first() != Some(&"sys") {
        return None;
    }
    match infer_descriptor_system_path(path, scope, diagnostics) {
        DescriptorPathInference::Resolved(inferred) => Some(inferred),
        DescriptorPathInference::Rejected => Some(Inferred::error()),
    }
}

pub(crate) fn infer_nominal_conversion_call(
    callee: &Expr,
    arguments: &[orna_syntax_ast::Argument],
    scope: &Scope,
    local: &BTreeMap<String, Symbol>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Inferred> {
    let Expr::Field { base, name, .. } = callee else {
        return None;
    };
    if name != "from" {
        return None;
    }
    let target_path = qualified_path(base);
    if target_path
        .as_ref()
        .is_some_and(|path| path.first().is_some_and(|root| local.contains_key(*root)))
    {
        return None;
    }
    let symbol = if let Expr::Name { text, .. } = base.as_ref() {
        if local.contains_key(text) {
            return None;
        }
        scope
            .names
            .get(text)
            .filter(|symbol| symbol.kind == SymbolKind::Type)
    } else {
        target_path
            .as_deref()
            .and_then(|path| qualified_module_symbol(path, scope))
            .filter(|symbol| symbol.kind == SymbolKind::Type)
    }?;
    let Type::Named(target) = &symbol.ty else {
        return None;
    };
    let sources = scope.nominal_conversions.get(target)?;
    if arguments.len() != 1 {
        diagnostics.push(diag(
            DIAG_TYPE,
            "nominal conversion requires exactly one source value",
        ));
        return Some(Inferred::error());
    }
    let argument = infer(&arguments[0].value, scope, local, diagnostics);
    if sources.iter().any(|source| source == &argument.ty) {
        let mut effects = argument.effects;
        if let Some(conversions) = scope.nominal_conversion_effects.get(target) {
            for (source, body_effects) in conversions {
                if source == &argument.ty {
                    effects.join(body_effects);
                }
            }
        }
        effects.may_fail = true;
        return Some(Inferred {
            ty: Type::Named(target.clone()),
            effects,
        });
    }
    // A refined type's base conversion `T.from(base)` is not a nested From
    // implementation, so an explicit From for other sources must not remove it.
    if scope
        .refined_types
        .get(target)
        .is_some_and(|underlying| *underlying == argument.ty)
    {
        return Some(Inferred {
            ty: Type::Named(target.clone()),
            effects: argument.effects,
        });
    }
    diagnostics.push(diag(
        DIAG_TYPE,
        "no matching From implementation for the source type",
    ));
    Some(Inferred::error_with(argument.effects))
}

pub(crate) fn is_snapshot_selector_context(selector: &str) -> bool {
    selector
        .strip_prefix("selector:")
        .is_some_and(|identity| !identity.is_empty())
}

pub(crate) fn contextual_snapshot_ref(selector: &str) -> Type {
    Type::Applied {
        base: "sys.SnapshotRefContext".into(),
        arguments: vec![Type::Named(selector.to_owned())],
    }
}

pub(crate) fn is_contextual_snapshot_ref(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Applied { base, arguments }
            if base == "sys.SnapshotRefContext"
                && matches!(arguments.as_slice(), [Type::Named(selector)] if is_snapshot_selector_context(selector))
    )
}

pub(crate) fn is_snapshot_ref_value(ty: &Type) -> bool {
    ty == &Type::Named("sys.SnapshotRef".into()) || is_contextual_snapshot_ref(ty)
}

/// Give every SnapshotRef lambda parameter a lexical identity, including each
/// component of a tuple pattern. This preserves the direct-binder scope used by
/// current main and extends it to destructured pins, whose identity mechanics
/// are unspecified by the reference.
pub(crate) fn scope_snapshot_ref_parameter_pattern(pattern: &Pattern, ty: &Type) -> Type {
    match (pattern, ty) {
        (Pattern::Name(name, span), ty) if is_snapshot_ref_value(ty) => {
            let source = span.file().unwrap_or("<unknown>");
            contextual_snapshot_ref(&format!(
                "selector:binder:{source}@{}..{}:parameter:{name}",
                span.start, span.end
            ))
        }
        (Pattern::Tuple { elements, .. }, Type::Tuple(types)) if elements.len() == types.len() => {
            Type::Tuple(
                elements
                    .iter()
                    .zip(types)
                    .map(|(pattern, ty)| scope_snapshot_ref_parameter_pattern(pattern, ty))
                    .collect(),
            )
        }
        _ => ty.clone(),
    }
}

pub(crate) fn snapshot_ref_context_key(ty: &Type) -> Option<&str> {
    let Type::Applied { base, arguments } = ty else {
        return None;
    };
    if base != "sys.SnapshotRefContext" {
        return None;
    }
    match arguments.as_slice() {
        [Type::Named(selector)] => Some(selector),
        _ => None,
    }
}

pub(crate) fn snapshot_ref_binder_id(selector: &str) -> Option<&str> {
    selector
        .strip_prefix("selector:binder:")?
        .split_once(":parameter:")
        .map(|(binder, _)| binder)
}

pub(crate) fn snapshot_parameter_context(selector: &str) -> Option<(Option<&str>, &str, bool)> {
    let selector = selector.strip_prefix("selector:")?;
    let (captured, selector) = if let Some(selector) = selector.strip_prefix("capture:") {
        (true, selector)
    } else {
        (false, selector)
    };
    if let Some(parameter) = selector.strip_prefix("parameter:") {
        return Some((None, parameter, captured));
    }
    let selector = selector.strip_prefix("binder:")?;
    let (binder, parameter) = selector.split_once(":parameter:")?;
    Some((Some(binder), parameter, captured))
}

/// Gives a selected history root a type-level context key so decomposing its
/// namespace or callable path does not erase which selected snapshot it came
/// from. Literal selectors use their decoded value; dynamic selectors use
/// their file and source expression identity because this semantic slice does not
/// execute or constant-fold selector expressions. An already contextualized
/// `SnapshotRef` keeps that identity across `sys.snapshot` calls and closure
/// chains; a typed snapshot parameter receives a parameter-scoped identity,
/// then callers specialize it from their actual pinned argument.
pub(crate) fn snapshot_selector_context(
    expression: &Expr,
    argument_type: Option<&Type>,
    local: &BTreeMap<String, Symbol>,
) -> Type {
    if let Some(argument_type) = argument_type
        && is_contextual_snapshot_ref(argument_type)
    {
        return argument_type.clone();
    }
    let selector = match expression {
        Expr::Literal {
            text,
            kind: orna_syntax_ast::LiteralKind::String,
            ..
        } => serde_json::from_str::<String>(text)
            .unwrap_or_else(|_| text.trim_matches('"').to_owned()),
        Expr::Name { text, .. } if matches!(text.as_str(), "CWD" | "HEAD") => text.clone(),
        Expr::Name { text, .. }
            if local
                .get(text)
                .is_some_and(|symbol| symbol.kind == SymbolKind::Parameter) =>
        {
            format!("parameter:{text}")
        }
        Expr::Name { text, span } => format!(
            "dynamic:{}:{text}@{}..{}",
            span.file().unwrap_or("<unknown>"),
            span.start,
            span.end
        ),
        Expr::Group { inner, .. } => {
            return snapshot_selector_context(inner, argument_type, local);
        }
        _ => format!("dynamic:{expression:?}"),
    };
    contextual_snapshot_ref(&format!("selector:{selector}"))
}
