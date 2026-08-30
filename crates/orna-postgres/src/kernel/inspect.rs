//! Protected inspection storage and projections from ADR 0064.
//!
//! Every capture is one immutable `_orna_kernel.inspect_snapshots` row keyed
//! by a fresh [`InspectEpochId`] and pinned by the source and catalogue
//! revision pair active at capture time. The canonical epoch payload
//! (`summary_bytes`) is the ORV5 encoding of a `BYTES` value whose body is
//! the closed [`InspectSnapshotEpoch`] envelope defined below, so the whole
//! epoch — id, pinned pair, owner, summary, and every projection row set —
//! round-trips through the verified standard's opaque codec registry exactly
//! like a USER state value.
//!
//! The trace relation retains one row per closed stream event kind. The v1
//! sealed route captures `started`/`value_batch`/`completed` rows for the
//! produced event batch plus one `inspect_snapshot` row; the model stream
//! API returns only the events the closed model payload set can express and
//! retains the richer durable kinds for later slices.
// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
#[path = "inspect/access.rs"]
mod access;
#[path = "inspect/capture.rs"]
mod capture;
#[path = "inspect/operations.rs"]
mod operations;
#[path = "inspect/payload.rs"]
mod payload;
#[path = "inspect/projection.rs"]
mod projection;
#[path = "inspect/storage.rs"]
mod storage;
pub(crate) use capture::capture_inspect_snapshot_in_transaction;
pub(crate) use storage::recover_inspect_relations;

use access::{
    finish_inspect_session, rebind_inspect_session, require_inspect_epoch_access,
    require_inspect_privilege,
};
use payload::encode_epoch_payload;
use projection::model_payload_for;
use storage::{
    decode_inspect_snapshot_row, inspect_id, inspect_value_registry, row_invocation_record,
};

use std::time::{Duration, SystemTime};

use orna_core::{
    CallSiteId, CatalogueRevisionId, FunctionId, InspectEpochId, InvocationId, PrincipalId,
    SecurityAuditEventId, SourceRevisionId, StateSlotId, TypeId,
    inspect::{
        CallRow, InspectClassifier, InspectInvocationNodeKind, InspectInvocationPhase,
        InspectObserverContext, InspectObserverPurpose, InspectOutcomeKind, InspectPrivilege,
        InspectResultSummary, InspectSecurityDecisionKind, InspectSecurityDecisionOutcome,
        InspectSnapshotEpoch, InspectSnapshotOptions, InspectSnapshotSummary, InspectTraceEvent,
        InspectTracePayload, InvocationNodeRow, PresentationCandidateRow, ResourceRow,
        RuntimeBindingRow, SecurityDecisionRow, StateCellRow, UiNodeRow,
    },
    inspect_carrier::MAX_INSPECT_CARRIER_ROWS,
    invocation::{
        InvocationClientOffer, InvocationEventBody, InvocationOutputRequirement, InvokeValue,
    },
    revision::ActiveDatabaseRevision,
    security::{
        AuthenticatedSession, InspectDecision, InspectDenial, InspectEpochScope,
        SecurityAuditDecision, SecuritySnapshot, authorise_inspect,
    },
    state::{UserStateCell, UserStateKeyWithoutPrincipal, is_sealed_inspect_runtime_value},
    types::TypeDescriptor,
    value::{
        MAX_OPAQUE_CODEC_PAYLOAD_LENGTH, MAX_RUNTIME_VALUE_NODES, OpaqueCodecRegistry, OpaqueValue,
        RuntimeValue,
    },
};
use orna_protocol::{InvocationEventBatch, decode_constructed_value, encode_constructed_value};
use orna_standard::{
    BYTE_STREAM_MAGIC, STD_IO_BYTE_STREAM_TYPE_ID, STD_TERMINAL_DOCUMENT_TYPE_ID, STD_UI_TYPE_ID,
    registered_opaque_codecs,
};
use tokio_postgres::{IsolationLevel, Row, Transaction, types::FromSqlOwned};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    is_sealed_inspect_type_id,
    physical::establish_trusted_search_path,
    security::{append_security_audit_event, recover_security_snapshot_for_active},
    security_admin::inspect_privileges_for_session,
    server_runtime::configure_and_recover,
};

const INSPECT_SNAPSHOT_RELATION: &str = "_orna_kernel.inspect_snapshots";
const INSPECT_TRACE_RELATION: &str = "_orna_kernel.inspect_trace_events";

