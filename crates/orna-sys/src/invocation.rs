//! Invocation admission, execution, and observation state.

use std::{
    fmt,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    diagnostic::Diagnostic,
    ids::{FunctionIdentity, InvocationId, InvocationRef, RuntimeId, SnapshotId, TypeId, TypeRef},
    value::{ArgumentMap, RetainedValue, TypedValue},
};

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
/// The authoritative portable descriptor for one intrinsic system function.
///
/// Descriptors are catalogue metadata, not executable closures.  Keeping this
/// metadata here gives semantic and CLI adapters one source of truth while
/// leaving invocation admission and execution under the existing runtime
/// boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SystemFunctionDescriptor {
    /// The generated catalogue identity of this callable.
    ///
    /// `None` is reserved for the two 1.0.0 callables retired by 1.1.0 that are
    /// still selectable by historical format 1/2 readers; their identity lives in
    /// [`orna_sys_id::RETIRED_ENGINE_FUNCTIONS`], not in the 1.1.0 catalogue.
    #[serde(skip)]
    pub function: Option<orna_sys_id::SysFunction>,
    pub name: &'static str,
    pub effect: SystemEffect,
    pub signature: &'static str,
    pub purpose: &'static str,
    /// Release coordinate for a callable introduced after the original API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<&'static str>,
    /// Extended source documentation for editor hover and reference views.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<&'static str>,
    /// Optional registry details useful when presenting the callable contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preconditions: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_rule: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub enum SystemEffect {
    Read,
    Invoke,
    Admin,
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
pub struct InvocationIdentity(pub(crate) String);
impl fmt::Debug for InvocationIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvocationIdentity(<withheld>)")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionBoundary {
    pub invocation: InvocationId,
    pub(crate) identity: InvocationIdentity,
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
    pub(crate) marker: PhantomData<fn() -> T>,
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

/// Complete, terminal outcome produced by an executor.
///
/// This is the only completion shape an executor can return: each variant
/// carries exactly the data its terminal class defines, so a live status or an
/// inconsistent value/failure pair is not representable. [`InvocationResult`]
/// remains the observation returned by `sys.await`, adding identity and
/// timing to the same terminal class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationCompletion<T> {
    Success(T),
    OrdinaryFailure(Diagnostic),
    Cancelled(Option<Diagnostic>),
    Orphaned(Option<Diagnostic>),
}

impl<T> InvocationCompletion<T> {
    pub fn terminal_class(&self) -> TerminalClass {
        match self {
            Self::Success(_) => TerminalClass::Succeeded,
            Self::OrdinaryFailure(_) => TerminalClass::Failed,
            Self::Cancelled(_) => TerminalClass::Cancelled,
            Self::Orphaned(_) => TerminalClass::Orphaned,
        }
    }
}

#[derive(Clone)]
pub struct CancellationToken {
    requested: Arc<AtomicBool>,
}

impl CancellationToken {
    pub(crate) fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(crate) fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
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
    fn execute(&mut self, boundary: &ExecutionBoundary) -> InvocationCompletion<TypedValue>;

    fn execute_controlled(
        &mut self,
        boundary: &ExecutionBoundary,
        _: &CancellationToken,
    ) -> InvocationCompletion<TypedValue> {
        self.execute(boundary)
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
