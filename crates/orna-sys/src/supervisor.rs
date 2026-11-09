//! The bounded runtime supervisor, its lifecycle fence, and worker scheduling.

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::BTreeMap,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use crate::{
    diagnostic::{AdmissionError, AwaitError, Diagnostic},
    ids::{InvocationId, RuntimeId},
    invocation::{
        Admission, AdmissionRequest, CancellationToken, ExecutionBoundary, InvocationCompletion,
        InvocationExecutor, InvocationHandle, InvocationMode, InvocationResult, InvocationState,
        TerminalClass,
    },
    runtime::Runtime,
    value::{InvocationMetadata, RetainedValue, TypedValue},
};

/// A small shared owner for independently awaitable local invocations. The
/// worker owns only execution; admission, cancellation and terminal retention
/// remain serialized through the runtime ledger.
#[derive(Clone, Debug)]
pub struct RuntimeSupervisor {
    runtime: Arc<Mutex<Runtime>>,
    pub(crate) workers: Arc<Mutex<BTreeMap<InvocationId, thread::JoinHandle<()>>>>,
    lifecycle: Arc<Lifecycle>,
    pub(crate) synchronous: Arc<SynchronousExecutions>,
    #[cfg(test)]
    test_hold_worker_start: Arc<AtomicBool>,
    #[cfg(test)]
    test_worker_gate: Arc<Mutex<Option<Arc<WorkerStartGate>>>>,
}

#[derive(Debug, Default)]
struct Lifecycle {
    state: Mutex<LifecycleState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct LifecycleState {
    restarting: bool,
}
/// Test-only synchronization hook shared between the admissions fence and
/// blocked worker threads.
#[cfg(test)]
type EnterHook = Arc<(Mutex<(bool, bool)>, Condvar)>;

#[derive(Debug, Default)]
pub(crate) struct SynchronousExecutions {
    pub(crate) state: Mutex<SynchronousExecutionState>,
    completed: Condvar,
    #[cfg(test)]
    enter_hook: Mutex<Option<EnterHook>>,
}

#[derive(Debug, Default)]
pub(crate) struct SynchronousExecutionState {
    active: Vec<ThreadId>,
    pending: usize,
}

/// Whether a newly spawned worker may enter user code. It starts `Closed` and
/// is decided exactly once.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum GateState {
    #[default]
    Closed,
    Open,
    Aborted,
}

/// Prevents a newly spawned worker from entering user code until the caller
/// has finished publishing its join handle. This keeps the lifecycle fence
/// out of the callback's reentrant call graph without creating an untracked
/// execution window.
#[derive(Debug, Default)]
struct WorkerStartGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl WorkerStartGate {
    /// Blocks until the gate is decided; true when the worker may run.
    fn wait(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        while *state == GateState::Closed {
            let Ok(next) = self.changed.wait(state) else {
                return false;
            };
            state = next;
        }
        *state == GateState::Open
    }

    fn release(&self) {
        self.decide(GateState::Open);
    }

    fn abort(&self) {
        self.decide(GateState::Aborted);
    }

    /// Settles a still-closed gate; a decided gate never changes.
    fn decide(&self, decision: GateState) {
        if let Ok(mut state) = self.state.lock()
            && *state == GateState::Closed
        {
            *state = decision;
            self.changed.notify_all();
        }
    }
}

impl SynchronousExecutions {
    fn contains(&self, thread: ThreadId) -> Result<bool, AdmissionError> {
        self.state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)
            .map(|state| state.active.contains(&thread))
    }

    fn enter(self: &Arc<Self>) -> Result<SynchronousExecution, AdmissionError> {
        self.state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .active
            .push(thread::current().id());
        #[cfg(test)]
        if let Some(hook) = self
            .enter_hook
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .clone()
        {
            let (state, wake) = &*hook;
            let mut state = state
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            state.0 = true;
            wake.notify_all();
            while !state.1 {
                state = wake
                    .wait(state)
                    .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            }
        }
        Ok(SynchronousExecution {
            executions: Arc::clone(self),
        })
    }

    pub(crate) fn wait_empty(&self) -> Result<(), AdmissionError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        while !state.active.is_empty() || state.pending != 0 {
            state = self
                .completed
                .wait(state)
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        }
        Ok(())
    }

    pub(crate) fn reserve(self: &Arc<Self>) -> Result<SynchronousReservation, AdmissionError> {
        self.state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .pending += 1;
        Ok(SynchronousReservation {
            executions: Arc::clone(self),
            reserved: true,
        })
    }

    fn leave(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                self.state.clear_poison();
                poisoned.into_inner()
            }
        };
        let current = thread::current().id();
        if let Some(index) = state.active.iter().position(|thread| *thread == current) {
            state.active.remove(index);
        }
        self.completed.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn set_enter_hook(&self, hook: Option<EnterHook>) {
        *self.enter_hook.lock().unwrap() = hook;
    }

    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.state.lock().unwrap().pending
    }
}

