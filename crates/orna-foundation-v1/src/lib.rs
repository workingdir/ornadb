//! Shared, lossless Orna 1.0 foundation contracts.
//!
//! Canonical payload ownership remains in `orna-value-v1`. This crate owns the
//! portable `sys` references, spans, diagnostics, and repository adapter seam.

use std::{collections::BTreeMap, fmt, marker::PhantomData, sync::Mutex};

use num_bigint::{BigInt, Sign};
use serde::{Serialize, Serializer, ser::SerializeStruct};

pub use orna_value_v1::{
    Error as ValueError, GitHash, OVB_VERSION, Raw as OvbRaw, SchemaDescriptor, Snapshot, Value,
};

/// Canonical values, closed descriptors and snapshot encodings are owned by
/// OVB-1. Neither `CanonicalSnapshot` nor `CanonicalValue` is a sys row ref.
pub type CanonicalValue = Value;
pub type TypeDescriptor = SchemaDescriptor;
pub type CanonicalSnapshot = Snapshot;

/// `sys.RowRef<T>` as OVB tag 60010. Key and snapshot context are identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RowRef {
    pub database_id: [u8; 16],
    pub table_id: [u8; 16],
    pub key: OvbRaw,
    pub snapshot: CanonicalSnapshot,
}
impl RowRef {
    pub fn new(
        database_id: [u8; 16],
        table_id: [u8; 16],
        key: OvbRaw,
        snapshot: CanonicalSnapshot,
    ) -> Result<Self, FoundationError> {
        let reference = Self {
            database_id,
            table_id,
            key,
            snapshot,
        };
        Value::new(reference.raw()?).map_err(FoundationError::Value)?;
        Ok(reference)
    }
    pub fn encode(&self) -> Result<Vec<u8>, FoundationError> {
        Value::new(self.raw()?)
            .map_err(FoundationError::Value)?
            .encode()
            .map_err(FoundationError::Value)
    }
    fn raw(&self) -> Result<OvbRaw, FoundationError> {
        Ok(OvbRaw::Tag(
            60010,
            Box::new(OvbRaw::Array(vec![
                uuid(self.database_id),
                uuid(self.table_id),
                self.key.clone(),
                self.snapshot.raw(),
            ])),
        ))
    }
}
/// A noninterchangeable typed `sys.RowRef<T>`.
///
/// This compatibility wrapper is only a type marker: it deliberately does not
/// validate a relation identity and never grants authority. The generic
/// conversion must not be treated as proof of `Kind`, row existence, or
/// authorization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedRowRef<Kind> {
    raw: RowRef,
    kind: PhantomData<Kind>,
}
impl<Kind> TypedRowRef<Kind> {
    pub fn from_row_ref(raw: RowRef) -> Self {
        Self {
            raw,
            kind: PhantomData,
        }
    }
    pub fn as_row_ref(&self) -> &RowRef {
        &self.raw
    }
    pub fn into_row_ref(self) -> RowRef {
        self.raw
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationArgumentKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointKind {}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureKind {}
/// Typed portable row references. `SnapshotRef` is a snapshot metadata row
/// reference, deliberately distinct from `CanonicalSnapshot` pin bytes.
pub type FileRef = TypedRowRef<FileKind>;
pub type SnapshotRef = TypedRowRef<SnapshotKind>;
pub type DiagnosticRef = TypedRowRef<DiagnosticKind>;
pub type ObjectRef = TypedRowRef<ObjectKind>;
pub type DefinitionRef = TypedRowRef<DefinitionKind>;
pub type TypeRef = TypedRowRef<TypeKind>;
pub type TraceRef = TypedRowRef<TraceKind>;
/// Typed `sys.RowRef<sys.Function>` marker. This is not proof that the
/// referenced row is a function, exists, or may be invoked.
pub type FunctionRef = TypedRowRef<FunctionKind>;
/// Typed `sys.RowRef<sys.Invocation>` marker. This is not proof that an
/// invocation exists or grants observation, cancellation, or await authority.
pub type InvocationRef = TypedRowRef<InvocationKind>;
/// Typed `sys.RowRef<sys.InvocationArgument>` marker. This is not proof that
/// an argument observation exists or permits reading retained argument values.
pub type InvocationArgumentRef = TypedRowRef<InvocationArgumentKind>;
/// Typed `sys.RowRef<sys.Run>` alias. The generic wrapper remains a marker;
/// use `validate_run_reference` when checked relation/context coordinates are
/// required.
pub type RunRef = TypedRowRef<RunKind>;
/// Typed `sys.RowRef<sys.Stream>` alias. The generic wrapper remains a
/// marker; use `validate_stream_reference` when checked relation/context
/// coordinates are required.
pub type StreamRef = TypedRowRef<StreamKind>;
/// Typed `sys.RowRef<sys.Checkpoint>` marker. This is not proof that a
/// checkpoint exists or grants progress-management authority.
pub type CheckpointRef = TypedRowRef<CheckpointKind>;
/// Typed `sys.RowRef<sys.Failure>` marker. This is not proof that a failure
/// exists or grants retry, skip, replay, or resolution authority.
pub type FailureRef = TypedRowRef<FailureKind>;

pub mod object_reference;
pub use object_reference::{
    SYS_OBJECT_TABLE_ID, object_reference, validate_object_reference,
    validate_object_reference_for_id,
};

/// Checks only that a row reference is pinned to the supplied CWD context,
/// then attaches a noninterchangeable marker. It intentionally does not
/// validate a relation identity: physical relation identities and row keys
/// are owned by the projection that defines them. This helper is therefore
/// implementation-defined coordinate validation, not authority, provenance,
/// row-existence, or semantic-relation proof.
pub fn validate_reference_context<Kind>(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<TypedRowRef<Kind>, SystemReferenceError> {
    if reference.database_id != capture.database_id() {
        return Err(SystemReferenceError::DatabaseMismatch);
    }
    if reference.snapshot != *capture.snapshot() {
        return Err(SystemReferenceError::SnapshotMismatch);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Stable implementation-defined physical identities for the canonical
/// `sys.Type`, `sys.Function`, `sys.Snapshot`, `sys.Invocation`,
/// `sys.InvocationArgument`, `sys.Run`, and `sys.Stream` relations. The Orna
/// specification names these relations but does not prescribe their
/// sixteen-byte physical identities.
/// These constants are therefore versioned implementation compatibility
/// values, not per-runtime generated identifiers.
pub const SYS_INVOCATION_TABLE_ID: [u8; 16] = [
    0x26, 0x90, 0x51, 0xdc, 0x83, 0x3a, 0x48, 0x61, 0x91, 0x07, 0x65, 0xe8, 0x2b, 0x74, 0x19, 0x03,
];
pub const SYS_TYPE_TABLE_ID: [u8; 16] = [
    0x58, 0xce, 0xa4, 0x91, 0x3e, 0x77, 0x4f, 0x08, 0x9a, 0x43, 0x2d, 0xb6, 0x05, 0x1f, 0x83, 0x05,
];
pub const SYS_FUNCTION_TABLE_ID: [u8; 16] = [
    0x6f, 0x29, 0xd2, 0x84, 0x4b, 0x5a, 0x45, 0xc1, 0x87, 0x06, 0x31, 0xe9, 0x94, 0x6c, 0x70, 0x06,
];
pub const SYS_SNAPSHOT_TABLE_ID: [u8; 16] = [
    0x81, 0xb5, 0x3a, 0xe7, 0x20, 0x4d, 0x49, 0x6c, 0x95, 0x18, 0x7f, 0xc2, 0x46, 0x09, 0xab, 0x07,
];
pub const SYS_INVOCATION_ARGUMENT_TABLE_ID: [u8; 16] = [
    0x4b, 0x17, 0x8d, 0x20, 0x6f, 0x91, 0x44, 0x72, 0xa8, 0x3e, 0x10, 0xc5, 0x59, 0xd2, 0x84, 0x04,
];

/// Constructs a checked `sys.TypeRef` from the exact `sys.Type` natural key
/// (`object`). The reference remains coordinate data only: it neither proves
/// that the object is a type nor establishes row existence or authority.
pub fn type_reference(object: ObjectRef) -> Result<TypeRef, SystemReferenceError> {
    catalogue_reference(SYS_TYPE_TABLE_ID, object)
}

/// Constructs a checked `sys.FunctionRef` from the exact `sys.Function`
/// natural key (`object`). The reference remains coordinate data only: it
/// neither proves that the object is a function nor establishes row existence
/// or authority.
pub fn function_reference(object: ObjectRef) -> Result<FunctionRef, SystemReferenceError> {
    catalogue_reference(SYS_FUNCTION_TABLE_ID, object)
}

/// Constructs a checked `sys.SnapshotRef` from the exact `sys.Snapshot`
/// natural key (`id`). The ID is derived from the pinned snapshot descriptor,
/// never accepted as an independently rebindable selector.
pub fn snapshot_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
) -> Result<SnapshotRef, SystemReferenceError> {
    ensure_snapshot_database(database_id, &snapshot)?;
    Ok(TypedRowRef::from_row_ref(RowRef {
        database_id,
        table_id: SYS_SNAPSHOT_TABLE_ID,
        key: OvbRaw::Bytes(snapshot_id(&snapshot)?.to_vec()),
        snapshot,
    }))
}
pub const SYS_RUN_TABLE_ID: [u8; 16] = [
    0x3d, 0x64, 0x87, 0x71, 0x5a, 0x4c, 0x4e, 0x80, 0x9d, 0x2f, 0x11, 0xa4, 0x92, 0x36, 0x70, 0x01,
];
pub const SYS_STREAM_TABLE_ID: [u8; 16] = [
    0x7c, 0x10, 0x5b, 0xa9, 0x63, 0x2e, 0x43, 0x8c, 0x88, 0x19, 0x56, 0xd0, 0x47, 0xaf, 0x20, 0x02,
];
pub const SYS_CHECKPOINT_TABLE_ID: [u8; 16] = [
    0x9a, 0x73, 0xf8, 0x1d, 0x4c, 0x5f, 0x40, 0x14, 0x92, 0x2e, 0x61, 0x09, 0x6b, 0x82, 0x1f, 0x03,
];
pub const SYS_FAILURE_TABLE_ID: [u8; 16] = [
    0xb4, 0x62, 0xcd, 0x70, 0x35, 0x2f, 0x4a, 0xb9, 0x8f, 0x3a, 0x97, 0x56, 0xe1, 0x4c, 0x28, 0x04,
];

/// Constructs a checked `sys.CheckpointRef` from the canonical durable
/// checkpoint natural key (`consumer_identity + source_identity + partition`).
/// It constructs coordinates only; it never proves durable retention or
/// observation authority.
pub fn checkpoint_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    consumer_identity: String,
    source_identity: String,
    partition: Option<String>,
) -> Result<CheckpointRef, SystemReferenceError> {
    ensure_snapshot_database(database_id, &snapshot)?;
    let key = checkpoint_key_raw(consumer_identity, source_identity, partition)?;
    Ok(TypedRowRef::from_row_ref(RowRef {
        database_id,
        table_id: SYS_CHECKPOINT_TABLE_ID,
        key,
        snapshot,
    }))
}

/// Constructs a checked `sys.FailureRef` from its canonical durable natural
/// key (`consumer_identity + source_identity + partition + position_format +
/// position`). A retained physical failure identity may locate this row, but
/// is deliberately not part of the public reference key.
pub fn failure_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    consumer_identity: String,
    source_identity: String,
    partition: Option<String>,
    position_format: String,
    position: String,
) -> Result<FailureRef, SystemReferenceError> {
    ensure_snapshot_database(database_id, &snapshot)?;
    let OvbRaw::Array(mut key) = checkpoint_key_raw(consumer_identity, source_identity, partition)
        .map_err(|_| SystemReferenceError::InvalidFailureKey)?
    else {
        return Err(SystemReferenceError::InvalidFailureKey);
    };
    if position_format.is_empty() || position.is_empty() {
        return Err(SystemReferenceError::InvalidFailureKey);
    }
    Ok(TypedRowRef::from_row_ref(RowRef {
        database_id,
        table_id: SYS_FAILURE_TABLE_ID,
        key: {
            key.push(OvbRaw::Text(position_format));
            key.push(OvbRaw::Text(position));
            OvbRaw::Array(key)
        },
        snapshot,
    }))
}

/// Constructs a checked `sys.InvocationRef` from the durable observation
/// coordinates. This validates only the canonical reference shape and that
/// the supplied snapshot belongs to `database_id`; it does not prove that the
/// row exists, is retained, or is observable by the caller.
pub fn invocation_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    id: [u8; 16],
) -> Result<InvocationRef, SystemReferenceError> {
    ensure_snapshot_database(database_id, &snapshot)?;
    Ok(TypedRowRef::from_row_ref(RowRef {
        database_id,
        table_id: SYS_INVOCATION_TABLE_ID,
        key: uuid(id),
        snapshot,
    }))
}

/// Constructs a checked `sys.InvocationArgumentRef` from the durable
/// observation coordinates. `position` is the nonnegative, `u64`-bounded
/// natural-key component required by the public relation. As with
/// [`invocation_reference`], this only constructs coordinates: it never
/// establishes row existence, retention, or authorization to read the
/// argument observation.
pub fn invocation_argument_reference(
    database_id: [u8; 16],
    snapshot: CanonicalSnapshot,
    invocation_id: [u8; 16],
    position: BigInt,
) -> Result<InvocationArgumentRef, SystemReferenceError> {
    ensure_snapshot_database(database_id, &snapshot)?;
    if position.sign() == Sign::Minus || position > BigInt::from(u64::MAX) {
        return Err(SystemReferenceError::InvalidInvocationArgumentKey);
    }
    let invocation = RowRef {
        database_id,
        table_id: SYS_INVOCATION_TABLE_ID,
        key: uuid(invocation_id),
        snapshot: snapshot.clone(),
    };
    Ok(TypedRowRef::from_row_ref(RowRef {
        database_id,
        table_id: SYS_INVOCATION_ARGUMENT_TABLE_ID,
        key: OvbRaw::Array(vec![row_ref_raw(&invocation), OvbRaw::Int(position)]),
        snapshot,
    }))
}

/// Closed 1.0.0 vocabulary for `sys.Invocation.status`. Its JSON form is the
/// exact lower-case vocabulary from `api/sys.json`; callers cannot construct
/// an extension status through this type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Orphaned,
}
impl InvocationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Orphaned => "orphaned",
        }
    }
}
impl Serialize for InvocationStatus {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Redaction-safe facts used to validate a durable `sys.Invocation` row.
///
/// This model intentionally stores only presence bits. It does not retain or
/// construct parent, session, trace, idempotency, result, or failure values,
/// so validation cannot manufacture public references or leak protected
/// payloads while the durable store lacks authoritative coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvocationFacts {
    pub status: InvocationStatus,
    pub has_parent: bool,
    pub has_owner_session: bool,
    pub is_top_level_command: bool,
    pub has_result: bool,
    pub has_failure: bool,
    pub has_trace: bool,
    pub has_idempotency_key_hash: bool,
}

