#[cfg(test)]
use super::repl_request::FIRST_SYS_DISPATCH_RUNNING;
use super::{
    AdmittedReplSession, ApplicationError, Arc, BufReader, CanonicalValue, ColorMode, Diagnostic,
    Endpoint, Limits, PinnedCommittedRows, QueryObservationRegistration, Rc, RefCell,
    ReplEvaluationFailure, ReplSnapshotSessionLoader, ReplTransactionScope, ReplTransactions,
    ReplWriterInputs, RequestIdentity, Reserved, StageOutcome, StagedTableActivation,
    TableActivationError, ValidatedTableRequestActivationCommit, committed_rows_at,
    diagnostic_documentation, ensure_writer, io, project_declares_assertions, repl,
    repl_effect_failure, repl_request_fingerprint, repl_session, request_nonce, runtime_identity,
    transactional_loaded_project_candidate_validator,
};

#[allow(clippy::too_many_lines)]
pub(crate) fn run_repl_input(
    repository: Option<&orna_repository::Repository>,
    transactions: Option<&mut ReplTransactions>,
    session: &mut AdmittedReplSession,
    source: &str,
) -> Result<Option<CanonicalValue>, repl::ReplFailure> {
    let Ok(staged) = session.stage_activation(source) else {
        return session.submit(source).map_err(repl::ReplFailure::from);
    };
    let (Some(repository), Some(transactions)) = (repository, transactions) else {
        return Err(repl::ReplFailure::from(
            orna_project_repl::effect_rejection(staged.effects()),
        ));
    };
    let (current_commit, _) = repository
        .open_pinned_format_context_with_commit("HEAD")
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    if current_commit != transactions.commit {
        return Err(repl_effect_failure(
            "the project changed since this REPL session started",
            "restart the REPL to bind to the current HEAD",
        ));
    }
    if !transactions.format_context.supports_writes() {
        return Err(repl_effect_failure(
            "this repository's format does not accept REPL writes",
            "open a repository whose format supports writes, then restart the REPL",
        ));
    }
    let writer = ensure_writer(
        &mut transactions.writer,
        &ReplWriterInputs {
            repository,
            commit: &transactions.commit,
            format_context: &transactions.format_context,
            runtime: &transactions.runtime,
            tables: &transactions.tables,
            capture_relation_id: transactions.capture_relation_id,
            identity: transactions.identity,
            initial_digest: transactions.initial_digest,
            session_id: transactions.session_id,
            host_bindings: &transactions.host_bindings,
        },
    )?;
    let state = &writer.state;
    let lease = writer.lease;
    let mut host_bindings = transactions.host_bindings.borrow_mut();
    let bindings = host_bindings
        .as_mut()
        .ok_or_else(|| repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned()))?;
    // The runtime doc for `begin_admitted_table_activation_over_committed`
    // requires the pin to be read before the rows it binds.
    let pin = transactions
        .runtime
        .block_on(state.published_pin())
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let (committed_rows, storage_orders) = committed_rows_at(
        repository,
        &transactions.commit,
        &transactions.table_schemas,
        &transactions.tables,
    )
    .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let committed =
        PinnedCommittedRows::new(pin, committed_rows).map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let snapshot = transactions
        .runtime
        .block_on(
            state.begin_admitted_table_activation_over_committed(&transactions.tables, committed),
        )
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let query_id = request_nonce().to_be_bytes();
    let expression =
        (source.len() <= 16 * 1024 && !source.is_empty() && !source.as_bytes().contains(&0))
            .then(|| source.to_owned());
    let request = RequestIdentity {
        session_id: transactions.session_id,
        request_id: query_id,
    };
    #[cfg(test)]
    let observe_first_dispatch = || {
        FIRST_SYS_DISPATCH_RUNNING.with(|first| {
            if first.get().is_none() {
                let running = transactions
                    .runtime
                    .block_on(state.running_requests())
                    .expect("running requests at first sys dispatch");
                first.set(Some(running.len()));
            }
        });
    };
    let scope = ReplTransactionScope::new(
        &transactions.project,
        repository,
        &transactions.format_context,
        &snapshot,
        &storage_orders,
        request,
    );
    #[cfg(test)]
    let scope = scope.with_dispatch_observer(&observe_first_dispatch);
    transactions
        .runtime
        .block_on(state.begin_query_observation(
            lease,
            QueryObservationRegistration {
                id: query_id,
                expression,
                capture: snapshot.context().capture().clone(),
            },
        ))
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    // REQUEST-1 (Orna-1.1.0 30-protocol.md:104): the request row is reserved
    // and started before evaluation, so an external sys effect dispatched while
    // the input evaluates is covered by a durable Running row. A panic between
    // reservation and settlement leaves that row Running, which is the same
    // state as a crash and is settled by fenced recovery.
    let fingerprint = repl_request_fingerprint(
        transactions.session_id,
        query_id,
        source,
        transactions.initial_digest,
    );
    let (reserved, admission) = Reserved::reserve(
        &transactions.runtime,
        state,
        lease,
        request,
        fingerprint,
        query_id,
    )?;
    let mut running = reserved.start(admission)?;
    let evaluation = transactions
        .authority
        .evaluate_staged_repl_transaction(session, staged, &scope, bindings);
    let (value, successor, activation, external_dispatched) = match evaluation {
        Ok(result) => result,
        Err(failure) => {
            if failure.external_effects_possible {
                // The row stays Running: fenced recovery settles it and the
                // input is never replayed.
                return Err(running.leave_running(repl_evaluation_failure(failure)));
            }
            return Err(running.abandon(repl_evaluation_failure(failure)));
        }
    };
    running.record_external_dispatch(external_dispatched);
    if activation.mutations().is_empty() {
        // Read-only results are retained: the request completes with its
        // terminal outcome instead of being left running.
        running.settle_read()?;
        *session = successor;
        return Ok(value);
    }
    let Ok(activation) =
        activation.stage_for_request(&transactions.authority, snapshot.context(), request)
    else {
        return Err(running.unsettled());
    };
    // The application stages with NoFault; rebuild the identical unit carrying
    // the runtime's injector so the request boundary owns the fault seam.
    let Ok(request_staged) = StagedTableActivation::from_source(
        activation.context().clone(),
        activation.mutations().to_vec(),
        activation.next_digest(),
        Arc::clone(&transactions.faults),
        activation.effects(),
    ) else {
        return Err(running.unsettled());
    };
    let Some(outcome) = running.terminal(b"orna.repl.commit.v1") else {
        return Err(running.unsettled());
    };
    // ORNA-ASSERT-042: a project that declares assertions checks the
    // post-mutation candidate against them before any row, checkpoint or
    // terminal is published. The request-scoped activation publishes the
    // request identity, fingerprint and terminal outcome in the same step as
    // the rows. A project without assertions has nothing to check, so its
    // commit keeps the unvalidated activation path.
    if project_declares_assertions(&transactions.project) {
        let cwd_generation = u64::try_from(request_staged.context().capture().generation())
            .ok()
            .and_then(|runtime| {
                repository
                    .cwd_generation(orna_repository::RuntimeGeneration::new(runtime))
                    .ok()
            });
        let Some(cwd_generation) = cwd_generation else {
            return Err(running.unsettled());
        };
        let mut validator = match transactional_loaded_project_candidate_validator(
            &transactions.project,
            &transactions.identities,
            Some(cwd_generation),
            Arc::clone(&transactions.table_object_ids),
            Limits::default(),
        ) {
            Ok(validator) => validator,
            Err(outcome) => {
                return Err(running.unsettled_because(stage_outcome_cause(&outcome)));
            }
        };
        let committed =
            transactions
                .runtime
                .block_on(state.commit_validated_table_request_activation(
                    ValidatedTableRequestActivationCommit {
                        writer: running.writer(),
                        identity: running.identity(),
                        fingerprint: running.fingerprint(),
                        context: request_staged.context(),
                        mutations: request_staged.mutations(),
                        next_digest: request_staged.next_digest(),
                        outcome,
                        validator: &mut *validator,
                        faults: request_staged.faults(),
                        effects: request_staged.effects(),
                    },
                ));
        match committed {
            Ok(_) => {}
            // Validation runs before any publication, so a failed validation
            // leaves the input uncommitted. It is cancelled unless an external
            // binding was actually dispatched during evaluation. Only a false
            // assertion is ORNA-A091-009; other validator failures keep their
            // diagnostic in the detail.
            Err(TableActivationError::ValidationFailed(diagnostic))
                if !running.external_dispatched =>
            {
                let failure = if orna_conformance::is_table_assertion_false(diagnostic) {
                    repl::ReplFailure::from("ORNA-A091-009".to_owned())
                } else {
                    repl_effect_cancelled(format!(
                        "table assertion validation did not complete: {diagnostic:?}"
                    ))
                };
                return Err(running.abandon(failure));
            }
            Err(error) => {
                return Err(running.commit_failed(format!(
                    "table commit failed with {}",
                    table_activation_cause(&error)
                )));
            }
        }
    } else {
        let committed =
            transactions
                .runtime
                .block_on(state.commit_staged_table_request_activation(
                    running.writer(),
                    running.identity(),
                    running.fingerprint(),
                    &request_staged,
                    outcome,
                ));
        if let Err(error) = committed {
            return Err(running.commit_failed(format!("table commit failed with {error}")));
        }
    }
    running.record_committed();
    *session = successor;
    Ok(value)
}

