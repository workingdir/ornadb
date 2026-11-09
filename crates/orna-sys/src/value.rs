//! Typed values, argument binding metadata, and redaction-safe value projections.

use std::{collections::BTreeMap, fmt, time::Instant};

use orna_security::{SecretMetadata, SecretRef};
use serde::Serialize;

use crate::{
    diagnostic::{AdmissionError, ArgumentTypeDetail, Diagnostic},
    ids::{FunctionIdentity, InvocationRef, TypeId, TypeRef},
    invocation::{InvocationContext, InvocationMode, InvocationStatus, TransactionMode},
};

pub const CANONICAL_VALUE_CODEC_V1: &str = "OVB-1";

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
    pub(crate) fn has_protected_identity(&self) -> bool {
        matches!(
            self.payload,
            Payload::Protected {
                identity: Some(_),
                ..
            }
        )
    }
    pub(crate) fn append_identity(&self, out: &mut Vec<u8>) {
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
    pub(crate) fn get(&self, name: &str) -> Option<&TypedValue> {
        self.entries.get(name)
    }
    pub(crate) fn append_identity(&self, out: &mut Vec<u8>) {
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
    pub(crate) fn metadata(&self) -> Vec<InvocationArgumentMetadata> {
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
    pub(crate) name: String,
    pub(crate) position: usize,
    pub(crate) static_type: TypeId,
    pub(crate) value: Option<Vec<u8>>,
    pub(crate) redacted: bool,
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
    pub(crate) invocation: InvocationRef,
    pub(crate) function: FunctionIdentity,
    pub(crate) arguments: Vec<InvocationArgumentMetadata>,
    pub(crate) mode: InvocationMode,
    pub(crate) transaction: TransactionMode,
    pub(crate) context: InvocationContext,
    pub(crate) result_type: TypeRef,
    pub(crate) status: InvocationStatus,
    pub(crate) failure: Option<Diagnostic>,
    pub(crate) started: Instant,
    pub(crate) ended: Option<Instant>,
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
    pub(crate) fn new(value: TypedValue) -> Self {
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

pub(crate) fn append(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}
