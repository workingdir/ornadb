//! Bounded, pre-effect admission for Orna 1.0 reflective invocation.
//!
//! Resolution, durable transaction ownership, and schema generation stay with
//! the evaluator/runtime that owns those concerns. The local supervisor below
//! only provides a bounded execution and await seam for admitted work.

use std::{
    collections::BTreeMap,
    fmt,
    marker::PhantomData,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const CANONICAL_VALUE_CODEC_V1: &str = "OVB-1";

macro_rules! identity {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identity!(FunctionId);
identity!(RevisionId);
identity!(SnapshotId);
identity!(RuntimeId);
identity!(InvocationId);
identity!(TypeId);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FunctionIdentity {
    pub function: FunctionId,
    pub revision: RevisionId,
    pub snapshot: SnapshotId,
}

#[derive(Clone, Eq, PartialEq)]
pub struct TypedValue {
    static_type: TypeId,
    canonical: Vec<u8>,
    redacted: bool,
}
impl fmt::Debug for TypedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedValue")
            .field("static_type", &self.static_type)
            .field("redacted", &self.redacted)
            .field("canonical", &"<withheld>")
            .finish()
    }
}

impl TypedValue {
    pub fn public(static_type: TypeId, canonical: impl Into<Vec<u8>>) -> Self {
        Self {
            static_type,
            canonical: canonical.into(),
            redacted: false,
        }
    }
    pub fn protected(static_type: TypeId, canonical: impl Into<Vec<u8>>) -> Self {
        Self {
            static_type,
            canonical: canonical.into(),
            redacted: true,
        }
    }
    pub fn static_type(&self) -> &TypeId {
        &self.static_type
    }
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }
    fn append_identity(&self, out: &mut Vec<u8>) {
        append(out, self.static_type.as_str().as_bytes());
        out.push(u8::from(self.redacted));
        append(out, &self.canonical);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Argument {
    pub name: String,
    pub value: TypedValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ArgumentMap {
    entries: BTreeMap<String, TypedValue>,
}

impl ArgumentMap {
    pub fn new(entries: impl IntoIterator<Item = Argument>) -> Result<Self, AdmissionError> {
        let mut map = BTreeMap::new();
        for entry in entries {
            if map.insert(entry.name.clone(), entry.value).is_some() {
                return Err(AdmissionError::ArgumentType {
                    name: entry.name,
                    detail: ArgumentTypeDetail::Duplicate,
                });
            }
        }
        Ok(Self { entries: map })
    }
    pub fn entries(&self) -> impl Iterator<Item = (&str, &TypedValue)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
    fn get(&self, name: &str) -> Option<&TypedValue> {
        self.entries.get(name)
    }
    fn append_identity(&self, out: &mut Vec<u8>) {
        for (name, value) in &self.entries {
            append(out, name.as_bytes());
            value.append_identity(out);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub static_type: TypeId,
    pub default: Option<TypedValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionDescriptor {
    pub identity: FunctionIdentity,
    pub parameters: Vec<Parameter>,
    pub result_type: TypeId,
    pub visible: bool,
    pub callable: bool,
    pub generics_resolved: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum InvocationMode {
    Invoke,
    Start,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum TransactionMode {
    Inherit,
    Separate,
    ReadOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, Default, Serialize)]
pub struct InvocationContext {
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub trace_owner: Option<String>,
    pub cancellation_owner: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeWitness<T> {
    static_type: TypeId,
    marker: PhantomData<fn() -> T>,
}
impl<T> TypeWitness<T> {
    pub fn new(static_type: TypeId) -> Self {
        Self {
            static_type,
            marker: PhantomData,
        }
    }
    pub fn static_type(&self) -> &TypeId {
        &self.static_type
    }
}

#[derive(Clone, Debug)]
pub struct AdmissionRequest<T> {
    pub function: FunctionDescriptor,
    pub arguments: ArgumentMap,
    pub explicit_snapshot: Option<SnapshotId>,
    pub mode: InvocationMode,
    pub transaction: TransactionMode,
    pub witness: TypeWitness<T>,
    pub idempotency_key: Option<String>,
    pub context: InvocationContext,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InvocationIdentity(String);
impl InvocationIdentity {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionBoundary {
    pub invocation: InvocationId,
    pub identity: InvocationIdentity,
    pub function: FunctionIdentity,
    pub arguments: ArgumentMap,
    pub mode: InvocationMode,
    pub transaction: TransactionMode,
    pub context: InvocationContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationHandle<T> {
    invocation: InvocationId,
    runtime: RuntimeId,
    result_type: TypeId,
    marker: PhantomData<fn() -> T>,
}
impl<T> InvocationHandle<T> {
    pub fn invocation(&self) -> &InvocationId {
        &self.invocation
    }
    pub fn runtime(&self) -> &RuntimeId {
        &self.runtime
    }
    pub fn result_type(&self) -> &TypeId {
        &self.result_type
    }
}

#[derive(Clone, Debug)]
pub enum Admission<T> {
    New {
        boundary: Box<ExecutionBoundary>,
        handle: InvocationHandle<T>,
    },
    Active {
        handle: InvocationHandle<T>,
    },
    Terminal {
        handle: InvocationHandle<T>,
        outcome: TerminalClass,
        result: RetainedInvocationResult,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum TerminalClass {
    Succeeded,
    Failed,
    Cancelled,
    Orphaned,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationResult<T> {
    Success(T),
    OrdinaryFailure(Diagnostic),
    Cancelled(Option<Diagnostic>),
    Orphaned(Option<Diagnostic>),
}

#[derive(Clone)]
pub struct CancellationToken {
    requested: Arc<AtomicBool>,
}

impl CancellationToken {
    fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl PartialEq for CancellationToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.requested, &other.requested)
    }
}

impl Eq for CancellationToken {}

pub trait InvocationExecutor {
    fn execute(&mut self, boundary: &ExecutionBoundary) -> InvocationResult<TypedValue>;

    fn execute_controlled(
        &mut self,
        boundary: &ExecutionBoundary,
        _: &CancellationToken,
    ) -> InvocationResult<TypedValue> {
        self.execute(boundary)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RetainedValue {
    static_type: TypeId,
    codec_version: String,
    canonical: Option<Vec<u8>>,
    redacted: bool,
}
impl fmt::Debug for RetainedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedValue")
            .field("static_type", &self.static_type)
            .field("codec_version", &self.codec_version)
            .field("redacted", &self.redacted)
            .field("canonical", &"<withheld>")
            .finish()
    }
}
impl RetainedValue {
    fn new(value: TypedValue) -> Self {
        let TypedValue {
            static_type,
            canonical,
            redacted,
        } = value;
        Self {
            static_type,
            codec_version: CANONICAL_VALUE_CODEC_V1.into(),
            canonical: (!redacted).then_some(canonical),
            redacted,
        }
    }
    pub fn static_type(&self) -> &TypeId {
        &self.static_type
    }
    pub fn codec_version(&self) -> &str {
        &self.codec_version
    }
    pub fn canonical(&self) -> Option<&[u8]> {
        self.canonical.as_deref()
    }
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetainedInvocationResult {
    Success(RetainedValue),
    OrdinaryFailure(Diagnostic),
    Cancelled(Option<Diagnostic>),
    Orphaned(Option<Diagnostic>),
    ClassificationOnly(TerminalClass),
}
impl RetainedInvocationResult {
    pub fn terminal_class(&self) -> TerminalClass {
        match self {
            Self::Success(_) => TerminalClass::Succeeded,
            Self::OrdinaryFailure(_) => TerminalClass::Failed,
            Self::Cancelled(_) => TerminalClass::Cancelled,
            Self::Orphaned(_) => TerminalClass::Orphaned,
            Self::ClassificationOnly(outcome) => outcome.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationState {
    Active,
    Terminal(RetainedInvocationResult),
}
impl<T> InvocationResult<T> {
    pub fn ordinary_failure(&self) -> Option<&Diagnostic> {
        match self {
            Self::OrdinaryFailure(diagnostic) => Some(diagnostic),
            _ => None,
        }
    }
    pub fn terminal_class(&self) -> TerminalClass {
        match self {
            Self::Success(_) => TerminalClass::Succeeded,
            Self::OrdinaryFailure(_) => TerminalClass::Failed,
            Self::Cancelled(_) => TerminalClass::Cancelled,
            Self::Orphaned(_) => TerminalClass::Orphaned,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Runtime {
    id: RuntimeId,
    generation: u64,
    next_invocation: u64,
    invocations: BTreeMap<InvocationId, StoredInvocation>,
    idempotency: BTreeMap<String, IdempotencyEntry>,
}
impl Runtime {
    pub fn new(id: RuntimeId) -> Self {
        Self {
            id,
            generation: 1,
            next_invocation: 0,
            invocations: BTreeMap::new(),
            idempotency: BTreeMap::new(),
        }
    }
    pub fn id(&self) -> &RuntimeId {
        &self.id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Starts a new owner generation and invalidates every handle from the
    /// previous generation. Live invocation state is deliberately not
    /// reparented across an owner restart.
    pub fn restart(&mut self) -> RuntimeId {
        self.generation = self.generation.saturating_add(1);
        self.id = RuntimeId::new(format!("{}@{}", self.id.as_str(), self.generation));
        self.next_invocation = 0;
        self.invocations.clear();
        self.idempotency.clear();
        self.id.clone()
    }
    pub fn admit<T>(
        &mut self,
        request: AdmissionRequest<T>,
    ) -> Result<Admission<T>, AdmissionError> {
        validate(&request)?;
        let bound = bind(&request.function, &request.arguments)?;
        let identity = identity(&request, &bound);
        if let Some(key) = &request.idempotency_key
            && let Some(entry) = self.idempotency.get(key)
        {
            if entry.identity != identity {
                return Err(AdmissionError::IdempotencyMismatch);
            }
            let handle = self.handle::<T>(&entry.invocation, request.witness.static_type().clone());
            return Ok(match entry.terminal.clone() {
                Some(result) => Admission::Terminal {
                    outcome: result.terminal_class(),
                    handle,
                    result,
                },
                None => Admission::Active { handle },
            });
        }
        self.next_invocation += 1;
        let invocation = InvocationId::new(format!("invocation-{}", self.next_invocation));
        let boundary = ExecutionBoundary {
            invocation: invocation.clone(),
            identity: identity.clone(),
            function: request.function.identity,
            arguments: bound,
            mode: request.mode,
            transaction: request.transaction,
            context: request.context,
        };
        self.invocations.insert(
            invocation.clone(),
            StoredInvocation {
                result_type: request.witness.static_type().clone(),
                terminal: None,
                cancellation_requested: false,
                cancellation_reason: None,
                cancellation: CancellationToken::new(),
            },
        );
        if let Some(key) = request.idempotency_key {
            self.idempotency.insert(
                key,
                IdempotencyEntry {
                    identity,
                    invocation: invocation.clone(),
                    terminal: None,
                },
            );
        }
        Ok(Admission::New {
            handle: self.handle(&invocation, request.witness.static_type().clone()),
            boundary: Box::new(boundary),
        })
    }
    /// Runs one newly admitted synchronous invocation through the supplied
    /// execution boundary. Existing active and terminal idempotency records
    /// never execute the callback a second time.
    pub fn run<T, E>(
        &mut self,
        request: AdmissionRequest<T>,
        executor: &mut E,
    ) -> Result<InvocationState, AdmissionError>
    where
        E: InvocationExecutor,
    {
        match self.admit(request)? {
            Admission::New { boundary, handle } => {
                let cancellation = self.cancellation_token(&handle)?;
                self.retain_terminal(
                    &handle,
                    executor.execute_controlled(&boundary, &cancellation),
                )?;
                self.invocation_state(&handle)
            }
            Admission::Active { .. } => Ok(InvocationState::Active),
            Admission::Terminal { result, .. } => Ok(InvocationState::Terminal(result)),
        }
    }
    pub fn classify_terminal<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        outcome: TerminalClass,
    ) -> Result<(), AdmissionError> {
        self.store_terminal(
            handle,
            RetainedInvocationResult::ClassificationOnly(outcome),
        )
    }
    pub fn retain_terminal<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        result: InvocationResult<TypedValue>,
    ) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        let cancellation_reason = self
            .invocations
            .get(&handle.invocation)
            .expect("checked")
            .cancellation_reason
            .clone();
        if self
            .invocations
            .get(&handle.invocation)
            .expect("checked")
            .terminal
            .is_some()
        {
            return Ok(());
        }
        let result = match result {
            InvocationResult::Success(value) => {
                if value.static_type() != handle.result_type() {
                    return Err(AdmissionError::ReturnType);
                }
                RetainedInvocationResult::Success(RetainedValue::new(value))
            }
            InvocationResult::OrdinaryFailure(diagnostic) => {
                RetainedInvocationResult::OrdinaryFailure(diagnostic)
            }
            InvocationResult::Cancelled(diagnostic) => {
                RetainedInvocationResult::Cancelled(diagnostic.or(cancellation_reason))
            }
            InvocationResult::Orphaned(diagnostic) => {
                RetainedInvocationResult::Orphaned(diagnostic)
            }
        };
        self.store_terminal(handle, result)
    }
    pub fn invocation_state<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<InvocationState, AdmissionError> {
        self.check_handle(handle)?;
        Ok(
            match self
                .invocations
                .get(&handle.invocation)
                .expect("checked")
                .terminal
                .clone()
            {
                Some(result) => InvocationState::Terminal(result),
                None => InvocationState::Active,
            },
        )
    }
    /// Requests cancellation without converting an active invocation into an
    /// ordinary failure. The target remains active until its owner records a
    /// terminal cancellation, so a concurrent completion can still win.
    pub fn cancel<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        reason: Option<Diagnostic>,
    ) -> Result<bool, AdmissionError> {
        self.check_handle(handle)?;
        let invocation = self
            .invocations
            .get_mut(&handle.invocation)
            .expect("checked");
        match invocation.terminal.as_ref() {
            Some(result) => Ok(result.terminal_class() == TerminalClass::Cancelled),
            None => {
                if invocation.cancellation_requested {
                    return Ok(true);
                }
                invocation.cancellation_requested = true;
                invocation
                    .cancellation
                    .requested
                    .store(true, Ordering::Release);
                invocation.cancellation_reason = reason;
                Ok(true)
            }
        }
    }
    pub fn cancellation_requested<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<bool, AdmissionError> {
        self.check_handle(handle)?;
        Ok(self
            .invocations
            .get(&handle.invocation)
            .expect("checked")
            .cancellation
            .is_cancelled())
    }
    fn request_cancellation_for_all(&mut self) {
        for invocation in self.invocations.values_mut() {
            if invocation.terminal.is_none() {
                invocation.cancellation_requested = true;
                invocation
                    .cancellation
                    .requested
                    .store(true, Ordering::Release);
            }
        }
    }
    fn cancellation_token<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<CancellationToken, AdmissionError> {
        self.check_handle(handle)?;
        Ok(self
            .invocations
            .get(&handle.invocation)
            .expect("checked")
            .cancellation
            .clone())
    }
    fn store_terminal<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        result: RetainedInvocationResult,
    ) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        let stored = self
            .invocations
            .get_mut(&handle.invocation)
            .expect("checked");
        if stored.terminal.is_some() {
            return Ok(());
        }
        stored.terminal = Some(result.clone());
        for entry in self
            .idempotency
            .values_mut()
            .filter(|entry| entry.invocation == handle.invocation)
        {
            entry.terminal = Some(result.clone());
        }
        Ok(())
    }
    pub fn check_handle<T>(&self, handle: &InvocationHandle<T>) -> Result<(), AdmissionError> {
        if handle.runtime != self.id {
            return Err(AdmissionError::ForeignRuntime);
        }
        let stored = self
            .invocations
            .get(&handle.invocation)
            .ok_or(AdmissionError::ExpiredHandle)?;
        if stored.result_type != handle.result_type {
            return Err(AdmissionError::ExpiredHandle);
        }
        Ok(())
    }
    pub fn expire<T>(&mut self, handle: &InvocationHandle<T>) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        self.invocations.remove(&handle.invocation);
        self.idempotency
            .retain(|_, entry| entry.invocation != handle.invocation);
        Ok(())
    }
    fn handle<T>(&self, invocation: &InvocationId, result_type: TypeId) -> InvocationHandle<T> {
        InvocationHandle {
            invocation: invocation.clone(),
            runtime: self.id.clone(),
            result_type,
            marker: PhantomData,
        }
    }
}

/// A small shared owner for independently awaitable local invocations. The
/// worker owns only execution; admission, cancellation and terminal retention
/// remain serialized through the runtime ledger.
#[derive(Clone, Debug)]
pub struct RuntimeSupervisor {
    runtime: Arc<Mutex<Runtime>>,
    workers: Arc<Mutex<BTreeMap<InvocationId, thread::JoinHandle<()>>>>,
    control: Arc<Mutex<()>>,
}

impl RuntimeSupervisor {
    pub fn new(id: RuntimeId) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Runtime::new(id))),
            workers: Arc::new(Mutex::new(BTreeMap::new())),
            control: Arc::new(Mutex::new(())),
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
        let _control = self
            .control
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        let workers = {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            runtime.request_cancellation_for_all();
            std::mem::take(
                &mut *self
                    .workers
                    .lock()
                    .map_err(|_| AdmissionError::RuntimeUnavailable)?,
            )
        };
        for (_, worker) in workers {
            let _ = worker.join();
        }
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
        let _control = self
            .control
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        self.runtime
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?
            .run(request, executor)
    }

    /// Admits a separate invocation and schedules exactly one worker for a new
    /// idempotency identity. Existing active or terminal identities reuse the
    /// original handle and never spawn another worker.
    pub fn start<T, E>(
        &self,
        request: AdmissionRequest<T>,
        mut executor: E,
    ) -> Result<InvocationHandle<T>, AdmissionError>
    where
        E: InvocationExecutor + Send + 'static,
    {
        let _control = self
            .control
            .lock()
            .map_err(|_| AdmissionError::RuntimeUnavailable)?;
        if request.mode != InvocationMode::Start {
            return Err(AdmissionError::StartMode);
        }
        let (handle, boundary) = {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            match runtime.admit(request)? {
                Admission::New { boundary, handle } => {
                    let worker_handle =
                        runtime.handle::<()>(&handle.invocation, handle.result_type.clone());
                    let cancellation = runtime.cancellation_token(&worker_handle)?;
                    (handle, Some((boundary, worker_handle, cancellation)))
                }
                Admission::Active { handle } | Admission::Terminal { handle, .. } => (handle, None),
            }
        };

        if let Some((boundary, worker_handle, cancellation)) = boundary {
            let runtime = Arc::clone(&self.runtime);
            let worker = thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    executor.execute_controlled(&boundary, &cancellation)
                }))
                .unwrap_or_else(|_| {
                    InvocationResult::Orphaned(Some(Diagnostic {
                        code: "sys.invoke.orphaned",
                        message: "invocation executor panicked",
                        fields: BTreeMap::new(),
                    }))
                });
                let Ok(mut runtime) = runtime.lock() else {
                    return;
                };
                if runtime.retain_terminal(&worker_handle, result).is_err() {
                    let _ = runtime.classify_terminal(&worker_handle, TerminalClass::Orphaned);
                }
            });
            self.workers
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?
                .insert(handle.invocation.clone(), worker);
        }
        Ok(handle)
    }

    /// Waits for one retained terminal outcome. A timeout is only an
    /// observation failure: the target remains active and independently
    /// cancellable.
    pub fn await_invocation<T>(
        &self,
        handle: &InvocationHandle<T>,
        timeout: Option<Duration>,
    ) -> Result<RetainedInvocationResult, AwaitError> {
        let deadline = timeout.map(|duration| {
            Instant::now()
                .checked_add(duration)
                .unwrap_or_else(Instant::now)
        });
        loop {
            let state = {
                let runtime = self
                    .runtime
                    .lock()
                    .map_err(|_| AwaitError::Admission(AdmissionError::RuntimeUnavailable))?;
                runtime
                    .invocation_state(handle)
                    .map_err(AwaitError::Admission)?
            };
            if let InvocationState::Terminal(result) = state {
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredInvocation {
    result_type: TypeId,
    terminal: Option<RetainedInvocationResult>,
    cancellation_requested: bool,
    cancellation_reason: Option<Diagnostic>,
    cancellation: CancellationToken,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct IdempotencyEntry {
    identity: InvocationIdentity,
    invocation: InvocationId,
    terminal: Option<RetainedInvocationResult>,
}

fn bind(
    function: &FunctionDescriptor,
    arguments: &ArgumentMap,
) -> Result<ArgumentMap, AdmissionError> {
    let declared: BTreeMap<&str, &Parameter> = function
        .parameters
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter))
        .collect();
    for (name, _) in arguments.entries() {
        if !declared.contains_key(name) {
            return Err(AdmissionError::ArgumentUnknown { name: name.into() });
        }
    }
    let mut bound = Vec::new();
    for parameter in &function.parameters {
        match arguments.get(&parameter.name) {
            Some(value) if value.static_type() == &parameter.static_type => bound.push(Argument {
                name: parameter.name.clone(),
                value: value.clone(),
            }),
            Some(_) => {
                return Err(AdmissionError::ArgumentType {
                    name: parameter.name.clone(),
                    detail: ArgumentTypeDetail::Mismatch,
                });
            }
            None => match &parameter.default {
                Some(value) => bound.push(Argument {
                    name: parameter.name.clone(),
                    value: value.clone(),
                }),
                None => {
                    return Err(AdmissionError::ArgumentMissing {
                        name: parameter.name.clone(),
                    });
                }
            },
        }
    }
    ArgumentMap::new(bound)
}
fn validate<T>(request: &AdmissionRequest<T>) -> Result<(), AdmissionError> {
    if request
        .explicit_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot != &request.function.identity.snapshot)
    {
        return Err(AdmissionError::SnapshotMismatch);
    }
    if !request.function.visible {
        return Err(AdmissionError::NotVisible);
    }
    if !request.function.callable || !request.function.generics_resolved {
        return Err(AdmissionError::NotCallable);
    }
    if request.witness.static_type() != &request.function.result_type {
        return Err(AdmissionError::ReturnType);
    }
    if request.mode == InvocationMode::Start && request.transaction == TransactionMode::Inherit {
        return Err(AdmissionError::TransactionMode);
    }
    Ok(())
}
fn identity<T>(request: &AdmissionRequest<T>, arguments: &ArgumentMap) -> InvocationIdentity {
    let mut bytes = Vec::new();
    append(
        &mut bytes,
        request.function.identity.function.as_str().as_bytes(),
    );
    append(
        &mut bytes,
        request.function.identity.revision.as_str().as_bytes(),
    );
    append(
        &mut bytes,
        request.function.identity.snapshot.as_str().as_bytes(),
    );
    arguments.append_identity(&mut bytes);
    append(
        &mut bytes,
        request.witness.static_type().as_str().as_bytes(),
    );
    bytes.push(request.mode as u8);
    bytes.push(request.transaction as u8);
    for value in [
        &request.context.locale,
        &request.context.timezone,
        &request.context.trace_owner,
        &request.context.cancellation_owner,
    ] {
        append(&mut bytes, value.as_deref().unwrap_or_default().as_bytes());
    }
    InvocationIdentity(hex(&Sha256::digest(bytes)))
}
fn append(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgumentTypeDetail {
    Duplicate,
    Mismatch,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    ArgumentMissing {
        name: String,
    },
    ArgumentUnknown {
        name: String,
    },
    ArgumentType {
        name: String,
        detail: ArgumentTypeDetail,
    },
    SnapshotMismatch,
    NotVisible,
    NotCallable,
    ReturnType,
    TransactionMode,
    IdempotencyMismatch,
    StartMode,
    ForeignRuntime,
    ExpiredHandle,
    RuntimeUnavailable,
}
impl AdmissionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ArgumentMissing { .. } => "sys.invoke.argument_missing",
            Self::ArgumentUnknown { .. } => "sys.invoke.argument_unknown",
            Self::ArgumentType { .. } => "sys.invoke.argument_type",
            Self::SnapshotMismatch => "sys.invoke.snapshot_mismatch",
            Self::NotVisible | Self::NotCallable => "sys.invoke.not_callable",
            Self::ReturnType => "sys.invoke.return_type",
            Self::TransactionMode => "sys.invoke.transaction_mode",
            Self::IdempotencyMismatch => "sys.invoke.idempotency_mismatch",
            Self::StartMode => "sys.invoke.start_mode",
            Self::ForeignRuntime => "sys.handle.foreign_runtime",
            Self::ExpiredHandle => "sys.handle.expired",
            Self::RuntimeUnavailable => "sys.runtime.unavailable",
        }
    }
    pub fn diagnostic(&self) -> Diagnostic {
        Diagnostic {
            code: self.code(),
            message: "reflective invocation admission rejected",
            fields: BTreeMap::new(),
        }
    }
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for AdmissionError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AwaitError {
    Timeout,
    Admission(AdmissionError),
}
impl AwaitError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Timeout => "sys.invoke.await_timeout",
            Self::Admission(error) => error.code(),
        }
    }
}
impl fmt::Display for AwaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for AwaitError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: &'static str,
    pub fields: BTreeMap<String, DiagnosticField>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticField {
    Text(String),
    Redacted { static_type: TypeId },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    fn ty(name: &str) -> TypeId {
        TypeId::new(name)
    }
    fn value(t: &str, data: &str) -> TypedValue {
        TypedValue::public(ty(t), data)
    }
    fn function(default: Option<TypedValue>) -> FunctionDescriptor {
        FunctionDescriptor {
            identity: FunctionIdentity {
                function: FunctionId::new("f"),
                revision: RevisionId::new("r1"),
                snapshot: SnapshotId::new("s1"),
            },
            parameters: vec![Parameter {
                name: "a".into(),
                static_type: ty("Int"),
                default,
            }],
            result_type: ty("Str"),
            visible: true,
            callable: true,
            generics_resolved: true,
        }
    }
    fn request(default: Option<TypedValue>, args: ArgumentMap) -> AdmissionRequest<String> {
        AdmissionRequest {
            function: function(default),
            arguments: args,
            explicit_snapshot: None,
            mode: InvocationMode::Invoke,
            transaction: TransactionMode::Inherit,
            witness: TypeWitness::new(ty("Str")),
            idempotency_key: None,
            context: InvocationContext::default(),
        }
    }
    fn args(entries: Vec<Argument>) -> ArgumentMap {
        ArgumentMap::new(entries).unwrap()
    }
    struct Executor {
        calls: usize,
        result: InvocationResult<TypedValue>,
    }
    impl InvocationExecutor for Executor {
        fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
            self.calls += 1;
            self.result.clone()
        }
    }
    struct BlockingExecutor {
        gate: Arc<(Mutex<bool>, Condvar)>,
        calls: Arc<AtomicUsize>,
        result: InvocationResult<TypedValue>,
    }
    impl InvocationExecutor for BlockingExecutor {
        fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (open, wake) = &*self.gate;
            let mut open = open.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
            self.result.clone()
        }
    }
    struct CancellableExecutor {
        observed: Arc<AtomicBool>,
    }
    impl InvocationExecutor for CancellableExecutor {
        fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
            InvocationResult::Success(value("Str", "uncancelled"))
        }

        fn execute_controlled(
            &mut self,
            _: &ExecutionBoundary,
            cancellation: &CancellationToken,
        ) -> InvocationResult<TypedValue> {
            while !cancellation.is_cancelled() {
                thread::yield_now();
            }
            self.observed.store(true, Ordering::SeqCst);
            InvocationResult::Cancelled(None)
        }
    }
    fn diagnostic(code: &'static str) -> Diagnostic {
        Diagnostic {
            code,
            message: "invocation terminated",
            fields: BTreeMap::from([(
                "detail".into(),
                DiagnosticField::Redacted {
                    static_type: ty("sys.Secret"),
                },
            )]),
        }
    }
    #[test]
    fn canonical_order_and_type_retention() {
        let map = args(vec![
            Argument {
                name: "z".into(),
                value: value("Int", "1"),
            },
            Argument {
                name: "a".into(),
                value: value("Str", "1"),
            },
        ]);
        let names: Vec<_> = map.entries().map(|(name, _)| name).collect();
        assert_eq!(names, ["a", "z"]);
        assert_eq!(map.entries().next().unwrap().1.static_type(), &ty("Str"));
        assert!(matches!(
            ArgumentMap::new(vec![
                Argument {
                    name: "a".into(),
                    value: value("Str", "first"),
                },
                Argument {
                    name: "a".into(),
                    value: value("Str", "second"),
                },
            ]),
            Err(AdmissionError::ArgumentType {
                detail: ArgumentTypeDetail::Duplicate,
                ..
            })
        ));
    }
    #[test]
    fn defaults_affect_identity() {
        let empty = ArgumentMap::default();
        let mut one = Runtime::new(RuntimeId::new("runtime"));
        let first = one
            .admit(request(Some(value("Int", "1")), empty.clone()))
            .unwrap();
        let mut two = Runtime::new(RuntimeId::new("runtime"));
        let second = two.admit(request(Some(value("Int", "2")), empty)).unwrap();
        let identity = |admission: Admission<String>| match admission {
            Admission::New { boundary, .. } => boundary.identity,
            _ => unreachable!(),
        };
        assert_ne!(identity(first), identity(second));
    }
    #[test]
    fn rejects_before_effect_boundary() {
        let map = args(vec![Argument {
            name: "a".into(),
            value: value("Str", "bad"),
        }]);
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        assert!(matches!(
            runtime.admit(request(None, map)),
            Err(AdmissionError::ArgumentType { .. })
        ));
        assert_eq!(runtime.invocations.len(), 0);
        let missing = runtime.admit(request(None, ArgumentMap::default()));
        assert!(matches!(
            missing,
            Err(AdmissionError::ArgumentMissing { .. })
        ));
        let unknown = args(vec![Argument {
            name: "x".into(),
            value: value("Int", "1"),
        }]);
        assert!(matches!(
            runtime.admit(request(None, unknown)),
            Err(AdmissionError::ArgumentUnknown { .. })
        ));
        let mut hidden = request(Some(value("Int", "1")), ArgumentMap::default());
        hidden.function.visible = false;
        assert!(matches!(
            runtime.admit(hidden),
            Err(AdmissionError::NotVisible)
        ));
        let mut wrong_result = request(Some(value("Int", "1")), ArgumentMap::default());
        wrong_result.witness = TypeWitness::new(ty("Int"));
        assert!(matches!(
            runtime.admit(wrong_result),
            Err(AdmissionError::ReturnType)
        ));
        let mut snap = request(Some(value("Int", "1")), ArgumentMap::default());
        snap.explicit_snapshot = Some(SnapshotId::new("other"));
        assert!(matches!(
            runtime.admit(snap),
            Err(AdmissionError::SnapshotMismatch)
        ));
    }
    #[test]
    fn idempotency_replays_active_terminal_and_conflicts() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut first = request(Some(value("Int", "1")), ArgumentMap::default());
        first.idempotency_key = Some("key".into());
        let accepted = runtime.admit(first.clone()).unwrap();
        let handle = match accepted {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        assert!(matches!(
            runtime.admit(first.clone()).unwrap(),
            Admission::Active { .. }
        ));
        runtime
            .retain_terminal(&handle, InvocationResult::Success(value("Str", "answer")))
            .unwrap();
        let replayed = runtime.admit(first.clone()).unwrap();
        let Admission::Terminal {
            handle: replayed_handle,
            outcome,
            result,
        } = replayed
        else {
            panic!("expected retained terminal replay");
        };
        assert_eq!(replayed_handle.invocation(), handle.invocation());
        assert_eq!(outcome, TerminalClass::Succeeded);
        let RetainedInvocationResult::Success(retained) = result else {
            panic!("expected retained success");
        };
        assert_eq!(retained.static_type(), &ty("Str"));
        assert_eq!(retained.codec_version(), CANONICAL_VALUE_CODEC_V1);
        assert_eq!(retained.canonical(), Some(b"answer".as_slice()));
        assert_eq!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(retained)
            ))
        );
        assert!(matches!(
            runtime.admit(first.clone()).unwrap(),
            Admission::Terminal {
                outcome: TerminalClass::Succeeded,
                result: RetainedInvocationResult::Success(_),
                ..
            }
        ));
        let mut conflict = first;
        conflict.context.locale = Some("cy".into());
        assert!(matches!(
            runtime.admit(conflict),
            Err(AdmissionError::IdempotencyMismatch)
        ));
    }
    #[test]
    fn run_executes_new_idempotent_work_once_and_replays_terminal_state() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let mut first = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "answer")),
        };
        assert!(matches!(
            runtime.run(request.clone(), &mut first),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ));
        assert_eq!(first.calls, 1);

        let mut replay = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "wrong")),
        };
        assert!(matches!(
            runtime.run(request, &mut replay),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(ref retained)
            )) if retained.canonical() == Some(b"answer".as_slice())
        ));
        assert_eq!(replay.calls, 0);
    }
    #[test]
    fn run_retains_cancellation_as_a_terminal_classification() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut executor = Executor {
            calls: 0,
            result: InvocationResult::Cancelled(Some(diagnostic("cancelled"))),
        };
        let state = runtime.run(
            request(Some(value("Int", "1")), ArgumentMap::default()),
            &mut executor,
        );
        assert!(matches!(
            state,
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Cancelled(Some(_))
            ))
        ));
        assert_eq!(executor.calls, 1);
    }
    #[test]
    fn supervised_run_replays_the_retained_terminal_result_and_obeys_restart_fence() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let mut first = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "answer")),
        };
        assert!(matches!(
            supervisor.run(request.clone(), &mut first),
            Ok(InvocationState::Terminal(RetainedInvocationResult::Success(ref retained)))
                if retained.canonical() == Some(b"answer".as_slice())
        ));
        assert_eq!(first.calls, 1);

        let mut replay = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "wrong")),
        };
        assert!(matches!(
            supervisor.run(request.clone(), &mut replay),
            Ok(InvocationState::Terminal(RetainedInvocationResult::Success(ref retained)))
                if retained.canonical() == Some(b"answer".as_slice())
        ));
        assert_eq!(replay.calls, 0);

        supervisor.restart().unwrap();
        let mut after_restart = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "after-restart")),
        };
        assert!(matches!(
            supervisor.run(request, &mut after_restart),
            Ok(InvocationState::Terminal(RetainedInvocationResult::Success(ref retained)))
                if retained.canonical() == Some(b"after-restart".as_slice())
        ));
        assert_eq!(after_restart.calls, 1);
    }
    #[test]
    fn supervised_start_timeout_does_not_cancel_or_duplicate_work() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        request.idempotency_key = Some("key".into());
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let handle = supervisor
            .start(
                request.clone(),
                BlockingExecutor {
                    gate: Arc::clone(&gate),
                    calls: Arc::clone(&calls),
                    result: InvocationResult::Success(value("Str", "answer")),
                },
            )
            .unwrap();

        assert_eq!(
            supervisor.await_invocation(&handle, Some(Duration::ZERO)),
            Err(AwaitError::Timeout)
        );
        assert_eq!(supervisor.state(&handle), Ok(InvocationState::Active));
        assert_eq!(
            supervisor.cancel(&handle, Some(diagnostic("cancelled"))),
            Ok(true)
        );
        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_one();

        assert!(matches!(
            supervisor.await_invocation(&handle, Some(Duration::from_secs(1))),
            Ok(RetainedInvocationResult::Success(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.cancel(&handle, None), Ok(false));

        let replay_calls = Arc::new(AtomicUsize::new(0));
        let replay = supervisor
            .start(
                request,
                BlockingExecutor {
                    gate,
                    calls: Arc::clone(&replay_calls),
                    result: InvocationResult::Success(value("Str", "wrong")),
                },
            )
            .unwrap();
        assert_eq!(replay.invocation(), handle.invocation());
        assert_eq!(replay_calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn supervised_start_retains_explicit_cancellation() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let handle = supervisor
            .start(
                request,
                BlockingExecutor {
                    gate: Arc::clone(&gate),
                    calls: Arc::new(AtomicUsize::new(0)),
                    result: InvocationResult::Cancelled(None),
                },
            )
            .unwrap();
        let reason = diagnostic("cancelled");
        assert_eq!(supervisor.cancel(&handle, Some(reason.clone())), Ok(true));
        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_one();

        assert_eq!(
            supervisor.await_invocation(&handle, Some(Duration::from_secs(1))),
            Ok(RetainedInvocationResult::Cancelled(Some(reason)))
        );
    }
    #[test]
    fn supervised_cancellation_token_reaches_running_executor() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        let observed = Arc::new(AtomicBool::new(false));
        let handle = supervisor
            .start(
                request,
                CancellableExecutor {
                    observed: Arc::clone(&observed),
                },
            )
            .unwrap();

        assert_eq!(supervisor.cancel(&handle, None), Ok(true));
        assert_eq!(
            supervisor.await_invocation(&handle, Some(Duration::from_secs(1))),
            Ok(RetainedInvocationResult::Cancelled(None))
        );
        assert!(observed.load(Ordering::SeqCst));
    }
    #[test]
    fn fences_foreign_and_stale_handles() {
        let mut left = Runtime::new(RuntimeId::new("left"));
        let accepted = left
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap();
        let handle = match accepted {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        let mut right = Runtime::new(RuntimeId::new("right"));
        assert_eq!(
            right.check_handle(&handle),
            Err(AdmissionError::ForeignRuntime)
        );
        assert_eq!(
            right.invocation_state(&handle),
            Err(AdmissionError::ForeignRuntime)
        );
        assert_eq!(
            right.retain_terminal(
                &handle,
                InvocationResult::OrdinaryFailure(diagnostic("failed")),
            ),
            Err(AdmissionError::ForeignRuntime)
        );
        left.expire(&handle).unwrap();
        assert_eq!(
            left.check_handle(&handle),
            Err(AdmissionError::ExpiredHandle)
        );
        assert_eq!(
            left.invocation_state(&handle),
            Err(AdmissionError::ExpiredHandle)
        );
        assert_eq!(
            left.retain_terminal(
                &handle,
                InvocationResult::OrdinaryFailure(diagnostic("failed")),
            ),
            Err(AdmissionError::ExpiredHandle)
        );
    }

    #[test]
    fn restart_changes_runtime_generation_and_invalidates_prior_handles() {
        let mut runtime = Runtime::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let old_id = runtime.id().clone();
        let old_generation = runtime.generation();
        let old_handle = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        let new_id = runtime.restart();

        assert_ne!(new_id, old_id);
        assert_eq!(runtime.id(), &new_id);
        assert_eq!(runtime.generation(), old_generation + 1);
        assert_eq!(
            runtime.check_handle(&old_handle),
            Err(AdmissionError::ForeignRuntime)
        );
        let new_handle = match runtime.admit(request).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => panic!("a restarted owner must not replay prior idempotency state"),
        };
        assert_ne!(new_handle.runtime(), old_handle.runtime());
        assert_eq!(new_handle.invocation(), old_handle.invocation());
    }

    #[test]
    fn supervisor_exposes_the_current_runtime_generation() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let old_id = supervisor.id().unwrap();
        assert_eq!(supervisor.generation(), Ok(1));

        let new_id = supervisor.restart().unwrap();

        assert_ne!(new_id, old_id);
        assert_eq!(supervisor.id(), Ok(new_id));
        assert_eq!(supervisor.generation(), Ok(2));
    }

    #[test]
    fn restart_cancels_and_joins_started_workers_before_rotating_owner() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        let observed = Arc::new(AtomicBool::new(false));
        let handle = supervisor
            .start(
                request,
                CancellableExecutor {
                    observed: Arc::clone(&observed),
                },
            )
            .unwrap();

        let new_id = supervisor.restart().unwrap();

        assert!(observed.load(Ordering::SeqCst));
        assert_ne!(handle.runtime(), &new_id);
        assert_eq!(
            supervisor.state(&handle),
            Err(AdmissionError::ForeignRuntime)
        );
    }

    #[test]
    fn expiration_releases_idempotency_key_with_its_invocation() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let first = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime.expire(&first).unwrap();

        let second = match runtime.admit(request).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => panic!("expired work must not replay as active or terminal"),
        };
        assert_ne!(second.invocation(), first.invocation());
        assert_eq!(runtime.check_handle(&second), Ok(()));
    }
    #[test]
    fn cancellation_is_not_ordinary_failure() {
        let cancelled: InvocationResult<()> = InvocationResult::Cancelled(Some(Diagnostic {
            code: "cancelled",
            message: "cancelled",
            fields: BTreeMap::new(),
        }));
        assert_eq!(cancelled.ordinary_failure(), None);
        assert_eq!(cancelled.terminal_class(), TerminalClass::Cancelled);
    }
    #[test]
    fn cancellation_request_is_idempotent_and_preserves_terminal_race() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        let reason = diagnostic("cancelled");

        assert!(runtime.cancel(&handle, Some(reason.clone())).unwrap());
        assert!(runtime.cancel(&handle, Some(diagnostic("other"))).unwrap());
        assert!(runtime.cancellation_requested(&handle).unwrap());
        assert_eq!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Active)
        );

        runtime
            .retain_terminal(&handle, InvocationResult::Cancelled(None))
            .unwrap();
        assert!(runtime.cancel(&handle, None).unwrap());
        assert_eq!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Cancelled(Some(reason))
            ))
        );

        let mut completed = Runtime::new(RuntimeId::new("completed"));
        let completed_handle = match completed
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        assert!(completed.cancel(&completed_handle, None).unwrap());
        completed
            .retain_terminal(
                &completed_handle,
                InvocationResult::Success(value("Str", "done")),
            )
            .unwrap();
        assert!(!completed.cancel(&completed_handle, None).unwrap());
        assert!(matches!(
            completed.invocation_state(&completed_handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ));
    }
    #[test]
    fn committed_terminal_classification_wins_over_late_cancellation() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut first = request(Some(value("Int", "1")), ArgumentMap::default());
        first.idempotency_key = Some("key".into());
        let handle = match runtime.admit(first.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime
            .classify_terminal(&handle, TerminalClass::Succeeded)
            .unwrap();
        runtime
            .classify_terminal(&handle, TerminalClass::Cancelled)
            .unwrap();

        assert!(matches!(
            runtime.admit(first).unwrap(),
            Admission::Terminal {
                outcome: TerminalClass::Succeeded,
                result: RetainedInvocationResult::ClassificationOnly(TerminalClass::Succeeded),
                ..
            }
        ));
    }
    #[test]
    fn retained_terminal_result_is_absorbing() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut first = request(Some(value("Int", "1")), ArgumentMap::default());
        first.idempotency_key = Some("key".into());
        let handle = match runtime.admit(first.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime
            .retain_terminal(&handle, InvocationResult::Success(value("Str", "answer")))
            .unwrap();
        runtime
            .retain_terminal(
                &handle,
                InvocationResult::Cancelled(Some(diagnostic("cancelled"))),
            )
            .unwrap();

        assert!(matches!(
            runtime.admit(first).unwrap(),
            Admission::Terminal {
                outcome: TerminalClass::Succeeded,
                result: RetainedInvocationResult::Success(ref value),
                ..
            } if value.canonical() == Some(b"answer".as_slice())
        ));
    }
    #[test]
    fn terminal_result_type_is_validated_before_retention() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        assert_eq!(
            runtime.retain_terminal(
                &handle,
                InvocationResult::Success(value("Int", "not-a-string")),
            ),
            Err(AdmissionError::ReturnType)
        );
        assert_eq!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Active)
        );
    }
    #[test]
    fn failure_cancellation_and_orphan_are_complete_terminal_results() {
        let outcomes = [
            InvocationResult::OrdinaryFailure(diagnostic("failed")),
            InvocationResult::Cancelled(Some(diagnostic("cancelled"))),
            InvocationResult::Orphaned(Some(diagnostic("orphaned"))),
        ];
        let expected = [
            TerminalClass::Failed,
            TerminalClass::Cancelled,
            TerminalClass::Orphaned,
        ];
        let retained_outcomes = [
            RetainedInvocationResult::OrdinaryFailure(diagnostic("failed")),
            RetainedInvocationResult::Cancelled(Some(diagnostic("cancelled"))),
            RetainedInvocationResult::Orphaned(Some(diagnostic("orphaned"))),
        ];

        for (index, ((result, terminal_class), expected_result)) in outcomes
            .into_iter()
            .zip(expected)
            .zip(retained_outcomes)
            .enumerate()
        {
            let mut runtime = Runtime::new(RuntimeId::new(format!("r-{index}")));
            let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
            request.idempotency_key = Some(format!("key-{index}"));
            let handle = match runtime.admit(request.clone()).unwrap() {
                Admission::New { handle, .. } => handle,
                _ => unreachable!(),
            };
            runtime.retain_terminal(&handle, result).unwrap();

            let InvocationState::Terminal(retained) = runtime.invocation_state(&handle).unwrap()
            else {
                panic!("expected terminal state");
            };
            assert_eq!(retained.terminal_class(), terminal_class);
            assert_eq!(retained, expected_result);
            assert!(matches!(
                runtime.admit(request).unwrap(),
                Admission::Terminal {
                    outcome,
                    result,
                    ..
                } if outcome == terminal_class && result == expected_result
            ));
        }
    }
    #[test]
    fn protected_success_discards_canonical_bytes_before_retention() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime
            .retain_terminal(
                &handle,
                InvocationResult::Success(TypedValue::protected(ty("Str"), "super-secret")),
            )
            .unwrap();
        let state = runtime.invocation_state(&handle).unwrap();
        let InvocationState::Terminal(RetainedInvocationResult::Success(value)) = &state else {
            panic!("expected retained success");
        };
        assert!(value.is_redacted());
        assert_eq!(value.canonical(), None);
        assert!(!format!("{state:?}").contains("super-secret"));
    }
    #[test]
    fn serialized_diagnostics_redact_secrets() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "token".into(),
            DiagnosticField::Redacted {
                static_type: ty("sys.Secret"),
            },
        );
        let json = serde_json::to_string(&Diagnostic {
            code: "sys.invoke.argument_type",
            message: "reflective invocation admission rejected",
            fields,
        })
        .unwrap();
        assert!(json.contains("redacted"));
        assert!(!json.contains("super-secret"));
    }
}
