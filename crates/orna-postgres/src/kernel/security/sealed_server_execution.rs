//! Sealed SERVER target execution and streaming.

use super::resource_producer::{
    ResourceProducerCancelled, ResourceProducerCommand, ResourceProducerCompleted,
    ResourceProducerExit, ResourceProducerFailed, ResourceProducerPull,
    ResourceProducerSealedFailed, wait_for_resource_producer_pull_or_cancel,
};
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

fn sealed_rows_preservation_is_supported(
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

fn resource_result_value_is_supported(value: &RuntimeValue) -> bool {
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

/// Internal result boundary for one sealed SERVER target.
///
/// Direct `ROWS` invocations retain the complete [`ResultRows`] until the
/// caller encodes it as one registered opaque value. Resource and mutation
/// callers continue through the existing flattened value sequence.
enum SealedServerTargetResult {
    Values(Vec<RuntimeValue>),
    Rows(ResultRows),
}
async fn execute_sealed_server_target(
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