/// Reported when external effects may already have happened after evaluation
/// and no terminal outcome is proven. The request row stays Running for fenced
/// recovery; the input is never replayed.
pub(crate) const REPL_EFFECT_UNCERTAIN: &str = "ORNA-REPL-EFFECT-UNCERTAIN";

pub(crate) fn repl_application_error(error: ApplicationError) -> String {
    match error {
        ApplicationError::Parse(codes) | ApplicationError::Semantic(codes) => codes
            .into_iter()
            .next()
            .unwrap_or_else(|| "ORNA-REPL-SEMANTIC".to_owned()),
        ApplicationError::Evaluation(code) | ApplicationError::EffectRejected(code) => code,
        ApplicationError::SourceEffectFailed(code) => code.as_str().to_owned(),
        ApplicationError::UnadmittedTable(_) => "ORNA-EVAL-TABLE-UNADMITTED".to_owned(),
        _ => "ORNA-REPL-EFFECT".to_owned(),
    }
}

/// Maps a failed REPL evaluation to its submission failure. When external
/// effects may already have happened, the code stays
/// `ORNA-REPL-EFFECT-UNCERTAIN` and the evaluator cause moves to the detail,
/// so the uncertain outcome stays diagnosable without a replay.
pub(crate) fn repl_evaluation_failure(failure: ReplEvaluationFailure) -> repl::ReplFailure {
    let cause = repl_application_error(failure.error);
    if !failure.external_effects_possible {
        return repl::ReplFailure::from(cause);
    }
    repl_effect_uncertain(format!(
        "external effects may already have happened after evaluation failed with {cause}"
    ))
}