pub(crate) struct SynchronousReservation {
    executions: Arc<SynchronousExecutions>,
    reserved: bool,
}

impl SynchronousReservation {
    fn begin(mut self) -> Result<SynchronousExecution, AdmissionError> {
        #[cfg(test)]
        if let Some(hook) = self.executions.enter_hook.lock().unwrap().clone() {
            let (state, wake) = &*hook;
            let mut state = state.lock().unwrap();
            state.0 = true;
            wake.notify_all();
            while !state.1 {
                state = wake.wait(state).unwrap();
            }
        }
        let mut state = self
            .executions
            .state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        state.pending = state
            .pending
            .checked_sub(1)
            .ok_or(AdmissionError::RuntimeUnavailable)?;
        state.active.push(thread::current().id());
        self.reserved = false;
        self.executions.completed.notify_all();
        Ok(SynchronousExecution {
            executions: Arc::clone(&self.executions),
        })
    }
}

impl Drop for SynchronousReservation {
    fn drop(&mut self) {
        if !self.reserved {
            return;
        }
        let mut state = match self.executions.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                self.executions.state.clear_poison();
                poisoned.into_inner()
            }
        };
        if let Some(pending) = state.pending.checked_sub(1) {
            state.pending = pending;
            self.executions.completed.notify_all();
        }
    }
}

struct SynchronousExecution {
    executions: Arc<SynchronousExecutions>,
}

impl Drop for SynchronousExecution {
    fn drop(&mut self) {
        self.executions.leave();
    }
}

struct RestartFence {
    lifecycle: Arc<Lifecycle>,
}

impl Drop for RestartFence {
    fn drop(&mut self) {
        if let Ok(mut state) = self.lifecycle.state.lock() {
            state.restarting = false;
            self.lifecycle.changed.notify_all();
        }
    }
}

