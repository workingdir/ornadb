//! Bounded, pre-effect admission for Orna 1.0 reflective invocation.
//!
//! Resolution, target execution, await scheduling, transaction ownership, and
//! schema generation stay with the evaluator/runtime that owns those concerns.

use std::{collections::BTreeMap, fmt, marker::PhantomData};

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
    next_invocation: u64,
    invocations: BTreeMap<InvocationId, StoredInvocation>,
    idempotency: BTreeMap<String, IdempotencyEntry>,
}
impl Runtime {
    pub fn new(id: RuntimeId) -> Self {
        Self {
            id,
            next_invocation: 0,
            invocations: BTreeMap::new(),
            idempotency: BTreeMap::new(),
        }
    }
    pub fn id(&self) -> &RuntimeId {
        &self.id
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
            .cancellation_requested)
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredInvocation {
    result_type: TypeId,
    terminal: Option<RetainedInvocationResult>,
    cancellation_requested: bool,
    cancellation_reason: Option<Diagnostic>,
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
    ForeignRuntime,
    ExpiredHandle,
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
            Self::ForeignRuntime => "sys.handle.foreign_runtime",
            Self::ExpiredHandle => "sys.handle.expired",
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
