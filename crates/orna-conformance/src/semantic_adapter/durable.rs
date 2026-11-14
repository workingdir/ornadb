use super::*;

/// The durable repository, runtime identity, and writer context for execution.
#[derive(Clone, Copy)]
pub struct RuntimeTarget<'a> {
    /// Repository containing the durable runtime state.
    pub repository: &'a Repository,
    /// Runtime identity whose state is opened for execution.
    pub identity: RuntimeIdentity,
    /// Writer identity used to acquire the runtime lease.
    pub owner_id: [u8; 16],
    /// Generation digest used when initializing runtime state.
    pub initial_digest: [u8; 32],
}

/// A source evaluator whose successful root activation is committed to the
/// durable runtime and whose next invocation rehydrates from that state.
pub struct DurableTransactionalEvaluator {
    pub(super) entry: String,
    pub(super) limits: EvaluatorLimits,
    pub(super) sys_host: Option<std::sync::Arc<SysHostAttachment>>,
    pub(super) catalogue: std::sync::Arc<Catalogue>,
    #[cfg(test)]
    pub(super) activation_time_observer:
        Option<std::sync::Arc<std::sync::Mutex<Option<SystemTime>>>>,
}

/// Runtime-adapter evidence returned by the bounded FAIL-001 witness.
///
/// The witness keeps one delivery identity for every retry and verifies the
/// same durable record, its private-table cardinality, and redacted payload
/// metadata before and after reopening. The row count is non-normative
/// runtime evidence, not a public `sys.Failure` projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fail001Witness {
    pub attempts: u32,
    pub durable_failure_rows: u64,
    pub single_failure_identity: bool,
    pub checkpoint_unchanged: bool,
    pub reopened: bool,
}

/// Runtime-adapter evidence returned by the bounded CP-001 witness.
///
/// The witnesses use the durable list-stream delivery boundary with the
/// immutable scenario's opaque `41` checkpoint and `42` item. They are not
/// compiler-produced engine evidence and do not claim the public
/// `sys.Checkpoint` projection or publication contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointAtomicityWitness {
    pub faulted_commit_rolls_back_rows_and_checkpoint: bool,
    pub rows_and_checkpoint_commit_together: bool,
    pub complete_committed_email_row: bool,
}

/// The outcome of attempting to continue an already-admitted table request.
///
/// `Committed` records controlled table writes and the supplied terminal in
/// one durable transaction. `NoMutation` is a successfully evaluated
/// read-only request; it deliberately leaves terminal handling to its
/// dedicated route rather than claiming table-activation proof.
///
/// A caller may retain a correlated protocol failure only for `Semantic`.
/// `Fenced` means durable request ownership, identity, capture, or terminal
/// state prevented this attempt from proceeding. The caller must re-read the
/// durable request instead of synthesizing a replacement terminal.
#[derive(Debug)]
pub enum RunningTableRequestDisposition {
    Committed,
    NoMutation,
    Semantic(StageOutcome<Diagnostic>),
    Fenced(RuntimeError),
}

/// Why [`DurableTransactionalEvaluator::publish_pending`] published nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingPublicationError {
    /// `HEAD` is a format-1/2 snapshot. It keeps its readers but has no row
    /// writer, so the pending prefix was not frozen (ORNA-UPGRADE-003).
    LegacyReadOnly { format: u8 },
    /// The runtime, the repository or the publication boundary failed.
    Runtime(RuntimeError),
}

