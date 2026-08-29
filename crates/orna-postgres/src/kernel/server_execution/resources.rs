use super::*;

/// Runs one authenticated SERVER resource query against one owned transaction.
///
/// Planning, statement preparation, the query stream, and every decoded row stay
/// in this task. The command receiver is deliberately pull-driven: at most one
/// row is decoded for one command, and a row that exceeds byte credit is retained
/// as one bounded pending value rather than materialising the result set.
pub(crate) async fn run_authenticated_server_resource_stream(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    arguments: &[FunctionArgument],
    commands: &mut mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> Result<ResourceProducerExit, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(server_error(ServerSelectError::AuthorisationMismatch {
            authorised: Box::new(target),
            active: active.pair(),
        }));
    }
    let function = active
        .catalogue()
        .function_by_id(target.function())
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: target.function(),
            })
        })?;
    let context = ServerSelectContext::new(
        active.pair(),
        target.function(),
        function.current_revision(),
    );
    let prepared =
        prepare_active_transaction(transaction, active, function, context, arguments).await?;
    let parameters = prepared
        .binds
        .iter()
        .map(SelectBindValue::as_to_sql)
        .collect::<Vec<_>>();
    let stream = transaction
        .query_raw(&prepared.statement, parameters)
        .await
        .map_err(PostgresKernelError::Database)?;
    futures_util::pin_mut!(stream);

    let mut rows_seen = 0usize;
    let mut cells = 0usize;
    let mut payload = initial_payload_len(&prepared.columns)?;
    let mut pending: Option<(RuntimeValue, u64)> = None;
    let mut batch_sequence = 0u64;
    let mut final_batch_sequence = 0u64;
    let mut total_items = 0u64;
    let mut total_bytes = 0u64;

    loop {
        let cancelled = cancellation.cancelled();
        let received = commands.recv();
        futures_util::pin_mut!(cancelled, received);
        let command = match select(cancelled, received).await {
            Either::Left(((), _received)) => {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            Either::Right((command, _cancelled)) => command,
        };
        let Some(ResourceProducerCommand::Pull(ResourceProducerPull { credit, response })) =
            command
        else {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: None,
            }));
        };
        let scalar = matches!(
            prepared.cardinality,
            ResultCardinality::ExactlyOne | ResultCardinality::AtMostOne
        );
        if (credit.item_count == 0 && rows_seen == 0)
            || (credit.byte_count == 0 && !scalar && rows_seen == 0)
        {
            return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                response: Some(response),
                error: server_error(ServerSelectError::Argument {
                    parameter: None,
                    rule: "resource pull credit must be non-zero",
                }),
            }));
        }
        if cancellation.is_requested() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            }));
        }

        let (value, byte_count) = if let Some(value) = pending.take() {
            value
        } else {
            let cancelled = cancellation.cancelled();
            let next_row = stream.try_next();
            futures_util::pin_mut!(cancelled, next_row);
            let row = match select(cancelled, next_row).await {
                Either::Left(((), _next_row)) => {
                    return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                        response: Some(response),
                    }));
                }
                Either::Right((row, _cancelled)) => match row {
                    Ok(row) => row,
                    Err(error) => {
                        return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                            response: Some(response),
                            error: PostgresKernelError::Database(error),
                        }));
                    }
                },
            };
            let Some(row) = row else {
                if let Err(error) = prepared.cardinality.finish(rows_seen) {
                    return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                        response: Some(response),
                        error,
                    }));
                }
                return Ok(ResourceProducerExit::Completed(ResourceProducerCompleted {
                    response,
                    final_batch_sequence,
                    total_items,
                    total_bytes,
                }));
            };
            if let Err(error) = prepared.cardinality.validate(rows_seen.saturating_add(1)) {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error,
                }));
            }
            if rows_seen == ROW_LIMIT {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::RowLimit { maximum: ROW_LIMIT }),
                }));
            }
            cells = match cells.checked_add(prepared.columns.len()) {
                Some(cells) => cells,
                None => {
                    return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                        response: Some(response),
                        error: server_error(ServerSelectError::CellLimit {
                            maximum: CELL_LIMIT,
                        }),
                    }));
                }
            };
            if cells > CELL_LIMIT {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::CellLimit {
                        maximum: CELL_LIMIT,
                    }),
                }));
            }
            let decoded = (|| -> Result<(RuntimeValue, u64), PostgresKernelError> {
                let row_index = rows_seen;
                for (guard_index, guard) in prepared.guards.iter().enumerate() {
                    let accepted = row
                        .try_get::<usize, bool>(prepared.columns.len() + guard_index)
                        .map_err(|source| {
                            server_error(ServerSelectError::RowDecode {
                                row: row_index,
                                column: prepared.columns.len() + guard_index,
                                source,
                            })
                        })?;
                    if !accepted {
                        return Err(server_error(ServerSelectError::VariablePayload {
                            row: row_index,
                            column: guard.column,
                            maximum: prepared.variable_payload_limit,
                        }));
                    }
                }
                let mut values = Vec::with_capacity(prepared.columns.len());
                for (column_index, column) in prepared.columns.iter().enumerate() {
                    let value = decode_value(active, &row, row_index, column_index, column)?;
                    let value_payload = match &value {
                        RuntimeValue::Record(_) => {
                            canonical_record_payload_len(active, &value, row_index, column_index)?
                        }
                        _ => logical_payload_len(&value)?,
                    };
                    payload = add_payload(payload, value_payload)?;
                    values.push(value);
                }
                rows_seen = rows_seen.saturating_add(1);
                let [value] = values.try_into().map_err(|_| {
                    server_error(ServerSelectError::PreparedResult {
                        rule: "resource SERVER execution must produce exactly one value per row",
                    })
                })?;
                let encoded = encode_active_value(active, &value).map_err(|source| {
                    server_error(ServerSelectError::ValueCodec {
                        row: row_index,
                        column: 0,
                        source,
                    })
                })?;
                let byte_count = u64::try_from(encoded.len()).map_err(|_| {
                    server_error(ServerSelectError::PayloadLimit {
                        maximum: PAYLOAD_LIMIT,
                    })
                })?;
                Ok((value, byte_count))
            })();
            let (value, byte_count) = match decoded {
                Ok(value) => value,
                Err(error) => {
                    return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                        response: Some(response),
                        error,
                    }));
                }
            };
            if !matches!(prepared.cardinality, ResultCardinality::BoundedMany) {
                let cancelled = cancellation.cancelled();
                let next_row = stream.try_next();
                futures_util::pin_mut!(cancelled, next_row);
                let lookahead = match select(cancelled, next_row).await {
                    Either::Left(((), _next_row)) => {
                        return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                            response: Some(response),
                        }));
                    }
                    Either::Right((row, _cancelled)) => row,
                };
                match lookahead {
                    Err(error) => {
                        return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                            response: Some(response),
                            error: PostgresKernelError::Database(error),
                        }));
                    }
                    Ok(Some(_)) => {
                        if let Err(error) =
                            prepared.cardinality.validate(rows_seen.saturating_add(1))
                        {
                            return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                                response: Some(response),
                                error,
                            }));
                        }
                    }
                    Ok(None) => {}
                }
            }
            (value, byte_count)
        };
        if credit.item_count == 0 {
            pending = Some((value, byte_count));
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                    required_bytes: byte_count,
                }))
                .is_err()
            {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            continue;
        }
        if byte_count > credit.byte_count {
            pending = Some((value, byte_count));
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                    required_bytes: byte_count,
                }))
                .is_err()
            {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            continue;
        }
        if cancellation.is_requested() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            }));
        }
        total_items = match total_items.checked_add(1) {
            Some(total_items) => total_items,
            None => {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::RowLimit { maximum: ROW_LIMIT }),
                }));
            }
        };
        total_bytes = match total_bytes.checked_add(byte_count) {
            Some(total_bytes) => total_bytes,
            None => {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::PayloadLimit {
                        maximum: PAYLOAD_LIMIT,
                    }),
                }));
            }
        };
        let event = AuthenticatedServerResourceEvent::Values {
            batch_sequence,
            item_count: 1,
            byte_count,
            values: vec![value],
        };
        final_batch_sequence = batch_sequence;
        batch_sequence = match batch_sequence.checked_add(1) {
            Some(batch_sequence) => batch_sequence,
            None => {
                return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                    response: Some(response),
                    error: server_error(ServerSelectError::RowLimit { maximum: ROW_LIMIT }),
                }));
            }
        };
        if response.send(Ok(event)).is_err() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: None,
            }));
        }
    }
}

