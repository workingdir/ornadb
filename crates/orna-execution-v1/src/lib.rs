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
    Committed {
        receipt: CommitReceipt,
    },
    RolledBack {
        reason: RollbackReason,
    },
    /// Termination has begun but an unfinished child has not acknowledged its
    /// cancellation and joined. This is deliberately non-terminal.
    ChildrenJoining {
        reason: RollbackReason,
    },
}

/// Stable child identity; every spawned child must be joined before prepare.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChildId(u64);

/// The execution host's cancellation and join boundary for an owned child.
///
/// The coordinator calls these methods in child-id order after it has fenced
/// new admissions. An adapter which cannot interrupt a child must return an
/// error and retain the owner in [`TransactionPhase::ChildrenJoining`]; it
/// must not report a terminal owner while that child can still publish.
pub trait ChildSupervisor {
    fn request_cancellation(&mut self, child: ChildId) -> Result<(), ChildTerminationError>;
    fn join(&mut self, child: ChildId) -> Result<(), ChildTerminationError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildTerminationError {
    Rejected,
    Incomplete,
}

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

/// The live owner-fence view that a store validates atomically with publication.
///
/// A durable implementation must resolve this against its durable lease or
/// transaction-generation authority, rather than treating an earlier
/// coordinator-side check as sufficient. This keeps a revoked or superseded
/// child from publishing through a store after it has lost its owner.
pub trait OwnerFence {
    fn permits(&self, owner: OwnerLease) -> bool;
}

/// The only persistence capability held by the coordinator.
///
/// `commit` MUST validate `owner_fence.permits(request.owner)` in the same
/// atomic operation that makes the write and checkpoint visible. Implementors
/// must reject the request when that validation fails; a prior caller-side
/// validation is not a substitute.
pub trait AtomicCommitStore {
    fn commit(
        &mut self,
        request: CommitRequest,
        owner_fence: &dyn OwnerFence,
    ) -> Result<CommitReceipt, StoreError>;
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
    OwnerFenceRejected,
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
    ending: bool,
    children: BTreeMap<ChildId, bool>,
}

impl Default for ActivationCoordinator {
    fn default() -> Self {
        Self {
            next_epoch: 0,
            next_child: 0,
            owner: None,
            phase: TransactionPhase::RolledBack,
            ending: false,
            children: BTreeMap::new(),
        }
    }
}

impl ActivationCoordinator {
    /// Starts an owner only when no earlier owner still requires child cleanup.
    /// Callers which can handle an admission error should prefer this method.
    pub fn try_activate(
        &mut self,
        activation: ActivationId,
    ) -> Result<OwnerLease, CoordinationError> {
        if matches!(
            self.phase,
            TransactionPhase::Running
                | TransactionPhase::ChildrenJoining
                | TransactionPhase::Prepared
        ) {
            return Err(CoordinationError::ChildOutstanding);
        }
        Ok(self.activate_unchecked(activation))
    }
    /// Compatibility convenience for fresh coordinators. Starting another
    /// owner while cleanup is pending is rejected before any child is lost.
    pub fn activate(&mut self, activation: ActivationId) -> OwnerLease {
        self.try_activate(activation)
            .expect("activation admission must not discard owned children")
    }
    fn activate_unchecked(&mut self, activation: ActivationId) -> OwnerLease {
        self.next_epoch += 1;
        let lease = OwnerLease {
            activation,
            epoch: self.next_epoch,
            cancellation_epoch: 0,
        };
        self.owner = Some(lease);
        self.phase = TransactionPhase::Running;
        self.ending = false;
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
        if self.children.values().any(|joined| !joined) {
            return Err(CoordinationError::ChildOutstanding);
        }
        Ok(self.activate_unchecked(activation))
    }
    pub fn cancel(&mut self, owner: OwnerLease) -> Result<(), CoordinationError> {
        // A cancellation request revokes the live capability by advancing the
        // current cancellation epoch.  Child cleanup can fail transiently, so
        // the original owner must still be able to repeat that request and
        // finish joining the same recorded children.  Requiring the complete
        // lease here would turn the first cancellation into a stale-owner
        // error on retry, leaving structured termination unfinishable.
        self.require_owner_identity(owner)?;
        self.ending = true;
        let current = self.owner.as_mut().expect("checked");
        if current.cancellation_epoch == 0 {
            current.cancellation_epoch += 1;
        }
        self.phase = if self.children.values().any(|joined| !joined) {
            TransactionPhase::ChildrenJoining
        } else {
            TransactionPhase::RolledBack
        };
        Ok(())
    }
    /// Cancel and join all children before making this owner terminal.
    pub fn cancel_with_children<C: ChildSupervisor>(
        &mut self,
        owner: OwnerLease,
        children: &mut C,
    ) -> Result<(), CoordinationError> {
        self.cancel(owner)?;
        if self.join_unfinished_children(owner, children).is_err() {
            return Err(CoordinationError::ChildOutstanding);
        }
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
        // Cancellation revokes the capability to do more work, but the owner
        // identity remains valid long enough to acknowledge child cleanup.
        // Requiring a live lease here would make structured cancellation
        // impossible: unfinished children could never be joined.
        self.require_owner_identity(owner)?;
        let Some(joined) = self.children.get_mut(&child) else {
            return Err(CoordinationError::InvalidPhase);
        };
        *joined = true;
        if self
            .owner
            .is_some_and(|current| current.cancellation_epoch != 0)
            && self.children.values().all(|joined| *joined)
        {
            self.phase = TransactionPhase::RolledBack;
        }
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
            self.ending = true;
            self.phase = TransactionPhase::ChildrenJoining;
            return Outcome::ChildrenJoining {
                reason: RollbackReason::ChildOutstanding,
            };
        }
        self.execute_after_children(owner, provider, store, checkpoint, faults)
    }

