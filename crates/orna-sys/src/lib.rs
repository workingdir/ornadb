#![recursion_limit = "512"]

//! Final Orna 1.1.0 portable sys catalogue and bounded, pre-effect admission.
//!
//! Resolution and durable transaction ownership stay with the evaluator and
//! runtime that own those concerns. The portable `sys` declaration schema is
//! collected from annotated descriptor methods in `system_api`. The local
//! supervisor below only provides a bounded execution and await seam for
//! admitted work.

use std::{
    collections::BTreeMap,
    fmt,
    marker::PhantomData,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use orna_security::{SecretMetadata, SecretRef};
use serde::{
    Serialize, Serializer,
    ser::{SerializeSeq, SerializeStruct},
};
use sha2::{Digest, Sha256};

mod introspection;
pub use introspection::{
    DefinitionRef, Dependency, DependencyConfidence, DependencyGraph, DependencyGraphError,
    DependencyInput, DependencyKind, DisjunctStormBranchDescription,
    DisjunctStormCascadeDescription, DisjunctStormDescription, DisjunctStormLimitRebindDescription,
    ExplainError, ExplainInputs, ExplainedPlan, ExpressionRef, FileRef, FunctionPlanDescription,
    FunctionRef, MAX_DEPENDENCY_EDGES, MAX_DEPENDENCY_OBJECTS, MAX_PLAN_EXPRESSIONS,
    MAX_PLAN_NODES, MAX_REFERENCE_BYTES, MutableBranchSnapshot, Plan, PlanByteCapHandoffRoute,
    PlanByteCapScopeSegment, PlanDetail, PlanNode, PlanNodeKind, PlanNodeRef, PlanNullOrder,
    PlanOrdering, PlanSortDirection, PlanWindowFrameBound, QueryCheckpointGenerationDescription,
    QueryDecorrelatedSubqueryDescription, QueryJoinDescription, QueryJoinPairIdentityDescription,
    QueryLimitPushdownDescription, QueryMutationDescription, QueryMutationKind,
    QueryPairedCheckpointSegmentCompactionChainDescription,
    QueryPairedCheckpointSegmentCompactionStepDescription,
    QueryPairedCheckpointSpillRestoreChainDescription,
    QueryPairedCheckpointSpillRestoreOccurrenceDescription,
    QueryPairedSegmentRotationChainDescription, QueryPairedSegmentRotationDescription,
    QueryPairedStreamCompactionChainDescription, QueryPairedStreamCompactionOccurrenceDescription,
    QueryPairedStreamRotationChainDescription, QueryPairedStreamRotationOccurrenceDescription,
    QueryPairedWalRotationChainDescription, QueryPairedWalRotationOccurrenceDescription,
    QueryPartialIndexDescription, QueryPlanDescription, QuerySourceStatistics,
    QueryWindowAggregatePushdownDescription, QueryWindowSpillDescription, SnapshotRef, SourceSpan,
    explain_function, explain_query, explain_query_with,
};

mod abi_version;
pub use abi_version::AbiVersion;

mod provider;
pub use provider::*;

mod provider_linkage;
pub use provider_linkage::{
    SystemProviderHost, SystemProviderLink, SystemProviderLinkage, SystemSurfaceMember,
    system_provider_linkage,
};

mod host_context;
pub use host_context::{
    BUILD_ID, IMPLEMENTATION_NAME, IMPLEMENTATION_VERSION, LANGUAGE_VERSION,
    PRESENTATION_PROTOCOL_VERSION, RuntimeMode, SUPPORTED_PROFILES, SYS_VERSION, StdSnapshotPin,
    SysHostContext, new_runtime_id,
};

mod host_registry_model;
pub use host_registry_model::{
    HostBindingDeclaration, HostOperationDescriptor, HostProviderRole, SystemHostOperationRegistry,
};

mod host_registry;
pub use host_registry::*;

mod host_environment;
pub use host_environment::*;

mod host_process;
pub use host_process::*;

mod host_clock;
pub use host_clock::*;

mod host_filesystem;
pub use host_filesystem::*;

mod host_network;
pub use host_network::*;

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

/// Safe metadata for one typed value.
///
/// These references are descriptive identities only. They do not prove that a
/// catalogue row exists or grant authority to read it; a trusted metadata
/// resolver must supply the nominal/protocol/codec facts for the exact static
/// type before this projection is produced.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ValueMetadata {
    static_type: TypeRef,
    nominal_type: Option<TypeRef>,
    protocols: Vec<TypeRef>,
    codecs: Vec<String>,
    redacted: bool,
}

