//! Post-audit execution of one sealed SERVER invocation.

use super::sealed_server_contract::{
    sealed_rows_preservation_is_supported, sealed_server_result_kind,
};
use super::sealed_server_target::{SealedServerTargetResult, execute_sealed_server_target};
use super::*;

pub(super) async fn execute_sealed_server_after_audit(
    client: &mut tokio_postgres::Client,
    active: &ActiveDatabaseRevision,
    pinned_security: &SecuritySnapshot,
    registry: &OpaqueCodecRegistry,
    authenticated_session: &AuthenticatedSession,
    definition: &FunctionDefinition,
    decoded: &orna_core::invocation::InvokeRequest,
    security_target: InvocationTarget,
    authorisation: &AuthorisedInvocation,
    invocation: InvocationId,
) -> Result<SealedInvocationResult, PostgresKernelError> {
    let mut transaction = match client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
    {
        Ok(transaction) => transaction,
        Err(_) => {
            return sealed_failure_result(invocation, SealedInvocationFailureClass::Internal);
        }
    };
    if require_current_migrations(&transaction).await.is_err() {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    if lock_active_revision(&transaction, active.pair())
        .await
        .is_err()
    {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    let execution_active = match configure_and_recover(&transaction).await {
        Ok(active) => active,
        Err(_) => {
            return finish_sealed_failure(
                transaction,
                invocation,
                SealedInvocationFailureClass::Internal,
            )
            .await;
        }
    };
    if execution_active.pair() != active.pair() {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    let execution_security =
        match recover_security_snapshot_for_active(&transaction, &execution_active).await {
            Ok(security) => security,
            Err(_) => {
                return finish_sealed_failure(
                    transaction,
                    invocation,
                    SealedInvocationFailureClass::Internal,
                )
                .await;
            }
        };
    if !security_snapshots_match(&execution_security, pinned_security) {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    let arguments = match bind_sealed_invoke_arguments(definition, decoded.arguments()) {
        Ok(arguments) => arguments,
        Err(_) => {
            return finish_sealed_failure(
                transaction,
                invocation,
                SealedInvocationFailureClass::Bind,
            )
            .await;
        }
    };
    let kind = match sealed_server_result_kind(definition.return_type()) {
        Some(kind) => kind,
        None => {
            return finish_sealed_failure(
                transaction,
                invocation,
                SealedInvocationFailureClass::Target,
            )
            .await;
        }
    };
    let preserve_rows = sealed_rows_preservation_is_supported(active, definition.return_type());
    let target_result = match execute_sealed_server_target(
        &mut transaction,
        active,
        authorisation,
        &arguments,
        kind,
        preserve_rows,
    )
    .await
    {
        Ok(result) => result,
        Err(failure) => {
            return finish_sealed_failure(transaction, invocation, failure).await;
        }
    };
    let values = match target_result {
        SealedServerTargetResult::Values(values) => values,
        SealedServerTargetResult::Rows(rows) => {
            let value = match encode_rows_value(active, registry, &rows) {
                Ok(value) => value,
                Err(_) => {
                    return finish_sealed_failure(
                        transaction,
                        invocation,
                        SealedInvocationFailureClass::Internal,
                    )
                    .await;
                }
            };
            vec![value]
        }
    };
    let events = match decoded.output_requirement() {
        Some(requirement) => {
            if values.len() != 1 {
                return finish_sealed_failure(
                    transaction,
                    invocation,
                    SealedInvocationFailureClass::Target,
                )
                .await;
            }
            let value = values
                .into_iter()
                .next()
                .expect("one result value was checked");
            match present_sealed_standard_output(
                requirement,
                value,
                decoded.client_offer(),
                active,
                registry,
            ) {
                Ok(presented) => match sealed_completed_events(
                    authenticated_session.principal(),
                    invocation,
                    presented,
                ) {
                    Ok(events) => events,
                    Err(_) => {
                        return finish_sealed_failure(
                            transaction,
                            invocation,
                            SealedInvocationFailureClass::Target,
                        )
                        .await;
                    }
                },
                Err(
                    SealedPresentationError::OutputResolution(_) | SealedPresentationError::NoPath,
                ) => {
                    if transaction.commit().await.is_err() {
                        return sealed_failure_result(
                            invocation,
                            SealedInvocationFailureClass::Internal,
                        );
                    }
                    return Ok(SealedInvocationResult::PresentationFailed { invocation });
                }
                Err(SealedPresentationError::Kernel(_)) => {
                    return finish_sealed_failure(
                        transaction,
                        invocation,
                        SealedInvocationFailureClass::Internal,
                    )
                    .await;
                }
            }
        }
        None => match sealed_completed_events_from_values(
            authenticated_session.principal(),
            invocation,
            values,
        ) {
            Ok(events) => events,
            Err(_) => {
                return finish_sealed_failure(
                    transaction,
                    invocation,
                    SealedInvocationFailureClass::Target,
                )
                .await;
            }
        },
    };
    if capture_sealed_invocation_snapshot(
        &transaction,
        active,
        registry,
        authenticated_session,
        invocation,
        security_target.function(),
        &events,
        decoded.client_offer(),
        None,
        decoded.output_requirement(),
    )
    .await
    .is_err()
    {
        return finish_sealed_failure(
            transaction,
            invocation,
            SealedInvocationFailureClass::Internal,
        )
        .await;
    }
    if transaction.commit().await.is_err() {
        return sealed_failure_result(invocation, SealedInvocationFailureClass::Internal);
    }
    Ok(SealedInvocationResult::Completed { invocation, events })
}
