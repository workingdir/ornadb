//! Bounded stream/checkpoint administration for Orna 1.0.
//!
//! This crate owns neither provider I/O nor table/function transaction execution.
//! A runtime owner obtains a [`CommitIntent`], atomically applies it through a
//! [`CheckpointBackend`], then performs any provider or execution work outside this
//! boundary.  A delivery can advance a checkpoint only through `Complete` or `Skip`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use orna_value::{AssertionRef, ExpressionRef, ObjectRef, SafeText, SourceSpan};

/// A non-empty, separator-free typed identity component.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Component(String);

impl Component {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.is_empty() || value.contains(['|', '\n', '\r']) {
            return Err(IdentityError::InvalidComponent);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity validation is deliberately small and stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    InvalidComponent,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid identity component")
    }
}

impl std::error::Error for IdentityError {}

/// The canonical consumer identity is composed from typed, semantic components.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConsumerIdentity {
    pub principal: Component,
    pub root: Component,
    pub function: Component,
    pub binding: Component,
}

impl ConsumerIdentity {
    pub fn canonical(&self) -> String {
        format!(
            "consumer/v1|{}|{}|{}|{}",
            self.principal.as_str(),
            self.root.as_str(),
            self.function.as_str(),
            self.binding.as_str()
        )
    }
}

/// Identifies one provider delivery and its explicit checkpoint transition.
#[derive(Clone, Debug)]
pub struct DeliveryIdentity {
    pub consumer: ConsumerIdentity,
    pub source_format: Component,
    pub source: Component,
    pub partition_format: Component,
    /// The optional provider partition. `None` is the canonical unpartitioned
    /// stream identity, not an empty partition name.
    pub partition: Option<Component>,
    pub position_format: Component,
    /// The opaque provider position that identifies this delivery.
    pub position: Position,
    /// The provider-selected checkpoint position after this delivery commits.
    pub successor: Position,
}

impl PartialEq for DeliveryIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.consumer == other.consumer
            && self.source_format == other.source_format
            && self.source == other.source
            && self.partition_format == other.partition_format
            && self.partition == other.partition
            && self.position_format == other.position_format
            && self.position == other.position
    }
}

impl Eq for DeliveryIdentity {}

impl PartialOrd for DeliveryIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeliveryIdentity {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            &self.consumer,
            &self.source_format,
            &self.source,
            &self.partition_format,
            &self.partition,
            &self.position_format,
            &self.position.token,
        )
            .cmp(&(
                &other.consumer,
                &other.source_format,
                &other.source,
                &other.partition_format,
                &other.partition,
                &other.position_format,
                &other.position.token,
            ))
    }
}

impl DeliveryIdentity {
    pub fn checkpoint_key(&self) -> CheckpointKey {
        CheckpointKey {
            consumer: self.consumer.clone(),
            source_format: self.source_format.clone(),
            source: self.source.clone(),
            partition_format: self.partition_format.clone(),
            partition: self.partition.clone(),
            position_format: self.position_format.clone(),
        }
    }

    /// Whether this delivery belongs to the ordered stream `key`, compared
    /// field by field so scanning many deliveries builds no keys.
    fn is_on_stream(&self, key: &CheckpointKey) -> bool {
        let CheckpointKey {
            consumer,
            source_format,
            source,
            partition_format,
            partition,
            position_format,
        } = key;
        self.consumer == *consumer
            && self.source_format == *source_format
            && self.source == *source
            && self.partition_format == *partition_format
            && self.partition == *partition
            && self.position_format == *position_format
    }

    pub fn canonical(&self) -> String {
        let prefix = if self.partition.is_some() {
            "delivery/v1"
        } else {
            "delivery/v1/null"
        };
        format!(
            "{prefix}|{}|{}|{}|{}|{}|{}|{}",
            self.consumer.canonical(),
            self.source_format.as_str(),
            self.source.as_str(),
            self.partition_format.as_str(),
            self.partition.as_ref().map(Component::as_str).unwrap_or(""),
            self.position_format.as_str(),
            self.position.token.as_str(),
        )
    }
}

/// An opaque provider-defined position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub token: Component,
}

/// The durable key for one ordered consumer/source/partition stream.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CheckpointKey {
    pub consumer: ConsumerIdentity,
    pub source_format: Component,
    pub source: Component,
    pub partition_format: Component,
    /// The optional partition component of the durable stream natural key.
    /// Empty components are invalid, so the empty canonical field unambiguously
    /// represents `None` while preserving pre-existing non-null encodings.
    pub partition: Option<Component>,
    pub position_format: Component,
}

/// A durable, compare-and-swap checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub key: CheckpointKey,
    pub version: u64,
    /// `None` means no item has been committed.
    pub committed: Option<Position>,
}

impl Checkpoint {
    /// The checkpoint of a stream that has never committed.
    fn unwritten(key: &CheckpointKey) -> Self {
        Self {
            key: key.clone(),
            version: 0,
            committed: None,
        }
    }
}

/// The result of merging a checkpoint position from two branches.
///
/// ORNA-CONSUMER-008 permits equal positions and a branch that retains the
/// base position to merge. Distinct opaque positions have no defined order
/// and must remain a conflict for the database merge owner to report as
/// `sys.CheckpointConflict`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointPositionMerge {
    Merged(Option<Position>),
    Conflict(CheckpointPositionConflict),
}

/// The exact positions that the database merge owner needs for a checkpoint
/// conflict record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointPositionConflict {
    pub base: Option<Position>,
    pub left: Option<Position>,
    pub right: Option<Position>,
}

/// Merge three branch positions without treating opaque provider tokens as
/// ordered values.
pub fn merge_checkpoint_position(
    base: Option<&Position>,
    left: Option<&Position>,
    right: Option<&Position>,
) -> CheckpointPositionMerge {
    if left == right {
        CheckpointPositionMerge::Merged(left.cloned())
    } else if left == base {
        CheckpointPositionMerge::Merged(right.cloned())
    } else if right == base {
        CheckpointPositionMerge::Merged(left.cloned())
    } else {
        CheckpointPositionMerge::Conflict(CheckpointPositionConflict {
            base: base.cloned(),
            left: left.cloned(),
            right: right.cloned(),
        })
    }
}

/// Durable admission state for one ordered stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamStatus {
    Running,
    Paused,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamState {
    pub key: CheckpointKey,
    pub status: StreamStatus,
}

/// The expected checkpoint version and position for a write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointPrecondition {
    pub version: u64,
    pub committed: Option<Position>,
}

impl From<&Checkpoint> for CheckpointPrecondition {
    fn from(value: &Checkpoint) -> Self {
        Self {
            version: value.version,
            committed: value.committed.clone(),
        }
    }
}

/// A stable failure row identity: exactly one row exists for one delivery.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FailureIdentity(pub DeliveryIdentity);

/// Only structured, secret-free diagnostic facts cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeDiagnostic {
    pub code: DiagnosticCode,
    pub class: DiagnosticClass,
}

/// The required code for a false table or cross-table assertion.
pub const ORNA_A091_009: &str = "ORNA-A091-009";

/// The catalogue owner category recorded for an assertion diagnostic.
///
/// These are the exact `sys.AssertionOwnerKind` variants. The stream crate
/// does not resolve or authorize the references; that remains the catalogue
/// and runtime owner's responsibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssertionOwnerKind {
    Executable,
    RefinedType,
    Table,
    Module,
}

/// A stable assertion-specific diagnostic code.
///
/// This is deliberately separate from [`DiagnosticCode`]. Adding a variant
/// to that pre-existing public enum would require every existing stream
/// backend and durable decoder to change together. The next runtime step can
/// persist this structured detail without changing ordinary diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssertionDiagnosticCode {
    /// `ORNA-A091-009`: a table or cross-table assertion evaluated false.
    TableOrCrossTableFalse,
}

impl AssertionDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TableOrCrossTableFalse => ORNA_A091_009,
        }
    }
}

/// A witness admitted to an assertion diagnostic.
///
/// The wrapper accepts only the foundation's existing diagnostic-safe text
/// type, never a source row or raw value. Producers must use a redacted value
/// when a deterministic witness is not presentable.
#[derive(Clone, Eq, PartialEq)]
pub struct SafeAssertionWitness(SafeText);

impl SafeAssertionWitness {
    pub fn from_safe_presentation(value: SafeText) -> Self {
        Self(value)
    }

    pub fn as_safe_presentation(&self) -> &SafeText {
        &self.0
    }
}

impl fmt::Debug for SafeAssertionWitness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SafeAssertionWitness")
            .field("redacted", &true)
            .finish()
    }
}

/// Structured context for an assertion failure.
///
/// It enters the failure path through [`CommitIntent::FailWithAssertion`] and
/// [`CommitIntent::ReplayFailWithAssertion`] and is retained on
/// [`FailureRecord::assertion_detail`].
///
/// `assertion` and `predicate` are snapshot-pinned `sys.AssertionRef` and
/// `sys.ExpressionRef` coordinates respectively. These foundation aliases
/// are compile-time markers, not authority: runtime and catalogue owners must
/// still verify relation identity, row existence, provenance, authorization,
/// and pinned-snapshot consistency before persistence or projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssertionDiagnosticDetail {
    pub code: AssertionDiagnosticCode,
    pub assertion: AssertionRef,
    pub owner: ObjectRef,
    pub owner_kind: AssertionOwnerKind,
    pub predicate: ExpressionRef,
    /// The authoritative, snapshot-pinned `sys.SourceSpan` for the assertion.
    ///
    /// This retains the typed `sys.FileRef` and one-based coordinates required
    /// by the system diagnostic contract. It remains diagnostic context only;
    /// it does not by itself establish catalogue or runtime authority.
    pub source_span: SourceSpan,
    /// Present only when the producer has a deterministic, safe presentation.
    pub witness: Option<SafeAssertionWitness>,
}

impl AssertionDiagnosticDetail {
    pub fn table_or_cross_table_false(
        assertion: AssertionRef,
        owner: ObjectRef,
        owner_kind: AssertionOwnerKind,
        predicate: ExpressionRef,
        source_span: SourceSpan,
        witness: Option<SafeAssertionWitness>,
    ) -> Self {
        Self {
            code: AssertionDiagnosticCode::TableOrCrossTableFalse,
            assertion,
            owner,
            owner_kind,
            predicate,
            source_span,
            witness,
        }
    }
}

/// Connector-selected retention for a failed delivery.
#[derive(Clone, Eq, PartialEq)]
pub enum StreamFailurePayload {
    /// The connector has not declared a safe retention mechanism.
    Unavailable,
    /// Retain source bytes behind the runtime's redacted failure boundary.
    Plaintext(Vec<u8>),
    /// Retain only a protected connector reference and its validation digest.
    ProtectedReference { reference: String, digest: [u8; 32] },
}