impl ValueMetadata {
    pub fn static_type(&self) -> &TypeRef {
        &self.static_type
    }

    pub fn nominal_type(&self) -> Option<&TypeRef> {
        self.nominal_type.as_ref()
    }

    pub fn protocols(&self) -> &[TypeRef] {
        &self.protocols
    }

    pub fn codecs(&self) -> &[String] {
        &self.codecs
    }

    pub const fn is_redacted(&self) -> bool {
        self.redacted
    }
}

/// Catalogue supplied facts used to project metadata for one static type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValueMetadataFacts {
    nominal_type: Option<TypeId>,
    protocols: Vec<TypeId>,
    codecs: Vec<String>,
}

impl ValueMetadataFacts {
    /// Creates metadata facts from the authoritative type catalogue.
    ///
    /// The Orna reference defines these as safe metadata but does not prescribe
    /// collection order, so protocol and codec names use sorted unique order.
    pub fn new(
        nominal_type: Option<TypeId>,
        protocols: impl IntoIterator<Item = TypeId>,
        codecs: impl IntoIterator<Item = String>,
    ) -> Result<Self, ValueMetadataError> {
        let protocols = protocols
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let codecs = codecs
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if nominal_type
            .iter()
            .chain(protocols.iter())
            .any(|name| name.as_str().trim().is_empty())
            || codecs.iter().any(|name| name.trim().is_empty())
        {
            return Err(ValueMetadataError::InvalidCatalogueMetadata);
        }
        Ok(Self {
            nominal_type,
            protocols: protocols.into_iter().collect(),
            codecs: codecs.into_iter().collect(),
        })
    }
}

/// Resolves metadata through the catalogue that owns the type identities.
pub trait ValueMetadataResolver {
    /// Returns metadata for an exact static type, or `None` when that type is
    /// not present in the resolver's admitted catalogue.
    fn value_metadata_facts(&self, static_type: &TypeId) -> Option<ValueMetadataFacts>;
}

/// Failure to produce safe metadata from an authoritative catalogue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueMetadataError {
    TypeUnavailable,
    InvalidCatalogueMetadata,
}

impl ValueMetadataError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::TypeUnavailable => "sys.meta.type_unavailable",
            Self::InvalidCatalogueMetadata => "sys.meta.invalid_metadata",
        }
    }
}

/// Projects safe type metadata without exposing or re-encoding the value.
///
/// Protected canonical bytes remain inside `TypedValue`; this operation only
/// observes its static type and redaction bit. It never derives catalogue or
/// protocol identities from payload bytes or type-name guesses.
pub fn system_value_metadata(
    value: &TypedValue,
    resolver: &impl ValueMetadataResolver,
) -> Result<ValueMetadata, ValueMetadataError> {
    if value.static_type().as_str().trim().is_empty() {
        return Err(ValueMetadataError::InvalidCatalogueMetadata);
    }
    let facts = resolver
        .value_metadata_facts(value.static_type())
        .ok_or(ValueMetadataError::TypeUnavailable)?;
    Ok(ValueMetadata {
        static_type: TypeRef::from_id(value.static_type().clone()),
        nominal_type: facts.nominal_type.map(TypeRef::from_id),
        protocols: facts.protocols.into_iter().map(TypeRef::from_id).collect(),
        codecs: facts.codecs,
        redacted: value.is_redacted(),
    })
}

/// Safe fields projected from a secret provider's non-sensitive metadata.
///
/// ORNA-SECRET-004 requires the stable name, provider, and availability while
/// forbidding secret contents. `SecretMetadata` does not carry enough evidence
/// to build the rest of a durable `sys.Secret` catalogue row, so this
/// projection does not invent row authority, paths, recipients, dependencies,
/// or diagnostics. Secret contents are not accepted by this API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SecretProjection {
    name: String,
    provider: String,
    available: bool,
}

impl SecretProjection {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub const fn is_available(&self) -> bool {
        self.available
    }
}

