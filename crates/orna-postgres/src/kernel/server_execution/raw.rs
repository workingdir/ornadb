use super::*;

/// Executes one raw-compatible SERVER SELECT through its existing authorised entry.
///
/// Parameter-free calls retain the one-column, many-row boundary. A call with
/// one Reference uses the version-2 identity-selected boundary and one Text
/// value uses the version-4 unique-Text-selected boundary. Both flatten only
/// their zero-or-one result row. The caller owns the savepoint and outer
/// transaction.
pub(crate) async fn execute_authorised_raw_server_select(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    let function = authorisation.target().function();
    if raw_unique_text_selected_server_select_target_is_selected(active, function) {
        validate_raw_unique_text_selected_server_select_target(active, function)?;
    } else if arguments.is_empty() {
        validate_raw_server_select_target(active, function)?;
    } else {
        validate_raw_identity_selected_server_select_target(active, function)?;
    }
    let result =
        execute_authorised_server_select(transaction, active, authorisation, arguments).await?;
    if arguments.is_empty() {
        into_raw_server_values(active, function, result)
    } else {
        into_raw_selected_server_values(active, function, result)
    }
}

/// Reports whether an active artifact is a superficial version-4 raw SELECT candidate.
///
/// The check deliberately stops before decoding or validating the target. An
/// authorised caller uses it only to select the existing SELECT savepoint;
/// complete validation remains in [`execute_authorised_raw_server_select`].
pub(crate) fn raw_unique_text_selected_server_select_target_is_selected(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> bool {
    let Some(function) = active.catalogue().function_by_id(function_id) else {
        return false;
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == function_id && revision.id() == function.current_revision()
    }) else {
        return false;
    };
    let artifact = revision.artifact();
    function.domain() == FunctionDomain::Server
        && artifact.kind() == ExecutableArtifactKind::Server
        && artifact.format() == SERVER_PLAN_FORMAT
        && artifact.version() == UNIQUE_TEXT_SELECTED_SERVER_PLAN_VERSION
}

/// Reports whether an active artifact is a superficial version-2 raw SELECT candidate.
///
/// The check deliberately stops before decoding or validating the target. An
/// authorised caller uses it only to select the existing SELECT savepoint;
/// complete validation remains in [`execute_authorised_raw_server_select`].
pub(crate) fn raw_identity_selected_server_select_target_is_selected(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> bool {
    let Some(function) = active.catalogue().function_by_id(function_id) else {
        return false;
    };
    let Some(revision) = active.function_revisions().iter().find(|revision| {
        revision.function() == function_id && revision.id() == function.current_revision()
    }) else {
        return false;
    };
    let artifact = revision.artifact();
    function.domain() == FunctionDomain::Server
        && artifact.kind() == ExecutableArtifactKind::Server
        && artifact.format() == SERVER_PLAN_FORMAT
        && artifact.version() == IDENTITY_SELECTED_SERVER_PLAN_VERSION
}

fn validate_raw_identity_selected_server_select_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Result<(), PostgresKernelError> {
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function_id,
        }));
    }
    if function.parameters().len() != 1 {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER calls must declare exactly one parameter",
        ));
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER calls must return nonempty ROWS",
        ));
    };
    if columns.is_empty() {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER calls must return nonempty ROWS",
        ));
    }
    if columns.iter().any(|column| {
        !raw_result_type_is_supported(
            active.catalogue(),
            active.catalogue_hash_context(),
            column.resolved_type(),
        )
    }) {
        return Err(raw_target_error(
            function_id,
            "raw identity-selected SERVER results support only protocol-1 scalar and reference values",
        ));
    }
    Ok(())
}

fn validate_raw_unique_text_selected_server_select_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Result<(), PostgresKernelError> {
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function_id,
        }));
    }
    if function.parameters().len() != 1 {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER calls must declare exactly one parameter",
        ));
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER calls must return nonempty ROWS",
        ));
    };
    if columns.is_empty() {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER calls must return nonempty ROWS",
        ));
    }
    if columns.iter().any(|column| {
        !raw_result_type_is_supported(
            active.catalogue(),
            active.catalogue_hash_context(),
            column.resolved_type(),
        )
    }) {
        return Err(raw_target_error(
            function_id,
            "raw unique-Text-selected SERVER results support only protocol-1 scalar and reference values",
        ));
    }
    Ok(())
}

