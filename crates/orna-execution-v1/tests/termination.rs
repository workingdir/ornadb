use orna_execution_v1::{
    ActivationCoordinator, ActivationId, AtomicCommitStore, CheckpointIntent, ChildId,
    ChildSupervisor, ChildTerminationError, CommitReceipt, CommitRequest, NoFault, Outcome,
    OwnerFence, OwnerLease, ProviderError, ProviderExecutor, RollbackReason, StoreError,
    TransactionPhase, WritePayload, WriteRecord,
};
use orna_stream_v1::{
    CheckpointPrecondition, CommitIntent, Component, ConsumerIdentity, DeliveryIdentity,
    DeliveryLease, LeasePurpose, Position,
};

#[derive(Clone)]
struct Value(u8);

impl WritePayload for Value {
    const KIND: &'static str = "test/value";

    fn encode(&self) -> Vec<u8> {
        vec![self.0]
    }
}

struct Provider;

impl ProviderExecutor<Value> for Provider {
    fn execute(&mut self, _: ActivationId) -> Result<Value, ProviderError> {
        Ok(Value(7))
    }
}

struct NeverProvider;

impl ProviderExecutor<Value> for NeverProvider {
    fn execute(&mut self, _: ActivationId) -> Result<Value, ProviderError> {
        panic!("a committed owner must not re-enter provider execution")
    }
}

struct RejectedProvider;

impl ProviderExecutor<Value> for RejectedProvider {
    fn execute(&mut self, _: ActivationId) -> Result<Value, ProviderError> {
        Err(ProviderError::Rejected)
    }
}

#[derive(Default)]
struct Store {
    commits: usize,
}

impl AtomicCommitStore for Store {
    fn commit(
        &mut self,
        request: CommitRequest,
        owner_fence: &dyn OwnerFence,
    ) -> Result<CommitReceipt, StoreError> {
        if !owner_fence.permits(request.owner) {
            return Err(StoreError::OwnerFenceRejected);
        }
        self.commits += 1;
        Ok(CommitReceipt {
            sequence: self.commits as u64,
        })
    }
}

#[derive(Default)]
struct Supervisor {
    events: Vec<(char, ChildId)>,
    fail_join_once: Option<ChildId>,
}

impl ChildSupervisor for Supervisor {
    fn request_cancellation(&mut self, child: ChildId) -> Result<(), ChildTerminationError> {
        self.events.push(('c', child));
        Ok(())
    }

