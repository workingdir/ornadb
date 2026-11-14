use super::{
    EnvironmentProvider, REPL_EFFECT_UNCERTAIN, Rc, RefCell, RequestAdmissionCapability,
    RequestIdentity, RuntimeIdentity, RuntimeState, RuntimeTableIdentity, SysHostBindingRegistry,
    TerminalOutcome, WriterLease, legacy_bare_module_candidates, legacy_bare_module_table, repl,
    repl_effect_cancelled, repl_effect_failure, repl_effect_uncertain, repl_lease_failure,
    settlement_cause,
};
use orna_host_fs::FilesystemProvider;

#[cfg(test)]
thread_local! {
    /// Running request rows counted when the first sys binding dispatches.
    pub(crate) static FIRST_SYS_DISPATCH_RUNNING: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// The runtime state and writer lease one REPL session owns once its first
/// transaction runs. Both are opened once, so every later input settles under
/// the same owner fence instead of re-taking the lease.
pub(crate) struct ReplWriter {
    pub(crate) state: RuntimeState,
    pub(crate) lease: WriterLease,
}

/// The transaction fields `ensure_writer` reads. They are passed separately so
/// the caller can keep borrowing them after the writer is opened.
pub(crate) struct ReplWriterInputs<'a> {
    pub(crate) repository: &'a orna_repository::Repository,
    pub(crate) commit: &'a orna_repository::GitCommitRef,
    pub(crate) format_context: &'a orna_repository::RepositoryFormatContext,
    pub(crate) runtime: &'a tokio::runtime::Runtime,
    pub(crate) tables: &'a [RuntimeTableIdentity],
    pub(crate) capture_relation_id: [u8; 16],
    pub(crate) identity: RuntimeIdentity,
    pub(crate) initial_digest: [u8; 32],
    pub(crate) session_id: [u8; 16],
    pub(crate) host_bindings: &'a Rc<RefCell<Option<SysHostBindingRegistry>>>,
}

/// Opens the runtime state and takes the writer lease once, and initialises the
/// session's sys host bindings. Every failure keeps the session usable for a
/// later input, exactly as the previous per-input retry did.
pub(crate) fn ensure_writer<'a>(
    writer: &'a mut Option<ReplWriter>,
    inputs: &ReplWriterInputs<'_>,
) -> Result<&'a mut ReplWriter, repl::ReplFailure> {
    if writer.is_none() {
        let state = inputs
            .runtime
            .block_on(RuntimeState::open_with_owner_id(
                inputs.repository,
                inputs.identity,
                inputs.initial_digest,
                inputs.session_id,
            ))
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        let tables = inputs
            .tables
            .iter()
            .map(RuntimeTableIdentity::table)
            .collect::<Vec<_>>();
        let candidates =
            legacy_bare_module_candidates(inputs.repository, Some(inputs.commit), &tables)
                .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        if let Some(table) = legacy_bare_module_table(&state, inputs.runtime, &candidates)
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?
        {
            return Err(repl_effect_failure(
                &format!(
                    "module table `{table}` has rows stored under its bare name by an earlier build"
                ),
                "re-create the store from the committed project; rows under bare module names are not migrated",
            ));
        }
        ensure_host_bindings(inputs, &state)?;
        let lease = inputs
            .runtime
            .block_on(state.acquire_lease(orna_runtime::process_owner_id()))
            .map_err(|error| repl_lease_failure(&error, &state))?;
        *writer = Some(ReplWriter { state, lease });
    }
    let opened = writer
        .as_mut()
        .ok_or_else(|| repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned()))?;
    ensure_host_bindings(inputs, &opened.state)?;
    Ok(opened)
}

/// Initialises the session's sys host bindings from the opened runtime state.
/// A `:at` snapshot replacement clears them, so they are rebuilt on demand.
pub(crate) fn ensure_host_bindings(
    inputs: &ReplWriterInputs<'_>,
    state: &RuntimeState,
) -> Result<(), repl::ReplFailure> {
    let mut host_bindings = inputs.host_bindings.borrow_mut();
    if host_bindings.is_some() {
        return Ok(());
    }
    let mut filesystem =
        FilesystemProvider::with_limits(65_536, 16).map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    filesystem
        .allow_root(inputs.repository.worktree())
        .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
    let mut bindings = SysHostBindingRegistry::new(EnvironmentProvider::default())
        .with_filesystem_provider(filesystem);
    if state.holds_runtime_owner_lock() {
        let capability = inputs
            .format_context
            .capture_capability(inputs.capture_relation_id)
            .map_err(|_| "ORNA-REPL-EFFECT".to_owned())?;
        bindings = bindings.with_repository_capture_capability(capability);
    }
    *host_bindings = Some(bindings);
    Ok(())
}

