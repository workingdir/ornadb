//! Pull-driven production for sealed SERVER resources.

use super::resource_producer::{
    ResourceProducerCancelled, ResourceProducerCommand, ResourceProducerCompleted,
    ResourceProducerExit, ResourceProducerFailed, ResourceProducerPull,
    ResourceProducerSealedFailed, wait_for_resource_producer_pull_or_cancel,
};
use super::sealed_server_contract::{
    classify_sealed_server_error, sealed_server_target_is_mutation,
};
use super::sealed_server_target::{SealedServerTargetResult, execute_sealed_server_target};
use super::*;

pub(super) fn sealed_server_stream_completed_event(
    final_batch_sequence: u64,
    total_items: u64,
    total_bytes: u64,
) -> AuthenticatedServerResourceEvent {
    AuthenticatedServerResourceEvent::Completed {
        final_batch_sequence,
        total_items,
        total_bytes,
    }
}

/// Executes one accepted mutation target and serves its bounded returned rows
/// through the existing sealed pull protocol.
async fn run_sealed_server_mutation_stream(
    transaction: &mut Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    commands: &mut tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> ResourceProducerExit {
    let failed = |response, failure| {
        ResourceProducerExit::SealedFailed(ResourceProducerSealedFailed {
            response: Some(response),
            failure,
        })
    };
    let mut values = None;
    let mut next_value = 0usize;
    let mut batch_sequence = 0u64;
    let mut total_items = 0u64;
    let mut total_bytes = 0u64;

    loop {
        let command = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                return ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None });
            }
            command = commands.recv() => command,
        };
        let Some(ResourceProducerCommand::Pull(ResourceProducerPull { credit, response })) =
            command
        else {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None });
        };
        if cancellation.is_requested() {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            });
        }

        if values.is_none() {
            let mutation_values = match execute_sealed_server_target(
                transaction,
                active,
                authorisation,
                arguments,
                ProtocolResourceKind::Stream,
                false,
            )
            .await
            {
                Ok(SealedServerTargetResult::Values(values)) => values,
                Ok(SealedServerTargetResult::Rows(_)) => {
                    return failed(response, SealedInvocationFailureClass::Internal);
                }
                Err(failure) => return failed(response, failure),
            };
            if mutation_values.len() > 1 {
                return failed(response, SealedInvocationFailureClass::Internal);
            }
            values = Some(mutation_values);
            if cancellation.is_requested() {
                return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: Some(response),
                });
            }
        }

        let mutation_values = values.as_ref().expect("sealed mutation values are loaded");
        let Some(value) = mutation_values.get(next_value).cloned() else {
            return ResourceProducerExit::Completed(ResourceProducerCompleted {
                response,
                final_batch_sequence: batch_sequence.saturating_sub(1),
                total_items,
                total_bytes,
            });
        };
        let byte_count = match encode_active_value(active, &value) {
            Ok(encoded) => match u64::try_from(encoded.len()) {
                Ok(byte_count) => byte_count,
                Err(_) => return failed(response, SealedInvocationFailureClass::Internal),
            },
            Err(_) => return failed(response, SealedInvocationFailureClass::Internal),
        };
        if credit.item_count == 0 || byte_count > credit.byte_count {
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                    required_bytes: byte_count,
                }))
                .is_err()
            {
                return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                });
            }
            continue;
        }
        if cancellation.is_requested() {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            });
        }
        let next_index = match next_value.checked_add(1) {
            Some(next_index) => next_index,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let total_items_next = match total_items.checked_add(1) {
            Some(total_items) => total_items,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let total_bytes_next = match total_bytes.checked_add(byte_count) {
            Some(total_bytes) => total_bytes,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let next_batch_sequence = match batch_sequence.checked_add(1) {
            Some(next_batch_sequence) => next_batch_sequence,
            None => return failed(response, SealedInvocationFailureClass::Internal),
        };
        let event = AuthenticatedServerResourceEvent::Values {
            batch_sequence,
            item_count: 1,
            byte_count,
            values: vec![value],
        };
        next_value = next_index;
        total_items = total_items_next;
        total_bytes = total_bytes_next;
        batch_sequence = next_batch_sequence;
        if response.send(Ok(event)).is_err() {
            return ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None });
        }
    }
}