    /// Execute an owner completion through a supervisor that can cancel and
    /// join each recorded unfinished child. The coordinator first fences child
    /// admission, then drains children in deterministic [`ChildId`] order,
    /// and only then permits its own provider and atomic publication.
    pub fn execute_with_children<
        T: WritePayload,
        P: ProviderExecutor<T>,
        S: AtomicCommitStore,
        F: FaultInjector,
        C: ChildSupervisor,
    >(
        &mut self,
        owner: OwnerLease,
        provider: &mut P,
        store: &mut S,
        checkpoint: CheckpointIntent,
        faults: &mut F,
        children: &mut C,
    ) -> Outcome {
        if self.require_live(owner).is_err() && !self.retrying_normal_completion(owner) {
            return self.rollback_for(owner);
        }
        self.ending = true;
        if self.join_unfinished_children(owner, children).is_err() {
            return Outcome::ChildrenJoining {
                reason: RollbackReason::ChildOutstanding,
            };
        }
        self.execute_after_children(owner, provider, store, checkpoint, faults)
    }

    fn execute_after_children<
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
        if self.require_committable(owner).is_err() {
            return self.rollback_for(owner);
        }
        self.phase = TransactionPhase::Prepared;
        if faults.check(FaultPoint::BeforeProvider).is_err() {
            return self.rollback(RollbackReason::FaultInjected);
        }
        let write = match provider.execute(owner.activation) {
            Ok(value) => TypedWrite::new(value).into_record(),
            Err(_) => return self.rollback(RollbackReason::ProviderFailed),
        };
        if self.require_committable(owner).is_err() {
            return self.rollback_for(owner);
        }
        if faults.check(FaultPoint::BeforeAtomicCommit).is_err() {
            return self.rollback(RollbackReason::FaultInjected);
        }
        // This final validation is adjacent to the only publication call.
        if self.require_committable(owner).is_err() {
            return self.rollback_for(owner);
        }
        match store.commit(
            CommitRequest {
                owner,
                write,
                checkpoint,
            },
            self,
        ) {
            Ok(receipt) => {
                self.phase = TransactionPhase::Committed;
                Outcome::Committed { receipt }
            }
            Err(_) => self.rollback(RollbackReason::StoreRejected),
        }
    }
    fn join_unfinished_children<C: ChildSupervisor>(
        &mut self,
        owner: OwnerLease,
        children: &mut C,
    ) -> Result<(), ChildTerminationError> {
        self.require_owner_identity(owner)
            .map_err(|_| ChildTerminationError::Rejected)?;
        self.phase = TransactionPhase::ChildrenJoining;
        let pending = self
            .children
            .iter()
            .filter_map(|(child, joined)| (!*joined).then_some(*child))
            .collect::<Vec<_>>();
        for child in pending {
            children.request_cancellation(child)?;
            children.join(child)?;
            *self.children.get_mut(&child).expect("recorded child") = true;
        }
        Ok(())
    }
    fn require_current(&self, owner: OwnerLease) -> Result<(), CoordinationError> {
        if self.owner == Some(owner) {
            Ok(())
        } else {
            Err(CoordinationError::StaleOwner)
        }
    }
    fn require_owner_identity(&self, owner: OwnerLease) -> Result<(), CoordinationError> {
        if self.owner.is_some_and(|current| {
            current.activation == owner.activation && current.epoch == owner.epoch
        }) {
            Ok(())
        } else {
            Err(CoordinationError::StaleOwner)
        }
    }
    fn require_live(&self, owner: OwnerLease) -> Result<(), CoordinationError> {
        self.require_current(owner)?;
        if owner.cancellation_epoch == 0 && !self.ending {
            Ok(())
        } else {
            Err(CoordinationError::Cancelled)
        }
    }
    fn require_committable(&self, owner: OwnerLease) -> Result<(), CoordinationError> {
        self.require_current(owner)?;
        if owner.cancellation_epoch == 0 {
            Ok(())
        } else {
            Err(CoordinationError::Cancelled)
        }
    }
    /// A transient join failure keeps a still-live owner in
    /// `ChildrenJoining`. Retrying normal completion may only resume that
    /// recorded drain; it cannot admit work or bypass the child set.
    fn retrying_normal_completion(&self, owner: OwnerLease) -> bool {
        self.owner == Some(owner)
            && owner.cancellation_epoch == 0
            && self.ending
            && self.phase == TransactionPhase::ChildrenJoining
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
        } else if self.children.values().any(|joined| !joined) {
            self.phase = TransactionPhase::ChildrenJoining;
            Outcome::RolledBack { reason }
        } else {
            self.rollback(reason)
        }
    }
}