impl InvocationFacts {
    /// Validates ownership, terminal outcome, and unsupported-link facts.
    pub fn validate(self) -> Result<(), InvocationFactsError> {
        let owner_count =
            self.has_parent as u8 + self.has_owner_session as u8 + self.is_top_level_command as u8;
        if owner_count != 1 {
            return Err(InvocationFactsError::OwnerExclusivity);
        }
        if self.has_trace || self.has_idempotency_key_hash {
            return Err(InvocationFactsError::UnsupportedLinkPresent);
        }

        match self.status {
            InvocationStatus::Queued | InvocationStatus::Running => {
                if self.has_result || self.has_failure {
                    return Err(InvocationFactsError::ActiveHasTerminalEvidence);
                }
            }
            InvocationStatus::Succeeded => {
                if !self.has_result || self.has_failure {
                    return Err(InvocationFactsError::SucceededOutcome);
                }
            }
            InvocationStatus::Failed => {
                if self.has_result || !self.has_failure {
                    return Err(InvocationFactsError::FailedOutcome);
                }
            }
            InvocationStatus::Cancelled | InvocationStatus::Orphaned => {
                if self.has_result {
                    return Err(InvocationFactsError::CancelledOrOrphanedOutcome);
                }
            }
        }
        Ok(())
    }
}

