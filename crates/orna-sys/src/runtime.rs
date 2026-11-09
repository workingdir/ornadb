//! The runtime ledger: admission identity, idempotency, and terminal retention.

use std::{collections::BTreeMap, marker::PhantomData, time::Instant};

use sha2::{Digest, Sha256};

use crate::{
    diagnostic::{AdmissionError, ArgumentTypeDetail, Diagnostic},
    ids::{FunctionIdentity, InvocationId, InvocationRef, RuntimeId, TypeId, TypeRef},
    invocation::{
        Admission, AdmissionRequest, CancellationToken, ExecutionBoundary, FunctionDescriptor,
        InvocationCompletion, InvocationContext, InvocationExecutor, InvocationHandle,
        InvocationIdentity, InvocationMode, InvocationResult, InvocationState, InvocationStatus,
        Parameter, RetainedInvocationResult, TerminalClass, TransactionMode,
    },
    value::append,
    value::{
        Argument, ArgumentMap, InvocationArgumentMetadata, InvocationMetadata, RetainedValue,
        TypedValue,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Runtime {
    pub(crate) id: RuntimeId,
    pub(crate) generation: u64,
    pub(crate) next_invocation: u64,
    pub(crate) invocations: BTreeMap<InvocationId, StoredInvocation>,
    pub(crate) idempotency: BTreeMap<String, IdempotencyEntry>,
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
            if invocation.terminal().is_none() {
                invocation.finish(RetainedInvocationResult::Orphaned(None));
            }
        }
        for entry in self.idempotency.values_mut() {
            if entry.terminal.is_none() {
                entry.terminal = self
                    .invocations
                    .get(&entry.invocation)
                    .and_then(|invocation| invocation.terminal().cloned());
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
        if request.idempotency_key.is_some()
            && let Some((name, _)) = bound
                .entries()
                .find(|(_, value)| value.is_redacted() && !value.has_protected_identity())
        {
            return Err(AdmissionError::ArgumentType {
                name: name.to_owned(),
                detail: ArgumentTypeDetail::ProtectedValueNeedsIdentity,
            });
        }
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
        let function = request.function.identity.clone();
        let context = request.context.clone();
        let boundary = ExecutionBoundary {
            invocation: invocation.clone(),
            identity: identity.clone(),
            function: function.clone(),
            arguments: bound,
            mode: request.mode,
            transaction: request.transaction,
            context: context.clone(),
        };
        self.invocations.insert(
            invocation.clone(),
            StoredInvocation {
                function,
                result_type: request.witness.static_type().clone(),
                arguments: metadata,
                mode: request.mode,
                transaction: request.transaction,
                context,
                started: Instant::now(),
                phase: match request.mode {
                    InvocationMode::Invoke => Phase::Running,
                    InvocationMode::Start => Phase::Queued,
                },
                cancellation: Cancellation::new(),
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

    /// Returns redaction-safe metadata for one local invocation.
    ///
    /// The returned object is a descriptive runtime observation. It is not a
    /// durable system row and cannot be used as await/cancel authority; those
    /// operations still require the original generation-bound handle.
    pub fn invocation_metadata<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<InvocationMetadata, AdmissionError> {
        let stored = self.stored(handle)?;
        let (status, failure) = match &stored.phase {
            Phase::Queued => (InvocationStatus::Queued, None),
            Phase::Running => (InvocationStatus::Running, None),
            Phase::Terminal { result, .. } => match result {
                RetainedInvocationResult::Success(_) => (InvocationStatus::Succeeded, None),
                RetainedInvocationResult::OrdinaryFailure(failure) => {
                    (InvocationStatus::Failed, Some(failure.clone()))
                }
                RetainedInvocationResult::Cancelled(failure) => {
                    (InvocationStatus::Cancelled, failure.clone())
                }
                RetainedInvocationResult::Orphaned(failure) => {
                    (InvocationStatus::Orphaned, failure.clone())
                }
            },
        };
        Ok(InvocationMetadata {
            invocation: handle.invocation.clone(),
            function: stored.function.clone(),
            arguments: stored.arguments.clone(),
            mode: stored.mode,
            transaction: stored.transaction,
            context: stored.context.clone(),
            result_type: TypeRef::from_id(stored.result_type.clone()),
            status,
            failure,
            started: stored.started,
            ended: stored.ended(),
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
        if request.mode != InvocationMode::Invoke {
            return Err(AdmissionError::StartMode);
        }
        match self.admit(request)? {
            Admission::New { boundary, handle } => {
                self.mark_running(&handle)?;
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
        completion: InvocationCompletion<TypedValue>,
    ) -> Result<(), AdmissionError> {
        self.check_handle(handle)?;
        self.retain_completion(handle, completion)
    }

    fn retain_completion<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        completion: InvocationCompletion<TypedValue>,
    ) -> Result<(), AdmissionError> {
        let stored = self.stored(handle)?;
        if stored.terminal().is_some() {
            return Ok(());
        }
        let cancellation_reason = stored.cancellation.reason.clone();
        let result = match completion {
            InvocationCompletion::Success(value) => {
                if value.static_type() != handle.result_type().id() {
                    self.store_terminal(
                        handle,
                        RetainedInvocationResult::OrdinaryFailure(Diagnostic::new(
                            "sys.invoke.return_type",
                            "invocation returned a value with an incompatible type",
                        )),
                    )?;
                    return Err(AdmissionError::ReturnType);
                }
                RetainedInvocationResult::Success(RetainedValue::new(value))
            }
            InvocationCompletion::OrdinaryFailure(diagnostic) => {
                RetainedInvocationResult::OrdinaryFailure(diagnostic)
            }
            InvocationCompletion::Cancelled(diagnostic) => {
                RetainedInvocationResult::Cancelled(diagnostic.or(cancellation_reason))
            }
            InvocationCompletion::Orphaned(diagnostic) => {
                RetainedInvocationResult::Orphaned(diagnostic)
            }
        };
        self.store_terminal(handle, result)
    }
    pub(crate) fn mark_running<T>(
        &mut self,
        handle: &InvocationHandle<T>,
    ) -> Result<(), AdmissionError> {
        let invocation = self.stored_mut(handle)?;
        if matches!(invocation.phase, Phase::Queued) {
            invocation.phase = Phase::Running;
        }
        Ok(())
    }
    pub fn invocation_state<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<InvocationState, AdmissionError> {
        Ok(match self.stored(handle)?.terminal().cloned() {
            Some(result) => InvocationState::Terminal(result),
            None => InvocationState::Active,
        })
    }

    pub(crate) fn terminal_result<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<Option<InvocationResult<RetainedValue>>, AdmissionError> {
        let stored = self.stored(handle)?;
        let Phase::Terminal { result, ended } = &stored.phase else {
            return Ok(None);
        };
        let ended = *ended;
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
        let invocation = self.stored_mut(handle)?;
        if let Some(result) = invocation.terminal() {
            return Ok(result.terminal_class() == TerminalClass::Cancelled);
        }
        invocation.cancellation.request(reason);
        Ok(true)
    }
    pub fn cancellation_requested<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<bool, AdmissionError> {
        Ok(self.stored(handle)?.cancellation.requested())
    }
    pub(crate) fn request_cancellation_for_all(&mut self) {
        for invocation in self.invocations.values_mut() {
            if invocation.terminal().is_none() {
                invocation.cancellation.request(None);
            }
        }
    }
    pub(crate) fn cancellation_token<T>(
        &self,
        handle: &InvocationHandle<T>,
    ) -> Result<CancellationToken, AdmissionError> {
        Ok(self.stored(handle)?.cancellation.token.clone())
    }
    fn store_terminal<T>(
        &mut self,
        handle: &InvocationHandle<T>,
        result: RetainedInvocationResult,
    ) -> Result<(), AdmissionError> {
        let stored = self.stored_mut(handle)?;
        if stored.terminal().is_some() {
            return Ok(());
        }
        let result = if stored.cancellation.requested()
            && result.terminal_class() != TerminalClass::Cancelled
        {
            RetainedInvocationResult::Cancelled(stored.cancellation.reason.clone())
        } else {
            result
        };
        stored.finish(result.clone());
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
        self.stored(handle).map(|_| ())
    }

    /// The ledger record this handle names, after the handle checks every
    /// operation needs: same runtime generation, not resumable, still stored,
    /// and issued for the stored result type.
    fn stored<T>(&self, handle: &InvocationHandle<T>) -> Result<&StoredInvocation, AdmissionError> {
        if handle.resumable {
            return Err(AdmissionError::ExpiredHandle);
        }
        if handle.runtime != self.id {
            return Err(AdmissionError::ForeignRuntime);
        }
        self.invocations
            .get(handle.invocation.id())
            .filter(|stored| stored.result_type == *handle.result_type.id())
            .ok_or(AdmissionError::ExpiredHandle)
    }

    fn stored_mut<T>(
        &mut self,
        handle: &InvocationHandle<T>,
    ) -> Result<&mut StoredInvocation, AdmissionError> {
        self.check_handle(handle)?;
        self.invocations
            .get_mut(handle.invocation.id())
            .ok_or(AdmissionError::ExpiredHandle)
    }
    pub fn expire<T>(&mut self, handle: &InvocationHandle<T>) -> Result<(), AdmissionError> {
        let Some(result) = self.stored(handle)?.terminal().cloned() else {
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
    pub(crate) fn handle<T>(
        &self,
        invocation: &InvocationId,
        result_type: TypeId,
    ) -> InvocationHandle<T> {
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
#[derive(Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// A `Start` admission that no worker has begun executing.
    Queued,
    Running,
    /// The retained outcome and the instant it was recorded, so a terminal
    /// invocation cannot lack an end time.
    Terminal {
        result: RetainedInvocationResult,
        ended: Instant,
    },
}

/// The cancellation request for one invocation: the token executors poll and
/// the reason recorded when it was raised.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Cancellation {
    token: CancellationToken,
    reason: Option<Diagnostic>,
}

impl Cancellation {
    fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            reason: None,
        }
    }

    fn requested(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Raises the request. The first request wins; a later one keeps the
    /// original reason.
    fn request(&mut self, reason: Option<Diagnostic>) {
        if !self.requested() {
            self.token.cancel();
            self.reason = reason;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredInvocation {
    function: FunctionIdentity,
    result_type: TypeId,
    pub(crate) arguments: Vec<InvocationArgumentMetadata>,
    mode: InvocationMode,
    transaction: TransactionMode,
    context: InvocationContext,
    started: Instant,
    phase: Phase,
    cancellation: Cancellation,
}

impl StoredInvocation {
    fn terminal(&self) -> Option<&RetainedInvocationResult> {
        match &self.phase {
            Phase::Terminal { result, .. } => Some(result),
            Phase::Queued | Phase::Running => None,
        }
    }

    fn ended(&self) -> Option<Instant> {
        match &self.phase {
            Phase::Terminal { ended, .. } => Some(*ended),
            Phase::Queued | Phase::Running => None,
        }
    }

    fn finish(&mut self, result: RetainedInvocationResult) {
        self.phase = Phase::Terminal {
            result,
            ended: Instant::now(),
        };
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IdempotencyEntry {
    identity: InvocationIdentity,
    invocation: InvocationId,
    terminal: Option<RetainedInvocationResult>,
}

pub(crate) fn bind(
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
pub(crate) fn identity<T>(
    request: &AdmissionRequest<T>,
    arguments: &ArgumentMap,
) -> InvocationIdentity {
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
        // An absent field and an empty one are different contexts; length
        // framing alone would hash both to the same bytes.
        bytes.push(u8::from(value.is_some()));
        append(&mut bytes, value.as_deref().unwrap_or_default().as_bytes());
    }
    InvocationIdentity(hex(&Sha256::digest(bytes)))
}
pub(crate) use orna_base::hex;
