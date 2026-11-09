//! Runtime error taxonomy and the deterministic fault-injection seam.

use orna_foundation::CwdCapture;
use orna_stream::SafeDiagnostic;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    AfterMutation,
    AfterCheckpoint,
    AfterCapture,
    /// The writer transaction has already committed when the injector runs.
    /// Only a whole-process abort here models a crash between durable commit
    /// and the caller observing the result; an in-process `Err` would report
    /// a rollback that did not happen.
    AfterCommit,
    AfterFailureRecord,
    AfterFailurePayload,
    AfterReplayFailureRecord,
    BeforeTableWrite,
    AfterTableWrite,
    BeforeTerminalClaim,
    AfterTerminalClaim,
}

/// Deterministic test seam that selects a failure. A rollback receipt still
/// requires the runtime to observe an explicit rollback of its own
/// transaction.
pub trait FaultInjector: Send + Sync {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError>;
}

#[derive(Debug, Default)]
pub struct NoFault;

impl FaultInjector for NoFault {
    fn check(&self, _: FaultPoint) -> Result<(), RuntimeError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidIdentity,
    InvalidDigest,
    InvalidObservationReference,
    ObservationCoordinateMismatch,
    ProcedureMaterializationConflict,
    InvocationObservationConflict,
    InvocationStateConflict,
    QueryObservationConflict,
    QueryStateConflict,
    InvocationChildrenActive,
    InvocationOwnerInvalid,
    InvocationTailInvalid,
    InvocationTailLimit,
    StreamIdentityMismatch,
    MigrationCarryTargetDiffers,
    StreamSourceNotReplayable,
    StreamCheckpointStale,
    CheckpointNotReplayable,
    SnapshotNotFound,
    SnapshotIncomplete,
    SnapshotContextMismatch,
    LeaseHeld,
    AdminBusy,
    AdminInvocationConflict,
    OwnerLost,
    RecoveryPending,
    StaleCapture {
        current: Box<CwdCapture>,
    },
    InvalidCapture,
    EmptyMutationBatch,
    InvalidTableMutation,
    StaleTableSchema,
    ConflictingPublicationIntent,
    ConflictingPublicationCommit,
    InvalidPublicationCommit,
    InvalidPublicationPolicy,
    CompactPublicationRequired,
    CompactReceiptKeyMismatch,
    InvalidCompactReceipt,
    RequestUnknown,
    RequestFingerprintMismatch,
    RequestOwnerConflict,
    RequestStateConflict,
    SessionClosed,
    SessionWorkActive,
    TerminalOutcomeTooLarge,
    RecoveryInvalid,
    FaultInjected(FaultPoint),
    SystemProviderAbiInvalid,
    StorageUnavailable,
    /// The format recorded at `HEAD` is 1 or 2, so durable writes are refused
    /// (ORNA-UPGRADE-001, ORNA-UPGRADE-003). The refusal carries its code,
    /// format and remedy.
    LegacyHeadReadOnly(orna_repository::LegacyWriteRefused),
    /// The format recorded at `HEAD` is not a format this build knows, so
    /// runtime admission is refused before any worktree metadata or runtime
    /// state is read (ORNA-REPO-CONTEXT-003). The refusal carries its code.
    UnknownHeadFormat(orna_repository::FormatContextError),
    /// A retry, skip or resolve named a failure version or successor that is
    /// not the stored one (`sys.failure.stale_version`).
    StreamFailureStale,
    /// The requested failure transition is not allowed from the stored state,
    /// such as resolving a blocking open or retrying failure
    /// (`sys.failure.state_conflict`).
    StreamFailureStateConflict,
    /// The provider cannot safely advance past this failed delivery
    /// (`sys.failure.skip_unsupported`).
    StreamFailureSkipUnsupported,
}