/// A redaction-safe failure from [`InvocationFacts::validate`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationFactsError {
    /// Exactly one of parent, owner session, or top-level command is required.
    OwnerExclusivity,
    /// Trace and idempotency-key links are not retained by this projection.
    UnsupportedLinkPresent,
    /// Active invocations cannot expose terminal result or failure evidence.
    ActiveHasTerminalEvidence,
    /// A succeeded invocation requires a result and no failure.
    SucceededOutcome,
    /// A failed invocation requires a failure and no result.
    FailedOutcome,
    /// Cancelled and orphaned invocations cannot expose a result.
    CancelledOrOrphanedOutcome,
}

/// Checks a candidate `sys.InvocationRef` against a supplied CWD context,
/// fixed `sys.Invocation` relation identity, and the declared `id` key. This
/// is reference-coordinate validation only; it does not establish
/// provenance, row existence, projection ownership, or authority.
pub fn validate_invocation_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<InvocationRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_INVOCATION_TABLE_ID)?;
    if !is_opaque_id_key(&reference.key) {
        return Err(SystemReferenceError::InvalidInvocationKey);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Checks a candidate `sys.TypeRef` against a supplied CWD context, fixed
/// `sys.Type` relation identity, and the exact one-field `object` natural
/// key. This validates coordinates only; it does not establish object kind,
/// relation existence, provenance, or authorization.
pub fn validate_type_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<TypeRef, SystemReferenceError> {
    validate_catalogue_reference(reference, capture, SYS_TYPE_TABLE_ID)
}

/// Checks a candidate `sys.FunctionRef` against a supplied CWD context, fixed
/// `sys.Function` relation identity, and the exact one-field `object` natural
/// key. This validates coordinates only; it does not establish object kind,
/// relation existence, provenance, or authorization.
pub fn validate_function_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<FunctionRef, SystemReferenceError> {
    validate_catalogue_reference(reference, capture, SYS_FUNCTION_TABLE_ID)
}

/// Checks a candidate `sys.SnapshotRef` against a supplied CWD context, fixed
/// `sys.Snapshot` relation identity, and the exact `id` natural key derived
/// from its pinned descriptor. This does not establish retained history or
/// authorization to observe it.
pub fn validate_snapshot_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<SnapshotRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_SNAPSHOT_TABLE_ID)?;
    if !matches!(&reference.key, OvbRaw::Bytes(id) if id.as_slice() == snapshot_id(&reference.snapshot)?.as_slice())
    {
        return Err(SystemReferenceError::InvalidSnapshotKey);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Checks a candidate `sys.InvocationArgumentRef` against a supplied CWD
/// context, fixed `sys.InvocationArgument` relation identity, and the
/// declared natural key (`invocation + position`). The position is a
/// nonnegative implementation-defined `u64` physical coordinate, rejecting
/// values that cannot be represented without overflow. This is a validation
/// primitive only; it does not establish durable observation or authority.
pub fn validate_invocation_argument_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<InvocationArgumentRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_INVOCATION_ARGUMENT_TABLE_ID)?;
    let OvbRaw::Array(key) = &reference.key else {
        return Err(SystemReferenceError::InvalidInvocationArgumentKey);
    };
    let [invocation, position] = key.as_slice() else {
        return Err(SystemReferenceError::InvalidInvocationArgumentKey);
    };
    let invocation = row_ref_from_raw(invocation)
        .map_err(|_| SystemReferenceError::InvalidInvocationArgumentKey)?;
    validate_invocation_reference(invocation, capture)
        .map_err(|_| SystemReferenceError::InvalidInvocationArgumentKey)?;
    if !is_nonnegative_u64_integer(position) {
        return Err(SystemReferenceError::InvalidInvocationArgumentKey);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Checks a candidate `sys.RunRef` against a supplied CWD context and the
/// fixed `sys.Run` relation identity. This is a validation primitive only:
/// it does not establish provenance, row existence, projection ownership, or
/// authorization, and it does not create `sys.rt.runs`.
pub fn validate_run_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<RunRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_RUN_TABLE_ID)?;
    if !is_run_id_key(&reference.key) {
        return Err(SystemReferenceError::InvalidRunKey);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Checks a candidate `sys.StreamRef` against a supplied CWD context, the
/// fixed `sys.Stream` relation identity, and the declared natural key
/// (`run + source_identity + partition`). This is a validation primitive
/// only; it does not establish a durable projection or grant authority.
pub fn validate_stream_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<StreamRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_STREAM_TABLE_ID)?;
    let OvbRaw::Array(key) = &reference.key else {
        return Err(SystemReferenceError::InvalidStreamKey);
    };
    let [run, source_identity, partition] = key.as_slice() else {
        return Err(SystemReferenceError::InvalidStreamKey);
    };
    let run = row_ref_from_raw(run).map_err(|_| SystemReferenceError::InvalidStreamKey)?;
    validate_run_reference(run, capture).map_err(|_| SystemReferenceError::InvalidStreamKey)?;
    if !matches!(source_identity, OvbRaw::Text(value) if !value.is_empty())
        || !(matches!(partition, OvbRaw::Null)
            || matches!(partition, OvbRaw::Text(value) if !value.is_empty()))
    {
        return Err(SystemReferenceError::InvalidStreamKey);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Checks a candidate `sys.CheckpointRef` against the supplied pinned CWD
/// context and its exact durable natural-key representation.
pub fn validate_checkpoint_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<CheckpointRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_CHECKPOINT_TABLE_ID)?;
    checkpoint_key_from_raw(&reference.key)?;
    Ok(TypedRowRef::from_row_ref(reference))
}

/// Checks a candidate `sys.FailureRef` against the supplied pinned CWD
/// context and its exact durable natural-key representation.
pub fn validate_failure_reference(
    reference: RowRef,
    capture: &CwdCapture,
) -> Result<FailureRef, SystemReferenceError> {
    validate_coordinates(&reference, capture, SYS_FAILURE_TABLE_ID)?;
    let OvbRaw::Array(key) = &reference.key else {
        return Err(SystemReferenceError::InvalidFailureKey);
    };
    let [consumer, source, partition, position_format, position] = key.as_slice() else {
        return Err(SystemReferenceError::InvalidFailureKey);
    };
    checkpoint_key_from_raw(&OvbRaw::Array(vec![
        consumer.clone(),
        source.clone(),
        partition.clone(),
    ]))
    .map_err(|_| SystemReferenceError::InvalidFailureKey)?;
    if !matches!(position_format, OvbRaw::Text(value) if !value.is_empty())
        || !matches!(position, OvbRaw::Text(value) if !value.is_empty())
    {
        return Err(SystemReferenceError::InvalidFailureKey);
    }
    Ok(TypedRowRef::from_row_ref(reference))
}

fn checkpoint_key_raw(
    consumer_identity: String,
    source_identity: String,
    partition: Option<String>,
) -> Result<OvbRaw, SystemReferenceError> {
    if consumer_identity.is_empty() || source_identity.is_empty() {
        return Err(SystemReferenceError::InvalidCheckpointKey);
    }
    if matches!(partition.as_deref(), Some("")) {
        return Err(SystemReferenceError::InvalidCheckpointKey);
    }
    Ok(OvbRaw::Array(vec![
        OvbRaw::Text(consumer_identity),
        OvbRaw::Text(source_identity),
        partition.map(OvbRaw::Text).unwrap_or(OvbRaw::Null),
    ]))
}

fn catalogue_reference<Kind>(
    table_id: [u8; 16],
    object: ObjectRef,
) -> Result<TypedRowRef<Kind>, SystemReferenceError> {
    let object = object.into_row_ref();
    object_reference::validate_object_reference_shape(&object)?;
    ensure_snapshot_database(object.database_id, &object.snapshot)?;
    Ok(TypedRowRef::from_row_ref(RowRef {
        database_id: object.database_id,
        table_id,
        key: row_ref_raw(&object),
        snapshot: object.snapshot,
    }))
}

fn validate_catalogue_reference<Kind>(
    reference: RowRef,
    capture: &CwdCapture,
    expected_table: [u8; 16],
) -> Result<TypedRowRef<Kind>, SystemReferenceError> {
    validate_coordinates(&reference, capture, expected_table)?;
    let object =
        row_ref_from_raw(&reference.key).map_err(|_| SystemReferenceError::InvalidObjectKey)?;
    object_reference::validate_object_reference(object, capture)
        .map_err(|_| SystemReferenceError::InvalidObjectKey)?;
    Ok(TypedRowRef::from_row_ref(reference))
}