impl fmt::Debug for StreamFailurePayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Unavailable"),
            Self::Plaintext(bytes) => f
                .debug_struct("Plaintext")
                .field("bytes", &bytes.len())
                .field("redacted", &true)
                .finish(),
            Self::ProtectedReference { .. } => f
                .debug_struct("ProtectedReference")
                .field("redacted", &true)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    ProviderUnavailable,
    DecodeRejected,
    ExecutionRejected,
    Cancelled,
    Internal,
    /// A table or cross-table assertion evaluated false. This is the primary
    /// safe diagnostic code; assertion provenance is carried separately.
    TableAssertionFalse,
    /// The captured write/temp-create baseline is no longer the accepted row,
    /// so the save is refused as a stale edit rather than a failed validation
    /// (ORNA-VFS-011). The caller's retained draft is untouched.
    StaleBaseline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticClass {
    Transient,
    Permanent,
    Cancellation,
}

/// Failure state is retained until an administrator explicitly resolves it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureRecord {
    pub identity: FailureIdentity,
    pub version: u64,
    pub attempts: u32,
    pub status: FailureStatus,
    pub diagnostic: SafeDiagnostic,
    /// Optional assertion-specific context carried alongside the stable
    /// generic diagnostic. Ordinary callers continue to use
    /// [`SafeDiagnostic`] and receive `None` here.
    pub assertion_detail: Option<AssertionDiagnosticDetail>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureStatus {
    Failed,
    Retrying,
    Succeeded,
    Skipped,
    Replaying,
    Replayed,
    Resolved,
}

/// A fenced lease. Its number is never reused for a checkpoint key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryLease {
    pub delivery: DeliveryIdentity,
    pub fence: u64,
    pub purpose: LeasePurpose,
}

/// The only work a lease may authorise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeasePurpose {
    Deliver,
    Skip,
}

/// A replay grant authorises re-reading a failed item but cannot move a live checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayGrant {
    pub failure: FailureIdentity,
    pub version: u64,
}

/// Atomic administration mutations. They do not initiate provider I/O or execute tables/functions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitIntent {
    Pause {
        key: CheckpointKey,
    },
    Resume {
        key: CheckpointKey,
    },
    Reset {
        key: CheckpointKey,
        expected: CheckpointPrecondition,
        to: Position,
    },
    Acquire {
        delivery: DeliveryIdentity,
        expected: CheckpointPrecondition,
        purpose: LeasePurpose,
    },
    Fail {
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
    },
    /// Fails a delivery while retaining safe, assertion-specific context.
    /// This additive variant keeps existing `Fail` callers source-compatible.
    FailWithAssertion {
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        detail: AssertionDiagnosticDetail,
    },
    Retry {
        failure: FailureIdentity,
        expected_version: u64,
        expected: CheckpointPrecondition,
    },
    Complete {
        lease: DeliveryLease,
        expected: CheckpointPrecondition,
    },
    Skip {
        lease: DeliveryLease,
        expected: CheckpointPrecondition,
        expected_failure_version: u64,
    },
    Replay {
        failure: FailureIdentity,
        expected_version: u64,
    },
    ReplayComplete {
        failure: FailureIdentity,
        expected_version: u64,
    },
    ReplayFail {
        failure: FailureIdentity,
        expected_version: u64,
        diagnostic: SafeDiagnostic,
    },
    /// Fails an admitted replay while retaining safe assertion context.
    ReplayFailWithAssertion {
        failure: FailureIdentity,
        expected_version: u64,
        diagnostic: SafeDiagnostic,
        detail: AssertionDiagnosticDetail,
    },
    /// Cancels an admitted replay without changing the live checkpoint.
    ReplayCancel {
        failure: FailureIdentity,
        expected_version: u64,
    },
    Resolve {
        failure: FailureIdentity,
        expected_version: u64,
    },
    Cancel {
        lease: DeliveryLease,
    },
}

/// Results expose what an owner may safely commit around this administrative transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommitResult {
    StreamStatusChanged {
        state: StreamState,
        changed: bool,
    },
    /// New delivery admission is stopped, but an already-admitted delivery
    /// still owns the current item/batch boundary. `state` deliberately
    /// remains `Running` until that lease reaches a terminal transition.
    PausePending {
        state: StreamState,
        changed: bool,
    },
    CheckpointReset {
        checkpoint: Checkpoint,
    },
    Acquired {
        lease: DeliveryLease,
    },
    Failed {
        failure: FailureRecord,
    },
    RetryScheduled {
        failure: FailureRecord,
    },
    CheckpointAdvanced {
        checkpoint: Checkpoint,
    },
    ReplayGranted {
        grant: ReplayGrant,
    },
    ReplayCompleted {
        failure: FailureRecord,
    },
    ReplayFailed {
        failure: FailureRecord,
    },
    ReplayCancelled {
        failure: FailureRecord,
    },
    Resolved {
        failure: FailureRecord,
    },
    Cancelled {
        checkpoint: Checkpoint,
        classification: CancellationClassification,
    },
    Rejected(RejectReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    StreamPaused,
    StreamNotPaused,
    StreamBusy,
    BlockingFailure,
    StaleCheckpoint,
    StaleFailure,
    LeaseFenced,
    LeaseAlreadyHeld,
    FailureMissing,
    RetryNotAllowed,
    ResolveBlocked,
}

/// Cancellation before an administrative commit is explicitly rollback-shaped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationClassification {
    RollbackShaped,
}

/// Persistence seam. An implementation must atomically apply each intent.
pub trait CheckpointBackend {
    fn apply(&mut self, intent: CommitIntent) -> CommitResult;
    fn checkpoint(&self, key: &CheckpointKey) -> Checkpoint;
    fn failure(&self, identity: &FailureIdentity) -> Option<&FailureRecord>;
}

/// Asynchronous durability seam for a runtime-owned checkpoint store.
///
/// The async backend has the same atomic intent boundary as
/// [`CheckpointBackend`]. Storage failures are distinct from deterministic
/// [`RejectReason`] outcomes, and checkpoint/failure reads return owned
/// snapshots so an implementation may hold a database borrow across an await
/// without exposing storage lifetimes to stream administration.
pub trait AsyncCheckpointBackend {
    type Error;
    type ApplyFuture<'a>: Future<Output = Result<CommitResult, Self::Error>> + 'a
    where
        Self: 'a;
    type CheckpointFuture<'a>: Future<Output = Result<Checkpoint, Self::Error>> + 'a
    where
        Self: 'a;
    type FailureFuture<'a>: Future<Output = Result<Option<FailureRecord>, Self::Error>> + 'a
    where
        Self: 'a;

    /// Atomically apply one complete stream intent.
    fn apply_async<'a>(&'a mut self, intent: CommitIntent) -> Self::ApplyFuture<'a>;

    /// Read the current owned checkpoint snapshot for an ordered stream key.
    fn checkpoint_async<'a>(&'a self, key: &'a CheckpointKey) -> Self::CheckpointFuture<'a>;

    /// Read the current owned failure snapshot, if one exists.
    fn failure_async<'a>(&'a self, identity: &'a FailureIdentity) -> Self::FailureFuture<'a>;
}

/// Additive async seam for connector-selected failed-delivery retention.
///
/// The original checkpoint intent enum remains source-compatible; durable
/// runtimes implement this companion seam when a failure carries retained
/// plaintext or a protected refetch reference.
pub trait AsyncFailurePayloadBackend {
    type Error;

    fn fail_with_payload_async<'a>(
        &'a mut self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        payload: StreamFailurePayload,
    ) -> Pin<Box<dyn Future<Output = Result<CommitResult, Self::Error>> + 'a>>;

    /// Additive assertion-aware failure seam. Implementations must either
    /// persist the detail or return an explicit unsupported error; silently
    /// degrading to a generic failure is not permitted.
    fn fail_with_assertion_detail_and_payload_async<'a>(
        &'a mut self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        detail: AssertionDiagnosticDetail,
        payload: StreamFailurePayload,
    ) -> Pin<Box<dyn Future<Output = Result<CommitResult, Self::Error>> + 'a>>;
}

/// Refuses an intent with `reason` when `condition` holds. Intent handlers use
/// it to read as the ordered list of guards they apply before mutating state.
fn reject_if(condition: bool, reason: RejectReason) -> Result<(), RejectReason> {
    if condition { Err(reason) } else { Ok(()) }
}

/// The diagnostic stored for an assertion failure: the primary code is fixed
/// and only the class is taken from the caller.
fn assertion_diagnostic(supplied: SafeDiagnostic) -> SafeDiagnostic {
    SafeDiagnostic {
        code: DiagnosticCode::TableAssertionFalse,
        class: supplied.class,
    }
}

impl FailureStatus {
    /// A row in this state holds back its stream's ordered delivery.
    const fn blocks_stream(self) -> bool {
        matches!(self, Self::Failed | Self::Retrying)
    }

    /// A settled row an administrator may archive with `Resolve`.
    const fn is_resolvable(self) -> bool {
        matches!(self, Self::Succeeded | Self::Skipped | Self::Replayed)
    }
}

/// A delivery lease and the requests that can only exist while it is held.
struct HeldLease {
    lease: DeliveryLease,
    /// A pause arrived during the lease (`PausePending`). It takes effect when
    /// the lease reaches a terminal transition.
    pause_requested: bool,
    /// `Retry` issued this lease, so the provider's retried delivery adopts it
    /// through the next `Acquire` instead of being refused as a second lease.
    awaiting_retry_acquire: bool,
}

/// What one ordered stream owns besides its checkpoint: admission status, the
/// single lease it may hold and its fence counter. They change together, so
/// they share one value.
struct StreamSlot {
    status: StreamStatus,
    lease: Option<HeldLease>,
    /// The highest fence issued for this stream. A fence is never reused.
    last_fence: u64,
}

impl Default for StreamSlot {
    fn default() -> Self {
        Self {
            status: StreamStatus::Running,
            lease: None,
            last_fence: 0,
        }
    }
}

impl StreamSlot {
    fn issue_lease(
        &mut self,
        delivery: DeliveryIdentity,
        purpose: LeasePurpose,
        awaiting_retry_acquire: bool,
    ) -> DeliveryLease {
        self.last_fence += 1;
        let lease = DeliveryLease {
            delivery,
            fence: self.last_fence,
            purpose,
        };
        self.lease = Some(HeldLease {
            lease: lease.clone(),
            pause_requested: false,
            awaiting_retry_acquire,
        });
        lease
    }
}

/// Checkpoints and per-stream state, keyed by the stream's checkpoint key.
#[derive(Default)]
struct StreamTable {
    checkpoints: BTreeMap<CheckpointKey, Checkpoint>,
    slots: BTreeMap<CheckpointKey, StreamSlot>,
}

impl StreamTable {
    fn checkpoint(&self, key: &CheckpointKey) -> Checkpoint {
        self.checkpoints
            .get(key)
            .cloned()
            .unwrap_or_else(|| Checkpoint::unwritten(key))
    }

