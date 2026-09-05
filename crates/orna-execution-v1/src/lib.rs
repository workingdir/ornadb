//! Bounded activation coordination for one owner and one stream delivery.
//!
//! This crate owns orchestration only. Provider I/O is behind [`ProviderExecutor`]
//! and durability is behind [`AtomicCommitStore`]. In particular, it cannot make
//! two independently committing external systems atomic: a production store must
//! provide one database transaction, or a transactional outbox, for its write and
//! checkpoint intent.

use std::{collections::BTreeMap, fmt, marker::PhantomData};

pub use orna_runtime_v1::{Mutation as RuntimeMutation, PublicationFreeze};
pub use orna_stream_v1::{CheckpointPrecondition, CommitIntent, DeliveryLease, FailureIdentity};

/// A non-zero, application-defined activation identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ActivationId(u64);

impl ActivationId {
    pub fn new(value: u64) -> Result<Self, CoordinationError> {
        if value == 0 {
            Err(CoordinationError::InvalidActivation)
        } else {
            Ok(Self(value))
        }
    }
}

/// Capability for the one currently active owner. Its fields are private so an
/// owner cannot manufacture a newer fence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerLease {
    activation: ActivationId,
    epoch: u64,
    cancellation_epoch: u64,
}

/// A typed value which can be staged as an execution write.
pub trait WritePayload: Send + Sync + 'static {
    const KIND: &'static str;
    fn encode(&self) -> Vec<u8>;
}

/// A typed write; it cannot be replaced with untyped bytes at the coordinator API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedWrite<T: WritePayload> {
    value: T,
}

impl<T: WritePayload> TypedWrite<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }
    fn into_record(self) -> WriteRecord {
        WriteRecord {
            kind: T::KIND,
            bytes: self.value.encode(),
        }
    }
}

/// The opaque typed form sent to the atomic durability seam.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteRecord {
    pub kind: &'static str,
    pub bytes: Vec<u8>,
}

/// The checkpoint update to publish only alongside the write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointIntent {
    pub commit: CommitIntent,
}

impl CheckpointIntent {
    pub fn retry(
        failure: FailureIdentity,
        expected_version: u64,
        expected: CheckpointPrecondition,
    ) -> Self {
        Self {
            commit: CommitIntent::Retry {
                failure,
                expected_version,
                expected,
            },
        }
    }
    pub fn complete(lease: DeliveryLease, expected: CheckpointPrecondition) -> Self {
        Self {
            commit: CommitIntent::Complete { lease, expected },
        }
    }
    pub fn skip(
        lease: DeliveryLease,
        expected: CheckpointPrecondition,
        expected_failure_version: u64,
    ) -> Self {
        Self {
            commit: CommitIntent::Skip {
                lease,
                expected,
                expected_failure_version,
            },
        }
    }
}

/// The observable activation state. Terminal values are rollback-shaped unless
/// [`TransactionPhase::Committed`] is returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionPhase {
    Running,
    ChildrenJoining,
    Prepared,
    Committed,
    RolledBack,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RollbackReason {
    Cancelled,
    StaleOwner,
    ChildOutstanding,
    ProviderFailed,
    FaultInjected,
    StoreRejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Committed { receipt: CommitReceipt },
    RolledBack { reason: RollbackReason },
}

/// Stable child identity; every spawned child must be joined before prepare.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChildId(u64);

/// A fully fenced atomic request. A store must check the supplied owner fence in
/// the same atomic operation that makes both values visible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRequest {
    pub owner: OwnerLease,
    pub write: WriteRecord,
    pub checkpoint: CheckpointIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitReceipt {
    pub sequence: u64,
}

/// The only persistence capability held by the coordinator.
pub trait AtomicCommitStore {
    fn commit(&mut self, request: CommitRequest) -> Result<CommitReceipt, StoreError>;
}