fn snapshot_id(snapshot: &CanonicalSnapshot) -> Result<[u8; 32], SystemReferenceError> {
    match snapshot {
        CanonicalSnapshot::Cwd { id, .. } => Ok(*id),
        CanonicalSnapshot::Commit { .. } => {
            orna_value_v1::domain_digest("orna.snapshot.v1", &snapshot.raw())
                .map_err(|_| SystemReferenceError::InvalidSnapshotKey)
        }
    }
}

fn checkpoint_key_from_raw(key: &OvbRaw) -> Result<(), SystemReferenceError> {
    let OvbRaw::Array(key) = key else {
        return Err(SystemReferenceError::InvalidCheckpointKey);
    };
    let [consumer, source, partition] = key.as_slice() else {
        return Err(SystemReferenceError::InvalidCheckpointKey);
    };
    match (consumer, source, partition) {
        (OvbRaw::Text(consumer), OvbRaw::Text(source), OvbRaw::Null)
            if !consumer.is_empty() && !source.is_empty() =>
        {
            Ok(())
        }
        (OvbRaw::Text(consumer), OvbRaw::Text(source), OvbRaw::Text(partition))
            if !consumer.is_empty() && !source.is_empty() && !partition.is_empty() =>
        {
            Ok(())
        }
        _ => Err(SystemReferenceError::InvalidCheckpointKey),
    }
}

fn validate_coordinates(
    reference: &RowRef,
    capture: &CwdCapture,
    expected_table: [u8; 16],
) -> Result<(), SystemReferenceError> {
    if reference.database_id != capture.database_id() {
        return Err(SystemReferenceError::DatabaseMismatch);
    }
    if reference.snapshot != *capture.snapshot() {
        return Err(SystemReferenceError::SnapshotMismatch);
    }
    if reference.table_id != expected_table {
        return Err(SystemReferenceError::RelationMismatch);
    }
    Ok(())
}

fn ensure_snapshot_database(
    database_id: [u8; 16],
    snapshot: &CanonicalSnapshot,
) -> Result<(), SystemReferenceError> {
    let snapshot_database = match snapshot {
        CanonicalSnapshot::Cwd { database, .. } | CanonicalSnapshot::Commit { database, .. } => {
            *database
        }
    };
    if snapshot_database != database_id {
        return Err(SystemReferenceError::DatabaseMismatch);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemReferenceError {
    DatabaseMismatch,
    SnapshotMismatch,
    RelationMismatch,
    InvalidInvocationKey,
    InvalidInvocationArgumentKey,
    InvalidObjectKey,
    InvalidSnapshotKey,
    InvalidRunKey,
    InvalidStreamKey,
    InvalidCheckpointKey,
    InvalidFailureKey,
}
impl fmt::Display for SystemReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DatabaseMismatch => f.write_str("system reference database does not match CWD"),
            Self::SnapshotMismatch => f.write_str("system reference snapshot does not match CWD"),
            Self::RelationMismatch => {
                f.write_str("system reference relation identity does not match")
            }
            Self::InvalidInvocationKey => f.write_str("invalid sys.Invocation natural key"),
            Self::InvalidInvocationArgumentKey => {
                f.write_str("invalid sys.InvocationArgument natural key")
            }
            Self::InvalidObjectKey => f.write_str("invalid sys.Object natural key"),
            Self::InvalidSnapshotKey => f.write_str("invalid sys.Snapshot natural key"),
            Self::InvalidRunKey => f.write_str("invalid sys.Run natural key"),
            Self::InvalidStreamKey => f.write_str("invalid sys.Stream natural key"),
            Self::InvalidCheckpointKey => f.write_str("invalid sys.Checkpoint natural key"),
            Self::InvalidFailureKey => f.write_str("invalid sys.Failure natural key"),
        }
    }
}
impl std::error::Error for SystemReferenceError {}

/// Exact `sys.SourceSpan`. Orna `Int` coordinates can exceed machine integers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub file: FileRef,
    pub start_byte: BigInt,
    pub end_byte: BigInt,
    pub start_line: BigInt,
    pub start_column: BigInt,
    pub end_line: BigInt,
    pub end_column: BigInt,
}
impl SourceSpan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file: FileRef,
        start_byte: BigInt,
        end_byte: BigInt,
        start_line: BigInt,
        start_column: BigInt,
        end_line: BigInt,
        end_column: BigInt,
    ) -> Result<Self, FoundationError> {
        if start_byte.sign() == Sign::Minus
            || end_byte < start_byte
            || [
                start_line.clone(),
                start_column.clone(),
                end_line.clone(),
                end_column.clone(),
            ]
            .iter()
            .any(|n| n <= &BigInt::from(0))
        {
            return Err(FoundationError::InvalidSourceSpan);
        }
        Ok(Self {
            file,
            start_byte,
            end_byte,
            start_line,
            start_column,
            end_line,
            end_column,
        })
    }

    /// Migration adapter for syntax front ends that only expose UTF-8 byte
    /// offsets. The source text supplies the required one-based coordinates;
    /// callers retain their existing parser-local span until migration.
    pub fn from_utf8_offsets(
        file: FileRef,
        source: &str,
        start_byte: usize,
        end_byte: usize,
    ) -> Result<Self, FoundationError> {
        if start_byte > end_byte
            || end_byte > source.len()
            || !source.is_char_boundary(start_byte)
            || !source.is_char_boundary(end_byte)
        {
            return Err(FoundationError::InvalidSourceSpan);
        }
        let coordinate = |offset: usize| {
            let prior = &source[..offset];
            (
                BigInt::from(prior.bytes().filter(|byte| *byte == b'\n').count() + 1),
                BigInt::from(
                    prior
                        .rsplit('\n')
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .count()
                        + 1,
                ),
            )
        };
        let (start_line, start_column) = coordinate(start_byte);
        let (end_line, end_column) = coordinate(end_byte);
        Self::new(
            file,
            start_byte.into(),
            end_byte.into(),
            start_line,
            start_column,
            end_line,
            end_column,
        )
    }
}
/// Exact `sys.DiagnosticLabel`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticLabel {
    pub span: SourceSpan,
    pub message: String,
    pub primary: bool,
}