impl OwnerFence for ActivationCoordinator {
    fn permits(&self, owner: OwnerLease) -> bool {
        self.require_committable(owner).is_ok()
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
    fn commit(
        &mut self,
        request: CommitRequest,
        owner_fence: &dyn OwnerFence,
    ) -> Result<CommitReceipt, StoreError> {
        if self.reject {
            return Err(StoreError::Rejected);
        }
        if !owner_fence.permits(request.owner) {
            return Err(StoreError::OwnerFenceRejected);
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
            partition: Some(component("0")),
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
    fn cancellation_waits_for_child_join_before_terminal_phase() {
        let (mut c, owner) = active();
        let child = c.spawn_child(owner).unwrap();

        c.cancel(owner).unwrap();
        assert_eq!(c.phase(), TransactionPhase::ChildrenJoining);
        assert!(c.join_child(owner, child).is_ok());
        assert_eq!(c.phase(), TransactionPhase::RolledBack);

        let mut store = InMemoryAtomicStore::default();
        let mut provider = Provider(Ok(Value(9)));
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
            Outcome::ChildrenJoining {
                reason: RollbackReason::ChildOutstanding
            }
        );
        assert!(store.visible().is_empty());
        c.join_child(owner, child).unwrap();
        assert_eq!(c.phase(), TransactionPhase::ChildrenJoining);
        assert!(store.visible().is_empty());
    }
}