/// One REPL request the runtime has durably reserved.
///
/// The request lifecycle is a typestate: a [`Reserved`] row that cannot start
/// is cancelled, a [`Running`] row is completed, cancelled, or left Running for
/// fenced recovery, and every settlement consumes the value. A request can
/// therefore be settled at most once, and the external-dispatch flag that
/// decides between cancellation and recovery travels with the Running row.
pub(crate) struct ReplRequest<'a> {
    pub(crate) runtime: &'a tokio::runtime::Runtime,
    pub(crate) state: &'a RuntimeState,
    pub(crate) identity: RequestIdentity,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) query_id: [u8; 16],
    pub(crate) writer: WriterLease,
}

/// A REQUEST-1 row that is durably reserved but not yet started.
pub(crate) struct Reserved<'a>(ReplRequest<'a>);

/// A REQUEST-1 row whose admission capability was consumed by a start.
pub(crate) struct Running<'a> {
    pub(crate) request: ReplRequest<'a>,
    /// Whether evaluation dispatched through an external sys binding. Such a
    /// request is left Running for fenced recovery and never cancelled.
    pub(crate) external_dispatched: bool,
}

impl ReplRequest<'_> {
    /// The request's terminal outcome, tagged with the operation settling it.
    pub(crate) fn terminal(&self, prefix: &'static [u8]) -> Option<TerminalOutcome> {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&self.query_id);
        TerminalOutcome::new(bytes).ok()
    }

    pub(crate) fn finish_observation(
        &self,
        status: orna_runtime::InvocationObservationStatus,
    ) -> Result<(), ()> {
        self.runtime
            .block_on(
                self.state
                    .finish_query_observation(self.writer, self.query_id, status),
            )
            .map(|_| ())
            .map_err(|_| ())
    }

    /// Cancels the row and reports `original`. A row whose input never reached
    /// an external binding is cancelled before its observation is recorded, so
    /// a failed observation write cannot strand it. A failed cancellation
    /// leaves the row unsettled, which is uncertain.
    pub(crate) fn abandon(self, original: repl::ReplFailure) -> repl::ReplFailure {
        let cancelled = self
            .terminal(b"orna.repl.cancel.v1")
            .is_some_and(|outcome| {
                self.runtime
                    .block_on(self.state.cancel_request_with_owner(
                        self.identity,
                        self.fingerprint,
                        self.writer,
                        outcome,
                    ))
                    .is_ok()
            });
        if self
            .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
            .is_err()
        {
            eprintln!("warning: query observation not recorded");
        }
        if cancelled {
            original
        } else {
            repl_effect_uncertain(settlement_cause(&original))
        }
    }
}

impl<'a> Reserved<'a> {
    /// Reserves the durable REQUEST-1 row and returns it with the admission
    /// capability its start consumes. A row this call did not insert is
    /// settled with a failed observation before the caller reports the code.
    pub(crate) fn reserve(
        runtime: &'a tokio::runtime::Runtime,
        state: &'a RuntimeState,
        writer: WriterLease,
        identity: RequestIdentity,
        fingerprint: [u8; 32],
        query_id: [u8; 16],
    ) -> Result<(Self, RequestAdmissionCapability), repl::ReplFailure> {
        let request = ReplRequest {
            runtime,
            state,
            identity,
            fingerprint,
            query_id,
            writer,
        };
        if let Ok((_, Some(admission))) =
            runtime.block_on(state.reserve_request_with_admission(identity, fingerprint))
        {
            Ok((Self(request), admission))
        } else {
            request
                .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
                .map_err(|()| repl::ReplFailure::from(REPL_EFFECT_UNCERTAIN.to_owned()))?;
            Err("ORNA-REPL-EFFECT".to_owned().into())
        }
    }