impl RuntimeSupervisor {
    pub fn new(id: RuntimeId) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Runtime::new(id))),
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            lifecycle: Arc::new(Lifecycle::default()),
            synchronous: Arc::new(SynchronousExecutions::default()),
            #[cfg(test)]
            test_hold_worker_start: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            test_worker_gate: Arc::new(Mutex::new(None)),
        }
    }

    pub fn id(&self) -> Result<RuntimeId, AdmissionError> {
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)
            .map(|runtime| runtime.id.clone())
    }

    pub fn generation(&self) -> Result<u64, AdmissionError> {
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)
            .map(|runtime| runtime.generation)
    }

    pub fn restart(&self) -> Result<RuntimeId, AdmissionError> {
        if self.synchronous.contains(thread::current().id())? {
            return Err(AdmissionError::RuntimeUnavailable);
        }

        let mut lifecycle = self
            .lifecycle
            .state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        while lifecycle.restarting {
            lifecycle = self
                .lifecycle
                .changed
                .wait(lifecycle)
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        }
        lifecycle.restarting = true;
        drop(lifecycle);
        let _fence = RestartFence {
            lifecycle: Arc::clone(&self.lifecycle),
        };

        let workers = {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            runtime.request_cancellation_for_all();
            let mut workers = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let workers = std::mem::take(&mut *workers);
            self.workers.clear_poison();
            workers
        };
        for (_, worker) in workers {
            let _ = worker.join();
        }
        self.synchronous.wait_empty()?;
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)
            .map(|mut runtime| runtime.restart())
    }

    /// Runs one admitted invocation synchronously under this owner's lifecycle
    /// fence. Idempotent replays retain the original terminal result and never
    /// execute the supplied callback again.
    pub fn run<T, E>(
        &self,
        request: AdmissionRequest<T>,
        executor: &mut E,
    ) -> Result<InvocationState, AdmissionError>
    where
        E: InvocationExecutor,
    {
        if request.mode != InvocationMode::Invoke {
            return Err(AdmissionError::StartMode);
        }
        let lifecycle = self
            .lifecycle
            .state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        if lifecycle.restarting {
            return Err(AdmissionError::RuntimeUnavailable);
        }
        let _execution = self.synchronous.enter()?;
        let admission = self
            .runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .admit(request)?;
        drop(lifecycle);
        match admission {
            Admission::New { boundary, handle } => {
                self.runtime
                    .lock()
                    .map_err(|_| AdmissionError::RuntimeUnavailable)?
                    .mark_running(&handle)?;
                let cancellation = {
                    self.runtime
                        .lock()
                        .map_err(|_| AdmissionError::RuntimeUnavailable)?
                        .cancellation_token(&handle)?
                };
                let result = execute_guarded(executor, &boundary, &cancellation);
                let mut runtime = self
                    .runtime
                    .lock()
                    .map_err(|_| AdmissionError::RuntimeUnavailable)?;
                runtime.retain_terminal(&handle, result)?;
                let state = runtime.invocation_state(&handle);
                drop(runtime);
                state
            }
            Admission::Active { .. } => Ok(InvocationState::Active),
            Admission::Terminal { result, .. } => Ok(InvocationState::Terminal(result)),
        }
    }

    /// Admits a separate invocation and schedules exactly one worker for a new
    /// idempotency identity. Existing active or terminal identities reuse the
    /// original handle and never spawn another worker.
    pub fn start<T, E>(
        &self,
        request: AdmissionRequest<T>,
        executor: E,
    ) -> Result<InvocationHandle<T>, AdmissionError>
    where
        E: InvocationExecutor + Send + 'static,
    {
        let lifecycle = self
            .lifecycle
            .state
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        if lifecycle.restarting {
            return Err(AdmissionError::RuntimeUnavailable);
        }
        if request.mode != InvocationMode::Start {
            return Err(AdmissionError::StartMode);
        }
        let (handle, new_work) = {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            match runtime.admit(request)? {
                Admission::New { boundary, handle } => {
                    let worker_handle = runtime
                        .handle::<()>(handle.invocation.id(), handle.result_type.id().clone());
                    let cancellation = runtime.cancellation_token(&worker_handle)?;
                    (
                        handle,
                        Some(NewWork {
                            boundary,
                            handle: worker_handle,
                            cancellation,
                        }),
                    )
                }
                Admission::Active { handle } | Admission::Terminal { handle, .. } => (handle, None),
            }
        };
        if let Some(work) = new_work {
            self.spawn_worker(work, executor, lifecycle)?;
        }
        Ok(handle)
    }

    /// Starts the one worker for a newly admitted invocation and publishes its
    /// join handle before releasing the lifecycle fence, so a concurrent
    /// restart can never miss it. Every failure to launch leaves the
    /// invocation orphaned rather than queued forever.
    fn spawn_worker<E>(
        &self,
        work: NewWork,
        executor: E,
        lifecycle: MutexGuard<'_, LifecycleState>,
    ) -> Result<(), AdmissionError>
    where
        E: InvocationExecutor + Send + 'static,
    {
        let NewWork {
            boundary,
            handle,
            cancellation,
        } = work;
        let reservation = match self.synchronous.reserve() {
            Ok(reservation) => reservation,
            Err(_) => {
                self.synchronous.state.clear_poison();
                return self.orphan(&handle);
            }
        };
        let start_gate = Arc::new(WorkerStartGate::default());
        let hold_start = self.hold_worker_start(&start_gate);
        let worker = thread::Builder::new().spawn({
            let runtime = Arc::clone(&self.runtime);
            let start_gate = Arc::clone(&start_gate);
            let handle = handle.clone();
            move || {
                run_worker(
                    &runtime,
                    &start_gate,
                    reservation,
                    executor,
                    &boundary,
                    &handle,
                    &cancellation,
                );
            }
        });
        let Ok(worker) = worker else {
            return self.orphan(&handle);
        };
        let workers = self
            .workers
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable);
        match workers {
            Ok(mut workers) => {
                workers.insert(handle.invocation.id().clone(), worker);
                drop(workers);
                drop(lifecycle);
                if !hold_start {
                    start_gate.release();
                }
                Ok(())
            }
            Err(error) => {
                self.workers.clear_poison();
                drop(lifecycle);
                start_gate.abort();
                // A worker that panicked has nothing left to report; the
                // invocation is orphaned below either way.
                let _ = worker.join();
                self.orphan(&handle)?;
                Err(error)
            }
        }
    }

    /// Records a launch failure: the admitted invocation will never run.
    fn orphan(&self, handle: &InvocationHandle<()>) -> Result<(), AdmissionError> {
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .classify_terminal(handle, TerminalClass::Orphaned)
    }

    /// Test hook: lets a test keep the next worker parked at its start gate.
    #[cfg(test)]
    fn hold_worker_start(&self, gate: &Arc<WorkerStartGate>) -> bool {
        let hold = self.test_hold_worker_start.load(Ordering::Acquire);
        if hold {
            *self.test_worker_gate.lock().unwrap() = Some(Arc::clone(gate));
        }
        hold
    }

    #[cfg(not(test))]
    fn hold_worker_start(&self, _: &Arc<WorkerStartGate>) -> bool {
        false
    }

    /// Waits for one retained terminal outcome. A timeout is only an
    /// observation failure: the target remains active and independently
    /// cancellable.
    pub fn await_invocation<T>(
        &self,
        handle: &InvocationHandle<T>,
        timeout: Option<Duration>,
    ) -> Result<InvocationResult<RetainedValue>, AwaitError> {
        let deadline = timeout.map(|duration| {
            Instant::now()
                .checked_add(duration)
                .unwrap_or_else(Instant::now)
        });
        loop {
            let terminal = {
                let runtime = self
                    .runtime
                    .lock()
                    .map_err(|_| AwaitError::Admission(AdmissionError::RuntimeUnavailable))?;
                let terminal = runtime
                    .terminal_result(handle)
                    .map_err(AwaitError::Admission)?;
                drop(runtime);
                match terminal {
                    None => None,
                    Some(result) => {
                        let worker = match self.workers.lock() {
                            Ok(mut workers) => workers.remove(handle.invocation().id()),
                            Err(poisoned) => {
                                self.workers.clear_poison();
                                let mut workers = poisoned.into_inner();
                                workers.remove(handle.invocation().id())
                            }
                        };
                        if let Some(worker) = worker {
                            let _ = worker.join();
                        }
                        Some(result)
                    }
                }
            };
            if let Some(result) = terminal {
                return Ok(result);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(AwaitError::Timeout);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn cancel<T>(
        &self,
        handle: &InvocationHandle<T>,
        reason: Option<Diagnostic>,
    ) -> Result<bool, AdmissionError> {
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .cancel(handle, reason)
    }

    pub fn state<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<InvocationState, AdmissionError> {
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .invocation_state(handle)
    }

    /// Returns redaction-safe metadata through the supervised runtime
    /// boundary without granting any new operational authority.
    pub fn invocation_metadata<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<InvocationMetadata, AdmissionError> {
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .invocation_metadata(handle)
    }
    #[cfg(test)]
    pub(crate) fn hold_next_worker_start(&self) {
        self.test_hold_worker_start.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn release_held_worker_start(&self) {
        self.test_hold_worker_start.store(false, Ordering::Release);
        if let Some(gate) = self.test_worker_gate.lock().unwrap().take() {
            gate.release();
        }
    }
}

struct NewWork {
    boundary: Box<ExecutionBoundary>,
    handle: InvocationHandle<()>,
    cancellation: CancellationToken,
}

/// The body of one supervised worker thread. Errors from the ledger mean a
/// restart already orphaned this invocation; there is no caller left to
/// report them to, so a failed write is dropped on purpose.
fn run_worker<E: InvocationExecutor>(
    runtime: &Mutex<Runtime>,
    start_gate: &WorkerStartGate,
    reservation: SynchronousReservation,
    mut executor: E,
    boundary: &ExecutionBoundary,
    handle: &InvocationHandle<()>,
    cancellation: &CancellationToken,
) {
    if !start_gate.wait() {
        return;
    }
    let Ok(execution) = reservation.begin() else {
        if let Ok(mut runtime) = runtime.lock() {
            let _ = runtime.classify_terminal(handle, TerminalClass::Orphaned);
        }
        return;
    };
    // Cancellation while queued fences callback entry. Once running is
    // published, cooperative executors observe later cancellation through the
    // shared token.
    let cancelled_before_running = {
        let Ok(mut runtime) = runtime.lock() else {
            return;
        };
        let cancelled = cancellation.is_cancelled();
        if !cancelled {
            let _ = runtime.mark_running(handle);
        }
        cancelled
    };
    if cancelled_before_running {
        if let Ok(mut runtime) = runtime.lock() {
            let _ = runtime.retain_terminal(handle, InvocationCompletion::Cancelled(None));
        }
        return;
    }
    let result = execute_guarded(&mut executor, boundary, cancellation);
    let Ok(mut runtime) = runtime.lock() else {
        return;
    };
    if runtime.retain_terminal(handle, result).is_err() {
        let _ = runtime.classify_terminal(handle, TerminalClass::Orphaned);
    }
    drop(runtime);
    drop(execution);
}

/// Runs the executor under the cancellation token. A panicking executor is an
/// orphaned completion, never an unwind into the supervisor.
fn execute_guarded<E: InvocationExecutor>(
    executor: &mut E,
    boundary: &ExecutionBoundary,
    cancellation: &CancellationToken,
) -> InvocationCompletion<TypedValue> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        executor.execute_controlled(boundary, cancellation)
    }))
    .unwrap_or_else(|_| {
        InvocationCompletion::Orphaned(Some(Diagnostic::new(
            "sys.invoke.orphaned",
            "invocation executor panicked",
        )))
    })
}