/// Provider execution has no access to the durability capability.
pub trait ProviderExecutor<T: WritePayload> {
    fn execute(&mut self, activation: ActivationId) -> Result<T, ProviderError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    BeforeProvider,
    BeforeAtomicCommit,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultError {
    Injected,
}
pub trait FaultInjector {
    fn check(&mut self, point: FaultPoint) -> Result<(), FaultError>;
}
#[derive(Default)]
pub struct NoFault;
impl FaultInjector for NoFault {
    fn check(&mut self, _: FaultPoint) -> Result<(), FaultError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderError {
    Rejected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    Rejected,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinationError {
    InvalidActivation,
    NoOwner,
    StaleOwner,
    Cancelled,
    ChildOutstanding,
    InvalidPhase,
}
impl fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("execution coordination rejected")
    }
}
impl std::error::Error for CoordinationError {}

/// A bounded, single-threaded state machine. Sharing requires external locking;
/// the `OwnerLease` fence is still rechecked immediately before publication.
pub struct ActivationCoordinator {
    next_epoch: u64,
    next_child: u64,
    owner: Option<OwnerLease>,
    phase: TransactionPhase,
    children: BTreeMap<ChildId, bool>,
}

impl Default for ActivationCoordinator {
    fn default() -> Self {
        Self {
            next_epoch: 0,
            next_child: 0,
            owner: None,
            phase: TransactionPhase::RolledBack,
            children: BTreeMap::new(),
        }
    }
}

impl ActivationCoordinator {
    pub fn activate(&mut self, activation: ActivationId) -> OwnerLease {
        self.next_epoch += 1;
        let lease = OwnerLease {
            activation,
            epoch: self.next_epoch,
            cancellation_epoch: 0,
        };
        self.owner = Some(lease);
        self.phase = TransactionPhase::Running;
        self.children.clear();
        lease
    }
    /// Explicitly takes over a known prior owner; all of its capabilities become stale.
    pub fn replace_stale(
        &mut self,
        stale: OwnerLease,
        activation: ActivationId,
    ) -> Result<OwnerLease, CoordinationError> {
        self.require_current(stale)?;
        Ok(self.activate(activation))
    }
    pub fn cancel(&mut self, owner: OwnerLease) -> Result<(), CoordinationError> {
        self.require_current(owner)?;
        let current = self.owner.as_mut().expect("checked");
        current.cancellation_epoch += 1;
        self.phase = TransactionPhase::RolledBack;
        Ok(())
    }
    pub fn spawn_child(&mut self, owner: OwnerLease) -> Result<ChildId, CoordinationError> {
        self.require_live(owner)?;
        self.next_child += 1;
        let child = ChildId(self.next_child);
        self.children.insert(child, false);
        Ok(child)
    }
    pub fn join_child(
        &mut self,
        owner: OwnerLease,
        child: ChildId,
    ) -> Result<(), CoordinationError> {
        self.require_live(owner)?;
        let Some(joined) = self.children.get_mut(&child) else {
            return Err(CoordinationError::InvalidPhase);
        };
        *joined = true;
        Ok(())
    }
    pub fn phase(&self) -> TransactionPhase {
        self.phase
    }