/// Runs one verified-standard SERVER resource target through the same bounded
/// pull protocol as an application target.
///
/// The standard executable is already pinned by the protected resource
/// decision. Standard resource targets currently use the closed parameter-echo
/// engine; no SQL preparation or PostgreSQL row stream is involved.
pub(crate) async fn run_authenticated_standard_resource_stream(
    active: &ActiveDatabaseRevision,
    authorisation: &AuthorisedInvocation,
    executable: &StandardExecutable,
    arguments: &[FunctionArgument],
    commands: &mut mpsc::Receiver<ResourceProducerCommand>,
    cancellation: &ResourceCancellation,
) -> Result<ResourceProducerExit, PostgresKernelError> {
    let target = authorisation.target();
    if target.revision() != active.pair() {
        return Err(server_error(ServerSelectError::AuthorisationMismatch {
            authorised: Box::new(target),
            active: active.pair(),
        }));
    }
    let standard = active.catalogue_hash_context().standard().ok_or_else(|| {
        server_error(ServerSelectError::FunctionNotActive {
            pair: active.pair(),
            function: target.function(),
        })
    })?;
    let function = standard
        .catalogue()
        .function_by_id(target.function())
        .ok_or_else(|| {
            server_error(ServerSelectError::FunctionNotActive {
                pair: active.pair(),
                function: target.function(),
            })
        })?;
    if executable.function() != target.function()
        || executable.revision().function() != target.function()
        || executable.revision().id() != function.current_revision()
    {
        return Err(server_error(ServerSelectError::FunctionNotActive {
            pair: active.pair(),
            function: target.function(),
        }));
    }
    let value = execute_standard_parameter_echo(function, executable.revision(), arguments)?;
    let encoded = encode_active_value(active, &value).map_err(|source| {
        server_error(ServerSelectError::ValueCodec {
            row: 0,
            column: 0,
            source,
        })
    })?;
    let byte_count = u64::try_from(encoded.len()).map_err(|_| {
        server_error(ServerSelectError::PayloadLimit {
            maximum: PAYLOAD_LIMIT,
        })
    })?;
    let mut emitted = false;

    loop {
        let cancelled = cancellation.cancelled();
        let received = commands.recv();
        futures_util::pin_mut!(cancelled, received);
        let command = match select(cancelled, received).await {
            Either::Left(((), _received)) => {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            Either::Right((command, _cancelled)) => command,
        };
        let Some(ResourceProducerCommand::Pull(ResourceProducerPull { credit, response })) =
            command
        else {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: None,
            }));
        };
        if credit.item_count == 0 && !emitted {
            return Ok(ResourceProducerExit::Failed(ResourceProducerFailed {
                response: Some(response),
                error: server_error(ServerSelectError::Argument {
                    parameter: None,
                    rule: "resource pull credit must be non-zero",
                }),
            }));
        }
        if cancellation.is_requested() {
            return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                response: Some(response),
            }));
        }
        if !emitted {
            if byte_count > credit.byte_count {
                if response
                    .send(Ok(AuthenticatedServerResourceEvent::Waiting {
                        required_bytes: byte_count,
                    }))
                    .is_err()
                {
                    return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                        response: None,
                    }));
                }
                continue;
            }
            emitted = true;
            if response
                .send(Ok(AuthenticatedServerResourceEvent::Values {
                    batch_sequence: 0,
                    item_count: 1,
                    byte_count,
                    values: vec![value.clone()],
                }))
                .is_err()
            {
                return Ok(ResourceProducerExit::Cancelled(ResourceProducerCancelled {
                    response: None,
                }));
            }
            continue;
        }
        return Ok(ResourceProducerExit::Completed(ResourceProducerCompleted {
            response,
            final_batch_sequence: 0,
            total_items: 1,
            total_bytes: byte_count,
        }));
    }
}
