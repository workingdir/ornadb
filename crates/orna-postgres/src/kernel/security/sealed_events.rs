use super::*;

/// Builds the exact sealed Event sequence for one completed invocation.
///
/// The batch carries `InvocationStarted(0)`, an optional non-empty
/// `ValueBatch(1)`, and `InvocationCompleted` as one contiguous outer record
/// sequence. A server adapter delivers this batch on the `RESULT_VALUES`
/// channel and then completes the call.
///
/// ADR 0057 step 7 passes either the canonical echo value (no output
/// requirement) or the presented opaque value in the final `ValueBatch`.
pub(crate) fn sealed_completed_events(
    _principal: PrincipalId,
    invocation: InvocationId,
    value: RuntimeValue,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    sealed_completed_events_from_values(_principal, invocation, vec![value])
}

/// Builds completed events from validated SERVER result values.
pub(super) fn sealed_completed_events_from_values(
    _principal: PrincipalId,
    invocation: InvocationId,
    values: Vec<RuntimeValue>,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    let mut records = vec![InvocationEventRecord::new(1, started)];
    let mut sequence = 1;
    if !values.is_empty() {
        let values = values
            .into_iter()
            .map(|value| InvokeValue::new(value).map_err(PostgresKernelError::InvocationCarrier))
            .collect::<Result<Vec<_>, _>>()?;
        let batch = InvokeEvent::new(
            invocation,
            sequence,
            InvocationEventBody::value_batch(None, values)
                .map_err(PostgresKernelError::InvocationCarrier)?,
        )
        .map_err(PostgresKernelError::InvocationCarrier)?;
        records.push(InvocationEventRecord::new(2, batch));
        sequence += 1;
    }
    let completed = InvokeEvent::new(
        invocation,
        sequence,
        InvocationEventBody::Completed {
            duration_nanoseconds: 0,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    records.push(InvocationEventRecord::new(sequence + 1, completed));
    InvocationEventBatch::new(records).map_err(PostgresKernelError::SealedInvocation)
}

pub(super) fn sealed_failure_events(
    invocation: InvocationId,
    failure: SealedInvocationFailureClass,
) -> Result<InvocationEventBatch, PostgresKernelError> {
    let (phase, code, message, retryability) = match failure {
        SealedInvocationFailureClass::Bind => (
            InvocationFailurePhase::Bind,
            "INVOKE_BIND_FAILED",
            "invocation arguments were not accepted",
            InvocationRetryability::No,
        ),
        SealedInvocationFailureClass::Target => (
            InvocationFailurePhase::Target,
            "INVOKE_TARGET_FAILED",
            "invocation target failed",
            InvocationRetryability::Unknown,
        ),
        SealedInvocationFailureClass::Internal => (
            InvocationFailurePhase::Internal,
            "INVOKE_INTERNAL_FAILURE",
            "invocation could not complete",
            InvocationRetryability::Unknown,
        ),
    };
    let started = InvokeEvent::new(
        invocation,
        0,
        InvocationEventBody::Started {
            visible_principal: None,
        },
    )
    .map_err(PostgresKernelError::InvocationCarrier)?;
    let failure = InvocationFailure::new(phase, code, message, None, retryability)
        .map_err(PostgresKernelError::InvocationCarrier)?;
    let failed = InvokeEvent::new(invocation, 1, InvocationEventBody::Failed(failure))
        .map_err(PostgresKernelError::InvocationCarrier)?;
    InvocationEventBatch::new(vec![
        InvocationEventRecord::new(1, started),
        InvocationEventRecord::new(2, failed),
    ])
    .map_err(PostgresKernelError::SealedInvocation)
}

pub(super) fn sealed_failure_result(
    invocation: InvocationId,
    failure: SealedInvocationFailureClass,
) -> Result<SealedInvocationResult, PostgresKernelError> {
    let events = sealed_failure_events(invocation, failure)?;
    Ok(SealedInvocationResult::Failed { invocation, events })
}

pub(super) async fn finish_sealed_failure(
    transaction: Transaction<'_>,
    invocation: InvocationId,
    failure: SealedInvocationFailureClass,
) -> Result<SealedInvocationResult, PostgresKernelError> {
    let events = sealed_failure_events(invocation, failure)?;
    let _ = transaction.rollback().await;
    Ok(SealedInvocationResult::Failed { invocation, events })
}
