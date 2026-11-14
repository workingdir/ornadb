use super::*;

pub(super) const REQUEST_TERMINAL_PREFIX: &[u8] = b"orna-conformance/request-terminal/v1/";

/// Encodes the conformance entrypoint's retained terminal result without
/// defining a second protocol. Runtime terminal outcomes are opaque bounded
/// bytes; this local envelope is only used to prevent a duplicate request
/// from executing source again.
pub(super) fn request_terminal(
    outcome: &StageOutcome<Diagnostic>,
) -> Result<TerminalOutcome, RuntimeError> {
    let mut bytes = Vec::from(REQUEST_TERMINAL_PREFIX);
    match outcome {
        StageOutcome::Passed => bytes.extend_from_slice(b"passed"),
        StageOutcome::Failed(diagnostic) => {
            bytes.extend_from_slice(b"failed/");
            bytes.extend(
                diagnostic
                    .encode_ovb()
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            );
        }
        StageOutcome::Cancelled(diagnostic) => {
            bytes.extend_from_slice(b"cancelled/");
            bytes.extend(
                diagnostic
                    .encode_ovb()
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            );
        }
        StageOutcome::Skipped { reason } => {
            bytes.extend_from_slice(b"skipped/");
            bytes.extend_from_slice(reason.as_bytes());
        }
    }
    TerminalOutcome::new(bytes)
}

pub(super) fn replay_request_terminal(
    bytes: &[u8],
) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
    let body = bytes
        .strip_prefix(REQUEST_TERMINAL_PREFIX)
        .ok_or(RuntimeError::RecoveryInvalid)?;
    if body == b"passed" {
        return Ok(StageOutcome::Passed);
    }
    if let Some(reason) = body.strip_prefix(b"skipped/") {
        return Ok(StageOutcome::Skipped {
            reason: std::str::from_utf8(reason)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                .into(),
        });
    }
    if let Some(diagnostic) = body.strip_prefix(b"cancelled/") {
        return Ok(StageOutcome::Cancelled(
            Diagnostic::decode_ovb(diagnostic).map_err(|_| RuntimeError::RecoveryInvalid)?,
        ));
    }
    let diagnostic = body
        .strip_prefix(b"failed/")
        .ok_or(RuntimeError::RecoveryInvalid)
        .and_then(|bytes| {
            Diagnostic::decode_ovb(bytes).map_err(|_| RuntimeError::RecoveryInvalid)
        })?;
    Ok(StageOutcome::Failed(diagnostic))
}

pub(super) fn replay_or_fence_request(
    status: RequestStatus,
) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
    match status.terminal_outcome {
        Some(outcome) => replay_request_terminal(outcome.as_bytes()),
        None => Err(RuntimeError::RequestStateConflict),
    }
}

pub(super) fn request_consumer_identity() -> ConsumerIdentity {
    ConsumerIdentity {
        principal: Component::new("orna").expect("static consumer component"),
        root: Component::new("conformance").expect("static consumer component"),
        function: Component::new("request").expect("static consumer component"),
        binding: Component::new("durable").expect("static consumer component"),
    }
}

pub(super) async fn terminalize_observed_outcome(
    state: &RuntimeState,
    request: RequestIdentity,
    fingerprint: [u8; 32],
    lease: WriterLease,
    outcome: StageOutcome<Diagnostic>,
) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
    let terminal = request_terminal(&outcome)?;
    match outcome {
        StageOutcome::Failed(_) => {
            state
                .fail_observed_request_with_owner(
                    request,
                    fingerprint,
                    lease,
                    terminal,
                    SafeDiagnostic {
                        code: DiagnosticCode::ExecutionRejected,
                        class: DiagnosticClass::Permanent,
                    },
                )
                .await?;
        }
        StageOutcome::Cancelled(_) => {
            state
                .cancel_observed_request_with_owner(request, fingerprint, lease, terminal)
                .await?;
        }
        StageOutcome::Passed | StageOutcome::Skipped { .. } => {
            state
                .complete_observed_request_with_owner(request, fingerprint, lease, terminal)
                .await?;
        }
    }
    Ok(outcome)
}

