//! Sealed SERVER protocol shape, binding, and error policy.

use super::*;

pub(super) fn sealed_server_target_is_mutation(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
) -> bool {
    raw_server_insert_target_is_selected(active, function)
        || raw_server_reference_mutation_target(active, function).is_some()
}

pub(super) fn sealed_server_result_kind(
    return_type: &FunctionReturn,
) -> Option<ProtocolResourceKind> {
    match return_type {
        FunctionReturn::Single(_) => Some(ProtocolResourceKind::Single),
        FunctionReturn::Stream(_) | FunctionReturn::Rows(_) => Some(ProtocolResourceKind::Stream),
    }
}

pub(super) fn sealed_rows_preservation_is_supported(
    active: &ActiveDatabaseRevision,
    return_type: &FunctionReturn,
) -> bool {
    matches!(return_type, FunctionReturn::Rows(_))
        && active
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| {
                let revision = standard.revision();
                revision == STANDARD_LIBRARY_V8_REVISION_ID
                    || revision == STANDARD_LIBRARY_V9_REVISION_ID
                    || revision == STANDARD_LIBRARY_V9_REVISION_ID
            })
}

pub(super) fn resource_target_security_is_supported(definition: &FunctionDefinition) -> bool {
    definition.security() == FunctionSecurity::Invoker
}

pub(super) fn resource_target_shape_is_supported(
    definition: &FunctionDefinition,
    kind: ProtocolResourceKind,
) -> bool {
    if definition.domain() != FunctionDomain::Server {
        return false;
    }
    match (kind, definition.return_type()) {
        (ProtocolResourceKind::Single, FunctionReturn::Single(_)) => true,
        (ProtocolResourceKind::Stream, FunctionReturn::Stream(_)) => true,
        _ => false,
    }
}

pub(super) fn bind_authenticated_resource_arguments(
    context: &CatalogueHashContext,
    definition: &FunctionDefinition,
    arguments: &[ResourceArgument],
) -> Option<Vec<FunctionArgument>> {
    if arguments.len() != definition.parameters().len() {
        return None;
    }
    let mut previous = None;
    let mut bound = Vec::with_capacity(arguments.len());
    for argument in arguments {
        if previous.is_some_and(|previous| argument.parameter <= previous) {
            return None;
        }
        previous = Some(argument.parameter);
        let parameter = definition.parameter_by_id(argument.parameter)?;
        if matches!(argument.value, RuntimeValue::Opaque(_)) {
            return None;
        }
        let RuntimeType::Flat(actual) = argument.value.runtime_type() else {
            return None;
        };
        if !runtime_types_match(context, actual, parameter.resolved_type()) {
            return None;
        }
        bound.push(FunctionArgument::new(argument.parameter, argument.value.clone()).ok()?);
    }
    Some(bound)
}

pub(super) fn resource_result_value_is_supported(value: &RuntimeValue) -> bool {
    !matches!(
        value,
        RuntimeValue::InvokeValue(_)
            | RuntimeValue::InvokeRequest(_)
            | RuntimeValue::InvokeEvent(_)
    )
}

pub(super) fn resource_values_from_server_result(
    kind: ProtocolResourceKind,
    result: ServerSelectResult,
) -> Option<Vec<RuntimeValue>> {
    let rows = result.into_rows().into_rows();
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        let [value] = row.into_values().try_into().ok()?;
        if !resource_result_value_is_supported(&value) {
            return None;
        }
        values.push(value);
    }
    if kind == ProtocolResourceKind::Single && values.len() != 1 {
        return None;
    }
    Some(values)
}

pub(super) fn classify_sealed_server_error(
    error: &PostgresKernelError,
) -> SealedInvocationFailureClass {
    match error {
        PostgresKernelError::ServerSelect(source) if raw_server_target_is_unavailable(source) => {
            SealedInvocationFailureClass::Target
        }
        PostgresKernelError::ServerInsert(source)
            if raw_server_insert_target_is_unavailable(source) =>
        {
            SealedInvocationFailureClass::Target
        }
        PostgresKernelError::ServerUpdate(source)
            if raw_server_update_target_is_unavailable(source) =>
        {
            SealedInvocationFailureClass::Target
        }
        PostgresKernelError::ServerDelete(source)
            if raw_server_delete_target_is_unavailable(source) =>
        {
            SealedInvocationFailureClass::Target
        }
        _ => SealedInvocationFailureClass::Internal,
    }
}
