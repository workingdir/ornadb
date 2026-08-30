//! Execution of one sealed SERVER target.

use super::sealed_server_contract::{
    classify_sealed_server_error, resource_result_value_is_supported,
    resource_values_from_server_result,
};
use super::*;

/// Internal result boundary for one sealed SERVER target.
///
/// Direct `ROWS` invocations retain the complete [`ResultRows`] until the
/// caller encodes it as one registered opaque value. Resource and mutation
/// callers continue through the existing flattened value sequence.
pub(super) enum SealedServerTargetResult {
    Values(Vec<RuntimeValue>),
    Rows(ResultRows),
}

pub(super) async fn execute_sealed_server_target(
    transaction: &mut Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    kind: ProtocolResourceKind,
    preserve_rows: bool,
) -> Result<SealedServerTargetResult, SealedInvocationFailureClass> {
    let savepoint = transaction
        .savepoint("sealed_server_execution")
        .await
        .map_err(|_| SealedInvocationFailureClass::Internal)?;
    let function = authorisation.target().function();
    let mutation = if raw_server_insert_target_is_selected(active, function) {
        Some(None)
    } else if arguments.len() == 2
        && raw_server_reference_value_update_target_is_selected(active, function)
    {
        Some(Some(RawServerReferenceMutation::Update))
    } else {
        raw_server_reference_mutation_target(active, function).map(Some)
    };
    let result = match mutation {
        Some(None) => {
            let result = if arguments.is_empty() {
                execute_authorised_raw_server_insert(&savepoint, active, authorisation).await
            } else {
                execute_authorised_raw_server_insert_with_arguments(
                    &savepoint,
                    active,
                    authorisation,
                    arguments,
                )
                .await
            };
            match result {
                Ok(value) => Some(SealedServerTargetResult::Values(vec![value])),
                Err(error) => {
                    let class = classify_sealed_server_error(&error);
                    if savepoint.rollback().await.is_err() {
                        return Err(SealedInvocationFailureClass::Internal);
                    }
                    return Err(class);
                }
            }
        }
        Some(Some(operation)) => {
            match execute_authorised_raw_server_reference_mutation(
                &savepoint,
                active,
                authorisation,
                operation,
                arguments,
            )
            .await
            {
                Ok(values) => Some(SealedServerTargetResult::Values(values)),
                Err(error) => {
                    let class = classify_sealed_server_error(&error);
                    if savepoint.rollback().await.is_err() {
                        return Err(SealedInvocationFailureClass::Internal);
                    }
                    return Err(class);
                }
            }
        }
        None => {
            match execute_authorised_server_select(&savepoint, active, authorisation, arguments)
                .await
            {
                Ok(server) if preserve_rows => {
                    Some(SealedServerTargetResult::Rows(server.into_rows()))
                }
                Ok(server) => resource_values_from_server_result(kind, server)
                    .map(SealedServerTargetResult::Values),
                Err(error) => {
                    let class = classify_sealed_server_error(&error);
                    if savepoint.rollback().await.is_err() {
                        return Err(SealedInvocationFailureClass::Internal);
                    }
                    return Err(class);
                }
            }
        }
    };
    let Some(result) = result else {
        if savepoint.rollback().await.is_err() {
            return Err(SealedInvocationFailureClass::Internal);
        }
        return Err(SealedInvocationFailureClass::Target);
    };
    let result = match result {
        SealedServerTargetResult::Values(values) => {
            if values.len() != 1 && kind != ProtocolResourceKind::Stream {
                if savepoint.rollback().await.is_err() {
                    return Err(SealedInvocationFailureClass::Internal);
                }
                return Err(SealedInvocationFailureClass::Target);
            }
            if values
                .iter()
                .any(|value| !resource_result_value_is_supported(value))
            {
                if savepoint.rollback().await.is_err() {
                    return Err(SealedInvocationFailureClass::Internal);
                }
                return Err(SealedInvocationFailureClass::Target);
            }
            SealedServerTargetResult::Values(values)
        }
        SealedServerTargetResult::Rows(rows) => SealedServerTargetResult::Rows(rows),
    };
    savepoint
        .commit()
        .await
        .map_err(|_| SealedInvocationFailureClass::Internal)?;
    Ok(result)
}