/// Submission failure for an input whose external effects may already have
/// happened. The code stays `ORNA-REPL-EFFECT-UNCERTAIN`; `message` names the
/// cause that stopped settlement, so the outcome is diagnosable without replay.
pub(crate) fn repl_effect_uncertain(message: String) -> repl::ReplFailure {
    repl::ReplFailure::located(
        REPL_EFFECT_UNCERTAIN,
        message,
        vec![
            "the input is not replayed; the request row stays running for fenced recovery"
                .to_owned(),
        ],
    )
}

/// Submission failure for an input that was not published and is cancelled.
/// `message` names the cause so the failure stays diagnosable.
pub(crate) fn repl_effect_cancelled(message: String) -> repl::ReplFailure {
    repl::ReplFailure::located("ORNA-REPL-EFFECT", message, Vec::new())
}

/// The failure that stopped settlement, named for the uncertain detail.
pub(crate) fn settlement_cause(failure: &repl::ReplFailure) -> String {
    match &failure.detail {
        Some(detail) => format!("{}: {}", failure.code, detail.message),
        None => failure.code.clone(),
    }
}

/// The commit error's inner cause, for the uncertain detail.
pub(crate) fn table_activation_cause(error: &TableActivationError) -> String {
    match error {
        TableActivationError::Runtime(runtime) => runtime.to_string(),
        TableActivationError::ValidationFailed(diagnostic) => {
            if orna_conformance::is_table_assertion_false(*diagnostic) {
                "ORNA-A091-009".to_owned()
            } else {
                format!("{diagnostic:?}")
            }
        }
    }
}