impl std::fmt::Display for PendingPublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyReadOnly { format } => write!(
                f,
                "repository format {format} is a read-only legacy snapshot without a row writer"
            ),
            Self::Runtime(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PendingPublicationError {}

impl DurableTransactionalEvaluator {
    /// Creates an evaluator that admits transactions against the authoritative
    /// core catalogue. Project runs attach the project's production catalogue
    /// with [`Self::with_catalogue`].
    #[must_use]
    pub fn new(entry: impl Into<String>, limits: EvaluatorLimits) -> Self {
        Self {
            entry: entry.into(),
            limits,
            sys_host: None,
            catalogue: std::sync::Arc::new(Catalogue::authoritative_core()),
            #[cfg(test)]
            activation_time_observer: None,
        }
    }

    /// Admits transactions against `catalogue`, the production catalogue of the
    /// project being run. Callers attach it before admitting any project.
    #[must_use]
    pub fn with_catalogue(mut self, catalogue: std::sync::Arc<Catalogue>) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Attaches the repository and activation facts that `sys.rt.info()` reads
    /// during every table activation this evaluator runs.
    #[must_use]
    pub fn with_sys_host_context(mut self, sys_host: SysHostContext) -> Self {
        self.sys_host = Some(std::sync::Arc::new(SysHostAttachment::new(sys_host)));
        self
    }

    /// Reports whether a project root is admitted by the same bounded
    /// finite-list stream seam used by [`Self::execute_project_stream`].
    ///
    /// This probe does not open runtime state or perform delivery. It exists
    /// for callers that must choose the stream route before invoking the
    /// transactional route; unsupported transactions must not be inferred from
    /// an evaluator failure.
    #[must_use]
    pub fn project_stream_root_admitted(&self, project: &ProjectUnit, root_entry: &str) -> bool {
        if !root_entry.contains('.') {
            return false;
        }
        let admitted =
            match admit_transaction_project(project, self.limits, root_entry, &self.catalogue) {
                Ok(value) => value,
                Err(_) => return false,
            };
        admit_project_list_stream(
            project,
            &admitted,
            root_entry,
            RuntimeIdentity {
                database_id: [0; 16],
                repository_id: [0; 16],
            },
        )
        .is_ok()
    }

    #[cfg(test)]
    pub(super) fn record_activation_time(&self, activation_time: SystemTime) {
        if let Some(observer) = &self.activation_time_observer {
            *observer.lock().expect("activation observer lock") = Some(activation_time);
        }
    }

    /// Only tests observe the activation instant.
    #[cfg(not(test))]
    pub(super) fn record_activation_time(&self, _: SystemTime) {}

    /// Executes one admitted source activation against durable committed rows.
    /// Failed source evaluation never reaches the durable commit boundary.
    /// Zero-scope: this entry point carries no request identity, so its table mutation
    /// ids stay content-addressed. The CLI never reaches it; conformance and tests do.
    pub async fn execute_source(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        unit: &SourceUnit,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let admitted =
            match admit_transaction_source(unit, self.limits, &self.entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => return Ok(*outcome),
            };
        if admitted
            .functions
            .get(&self.entry)
            .is_some_and(|function| literal_stream_pipeline(&function.body).is_some())
        {
            return self
                .execute_list_stream_source(repository, identity, owner_id, initial_digest, unit)
                .await;
        }
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let lease = state.acquire_lease(owner_id).await?;
        let tables = admitted
            .key_fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let snapshot = state.begin_table_activation(&tables).await?;
        let context = snapshot.context();
        self.record_activation_time(context.activation_time());
        let mut evaluator = TransactionalEvaluator::new(&self.entry, self.limits)
            .with_sys_host_context(self.sys_host.clone())
            .with_activation_time(context.activation_time());
        for (table, rows) in snapshot.table_rows() {
            for (key, row) in rows {
                let row = Value::decode(row).map_err(|_| RuntimeError::RecoveryInvalid)?;
                evaluator.seed_committed_from_admission(
                    table.clone(),
                    key.clone(),
                    row,
                    &admitted.key_fields,
                    &BTreeMap::new(),
                    &admitted.table_fields,
                )?;
            }
        }
        let mutations = match evaluator.execute_admitted(&admitted) {
            Ok(mutations) => mutations,
            Err(diagnostic) => return Ok(StageOutcome::Failed(*diagnostic)),
        };
        if mutations.is_empty() {
            return Ok(StageOutcome::Passed);
        }
        let next_digest =
            durable_activation_digest(context.capture().generation_digest(), &mutations);
        let cwd_generation = activation_cwd_generation(repository, context)?;
        let mut validator = TransactionalTableCandidateValidator::new(
            Some(cwd_generation.clone()),
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
            &admitted.table_assertions,
            &admitted.module_assertions,
            Arc::new(BTreeMap::new()),
            self.limits,
        );
        match state
            .commit_validated_table_activation(ValidatedTableActivationCommit {
                writer: lease,
                context,
                cwd_generation: Some(&cwd_generation),
                expected_schema: None,
                mutations: &mutations,
                content_pins: &mut [],
                next_digest,
                validator: &mut validator,
                faults: &NoFault,
            })
            .await
        {
            Ok(_) => {}
            Err(TableActivationError::Runtime(error)) => return Err(error),
            Err(TableActivationError::ValidationFailed(_)) => {
                return Ok(StageOutcome::Failed(
                    transaction_error("ORNA-EVAL-TABLE-ASSERT")
                        .diagnostic()
                        .clone(),
                ));
            }
        }
        Ok(StageOutcome::Passed)
    }

    /// Executes an admitted source activation as one durable REQUEST-1
    /// activation. A successful nonempty table batch, its capture, and the
    /// canonical terminal result commit together under the request owner.
    ///
    /// Admission precedes writer acquisition: a matching terminal request
    /// replays without contending for a new writer lease. A matching
    /// nonterminal request never permits source execution. If writer
    /// acquisition, owner-fenced terminalization, or durable storage itself
    /// fails, the reservation is deliberately left for runtime recovery; a
    /// retry sees that nonterminal record and remains fenced rather than
    /// executing source a second time. This boundary has no cancellation
    /// input, so it never translates a failure into cancellation.
    ///
    /// This is intentionally separate from [`Self::execute_source`]: callers
    /// that do not carry a protocol request identity retain the established
    /// request-free activation API.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_source_request(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        request: RequestIdentity,
        fingerprint: [u8; 32],
        unit: &SourceUnit,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        self.execute_source_request_with_success_terminal(
            repository,
            identity,
            owner_id,
            initial_digest,
            request,
            fingerprint,
            unit,
            None,
        )
        .await
    }

    /// Executes a request-bound source activation while retaining a caller's
    /// already validated protocol success response atomically with table
    /// mutations. The caller owns the response encoding; this adapter only
    /// accepts it at the runtime's owner-fenced commit boundary.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_source_request_with_success_terminal(
        &self,
        repository: &Repository,
        identity: RuntimeIdentity,
        owner_id: [u8; 16],
        initial_digest: [u8; 32],
        request: RequestIdentity,
        fingerprint: [u8; 32],
        unit: &SourceUnit,
        success_terminal: Option<TerminalOutcome>,
    ) -> Result<StageOutcome<Diagnostic>, RuntimeError> {
        let state = RuntimeState::open(repository, identity, initial_digest).await?;
        let registration = RunObservationRegistration {
            request,
            consumer_identity: request_consumer_identity(),
            function: self.entry.clone(),
            source_identity: Some(unit.source_id.clone()),
            // A request identity names this invocation before any evaluator
            // work. The runtime stores it atomically with admission and its
            // pinned capture.
            invocation_id: request.request_id,
        };

        if let Some(status) = state.request_status(request, fingerprint).await? {
            return replay_or_fence_request(status);
        }
        let lease = match state.acquire_lease(owner_id).await {
            Ok(lease) => lease,
            // A terminal may have been inserted after the read above and
            // before lease acquisition. Re-read before reporting contention
            // so a completed request still replays without source execution.
            Err(RuntimeError::LeaseHeld) => match state.request_status(request, fingerprint).await?
            {
                Some(status) => return replay_or_fence_request(status),
                None => return Err(RuntimeError::LeaseHeld),
            },
            Err(error) => return Err(error),
        };
        let start = state
            .begin_observed_request(registration, fingerprint, lease)
            .await?;
        if !start.admitted {
            return replay_or_fence_request(start.request);
        }

        let admitted =
            match admit_transaction_source(unit, self.limits, &self.entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => {
                    let outcome = *outcome;
                    return terminalize_observed_outcome(
                        &state,
                        request,
                        fingerprint,
                        lease,
                        outcome,
                    )
                    .await;
                }
            };
        if admitted
            .functions
            .get(&self.entry)
            .is_some_and(|function| literal_stream_pipeline(&function.body).is_some())
        {
            let outcome = StageOutcome::Skipped {
                reason: "request-bound table execution does not admit stream roots".into(),
            };
            return terminalize_observed_outcome(&state, request, fingerprint, lease, outcome)
                .await;
        }

        let tables = admitted
            .key_fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let snapshot = match state.begin_table_activation(&tables).await {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return fail_observed_request_runtime(&state, request, fingerprint, lease).await;
            }
        };
        let context = snapshot.context();
        self.record_activation_time(context.activation_time());
        let mut evaluator = TransactionalEvaluator::new(&self.entry, self.limits)
            .with_sys_host_context(self.sys_host.clone())
            .with_activation_time(context.activation_time())
            .with_mutation_scope(request_mutation_scope(&request));
        for (table, rows) in snapshot.table_rows() {
            for (key, row) in rows {
                let row = match Value::decode(row) {
                    Ok(row) => row,
                    Err(_) => {
                        return fail_observed_request_runtime(&state, request, fingerprint, lease)
                            .await;
                    }
                };
                if evaluator
                    .seed_committed_from_admission(
                        table.clone(),
                        key.clone(),
                        row,
                        &admitted.key_fields,
                        &BTreeMap::new(),
                        &admitted.table_fields,
                    )
                    .is_err()
                {
                    return fail_observed_request_runtime(&state, request, fingerprint, lease)
                        .await;
                }
            }
        }
        let mutations = match evaluator.execute_admitted(&admitted) {
            Ok(mutations) => mutations,
            Err(diagnostic) => {
                let outcome = StageOutcome::Failed(*diagnostic);
                return fail_observed_request_outcome(&state, request, fingerprint, lease, outcome)
                    .await;
            }
        };
        if mutations.is_empty() {
            let outcome = StageOutcome::Passed;
            return terminalize_observed_outcome(&state, request, fingerprint, lease, outcome)
                .await;
        }
        let next_digest =
            durable_activation_digest(context.capture().generation_digest(), &mutations);
        let cwd_generation = activation_cwd_generation(repository, context)?;
        let mut validator = TransactionalTableCandidateValidator::new(
            Some(cwd_generation.clone()),
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
            &admitted.table_assertions,
            &admitted.module_assertions,
            Arc::new(BTreeMap::new()),
            self.limits,
        );
        match state
            .commit_validated_table_request_activation(ValidatedTableRequestActivationCommit {
                writer: lease,
                identity: request,
                fingerprint,
                context,
                mutations: &mutations,
                next_digest,
                outcome: success_terminal.unwrap_or(request_terminal(&StageOutcome::Passed)?),
                validator: &mut validator,
                faults: &NoFault,
                effects: declared_effect_report(self.sys_host.as_deref()),
            })
            .await
        {
            Ok(_) => {}
            Err(TableActivationError::Runtime(error)) => return Err(error),
            Err(TableActivationError::ValidationFailed(_)) => {
                let outcome = StageOutcome::Failed(
                    transaction_error("ORNA-EVAL-TABLE-ASSERT")
                        .diagnostic()
                        .clone(),
                );
                return fail_observed_request_outcome(&state, request, fingerprint, lease, outcome)
                    .await;
            }
        }
        Ok(StageOutcome::Passed)
    }

    /// Continues one already-admitted, owner-fenced table request.
    ///
    /// This entry point deliberately has no admission inputs. The supplied
    /// continuation is revalidated against `state` before parsing or
    /// evaluation, then its identity, fingerprint, writer lease, and pinned
    /// activation context are passed unchanged to the runtime's atomic table
    /// commit boundary. It neither reserves nor starts a request, and it does
    /// not acquire a replacement lease.
    ///
    /// Only the controlled table-mutation evaluator is admitted here. Stream
    /// roots and all source forms rejected by the retained transactional
    /// admission path return their original `StageOutcome` before a table
    /// snapshot is read or a mutation is staged. A caller-provided success
    /// terminal remains opaque and is retained byte-for-byte; this adapter
    /// never synthesizes rollback proof.
    pub async fn execute_running_table_request_with_success_terminal(
        &self,
        repository: &Repository,
        state: &RuntimeState,
        continuation: RunningTableRequestContinuation,
        unit: &SourceUnit,
        success_terminal: TerminalOutcome,
    ) -> RunningTableRequestDisposition {
        self.execute_running_table_request_with_success_terminal_with_faults(
            repository,
            state,
            continuation,
            unit,
            success_terminal,
            &NoFault,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_running_table_request_with_success_terminal_with_faults(
        &self,
        repository: &Repository,
        state: &RuntimeState,
        continuation: RunningTableRequestContinuation,
        unit: &SourceUnit,
        success_terminal: TerminalOutcome,
        faults: &dyn FaultInjector,
    ) -> RunningTableRequestDisposition {
        // Reconstruct the capability from the durable Running request before
        // source work. In particular, a completed, foreign, stale, or
        // owner-mismatched capability cannot cause a second evaluation.
        let continuation = match state
            .continue_running_table_request(
                continuation.identity(),
                continuation.fingerprint(),
                continuation.writer_lease(),
            )
            .await
        {
            Ok(continuation) => continuation,
            Err(error) => return RunningTableRequestDisposition::Fenced(error),
        };

        let admitted =
            match admit_transaction_source(unit, self.limits, &self.entry, &self.catalogue) {
                Ok(value) => value,
                Err(outcome) => return RunningTableRequestDisposition::Semantic(*outcome),
            };
        if admitted
            .functions
            .get(&self.entry)
            .is_some_and(|function| literal_stream_pipeline(&function.body).is_some())
        {
            return RunningTableRequestDisposition::Semantic(StageOutcome::Skipped {
                reason: "running table request continuation does not admit stream roots".into(),
            });
        }

        let tables = admitted
            .key_fields
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let snapshot = match state.begin_table_activation(&tables).await {
            Ok(snapshot) => snapshot,
            Err(error) => return RunningTableRequestDisposition::Fenced(error),
        };
        if snapshot.context().capture() != continuation.context().capture() {
            return RunningTableRequestDisposition::Fenced(RuntimeError::StaleCapture {
                current: Box::new(snapshot.context().capture().clone()),
            });
        }
        let mut evaluator = TransactionalEvaluator::new(&self.entry, self.limits)
            .with_sys_host_context(self.sys_host.clone())
            .with_activation_time(continuation.context().activation_time())
            .with_mutation_scope(request_mutation_scope(&continuation.identity()));
        for (table, rows) in snapshot.table_rows() {
            for (key, row) in rows {
                let row = match Value::decode(row) {
                    Ok(row) => row,
                    Err(_) => {
                        return RunningTableRequestDisposition::Fenced(
                            RuntimeError::RecoveryInvalid,
                        );
                    }
                };
                if let Err(error) = evaluator.seed_committed_from_admission(
                    table.clone(),
                    key.clone(),
                    row,
                    &admitted.key_fields,
                    &BTreeMap::new(),
                    &admitted.table_fields,
                ) {
                    return RunningTableRequestDisposition::Fenced(error);
                }
            }
        }
        let mutations = match evaluator.execute_admitted(&admitted) {
            Ok(mutations) => mutations,
            Err(diagnostic) => {
                return RunningTableRequestDisposition::Semantic(StageOutcome::Failed(*diagnostic));
            }
        };
        // The atomic request activation boundary intentionally rejects an
        // empty batch. A pure result must use its dedicated terminal route;
        // accepting it here would claim table-activation proof without a
        // controlled table write.
        if mutations.is_empty() {
            return RunningTableRequestDisposition::NoMutation;
        }
        let next_digest = durable_activation_digest(
            continuation.context().capture().generation_digest(),
            &mutations,
        );
        let cwd_generation = match activation_cwd_generation(repository, continuation.context()) {
            Ok(generation) => generation,
            Err(error) => return RunningTableRequestDisposition::Fenced(error),
        };
        let mut validator = TransactionalTableCandidateValidator::new(
            Some(cwd_generation.clone()),
            &admitted.functions,
            &admitted.key_fields,
            &admitted.float_fields,
            &admitted.table_fields,
            &admitted.table_assertions,
            &admitted.module_assertions,
            Arc::new(BTreeMap::new()),
            self.limits,
        );
        match state
            .commit_validated_table_request_activation(ValidatedTableRequestActivationCommit {
                writer: continuation.writer_lease(),
                identity: continuation.identity(),
                fingerprint: continuation.fingerprint(),
                context: continuation.context(),
                mutations: &mutations,
                next_digest,
                outcome: success_terminal,
                validator: &mut validator,
                faults,
                effects: declared_effect_report(self.sys_host.as_deref()),
            })
            .await
        {
            Ok(_) => RunningTableRequestDisposition::Committed,
            Err(TableActivationError::Runtime(error)) => {
                RunningTableRequestDisposition::Fenced(error)
            }
            Err(TableActivationError::ValidationFailed(_)) => {
                let outcome = StageOutcome::Failed(
                    transaction_error("ORNA-EVAL-TABLE-ASSERT")
                        .diagnostic()
                        .clone(),
                );
                match fail_observed_request_outcome(
                    state,
                    continuation.identity(),
                    continuation.fingerprint(),
                    continuation.writer_lease(),
                    outcome.clone(),
                )
                .await
                {
                    Ok(_) => RunningTableRequestDisposition::Semantic(outcome),
                    Err(error) => RunningTableRequestDisposition::Fenced(error),
                }
            }
        }
    }
}