/// Validates the closed parameter-free, one-column raw SERVER target shape.
fn validate_raw_server_select_target(
    active: &ActiveDatabaseRevision,
    function_id: FunctionId,
) -> Result<(), PostgresKernelError> {
    let function = active
        .catalogue()
        .function_by_id(function_id)
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: function_id,
            })
        })?;
    if function.domain() != FunctionDomain::Server {
        return Err(server_error(ServerSelectError::FunctionDomain {
            function: function_id,
        }));
    }
    if !function.parameters().is_empty() {
        return Err(raw_target_error(
            function_id,
            "raw SERVER calls must have zero parameters",
        ));
    }
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(raw_target_error(
            function_id,
            "raw SERVER calls must return ROWS with exactly one column",
        ));
    };
    let [column] = columns.as_slice() else {
        return Err(raw_target_error(
            function_id,
            "raw SERVER calls must return ROWS with exactly one column",
        ));
    };
    if !raw_result_type_is_supported(
        active.catalogue(),
        active.catalogue_hash_context(),
        column.resolved_type(),
    ) {
        return Err(raw_target_error(
            function_id,
            "raw SERVER results support only protocol-1 scalar and reference values",
        ));
    }
    Ok(())
}

/// Transfers one-column raw SERVER results without cloning value payloads.
fn into_raw_server_values(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    result: ServerSelectResult,
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    into_raw_server_values_for_context(
        active.catalogue(),
        active.catalogue_hash_context(),
        function,
        result,
    )
}

pub(super) fn into_raw_server_values_for_context(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: FunctionId,
    result: ServerSelectResult,
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    result
        .into_rows()
        .into_rows()
        .into_iter()
        .map(|row| {
            let [value] =
                <Vec<RuntimeValue> as TryInto<[RuntimeValue; 1]>>::try_into(row.into_values())
                    .map_err(|_| {
                        raw_target_error(
                            function,
                            "raw SERVER execution must produce exactly one value per row",
                        )
                    })?;
            normalise_raw_runtime_value(catalogue, context, function, value)
        })
        .collect()
}

/// Transfers one zero-or-one-row raw identity result in projection order.
fn into_raw_selected_server_values(
    active: &ActiveDatabaseRevision,
    function: FunctionId,
    result: ServerSelectResult,
) -> Result<Vec<RuntimeValue>, PostgresKernelError> {
    let mut rows = result.into_rows().into_rows().into_iter();
    let Some(row) = rows.next() else {
        return Ok(Vec::new());
    };
    if rows.next().is_some() {
        return Err(raw_target_error(
            function,
            "raw selected SERVER execution must produce at most one row",
        ));
    }
    row.into_values()
        .into_iter()
        .map(|value| {
            normalise_raw_runtime_value(
                active.catalogue(),
                active.catalogue_hash_context(),
                function,
                value,
            )
        })
        .collect()
}

fn normalise_raw_runtime_value(
    catalogue: &CatalogueSnapshot,
    context: &CatalogueHashContext,
    function: FunctionId,
    value: RuntimeValue,
) -> Result<RuntimeValue, PostgresKernelError> {
    if let RuntimeValue::Null(value) = value {
        normalise_raw_null(catalogue, context, function, value.resolved_type())
    } else if raw_runtime_value_is_supported(&value) {
        Ok(value)
    } else {
        Err(raw_target_error(
            function,
            "raw SERVER execution produced a value outside the protocol-1 subset",
        ))
    }
}

/// Reports whether a SERVER failure is an unavailable raw target, not an operational failure.
pub(crate) const fn raw_server_target_is_unavailable(error: &ServerSelectError) -> bool {
    match error {
        ServerSelectError::Execution { source, .. } => raw_server_target_is_unavailable(source),
        ServerSelectError::FunctionNotActive { .. }
        | ServerSelectError::FunctionDomain { .. }
        | ServerSelectError::FunctionSignature { .. }
        | ServerSelectError::RawTarget { .. }
        | ServerSelectError::Artifact { .. }
        | ServerSelectError::PlanDecode(_)
        | ServerSelectError::ParameterEchoDecode(_)
        | ServerSelectError::JsonEncodeDecode(_)
        | ServerSelectError::TerminalTableDecode(_)
        | ServerSelectError::CsvEncodeDecode(_)
        | ServerSelectError::PlanInvariant { .. }
        | ServerSelectError::Distinct { .. }
        | ServerSelectError::ReferenceEvidence { .. }
        | ServerSelectError::Argument { .. }
        | ServerSelectError::Cardinality { .. }
        | ServerSelectError::ResultRows(_)
        | ServerSelectError::VariablePayload { .. }
        | ServerSelectError::ComplexityLimit { .. }
        | ServerSelectError::RowLimit { .. }
        | ServerSelectError::CellLimit { .. }
        | ServerSelectError::PayloadLimit { .. } => true,
        ServerSelectError::AuthorisationMismatch { .. }
        | ServerSelectError::Database { .. }
        | ServerSelectError::Kernel { .. }
        | ServerSelectError::CurrentRevision { .. }
        | ServerSelectError::PreparedResult { .. }
        | ServerSelectError::ReturnedRows(_)
        | ServerSelectError::Presenter { .. }
        | ServerSelectError::PresenterOpaque(_)
        | ServerSelectError::RowDecode { .. }
        | ServerSelectError::ValueInvariant { .. }
        | ServerSelectError::ValueCodec { .. } => false,
    }
}