    pub fn execute<
        T: WritePayload,
        P: ProviderExecutor<T>,
        S: AtomicCommitStore,
        F: FaultInjector,
    >(
        &mut self,
        owner: OwnerLease,
        provider: &mut P,
        store: &mut S,
        checkpoint: CheckpointIntent,
        faults: &mut F,
    ) -> Outcome {
        if self.require_live(owner).is_err() {
            return self.rollback_for(owner);
        }
        if self.children.values().any(|joined| !joined) {
            self.phase = TransactionPhase::ChildrenJoining;
            return Outcome::RolledBack {
                reason: RollbackReason::ChildOutstanding,
            };
        }
        self.phase = TransactionPhase::Prepared;
        if faults.check(FaultPoint::BeforeProvider).is_err() {
            return self.rollback(RollbackReason::FaultInjected);
        }
        let write = match provider.execute(owner.activation) {
            Ok(value) => TypedWrite::new(value).into_record(),
            Err(_) => return self.rollback(RollbackReason::ProviderFailed),
        };
        if self.require_live(owner).is_err() {
            return self.rollback_for(owner);
        }
        if faults.check(FaultPoint::BeforeAtomicCommit).is_err() {
            return self.rollback(RollbackReason::FaultInjected);
        }
        // This final validation is adjacent to the only publication call.
        if self.require_live(owner).is_err() {
            return self.rollback_for(owner);
        }
        match store.commit(CommitRequest {
            owner,
            write,
            checkpoint,
        }) {
            Ok(receipt) => {
                self.phase = TransactionPhase::Committed;
                Outcome::Committed { receipt }
            }
            Err(_) => self.rollback(RollbackReason::StoreRejected),
        }
    }
    fn require_current(&self, owner: OwnerLease) -> Result<(), CoordinationError> {
        if self.owner == Some(owner) {
            Ok(())
        } else {
            Err(CoordinationError::StaleOwner)
        }
    }
    fn require_live(&self, owner: OwnerLease) -> Result<(), CoordinationError> {
        self.require_current(owner)?;
        if owner.cancellation_epoch == 0 {
            Ok(())
        } else {
            Err(CoordinationError::Cancelled)
        }
    }
    fn rollback(&mut self, reason: RollbackReason) -> Outcome {
        self.phase = TransactionPhase::RolledBack;
        Outcome::RolledBack { reason }
    }
    fn rollback_for(&mut self, owner: OwnerLease) -> Outcome {
        let reason = if self.owner.is_some_and(|current| {
            current.activation == owner.activation && current.epoch == owner.epoch
        }) {
            RollbackReason::Cancelled
        } else {
            RollbackReason::StaleOwner
        };
        // A stale completion belongs to an earlier owner. It must not change
        // the phase of a successor which has already taken the coordinator.
        if reason == RollbackReason::StaleOwner {
            Outcome::RolledBack { reason }
        } else {
            self.rollback(reason)
        }
    }
}

/// Test/reference store: publication appends a write and checkpoint together.
#[derive(Default)]
pub struct InMemoryAtomicStore {
    visible: Vec<(WriteRecord, CheckpointIntent)>,
    next_sequence: u64,
    reject: bool,
}
impl InMemoryAtomicStore {
    pub fn visible(&self) -> &[(WriteRecord, CheckpointIntent)] {
        &self.visible
    }
    pub fn set_reject(&mut self, reject: bool) {
        self.reject = reject;
    }
}
impl AtomicCommitStore for InMemoryAtomicStore {
    fn commit(&mut self, request: CommitRequest) -> Result<CommitReceipt, StoreError> {
        if self.reject {
            return Err(StoreError::Rejected);
        }
        self.next_sequence += 1;
        self.visible.push((request.write, request.checkpoint));
        Ok(CommitReceipt {
            sequence: self.next_sequence,
        })
    }
}

/// Makes the generic payload visible in documentation-generated type signatures.
pub struct ProviderBoundary<T: WritePayload>(PhantomData<T>);

#[cfg(test)]
mod tests {
    use super::*;
    use orna_stream_v1::{Component, ConsumerIdentity, DeliveryIdentity, LeasePurpose, Position};

    #[derive(Clone)]
    struct Value(u8);
    impl WritePayload for Value {
        const KIND: &'static str = "test/value";
        fn encode(&self) -> Vec<u8> {
            vec![self.0]
        }
    }
    struct Provider(Result<Value, ProviderError>);
    impl ProviderExecutor<Value> for Provider {
        fn execute(&mut self, _: ActivationId) -> Result<Value, ProviderError> {
            self.0.clone()
        }
    }
    struct Fail(FaultPoint);
    impl FaultInjector for Fail {
        fn check(&mut self, point: FaultPoint) -> Result<(), FaultError> {
            if point == self.0 {
                Err(FaultError::Injected)
            } else {
                Ok(())
            }
        }
    }
    fn component(value: &str) -> Component {
        Component::new(value).unwrap()
    }
    fn checkpoint() -> CheckpointIntent {
        let delivery = DeliveryIdentity {
            consumer: ConsumerIdentity {
                principal: component("p"),
                root: component("r"),
                function: component("f"),
                binding: component("b"),
            },
            source_format: component("s"),
            source: component("source"),
            partition_format: component("p"),
            partition: component("0"),
            position_format: component("offset"),
            position: Position {
                token: component("0"),
            },
            successor: Position {
                token: component("1"),
            },
        };
        CheckpointIntent::complete(
            DeliveryLease {
                delivery,
                fence: 1,
                purpose: LeasePurpose::Deliver,
            },
            CheckpointPrecondition {
                version: 0,
                committed: None,
            },
        )
    }
    fn active() -> (ActivationCoordinator, OwnerLease) {
        let mut c = ActivationCoordinator::default();
        let o = c.activate(ActivationId::new(1).unwrap());
        (c, o)
    }