    /// Starts the reserved row. A failed start leaves it Reserved and no
    /// binding has run, so it is cancelled before the failure is reported.
    pub(crate) fn start(
        self,
        admission: RequestAdmissionCapability,
    ) -> Result<Running<'a>, repl::ReplFailure> {
        let started = self
            .0
            .runtime
            .block_on(self.0.state.start_request_with_owner_and_admission(
                self.0.identity,
                self.0.fingerprint,
                self.0.writer,
                admission,
            ));
        if started.is_err() {
            return Err(self.0.abandon("ORNA-REPL-EFFECT".to_owned().into()));
        }
        Ok(Running {
            request: self.0,
            external_dispatched: false,
        })
    }
}

impl Running<'_> {
    pub(crate) fn record_external_dispatch(&mut self, dispatched: bool) {
        self.external_dispatched |= dispatched;
    }

    pub(crate) fn identity(&self) -> RequestIdentity {
        self.request.identity
    }

    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        self.request.fingerprint
    }

    pub(crate) fn writer(&self) -> WriterLease {
        self.request.writer
    }

    /// The request's terminal outcome, tagged with the settling operation.
    pub(crate) fn terminal(&self, prefix: &'static [u8]) -> Option<TerminalOutcome> {
        self.request.terminal(prefix)
    }

    /// Leaves a started request Running for fenced recovery after evaluation
    /// failed with an external effect possible. The input is never replayed.
    pub(crate) fn leave_running(self, failure: repl::ReplFailure) -> repl::ReplFailure {
        if self
            .request
            .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
            .is_err()
        {
            return REPL_EFFECT_UNCERTAIN.to_owned().into();
        }
        failure
    }

    /// Completes a read-only input with its terminal outcome. Read-only
    /// results are retained, so the request never stays Running.
    pub(crate) fn settle_read(self) -> Result<(), repl::ReplFailure> {
        let completed = self
            .request
            .terminal(b"orna.repl.read.v1")
            .is_some_and(|outcome| {
                self.request
                    .runtime
                    .block_on(self.request.state.complete_request_with_owner(
                        self.request.identity,
                        self.request.fingerprint,
                        self.request.writer,
                        outcome,
                    ))
                    .is_ok()
            });
        if !completed {
            return Err(self.unsettled());
        }
        self.record_committed();
        Ok(())
    }

    /// Failure after evaluation: uncertain when an external binding was
    /// dispatched (the row stays Running for fenced recovery); otherwise
    /// cancelled.
    pub(crate) fn unsettled(self) -> repl::ReplFailure {
        if self.external_dispatched {
            let _ = self
                .request
                .finish_observation(orna_runtime::InvocationObservationStatus::Failed);
            REPL_EFFECT_UNCERTAIN.to_owned().into()
        } else {
            self.request.abandon("ORNA-REPL-EFFECT".to_owned().into())
        }
    }

    /// The same decision as [`Self::unsettled`], naming `cause` in the detail.
    pub(crate) fn unsettled_because(self, cause: String) -> repl::ReplFailure {
        if self.external_dispatched {
            let _ = self
                .request
                .finish_observation(orna_runtime::InvocationObservationStatus::Failed);
            repl_effect_uncertain(cause)
        } else {
            self.request.abandon(repl_effect_cancelled(cause))
        }
    }

    /// A failed table commit after evaluation. A failed observation write
    /// makes the outcome uncertain instead of a proven failure.
    pub(crate) fn commit_failed(self, cause: String) -> repl::ReplFailure {
        if self
            .request
            .finish_observation(orna_runtime::InvocationObservationStatus::Failed)
            .is_err()
        {
            return REPL_EFFECT_UNCERTAIN.to_owned().into();
        }
        repl_effect_uncertain(cause)
    }

    pub(crate) fn abandon(self, original: repl::ReplFailure) -> repl::ReplFailure {
        self.request.abandon(original)
    }

    /// The request is committed, so an observation write failure is only a
    /// warning and never reports the committed input as uncertain.
    pub(crate) fn record_committed(self) {
        if self
            .request
            .finish_observation(orna_runtime::InvocationObservationStatus::Succeeded)
            .is_err()
        {
            eprintln!("warning: committed; query observation not recorded");
        }
    }
}