/// Exact `sys.Value`: a validated tag-60026 `[closed_type_descriptor, value]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysValue(CanonicalValue);
impl SysValue {
    pub fn decode(bytes: &[u8]) -> Result<Self, FoundationError> {
        Self::from_value(Value::decode(bytes).map_err(FoundationError::Value)?)
    }
    pub fn from_value(value: CanonicalValue) -> Result<Self, FoundationError> {
        if matches!(value.raw(), OvbRaw::Tag(60026, _)) {
            Ok(Self(value))
        } else {
            Err(FoundationError::ExpectedSysValue)
        }
    }
    pub fn as_value(&self) -> &CanonicalValue {
        &self.0
    }
    pub fn encode(&self) -> Result<Vec<u8>, FoundationError> {
        self.0.encode().map_err(FoundationError::Value)
    }
}
/// Exact note/help/warning/error/fatal severity required by the portable
/// diagnostic boundary and the tag-60011 live protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Note,
    Help,
    Warning,
    Error,
    Fatal,
}
/// Text admitted to a diagnostic boundary. It rejects NUL/control injection
/// and requires producers to deliberately use [`SafeText::redacted`] when the
/// source text is not safe to disclose. This is the only constructor accepted
/// by the live diagnostic builder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeText(String);
impl SafeText {
    pub fn new(value: impl Into<String>) -> Result<Self, FoundationError> {
        let value = value.into();
        if value.chars().any(|character| {
            character == '\0' || character.is_control() && character != '\n' && character != '\t'
        }) {
            return Err(FoundationError::UnsafeDiagnosticText);
        }
        Ok(Self(value))
    }
    pub fn redacted() -> Self {
        Self("<redacted>".into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl DiagnosticSeverity {
    fn wire(self) -> u8 {
        match self {
            Self::Note => 0,
            Self::Help => 1,
            Self::Warning => 2,
            Self::Error => 3,
            Self::Fatal => 4,
        }
    }
    fn parse(value: u64) -> Result<Self, FoundationError> {
        match value {
            0 => Ok(Self::Note),
            1 => Ok(Self::Help),
            2 => Ok(Self::Warning),
            3 => Ok(Self::Error),
            4 => Ok(Self::Fatal),
            _ => Err(FoundationError::InvalidDiagnosticEncoding),
        }
    }
}
/// Exact `api/sys.json` `sys.Diagnostic` relation shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemDiagnostic {
    pub reference: DiagnosticRef,
    pub id: String,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub object: Option<ObjectRef>,
    pub definition: Option<DefinitionRef>,
    pub primary_span: Option<SourceSpan>,
    pub labels: Vec<DiagnosticLabel>,
    pub causes: Vec<SystemDiagnostic>,
    pub help: Vec<String>,
    pub data: Option<SysValue>,
    pub redacted: bool,
    pub trace: Option<TraceRef>,
}
/// Protocol span `[snapshot, file_path, start_byte, end_byte]`. The source
/// path is repository-relative or the explicit `<redacted>` marker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticSpan {
    pub snapshot: CanonicalSnapshot,
    pub file_path: String,
    pub start_byte: BigInt,
    pub end_byte: BigInt,
}
impl DiagnosticSpan {
    pub fn new(
        snapshot: CanonicalSnapshot,
        file_path: impl Into<String>,
        start_byte: BigInt,
        end_byte: BigInt,
    ) -> Result<Self, FoundationError> {
        let file_path = file_path.into();
        if file_path.is_empty()
            || (file_path != "<redacted>"
                && (!file_path.is_ascii()
                    || file_path.starts_with('/')
                    || file_path
                        .split('/')
                        .any(|x| x.is_empty() || matches!(x, "." | ".."))))
            || start_byte.sign() == Sign::Minus
            || end_byte < start_byte
        {
            return Err(FoundationError::InvalidDiagnosticSpan);
        }
        Ok(Self {
            snapshot,
            file_path,
            start_byte,
            end_byte,
        })
    }
    fn raw(&self) -> OvbRaw {
        OvbRaw::Array(vec![
            self.snapshot.raw(),
            OvbRaw::Text(self.file_path.clone()),
            OvbRaw::Int(self.start_byte.clone()),
            OvbRaw::Int(self.end_byte.clone()),
        ])
    }
    fn from_raw(raw: &OvbRaw) -> Result<Self, FoundationError> {
        let values = array(raw)?;
        if values.len() != 4 {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        }
        Self::new(
            Snapshot::decode(&values[0]).map_err(FoundationError::Value)?,
            text(&values[1])?,
            integer(&values[2])?,
            integer(&values[3])?,
        )
    }
}
/// Live-protocol `Diagnostic`: tag 60011 around exact integer-key map
/// `{0: code, 1: severity, 2: message, 3: spans, 4: notes, 5: causes,
/// 6: redacted, 7?: stable diagnostic UUID}`. Causes are recursively tagged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    code: SafeText,
    severity: DiagnosticSeverity,
    message: SafeText,
    spans: Vec<DiagnosticSpan>,
    notes: Vec<SafeText>,
    causes: Vec<Diagnostic>,
    redacted: bool,
    reference: Option<[u8; 16]>,
}
impl Diagnostic {
    pub fn new(
        code: SafeText,
        severity: DiagnosticSeverity,
        message: SafeText,
    ) -> Result<Self, FoundationError> {
        if code.as_str().is_empty() {
            return Err(FoundationError::InvalidDiagnostic);
        }
        Ok(Self {
            code,
            severity,
            message,
            spans: vec![],
            notes: vec![],
            causes: vec![],
            redacted: false,
            reference: None,
        })
    }
    pub fn with_span(mut self, span: DiagnosticSpan) -> Self {
        self.spans.push(span);
        self
    }
    pub fn with_note(mut self, note: SafeText) -> Self {
        self.notes.push(note);
        self
    }
    pub fn with_cause(mut self, cause: Diagnostic) -> Self {
        self.causes.push(cause);
        self
    }
    pub fn redacted(mut self) -> Self {
        self.redacted = true;
        self
    }
    pub fn with_reference(mut self, reference: [u8; 16]) -> Self {
        self.reference = Some(reference);
        self
    }
    pub fn code(&self) -> &str {
        self.code.as_str()
    }
    pub fn message(&self) -> &str {
        self.message.as_str()
    }
    pub fn encode_ovb(&self) -> Result<Vec<u8>, FoundationError> {
        Value::new(self.raw()?)
            .map_err(FoundationError::Value)?
            .encode()
            .map_err(FoundationError::Value)
    }
    pub fn decode_ovb(bytes: &[u8]) -> Result<Self, FoundationError> {
        Self::from_raw(Value::decode(bytes).map_err(FoundationError::Value)?.raw())
    }
    fn raw(&self) -> Result<OvbRaw, FoundationError> {
        let mut fields = vec![
            (0, OvbRaw::Text(self.code.as_str().into())),
            (1, OvbRaw::Int(self.severity.wire().into())),
            (2, OvbRaw::Text(self.message.as_str().into())),
            (
                3,
                OvbRaw::Array(self.spans.iter().map(DiagnosticSpan::raw).collect()),
            ),
            (
                4,
                OvbRaw::Array(
                    self.notes
                        .iter()
                        .map(|note| OvbRaw::Text(note.as_str().into()))
                        .collect(),
                ),
            ),
            (
                5,
                OvbRaw::Array(
                    self.causes
                        .iter()
                        .map(Diagnostic::raw)
                        .collect::<Result<_, _>>()?,
                ),
            ),
            (6, OvbRaw::Bool(self.redacted)),
        ];
        if let Some(reference) = self.reference {
            fields.push((7, uuid(reference)));
        }
        Ok(OvbRaw::Tag(
            60011,
            Box::new(OvbRaw::Map(
                fields
                    .into_iter()
                    .map(|(key, value)| (OvbRaw::Int(key.into()), value))
                    .collect(),
            )),
        ))
    }
    fn from_raw(raw: &OvbRaw) -> Result<Self, FoundationError> {
        let OvbRaw::Tag(60011, body) = raw else {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        };
        let fields = integer_fields(body)?;
        if fields.keys().any(|key| *key > 7) {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        }
        Ok(Self {
            code: SafeText::new(text(required(&fields, 0)?)?)?,
            severity: DiagnosticSeverity::parse(natural(required(&fields, 1)?)?)?,
            message: SafeText::new(text(required(&fields, 2)?)?)?,
            spans: array_field(&fields, 3)?
                .iter()
                .map(DiagnosticSpan::from_raw)
                .collect::<Result<_, _>>()?,
            notes: array_field(&fields, 4)?
                .iter()
                .map(text)
                .map(|note| note.and_then(SafeText::new))
                .collect::<Result<_, _>>()?,
            causes: array_field(&fields, 5)?
                .iter()
                .map(Self::from_raw)
                .collect::<Result<_, _>>()?,
            redacted: boolean(required(&fields, 6)?)?,
            reference: fields.get(&7).map(|value| uuid_bytes(value)).transpose()?,
        })
    }
}

/// JSON evidence representation for the live protocol diagnostic. This is
/// intentionally separate from tag-60011: JSON is an audit/report transport,
/// whereas tag-60011 remains the normative binary codec.
///
/// Values that JSON cannot represent safely are deliberately strings: UUIDs
/// are canonical lower-case UUID text, arbitrary precision integers are base
/// ten text, and canonical OVB values are lower-case hexadecimal bytes.
impl Serialize for Diagnostic {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        let mut state = serializer.serialize_struct("Diagnostic", 8)?;
        state.serialize_field("code", self.code.as_str())?;
        state.serialize_field("severity", diagnostic_severity_name(self.severity))?;
        state.serialize_field("message", self.message.as_str())?;
        state.serialize_field("spans", &DiagnosticSpans(&self.spans))?;
        state.serialize_field("notes", &SafeTexts(&self.notes))?;
        state.serialize_field("causes", &Diagnostics(&self.causes))?;
        state.serialize_field("redacted", &self.redacted)?;
        if let Some(reference) = self.reference {
            state.serialize_field("reference", &uuid_text(reference))?;
        }
        state.end()
    }
}