    #[test]
    fn no_partial_visibility_when_atomic_commit_faults() {
        let (mut c, owner) = active();
        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Ok(Value(7)));
        let mut fault = Fail(FaultPoint::BeforeAtomicCommit);
        assert_eq!(
            c.execute(owner, &mut provider, &mut store, checkpoint(), &mut fault),
            Outcome::RolledBack {
                reason: RollbackReason::FaultInjected
            }
        );
        assert!(store.visible().is_empty());
    }
    #[test]
    fn provider_failure_is_rollback_shaped() {
        let (mut c, owner) = active();
        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Err(ProviderError::Rejected));
        let mut fault = NoFault;
        assert_eq!(
            c.execute(owner, &mut provider, &mut store, checkpoint(), &mut fault),
            Outcome::RolledBack {
                reason: RollbackReason::ProviderFailed
            }
        );
        assert_eq!(c.phase(), TransactionPhase::RolledBack);
        assert!(store.visible().is_empty());
    }
    #[test]
    fn retry_and_skip_are_explicit_checkpoint_intents() {
        let (mut c, owner) = active();
        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Ok(Value(2)));
        let mut fault = NoFault;
        let intent = checkpoint();
        let CommitIntent::Complete { lease, expected } = intent.commit else {
            unreachable!()
        };
        let retry =
            CheckpointIntent::retry(FailureIdentity(lease.delivery.clone()), 2, expected.clone());
        assert!(matches!(retry.commit, CommitIntent::Retry { .. }));
        let skip = CheckpointIntent::skip(
            DeliveryLease {
                purpose: LeasePurpose::Skip,
                ..lease
            },
            expected,
            3,
        );
        assert!(matches!(
            c.execute(owner, &mut provider, &mut store, skip, &mut fault),
            Outcome::Committed { .. }
        ));
        assert!(matches!(
            store.visible()[0].1.commit,
            CommitIntent::Skip { .. }
        ));
    }
    #[test]
    fn cancelled_owner_cannot_publish() {
        let (mut c, owner) = active();
        c.cancel(owner).unwrap();
        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Ok(Value(3)));
        let mut fault = NoFault;
        assert_eq!(
            c.execute(owner, &mut provider, &mut store, checkpoint(), &mut fault),
            Outcome::RolledBack {
                reason: RollbackReason::Cancelled
            }
        );
        assert!(store.visible().is_empty());
    }
    #[test]
    fn stale_lease_cannot_publish_after_handover() {
        let (mut c, owner) = active();
        let replacement = c
            .replace_stale(owner, ActivationId::new(2).unwrap())
            .unwrap();
        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Ok(Value(4)));
        let mut fault = NoFault;
        assert_eq!(
            c.execute(owner, &mut provider, &mut store, checkpoint(), &mut fault),
            Outcome::RolledBack {
                reason: RollbackReason::StaleOwner
            }
        );
        assert!(store.visible().is_empty());
        assert_eq!(c.phase(), TransactionPhase::Running);
        assert!(matches!(
            c.execute(
                replacement,
                &mut provider,
                &mut store,
                checkpoint(),
                &mut fault
            ),
            Outcome::Committed { .. }
        ));
        assert_eq!(store.visible().len(), 1);
    }
    #[test]
    fn children_must_join_before_commit() {
        let (mut c, owner) = active();
        let child = c.spawn_child(owner).unwrap();
        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Ok(Value(5)));
        let mut fault = NoFault;
        assert_eq!(
            c.execute(owner, &mut provider, &mut store, checkpoint(), &mut fault),
            Outcome::RolledBack {
                reason: RollbackReason::ChildOutstanding
            }
        );
        assert!(store.visible().is_empty());
        c.join_child(owner, child).unwrap();
        assert!(matches!(
            c.execute(owner, &mut provider, &mut store, checkpoint(), &mut fault),
            Outcome::Committed { .. }
        ));
    }
}