    fn checkpoint_mut(&mut self, key: &CheckpointKey) -> &mut Checkpoint {
        self.checkpoints
            .entry(key.clone())
            .or_insert_with(|| Checkpoint::unwritten(key))
    }

    /// A stream that was never written holds the version-0, no-position
    /// checkpoint.
    fn checkpoint_matches(&self, key: &CheckpointKey, expected: &CheckpointPrecondition) -> bool {
        self.checkpoints.get(key).map_or_else(
            || expected.version == 0 && expected.committed.is_none(),
            |checkpoint| {
                checkpoint.version == expected.version && checkpoint.committed == expected.committed
            },
        )
    }

    fn slot_mut(&mut self, key: &CheckpointKey) -> &mut StreamSlot {
        self.slots.entry(key.clone()).or_default()
    }

    /// Ends the stream's lease when `lease` is exactly the one held, including
    /// the provider-selected successor, and applies a pause that waited for it.
    fn consume_lease(&mut self, lease: &DeliveryLease) -> Result<(), RejectReason> {
        let slot = self
            .slots
            .get_mut(&lease.delivery.checkpoint_key())
            .ok_or(RejectReason::LeaseFenced)?;
        let held = slot
            .lease
            .as_ref()
            .filter(|held| {
                held.lease == *lease && held.lease.delivery.successor == lease.delivery.successor
            })
            .ok_or(RejectReason::LeaseFenced)?;
        if held.pause_requested {
            slot.status = StreamStatus::Paused;
        }
        slot.lease = None;
        Ok(())
    }

    /// Moves the checkpoint to the delivery's successor. The caller has already
    /// verified the compare-and-set precondition.
    fn advance(&mut self, delivery: &DeliveryIdentity) -> Checkpoint {
        let checkpoint = self.checkpoint_mut(&delivery.checkpoint_key());
        checkpoint.committed = Some(delivery.successor.clone());
        checkpoint.version += 1;
        checkpoint.clone()
    }
}

/// A stored failure row plus the in-memory-only replay claim.
struct FailureEntry {
    record: FailureRecord,
    /// An admitted replay is claimed before provider or handler execution
    /// starts. Only a `Replaying` row can hold a claim.
    replay_claimed: bool,
}

#[derive(Default)]
struct FailureTable {
    rows: BTreeMap<FailureIdentity, FailureEntry>,
}

impl FailureTable {
    fn get(&self, identity: &FailureIdentity) -> Option<&FailureEntry> {
        self.rows.get(identity)
    }

    /// The row at `identity`, provided it is still at `expected_version`.
    fn at_version(
        &mut self,
        identity: &FailureIdentity,
        expected_version: u64,
    ) -> Result<&mut FailureEntry, RejectReason> {
        let entry = self
            .rows
            .get_mut(identity)
            .ok_or(RejectReason::FailureMissing)?;
        reject_if(
            entry.record.version != expected_version,
            RejectReason::StaleFailure,
        )?;
        Ok(entry)
    }

    fn has_blocking(&self, key: &CheckpointKey) -> bool {
        self.rows.values().any(|entry| {
            entry.record.status.blocks_stream() && entry.record.identity.0.is_on_stream(key)
        })
    }
}

/// Deterministic in-memory backend for runtime integration and focused tests.
///
/// Failure rows move through one lifecycle, and every transition bumps the
/// row's `version`:
///
/// ```text
/// Failed --Retry--> Retrying --Complete--> Succeeded
///   ^                  |
///   +-----Fail/Cancel--+
/// Failed --Skip--> Skipped --Replay--> Replaying --ReplayComplete--> Replayed
///                     ^                    |
///                     +-ReplayFail/Cancel--+
/// Succeeded | Skipped | Replayed --Resolve--> Resolved
/// ```
///
/// `Fail` on a delivery with no row creates the `Failed` row, and a
/// `Deliver` lease over a `Succeeded` or `Resolved` row admits a redelivery.
#[derive(Default)]
pub struct InMemoryCheckpointBackend {
    streams: StreamTable,
    failures: FailureTable,
}

impl InMemoryCheckpointBackend {
    /// Claims an admitted replay before provider or handler execution begins.
    ///
    /// Once claimed, administrative cancellation is fenced until the replay
    /// reaches a terminal completion or failure transition.
    pub fn claim_replay(&mut self, grant: &ReplayGrant) -> Result<(), RejectReason> {
        let entry = self.failures.at_version(&grant.failure, grant.version)?;
        reject_if(
            entry.record.status != FailureStatus::Replaying,
            RejectReason::RetryNotAllowed,
        )?;
        reject_if(entry.replay_claimed, RejectReason::LeaseAlreadyHeld)?;
        entry.replay_claimed = true;
        Ok(())
    }

    fn pause(&mut self, key: CheckpointKey) -> CommitResult {
        let slot = self.streams.slot_mut(&key);
        if slot.status == StreamStatus::Paused {
            return CommitResult::StreamStatusChanged {
                state: StreamState {
                    key,
                    status: StreamStatus::Paused,
                },
                changed: false,
            };
        }
        if let Some(held) = slot.lease.as_mut() {
            // An admitted delivery still owns the item boundary, so the stream
            // stays `Running` until that lease ends.
            let changed = !std::mem::replace(&mut held.pause_requested, true);
            return CommitResult::PausePending {
                state: StreamState {
                    key,
                    status: slot.status,
                },
                changed,
            };
        }
        slot.status = StreamStatus::Paused;
        CommitResult::StreamStatusChanged {
            state: StreamState {
                key,
                status: StreamStatus::Paused,
            },
            changed: true,
        }
    }

    fn resume(&mut self, key: CheckpointKey) -> Result<CommitResult, RejectReason> {
        let slot = self.streams.slot_mut(&key);
        if slot.status == StreamStatus::Running {
            return Ok(CommitResult::StreamStatusChanged {
                state: StreamState {
                    key,
                    status: StreamStatus::Running,
                },
                changed: false,
            });
        }
        reject_if(slot.lease.is_some(), RejectReason::StreamBusy)?;
        reject_if(
            self.failures.has_blocking(&key),
            RejectReason::BlockingFailure,
        )?;
        slot.status = StreamStatus::Running;
        Ok(CommitResult::StreamStatusChanged {
            state: StreamState {
                key,
                status: StreamStatus::Running,
            },
            changed: true,
        })
    }

    fn reset(
        &mut self,
        key: CheckpointKey,
        expected: &CheckpointPrecondition,
        to: Position,
    ) -> Result<CommitResult, RejectReason> {
        let slot = self.streams.slot_mut(&key);
        reject_if(
            slot.status != StreamStatus::Paused,
            RejectReason::StreamNotPaused,
        )?;
        reject_if(slot.lease.is_some(), RejectReason::StreamBusy)?;
        reject_if(
            self.failures.has_blocking(&key),
            RejectReason::BlockingFailure,
        )?;
        reject_if(
            !self.streams.checkpoint_matches(&key, expected),
            RejectReason::StaleCheckpoint,
        )?;
        let checkpoint = self.streams.checkpoint_mut(&key);
        checkpoint.committed = Some(to);
        checkpoint.version += 1;
        Ok(CommitResult::CheckpointReset {
            checkpoint: checkpoint.clone(),
        })
    }

    fn acquire(
        &mut self,
        delivery: DeliveryIdentity,
        expected: &CheckpointPrecondition,
        purpose: LeasePurpose,
    ) -> Result<CommitResult, RejectReason> {
        let key = delivery.checkpoint_key();
        reject_if(
            !self.streams.checkpoint_matches(&key, expected),
            RejectReason::StaleCheckpoint,
        )?;
        let identity = FailureIdentity(delivery);
        let failure_status = self
            .failures
            .get(&identity)
            .map(|entry| entry.record.status);
        // A blocking failure admits only the delivery that works it off: the
        // retry of a `Retrying` row or the skip of a `Failed` one.
        let works_off_blocking_failure = matches!(
            (purpose, failure_status),
            (LeasePurpose::Deliver, Some(FailureStatus::Retrying))
                | (LeasePurpose::Skip, Some(FailureStatus::Failed))
        );
        reject_if(
            self.failures.has_blocking(&key) && !works_off_blocking_failure,
            RejectReason::BlockingFailure,
        )?;
        let FailureIdentity(delivery) = identity;
        let slot = self.streams.slot_mut(&key);
        if let Some(held) = slot.lease.as_mut() {
            // `Retry` already issued this lease; the provider's retried
            // delivery adopts it, keeping the fence and possibly selecting a
            // different successor.
            if purpose == LeasePurpose::Deliver
                && failure_status == Some(FailureStatus::Retrying)
                && std::mem::take(&mut held.awaiting_retry_acquire)
            {
                held.lease = DeliveryLease {
                    delivery,
                    fence: held.lease.fence,
                    purpose: held.lease.purpose,
                };
                return Ok(CommitResult::Acquired {
                    lease: held.lease.clone(),
                });
            }
            return Err(RejectReason::LeaseAlreadyHeld);
        }
        match failure_status {
            Some(status) => reject_if(
                !matches!(
                    (purpose, status),
                    (LeasePurpose::Deliver, FailureStatus::Retrying)
                        | (
                            LeasePurpose::Deliver,
                            FailureStatus::Succeeded | FailureStatus::Resolved
                        )
                        | (LeasePurpose::Skip, FailureStatus::Failed)
                ),
                RejectReason::RetryNotAllowed,
            )?,
            None => reject_if(purpose == LeasePurpose::Skip, RejectReason::FailureMissing)?,
        }
        reject_if(
            purpose == LeasePurpose::Deliver
                && slot.status == StreamStatus::Paused
                && failure_status != Some(FailureStatus::Retrying),
            RejectReason::StreamPaused,
        )?;
        Ok(CommitResult::Acquired {
            lease: slot.issue_lease(delivery, purpose, false),
        })
    }

    /// Records a failed delivery. `detail` is `Some` only for an assertion
    /// failure, whose `diagnostic` has already been normalised.
    fn fail(
        &mut self,
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
        detail: Option<AssertionDiagnosticDetail>,
    ) -> Result<CommitResult, RejectReason> {
        reject_if(
            lease.purpose != LeasePurpose::Deliver,
            RejectReason::LeaseFenced,
        )?;
        self.streams.consume_lease(&lease)?;
        let identity = FailureIdentity(lease.delivery);
        let entry = self
            .failures
            .rows
            .entry(identity.clone())
            .or_insert_with(|| FailureEntry {
                record: FailureRecord {
                    identity: identity.clone(),
                    version: 0,
                    attempts: 0,
                    status: FailureStatus::Failed,
                    diagnostic,
                    assertion_detail: None,
                },
                replay_claimed: false,
            });
        let record = &mut entry.record;
        // A provider may select a different successor when the same opaque
        // delivery is retried. Keep the natural identity stable while
        // retaining the newest successor for a later explicit skip.
        record.identity = identity;
        record.version += 1;
        record.attempts += u32::from(record.status != FailureStatus::Retrying);
        record.status = FailureStatus::Failed;
        record.diagnostic = diagnostic;
        record.assertion_detail = detail;
        Ok(CommitResult::Failed {
            failure: record.clone(),
        })
    }