/// The validator-build outcome's cause, for the unsettled detail.
pub(crate) fn stage_outcome_cause(outcome: &StageOutcome<orna_value::Diagnostic>) -> String {
    match outcome {
        StageOutcome::Failed(diagnostic) | StageOutcome::Cancelled(diagnostic) => {
            format!("{}: {}", diagnostic.code(), diagnostic.message())
        }
        StageOutcome::Skipped { reason } => reason.clone(),
        StageOutcome::Passed => "validator build produced no validator".to_owned(),
    }
}

pub(crate) fn run_repl_submission_with<W, S>(
    session: &mut AdmittedReplSession,
    source: &str,
    writer: &mut W,
    color_enabled: bool,
    submitter: &mut S,
) -> Result<(), Diagnostic>
where
    W: io::Write,
    S: FnMut(&mut AdmittedReplSession, &str) -> Result<Option<CanonicalValue>, repl::ReplFailure>,
{
    if source.trim() == ":quit" {
        return Ok(());
    }
    match submitter(session, source) {
        Ok(Some(value)) => {
            repl::write_rendered_value(writer, &repl::inspect(&value), color_enabled).map_err(
                |_| {
                    Diagnostic::target(
                        "E2200",
                        "REPL console I/O failed",
                        "check the terminal input and output streams, then start a new session",
                    )
                },
            )
        }
        Ok(None) => Ok(()),
        Err(failure) => {
            let body = failure
                .detail
                .is_some()
                .then(|| repl::render_repl_failure(&failure, source));
            Err(repl_failure_diagnostic(&failure.code, body))
        }
    }
}

/// The diagnostic an expression or console failure reports: the documented
/// title and help for `code`, with the located `body` when there is one.
pub(crate) fn repl_failure_diagnostic(code: &str, body: Option<String>) -> Diagnostic {
    let mut diagnostic = diagnostic_documentation(code).map_or_else(
        || {
            Diagnostic::unavailable(
                "REPL expression could not be evaluated",
                "revise the expression using names and values available in this REPL session",
            )
        },
        |documentation| {
            Diagnostic::target(documentation.code, documentation.title, documentation.help)
        },
    );
    diagnostic.body = body;
    diagnostic
}
pub(crate) fn run_repl(
    endpoint: &Endpoint,
    expression: Option<&str>,
    color_mode: ColorMode,
) -> Result<(), Diagnostic> {
    if matches!(endpoint, Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_)) {
        return Err(Diagnostic::target(
            "E2100",
            "remote REPL sessions are unavailable",
            "use a local Git worktree until the Orna transport is available",
        ));
    }
    if crate::repl_hir::enabled() {
        return crate::repl_hir::run_repl_hir(endpoint, expression, color_mode);
    }
    let mut runtime = repl_session(endpoint)?;
    let color_enabled = color_mode.stdout_enabled();
    let repository = runtime.repository.as_ref();
    let host_bindings = runtime.transactions.as_ref().map_or_else(
        || Rc::new(RefCell::new(None)),
        |transactions| transactions.host_bindings.clone(),
    );
    let mut transactions = runtime.transactions.as_mut();
    let mut submitter = |session: &mut AdmittedReplSession, source: &str| {
        run_repl_input(repository, transactions.as_deref_mut(), session, source)
    };
    if let Some(source) = expression {
        return run_repl_submission_with(
            &mut runtime.session,
            source,
            &mut io::stdout().lock(),
            color_enabled,
            &mut submitter,
        );
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let loader = ReplSnapshotSessionLoader {
        repository: runtime.repository.as_ref(),
        host_bindings,
    };
    // A watch observation names the runtime it observes, so the host is built
    // only when the session has a repository identity to pin it to.
    let watch_identity = runtime
        .repository
        .as_ref()
        .and_then(|repository| runtime_identity(repository).ok())
        .map(|(identity, _)| (identity.database_id, identity.repository_id));
    let mut watch = repl::ReplWatchHost::new(watch_identity, orna_protocol::Limits::default());
    let mut watch_state = repl::WatchCommandState::default();
    repl::run_with_loader_and_submitter(
        &mut BufReader::new(stdin.lock()),
        &mut stdout.lock(),
        &mut runtime.session,
        &loader,
        &mut submitter,
        (&mut watch, &mut watch_state),
        color_enabled,
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "REPL console I/O failed",
            "check the terminal input and output streams, then start a new session",
        )
    })
}