pub(super) async fn fail_observed_request_outcome(
    state: &RuntimeState,
    request: RequestIdentity,
    fingerprint: [u8; 32],
    lease: WriterLease,
    outcome: StageOutcome<Diagnostic>,
) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
    let terminal = request_terminal(&outcome)?;
    state
        .fail_observed_request_with_owner(
            request,
            fingerprint,
            lease,
            terminal,
            SafeDiagnostic {
                code: DiagnosticCode::ExecutionRejected,
                class: DiagnosticClass::Permanent,
            },
        )
        .await?;
    Ok(outcome)
}

pub(super) async fn cancel_observed_request_outcome(
    state: &RuntimeState,
    request: RequestIdentity,
    fingerprint: [u8; 32],
    lease: WriterLease,
    outcome: StageOutcome<Diagnostic>,
) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
    let terminal = request_terminal(&outcome)?;
    state
        .cancel_observed_request_with_owner(request, fingerprint, lease, terminal)
        .await?;
    Ok(outcome)
}

pub(super) async fn fail_observed_request_runtime(
    state: &RuntimeState,
    request: RequestIdentity,
    fingerprint: [u8; 32],
    lease: WriterLease,
) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
    let outcome = StageOutcome::Failed(request_runtime_failure_diagnostic());
    let terminal = request_terminal(&outcome)?;
    state
        .fail_observed_request_with_owner(
            request,
            fingerprint,
            lease,
            terminal,
            SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Transient,
            },
        )
        .await?;
    Ok(outcome)
}

pub(super) fn request_runtime_failure_diagnostic() -> Diagnostic {
    Diagnostic::new(
        SafeText::new("ORNA-RUNTIME-REQUEST").expect("static code"),
        DiagnosticSeverity::Error,
        SafeText::new("request-bound durable activation could not be completed")
            .expect("static message"),
    )
    .expect("valid diagnostic")
    .redacted()
}

impl Default for DurableTransactionalEvaluator {
    fn default() -> Self {
        Self::new("main", EvaluatorLimits::default())
    }
}

impl DurableTransactionalEvaluator {
    /// Admits the bounded finite-list stream shape without opening runtime
    /// state or beginning delivery. Callers must use an explicit execution
    /// entry point to acquire a lease, poll the source, and invoke callbacks.
    pub fn admit_list_stream_source(&self, unit: &SourceUnit) -> StageOutcome<Diagnostic> {
        match admit_list_stream_source(unit, self.limits, &self.entry, &self.catalogue) {
            Ok(_) => StageOutcome::Passed,
            Err(outcome) => *outcome,
        }
    }

    /// Runs the deliberately narrow declarative finite-list bridge through the
    /// durable stream runner. It admits one literal `Stream.from_list`, one
    /// explicitly keyed table, and one `for_each` insert body; it does not
    /// interpret arbitrary stream expressions or administrative APIs.
    pub async fn execute_list_stream_source(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        unit: &SourceUnit,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let mut bridge =
            match admit_list_stream_source(unit, self.limits, &self.entry, &self.catalogue) {
                Ok(bridge) => bridge,
                Err(outcome) => return Ok(*outcome),
            };
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let writer = state.acquire_lease(owner_id).await?;
        let key = bridge.checkpoint_key()?;
        let mut source = ListStreamSource::new(key.clone(), std::mem::take(&mut bridge.payloads));
        let mut handler = ListTableHandler::new(bridge, self.limits);
        match state
            .run_stream(
                writer,
                &key,
                &mut source,
                &mut handler,
                &orna_runtime::NeverCancelled,
            )
            .await
        {
            Ok(StreamRunOutcome::Exhausted { .. }) => Ok(StageOutcome::Passed),
            Ok(StreamRunOutcome::Failed { failure, .. }) => Ok(StageOutcome::Failed(
                stream_failure_diagnostic(&mut handler, &failure),
            )),
            Ok(_) => Ok(StageOutcome::Skipped {
                reason: "literal list stream did not exhaust".into(),
            }),
            Err(error) => Ok(StageOutcome::Failed(stream_error_diagnostic(error))),
        }
    }
}