    fn retry(
        &mut self,
        failure: FailureIdentity,
        expected_version: u64,
        expected: &CheckpointPrecondition,
    ) -> Result<CommitResult, RejectReason> {
        let key = failure.0.checkpoint_key();
        reject_if(
            !self.streams.checkpoint_matches(&key, expected),
            RejectReason::StaleCheckpoint,
        )?;
        let entry = self
            .failures
            .rows
            .get_mut(&failure)
            .ok_or(RejectReason::FailureMissing)?;
        reject_if(
            entry.record.version != expected_version
                || entry.record.identity.0.successor != failure.0.successor,
            RejectReason::StaleFailure,
        )?;
        reject_if(
            entry.record.status != FailureStatus::Failed,
            RejectReason::RetryNotAllowed,
        )?;
        let slot = self.streams.slot_mut(&key);
        reject_if(slot.lease.is_some(), RejectReason::LeaseAlreadyHeld)?;
        slot.issue_lease(failure.0, LeasePurpose::Deliver, true);
        entry.record.version += 1;
        entry.record.attempts += 1;
        entry.record.status = FailureStatus::Retrying;
        Ok(CommitResult::RetryScheduled {
            failure: entry.record.clone(),
        })
    }

    fn complete(
        &mut self,
        lease: DeliveryLease,
        expected: &CheckpointPrecondition,
    ) -> Result<CommitResult, RejectReason> {
        reject_if(
            lease.purpose != LeasePurpose::Deliver,
            RejectReason::LeaseFenced,
        )?;
        reject_if(
            !self
                .streams
                .checkpoint_matches(&lease.delivery.checkpoint_key(), expected),
            RejectReason::StaleCheckpoint,
        )?;
        self.streams.consume_lease(&lease)?;
        let checkpoint = self.streams.advance(&lease.delivery);
        if let Some(entry) = self.failures.rows.get_mut(&FailureIdentity(lease.delivery)) {
            entry.record.version += 1;
            entry.record.status = FailureStatus::Succeeded;
        }
        Ok(CommitResult::CheckpointAdvanced { checkpoint })
    }

    fn skip(
        &mut self,
        lease: DeliveryLease,
        expected: &CheckpointPrecondition,
        expected_failure_version: u64,
    ) -> Result<CommitResult, RejectReason> {
        reject_if(
            lease.purpose != LeasePurpose::Skip,
            RejectReason::LeaseFenced,
        )?;
        let identity = FailureIdentity(lease.delivery.clone());
        let entry = self
            .failures
            .rows
            .get_mut(&identity)
            .ok_or(RejectReason::FailureMissing)?;
        reject_if(
            entry.record.version != expected_failure_version
                || entry.record.status != FailureStatus::Failed
                || entry.record.identity.0.successor != lease.delivery.successor,
            RejectReason::StaleFailure,
        )?;
        reject_if(
            !self
                .streams
                .checkpoint_matches(&lease.delivery.checkpoint_key(), expected),
            RejectReason::StaleCheckpoint,
        )?;
        self.streams.consume_lease(&lease)?;
        let checkpoint = self.streams.advance(&lease.delivery);
        entry.record.version += 1;
        entry.record.status = FailureStatus::Skipped;
        Ok(CommitResult::CheckpointAdvanced { checkpoint })
    }

    fn replay(
        &mut self,
        failure: FailureIdentity,
        expected_version: u64,
    ) -> Result<CommitResult, RejectReason> {
        let entry = self.failures.at_version(&failure, expected_version)?;
        reject_if(
            entry.record.status != FailureStatus::Skipped,
            RejectReason::RetryNotAllowed,
        )?;
        entry.record.version += 1;
        entry.record.attempts += 1;
        entry.record.status = FailureStatus::Replaying;
        Ok(CommitResult::ReplayGranted {
            grant: ReplayGrant {
                failure,
                version: entry.record.version,
            },
        })
    }

    /// Releases the claim of an admitted replay and bumps its row. Only the
    /// claim holder may complete or fail a replay.
    fn end_claimed_replay(
        &mut self,
        failure: &FailureIdentity,
        expected_version: u64,
    ) -> Result<&mut FailureRecord, RejectReason> {
        let entry = self.failures.at_version(failure, expected_version)?;
        reject_if(
            entry.record.status != FailureStatus::Replaying,
            RejectReason::RetryNotAllowed,
        )?;
        reject_if(!entry.replay_claimed, RejectReason::LeaseFenced)?;
        entry.replay_claimed = false;
        entry.record.version += 1;
        Ok(&mut entry.record)
    }

    fn complete_replay(
        &mut self,
        failure: FailureIdentity,
        expected_version: u64,
    ) -> Result<CommitResult, RejectReason> {
        let record = self.end_claimed_replay(&failure, expected_version)?;
        record.status = FailureStatus::Replayed;
        Ok(CommitResult::ReplayCompleted {
            failure: record.clone(),
        })
    }

    /// A failed replay returns the row to `Skipped`; `detail` is `Some` only
    /// for an assertion failure, whose `diagnostic` is already normalised.
    fn fail_replay(
        &mut self,
        failure: FailureIdentity,
        expected_version: u64,
        diagnostic: SafeDiagnostic,
        detail: Option<AssertionDiagnosticDetail>,
    ) -> Result<CommitResult, RejectReason> {
        let record = self.end_claimed_replay(&failure, expected_version)?;
        record.status = FailureStatus::Skipped;
        record.diagnostic = diagnostic;
        record.assertion_detail = detail;
        Ok(CommitResult::ReplayFailed {
            failure: record.clone(),
        })
    }

    /// Withdraws an admitted replay that has not been claimed.
    fn cancel_replay(
        &mut self,
        failure: FailureIdentity,
        expected_version: u64,
    ) -> Result<CommitResult, RejectReason> {
        let entry = self.failures.at_version(&failure, expected_version)?;
        reject_if(
            entry.record.status != FailureStatus::Replaying,
            RejectReason::RetryNotAllowed,
        )?;
        reject_if(entry.replay_claimed, RejectReason::LeaseAlreadyHeld)?;
        entry.record.version += 1;
        entry.record.status = FailureStatus::Skipped;
        Ok(CommitResult::ReplayCancelled {
            failure: entry.record.clone(),
        })
    }

    fn resolve(
        &mut self,
        failure: FailureIdentity,
        expected_version: u64,
    ) -> Result<CommitResult, RejectReason> {
        let entry = self.failures.at_version(&failure, expected_version)?;
        reject_if(
            !entry.record.status.is_resolvable(),
            RejectReason::ResolveBlocked,
        )?;
        entry.record.version += 1;
        entry.record.status = FailureStatus::Resolved;
        Ok(CommitResult::Resolved {
            failure: entry.record.clone(),
        })
    }

    fn cancel(&mut self, lease: DeliveryLease) -> Result<CommitResult, RejectReason> {
        self.streams.consume_lease(&lease)?;
        let key = lease.delivery.checkpoint_key();
        if let Some(entry) = self.failures.rows.get_mut(&FailureIdentity(lease.delivery))
            && entry.record.status == FailureStatus::Retrying
        {
            entry.record.version += 1;
            entry.record.status = FailureStatus::Failed;
        }
        Ok(CommitResult::Cancelled {
            checkpoint: self.streams.checkpoint(&key),
            classification: CancellationClassification::RollbackShaped,
        })
    }
}

impl CheckpointBackend for InMemoryCheckpointBackend {
    fn apply(&mut self, intent: CommitIntent) -> CommitResult {
        let outcome = match intent {
            CommitIntent::Pause { key } => Ok(self.pause(key)),
            CommitIntent::Resume { key } => self.resume(key),
            CommitIntent::Reset { key, expected, to } => self.reset(key, &expected, to),
            CommitIntent::Acquire {
                delivery,
                expected,
                purpose,
            } => self.acquire(delivery, &expected, purpose),
            CommitIntent::Fail { lease, diagnostic } => self.fail(lease, diagnostic, None),
            CommitIntent::FailWithAssertion {
                lease,
                diagnostic,
                detail,
            } => self.fail(lease, assertion_diagnostic(diagnostic), Some(detail)),
            CommitIntent::Retry {
                failure,
                expected_version,
                expected,
            } => self.retry(failure, expected_version, &expected),
            CommitIntent::Complete { lease, expected } => self.complete(lease, &expected),
            CommitIntent::Skip {
                lease,
                expected,
                expected_failure_version,
            } => self.skip(lease, &expected, expected_failure_version),
            CommitIntent::Replay {
                failure,
                expected_version,
            } => self.replay(failure, expected_version),
            CommitIntent::ReplayComplete {
                failure,
                expected_version,
            } => self.complete_replay(failure, expected_version),
            CommitIntent::ReplayFail {
                failure,
                expected_version,
                diagnostic,
            } => self.fail_replay(failure, expected_version, diagnostic, None),
            CommitIntent::ReplayFailWithAssertion {
                failure,
                expected_version,
                diagnostic,
                detail,
            } => self.fail_replay(
                failure,
                expected_version,
                assertion_diagnostic(diagnostic),
                Some(detail),
            ),
            CommitIntent::ReplayCancel {
                failure,
                expected_version,
            } => self.cancel_replay(failure, expected_version),
            CommitIntent::Resolve {
                failure,
                expected_version,
            } => self.resolve(failure, expected_version),
            CommitIntent::Cancel { lease } => self.cancel(lease),
        };
        outcome.unwrap_or_else(CommitResult::Rejected)
    }

    fn checkpoint(&self, key: &CheckpointKey) -> Checkpoint {
        self.streams.checkpoint(key)
    }

