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
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, ThreadId},
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
identity!(SnapshotId);
identity!(RuntimeId);
identity!(InvocationId);
identity!(TypeId);

/// Runtime-issued descriptive reference to an invocation.
///
/// This is deliberately not a durable `sys.RowRef<sys.Invocation>`: this
/// local supervisor has neither a catalogue nor row-authority evidence. It
/// can describe an invocation without granting the ability to operate on it.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct InvocationRef(InvocationId);

impl InvocationRef {
    fn from_id(id: InvocationId) -> Self {
        Self(id)
    }

    fn id(&self) -> &InvocationId {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Runtime-issued descriptive reference to a static result type.
///
/// As with [`InvocationRef`], this is a nominal description only, not a
/// catalogue-authorized durable type-row reference.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct TypeRef(TypeId);

impl TypeRef {
    fn from_id(id: TypeId) -> Self {
        Self(id)
    }

    fn id(&self) -> &TypeId {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Opaque identity for one immutable semantic revision.
///
/// Its 32 bytes are supplied by the authoritative revision producer. This
/// type intentionally neither derives those bytes nor treats them as another
/// identifier or digest type.
///
/// The derived `Serialize` implementation is an internal, non-normative
/// projection, including when a caller selects JSON. It is not a portable wire
/// encoding. Portable `sys.RevisionId` values use the separate OVB tag-60023
/// `[qualified_type_name, representation]` boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct RevisionId([u8; 32]);

impl RevisionId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

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
    /// Returns public OVB-1 bytes for a bounded pure evaluator.
    ///
    /// Protected values deliberately never cross the source-evaluation seam.
    pub fn canonical(&self) -> Option<&[u8]> {
        (!self.redacted).then_some(self.canonical.as_slice())
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

    /// Returns the retained, redaction-safe metadata for the bound arguments.
    ///
    /// `ArgumentMap` is backed by a `BTreeMap`, so positions follow the same
    /// canonical name order used for invocation identity hashing. Public
    /// values retain their canonical bytes; protected values retain only
    /// presence, type and redaction metadata and never expose their payload.
    fn metadata(&self) -> Vec<InvocationArgumentMetadata> {
        self.entries
            .iter()
            .enumerate()
            .map(|(position, (name, value))| InvocationArgumentMetadata {
                name: name.clone(),
                position,
                static_type: value.static_type.clone(),
                value: (!value.is_redacted()).then(|| value.canonical.clone()),
                redacted: value.is_redacted(),
            })
            .collect()
    }
}

/// Safe metadata retained for one admitted invocation argument.
///
/// This is a local observation helper, not a portable `sys.InvocationArgument`
/// row: it deliberately carries no synthesized row reference. Protected
/// payloads are never retained in observation metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct InvocationArgumentMetadata {
    name: String,
    position: usize,
    static_type: TypeId,
    value: Option<Vec<u8>>,
    redacted: bool,
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

#[derive(Clone, Eq, PartialEq)]
pub struct InvocationIdentity(String);
impl fmt::Debug for InvocationIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvocationIdentity(<withheld>)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionBoundary {
    pub invocation: InvocationId,
    identity: InvocationIdentity,
    pub function: FunctionIdentity,
    pub arguments: ArgumentMap,
    pub mode: InvocationMode,
    pub transaction: TransactionMode,
    pub context: InvocationContext,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationHandle<T> {
    pub invocation: InvocationRef,
    pub runtime: RuntimeId,
    pub result_type: TypeRef,
    pub resumable: bool,
    marker: PhantomData<fn() -> T>,
}
impl<T> InvocationHandle<T> {
    pub fn invocation(&self) -> &InvocationRef {
        &self.invocation
    }
    pub fn runtime(&self) -> &RuntimeId {
        &self.runtime
    }
    pub fn result_type(&self) -> &TypeRef {
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
/// Complete, terminal result returned by `sys.await`.
///
/// `value` is present only for `Succeeded`; `failure` is present only for
/// `Failed` or may explain `Cancelled`/`Orphaned`. `Queued` and `Running` are
/// deliberately not constructible through this result type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationResult<T> {
    pub invocation: InvocationRef,
    pub status: InvocationStatus,
    pub value: Option<T>,
    pub failure: Option<Diagnostic>,
    pub started: Option<Instant>,
    pub ended: Instant,
    pub duration: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum InvocationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Orphaned,
}

impl InvocationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Orphaned
        )
    }
}

/// Closed executor-side completion protocol.
///
/// This stays internal to the supervisor. Unlike [`InvocationResult`], it
/// has no observation identity/timing fields and cannot represent a live
/// status. Public executor implementations still return the API-shaped
/// `InvocationResult`; it is validated and translated at this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ExecutionResult<T> {
    Success(T),
    OrdinaryFailure(Diagnostic),
    Cancelled(Option<Diagnostic>),
    Orphaned(Option<Diagnostic>),
}

impl<T> TryFrom<InvocationResult<T>> for ExecutionResult<T> {
    type Error = ();

    fn try_from(result: InvocationResult<T>) -> Result<Self, Self::Error> {
        match result {
            InvocationResult {
                status: InvocationStatus::Succeeded,
                value: Some(value),
                failure: None,
                ..
            } => Ok(Self::Success(value)),
            InvocationResult {
                status: InvocationStatus::Failed,
                value: None,
                failure: Some(failure),
                ..
            } => Ok(Self::OrdinaryFailure(failure)),
            InvocationResult {
                status: InvocationStatus::Cancelled,
                value: None,
                failure,
                ..
            } => Ok(Self::Cancelled(failure)),
            InvocationResult {
                status: InvocationStatus::Orphaned,
                value: None,
                failure,
                ..
            } => Ok(Self::Orphaned(failure)),
            _ => Err(()),
        }
    }
}

#[allow(non_snake_case)]
impl<T> InvocationResult<T> {
    fn executor_terminal(
        status: InvocationStatus,
        value: Option<T>,
        failure: Option<Diagnostic>,
    ) -> Self {
        Self {
            invocation: InvocationRef::from_id(InvocationId::new("executor-completion")),
            status,
            value,
            failure,
            started: None,
            ended: Instant::now(),
            duration: None,
        }
    }

    pub fn Success(value: T) -> Self {
        Self::executor_terminal(InvocationStatus::Succeeded, Some(value), None)
    }

    pub fn OrdinaryFailure(failure: Diagnostic) -> Self {
        Self::executor_terminal(InvocationStatus::Failed, None, Some(failure))
    }

    pub fn Cancelled(failure: Option<Diagnostic>) -> Self {
        Self::executor_terminal(InvocationStatus::Cancelled, None, failure)
    }

    pub fn Orphaned(failure: Option<Diagnostic>) -> Self {
        Self::executor_terminal(InvocationStatus::Orphaned, None, failure)
    }
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
}
impl RetainedInvocationResult {
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
pub enum InvocationState {
    Active,
    Terminal(RetainedInvocationResult),
}
impl<T> InvocationResult<T> {
    pub fn ordinary_failure(&self) -> Option<&Diagnostic> {
        (self.status == InvocationStatus::Failed)
            .then_some(self.failure.as_ref())
            .flatten()
    }
    pub fn terminal_class(&self) -> TerminalClass {
        match self.status {
            InvocationStatus::Succeeded => TerminalClass::Succeeded,
            InvocationStatus::Failed => TerminalClass::Failed,
            InvocationStatus::Cancelled => TerminalClass::Cancelled,
            InvocationStatus::Orphaned => TerminalClass::Orphaned,
            InvocationStatus::Queued | InvocationStatus::Running => {
                panic!("an executor completion must be terminal")
            }
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
    /// previous generation. Terminal observations remain available to a
    /// matching idempotent request; work that was still active is retained as
    /// an orphaned classification and is never re-executed.
    pub fn restart(&mut self) -> RuntimeId {
        self.generation = self.generation.saturating_add(1);
        self.id = RuntimeId::new(format!("{}@{}", self.id.as_str(), self.generation));
        for invocation in self.invocations.values_mut() {
            if invocation.terminal.is_none() {
                invocation.terminal = Some(RetainedInvocationResult::Orphaned(None));
                invocation.ended = Some(Instant::now());
            }
        }
        for entry in self.idempotency.values_mut() {
            if entry.terminal.is_none() {
                entry.terminal = self
                    .invocations
                    .get(&entry.invocation)
                    .and_then(|invocation| invocation.terminal.clone());
            }
        }
        self.id.clone()
    }
    pub fn admit<T>(
        &mut self,
        request: AdmissionRequest<T>,
    ) -> Result<Admission<T>, AdmissionError> {
        validate_target(&request)?;
        let bound = bind(&request.function, &request.arguments)?;
        validate_execution(&request)?;
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
        let metadata = bound.metadata();
        let next_invocation = self
            .next_invocation
            .checked_add(1)
            .ok_or(AdmissionError::RuntimeUnavailable)?;
        self.next_invocation = next_invocation;
        let invocation = InvocationId::new(format!("invocation-{next_invocation}"));
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
                arguments: metadata,
                terminal: None,
                started: Instant::now(),
                ended: None,
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
        if matches!(outcome, TerminalClass::Succeeded | TerminalClass::Failed) {
            return Err(AdmissionError::TerminalInvariant);
        }
        self.store_terminal(
            handle,
            match outcome {
                TerminalClass::Cancelled => RetainedInvocationResult::Cancelled(None),
                TerminalClass::Orphaned => RetainedInvocationResult::Orphaned(None),
                TerminalClass::Succeeded | TerminalClass::Failed => unreachable!("validated above"),
            },
        )
    }
    pub fn retain_terminal<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        result: InvocationResult<TypedValue>,
    ) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        let completion = match ExecutionResult::try_from(result) {
            Ok(completion) => completion,
            Err(()) => {
                // A malformed executor reply is not an observation timeout:
                // this synchronous owner has completed its callback and must
                // never leave the admitted invocation active.
                self.store_terminal(handle, malformed_completion_failure())?;
                return Err(AdmissionError::MalformedCompletion);
            }
        };
        self.retain_completion(handle, completion)
    }

    fn retain_completion<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        completion: ExecutionResult<TypedValue>,
    ) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        let cancellation_reason = self
            .invocations
            .get(handle.invocation.id())
            .expect("checked")
            .cancellation_reason
            .clone();
        if self
            .invocations
            .get(handle.invocation.id())
            .expect("checked")
            .terminal
            .is_some()
        {
            return Ok(());
        }
        let result = match completion {
            ExecutionResult::Success(value) => {
                if value.static_type() != handle.result_type().id() {
                    self.store_terminal(
                        handle,
                        RetainedInvocationResult::OrdinaryFailure(Diagnostic {
                            code: "sys.invoke.return_type",
                            message: "invocation returned a value with an incompatible type",
                            fields: BTreeMap::new(),
                        }),
                    )?;
                    return Err(AdmissionError::ReturnType);
                }
                RetainedInvocationResult::Success(RetainedValue::new(value))
            }
            ExecutionResult::OrdinaryFailure(diagnostic) => {
                RetainedInvocationResult::OrdinaryFailure(diagnostic)
            }
            ExecutionResult::Cancelled(diagnostic) => {
                RetainedInvocationResult::Cancelled(diagnostic.or(cancellation_reason))
            }
            ExecutionResult::Orphaned(diagnostic) => RetainedInvocationResult::Orphaned(diagnostic),
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
                .get(handle.invocation.id())
                .expect("checked")
                .terminal
                .clone()
            {
                Some(result) => InvocationState::Terminal(result),
                None => InvocationState::Active,
            },
        )
    }

    fn terminal_result<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<Option<InvocationResult<RetainedValue>>, AdmissionError> {
        self.check_handle(handle)?;
        let stored = self
            .invocations
            .get(handle.invocation.id())
            .expect("checked");
        let Some(result) = stored.terminal.as_ref() else {
            return Ok(None);
        };
        let ended = stored.ended.expect("terminal results have an end time");
        let (status, value, failure) = match result {
            RetainedInvocationResult::Success(value) => {
                (InvocationStatus::Succeeded, Some(value.clone()), None)
            }
            RetainedInvocationResult::OrdinaryFailure(failure) => {
                (InvocationStatus::Failed, None, Some(failure.clone()))
            }
            RetainedInvocationResult::Cancelled(failure) => {
                (InvocationStatus::Cancelled, None, failure.clone())
            }
            RetainedInvocationResult::Orphaned(failure) => {
                (InvocationStatus::Orphaned, None, failure.clone())
            }
        };
        Ok(Some(InvocationResult {
            invocation: handle.invocation.clone(),
            status,
            value,
            failure,
            started: Some(stored.started),
            ended,
            duration: ended.checked_duration_since(stored.started),
        }))
    }
    /// Requests cancellation without converting an active invocation into an
    /// ordinary failure. The target remains active until its owner records a
    /// terminal result; cancellation wins unless that result was already
    /// committed.
    pub fn cancel<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        reason: Option<Diagnostic>,
    ) -> Result<bool, AdmissionError> {
        self.check_handle(handle)?;
        let invocation = self
            .invocations
            .get_mut(handle.invocation.id())
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
            .get(handle.invocation.id())
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
            .get(handle.invocation.id())
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
            .get_mut(handle.invocation.id())
            .expect("checked");
        if stored.terminal.is_some() {
            return Ok(());
        }
        let result = if stored.cancellation_requested
            && result.terminal_class() != TerminalClass::Cancelled
        {
            RetainedInvocationResult::Cancelled(stored.cancellation_reason.clone())
        } else {
            result
        };
        stored.terminal = Some(result.clone());
        stored.ended = Some(Instant::now());
        for entry in self
            .idempotency
            .values_mut()
            .filter(|entry| entry.invocation == *handle.invocation.id())
        {
            entry.terminal = Some(result.clone());
        }
        Ok(())
    }
    pub fn check_handle<T>(&self, handle: &InvocationHandle<T>) -> Result<(), AdmissionError> {
        if handle.resumable {
            return Err(AdmissionError::ExpiredHandle);
        }
        if handle.runtime != self.id {
            return Err(AdmissionError::ForeignRuntime);
        }
        let stored = self
            .invocations
            .get(handle.invocation.id())
            .ok_or(AdmissionError::ExpiredHandle)?;
        if stored.result_type != *handle.result_type.id() {
            return Err(AdmissionError::ExpiredHandle);
        }
        Ok(())
    }
    pub fn expire<T>(&mut self, handle: &InvocationHandle<T>) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        let Some(result) = self
            .invocations
            .get(handle.invocation.id())
            .expect("checked")
            .terminal
            .as_ref()
            .cloned()
        else {
            return Ok(());
        };
        for entry in self
            .idempotency
            .values_mut()
            .filter(|entry| entry.invocation == *handle.invocation.id())
        {
            entry.terminal = Some(result.clone());
        }
        self.invocations.remove(handle.invocation.id());
        Ok(())
    }
    fn handle<T>(&self, invocation: &InvocationId, result_type: TypeId) -> InvocationHandle<T> {
        InvocationHandle {
            invocation: InvocationRef::from_id(invocation.clone()),
            runtime: self.id.clone(),
            result_type: TypeRef::from_id(result_type),
            resumable: false,
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
    lifecycle: Arc<Lifecycle>,
    synchronous: Arc<SynchronousExecutions>,
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
struct SynchronousExecutions {
    state: Mutex<SynchronousExecutionState>,
    completed: Condvar,
    #[cfg(test)]
    enter_hook: Mutex<Option<EnterHook>>,
}

#[derive(Debug, Default)]
struct SynchronousExecutionState {
    active: Vec<ThreadId>,
    pending: usize,
}

/// Prevents a newly spawned worker from entering user code until the caller
/// has finished publishing its join handle. This keeps the lifecycle fence
/// out of the callback's reentrant call graph without creating an untracked
/// execution window.
#[derive(Debug, Default)]
struct WorkerStartGate {
    open: Mutex<Option<bool>>,
    changed: Condvar,
}

impl WorkerStartGate {
    fn wait(&self) -> bool {
        let Ok(mut open) = self.open.lock() else {
            return false;
        };
        while open.is_none() {
            let Ok(next) = self.changed.wait(open) else {
                return false;
            };
            open = next;
        }
        *open == Some(true)
    }

    fn release(&self) {
        if let Ok(mut open) = self.open.lock()
            && open.is_none()
        {
            *open = Some(true);
            self.changed.notify_all();
        }
    }

    fn abort(&self) {
        if let Ok(mut open) = self.open.lock()
            && open.is_none()
        {
            *open = Some(false);
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

    fn wait_empty(&self) -> Result<(), AdmissionError> {
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

    fn reserve(self: &Arc<Self>) -> Result<SynchronousReservation, AdmissionError> {
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
        if let Ok(mut state) = self.state.lock() {
            let current = thread::current().id();
            if let Some(index) = state.active.iter().position(|thread| *thread == current) {
                state.active.remove(index);
            }
            self.completed.notify_all();
        }
    }

    #[cfg(test)]
    fn set_enter_hook(&self, hook: Option<EnterHook>) {
        *self.enter_hook.lock().unwrap() = hook;
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.state.lock().unwrap().pending
    }
}

struct SynchronousReservation {
    executions: Arc<SynchronousExecutions>,
    reserved: bool,
}

impl SynchronousReservation {
    fn begin(mut self) -> Result<SynchronousExecution, AdmissionError> {
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
        if let Ok(mut state) = self.executions.state.lock()
            && let Some(pending) = state.pending.checked_sub(1)
        {
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
                let cancellation = {
                    self.runtime
                        .lock()
                        .map_err(|_| AdmissionError::RuntimeUnavailable)?
                        .cancellation_token(&handle)?
                };
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
        mut executor: E,
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
        let (handle, boundary) = {
            let mut runtime = self
                .runtime
                .lock()
                .map_err(|_| AdmissionError::RuntimeUnavailable)?;
            match runtime.admit(request)? {
                Admission::New { boundary, handle } => {
                    let worker_handle = runtime
                        .handle::<()>(handle.invocation.id(), handle.result_type.id().clone());
                    let cancellation = runtime.cancellation_token(&worker_handle)?;
                    (handle, Some((boundary, worker_handle, cancellation)))
                }
                Admission::Active { handle } | Admission::Terminal { handle, .. } => (handle, None),
            }
        };

        if let Some((boundary, worker_handle, cancellation)) = boundary {
            let runtime = Arc::clone(&self.runtime);
            let synchronous = Arc::clone(&self.synchronous);
            let reservation = synchronous.reserve()?;
            let worker_handle_for_worker = worker_handle.clone();
            let start_gate = Arc::new(WorkerStartGate::default());
            let start_gate_for_worker = Arc::clone(&start_gate);
            let worker = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                thread::spawn(move || {
                    if !start_gate_for_worker.wait() {
                        return;
                    }
                    let execution = match reservation.begin() {
                        Ok(execution) => execution,
                        Err(_) => return,
                    };
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
                    if runtime
                        .retain_terminal(&worker_handle_for_worker, result)
                        .is_err()
                    {
                        let _ = runtime
                            .classify_terminal(&worker_handle_for_worker, TerminalClass::Orphaned);
                    }
                    drop(execution);
                })
            })) {
                Ok(worker) => worker,
                Err(_) => {
                    let mut runtime = self
                        .runtime
                        .lock()
                        .map_err(|_| AdmissionError::RuntimeUnavailable)?;
                    runtime.classify_terminal(&worker_handle, TerminalClass::Orphaned)?;
                    return Ok(handle);
                }
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
                    start_gate.release();
                }
                Err(error) => {
                    drop(lifecycle);
                    start_gate.abort();
                    let _ = worker.join();
                    self.runtime
                        .lock()
                        .map_err(|_| AdmissionError::RuntimeUnavailable)?
                        .classify_terminal(&worker_handle, TerminalClass::Orphaned)?;
                    return Err(error);
                }
            }
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
                        let worker = self
                            .workers
                            .lock()
                            .ok()
                            .and_then(|mut workers| workers.remove(handle.invocation().id()));
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredInvocation {
    result_type: TypeId,
    arguments: Vec<InvocationArgumentMetadata>,
    terminal: Option<RetainedInvocationResult>,
    started: Instant,
    ended: Option<Instant>,
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
                Some(value) if value.static_type() == &parameter.static_type => {
                    bound.push(Argument {
                        name: parameter.name.clone(),
                        value: value.clone(),
                    })
                }
                Some(_) => {
                    return Err(AdmissionError::ArgumentType {
                        name: parameter.name.clone(),
                        detail: ArgumentTypeDetail::Mismatch,
                    });
                }
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
fn validate_target<T>(request: &AdmissionRequest<T>) -> Result<(), AdmissionError> {
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
    Ok(())
}
fn validate_execution<T>(request: &AdmissionRequest<T>) -> Result<(), AdmissionError> {
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
    append(&mut bytes, request.function.identity.revision.as_bytes());
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

/// The executor response was structurally unusable.  Do not retain any
/// caller-provided diagnostic, identity, timing, or value from that response.
fn malformed_completion_failure() -> RetainedInvocationResult {
    RetainedInvocationResult::OrdinaryFailure(Diagnostic {
        code: "sys.invoke.malformed_completion",
        message: "invocation executor returned an invalid terminal completion",
        fields: BTreeMap::new(),
    })
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
    SourceMismatch,
    TransactionMode,
    IdempotencyMismatch,
    StartMode,
    ForeignRuntime,
    ExpiredHandle,
    MalformedCompletion,
    TerminalInvariant,
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
            Self::SourceMismatch => "sys.invoke.source_mismatch",
            Self::TransactionMode => "sys.invoke.transaction_mode",
            Self::IdempotencyMismatch => "sys.invoke.idempotency_mismatch",
            Self::StartMode => "sys.invoke.start_mode",
            Self::ForeignRuntime => "sys.handle.foreign_runtime",
            Self::ExpiredHandle => "sys.handle.expired",
            Self::MalformedCompletion => "sys.invoke.malformed_completion",
            Self::TerminalInvariant => "sys.runtime.unavailable",
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
    use std::sync::mpsc;
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
                revision: RevisionId::from_bytes([0x11; 32]),
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
    #[test]
    fn revision_id_is_an_exact_byte_oriented_nominal_value() {
        let bytes = [0x7e; 32];
        let revision = RevisionId::from_bytes(bytes);

        let _: fn([u8; 32]) -> RevisionId = RevisionId::from_bytes;
        assert_eq!(revision.as_bytes(), &bytes);
        assert_eq!(revision.into_bytes(), bytes);
    }
    #[test]
    fn changing_one_revision_byte_changes_the_invocation_identity() {
        let request = request(Some(value("Int", "1")), ArgumentMap::default());
        let bound = bind(&request.function, &request.arguments).unwrap();
        let original = identity(&request, &bound);

        let mut changed = request;
        let mut revision = *changed.function.identity.revision.as_bytes();
        revision[0] ^= 1;
        changed.function.identity.revision = RevisionId::from_bytes(revision);
        let changed_bound = bind(&changed.function, &changed.arguments).unwrap();

        assert_ne!(original, identity(&changed, &changed_bound));
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
    struct CountingExecutor {
        calls: Arc<AtomicUsize>,
    }
    impl InvocationExecutor for CountingExecutor {
        fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            InvocationResult::Success(value("Str", "unexpected"))
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
    struct ReentrantExecutor {
        supervisor: RuntimeSupervisor,
        restarted: Arc<AtomicBool>,
    }
    impl InvocationExecutor for ReentrantExecutor {
        fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
            self.restarted
                .store(self.supervisor.restart().is_err(), Ordering::SeqCst);
            InvocationResult::Success(value("Str", "stale"))
        }
    }
    struct PanicExecutor {
        calls: Arc<AtomicUsize>,
    }
    impl InvocationExecutor for PanicExecutor {
        fn execute(&mut self, _: &ExecutionBoundary) -> InvocationResult<TypedValue> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("executor panic")
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
    fn rejects_mismatched_default_before_admission() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let result = runtime.admit(request(
            Some(value("Str", "not-an-int")),
            ArgumentMap::default(),
        ));

        assert!(matches!(
            result,
            Err(AdmissionError::ArgumentType {
                name,
                detail: ArgumentTypeDetail::Mismatch,
            }) if name == "a"
        ));
        assert!(runtime.invocations.is_empty());
    }
    #[test]
    fn protected_argument_identities_remain_private_but_enforce_idempotency() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut first = request(
            None,
            args(vec![Argument {
                name: "a".into(),
                value: TypedValue::protected(ty("Int"), "1234"),
            }]),
        );
        first.idempotency_key = Some("key".into());

        let boundary = match runtime.admit(first.clone()).unwrap() {
            Admission::New { boundary, .. } => boundary,
            _ => unreachable!(),
        };
        let rendered = format!("{boundary:?}");
        assert!(rendered.contains("InvocationIdentity(<withheld>)"));
        assert!(!rendered.contains("1234"));
        assert!(!rendered.contains(boundary.identity.0.as_str()));

        let mut different_secret = first;
        different_secret.arguments = args(vec![Argument {
            name: "a".into(),
            value: TypedValue::protected(ty("Int"), "5678"),
        }]);
        assert!(matches!(
            runtime.admit(different_secret),
            Err(AdmissionError::IdempotencyMismatch)
        ));
    }

    #[test]
    fn retained_argument_metadata_is_ordered_and_redaction_safe() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(
            None,
            args(vec![
                Argument {
                    name: "b".into(),
                    value: TypedValue::protected(ty("Str"), "super-secret"),
                },
                Argument {
                    name: "a".into(),
                    value: value("Int", "42"),
                },
            ]),
        );
        request.function.parameters = vec![
            Parameter {
                name: "b".into(),
                static_type: ty("Str"),
                default: None,
            },
            Parameter {
                name: "a".into(),
                static_type: ty("Int"),
                default: None,
            },
        ];

        let handle = match runtime.admit(request).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!("a new request must create one retained observation"),
        };
        let metadata = runtime
            .invocations
            .get(handle.invocation.id())
            .expect("admitted invocation row")
            .arguments
            .clone();

        assert_eq!(metadata.len(), 2);
        assert_eq!(metadata[0].name, "a");
        assert_eq!(metadata[0].position, 0);
        assert_eq!(metadata[0].static_type, ty("Int"));
        assert_eq!(metadata[0].value, Some(b"42".to_vec()));
        assert!(!metadata[0].redacted);

        assert_eq!(metadata[1].name, "b");
        assert_eq!(metadata[1].position, 1);
        assert_eq!(metadata[1].static_type, ty("Str"));
        assert_eq!(metadata[1].value, None);
        assert!(metadata[1].redacted);

        let rendered = format!("{metadata:?}");
        assert!(!rendered.contains("super-secret"));
        assert!(!format!("{runtime:?}").contains("digest:"));
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
        let mut wrong_argument_and_result = request(
            None,
            args(vec![Argument {
                name: "a".into(),
                value: value("Str", "bad"),
            }]),
        );
        wrong_argument_and_result.witness = TypeWitness::new(ty("Int"));
        assert!(matches!(
            runtime.admit(wrong_argument_and_result),
            Err(AdmissionError::ArgumentType { .. })
        ));
        assert!(runtime.invocations.is_empty());
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
                if retained.canonical() == Some(b"answer".as_slice())
        ));
        assert_eq!(after_restart.calls, 0);
    }

    #[test]
    fn supervised_run_does_not_block_cancel_or_timeout_observation() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let mut active_request = request(Some(value("Int", "1")), ArgumentMap::default());
        active_request.mode = InvocationMode::Start;
        active_request.transaction = TransactionMode::Separate;
        let active_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let active_handle = supervisor
            .start(
                active_request,
                BlockingExecutor {
                    gate: Arc::clone(&active_gate),
                    calls: Arc::new(AtomicUsize::new(0)),
                    result: InvocationResult::Success(value("Str", "active")),
                },
            )
            .unwrap();

        let run_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let run_calls = Arc::new(AtomicUsize::new(0));
        let run_thread = {
            let supervisor = supervisor.clone();
            let run_gate = Arc::clone(&run_gate);
            let run_calls = Arc::clone(&run_calls);
            thread::spawn(move || {
                let mut executor = BlockingExecutor {
                    gate: run_gate,
                    calls: run_calls,
                    result: InvocationResult::Success(value("Str", "run")),
                };
                supervisor.run(
                    request(Some(value("Int", "2")), ArgumentMap::default()),
                    &mut executor,
                )
            })
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        while run_calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(run_calls.load(Ordering::SeqCst), 1);

        let (await_sender, await_receiver) = mpsc::channel();
        let await_supervisor = supervisor.clone();
        let await_handle = active_handle.clone();
        let await_thread = thread::spawn(move || {
            let result =
                await_supervisor.await_invocation(&await_handle, Some(Duration::from_millis(20)));
            await_sender.send(result).unwrap();
        });
        assert!(matches!(
            await_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Err(AwaitError::Timeout))
        ));

        let (cancel_sender, cancel_receiver) = mpsc::channel();
        let cancel_supervisor = supervisor.clone();
        let cancel_handle = active_handle.clone();
        let cancel_thread = thread::spawn(move || {
            cancel_sender
                .send(cancel_supervisor.cancel(&cancel_handle, Some(diagnostic("cancelled"))))
                .unwrap();
        });
        assert_eq!(
            cancel_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(true))
        );

        let (active_open, active_wake) = &*active_gate;
        *active_open.lock().unwrap() = true;
        active_wake.notify_one();
        let (run_open, run_wake) = &*run_gate;
        *run_open.lock().unwrap() = true;
        run_wake.notify_one();
        await_thread.join().unwrap();
        cancel_thread.join().unwrap();
        assert!(matches!(
            supervisor.await_invocation(&active_handle, Some(Duration::from_secs(1))),
            Ok(super::InvocationResult {
                status: InvocationStatus::Cancelled,
                value: None,
                failure: Some(_),
                ..
            })
        ));
        let run_result = run_thread.join().unwrap();
        assert!(matches!(run_result, Ok(InvocationState::Terminal(_))));
    }

    #[test]
    fn supervised_run_rejects_reentrant_restart_without_rotating_owner() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("reentrant".into());
        let restarted = Arc::new(AtomicBool::new(false));

        let result = supervisor.run(
            request.clone(),
            &mut ReentrantExecutor {
                supervisor: supervisor.clone(),
                restarted: Arc::clone(&restarted),
            },
        );

        assert!(matches!(
            result,
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ));
        assert!(restarted.load(Ordering::SeqCst));
        assert_eq!(supervisor.generation(), Ok(1));
    }

    #[test]
    fn supervised_run_fences_restart_before_admission_and_execution() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let hook = Arc::new((Mutex::new((false, false)), Condvar::new()));
        supervisor
            .synchronous
            .set_enter_hook(Some(Arc::clone(&hook)));
        let run_supervisor = supervisor.clone();
        let run_thread = thread::spawn(move || {
            let mut executor = Executor {
                calls: 0,
                result: InvocationResult::Success(value("Str", "done")),
            };
            run_supervisor.run(
                request(Some(value("Int", "1")), ArgumentMap::default()),
                &mut executor,
            )
        });
        {
            let (state, wake) = &*hook;
            let mut state = state.lock().unwrap();
            while !state.0 {
                state = wake.wait(state).unwrap();
            }
        }
        let (restart_sender, restart_receiver) = mpsc::channel();
        let restart_supervisor = supervisor.clone();
        let restart_thread = thread::spawn(move || {
            restart_sender.send(restart_supervisor.restart()).unwrap();
        });
        assert!(
            restart_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
        {
            let (state, wake) = &*hook;
            state.lock().unwrap().1 = true;
            wake.notify_all();
        }
        supervisor.synchronous.set_enter_hook(None);
        assert!(matches!(
            run_thread.join().unwrap(),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ));
        assert_eq!(
            restart_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(RuntimeId::new("owner@2")))
        );
        restart_thread.join().unwrap();
    }

    #[test]
    fn supervised_run_replays_one_orphaned_result_after_executor_panic() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("panic".into());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut first = PanicExecutor {
            calls: Arc::clone(&calls),
        };
        assert!(matches!(
            supervisor.run(request.clone(), &mut first),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Orphaned(Some(_))
            ))
        ));
        let mut replay = PanicExecutor {
            calls: Arc::clone(&calls),
        };
        assert!(matches!(
            supervisor.run(request, &mut replay),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Orphaned(Some(_))
            ))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn restart_waits_for_a_running_synchronous_callback() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let run_thread = {
            let supervisor = supervisor.clone();
            let gate = Arc::clone(&gate);
            let calls = Arc::clone(&calls);
            thread::spawn(move || {
                let mut executor = BlockingExecutor {
                    gate,
                    calls,
                    result: InvocationResult::Success(value("Str", "done")),
                };
                supervisor.run(
                    request(Some(value("Int", "1")), ArgumentMap::default()),
                    &mut executor,
                )
            })
        };
        while calls.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }

        let (restart_sender, restart_receiver) = mpsc::channel();
        let restart_supervisor = supervisor.clone();
        let restart_thread = thread::spawn(move || {
            restart_sender.send(restart_supervisor.restart()).unwrap();
        });
        assert!(
            restart_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );

        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_one();
        assert!(matches!(
            run_thread.join().unwrap(),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Cancelled(_)
            ))
        ));
        assert_eq!(
            restart_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(RuntimeId::new("owner@2")))
        );
        restart_thread.join().unwrap();
    }

    #[test]
    fn restart_pending_does_not_block_callback_cancel_or_await() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let handle = supervisor
            .start(
                request,
                BlockingExecutor {
                    gate: Arc::clone(&gate),
                    calls: Arc::clone(&calls),
                    result: InvocationResult::Success(value("Str", "done")),
                },
            )
            .unwrap();
        while calls.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }

        let (restart_sender, restart_receiver) = mpsc::channel();
        let restart_supervisor = supervisor.clone();
        let restart_thread = thread::spawn(move || {
            restart_sender.send(restart_supervisor.restart()).unwrap();
        });
        assert!(
            restart_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );

        assert_eq!(
            supervisor.await_invocation(&handle, Some(Duration::ZERO)),
            Err(AwaitError::Timeout)
        );
        assert_eq!(
            supervisor.cancel(&handle, Some(diagnostic("cancelled"))),
            Ok(true)
        );
        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_one();

        assert_eq!(
            restart_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(RuntimeId::new("owner@2")))
        );
        assert_eq!(
            supervisor.state(&handle),
            Err(AdmissionError::ForeignRuntime)
        );
        restart_thread.join().unwrap();
    }

    #[test]
    fn synchronous_reservation_rolls_back_when_worker_is_not_started() {
        let executions = Arc::new(SynchronousExecutions::default());
        {
            let _reservation = executions.reserve().unwrap();
            assert_eq!(executions.pending(), 1);
        }
        assert_eq!(executions.pending(), 0);
        executions.wait_empty().unwrap();
    }

    #[test]
    fn restart_fences_admission_before_a_new_callback_can_begin() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("owner"));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = Arc::new((Mutex::new((false, false)), Condvar::new()));
        supervisor
            .synchronous
            .set_enter_hook(Some(Arc::clone(&hook)));

        let run_supervisor = supervisor.clone();
        let run_gate = Arc::clone(&gate);
        let run_calls = Arc::clone(&calls);
        let run_thread = thread::spawn(move || {
            let mut executor = BlockingExecutor {
                gate: run_gate,
                calls: run_calls,
                result: InvocationResult::Success(value("Str", "unexpected")),
            };
            run_supervisor.run(
                request(Some(value("Int", "1")), ArgumentMap::default()),
                &mut executor,
            )
        });

        {
            let (entered, wake) = &*hook;
            let mut state = entered.lock().unwrap();
            while !state.0 {
                state = wake.wait(state).unwrap();
            }
        }
        let (restart_sender, restart_receiver) = mpsc::channel();
        let restart_supervisor = supervisor.clone();
        let restart_thread = thread::spawn(move || {
            restart_sender.send(restart_supervisor.restart()).unwrap();
        });
        assert!(
            restart_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );

        {
            let (entered, wake) = &*hook;
            entered.lock().unwrap().1 = true;
            wake.notify_all();
        }
        supervisor.synchronous.set_enter_hook(None);
        let (open, wake) = &*gate;
        *open.lock().unwrap() = true;
        wake.notify_one();
        assert!(run_thread.join().unwrap().is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            restart_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(RuntimeId::new("owner@2")))
        );
        restart_thread.join().unwrap();
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

        let result = supervisor
            .await_invocation(&handle, Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(result.status, InvocationStatus::Cancelled);
        assert_eq!(result.value, None);
        assert_eq!(result.failure, Some(diagnostic("cancelled")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.cancel(&handle, None), Ok(true));

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
    fn supervised_start_registration_failure_retains_orphaned_identity() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
        let workers = Arc::clone(&supervisor.workers);
        let poisoner = thread::spawn(move || {
            let _workers = workers.lock().unwrap();
            panic!("poison worker registry");
        });
        assert!(poisoner.join().is_err());

        let mut initial_request = request(Some(value("Int", "1")), ArgumentMap::default());
        initial_request.mode = InvocationMode::Start;
        initial_request.transaction = TransactionMode::Separate;
        initial_request.idempotency_key = Some("registration-failure".into());
        let calls = Arc::new(AtomicUsize::new(0));
        assert_eq!(
            supervisor.start(
                initial_request.clone(),
                CountingExecutor {
                    calls: Arc::clone(&calls),
                },
            ),
            Err(AdmissionError::RuntimeUnavailable)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.synchronous.pending(), 0);

        let replay_calls = Arc::new(AtomicUsize::new(0));
        let replay = supervisor
            .start(
                initial_request,
                CountingExecutor {
                    calls: Arc::clone(&replay_calls),
                },
            )
            .unwrap();
        assert!(matches!(
            supervisor.state(&replay),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Orphaned(None)
            ))
        ));
        assert!(matches!(
            supervisor.await_invocation(&replay, Some(Duration::ZERO)),
            Ok(super::InvocationResult {
                status: InvocationStatus::Orphaned,
                value: None,
                failure: None,
                ..
            })
        ));
        assert!(supervisor.restart().is_ok());
        assert_eq!(replay_calls.load(Ordering::SeqCst), 0);

        let mut fresh_request = request(Some(value("Int", "2")), ArgumentMap::default());
        fresh_request.mode = InvocationMode::Start;
        fresh_request.transaction = TransactionMode::Separate;
        let fresh_calls = Arc::new(AtomicUsize::new(0));
        let fresh_handle = supervisor
            .start(
                fresh_request,
                CountingExecutor {
                    calls: Arc::clone(&fresh_calls),
                },
            )
            .unwrap();
        assert!(matches!(
            supervisor.await_invocation(&fresh_handle, Some(Duration::from_secs(1))),
            Ok(super::InvocationResult {
                status: InvocationStatus::Succeeded,
                value: Some(_),
                failure: None,
                ..
            })
        ));
        assert_eq!(fresh_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn supervised_await_reaps_the_completed_worker() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        let handle = supervisor
            .start(
                request,
                Executor {
                    calls: 0,
                    result: InvocationResult::Success(value("Str", "answer")),
                },
            )
            .unwrap();

        assert!(matches!(
            supervisor.await_invocation(&handle, Some(Duration::from_secs(1))),
            Ok(super::InvocationResult {
                status: InvocationStatus::Succeeded,
                value: Some(ref retained),
                failure: None,
                ..
            }) if retained.canonical() == Some(b"answer".as_slice())
        ));
        assert!(supervisor.workers.lock().unwrap().is_empty());
    }

    #[test]
    fn public_handle_and_await_result_have_the_portable_terminal_shape() {
        let supervisor = RuntimeSupervisor::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.mode = InvocationMode::Start;
        request.transaction = TransactionMode::Separate;
        let handle = supervisor
            .start(
                request,
                Executor {
                    calls: 0,
                    result: InvocationResult::Success(value("Str", "answer")),
                },
            )
            .unwrap();

        assert_eq!(handle.invocation, handle.invocation().clone());
        assert_eq!(handle.runtime, handle.runtime().clone());
        assert_eq!(handle.result_type, handle.result_type().clone());
        assert!(!handle.resumable);

        let result = supervisor
            .await_invocation(&handle, Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(result.invocation, handle.invocation);
        assert_eq!(result.status, InvocationStatus::Succeeded);
        assert!(result.status.is_terminal());
        assert!(result.value.is_some());
        assert_eq!(result.failure, None);
        assert!(result.started.is_some());
        assert!(result.ended >= result.started.unwrap());
        assert!(result.duration.is_some());
    }

    #[test]
    fn descriptive_refs_are_nominal_runtime_issued_tokens() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        let _: InvocationRef = handle.invocation.clone();
        let _: TypeRef = handle.result_type.clone();
        assert_eq!(handle.invocation.as_str(), "invocation-1");
        assert_eq!(handle.result_type.as_str(), "Str");
    }

    #[test]
    fn resumable_handles_are_rejected_even_when_the_public_field_is_mutated() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        handle.resumable = true;
        assert_eq!(
            runtime.check_handle(&handle),
            Err(AdmissionError::ExpiredHandle)
        );
        assert_eq!(
            runtime.invocation_state(&handle),
            Err(AdmissionError::ExpiredHandle)
        );
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

        let result = supervisor
            .await_invocation(&handle, Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(result.status, InvocationStatus::Cancelled);
        assert_eq!(result.value, None);
        assert_eq!(result.failure, Some(reason));
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
        let result = supervisor
            .await_invocation(&handle, Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(result.status, InvocationStatus::Cancelled);
        assert_eq!(result.value, None);
        assert_eq!(result.failure, None);
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
        left.retain_terminal(
            &handle,
            InvocationResult::OrdinaryFailure(diagnostic("failed")),
        )
        .unwrap();
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
    fn restart_changes_runtime_generation_and_retains_active_as_orphaned() {
        let mut runtime = Runtime::new(RuntimeId::new("owner"));
        let mut active_request = request(Some(value("Int", "1")), ArgumentMap::default());
        active_request.idempotency_key = Some("key".into());
        let old_id = runtime.id().clone();
        let old_generation = runtime.generation();
        let old_handle = match runtime.admit(active_request.clone()).unwrap() {
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
        assert_eq!(
            runtime.invocation_state(&old_handle),
            Err(AdmissionError::ForeignRuntime)
        );
        let new_handle = match runtime.admit(active_request.clone()).unwrap() {
            Admission::Terminal {
                handle,
                outcome: TerminalClass::Orphaned,
                result: RetainedInvocationResult::Orphaned(None),
            } => handle,
            _ => panic!("a restarted owner must retain the orphaned operation"),
        };
        assert_ne!(new_handle.runtime(), old_handle.runtime());
        assert_eq!(new_handle.invocation(), old_handle.invocation());

        let mut executor = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "must-not-run")),
        };
        assert!(matches!(
            runtime.run(active_request, &mut executor),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Orphaned(None)
            ))
        ));
        assert_eq!(executor.calls, 0);

        let mut next_request = request(Some(value("Int", "1")), ArgumentMap::default());
        next_request.idempotency_key = Some("next-key".into());
        let next_handle = match runtime.admit(next_request).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        assert_ne!(next_handle.invocation(), old_handle.invocation());
    }

    #[test]
    fn restart_preserves_terminal_result_and_fences_old_handle() {
        let mut runtime = Runtime::new(RuntimeId::new("owner"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let old_handle = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        runtime
            .retain_terminal(
                &old_handle,
                InvocationResult::Success(value("Str", "answer")),
            )
            .unwrap();

        runtime.restart();

        assert_eq!(
            runtime.check_handle(&old_handle),
            Err(AdmissionError::ForeignRuntime)
        );
        let mut executor = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "must-not-run")),
        };
        assert!(matches!(
            runtime.run(request, &mut executor),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(ref value)
            )) if value.canonical() == Some(b"answer".as_slice())
        ));
        assert_eq!(executor.calls, 0);
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
        assert_eq!(
            supervisor.cancel(&handle, Some(diagnostic("late cancellation"))),
            Err(AdmissionError::ForeignRuntime)
        );
    }

    #[test]
    fn active_expiration_never_allocates_a_second_executable_operation() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let first = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        assert_eq!(runtime.next_invocation, 1);
        assert_eq!(runtime.invocations.len(), 1);
        assert_eq!(runtime.idempotency.len(), 1);
        runtime.expire(&first).unwrap();
        assert_eq!(runtime.next_invocation, 1);
        assert_eq!(runtime.invocations.len(), 1);
        assert_eq!(runtime.idempotency.len(), 1);

        let second = match runtime.admit(request).unwrap() {
            Admission::Active { handle } => handle,
            _ => panic!("active expiry must retain the original operation"),
        };
        assert_eq!(second.invocation(), first.invocation());
        assert_eq!(runtime.next_invocation, 1);
        assert_eq!(runtime.invocations.len(), 1);
        assert_eq!(runtime.idempotency.len(), 1);
        assert_eq!(runtime.check_handle(&first), Ok(()));
        assert_eq!(runtime.check_handle(&second), Ok(()));
    }

    #[test]
    fn active_expiration_rejects_mismatched_idempotency_reuse() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let first = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime.expire(&first).unwrap();

        let mut mismatched = request;
        mismatched.context.locale = Some("en-GB".into());
        assert!(matches!(
            runtime.admit(mismatched),
            Err(AdmissionError::IdempotencyMismatch)
        ));
    }
    #[test]
    fn terminal_expiration_retains_complete_result_without_operational_handle() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let handle = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime
            .retain_terminal(&handle, InvocationResult::Success(value("Str", "answer")))
            .unwrap();
        runtime.expire(&handle).unwrap();

        assert_eq!(
            runtime.check_handle(&handle),
            Err(AdmissionError::ExpiredHandle)
        );
        let replay = runtime.admit(request.clone()).unwrap();
        assert!(matches!(
            replay,
            Admission::Terminal {
                outcome: TerminalClass::Succeeded,
                result: RetainedInvocationResult::Success(_),
                ..
            }
        ));

        let mut executor = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "rerun")),
        };
        assert!(matches!(
            runtime.run(request.clone(), &mut executor),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ));
        assert_eq!(executor.calls, 0);

        let mut mismatched = request;
        mismatched.context.locale = Some("en-GB".into());
        assert!(matches!(
            runtime.admit(mismatched),
            Err(AdmissionError::IdempotencyMismatch)
        ));
    }
    #[test]
    fn terminal_expiration_retains_complete_result_across_restart() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("key".into());
        let handle = match runtime.admit(request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        runtime
            .retain_terminal(&handle, InvocationResult::Success(value("Str", "before")))
            .unwrap();
        runtime.expire(&handle).unwrap();
        runtime.restart();

        let mut executor = Executor {
            calls: 0,
            result: InvocationResult::Success(value("Str", "after")),
        };
        assert!(matches!(
            runtime.run(request, &mut executor),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Success(_)
            ))
        ));
        assert_eq!(executor.calls, 0);
    }

    #[test]
    fn invocation_id_exhaustion_fails_closed_without_overwriting_retained_state() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut retained_request = request(Some(value("Int", "1")), ArgumentMap::default());
        retained_request.idempotency_key = Some("retained".into());
        let retained_handle = match runtime.admit(retained_request.clone()).unwrap() {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };
        runtime
            .retain_terminal(
                &retained_handle,
                InvocationResult::Success(value("Str", "retained")),
            )
            .unwrap();

        runtime.next_invocation = u64::MAX;
        let mut fresh_request = request(Some(value("Int", "1")), ArgumentMap::default());
        fresh_request.idempotency_key = Some("fresh".into());

        assert!(matches!(
            runtime.admit(fresh_request),
            Err(AdmissionError::RuntimeUnavailable)
        ));
        assert_eq!(runtime.next_invocation, u64::MAX);
        assert_eq!(runtime.invocations.len(), 1);
        assert_eq!(runtime.idempotency.len(), 1);
        assert!(matches!(
            runtime.admit(retained_request),
            Ok(Admission::Terminal {
                result: RetainedInvocationResult::Success(ref result),
                ..
            }) if result.canonical() == Some(b"retained".as_slice())
        ));
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
    fn cancellation_request_is_idempotent_and_wins_over_late_completion() {
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
        assert!(completed.cancel(&completed_handle, None).unwrap());
        assert_eq!(
            completed.invocation_state(&completed_handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Cancelled(None)
            ))
        );
    }

    #[test]
    fn cancellation_wins_over_late_terminal_classification() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        runtime
            .cancel(&handle, Some(diagnostic("cancelled")))
            .unwrap();
        runtime
            .retain_terminal(&handle, InvocationResult::Success(value("Str", "late")))
            .unwrap();

        assert_eq!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::Cancelled(Some(diagnostic("cancelled")))
            ))
        );
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
            .retain_terminal(&handle, InvocationResult::Success(value("Str", "done")))
            .unwrap();
        runtime
            .classify_terminal(&handle, TerminalClass::Cancelled)
            .unwrap();

        assert!(matches!(
            runtime.admit(first).unwrap(),
            Admission::Terminal {
                outcome: TerminalClass::Succeeded,
                result: RetainedInvocationResult::Success(_),
                ..
            }
        ));
    }

    #[test]
    fn incomplete_terminal_classification_cannot_publish_success_or_failure() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let handle = match runtime
            .admit(request(Some(value("Int", "1")), ArgumentMap::default()))
            .unwrap()
        {
            Admission::New { handle, .. } => handle,
            _ => unreachable!(),
        };

        for outcome in [TerminalClass::Succeeded, TerminalClass::Failed] {
            assert_eq!(
                runtime.classify_terminal(&handle, outcome),
                Err(AdmissionError::TerminalInvariant)
            );
            assert_eq!(
                runtime.invocation_state(&handle),
                Ok(InvocationState::Active)
            );
        }
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
        assert!(matches!(
            runtime.invocation_state(&handle),
            Ok(InvocationState::Terminal(
                RetainedInvocationResult::OrdinaryFailure(_)
            ))
        ));
    }

    #[test]
    fn malformed_synchronous_completion_is_redacted_terminal_failure_before_error() {
        let mut runtime = Runtime::new(RuntimeId::new("r"));
        let mut request = request(Some(value("Int", "1")), ArgumentMap::default());
        request.idempotency_key = Some("malformed".into());
        let mut executor = Executor {
            calls: 0,
            result: InvocationResult {
                invocation: InvocationRef::from_id(InvocationId::new("forged")),
                status: InvocationStatus::Running,
                value: Some(value("Str", "must-not-retain")),
                failure: Some(diagnostic("must-not-retain")),
                started: Some(Instant::now()),
                ended: Instant::now(),
                duration: Some(Duration::from_secs(1)),
            },
        };

        assert_eq!(
            runtime.run(request.clone(), &mut executor),
            Err(AdmissionError::MalformedCompletion)
        );
        assert_eq!(executor.calls, 1);
        assert!(matches!(
            runtime.admit(request),
            Ok(Admission::Terminal {
                outcome: TerminalClass::Failed,
                result: RetainedInvocationResult::OrdinaryFailure(Diagnostic {
                    code: "sys.invoke.malformed_completion",
                    fields,
                    ..
                }),
                ..
            }) if fields.is_empty()
        ));
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

    #[test]
    fn normative_sys_descriptor_metadata_is_present_and_runtime_neutral() {
        // This is descriptor evidence only: the normative JSON is parsed and
        // validated here, but no sys function is implemented or invoked.
        let document: serde_json::Value =
            serde_json::from_str(include_str!("../../../api/sys.json"))
                .expect("api/sys.json must remain valid JSON");

        assert_eq!(document["status"], "specification");
        let functions = document["functions"]
            .as_array()
            .expect("sys API must declare function descriptors");
        assert!(!functions.is_empty());

        let mut names = std::collections::BTreeSet::new();
        for function in functions {
            let name = function["name"]
                .as_str()
                .filter(|name| !name.trim().is_empty())
                .expect("every sys function descriptor needs a name");
            assert!(
                names.insert(name),
                "duplicate sys function descriptor: {name}"
            );
            assert!(matches!(
                function["effect"].as_str(),
                Some("read" | "invoke" | "admin")
            ));
            for field in ["signature", "purpose"] {
                assert!(
                    function[field]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "{name} needs non-blank {field} metadata"
                );
            }

            if name.starts_with("sys.admin.") {
                assert!(
                    function["contract"]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "{name} needs its administrative contract metadata"
                );
            }
            if name.starts_with("sys.invoke") || name.starts_with("sys.start") {
                assert!(
                    function["snapshot_rule"]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "{name} needs its snapshot rule metadata"
                );
            }
            if name.starts_with("sys.start") {
                assert!(
                    function["ownership"]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "{name} needs its ownership metadata"
                );
            }
        }

        for required in [
            "sys.meta",
            "sys.describe",
            "sys.invoke(Value)",
            "sys.invoke<T>",
            "sys.start(Value)",
            "sys.start<T>",
            "sys.await",
            "sys.cancel",
        ] {
            assert!(names.contains(required), "missing descriptor: {required}");
        }

        let value_metadata = document["value_types"]
            .as_array()
            .and_then(|types| {
                types
                    .iter()
                    .find(|value| value["name"] == "sys.ValueMetadata<T>")
            })
            .expect("sys.ValueMetadata<T> descriptor must be present");
        assert_eq!(value_metadata["kind"], "record-generic");
        assert!(
            value_metadata["purpose"]
                .as_str()
                .is_some_and(|purpose| !purpose.trim().is_empty())
        );
        assert_eq!(
            value_metadata["fields"]
                .as_array()
                .expect("sys.ValueMetadata<T> fields must be declared")
                .iter()
                .map(|field| field["name"].as_str().expect("field name"))
                .collect::<Vec<_>>(),
            [
                "static_type",
                "nominal_type",
                "protocols",
                "codecs",
                "redacted"
            ]
        );
    }
}