    fn join(&mut self, child: ChildId) -> Result<(), ChildTerminationError> {
        self.events.push(('j', child));
        if self.fail_join_once == Some(child) {
            self.fail_join_once = None;
            Err(ChildTerminationError::Incomplete)
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
    CheckpointIntent {
        commit: CommitIntent::Complete {
            lease: DeliveryLease {
                delivery,
                fence: 1,
                purpose: LeasePurpose::Deliver,
            },
            expected: CheckpointPrecondition {
                version: 0,
                committed: None,
            },
        },
    }
}

fn active() -> (ActivationCoordinator, OwnerLease) {
    let mut coordinator = ActivationCoordinator::default();
    let owner = coordinator.activate(ActivationId::new(1).unwrap());
    (coordinator, owner)
}

#[test]
fn ending_owner_fences_new_child_admission() {
    let (mut coordinator, owner) = active();
    coordinator.spawn_child(owner).unwrap();

    coordinator.cancel(owner).unwrap();

    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);
    assert!(coordinator.spawn_child(owner).is_err());
    assert!(
        coordinator
            .try_activate(ActivationId::new(2).unwrap())
            .is_err()
    );
    assert!(
        coordinator
            .replace_stale(owner, ActivationId::new(2).unwrap())
            .is_err()
    );
}

#[test]
fn success_cancels_and_joins_children_in_child_id_order_before_commit() {
    let (mut coordinator, owner) = active();
    let first = coordinator.spawn_child(owner).unwrap();
    let second = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor::default();
    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert!(matches!(
        coordinator.execute_with_children(
            owner,
            &mut provider,
            &mut store,
            checkpoint(),
            &mut faults,
            &mut supervisor,
        ),
        Outcome::Committed { .. }
    ));
    assert_eq!(
        supervisor.events,
        vec![('c', first), ('j', first), ('c', second), ('j', second)]
    );
    assert_eq!(store.commits, 1);
    assert_eq!(coordinator.phase(), TransactionPhase::Committed);
}

#[test]
fn committed_owner_cannot_be_cancelled_or_relabelled_as_rolled_back() {
    let (mut coordinator, owner) = active();
    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert!(matches!(
        coordinator.execute(owner, &mut provider, &mut store, checkpoint(), &mut faults,),
        Outcome::Committed { .. }
    ));
    assert_eq!(coordinator.phase(), TransactionPhase::Committed);

    assert_eq!(
        coordinator.cancel(owner),
        Err(orna_execution_v1::CoordinationError::InvalidPhase)
    );
    assert_eq!(coordinator.phase(), TransactionPhase::Committed);
}

#[test]
fn committed_owner_cannot_execute_publish_or_pass_its_fence() {
    let (mut coordinator, owner) = active();
    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert!(matches!(
        coordinator.execute(owner, &mut provider, &mut store, checkpoint(), &mut faults,),
        Outcome::Committed { .. }
    ));
    assert_eq!(store.commits, 1);
    assert!(!coordinator.permits(owner));
    assert_eq!(
        store.commit(stale_commit(owner), &coordinator),
        Err(StoreError::OwnerFenceRejected)
    );

    let mut never_provider = NeverProvider;
    assert_eq!(
        coordinator.execute(
            owner,
            &mut never_provider,
            &mut store,
            checkpoint(),
            &mut faults,
        ),
        Outcome::RolledBack {
            reason: RollbackReason::Cancelled,
        }
    );
    assert_eq!(store.commits, 1);
    assert_eq!(coordinator.phase(), TransactionPhase::Committed);
}

#[test]
fn rolled_back_owner_cannot_execute_publish_or_pass_its_fence() {
    let (mut coordinator, owner) = active();
    let mut rejected_provider = RejectedProvider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert_eq!(
        coordinator.execute(
            owner,
            &mut rejected_provider,
            &mut store,
            checkpoint(),
            &mut faults,
        ),
        Outcome::RolledBack {
            reason: RollbackReason::ProviderFailed,
        }
    );
    assert_eq!(coordinator.phase(), TransactionPhase::RolledBack);
    assert!(!coordinator.permits(owner));
    assert_eq!(
        store.commit(stale_commit(owner), &coordinator),
        Err(StoreError::OwnerFenceRejected)
    );

    let mut never_provider = NeverProvider;
    assert_eq!(
        coordinator.execute(
            owner,
            &mut never_provider,
            &mut store,
            checkpoint(),
            &mut faults,
        ),
        Outcome::RolledBack {
            reason: RollbackReason::Cancelled,
        }
    );
    assert_eq!(store.commits, 0);
    assert_eq!(coordinator.phase(), TransactionPhase::RolledBack);
}

#[test]
fn children_joining_owner_cannot_execute_or_pass_its_fence_before_retrying_cleanup() {
    let (mut coordinator, owner) = active();
    let child = coordinator.spawn_child(owner).unwrap();
    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert_eq!(
        coordinator.execute(owner, &mut provider, &mut store, checkpoint(), &mut faults),
        Outcome::ChildrenJoining {
            reason: RollbackReason::ChildOutstanding,
        }
    );
    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);
    assert!(!coordinator.permits(owner));
    assert_eq!(
        store.commit(stale_commit(owner), &coordinator),
        Err(StoreError::OwnerFenceRejected)
    );

    let mut never_provider = NeverProvider;
    assert_eq!(
        coordinator.execute(
            owner,
            &mut never_provider,
            &mut store,
            checkpoint(),
            &mut faults,
        ),
        Outcome::RolledBack {
            reason: RollbackReason::Cancelled,
        }
    );
    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);
    assert_eq!(store.commits, 0);

    let mut supervisor = Supervisor::default();
    assert!(matches!(
        coordinator.execute_with_children(
            owner,
            &mut provider,
            &mut store,
            checkpoint(),
            &mut faults,
            &mut supervisor,
        ),
        Outcome::Committed { .. }
    ));
    assert_eq!(supervisor.events, vec![('c', child), ('j', child)]);
    assert_eq!(store.commits, 1);
    assert_eq!(coordinator.phase(), TransactionPhase::Committed);
}