    fn failure(&self, identity: &FailureIdentity) -> Option<&FailureRecord> {
        self.failures.get(identity).map(|entry| &entry.record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_value::FileRef;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    fn component(value: &str) -> Component {
        Component::new(value).unwrap()
    }
    fn position(token: &str) -> Position {
        Position {
            token: component(token),
        }
    }

    /// ORNA-CONSUMER-008 / ORNA-CP-004: opaque positions merge by equality, never inferred order.
    #[test]
    fn checkpoint_merge_adopts_equal_or_unchanged_positions_and_preserves_divergence() {
        let base = position("base-token");
        let left = position("zeta-token");
        let right = position("alpha-token");

        assert_eq!(
            merge_checkpoint_position(Some(&base), Some(&left), Some(&left)),
            CheckpointPositionMerge::Merged(Some(left.clone()))
        );
        assert_eq!(
            merge_checkpoint_position(Some(&base), Some(&base), Some(&right)),
            CheckpointPositionMerge::Merged(Some(right.clone()))
        );
        assert_eq!(
            merge_checkpoint_position(Some(&base), Some(&left), Some(&base)),
            CheckpointPositionMerge::Merged(Some(left.clone()))
        );
        assert_eq!(
            merge_checkpoint_position(None, Some(&left), Some(&right)),
            CheckpointPositionMerge::Conflict(CheckpointPositionConflict {
                base: None,
                left: Some(left.clone()),
                right: Some(right.clone()),
            })
        );
        assert_eq!(
            merge_checkpoint_position(Some(&base), Some(&left), Some(&right)),
            CheckpointPositionMerge::Conflict(CheckpointPositionConflict {
                base: Some(base),
                left: Some(left),
                right: Some(right),
            })
        );
        assert_eq!(
            merge_checkpoint_position(None, None, Some(&position("first-token"))),
            CheckpointPositionMerge::Merged(Some(position("first-token")))
        );
    }

    struct ReadyAsyncBackend {
        inner: InMemoryCheckpointBackend,
    }

    impl AsyncCheckpointBackend for ReadyAsyncBackend {
        type Error = ();
        type ApplyFuture<'a>
            = std::future::Ready<Result<CommitResult, ()>>
        where
            Self: 'a;
        type CheckpointFuture<'a>
            = std::future::Ready<Result<Checkpoint, ()>>
        where
            Self: 'a;
        type FailureFuture<'a>
            = std::future::Ready<Result<Option<FailureRecord>, ()>>
        where
            Self: 'a;

        fn apply_async<'a>(&'a mut self, intent: CommitIntent) -> Self::ApplyFuture<'a> {
            std::future::ready(Ok(self.inner.apply(intent)))
        }

        fn checkpoint_async<'a>(&'a self, key: &'a CheckpointKey) -> Self::CheckpointFuture<'a> {
            std::future::ready(Ok(self.inner.checkpoint(key)))
        }

        fn failure_async<'a>(&'a self, identity: &'a FailureIdentity) -> Self::FailureFuture<'a> {
            std::future::ready(Ok(self.inner.failure(identity).cloned()))
        }
    }