struct SafeTexts<'a>(&'a [SafeText]);
impl Serialize for SafeTexts<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.collect_seq(self.0.iter().map(SafeText::as_str))
    }
}
struct Diagnostics<'a>(&'a [Diagnostic]);
impl Serialize for Diagnostics<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.collect_seq(self.0)
    }
}
struct DiagnosticSpans<'a>(&'a [DiagnosticSpan]);
impl Serialize for DiagnosticSpans<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        serializer.collect_seq(self.0.iter().map(SerializableDiagnosticSpan))
    }
}
struct SerializableDiagnosticSpan<'a>(&'a DiagnosticSpan);
impl Serialize for SerializableDiagnosticSpan<'_> {
    fn serialize<T: Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        let span = self.0;
        let snapshot = Value::new(span.snapshot.raw())
            .and_then(|value| value.encode())
            .map_err(serde::ser::Error::custom)?;
        let mut state = serializer.serialize_struct("DiagnosticSpan", 4)?;
        state.serialize_field("snapshot", &hex(&snapshot))?;
        state.serialize_field("file-path", &span.file_path)?;
        state.serialize_field("start-byte", &span.start_byte.to_string())?;
        state.serialize_field("end-byte", &span.end_byte.to_string())?;
        state.end()
    }
}

fn diagnostic_severity_name(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Note => "note",
        DiagnosticSeverity::Help => "help",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Fatal => "fatal",
    }
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    text
}
fn uuid_text(value: [u8; 16]) -> String {
    let hex = hex(&value);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Identity supplied before CWD admission, preventing cross-repository CAS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryIdentity {
    pub database_id: [u8; 16],
    pub repository_id: [u8; 16],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RepositoryProfile {
    pub bare: bool,
}
/// Exact logical CWD pin; committed snapshots cannot be placed here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwdCapture {
    snapshot: CanonicalSnapshot,
    /// The durable state digest observed with this logical generation. It is
    /// part of the capture and therefore part of the compare-and-set key.
    generation_digest: [u8; 32],
}
impl CwdCapture {
    pub fn new(
        snapshot: CanonicalSnapshot,
        generation_digest: [u8; 32],
    ) -> Result<Self, FoundationError> {
        if matches!(snapshot, Snapshot::Cwd { .. }) {
            Ok(Self {
                snapshot,
                generation_digest,
            })
        } else {
            Err(FoundationError::ExpectedCwdSnapshot)
        }
    }
    pub fn database_id(&self) -> [u8; 16] {
        match &self.snapshot {
            Snapshot::Cwd { database, .. } => *database,
            Snapshot::Commit { .. } => unreachable!("CwdCapture validates CWD snapshots"),
        }
    }
    pub fn runtime_id(&self) -> [u8; 16] {
        match &self.snapshot {
            Snapshot::Cwd { runtime, .. } => *runtime,
            Snapshot::Commit { .. } => unreachable!("CwdCapture validates CWD snapshots"),
        }
    }
    pub fn generation(&self) -> &BigInt {
        match &self.snapshot {
            Snapshot::Cwd { generation, .. } => generation,
            Snapshot::Commit { .. } => unreachable!("CwdCapture validates CWD snapshots"),
        }
    }
    pub fn snapshot(&self) -> &CanonicalSnapshot {
        &self.snapshot
    }
    pub fn generation_digest(&self) -> [u8; 32] {
        self.generation_digest
    }
}
/// CAS never collapses stale state into a generic error; callers receive the
/// currently authoritative CWD pin and must retry deliberately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwdCas {
    Updated { current: CwdCapture },
    Stale { current: CwdCapture },
}
/// Durable runtime state owned below the repository boundary. Git never
/// manufactures a runtime UUID or logical CWD generation: the store supplies
/// both, and advances generation monotonically when it publishes a new CWD.
pub trait RuntimeIdentityStore {
    type Error: std::error::Error + Send + Sync + 'static;
    fn database_id(&self) -> Result<[u8; 16], Self::Error>;
    fn repository_id(&self) -> Result<[u8; 16], Self::Error>;
    fn runtime_id(&self) -> Result<[u8; 16], Self::Error>;
    /// Returns the persisted complete CWD capture, including the digest which
    /// binds the logical generation to the durable runtime state.
    fn capture_cwd(&self) -> Result<CwdCapture, Self::Error>;
    /// Atomically publishes `next` only when the durable current pin equals
    /// `expected`; stale publication returns the current authoritative pin.
    fn compare_and_set_cwd(
        &self,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error>;
}
/// Repository/Git and runtime adapters meet at this direction-only contract.
pub trait RepositoryGenerationAdapter {
    type Error: std::error::Error + Send + Sync + 'static;
    fn require_cwd(&self) -> Result<RepositoryIdentity, Self::Error>;
    fn profile(&self) -> Result<RepositoryProfile, Self::Error>;
    fn committed_snapshot(&self) -> Result<Option<CanonicalSnapshot>, Self::Error>;
    fn capture_cwd(&self, identity: RepositoryIdentity) -> Result<CwdCapture, Self::Error>;
    fn compare_and_set_cwd(
        &self,
        identity: RepositoryIdentity,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error>;
}
pub fn require_cwd_repository(profile: RepositoryProfile) -> Result<(), FoundationError> {
    if profile.bare {
        Err(FoundationError::BareRepositoryHasNoCwd)
    } else {
        Ok(())
    }
}

/// Atomic embedded reference implementation of the shared CWD CAS contract.
pub struct InMemoryRepositoryAdapter {
    identity: RepositoryIdentity,
    profile: RepositoryProfile,
    cwd: Mutex<CwdCapture>,
}
impl InMemoryRepositoryAdapter {
    pub fn new(identity: RepositoryIdentity, profile: RepositoryProfile, cwd: CwdCapture) -> Self {
        Self {
            identity,
            profile,
            cwd: Mutex::new(cwd),
        }
    }
}
#[derive(Debug)]
pub struct InMemoryRepositoryError;
impl fmt::Display for InMemoryRepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("repository adapter state unavailable")
    }
}
impl std::error::Error for InMemoryRepositoryError {}
impl RepositoryGenerationAdapter for InMemoryRepositoryAdapter {
    type Error = InMemoryRepositoryError;
    fn require_cwd(&self) -> Result<RepositoryIdentity, Self::Error> {
        require_cwd_repository(self.profile).map_err(|_| InMemoryRepositoryError)?;
        Ok(self.identity)
    }
    fn profile(&self) -> Result<RepositoryProfile, Self::Error> {
        Ok(self.profile)
    }
    fn committed_snapshot(&self) -> Result<Option<CanonicalSnapshot>, Self::Error> {
        Ok(None)
    }
    fn capture_cwd(&self, identity: RepositoryIdentity) -> Result<CwdCapture, Self::Error> {
        if identity != self.identity {
            return Err(InMemoryRepositoryError);
        }
        Ok(self
            .cwd
            .lock()
            .map_err(|_| InMemoryRepositoryError)?
            .clone())
    }
    fn compare_and_set_cwd(
        &self,
        identity: RepositoryIdentity,
        expected: &CwdCapture,
        next: &CwdCapture,
    ) -> Result<CwdCas, Self::Error> {
        if identity != self.identity {
            return Err(InMemoryRepositoryError);
        }
        let mut current = self.cwd.lock().map_err(|_| InMemoryRepositoryError)?;
        if *current != *expected {
            return Ok(CwdCas::Stale {
                current: current.clone(),
            });
        }
        *current = next.clone();
        Ok(CwdCas::Updated {
            current: next.clone(),
        })
    }
}

#[derive(Debug)]
pub enum FoundationError {
    Value(ValueError),
    InvalidSourceSpan,
    InvalidDiagnosticSpan,
    InvalidDiagnostic,
    InvalidDiagnosticEncoding,
    ExpectedSysValue,
    ExpectedCwdSnapshot,
    BareRepositoryHasNoCwd,
    UnsafeDiagnosticText,
    InvalidSystemReferenceEncoding,
}
impl fmt::Display for FoundationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(error) => write!(f, "canonical value error: {error}"),
            Self::InvalidSourceSpan => f.write_str("invalid source span"),
            Self::InvalidDiagnosticSpan => f.write_str("invalid diagnostic span"),
            Self::InvalidDiagnostic => f.write_str("invalid diagnostic"),
            Self::InvalidDiagnosticEncoding => f.write_str("invalid diagnostic encoding"),
            Self::ExpectedSysValue => f.write_str("expected tag 60026 sys.Value"),
            Self::ExpectedCwdSnapshot => f.write_str("expected CWD snapshot"),
            Self::BareRepositoryHasNoCwd => f.write_str("a bare repository has no CWD"),
            Self::UnsafeDiagnosticText => f.write_str("unsafe diagnostic text"),
            Self::InvalidSystemReferenceEncoding => {
                f.write_str("invalid canonical system reference encoding")
            }
        }
    }
}
impl std::error::Error for FoundationError {}