impl RuntimeError {
    /// Returns the stable public system code when this runtime error has one.
    ///
    /// The provider boundary exposes the checkpoint outcomes and the failure
    /// transition outcomes required by the system contract. Other runtime
    /// failures stay internal to this crate's existing error convention.
    pub fn public_code(&self) -> Option<orna_sys::SysFailureCode> {
        match self {
            Self::AdminBusy => Some(orna_sys::SysFailureCode::AdminBusy),
            Self::StreamCheckpointStale => Some(orna_sys::SysFailureCode::CheckpointConflict),
            Self::CheckpointNotReplayable => {
                Some(orna_sys::SysFailureCode::CheckpointNotReplayable)
            }
            Self::SnapshotNotFound => Some(orna_sys::SysFailureCode::SnapshotNotFound),
            Self::SnapshotIncomplete => Some(orna_sys::SysFailureCode::SnapshotIncomplete),
            // No standalone cross-context code is defined; an exact pin that
            // cannot be resolved in this runtime is incomplete, never rebound.
            Self::SnapshotContextMismatch => Some(orna_sys::SysFailureCode::SnapshotIncomplete),
            Self::StreamFailureStale => Some(orna_sys::SysFailureCode::FailureStaleVersion),
            Self::StreamFailureStateConflict => {
                Some(orna_sys::SysFailureCode::FailureStateConflict)
            }
            Self::StreamFailureSkipUnsupported => {
                Some(orna_sys::SysFailureCode::FailureSkipUnsupported)
            }
            _ => None,
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Do not disclose local paths, SQL, payloads, or native error text.
        f.write_str(match self {
            Self::InvalidIdentity => "invalid runtime identity",
            Self::InvalidDigest => "invalid durable digest",
            Self::InvalidObservationReference => "invalid runtime observation reference",
            Self::ObservationCoordinateMismatch => "runtime observation coordinates do not match",
            Self::ProcedureMaterializationConflict => {
                "runtime procedure materialization conflicts with its catalogue row"
            }
            Self::InvocationObservationConflict => {
                "runtime invocation observation conflicts with retained state"
            }
            Self::InvocationStateConflict => "runtime invocation state transition conflicts",
            Self::QueryObservationConflict => {
                "runtime query observation conflicts with retained state"
            }
            Self::QueryStateConflict => "runtime query state transition conflicts",
            Self::InvocationChildrenActive => "runtime invocation has active child invocations",
            Self::InvocationOwnerInvalid => "runtime invocation owner or arguments are invalid",
            Self::InvocationTailInvalid => "runtime invocation tail cursor is invalid",
            Self::InvocationTailLimit => "runtime invocation tail page limit is invalid",
            Self::StreamIdentityMismatch => "stream source identity mismatch",
            Self::MigrationCarryTargetDiffers => {
                "migration carry target holds stream rows that differ from the carry"
            }
            Self::StreamSourceNotReplayable => "stream source does not support durable recovery",
            Self::StreamCheckpointStale => "stream checkpoint is stale",
            Self::CheckpointNotReplayable => "checkpoint target is not replayable",
            Self::SnapshotNotFound => "runtime checkpoint generation was not found",
            Self::SnapshotIncomplete => "historical runtime generation is incomplete",
            Self::SnapshotContextMismatch => "historical snapshot contexts do not match",
            Self::StreamFailureStale => "stream failure changed after it was observed",
            Self::StreamFailureStateConflict => {
                "stream failure transition is not allowed from its state"
            }
            Self::StreamFailureSkipUnsupported => "stream failure cannot be skipped safely",
            Self::LeaseHeld => "runtime writer is held",
            Self::AdminBusy => "runtime administration callback is busy",
            Self::AdminInvocationConflict => "administrative invocation identity was reused",
            Self::OwnerLost => "runtime writer ownership was lost",
            Self::RecoveryPending => "runtime takeover recovery is pending",
            Self::StaleCapture { .. } => "runtime capture is stale",
            Self::InvalidCapture => "invalid runtime capture",
            Self::EmptyMutationBatch => "empty runtime mutation batch",
            Self::InvalidTableMutation => "invalid durable table mutation",
            Self::StaleTableSchema => "table schema changed since the save was admitted",
            Self::ConflictingPublicationIntent => "conflicting publication intent",
            Self::ConflictingPublicationCommit => "conflicting publication commit",
            Self::InvalidPublicationCommit => "invalid publication commit",
            Self::InvalidPublicationPolicy => "invalid publication policy",
            Self::CompactPublicationRequired => {
                "compact-bound publication requires a runtime receipt"
            }
            Self::CompactReceiptKeyMismatch => {
                "runtime compact receipt key does not match initialization"
            }
            Self::InvalidCompactReceipt => "invalid compact runtime receipt",
            Self::RequestUnknown => "runtime request is unknown",
            Self::RequestFingerprintMismatch => "runtime request fingerprint mismatch",
            Self::RequestOwnerConflict => "runtime request owner cannot be recovered",
            Self::RequestStateConflict => "runtime request state conflict",
            Self::SessionClosed => "runtime session is closed",
            Self::SessionWorkActive => "runtime session still has active work",
            Self::TerminalOutcomeTooLarge => "runtime terminal outcome exceeds its bound",
            Self::RecoveryInvalid => "runtime recovery validation failed",
            Self::FaultInjected(_) => "runtime fault injected",
            Self::SystemProviderAbiInvalid => "baked system provider ABI is invalid",
            Self::StorageUnavailable => "runtime state unavailable",
            Self::LegacyHeadReadOnly(refusal) => refusal.title(),
            Self::UnknownHeadFormat(_) => "repository format is unknown",
        })
    }
}

impl std::error::Error for RuntimeError {}

impl From<libsql::Error> for RuntimeError {
    /// Every embedded-storage failure becomes `StorageUnavailable`.
    ///
    /// `RuntimeError` is deliberately redacted (`Display` must not disclose
    /// local paths, SQL or native error text) and is `Clone + Eq`, so the
    /// `libsql` cause cannot ride along. Keeping this lossy mapping in one
    /// place lets storage calls use `?` instead of repeating it per site.
    /// A site that maps a specific storage failure to a different runtime
    /// error keeps its explicit `map_err`.
    fn from(_: libsql::Error) -> Self {
        Self::StorageUnavailable
    }
}

#[derive(Debug)]
pub enum StreamTableDeliveryError {
    Runtime(RuntimeError),
    ValidationFailed(SafeDiagnostic),
}

#[derive(Debug)]
pub enum TableActivationError {
    Runtime(RuntimeError),
    ValidationFailed(SafeDiagnostic),
}