    fn poll_ready<F: Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        match Pin::new(&mut future).poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test future was not ready"),
        }
    }

    #[test]
    fn async_backend_preserves_atomic_intent_result_shapes() {
        let item = delivery("one", "two");
        let key = item.checkpoint_key();
        let expected = CheckpointPrecondition {
            version: 0,
            committed: None,
        };
        let mut backend = ReadyAsyncBackend {
            inner: InMemoryCheckpointBackend::default(),
        };
        let lease = match poll_ready(backend.apply_async(CommitIntent::Acquire {
            delivery: item.clone(),
            expected: expected.clone(),
            purpose: LeasePurpose::Deliver,
        }))
        .unwrap()
        {
            CommitResult::Acquired { lease } => lease,
            other => panic!("unexpected async acquire result: {other:?}"),
        };
        let result =
            poll_ready(backend.apply_async(CommitIntent::Complete { lease, expected })).unwrap();
        assert!(matches!(result, CommitResult::CheckpointAdvanced { .. }));
        assert_eq!(
            poll_ready(backend.checkpoint_async(&key))
                .unwrap()
                .committed,
            Some(position("two"))
        );
        assert!(
            poll_ready(backend.failure_async(&FailureIdentity(item)))
                .unwrap()
                .is_none()
        );
    }

    fn delivery(token: &str, successor: &str) -> DeliveryIdentity {
        DeliveryIdentity {
            consumer: ConsumerIdentity {
                principal: component("principal-a"),
                root: component("root-a"),
                function: component("fn-a"),
                binding: component("binding-a"),
            },
            source_format: component("source-format"),
            source: component("source-a"),
            partition_format: component("partition-format"),
            partition: Some(component("partition-a")),
            position_format: component("offset-v1"),
            position: position(token),
            successor: position(successor),
        }
    }
    #[test]
    /// Delivery identity equality covers the typed natural key and excludes the successor position.
    fn failure_identity_is_the_exact_typed_delivery_composite() {
        let base = delivery("opaque-position", "successor-one");
        let same_delivery = delivery("opaque-position", "successor-two");
        assert_eq!(
            FailureIdentity(base.clone()),
            FailureIdentity(same_delivery)
        );

        let assert_distinct = |changed: DeliveryIdentity| {
            assert_ne!(FailureIdentity(base.clone()), FailureIdentity(changed));
        };

        let mut changed = base.clone();
        changed.consumer.function = component("fn-b");
        assert_distinct(changed);
        let mut changed = base.clone();
        changed.source_format = component("source-format-b");
        assert_distinct(changed);
        let mut changed = base.clone();
        changed.source = component("source-b");
        assert_distinct(changed);
        let mut changed = base.clone();
        changed.partition_format = component("partition-format-b");
        assert_distinct(changed);
        let mut changed = base.clone();
        changed.partition = Some(component("partition-b"));
        assert_distinct(changed);
        let mut changed = base.clone();
        changed.position_format = component("offset-v2");
        assert_distinct(changed);
        let mut changed = base.clone();
        changed.position = position("another-position");
        assert_distinct(changed);
    }
    fn expected(
        backend: &InMemoryCheckpointBackend,
        delivery: &DeliveryIdentity,
    ) -> CheckpointPrecondition {
        CheckpointPrecondition::from(&backend.checkpoint(&delivery.checkpoint_key()))
    }
    fn acquire(
        backend: &mut InMemoryCheckpointBackend,
        delivery: DeliveryIdentity,
    ) -> DeliveryLease {
        match backend.apply(CommitIntent::Acquire {
            expected: expected(backend, &delivery),
            delivery,
            purpose: LeasePurpose::Deliver,
        }) {
            CommitResult::Acquired { lease } => lease,
            result => panic!("unexpected result: {result:?}"),
        }
    }
    fn acquire_skip(
        backend: &mut InMemoryCheckpointBackend,
        delivery: DeliveryIdentity,
    ) -> DeliveryLease {
        match backend.apply(CommitIntent::Acquire {
            expected: expected(backend, &delivery),
            delivery,
            purpose: LeasePurpose::Skip,
        }) {
            CommitResult::Acquired { lease } => lease,
            result => panic!("unexpected result: {result:?}"),
        }
    }
    fn diagnostic() -> SafeDiagnostic {
        SafeDiagnostic {
            code: DiagnosticCode::ProviderUnavailable,
            class: DiagnosticClass::Transient,
        }
    }
    fn fail(backend: &mut InMemoryCheckpointBackend, lease: DeliveryLease) -> FailureRecord {
        match backend.apply(CommitIntent::Fail {
            lease,
            diagnostic: diagnostic(),
        }) {
            CommitResult::Failed { failure } => failure,
            result => panic!("unexpected result: {result:?}"),
        }
    }
    fn acquire_and_fail(
        backend: &mut InMemoryCheckpointBackend,
        delivery: DeliveryIdentity,
    ) -> FailureRecord {
        let lease = acquire(backend, delivery);
        fail(backend, lease)
    }

    // ORNA-FAIL-005: delivery identity includes the complete typed position, not a digest alone.
    #[test]
    fn identity_is_canonical_and_stable() {
        let first = delivery("receipt:opaque-a", "resume:opaque-b");
        let second = delivery("receipt:opaque-a", "resume:different");
        assert_eq!(first.canonical(), second.canonical());
        assert_eq!(first.checkpoint_key(), second.checkpoint_key());
        assert_eq!(first, second);
        assert!(Component::new("bad|identity").is_err());
    }

    #[test]
    fn nullable_partition_has_a_distinct_stable_checkpoint_identity() {
        let partitioned = delivery("receipt:one", "resume:two");
        let mut unpartitioned = partitioned.clone();
        unpartitioned.partition = None;
        assert_ne!(partitioned, unpartitioned);
        assert_ne!(partitioned.checkpoint_key(), unpartitioned.checkpoint_key());
        assert_ne!(partitioned.canonical(), unpartitioned.canonical());

        let mut backend = InMemoryCheckpointBackend::default();
        let expected = expected(&backend, &unpartitioned);
        let lease = acquire(&mut backend, unpartitioned.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Complete { lease, expected }),
            CommitResult::CheckpointAdvanced { ref checkpoint }
                if checkpoint.key == unpartitioned.checkpoint_key()
        ));
        assert_eq!(
            backend.checkpoint(&partitioned.checkpoint_key()).committed,
            None
        );
    }

    #[test]
    fn diagnostics_are_redacted() {
        let printed = format!("{:?}", diagnostic());
        assert!(!printed.contains("password"));
        assert!(!printed.contains("token="));
        assert!(!printed.contains("secret"));
    }

    fn diagnostic_snapshot() -> orna_value::Snapshot {
        orna_value::Snapshot::cwd([3; 16], [4; 16], 5.into()).unwrap()
    }

    fn diagnostic_row(key: &str) -> orna_value::RowRef {
        orna_value::RowRef::new(
            [3; 16],
            [6; 16],
            orna_value::OvbRaw::Text(key.into()),
            diagnostic_snapshot(),
        )
        .unwrap()
    }

    fn diagnostic_file() -> FileRef {
        FileRef::from_row_ref(diagnostic_row("orders.orna"))
    }

    #[test]
    fn table_assertion_diagnostic_uses_pinned_safe_context() {
        let assertion = AssertionRef::from_row_ref(diagnostic_row("assertion"));
        let owner = ObjectRef::from_row_ref(diagnostic_row("owner"));
        let predicate = ExpressionRef::from_row_ref(diagnostic_row("predicate"));
        let source_span = SourceSpan::new(
            diagnostic_file(),
            orna_value::SourceSpanExtent {
                start_byte: 24.into(),
                end_byte: 53.into(),
                start_line: 2.into(),
                start_column: 5.into(),
                end_line: 4.into(),
                end_column: 11.into(),
            },
        )
        .unwrap();
        let witness = SafeAssertionWitness::from_safe_presentation(
            SafeText::new("deterministic-safe-witness").unwrap(),
        );
        let detail = AssertionDiagnosticDetail::table_or_cross_table_false(
            assertion.clone(),
            owner.clone(),
            AssertionOwnerKind::Table,
            predicate.clone(),
            source_span.clone(),
            Some(witness),
        );

        assert_eq!(
            AssertionDiagnosticCode::TableOrCrossTableFalse.as_str(),
            ORNA_A091_009
        );
        assert_eq!(detail.assertion, assertion);
        assert_eq!(detail.owner, owner);
        assert_eq!(detail.predicate, predicate);
        assert_eq!(detail.source_span, source_span);
        assert_eq!(detail.owner_kind, AssertionOwnerKind::Table);
        assert_eq!(
            detail
                .witness
                .as_ref()
                .unwrap()
                .as_safe_presentation()
                .as_str(),
            "deterministic-safe-witness"
        );
        assert!(!format!("{detail:?}").contains("deterministic-safe-witness"));
    }

    #[test]
    fn assertion_failure_intent_retains_detail_through_retry() {
        let detail = AssertionDiagnosticDetail::table_or_cross_table_false(
            AssertionRef::from_row_ref(diagnostic_row("assertion")),
            ObjectRef::from_row_ref(diagnostic_row("owner")),
            AssertionOwnerKind::Table,
            ExpressionRef::from_row_ref(diagnostic_row("predicate")),
            SourceSpan::new(
                diagnostic_file(),
                orna_value::SourceSpanExtent {
                    start_byte: 1.into(),
                    end_byte: 2.into(),
                    start_line: 1.into(),
                    start_column: 2.into(),
                    end_line: 1.into(),
                    end_column: 3.into(),
                },
            )
            .unwrap(),
            None,
        );
        let delivery = delivery("assertion", "after-assertion");
        let mut backend = InMemoryCheckpointBackend::default();
        let expected = expected(&backend, &delivery);
        let lease = acquire(&mut backend, delivery);
        let failed = match backend.apply(CommitIntent::FailWithAssertion {
            lease,
            diagnostic: diagnostic(),
            detail: detail.clone(),
        }) {
            CommitResult::Failed { failure } => failure,
            result => panic!("unexpected result: {result:?}"),
        };
        assert_eq!(failed.assertion_detail, Some(detail.clone()));
        assert_eq!(failed.diagnostic.code, DiagnosticCode::TableAssertionFalse);
        assert!(matches!(
            backend.apply(CommitIntent::Retry {
                failure: failed.identity,
                expected_version: failed.version,
                expected,
            }),
            CommitResult::RetryScheduled { ref failure }
                if failure.assertion_detail == Some(detail)
        ));
    }

    // ORNA-SYS-066: retained payload material is redacted from diagnostic formatting.
    #[test]
    fn retained_provider_payload_debug_output_is_redacted() {
        let plaintext = StreamFailurePayload::Plaintext(b"provider-secret-payload".to_vec());
        let protected = StreamFailurePayload::ProtectedReference {
            reference: "provider-secret-reference".to_owned(),
            digest: [7; 32],
        };

        let plaintext_debug = format!("{plaintext:?}");
        assert!(plaintext_debug.contains("Plaintext"));
        assert!(plaintext_debug.contains("bytes: 23"));
        assert!(plaintext_debug.contains("redacted: true"));
        assert!(!plaintext_debug.contains("provider-secret-payload"));

        let protected_debug = format!("{protected:?}");
        assert!(protected_debug.contains("ProtectedReference"));
        assert!(protected_debug.contains("redacted: true"));
        assert!(!protected_debug.contains("provider-secret-reference"));
        assert!(!protected_debug.contains("7, 7, 7"));
    }

    /// ORNA-FAIL-001: repeated processing failures update the one durable failure identity.
    #[test]
    fn repeated_failures_update_one_stable_row() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let first = acquire_and_fail(&mut backend, item.clone());
        let retry = backend.apply(CommitIntent::Retry {
            failure: first.identity.clone(),
            expected_version: first.version,
            expected: expected(&backend, &item),
        });
        let retry_version = match retry {
            CommitResult::RetryScheduled { failure } => failure.version,
            _ => panic!(),
        };
        let second = acquire_and_fail(&mut backend, item);
        assert_eq!(first.identity, second.identity);
        assert_eq!(second.attempts, 2);
        assert!(second.version > retry_version);
    }

    #[test]
    fn provider_selected_successor_advances_opaque_positions() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first = delivery("receipt/first", "resume::after-first");
        let expected_before = expected(&backend, &first);
        let lease = acquire(&mut backend, first.clone());
        let committed = match backend.apply(CommitIntent::Complete {
            lease,
            expected: expected_before,
        }) {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            result => panic!("unexpected result: {result:?}"),
        };
        assert_eq!(committed.committed, Some(position("resume::after-first")));

        let next = delivery("receipt/second", "resume::final");
        assert!(matches!(
            backend.apply(CommitIntent::Acquire {
                delivery: next,
                expected: expected(&backend, &first),
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Acquired { .. }
        ));
    }

    // ORNA-CP-006: a divergent successor is rejected rather than advancing beyond a blocked delivery.
    #[test]
    fn divergent_checkpoint_position_is_rejected() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first = delivery("receipt/first", "resume::after-first");
        let expected_before = expected(&backend, &first);
        let lease = acquire(&mut backend, first.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Complete {
                lease,
                expected: expected_before
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let next = delivery("receipt/second", "resume::final");
        let divergent = CheckpointPrecondition {
            version: backend.checkpoint(&next.checkpoint_key()).version,
            committed: Some(position("resume::elsewhere")),
        };
        assert_eq!(
            backend.apply(CommitIntent::Acquire {
                delivery: next,
                expected: divergent,
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
    }

    // ORNA-TYPE-003: a failed delivery remains explicit until the stream retry succeeds or fails.
    // ORNA-SYS-122: retry transitions preserve one failure identity and checkpoint semantics.
    /// ORNA-FAIL-002 / ORNA-SYS-121 / ORNA-SYS-122: retry admission and outcomes preserve the same blocked delivery until success.
    #[test]
    fn retry_can_succeed_or_fail() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failed = acquire_and_fail(&mut backend, item.clone());
        let scheduled = match backend.apply(CommitIntent::Retry {
            failure: failed.identity.clone(),
            expected_version: failed.version,
            expected: expected(&backend, &item),
        }) {
            CommitResult::RetryScheduled { failure } => failure,
            _ => panic!(),
        };
        assert_eq!(scheduled.attempts, 2);
        let lease = acquire(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Complete {
                lease,
                expected: expected(&backend, &item)
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        assert_eq!(
            backend.failure(&failed.identity).unwrap().status,
            FailureStatus::Succeeded
        );
        assert_eq!(backend.failure(&failed.identity).unwrap().attempts, 2);

        let second = delivery("receipt:one", "resume:two");
        let failed = acquire_and_fail(&mut backend, second.clone());
        let scheduled = match backend.apply(CommitIntent::Retry {
            failure: failed.identity.clone(),
            expected_version: failed.version,
            expected: expected(&backend, &second),
        }) {
            CommitResult::RetryScheduled { failure } => failure,
            _ => panic!(),
        };
        assert_eq!(scheduled.attempts, 2);
        let failed_again = acquire_and_fail(&mut backend, second);
        assert_eq!(failed_again.status, FailureStatus::Failed);
        assert_eq!(failed_again.attempts, 2);
        assert!(scheduled.version < backend.failure(&FailureIdentity(item)).unwrap().version);
    }

    // ORNA-SYS-121: retry admission checks failure preconditions before user work executes.
    #[test]
    fn retry_admission_preserves_attempts_when_interrupted() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failed = acquire_and_fail(&mut backend, item.clone());
        let retry = match backend.apply(CommitIntent::Retry {
            failure: failed.identity.clone(),
            expected_version: failed.version,
            expected: expected(&backend, &item),
        }) {
            CommitResult::RetryScheduled { failure } => failure,
            result => panic!("unexpected result: {result:?}"),
        };
        assert_eq!(retry.attempts, 2);
        let before = backend.checkpoint(&item.checkpoint_key());
        let lease = acquire(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Cancel { lease }),
            CommitResult::Cancelled { .. }
        ));
        assert_eq!(backend.checkpoint(&item.checkpoint_key()), before);
        let recovered = backend.failure(&failed.identity).unwrap();
        assert_eq!(recovered.status, FailureStatus::Failed);
        assert_eq!(recovered.attempts, 2);
    }

    // ORNA-FAIL-003: explicit skip records the delivery and advances to its provider successor.
    #[test]
    /// ORNA-FAIL-003 / ORNA-CP-006: explicit skip advances only the blocked delivery to its provider successor.
    fn skip_advances_exactly_one_position() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let lease = acquire_skip(&mut backend, item.clone());
        let result = backend.apply(CommitIntent::Skip {
            lease,
            expected: expected(&backend, &item),
            expected_failure_version: failure.version,
        });
        assert!(
            matches!(result, CommitResult::CheckpointAdvanced { ref checkpoint } if checkpoint.committed == Some(item.successor))
        );
        assert_eq!(
            backend.failure(&failure.identity).unwrap().status,
            FailureStatus::Skipped
        );
    }

    #[test]
    /// ORNA-FAIL-004 / ORNA-SYS-123: replay succeeds without moving the live checkpoint.
    fn replay_does_not_move_live_checkpoint() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease,
                expected: expected(&backend, &item),
                expected_failure_version: failure.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let before = backend.checkpoint(&item.checkpoint_key());
        let replay = match backend.apply(CommitIntent::Replay {
            failure: failure.identity.clone(),
            expected_version: failure.version + 1,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected result: {result:?}"),
        };
        backend.claim_replay(&replay).unwrap();
        assert_eq!(
            backend.failure(&failure.identity).unwrap().status,
            FailureStatus::Replaying
        );
        assert!(matches!(
            backend.apply(CommitIntent::ReplayComplete {
                failure: replay.failure,
                expected_version: replay.version,
            }),
            CommitResult::ReplayCompleted { .. }
        ));
        assert_eq!(backend.checkpoint(&item.checkpoint_key()), before);
        assert_eq!(
            backend.failure(&failure.identity).unwrap().status,
            FailureStatus::Replayed
        );
    }

    #[test]
    /// ORNA-SYS-123: replaying a skipped delivery does not block later ordered delivery admission.
    fn admitted_replay_does_not_block_later_ordered_delivery() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failed = acquire_and_fail(&mut backend, item.clone());
        let skip_lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease: skip_lease,
                expected: expected(&backend, &item),
                expected_failure_version: failed.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let skipped = backend.failure(&failed.identity).unwrap().clone();
        let replay = match backend.apply(CommitIntent::Replay {
            failure: failed.identity.clone(),
            expected_version: skipped.version,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected replay result: {result:?}"),
        };
        assert_eq!(
            backend.failure(&failed.identity).unwrap().status,
            FailureStatus::Replaying
        );

        let later = delivery("receipt:later", "resume:later");
        assert!(matches!(
            backend.apply(CommitIntent::Acquire {
                delivery: later,
                expected: expected(&backend, &item),
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Acquired { .. }
        ));
        assert_eq!(
            backend.failure(&replay.failure).unwrap().status,
            FailureStatus::Replaying
        );
    }

    #[test]
    /// ORNA-SYS-123: an admitted replay does not prevent stream resume.
    fn admitted_replay_does_not_block_stream_resume() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:resume", "resume:one");
        let failed = acquire_and_fail(&mut backend, item.clone());
        let skip_lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease: skip_lease,
                expected: expected(&backend, &item),
                expected_failure_version: failed.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let skipped = backend.failure(&failed.identity).unwrap().clone();
        assert!(matches!(
            backend.apply(CommitIntent::Replay {
                failure: failed.identity,
                expected_version: skipped.version,
            }),
            CommitResult::ReplayGranted { .. }
        ));

        assert!(matches!(
            backend.apply(CommitIntent::Pause {
                key: item.checkpoint_key(),
            }),
            CommitResult::StreamStatusChanged { changed: true, .. }
        ));
        assert!(matches!(
            backend.apply(CommitIntent::Resume {
                key: item.checkpoint_key(),
            }),
            CommitResult::StreamStatusChanged { changed: true, .. }
        ));
        assert_eq!(
            backend.failure(&skipped.identity).unwrap().status,
            FailureStatus::Replaying
        );
    }

    #[test]
    /// ORNA-SYS-123: failed replay restores skipped state and preserves the admitted attempt count.
    fn replay_failure_preserves_the_admission_attempt_count() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease,
                expected: expected(&backend, &item),
                expected_failure_version: failure.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let replay = match backend.apply(CommitIntent::Replay {
            failure: failure.identity.clone(),
            expected_version: failure.version + 1,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected result: {result:?}"),
        };
        backend.claim_replay(&replay).unwrap();
        let failed_again = match backend.apply(CommitIntent::ReplayFail {
            failure: replay.failure,
            expected_version: replay.version,
            diagnostic: diagnostic(),
        }) {
            CommitResult::ReplayFailed { failure } => failure,
            result => panic!("unexpected result: {result:?}"),
        };
        assert_eq!(failed_again.identity, failure.identity);
        assert_eq!(failed_again.attempts, 2);
        assert_eq!(failed_again.status, FailureStatus::Skipped);
    }

    #[test]
    /// ORNA-SYS-064 / ORNA-SYS-123: replay cancellation is version-fenced and leaves live progress unchanged.
    fn replay_cancellation_is_version_fenced_and_preserves_live_checkpoint() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease,
                expected: expected(&backend, &item),
                expected_failure_version: failure.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let before_checkpoint = backend.checkpoint(&item.checkpoint_key());
        let replay = match backend.apply(CommitIntent::Replay {
            failure: failure.identity.clone(),
            expected_version: failure.version + 1,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected result: {result:?}"),
        };

        assert_eq!(
            backend.apply(CommitIntent::ReplayCancel {
                failure: replay.failure.clone(),
                expected_version: replay.version - 1,
            }),
            CommitResult::Rejected(RejectReason::StaleFailure)
        );
        assert_eq!(
            backend.checkpoint(&item.checkpoint_key()),
            before_checkpoint
        );
        assert_eq!(
            backend.failure(&failure.identity).unwrap().status,
            FailureStatus::Replaying
        );

        let cancelled = match backend.apply(CommitIntent::ReplayCancel {
            failure: replay.failure,
            expected_version: replay.version,
        }) {
            CommitResult::ReplayCancelled { failure } => failure,
            result => panic!("unexpected result: {result:?}"),
        };
        assert_eq!(cancelled.identity, failure.identity);
        assert_eq!(cancelled.attempts, 2);
        assert_eq!(cancelled.status, FailureStatus::Skipped);
        assert_eq!(
            backend.checkpoint(&item.checkpoint_key()),
            before_checkpoint
        );
    }

    #[test]
    /// ORNA-SYS-123: cancellation after replay claim cannot mutate terminal replay state.
    fn replay_cancellation_is_fenced_after_execution_claim() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:claimed", "resume:claimed");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease,
                expected: expected(&backend, &item),
                expected_failure_version: failure.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let replay = match backend.apply(CommitIntent::Replay {
            failure: failure.identity.clone(),
            expected_version: failure.version + 1,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected result: {result:?}"),
        };
        backend.claim_replay(&replay).unwrap();
        assert_eq!(
            backend.apply(CommitIntent::ReplayCancel {
                failure: replay.failure.clone(),
                expected_version: replay.version,
            }),
            CommitResult::Rejected(RejectReason::LeaseAlreadyHeld)
        );
        assert_eq!(
            backend.failure(&replay.failure).unwrap().status,
            FailureStatus::Replaying
        );
    }

    // ORNA-SYS-124: a blocking open failure cannot be resolved without success or skip.
    #[test]
    fn resolve_requires_success_or_skip() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let failure = acquire_and_fail(&mut backend, item.clone());
        assert_eq!(
            backend.apply(CommitIntent::Resolve {
                failure: failure.identity.clone(),
                expected_version: failure.version
            }),
            CommitResult::Rejected(RejectReason::ResolveBlocked)
        );
        match backend.apply(CommitIntent::Retry {
            failure: failure.identity.clone(),
            expected_version: failure.version,
            expected: expected(&backend, &item),
        }) {
            CommitResult::RetryScheduled { .. } => (),
            result => panic!("unexpected result: {result:?}"),
        };
        let lease = acquire(&mut backend, item.clone());
        let advanced = backend.apply(CommitIntent::Complete {
            lease,
            expected: expected(&backend, &item),
        });
        assert!(matches!(advanced, CommitResult::CheckpointAdvanced { .. }));
        let record = backend.failure(&failure.identity).unwrap().clone();
        assert!(matches!(
            backend.apply(CommitIntent::Resolve {
                failure: failure.identity,
                expected_version: record.version
            }),
            CommitResult::Resolved { .. }
        ));
    }

    #[test]
    fn lease_fencing_rejects_stale_holder() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let first = acquire(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Cancel {
                lease: first.clone()
            }),
            CommitResult::Cancelled { .. }
        ));
        let second = acquire(&mut backend, item.clone());
        assert!(second.fence > first.fence);
        assert_eq!(
            backend.apply(CommitIntent::Fail {
                lease: first,
                diagnostic: diagnostic()
            }),
            CommitResult::Rejected(RejectReason::LeaseFenced)
        );
    }

    #[test]
    fn ordered_stream_admits_only_one_provider_delivery_lease() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first_delivery = delivery("receipt:first", "resume:after-first");
        let first = acquire(&mut backend, first_delivery.clone());
        let next_delivery = delivery("receipt:next", "resume:after-next");
        let before = expected(&backend, &next_delivery);

        assert_eq!(
            backend.apply(CommitIntent::Acquire {
                delivery: next_delivery.clone(),
                expected: before.clone(),
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Rejected(RejectReason::LeaseAlreadyHeld)
        );

        assert!(matches!(
            backend.apply(CommitIntent::Cancel { lease: first }),
            CommitResult::Cancelled { .. }
        ));
        assert!(matches!(
            backend.apply(CommitIntent::Acquire {
                delivery: next_delivery,
                expected: before,
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Acquired { .. }
        ));
    }

    // ORNA-FAIL-002: an unresolved failure blocks later ordered delivery instead of discarding it.
    #[test]
    fn failed_delivery_blocks_later_ordered_delivery_until_administration() {
        let mut backend = InMemoryCheckpointBackend::default();
        let failed_delivery = delivery("receipt:failed", "resume:after-failed");
        acquire_and_fail(&mut backend, failed_delivery.clone());
        let later_delivery = delivery("receipt:later", "resume:after-later");

        assert_eq!(
            backend.apply(CommitIntent::Acquire {
                delivery: later_delivery,
                expected: expected(&backend, &failed_delivery),
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Rejected(RejectReason::BlockingFailure)
        );
    }

    #[test]
    fn pause_stops_new_admission_at_the_active_delivery_boundary() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let active = acquire(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Pause {
                key: item.checkpoint_key(),
            }),
            CommitResult::PausePending {
                state: StreamState {
                    status: StreamStatus::Running,
                    ..
                },
                changed: true,
            }
        ));
        assert_eq!(
            backend.apply(CommitIntent::Reset {
                key: item.checkpoint_key(),
                expected: expected(&backend, &item),
                to: position("opaque/provider-token"),
            }),
            CommitResult::Rejected(RejectReason::StreamNotPaused)
        );
        assert_eq!(
            backend.apply(CommitIntent::Acquire {
                delivery: delivery("receipt:next", "resume:two"),
                expected: expected(&backend, &item),
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Rejected(RejectReason::LeaseAlreadyHeld)
        );
        assert!(matches!(
            backend.apply(CommitIntent::Complete {
                lease: active,
                expected: expected(&backend, &item),
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        assert_eq!(
            backend.apply(CommitIntent::Acquire {
                delivery: delivery("receipt:next", "resume:two"),
                expected: expected(&backend, &item),
                purpose: LeasePurpose::Deliver,
            }),
            CommitResult::Rejected(RejectReason::StreamPaused)
        );

        let key = item.checkpoint_key();
        let before = backend.checkpoint(&key);
        let rewound = position("opaque/provider-token");
        assert!(matches!(
            backend.apply(CommitIntent::Reset {
                key: key.clone(),
                expected: CheckpointPrecondition::from(&before),
                to: rewound.clone(),
            }),
            CommitResult::CheckpointReset {
                checkpoint: Checkpoint {
                    committed: Some(ref position),
                    version: 2,
                    ..
                }
            } if position == &rewound
        ));
        assert_eq!(
            backend.apply(CommitIntent::Reset {
                key: key.clone(),
                expected: CheckpointPrecondition::from(&before),
                to: position("stale-token"),
            }),
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
        assert!(matches!(
            backend.apply(CommitIntent::Resume { key: key.clone() }),
            CommitResult::StreamStatusChanged {
                state: StreamState {
                    status: StreamStatus::Running,
                    ..
                },
                changed: true,
            }
        ));
        assert!(matches!(
            backend.apply(CommitIntent::Resume { key }),
            CommitResult::StreamStatusChanged { changed: false, .. }
        ));
    }

    // ORNA-CP-004: reset treats positions as opaque and requires matching preconditions.
    #[test]
    fn reset_is_opaque_and_compare_and_set() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let key = item.checkpoint_key();
        assert!(matches!(
            backend.apply(CommitIntent::Pause { key: key.clone() }),
            CommitResult::StreamStatusChanged { .. }
        ));
        let rewound = position("opaque/provider-token");

        assert!(matches!(
            backend.apply(CommitIntent::Reset {
                key: key.clone(),
                expected: expected(&backend, &item),
                to: rewound.clone(),
            }),
            CommitResult::CheckpointReset {
                checkpoint: Checkpoint {
                    committed: Some(ref position),
                    version: 1,
                    ..
                }
            } if position == &rewound
        ));
        assert_eq!(
            backend.apply(CommitIntent::Reset {
                key: key.clone(),
                expected: CheckpointPrecondition {
                    version: 0,
                    committed: None,
                },
                to: position("stale-token"),
            }),
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
    }

    #[test]
    fn active_delivery_lease_rejects_a_forged_successor() {
        let mut backend = InMemoryCheckpointBackend::default();
        let admitted = delivery("receipt:leased", "resume:admitted");
        let lease = acquire(&mut backend, admitted.clone());
        let expected = expected(&backend, &admitted);

        let mut forged = lease.clone();
        forged.delivery.successor = position("resume:forged");
        assert_eq!(forged.delivery, admitted);
        assert_eq!(forged.delivery.canonical(), admitted.canonical());

        assert_eq!(
            backend.apply(CommitIntent::Complete {
                lease: forged,
                expected: expected.clone(),
            }),
            CommitResult::Rejected(RejectReason::LeaseFenced)
        );
        let checkpoint = backend.checkpoint(&admitted.checkpoint_key());
        assert_eq!(checkpoint.version, 0);
        assert_eq!(checkpoint.committed, None);

        assert!(matches!(
            backend.apply(CommitIntent::Complete { lease, expected }),
            CommitResult::CheckpointAdvanced { ref checkpoint }
                if checkpoint.committed == Some(admitted.successor.clone())
        ));
    }

    #[test]
    fn successor_drift_updates_one_stable_failure_row() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first = delivery("receipt:stable", "resume:first");
        let failed = acquire_and_fail(&mut backend, first.clone());
        let same_delivery = delivery("receipt:stable", "resume:changed");
        let retry = match backend.apply(CommitIntent::Retry {
            failure: failed.identity.clone(),
            expected_version: failed.version,
            expected: expected(&backend, &same_delivery),
        }) {
            CommitResult::RetryScheduled { failure } => failure,
            result => panic!("unexpected retry result: {result:?}"),
        };
        let lease = acquire(&mut backend, same_delivery);
        let advanced = match backend.apply(CommitIntent::Complete {
            lease,
            expected: expected(&backend, &first),
        }) {
            CommitResult::CheckpointAdvanced { checkpoint } => checkpoint,
            result => panic!("unexpected retry completion result: {result:?}"),
        };
        assert_eq!(advanced.committed, Some(position("resume:changed")));
        let succeeded = backend.failure(&failed.identity).unwrap();
        assert_eq!(succeeded.identity, failed.identity);
        assert_eq!(succeeded.attempts, retry.attempts);
        assert_eq!(succeeded.status, FailureStatus::Succeeded);
    }

    #[test]
    fn retry_rejects_a_successor_the_failure_did_not_record() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first = delivery("receipt:retry-successor", "resume:recorded");
        let failed = acquire_and_fail(&mut backend, first.clone());
        let forged = delivery("receipt:retry-successor", "resume:forged");
        assert_eq!(
            backend.apply(CommitIntent::Retry {
                failure: FailureIdentity(forged.clone()),
                expected_version: failed.version,
                expected: expected(&backend, &forged),
            }),
            CommitResult::Rejected(RejectReason::StaleFailure)
        );
        let record = backend.failure(&failed.identity).unwrap();
        assert_eq!(record.version, failed.version);
        assert_eq!(record.status, FailureStatus::Failed);
        assert_eq!(backend.checkpoint(&first.checkpoint_key()).version, 0);
    }

    #[test]
    fn skip_rejects_a_successor_the_failure_did_not_record() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first = delivery("receipt:skip-successor", "resume:recorded");
        let failed = acquire_and_fail(&mut backend, first.clone());
        let forged = delivery("receipt:skip-successor", "resume:forged");
        let lease = acquire_skip(&mut backend, forged.clone());
        assert_eq!(
            backend.apply(CommitIntent::Skip {
                lease,
                expected: expected(&backend, &forged),
                expected_failure_version: failed.version,
            }),
            CommitResult::Rejected(RejectReason::StaleFailure)
        );
        let record = backend.failure(&failed.identity).unwrap();
        assert_eq!(record.status, FailureStatus::Failed);
        assert_eq!(backend.checkpoint(&first.checkpoint_key()).committed, None);
    }

    #[test]
    fn failed_retry_refreshes_successor_without_changing_identity() {
        let mut backend = InMemoryCheckpointBackend::default();
        let original = delivery("receipt:retry-failed", "resume:original");
        let first = acquire_and_fail(&mut backend, original.clone());
        let revised = delivery("receipt:retry-failed", "resume:revised");
        let scheduled = backend.apply(CommitIntent::Retry {
            failure: first.identity.clone(),
            expected_version: first.version,
            expected: expected(&backend, &revised),
        });
        assert!(matches!(scheduled, CommitResult::RetryScheduled { .. }));
        let revised_lease = acquire(&mut backend, revised.clone());
        let failed = fail(&mut backend, revised_lease);
        assert_eq!(failed.identity, first.identity);
        assert_eq!(failed.identity.0.successor, revised.successor);
        assert_eq!(failed.attempts, 2);
        assert_eq!(failed.status, FailureStatus::Failed);
        assert_eq!(backend.checkpoint(&revised.checkpoint_key()).version, 0);
        assert_eq!(
            backend.checkpoint(&revised.checkpoint_key()).committed,
            None
        );
        let skip_lease = acquire_skip(&mut backend, revised.clone());
        let skipped = backend.apply(CommitIntent::Skip {
            lease: skip_lease,
            expected: expected(&backend, &revised),
            expected_failure_version: failed.version,
        });
        assert!(matches!(
            skipped,
            CommitResult::CheckpointAdvanced { ref checkpoint }
                if checkpoint.committed == Some(revised.successor)
        ));
    }

    #[test]
    fn reset_allows_reprocessing_a_resolved_failure_with_one_stable_identity() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:resolved", "resume:resolved");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let skip_lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease: skip_lease,
                expected: expected(&backend, &item),
                expected_failure_version: failure.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let skipped = backend.failure(&failure.identity).unwrap().clone();
        let replay = match backend.apply(CommitIntent::Replay {
            failure: skipped.identity.clone(),
            expected_version: skipped.version,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected replay result: {result:?}"),
        };
        backend.claim_replay(&replay).unwrap();
        let replayed = match backend.apply(CommitIntent::ReplayComplete {
            failure: replay.failure,
            expected_version: replay.version,
        }) {
            CommitResult::ReplayCompleted { failure } => failure,
            result => panic!("unexpected replay completion result: {result:?}"),
        };
        let resolved = match backend.apply(CommitIntent::Resolve {
            failure: replayed.identity.clone(),
            expected_version: replayed.version,
        }) {
            CommitResult::Resolved { failure } => failure,
            result => panic!("unexpected resolve result: {result:?}"),
        };

        let paused = backend.apply(CommitIntent::Pause {
            key: item.checkpoint_key(),
        });
        assert!(matches!(paused, CommitResult::StreamStatusChanged { .. }));
        let reset = match backend.apply(CommitIntent::Reset {
            key: item.checkpoint_key(),
            expected: expected(&backend, &item),
            to: item.position.clone(),
        }) {
            CommitResult::CheckpointReset { checkpoint } => checkpoint,
            result => panic!("unexpected reset result: {result:?}"),
        };
        assert!(matches!(
            backend.apply(CommitIntent::Resume {
                key: item.checkpoint_key(),
            }),
            CommitResult::StreamStatusChanged { .. }
        ));
        let lease = match backend.apply(CommitIntent::Acquire {
            delivery: item.clone(),
            expected: (&reset).into(),
            purpose: LeasePurpose::Deliver,
        }) {
            CommitResult::Acquired { lease } => lease,
            result => panic!("unexpected reprocessing acquire result: {result:?}"),
        };
        let again = match backend.apply(CommitIntent::Fail {
            lease,
            diagnostic: diagnostic(),
        }) {
            CommitResult::Failed { failure } => failure,
            result => panic!("unexpected reprocessing failure result: {result:?}"),
        };
        assert_eq!(again.identity, resolved.identity);
        assert_eq!(again.attempts, resolved.attempts + 1);
        assert_eq!(again.status, FailureStatus::Failed);
    }

    #[test]
    fn replay_terminal_transition_requires_execution_claim() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:claim-required", "resume:claim-required");
        let failure = acquire_and_fail(&mut backend, item.clone());
        let skip_lease = acquire_skip(&mut backend, item.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Skip {
                lease: skip_lease,
                expected: expected(&backend, &item),
                expected_failure_version: failure.version,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));
        let skipped = backend.failure(&failure.identity).unwrap().clone();
        let replay = match backend.apply(CommitIntent::Replay {
            failure: skipped.identity.clone(),
            expected_version: skipped.version,
        }) {
            CommitResult::ReplayGranted { grant } => grant,
            result => panic!("unexpected replay result: {result:?}"),
        };
        let checkpoint_before = backend.checkpoint(&item.checkpoint_key());

        assert_eq!(
            backend.apply(CommitIntent::ReplayComplete {
                failure: replay.failure.clone(),
                expected_version: replay.version,
            }),
            CommitResult::Rejected(RejectReason::LeaseFenced)
        );
        assert_eq!(
            backend.failure(&replay.failure).unwrap().status,
            FailureStatus::Replaying
        );
        assert_eq!(
            backend.checkpoint(&item.checkpoint_key()),
            checkpoint_before
        );

        backend.claim_replay(&replay).unwrap();
        assert!(matches!(
            backend.apply(CommitIntent::ReplayComplete {
                failure: replay.failure,
                expected_version: replay.version,
            }),
            CommitResult::ReplayCompleted { .. }
        ));
    }

    // ORNA-SYS-063: retry admission compares both expected checkpoint version and typed position.
    #[test]
    fn retry_rejects_a_stale_blocking_checkpoint() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first_delivery = delivery("receipt:first", "resume:after-first");
        let first_expected = expected(&backend, &first_delivery);
        let first_lease = acquire(&mut backend, first_delivery.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Complete {
                lease: first_lease,
                expected: first_expected,
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));

        let failed_delivery = delivery("receipt:failed", "resume:after-failed");
        let failure = acquire_and_fail(&mut backend, failed_delivery.clone());
        let current = expected(&backend, &failed_delivery);
        let stale = CheckpointPrecondition {
            version: current.version - 1,
            committed: Some(position("stale-position")),
        };

        assert_eq!(
            backend.apply(CommitIntent::Retry {
                failure: failure.identity.clone(),
                expected_version: failure.version,
                expected: stale,
            }),
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
        assert_eq!(backend.failure(&failure.identity).unwrap(), &failure);
    }

    // ORNA-SYS-062: cancellation leaves committed progress unchanged and creates no failure.
    #[test]
    fn cancellation_is_rollback_shaped_and_does_not_create_a_failure() {
        let mut backend = InMemoryCheckpointBackend::default();
        let item = delivery("receipt:zero", "resume:one");
        let lease = acquire(&mut backend, item.clone());
        let before = backend.checkpoint(&item.checkpoint_key());
        assert_eq!(
            backend.apply(CommitIntent::Cancel { lease }),
            CommitResult::Cancelled {
                checkpoint: before.clone(),
                classification: CancellationClassification::RollbackShaped,
            }
        );
        assert_eq!(backend.checkpoint(&item.checkpoint_key()), before);
        assert!(backend.failure(&FailureIdentity(item)).is_none());
    }
}