const INSPECT_SNAPSHOT_SELECT: &str = "SELECT epoch_id, invocation_id, recorded_at,
        owner_principal_id, source_revision_id, catalogue_revision_id, summary_bytes,
        observer_root_invocation_id, observer_parent_invocation_id, observer_purpose
 FROM _orna_kernel.inspect_snapshots
 WHERE epoch_id = $1";
const INSPECT_SNAPSHOT_INSERT: &str = "INSERT INTO _orna_kernel.inspect_snapshots
    (epoch_id, invocation_id, recorded_at, owner_principal_id,
     source_revision_id, catalogue_revision_id, summary_bytes,
     observer_root_invocation_id, observer_parent_invocation_id, observer_purpose)
 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)";

/// The closed durable stream-kind set admitted by migration 0027.
const INSPECT_TRACE_KINDS: &[&str] = &[
    "started",
    "value_batch",
    "completed",
    "diagnostic",
    "inspect_snapshot",
    "inspect_projection",
    "inspect_trace",
    "security_decision",
];

/// The closed envelope magic and version of the canonical epoch payload.
const INSPECT_EPOCH_MAGIC: &[u8; 4] = b"INEP";
const INSPECT_EPOCH_VERSION_V1: u8 = 1;
const INSPECT_EPOCH_VERSION_V2: u8 = 2;
const INSPECT_OBSERVER_CONTEXT_PRESENT: u8 = 1;
const INSPECT_OBSERVER_PURPOSE_INSPECT: u8 = 1;

const PAYLOAD_ID_BYTES: usize = 16;
const PAYLOAD_U64_BYTES: usize = 8;

// Each persisted collection is bounded by the existing inspect carrier row
// limit. The per-item lower bounds below are only used to prove that a
// declared count can fit in the bytes left in the payload before allocating.
const MAX_PERSISTED_COLLECTION_ITEMS: usize = MAX_INSPECT_CARRIER_ROWS;
const INVOCATION_NODE_MIN_BYTES: usize =
    PAYLOAD_ID_BYTES + 1 + 1 + 1 + PAYLOAD_ID_BYTES + PAYLOAD_U64_BYTES;
const CALL_ROW_MIN_BYTES: usize = PAYLOAD_ID_BYTES + 1 + PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES;
const RESOURCE_ROW_MIN_BYTES: usize = 2;
const STATE_CELL_ROW_MIN_BYTES: usize = PAYLOAD_ID_BYTES
    + PAYLOAD_U64_BYTES
    + PAYLOAD_ID_BYTES
    + PAYLOAD_U64_BYTES
    + PAYLOAD_ID_BYTES
    + PAYLOAD_ID_BYTES
    + PAYLOAD_U64_BYTES
    + PAYLOAD_U64_BYTES
    + 4
    + 1;
const UI_NODE_ROW_MIN_BYTES: usize = PAYLOAD_ID_BYTES + PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES;
const PRESENTATION_CANDIDATE_ROW_MIN_BYTES: usize =
    PAYLOAD_U64_BYTES + 1 + PAYLOAD_U64_BYTES + 1 + 1;
const RUNTIME_BINDING_ROW_MIN_BYTES: usize =
    PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES + 1 + 4;
const SECURITY_DECISION_ROW_MIN_BYTES: usize =
    1 + 1 + PAYLOAD_U64_BYTES + 1 + 1 + PAYLOAD_U64_BYTES;
const TYPE_DESCRIPTOR_MIN_BYTES: usize = 1 + PAYLOAD_ID_BYTES;
const RUNTIME_CONTRACT_MIN_BYTES: usize = PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES + PAYLOAD_U64_BYTES;
const RUNTIME_FEATURE_MIN_BYTES: usize = PAYLOAD_U64_BYTES;

/// A snapshot authenticated against the current security snapshot.
///
/// The epoch, rebound session, and effective grants are captured together so
/// projection callers cannot substitute a forged epoch or privilege slice.
/// Instances can only be obtained from [`PostgresKernel::load_inspect_snapshot`].
pub struct AuthenticatedInspectSnapshot {
    epoch: InspectSnapshotEpoch,
    session: AuthenticatedSession,
    granted: Vec<InspectPrivilege>,
}

impl AuthenticatedInspectSnapshot {
    /// Returns the immutable inspection epoch identity.
    pub fn id(&self) -> InspectEpochId {
        self.epoch.id()
    }

    /// Returns the invocation identity captured by the epoch.
    pub fn invocation_id(&self) -> InvocationId {
        self.epoch.invocation_id()
    }