#[test]
fn incomplete_join_keeps_owner_nonterminal_and_prevents_publication() {
    let (mut coordinator, owner) = active();
    let child = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor {
        fail_join_once: Some(child),
        ..Supervisor::default()
    };
    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert_eq!(
        coordinator.execute_with_children(
            owner,
            &mut provider,
            &mut store,
            checkpoint(),
            &mut faults,
            &mut supervisor,
        ),
        Outcome::ChildrenJoining {
            reason: RollbackReason::ChildOutstanding,
        }
    );
    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);
    assert_eq!(store.commits, 0);
}

#[test]
fn cancelled_owner_retries_child_join_without_regaining_publication() {
    let (mut coordinator, owner) = active();
    let child = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor {
        fail_join_once: Some(child),
        ..Supervisor::default()
    };

    assert_eq!(
        coordinator.cancel_with_children(owner, &mut supervisor),
        Err(orna_execution_v1::CoordinationError::ChildOutstanding)
    );
    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);

    // The same pre-cancellation lease may complete the already-requested
    // structured cleanup, but it remains unable to publish afterwards.
    coordinator
        .cancel_with_children(owner, &mut supervisor)
        .unwrap();
    assert_eq!(coordinator.phase(), TransactionPhase::RolledBack);
    assert_eq!(
        supervisor.events,
        vec![('c', child), ('j', child), ('c', child), ('j', child)]
    );

    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;
    assert_eq!(
        coordinator.execute(owner, &mut provider, &mut store, checkpoint(), &mut faults),
        Outcome::RolledBack {
            reason: RollbackReason::Cancelled,
        }
    );
    assert_eq!(store.commits, 0);
}

#[test]
fn transient_partial_join_failure_retries_normal_completion_without_publication() {
    let (mut coordinator, owner) = active();
    let first = coordinator.spawn_child(owner).unwrap();
    let second = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor {
        fail_join_once: Some(second),
        ..Supervisor::default()
    };
    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;

    assert_eq!(
        coordinator.execute_with_children(
            owner,
            &mut provider,
            &mut store,
            checkpoint(),
            &mut faults,
            &mut supervisor,
        ),
        Outcome::ChildrenJoining {
            reason: RollbackReason::ChildOutstanding,
        }
    );
    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);
    assert_eq!(store.commits, 0);

    assert!(matches!(
        coordinator.execute_with_children(
            owner,
            &mut provider,
            &mut store,
            checkpoint(),
            &mut faults,
            &mut supervisor,
        ),
        Outcome::Committed { .. }
    ));
    assert_eq!(
        supervisor.events,
        vec![
            ('c', first),
            ('j', first),
            ('c', second),
            ('j', second),
            ('c', second),
            ('j', second),
        ]
    );
    assert_eq!(store.commits, 1);
    assert_eq!(coordinator.phase(), TransactionPhase::Committed);
}

#[test]
fn acknowledged_cancellation_fences_late_publication() {
    let (mut coordinator, owner) = active();
    let child = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor::default();

    coordinator
        .cancel_with_children(owner, &mut supervisor)
        .unwrap();

    let mut provider = Provider;
    let mut store = Store::default();
    let mut faults = NoFault;
    assert_eq!(
        coordinator.execute(owner, &mut provider, &mut store, checkpoint(), &mut faults),
        Outcome::RolledBack {
            reason: RollbackReason::Cancelled,
        }
    );
    assert_eq!(supervisor.events, vec![('c', child), ('j', child)]);
    assert_eq!(store.commits, 0);
}

fn stale_commit(owner: OwnerLease) -> CommitRequest {
    CommitRequest {
        owner,
        write: WriteRecord {
            kind: Value::KIND,
            bytes: vec![9],
        },
        checkpoint: checkpoint(),
    }
}

#[test]
fn store_refuses_stale_and_revoked_owner_fences() {
    let (mut coordinator, owner) = active();
    let mut store = Store::default();

    coordinator
        .replace_stale(owner, ActivationId::new(2).unwrap())
        .unwrap();

    assert_eq!(
        store.commit(stale_commit(owner), &coordinator),
        Err(StoreError::OwnerFenceRejected)
    );
    assert_eq!(store.commits, 0);

    let (mut coordinator, owner) = active();

    coordinator.cancel(owner).unwrap();

    assert_eq!(
        store.commit(stale_commit(owner), &coordinator),
        Err(StoreError::OwnerFenceRejected)
    );
    assert_eq!(store.commits, 0);
}