/// Projects the safe `name`, `provider`, and `available` fields for `sys.Secret`.
pub fn system_secret_projection(metadata: &SecretMetadata) -> SecretProjection {
    SecretProjection {
        name: metadata.reference().as_str().to_owned(),
        provider: metadata.provider().to_owned(),
        available: metadata.available(),
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

/// What a typed value holds. Protected bytes never cross the source-evaluation
/// seam; only the value's type and redaction bit are observable.
#[derive(Clone, Eq, PartialEq)]
enum Payload {
    Public(Vec<u8>),
    Protected {
        canonical: Vec<u8>,
        /// Caller-supplied stable name for idempotency matching. A digest of
        /// low-entropy plaintext would remain dictionary-testable, so the
        /// canonical bytes of a protected value never participate.
        identity: Option<Vec<u8>>,
    },
}

#[derive(Clone, Eq, PartialEq)]
pub struct TypedValue {
    static_type: TypeId,
    payload: Payload,
}
impl fmt::Debug for TypedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedValue")
            .field("static_type", &self.static_type)
            .field("redacted", &self.is_redacted())
            .field("canonical", &"<withheld>")
            .finish()
    }
}

impl TypedValue {
    pub fn public(static_type: TypeId, canonical: impl Into<Vec<u8>>) -> Self {
        Self {
            static_type,
            payload: Payload::Public(canonical.into()),
        }
    }
    pub fn protected(static_type: TypeId, canonical: impl Into<Vec<u8>>) -> Self {
        Self {
            static_type,
            payload: Payload::Protected {
                canonical: canonical.into(),
                identity: None,
            },
        }
    }
    /// Creates a protected value with its stable `SecretRef` identity. The
    /// reference name may participate in idempotency matching; protected
    /// canonical bytes never do.
    pub fn protected_with_secret_ref(
        static_type: TypeId,
        canonical: impl Into<Vec<u8>>,
        reference: &SecretRef,
    ) -> Self {
        Self {
            static_type,
            payload: Payload::Protected {
                canonical: canonical.into(),
                identity: Some(reference.as_str().as_bytes().to_vec()),
            },
        }
    }
    pub fn static_type(&self) -> &TypeId {
        &self.static_type
    }
    pub fn is_redacted(&self) -> bool {
        matches!(self.payload, Payload::Protected { .. })
    }
    /// Returns public OVB-1 bytes for a bounded pure evaluator.
    ///
    /// Protected values deliberately never cross the source-evaluation seam.
    pub fn canonical(&self) -> Option<&[u8]> {
        match &self.payload {
            Payload::Public(canonical) => Some(canonical),
            Payload::Protected { .. } => None,
        }
    }
    fn has_protected_identity(&self) -> bool {
        matches!(
            self.payload,
            Payload::Protected {
                identity: Some(_),
                ..
            }
        )
    }
    fn append_identity(&self, out: &mut Vec<u8>) {
        append(out, self.static_type.as_str().as_bytes());
        match &self.payload {
            Payload::Public(canonical) => {
                out.push(0);
                append(out, canonical);
            }
            Payload::Protected { identity, .. } => {
                out.push(1);
                append(out, identity.as_deref().unwrap_or_default());
            }
        }
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
                value: value.canonical().map(<[u8]>::to_vec),
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
pub struct InvocationArgumentMetadata {
    name: String,
    position: usize,
    static_type: TypeId,
    value: Option<Vec<u8>>,
    redacted: bool,
}

impl InvocationArgumentMetadata {
    /// Returns the bound parameter name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the canonical zero-based position in Unicode name order.
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Returns the exact originating static type.
    pub fn static_type(&self) -> &TypeId {
        &self.static_type
    }

    /// Returns public canonical bytes, or `None` for a protected value.
    pub fn canonical(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// Returns whether the argument payload is protected.
    pub const fn is_redacted(&self) -> bool {
        self.redacted
    }
}

/// Redaction-safe metadata for one locally admitted invocation.
///
/// This is deliberately a local runtime observation rather than a portable
/// `sys.Invocation` row. It carries no fabricated `RowRef`, diagnostic
/// identity, parent/session link, or type witness. Those fields require the
/// corresponding durable authority and remain absent until an owner supplies
/// them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationMetadata {
    invocation: InvocationRef,
    function: FunctionIdentity,
    arguments: Vec<InvocationArgumentMetadata>,
    mode: InvocationMode,
    transaction: TransactionMode,
    context: InvocationContext,
    result_type: TypeRef,
    status: InvocationStatus,
    failure: Option<Diagnostic>,
    started: Instant,
    ended: Option<Instant>,
}

impl InvocationMetadata {
    /// Returns the local descriptive invocation identity.
    pub fn invocation(&self) -> &InvocationRef {
        &self.invocation
    }

    /// Returns the exact pinned function identity used for admission.
    pub fn function(&self) -> &FunctionIdentity {
        &self.function
    }

    /// Returns bound arguments in canonical name order.
    pub fn arguments(&self) -> &[InvocationArgumentMetadata] {
        &self.arguments
    }

    /// Returns whether the invocation was admitted synchronously or as a start.
    pub const fn mode(&self) -> InvocationMode {
        self.mode
    }

    /// Returns the transaction mode selected before execution.
    pub const fn transaction(&self) -> TransactionMode {
        self.transaction
    }

    /// Returns the captured caller context.
    pub fn context(&self) -> &InvocationContext {
        &self.context
    }

    /// Returns the result type selected by the explicit typed boundary.
    pub fn result_type(&self) -> &TypeRef {
        &self.result_type
    }

    /// Returns the current terminal or running status.
    pub const fn status(&self) -> InvocationStatus {
        self.status
    }

    /// Returns a retained safe diagnostic, when one exists.
    pub fn failure(&self) -> Option<&Diagnostic> {
        self.failure.as_ref()
    }

    /// Returns the local admission instant.
    pub const fn started(&self) -> Instant {
        self.started
    }

    /// Returns the terminal instant, if the invocation has completed.
    pub const fn ended(&self) -> Option<Instant> {
        self.ended
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
/// The authoritative portable descriptor for one intrinsic system function.
///
/// Descriptors are catalogue metadata, not executable closures.  Keeping this
/// metadata here gives semantic and CLI adapters one source of truth while
/// leaving invocation admission and execution under the existing runtime
/// boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SystemFunctionDescriptor {
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

mod system_api;
pub use system_api::*;

pub mod format_api_selection;

/// Returns final-format declaration metadata for a portable system function.
///
/// This context-free compatibility lookup assumes format 3. Callers handling
/// historical repositories must use [`format_api_selection`] with the
/// repository's verified recorded format/profile. The descriptor is static
/// metadata and does not grant invocation or administrative authority.
pub fn system_function_descriptor(name: &str) -> Option<&'static SystemFunctionDescriptor> {
    let context = format_api_selection::SystemFormatContext::from_recorded_pair(
        format_api_selection::RepositoryFormat::Final3,
        format_api_selection::ReaderContext::Final20261005,
    )?;
    format_api_selection::system_function_descriptor_for(context, name)
}

/// A descriptive object reference in an explanation.
///
/// This is intentionally not a durable `sys.RowRef<sys.Object>`: only a
/// catalogue/runtime owner can supply row-authoritative references.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct ObjectRef(String);

impl ObjectRef {
    pub fn descriptive(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A descriptive plan reference in an explanation.
///
/// A plan is optional because an explanation may be produced before planning;
/// this type does not make an unverified plan executable.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct PlanRef(String);

impl PlanRef {
    pub fn descriptive(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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

    fn cancel(&self) {
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
    /// The public canonical bytes; `None` exactly when the value is protected.
    canonical: Option<Vec<u8>>,
}
impl fmt::Debug for RetainedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedValue")
            .field("static_type", &self.static_type)
            .field("codec_version", &self.codec_version)
            .field("redacted", &self.is_redacted())
            .field("canonical", &"<withheld>")
            .finish()
    }
}
impl RetainedValue {
    fn new(value: TypedValue) -> Self {
        let TypedValue {
            static_type,
            payload,
        } = value;
        Self {
            static_type,
            codec_version: CANONICAL_VALUE_CODEC_V1.into(),
            canonical: match payload {
                Payload::Public(canonical) => Some(canonical),
                Payload::Protected { .. } => None,
            },
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
        self.canonical.is_none()
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
        let stored = self.stored(handle)?;
        if stored.terminal().is_some() {
            return Ok(());
        }
        let cancellation_reason = stored.cancellation.reason.clone();
        let result = match completion {
            ExecutionResult::Success(value) => {
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
    fn mark_running<T>(&mut self, handle: &InvocationHandle<T>) -> Result<(), AdmissionError> {
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

    fn terminal_result<T>(
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
    fn request_cancellation_for_all(&mut self) {
        for invocation in self.invocations.values_mut() {
            if invocation.terminal().is_none() {
                invocation.cancellation.request(None);
            }
        }
    }
    fn cancellation_token<T>(
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
    fn hold_next_worker_start(&self) {
        self.test_hold_worker_start.store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn release_held_worker_start(&self) {
        self.test_hold_worker_start.store(false, Ordering::Release);
        if let Some(gate) = self.test_worker_gate.lock().unwrap().take() {
            gate.release();
        }
    }
}

/// Where one admitted invocation is in its life.
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
struct StoredInvocation {
    function: FunctionIdentity,
    result_type: TypeId,
    arguments: Vec<InvocationArgumentMetadata>,
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
        // An absent field and an empty one are different contexts; length
        // framing alone would hash both to the same bytes.
        bytes.push(u8::from(value.is_some()));
        append(&mut bytes, value.as_deref().unwrap_or_default().as_bytes());
    }
    InvocationIdentity(hex(&Sha256::digest(bytes)))
}
fn append(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}
fn malformed_completion_failure() -> RetainedInvocationResult {
    RetainedInvocationResult::OrdinaryFailure(Diagnostic::new(
        "sys.invoke.malformed_completion",
        "invocation executor returned an invalid terminal completion",
    ))
}

/// The worker-side pieces of a freshly admitted `Start` invocation.
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
            let _ = runtime.retain_terminal(handle, InvocationResult::Cancelled(None));
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
) -> InvocationResult<TypedValue> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        executor.execute_controlled(boundary, cancellation)
    }))
    .unwrap_or_else(|_| {
        InvocationResult::Orphaned(Some(Diagnostic::new(
            "sys.invoke.orphaned",
            "invocation executor panicked",
        )))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgumentTypeDetail {
    Duplicate,
    Mismatch,
    ProtectedValueNeedsIdentity,
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
        Diagnostic::new(self.code(), "reflective invocation admission rejected")
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

/// Local, untrusted diagnostic input accepted by the sys supervisor.
///
/// This is intentionally not `sys.Diagnostic`: it has no durable identity,
/// severity, source references, or system-row authority.  A Foundation
/// adapter must construct the canonical row before this boundary can be
/// exposed as the portable relation.  Every text-bearing projection emitted
/// by this crate is redacted at serialization and explanation time.
#[derive(Clone, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: &'static str,
    pub fields: BTreeMap<String, DiagnosticField>,
    pub causes: Vec<Diagnostic>,
}

impl Diagnostic {
    /// A diagnostic that carries only its fixed code and message, with no
    /// retained fields or causes.
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            fields: BTreeMap::new(),
            causes: Vec::new(),
        }
    }
}

impl fmt::Debug for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Diagnostic")
            .field("code", &self.code)
            .field("message", &"<redacted>")
            .field("fields", &self.fields.values())
            .field("causes", &self.causes)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum DiagnosticField {
    Text(String),
    Redacted { static_type: TypeId },
}

impl fmt::Debug for DiagnosticField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let static_type = match self {
            Self::Text(_) => TypeId::new("Str"),
            Self::Redacted { static_type } => static_type.clone(),
        };
        formatter
            .debug_struct("DiagnosticField")
            .field("kind", &"redacted")
            .field("static_type", &static_type)
            .finish()
    }
}

impl Serialize for DiagnosticField {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("DiagnosticField", 2)?;
        state.serialize_field("kind", "redacted")?;
        let static_type = match self {
            Self::Text(_) => TypeId::new("Str"),
            Self::Redacted { static_type } => static_type.clone(),
        };
        state.serialize_field("static_type", &static_type)?;
        state.end()
    }
}

struct SafeDiagnosticFields<'a>(&'a BTreeMap<String, DiagnosticField>);

impl Serialize for SafeDiagnosticFields<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for field in self.0.values() {
            sequence.serialize_element(&("<redacted>", field))?;
        }
        sequence.end()
    }
}

impl Serialize for Diagnostic {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Diagnostic", 4)?;
        state.serialize_field("code", self.code)?;
        state.serialize_field("message", "<redacted>")?;
        state.serialize_field("fields", &SafeDiagnosticFields(&self.fields))?;
        state.serialize_field("causes", &self.causes)?;
        state.end()
    }
}

#[cfg(test)]
mod tests;
