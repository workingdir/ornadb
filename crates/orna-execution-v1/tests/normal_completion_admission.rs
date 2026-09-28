use orna_execution_v1::{
    ActivationCoordinator, ActivationId, AtomicCommitStore, CheckpointIntent, ChildId,
    ChildSupervisor, ChildTerminationError, CommitReceipt, CommitRequest, NoFault,
    Outcome, OwnerFence, ProviderError, ProviderExecutor, RollbackReason, StoreError,
    TransactionPhase, WritePayload,
};
use orna_stream_v1::{
    CheckpointPrecondition, CommitIntent, Component, ConsumerIdentity, DeliveryIdentity,
    DeliveryLease, LeasePurpose, Position,
};

struct Value;

impl WritePayload for Value {
    const KIND: &'static str = "test/value";

    fn encode(&self) -> Vec<u8> {
        vec![1]
    }
}

#[derive(Default)]
struct Provider {
    calls: usize,
}

impl ProviderExecutor<Value> for Provider {
    fn execute(&mut self, _: ActivationId) -> Result<Value, ProviderError> {
        self.calls += 1;
        Ok(Value)
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

#[test]
fn transient_normal_completion_join_failure_keeps_child_admission_fenced() {
    let mut coordinator = ActivationCoordinator::default();
    let owner = coordinator.activate(ActivationId::new(1).unwrap());
    let child = coordinator.spawn_child(owner).unwrap();
    let mut supervisor = Supervisor {
        fail_join_once: Some(child),
        ..Supervisor::default()
    };
    let mut provider = Provider::default();
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
    assert_eq!(provider.calls, 0);
    assert_eq!(store.commits, 0);

    assert!(coordinator.spawn_child(owner).is_err());
    assert_eq!(coordinator.phase(), TransactionPhase::ChildrenJoining);
    assert!(coordinator.try_activate(ActivationId::new(2).unwrap()).is_err());

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
    assert_eq!(provider.calls, 1);
    assert_eq!(store.commits, 1);
    assert_eq!(
        supervisor.events,
        vec![('c', child), ('j', child), ('c', child), ('j', child)]
    );
}