fn uuid(value: [u8; 16]) -> OvbRaw {
    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(value.to_vec())))
}
fn row_ref_from_raw(raw: &OvbRaw) -> Result<RowRef, FoundationError> {
    let OvbRaw::Tag(60010, body) = raw else {
        return Err(FoundationError::InvalidSystemReferenceEncoding);
    };
    let fields = array(body)?;
    let [database_id, table_id, key, snapshot] = fields.as_slice() else {
        return Err(FoundationError::InvalidSystemReferenceEncoding);
    };
    RowRef::new(
        uuid_bytes(database_id)?,
        uuid_bytes(table_id)?,
        key.clone(),
        Snapshot::decode(snapshot).map_err(FoundationError::Value)?,
    )
}
fn row_ref_raw(reference: &RowRef) -> OvbRaw {
    OvbRaw::Tag(
        60010,
        Box::new(OvbRaw::Array(vec![
            uuid(reference.database_id),
            uuid(reference.table_id),
            reference.key.clone(),
            reference.snapshot.raw(),
        ])),
    )
}
fn is_run_id_key(raw: &OvbRaw) -> bool {
    // The 1.0.0 sys contract uses the canonical UUID representation for
    // `sys.RunId`: OVB tag 37 wrapping exactly sixteen bytes.
    is_opaque_id_key(raw)
}
fn is_opaque_id_key(raw: &OvbRaw) -> bool {
    // The 1.0.0 sys contract declares InvocationId and RunId as opaque
    // identifiers; this implementation uses the canonical UUID OVB form.
    matches!(raw, OvbRaw::Tag(37, value) if matches!(value.as_ref(), OvbRaw::Bytes(bytes) if bytes.len() == 16))
}
fn is_nonnegative_u64_integer(raw: &OvbRaw) -> bool {
    matches!(raw, OvbRaw::Int(value) if value.sign() != Sign::Minus && value <= &BigInt::from(u64::MAX))
}
fn array(raw: &OvbRaw) -> Result<&Vec<OvbRaw>, FoundationError> {
    if let OvbRaw::Array(values) = raw {
        Ok(values)
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn text(raw: &OvbRaw) -> Result<String, FoundationError> {
    if let OvbRaw::Text(value) = raw {
        Ok(value.clone())
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn integer(raw: &OvbRaw) -> Result<BigInt, FoundationError> {
    if let OvbRaw::Int(value) = raw {
        Ok(value.clone())
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn natural(raw: &OvbRaw) -> Result<u64, FoundationError> {
    integer(raw)?
        .try_into()
        .map_err(|_| FoundationError::InvalidDiagnosticEncoding)
}
fn boolean(raw: &OvbRaw) -> Result<bool, FoundationError> {
    if let OvbRaw::Bool(value) = raw {
        Ok(*value)
    } else {
        Err(FoundationError::InvalidDiagnosticEncoding)
    }
}
fn uuid_bytes(raw: &OvbRaw) -> Result<[u8; 16], FoundationError> {
    let OvbRaw::Tag(37, value) = raw else {
        return Err(FoundationError::InvalidDiagnosticEncoding);
    };
    let OvbRaw::Bytes(bytes) = value.as_ref() else {
        return Err(FoundationError::InvalidDiagnosticEncoding);
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| FoundationError::InvalidDiagnosticEncoding)
}
fn integer_fields(raw: &OvbRaw) -> Result<BTreeMap<u64, &OvbRaw>, FoundationError> {
    let OvbRaw::Map(entries) = raw else {
        return Err(FoundationError::InvalidDiagnosticEncoding);
    };
    let mut fields = BTreeMap::new();
    for (key, value) in entries {
        if fields.insert(natural(key)?, value).is_some() {
            return Err(FoundationError::InvalidDiagnosticEncoding);
        }
    }
    Ok(fields)
}
fn required<'a>(
    fields: &'a BTreeMap<u64, &'a OvbRaw>,
    key: u64,
) -> Result<&'a OvbRaw, FoundationError> {
    fields
        .get(&key)
        .copied()
        .ok_or(FoundationError::InvalidDiagnosticEncoding)
}
fn array_field<'a>(
    fields: &'a BTreeMap<u64, &'a OvbRaw>,
    key: u64,
) -> Result<&'a Vec<OvbRaw>, FoundationError> {
    array(required(fields, key)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit() -> CanonicalSnapshot {
        Snapshot::Commit {
            database: [7; 16],
            algorithm: GitHash::Sha256,
            oid: vec![9; 32],
        }
    }
    #[test]
    fn diagnostic_ovb_round_trip_uses_tag_60011_integer_keys_and_recursive_tags() {
        let cause = Diagnostic::new(
            SafeText::new("CAUSE").unwrap(),
            DiagnosticSeverity::Help,
            SafeText::new("nested").unwrap(),
        )
        .unwrap();
        let diagnostic = Diagnostic::new(
            SafeText::new("ORNA091-E-VAR").unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new("use let").unwrap(),
        )
        .unwrap()
        .with_span(DiagnosticSpan::new(commit(), "src/main.orna", 0.into(), 3.into()).unwrap())
        .with_note(SafeText::new("safe note").unwrap())
        .with_cause(cause)
        .with_reference([3; 16]);
        let bytes = diagnostic.encode_ovb().unwrap();
        assert!(matches!(
            Value::decode(&bytes).unwrap().raw(),
            OvbRaw::Tag(60011, _)
        ));
        assert_eq!(Diagnostic::decode_ovb(&bytes).unwrap(), diagnostic);
    }
    #[test]
    fn committed_and_cwd_snapshot_pins_round_trip_without_rebinding() {
        for snapshot in [
            commit(),
            Snapshot::cwd([1; 16], [2; 16], 42.into()).unwrap(),
        ] {
            let bytes = Value::new(snapshot.raw()).unwrap().encode().unwrap();
            assert_eq!(
                Snapshot::decode(Value::decode(&bytes).unwrap().raw()).unwrap(),
                snapshot
            );
        }
    }
    #[test]
    fn bare_repository_is_rejected_before_cwd_capture() {
        assert!(matches!(
            require_cwd_repository(RepositoryProfile { bare: true }),
            Err(FoundationError::BareRepositoryHasNoCwd)
        ));
    }
    #[test]
    fn in_memory_adapter_rejects_bare_repository_before_observing_capture() {
        let capture =
            CwdCapture::new(Snapshot::cwd([1; 16], [2; 16], 0.into()).unwrap(), [3; 32]).unwrap();
        let adapter = InMemoryRepositoryAdapter::new(
            RepositoryIdentity {
                database_id: [1; 16],
                repository_id: [4; 16],
            },
            RepositoryProfile { bare: true },
            capture,
        );
        assert!(adapter.require_cwd().is_err());
    }
    #[test]
    fn source_span_uses_pinned_file_ref_and_unbounded_int_offsets() {
        let file = FileRef::from_row_ref(
            RowRef::new([1; 16], [2; 16], OvbRaw::Text("file".into()), commit()).unwrap(),
        );
        let span = SourceSpan::new(
            file,
            0.into(),
            BigInt::from(u64::MAX) + 1,
            1.into(),
            1.into(),
            1.into(),
            2.into(),
        )
        .unwrap();
        assert!(span.end_byte > BigInt::from(u64::MAX));
    }
    #[test]
    fn lifecycle_reference_markers_round_trip_and_require_matching_cwd_context() {
        let snapshot = Snapshot::cwd([1; 16], [2; 16], 3.into()).unwrap();
        let capture = CwdCapture::new(snapshot.clone(), [4; 32]).unwrap();
        let reference = RowRef::new(
            [1; 16],
            [5; 16],
            OvbRaw::Text("implementation-defined-key".into()),
            snapshot,
        )
        .unwrap();
        let encoded = reference.encode().unwrap();
        let decoded = row_ref_from_raw(Value::decode(&encoded).unwrap().raw()).unwrap();
        assert_eq!(decoded, reference);

        let function: FunctionRef = validate_reference_context(decoded.clone(), &capture).unwrap();
        let invocation: InvocationRef =
            validate_reference_context(decoded.clone(), &capture).unwrap();
        let checkpoint: CheckpointRef =
            validate_reference_context(decoded.clone(), &capture).unwrap();
        let failure: FailureRef = validate_reference_context(decoded.clone(), &capture).unwrap();
        assert_eq!(function.as_row_ref(), invocation.as_row_ref());
        assert_eq!(checkpoint.as_row_ref(), failure.as_row_ref());

        let wrong_database =
            CwdCapture::new(Snapshot::cwd([9; 16], [2; 16], 3.into()).unwrap(), [4; 32]).unwrap();
        assert_eq!(
            validate_reference_context::<FunctionKind>(reference, &wrong_database),
            Err(SystemReferenceError::DatabaseMismatch)
        );

        let wrong_snapshot =
            CwdCapture::new(Snapshot::cwd([1; 16], [2; 16], 4.into()).unwrap(), [4; 32]).unwrap();
        assert_eq!(
            validate_reference_context::<InvocationKind>(decoded, &wrong_snapshot),
            Err(SystemReferenceError::SnapshotMismatch)
        );
    }
    #[test]
    fn catalogue_and_snapshot_references_use_exact_pinned_natural_keys() {
        let snapshot = Snapshot::cwd([1; 16], [2; 16], 3.into()).unwrap();
        let capture = CwdCapture::new(snapshot.clone(), [4; 32]).unwrap();
        let object = object_reference([1; 16], [6; 16], snapshot.clone()).unwrap();

        let type_reference = type_reference(object.clone()).unwrap();
        let function_reference = function_reference(object.clone()).unwrap();
        let snapshot_ref = snapshot_reference([1; 16], snapshot.clone()).unwrap();

        assert_eq!(
            row_ref_from_raw(&type_reference.as_row_ref().key).unwrap(),
            object.clone().into_row_ref()
        );
        assert_eq!(
            row_ref_from_raw(&function_reference.as_row_ref().key).unwrap(),
            object.into_row_ref()
        );
        assert_eq!(
            snapshot_ref.as_row_ref().key,
            OvbRaw::Bytes(match &snapshot {
                Snapshot::Cwd { id, .. } => id.to_vec(),
                Snapshot::Commit { .. } => unreachable!(),
            })
        );
        let committed = commit();
        let committed_reference = snapshot_reference([7; 16], committed.clone()).unwrap();
        assert_eq!(
            committed_reference.as_row_ref().key,
            OvbRaw::Bytes(
                orna_value_v1::domain_digest("orna.snapshot.v1", &committed.raw())
                    .unwrap()
                    .to_vec()
            )
        );

        for reference in [
            type_reference.as_row_ref(),
            function_reference.as_row_ref(),
            snapshot_ref.as_row_ref(),
        ] {
            let decoded =
                row_ref_from_raw(Value::decode(&reference.encode().unwrap()).unwrap().raw())
                    .unwrap();
            assert_eq!(decoded, *reference);
        }
        assert_eq!(
            validate_type_reference(type_reference.clone().into_row_ref(), &capture)
                .unwrap()
                .as_row_ref(),
            type_reference.as_row_ref()
        );
        assert_eq!(
            validate_function_reference(function_reference.clone().into_row_ref(), &capture)
                .unwrap()
                .as_row_ref(),
            function_reference.as_row_ref()
        );
        assert_eq!(
            validate_snapshot_reference(snapshot_ref.clone().into_row_ref(), &capture)
                .unwrap()
                .as_row_ref(),
            snapshot_ref.as_row_ref()
        );
    }
    #[test]
    fn catalogue_and_snapshot_reference_validation_rejects_invalid_coordinates_and_keys() {
        let snapshot = Snapshot::cwd([1; 16], [2; 16], 3.into()).unwrap();
        let capture = CwdCapture::new(snapshot.clone(), [4; 32]).unwrap();
        let object = object_reference([1; 16], [6; 16], snapshot.clone()).unwrap();
        let type_reference = type_reference(object).unwrap().into_row_ref();
        let snapshot_ref = snapshot_reference([1; 16], snapshot.clone())
            .unwrap()
            .into_row_ref();

        let mut null_object_key = type_reference.clone();
        null_object_key.key = OvbRaw::Null;
        assert_eq!(
            validate_type_reference(null_object_key, &capture),
            Err(SystemReferenceError::InvalidObjectKey)
        );
        let mut wrong_object_snapshot = type_reference.clone();
        wrong_object_snapshot.key = row_ref_raw(
            &RowRef::new(
                [1; 16],
                SYS_OBJECT_TABLE_ID,
                OvbRaw::Array(vec![
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes([6; 16].to_vec()))),
                    Snapshot::cwd([1; 16], [2; 16], 4.into()).unwrap().raw(),
                ]),
                Snapshot::cwd([1; 16], [2; 16], 4.into()).unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(
            validate_type_reference(wrong_object_snapshot, &capture),
            Err(SystemReferenceError::InvalidObjectKey)
        );
        let mut null_snapshot_key = snapshot_ref.clone();
        null_snapshot_key.key = OvbRaw::Null;
        assert_eq!(
            validate_snapshot_reference(null_snapshot_key, &capture),
            Err(SystemReferenceError::InvalidSnapshotKey)
        );

        let wrong_database =
            CwdCapture::new(Snapshot::cwd([9; 16], [2; 16], 3.into()).unwrap(), [4; 32]).unwrap();
        assert_eq!(
            validate_type_reference(type_reference.clone(), &wrong_database),
            Err(SystemReferenceError::DatabaseMismatch)
        );
        let wrong_snapshot =
            CwdCapture::new(Snapshot::cwd([1; 16], [2; 16], 4.into()).unwrap(), [4; 32]).unwrap();
        assert_eq!(
            validate_snapshot_reference(snapshot_ref, &wrong_snapshot),
            Err(SystemReferenceError::SnapshotMismatch)
        );
        assert_eq!(
            snapshot_reference([9; 16], snapshot),
            Err(SystemReferenceError::DatabaseMismatch)
        );
    }
    #[test]
    fn checkpoint_and_failure_references_preserve_nullable_natural_keys() {
        let snapshot = Snapshot::cwd([1; 16], [2; 16], 3.into()).unwrap();
        let capture = CwdCapture::new(snapshot.clone(), [4; 32]).unwrap();
        for partition in [None, Some("partition-a".into())] {
            let checkpoint = checkpoint_reference(
                [1; 16],
                snapshot.clone(),
                "principal/root/function/binding".into(),
                "source-a".into(),
                partition.clone(),
            )
            .unwrap();
            assert_eq!(
                validate_checkpoint_reference(checkpoint.as_row_ref().clone(), &capture).unwrap(),
                checkpoint
            );
            let failure = failure_reference(
                [1; 16],
                snapshot.clone(),
                "principal/root/function/binding".into(),
                "source-a".into(),
                partition,
                "position-format".into(),
                "position-a".into(),
            )
            .unwrap();
            assert_eq!(
                validate_failure_reference(failure.as_row_ref().clone(), &capture).unwrap(),
                failure
            );
        }
        assert!(
            checkpoint_reference(
                [1; 16],
                snapshot,
                "consumer".into(),
                "source".into(),
                Some(String::new()),
            )
            .is_err()
        );
    }
    #[test]
    fn diagnostic_json_evidence_is_safe_lossless_and_not_the_ovb_codec() {
        let diagnostic = Diagnostic::new(
            SafeText::new("ORNA091-E-VAR").unwrap(),
            DiagnosticSeverity::Error,
            SafeText::new("safe message").unwrap(),
        )
        .unwrap()
        .with_span(
            DiagnosticSpan::new(
                commit(),
                "src/main.orna",
                BigInt::from(u64::MAX) + 1,
                BigInt::from(u64::MAX) + 2,
            )
            .unwrap(),
        )
        .with_note(SafeText::redacted())
        .with_cause(
            Diagnostic::new(
                SafeText::new("CAUSE").unwrap(),
                DiagnosticSeverity::Help,
                SafeText::new("nested").unwrap(),
            )
            .unwrap()
            .redacted(),
        )
        .redacted()
        .with_reference([0xab; 16]);
        let json = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(json["severity"], "error");
        assert_eq!(json["reference"], "abababab-abab-abab-abab-abababababab");
        assert_eq!(json["spans"][0]["start-byte"], "18446744073709551616");
        assert!(
            json["spans"][0]["snapshot"]
                .as_str()
                .unwrap()
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert_eq!(json["notes"][0], "<redacted>");
        assert_eq!(json["causes"][0]["redacted"], true);
        // The audit representation admits arbitrary precision coordinates as
        // decimal text; tag-60011 remains independently verified above.
        assert!(serde_json::to_vec(&diagnostic).unwrap().starts_with(b"{"));
    }
}
