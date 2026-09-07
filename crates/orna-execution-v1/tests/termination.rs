use orna_execution_v1::{
    ActivationCoordinator, ActivationId, AtomicCommitStore, CheckpointIntent, ChildId,
    ChildSupervisor, ChildTerminationError, CommitReceipt, CommitRequest, NoFault, Outcome,
    OwnerLease, ProviderError, ProviderExecutor, RollbackReason, StoreError, TransactionPhase,
    WritePayload,
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

#[derive(Default)]
struct Store {
    commits: usize,
}

impl AtomicCommitStore for Store {
    fn commit(&mut self, _: CommitRequest) -> Result<CommitReceipt, StoreError> {
        self.commits += 1;
        Ok(CommitReceipt {
            sequence: self.commits as u64,
        })
    }
}

#[derive(Default)]
struct Supervisor {
    events: Vec<(char, ChildId)>,
    fail_join: Option<ChildId>,
}

impl ChildSupervisor for Supervisor {
    fn request_cancellation(&mut self, child: ChildId) -> Result<(), ChildTerminationError> {
        self.events.push(('c', child));
        Ok(())
    }

    fn join(&mut self, child: ChildId) -> Result<(), ChildTerminationError> {
        self.events.push(('j', child));
        if self.fail_join == Some(child) {
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
        partition: component("0"),
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
fn incomplete_join_keeps_owner_nonterminal_and_prevents_publication() {
    let (mut coordinator, owner) = active();
    let child = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor {
        fail_join: Some(child),
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