    /// Returns the source revision pinned by the epoch.
    pub fn source_revision_id(&self) -> SourceRevisionId {
        self.epoch.source_revision_id()
    }

    /// Returns the catalogue revision pinned by the epoch.
    pub fn catalogue_revision_id(&self) -> CatalogueRevisionId {
        self.epoch.catalogue_revision_id()
    }

    /// Returns the principal that owns the epoch.
    pub fn owner(&self) -> PrincipalId {
        self.epoch.owner()
    }

    /// Returns the time at which the epoch was recorded.
    pub fn recorded_at(&self) -> SystemTime {
        self.epoch.recorded_at()
    }

    /// Returns the root function targeted by the epoch.
    pub fn root_target(&self) -> FunctionId {
        self.epoch.root_target()
    }

    /// Returns the closed invocation outcome.
    pub fn outcome(&self) -> InspectOutcomeKind {
        self.epoch.outcome()
    }

    /// Returns the closed invocation summary.
    pub fn summary(&self) -> InspectSnapshotSummary {
        self.epoch.summary()
    }

    /// Returns the optional trusted observer context carried by this epoch.
    pub fn observer_context(&self) -> Option<InspectObserverContext> {
        self.epoch.observer_context()
    }

    /// Returns the effective INSPECT privileges captured with the epoch.
    pub fn granted(&self) -> &[InspectPrivilege] {
        &self.granted
    }
}
async fn lock_current_active_revision(
    transaction: &Transaction<'_>,
) -> Result<(), PostgresKernelError> {
    let row = transaction
        .query_opt(
            "SELECT singleton
             FROM _orna_kernel.active_revision
             WHERE singleton = true
             FOR UPDATE",
            &[],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    if row.is_none() {
        return Err(PostgresKernelError::DurableInvariant {
            relation: "_orna_kernel.active_revision",
            record: "singleton".to_owned(),
            rule: "the active revision row must exist before an Inspector clone",
        });
    }
    Ok(())
}

async fn persist_inspect_snapshot_clone(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    registry: &OpaqueCodecRegistry,
    target: InspectSnapshotEpoch,
    observer_context: InspectObserverContext,
    session: AuthenticatedSession,
    granted: Vec<InspectPrivilege>,
) -> Result<AuthenticatedInspectSnapshot, PostgresKernelError> {
    if target.source_revision_id() != active.pair().source()
        || target.catalogue_revision_id() != active.pair().catalogue()
    {
        return Err(PostgresKernelError::DurableInvariant {
            relation: INSPECT_SNAPSHOT_RELATION,
            record: target.id().canonical(),
            rule: "observer clone target must match the active revision pair",
        });
    }
    let epoch = target
        .clone_for_observer(observer_context)
        .map_err(PostgresKernelError::Inspect)?;
    let epoch_id = epoch.id();
    let recorded_at = epoch.recorded_at();
    let payload = encode_epoch_payload(active, registry, &epoch)?;
    let summary_bytes = encode_constructed_value(active, registry, &RuntimeValue::Bytes(payload))
        .map_err(PostgresKernelError::InspectValueCodec)?;
    let observer_context =
        epoch
            .observer_context()
            .ok_or_else(|| PostgresKernelError::DurableInvariant {
                relation: INSPECT_SNAPSHOT_RELATION,
                record: epoch_id.canonical(),
                rule: "observer clone must carry a trusted observer context",
            })?;
    let observer_root = observer_context
        .observer_root_invocation_id()
        .to_bytes()
        .to_vec();
    let observer_parent = observer_context
        .observer_parent_invocation_id()
        .to_bytes()
        .to_vec();
    let invocation_id = epoch.invocation_id().to_bytes().to_vec();
    let owner = epoch.owner().to_bytes().to_vec();
    let source_revision_id = epoch.source_revision_id().to_bytes().to_vec();
    let catalogue_revision_id = epoch.catalogue_revision_id().to_bytes().to_vec();
    let observer_purpose = observer_context.purpose().as_str();
    transaction
        .execute(
            INSPECT_SNAPSHOT_INSERT,
            &[
                &epoch_id.to_bytes().to_vec(),
                &invocation_id,
                &recorded_at,
                &owner,
                &source_revision_id,
                &catalogue_revision_id,
                &summary_bytes,
                &observer_root,
                &observer_parent,
                &observer_purpose,
            ],
        )
        .await
        .map_err(PostgresKernelError::Database)?;
    Ok(AuthenticatedInspectSnapshot {
        epoch,
        session,
        granted,
    })
}