async fn run_sealed_server_stream_producer(
    kernel: PostgresKernel,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    authorisation: AuthorisedInvocation,
    arguments: Vec<FunctionArgument>,
    invocation: InvocationId,
    cancellation: ResourceCancellation,
    mut commands: tokio::sync::mpsc::Receiver<ResourceProducerCommand>,
    ready: tokio::sync::oneshot::Sender<Result<(), SealedInvocationFailureClass>>,
) {
    let mut database_session = match kernel.open().await {
        Ok(session) => session,
        Err(_) => {
            let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
            return;
        }
    };
    let mut transaction = match database_session
        .client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .start()
        .await
    {
        Ok(transaction) => transaction,
        Err(_) => {
            let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
            return;
        }
    };
    let validation = async {
        require_current_migrations(&transaction).await?;
        lock_active_revision(&transaction, active.pair()).await?;
        let execution_active = configure_and_recover(&transaction).await?;
        if execution_active.pair() != active.pair() {
            return Err(PostgresKernelError::SecurityRevisionMismatch {
                expected: active.pair(),
                active: execution_active.pair(),
            });
        }
        let execution_security =
            recover_security_snapshot_for_active(&transaction, &execution_active).await?;
        if !security_snapshots_match(&execution_security, &security) {
            return Err(PostgresKernelError::SecurityFunctionSetMismatch);
        }
        Ok::<_, PostgresKernelError>(())
    }
    .await;
    if validation.is_err() {
        let _ = transaction.rollback().await;
        let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
        let _ = database_session.shutdown().await;
        return;
    }
    if cancellation.is_requested() {
        let _ = transaction.rollback().await;
        let _ = ready.send(Err(SealedInvocationFailureClass::Internal));
        let _ = database_session.shutdown().await;
        return;
    }
    if ready.send(Ok(())).is_err() {
        let _ = transaction.rollback().await;
        let _ = database_session.shutdown().await;
        return;
    }

    let mutation_target =
        sealed_server_target_is_mutation(&active, authorisation.target().function());
    let stream_result = if mutation_target {
        Ok(run_sealed_server_mutation_stream(
            &mut transaction,
            &active,
            &authorisation,
            &arguments,
            &mut commands,
            &cancellation,
        )
        .await)
    } else {
        run_authenticated_server_resource_stream(
            &transaction,
            &active,
            &authorisation,
            &arguments,
            &mut commands,
            &cancellation,
        )
        .await
    };
    let stream_result = match stream_result {
        Ok(result) => result,
        Err(error) => match wait_for_resource_producer_pull_or_cancel(&mut commands, &cancellation)
            .await
        {
            Some(pull) => ResourceProducerExit::Failed(ResourceProducerFailed {
                response: Some(pull.response),
                error,
            }),
            None => ResourceProducerExit::Cancelled(ResourceProducerCancelled { response: None }),
        },
    };
    match stream_result {
        ResourceProducerExit::Completed(ResourceProducerCompleted {
            response,
            final_batch_sequence,
            total_items,
            total_bytes,
        }) => {
            if !cancellation.try_begin_commit() {
                let _ = transaction.rollback().await;
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
            } else {
                let commit = transaction.commit().await;
                if commit.is_ok() {
                    cancellation.commit_finished();
                    let _ = response.send(Ok(sealed_server_stream_completed_event(
                        final_batch_sequence,
                        total_items,
                        total_bytes,
                    )));
                } else {
                    let _ = response.send(Err(PostgresKernelError::DurableInvariant {
                        relation: "sealed invocation producer",
                        record: invocation.canonical(),
                        rule: "sealed server stream transaction commit failed",
                    }));
                }
            }
        }
        ResourceProducerExit::Cancelled(ResourceProducerCancelled { response }) => {
            let _ = transaction.rollback().await;
            if let Some(response) = response {
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Cancelled));
            }
        }
        ResourceProducerExit::Failed(ResourceProducerFailed { response, error }) => {
            let _ = transaction.rollback().await;
            if let Some(response) = response {
                let failure = if classify_sealed_server_error(&error)
                    == SealedInvocationFailureClass::Target
                {
                    CallFailure::TargetUnavailable
                } else {
                    CallFailure::InternalFailure
                };
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Failed { failure }));
            }
        }
        ResourceProducerExit::SealedFailed(ResourceProducerSealedFailed { response, failure }) => {
            let _ = transaction.rollback().await;
            if let Some(response) = response {
                let failure = match failure {
                    SealedInvocationFailureClass::Target | SealedInvocationFailureClass::Bind => {
                        CallFailure::TargetUnavailable
                    }
                    SealedInvocationFailureClass::Internal => CallFailure::InternalFailure,
                };
                let _ = response.send(Ok(AuthenticatedServerResourceEvent::Failed { failure }));
            }
        }
    }
    let _ = database_session.shutdown().await;
}

pub(super) async fn start_sealed_server_stream_producer(
    kernel: PostgresKernel,
    active: ActiveDatabaseRevision,
    security: SecuritySnapshot,
    authorisation: AuthorisedInvocation,
    arguments: Vec<FunctionArgument>,
    invocation: InvocationId,
    cancellation: ResourceCancellation,
    runtime_handle: tokio::runtime::Handle,
) -> Result<AuthenticatedServerResourceProducer, SealedInvocationFailureClass> {
    let target_revision = active.pair();
    let (commands, receiver) = tokio::sync::mpsc::channel(1);
    let (ready, ready_receiver) = tokio::sync::oneshot::channel();
    runtime_handle.spawn(run_sealed_server_stream_producer(
        kernel,
        active,
        security,
        authorisation,
        arguments,
        invocation,
        cancellation.clone(),
        receiver,
        ready,
    ));
    match ready_receiver.await {
        Ok(Ok(())) => Ok(AuthenticatedServerResourceProducer {
            accepted: AuthenticatedServerResourceAccepted {
                stream_id: 0,
                request_id: invocation,
                nested_invocation_id: invocation,
                target_revision,
                resource_kind: AuthenticatedServerResourceKind::Stream,
            },
            commands,
            cancellation,
        }),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(SealedInvocationFailureClass::Internal),
    }
}
