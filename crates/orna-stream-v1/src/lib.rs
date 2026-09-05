//! Bounded stream/checkpoint administration for Orna 1.0.
//!
//! This crate owns neither provider I/O nor table/function transaction execution.
//! A runtime owner obtains a [`CommitIntent`], atomically applies it through a
//! [`CheckpointBackend`], then performs any provider or execution work outside this
//! boundary.  A delivery can advance a checkpoint only through `Complete` or `Skip`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

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
    pub partition: Component,
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
            &self.position,
        )
            .cmp(&(
                &other.consumer,
                &other.source_format,
                &other.source,
                &other.partition_format,
                &other.partition,
                &other.position_format,
                &other.position,
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

    pub fn canonical(&self) -> String {
        format!(
            "delivery/v1|{}|{}|{}|{}|{}|{}|{}",
            self.consumer.canonical(),
            self.source_format.as_str(),
            self.source.as_str(),
            self.partition_format.as_str(),
            self.partition.as_str(),
            self.position_format.as_str(),
            self.position.token.as_str(),
        )
    }
}

/// An opaque provider-defined position.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
    pub partition: Component,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    ProviderUnavailable,
    DecodeRejected,
    ExecutionRejected,
    Cancelled,
    Internal,
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
    Acquire {
        delivery: DeliveryIdentity,
        expected: CheckpointPrecondition,
        purpose: LeasePurpose,
    },
    Fail {
        lease: DeliveryLease,
        diagnostic: SafeDiagnostic,
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

/// Deterministic in-memory backend for runtime integration and focused tests.
#[derive(Default)]
pub struct InMemoryCheckpointBackend {
    checkpoints: BTreeMap<CheckpointKey, Checkpoint>,
    failures: BTreeMap<FailureIdentity, FailureRecord>,
    leases: BTreeMap<DeliveryIdentity, DeliveryLease>,
    next_fence: BTreeMap<CheckpointKey, u64>,
}

impl InMemoryCheckpointBackend {
    fn checkpoint_mut(&mut self, key: &CheckpointKey) -> &mut Checkpoint {
        self.checkpoints
            .entry(key.clone())
            .or_insert_with(|| Checkpoint {
                key: key.clone(),
                version: 0,
                committed: None,
            })
    }

    fn checkpoint_matches(
        &mut self,
        key: &CheckpointKey,
        expected: &CheckpointPrecondition,
    ) -> bool {
        let checkpoint = self.checkpoint_mut(key);
        checkpoint.version == expected.version && checkpoint.committed == expected.committed
    }

    fn consume_lease(&mut self, lease: &DeliveryLease) -> Result<(), RejectReason> {
        match self.leases.get(&lease.delivery) {
            Some(current) if current == lease => {
                self.leases.remove(&lease.delivery);
                Ok(())
            }
            _ => Err(RejectReason::LeaseFenced),
        }
    }

    fn advance(
        &mut self,
        delivery: &DeliveryIdentity,
        expected: &CheckpointPrecondition,
    ) -> CommitResult {
        let key = delivery.checkpoint_key();
        if !self.checkpoint_matches(&key, expected) {
            return CommitResult::Rejected(RejectReason::StaleCheckpoint);
        }
        let checkpoint = self.checkpoint_mut(&key);
        checkpoint.committed = Some(delivery.successor.clone());
        checkpoint.version += 1;
        CommitResult::CheckpointAdvanced {
            checkpoint: checkpoint.clone(),
        }
    }
}

impl CheckpointBackend for InMemoryCheckpointBackend {
    fn apply(&mut self, intent: CommitIntent) -> CommitResult {
        match intent {
            CommitIntent::Acquire {
                delivery,
                expected,
                purpose,
            } => {
                let key = delivery.checkpoint_key();
                if !self.checkpoint_matches(&key, &expected) {
                    return CommitResult::Rejected(RejectReason::StaleCheckpoint);
                }
                if self.leases.contains_key(&delivery) {
                    return CommitResult::Rejected(RejectReason::LeaseAlreadyHeld);
                }
                if let Some(failure) = self.failures.get(&FailureIdentity(delivery.clone())) {
                    let allowed = matches!(
                        (purpose, failure.status),
                        (LeasePurpose::Deliver, FailureStatus::Retrying)
                            | (LeasePurpose::Skip, FailureStatus::Failed)
                    );
                    if !allowed {
                        return CommitResult::Rejected(RejectReason::RetryNotAllowed);
                    }
                } else if purpose == LeasePurpose::Skip {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                }
                let fence = self.next_fence.entry(key).or_insert(0);
                *fence += 1;
                let lease = DeliveryLease {
                    delivery: delivery.clone(),
                    fence: *fence,
                    purpose,
                };
                self.leases.insert(delivery, lease.clone());
                CommitResult::Acquired { lease }
            }
            CommitIntent::Fail { lease, diagnostic } => {
                if lease.purpose != LeasePurpose::Deliver {
                    return CommitResult::Rejected(RejectReason::LeaseFenced);
                }
                if let Err(reason) = self.consume_lease(&lease) {
                    return CommitResult::Rejected(reason);
                }
                let identity = FailureIdentity(lease.delivery);
                let failure = self
                    .failures
                    .entry(identity.clone())
                    .or_insert(FailureRecord {
                        identity,
                        version: 0,
                        attempts: 0,
                        status: FailureStatus::Failed,
                        diagnostic,
                    });
                failure.version += 1;
                failure.attempts += 1;
                failure.status = FailureStatus::Failed;
                failure.diagnostic = diagnostic;
                CommitResult::Failed {
                    failure: failure.clone(),
                }
            }
            CommitIntent::Retry {
                failure,
                expected_version,
                expected,
            } => {
                if !self.checkpoint_matches(&failure.0.checkpoint_key(), &expected) {
                    return CommitResult::Rejected(RejectReason::StaleCheckpoint);
                }
                let Some(record) = self.failures.get_mut(&failure) else {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                };
                if record.version != expected_version {
                    return CommitResult::Rejected(RejectReason::StaleFailure);
                }
                if record.status != FailureStatus::Failed {
                    return CommitResult::Rejected(RejectReason::RetryNotAllowed);
                }
                record.version += 1;
                record.status = FailureStatus::Retrying;
                CommitResult::RetryScheduled {
                    failure: record.clone(),
                }
            }
            CommitIntent::Complete { lease, expected } => {
                if lease.purpose != LeasePurpose::Deliver {
                    return CommitResult::Rejected(RejectReason::LeaseFenced);
                }
                let key = lease.delivery.checkpoint_key();
                if !self.checkpoint_matches(&key, &expected) {
                    return CommitResult::Rejected(RejectReason::StaleCheckpoint);
                }
                if let Err(reason) = self.consume_lease(&lease) {
                    return CommitResult::Rejected(reason);
                }
                let delivery = lease.delivery;
                let result = self.advance(&delivery, &expected);
                if matches!(result, CommitResult::CheckpointAdvanced { .. })
                    && let Some(record) = self.failures.get_mut(&FailureIdentity(delivery))
                {
                    record.version += 1;
                    record.status = FailureStatus::Succeeded;
                }
                result
            }
            CommitIntent::Skip {
                lease,
                expected,
                expected_failure_version,
            } => {
                if lease.purpose != LeasePurpose::Skip {
                    return CommitResult::Rejected(RejectReason::LeaseFenced);
                }
                let identity = FailureIdentity(lease.delivery.clone());
                let Some(record) = self.failures.get(&identity) else {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                };
                if record.version != expected_failure_version
                    || record.status != FailureStatus::Failed
                {
                    return CommitResult::Rejected(RejectReason::StaleFailure);
                }
                let key = lease.delivery.checkpoint_key();
                if !self.checkpoint_matches(&key, &expected) {
                    return CommitResult::Rejected(RejectReason::StaleCheckpoint);
                }
                if let Err(reason) = self.consume_lease(&lease) {
                    return CommitResult::Rejected(reason);
                }
                let result = self.advance(&lease.delivery, &expected);
                if matches!(result, CommitResult::CheckpointAdvanced { .. }) {
                    let record = self.failures.get_mut(&identity).expect("checked above");
                    record.version += 1;
                    record.status = FailureStatus::Skipped;
                }
                result
            }
            CommitIntent::Replay {
                failure,
                expected_version,
            } => {
                let Some(record) = self.failures.get_mut(&failure) else {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                };
                if record.version != expected_version {
                    return CommitResult::Rejected(RejectReason::StaleFailure);
                }
                if record.status != FailureStatus::Skipped {
                    return CommitResult::Rejected(RejectReason::RetryNotAllowed);
                }
                record.version += 1;
                record.status = FailureStatus::Replaying;
                CommitResult::ReplayGranted {
                    grant: ReplayGrant {
                        failure,
                        version: record.version,
                    },
                }
            }
            CommitIntent::ReplayComplete {
                failure,
                expected_version,
            } => {
                let Some(record) = self.failures.get_mut(&failure) else {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                };
                if record.version != expected_version {
                    return CommitResult::Rejected(RejectReason::StaleFailure);
                }
                if record.status != FailureStatus::Replaying {
                    return CommitResult::Rejected(RejectReason::RetryNotAllowed);
                }
                record.version += 1;
                record.status = FailureStatus::Replayed;
                CommitResult::ReplayCompleted {
                    failure: record.clone(),
                }
            }
            CommitIntent::ReplayFail {
                failure,
                expected_version,
                diagnostic,
            } => {
                let Some(record) = self.failures.get_mut(&failure) else {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                };
                if record.version != expected_version {
                    return CommitResult::Rejected(RejectReason::StaleFailure);
                }
                if record.status != FailureStatus::Replaying {
                    return CommitResult::Rejected(RejectReason::RetryNotAllowed);
                }
                record.version += 1;
                record.attempts += 1;
                record.status = FailureStatus::Skipped;
                record.diagnostic = diagnostic;
                CommitResult::ReplayFailed {
                    failure: record.clone(),
                }
            }
            CommitIntent::Resolve {
                failure,
                expected_version,
            } => {
                let Some(record) = self.failures.get_mut(&failure) else {
                    return CommitResult::Rejected(RejectReason::FailureMissing);
                };
                if record.version != expected_version {
                    return CommitResult::Rejected(RejectReason::StaleFailure);
                }
                if !matches!(
                    record.status,
                    FailureStatus::Succeeded | FailureStatus::Skipped | FailureStatus::Replayed
                ) {
                    return CommitResult::Rejected(RejectReason::ResolveBlocked);
                }
                record.version += 1;
                record.status = FailureStatus::Resolved;
                CommitResult::Resolved {
                    failure: record.clone(),
                }
            }
            CommitIntent::Cancel { lease } => {
                if let Err(reason) = self.consume_lease(&lease) {
                    return CommitResult::Rejected(reason);
                }
                let identity = FailureIdentity(lease.delivery.clone());
                if let Some(record) = self.failures.get_mut(&identity)
                    && record.status == FailureStatus::Retrying
                {
                    record.version += 1;
                    record.status = FailureStatus::Failed;
                }
                CommitResult::Cancelled {
                    checkpoint: self.checkpoint(&lease.delivery.checkpoint_key()),
                    classification: CancellationClassification::RollbackShaped,
                }
            }
        }
    }

    fn checkpoint(&self, key: &CheckpointKey) -> Checkpoint {
        self.checkpoints
            .get(key)
            .cloned()
            .unwrap_or_else(|| Checkpoint {
                key: key.clone(),
                version: 0,
                committed: None,
            })
    }

    fn failure(&self, identity: &FailureIdentity) -> Option<&FailureRecord> {
        self.failures.get(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(value: &str) -> Component {
        Component::new(value).unwrap()
    }
    fn position(token: &str) -> Position {
        Position {
            token: component(token),
        }
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
            partition: component("partition-a"),
            position_format: component("offset-v1"),
            position: position(token),
            successor: position(successor),
        }
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
    fn diagnostics_are_redacted() {
        let printed = format!("{:?}", diagnostic());
        assert!(!printed.contains("password"));
        assert!(!printed.contains("token="));
        assert!(!printed.contains("secret"));
    }

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

        let second = delivery("receipt:one", "resume:two");
        let failed = acquire_and_fail(&mut backend, second.clone());
        let _ = backend.apply(CommitIntent::Retry {
            failure: failed.identity.clone(),
            expected_version: failed.version,
            expected: expected(&backend, &second),
        });
        let failed_again = acquire_and_fail(&mut backend, second);
        assert_eq!(failed_again.status, FailureStatus::Failed);
        assert_eq!(failed_again.attempts, 2);
        assert!(scheduled.version < backend.failure(&FailureIdentity(item)).unwrap().version);
    }

    #[test]
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
    fn replay_failure_updates_the_stable_failure_attempt_count() {
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
        let _ = backend.apply(CommitIntent::Retry {
            failure: failure.identity.clone(),
            expected_version: failure.version,
            expected: expected(&backend, &item),
        });
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
    fn successor_drift_updates_one_stable_failure_row() {
        let mut backend = InMemoryCheckpointBackend::default();
        let first = delivery("receipt:stable", "resume:first");
        let failed = acquire_and_fail(&mut backend, first.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Retry {
                failure: failed.identity.clone(),
                expected_version: failed.version,
                expected: expected(&backend, &first),
            }),
            CommitResult::RetryScheduled { .. }
        ));

        let same_delivery = delivery("receipt:stable", "resume:changed");
        let failed_again = acquire_and_fail(&mut backend, same_delivery);
        assert_eq!(failed_again.identity, failed.identity);
        assert_eq!(failed_again.attempts, 2);
    }

    #[test]
    fn retry_rejects_a_stale_blocking_checkpoint() {
        let mut backend = InMemoryCheckpointBackend::default();
        let failed_delivery = delivery("receipt:failed", "resume:after-failed");
        let failure = acquire_and_fail(&mut backend, failed_delivery.clone());
        let failed_expected = expected(&backend, &failed_delivery);

        let later_delivery = delivery("receipt:later", "resume:after-later");
        let lease = acquire(&mut backend, later_delivery.clone());
        assert!(matches!(
            backend.apply(CommitIntent::Complete {
                lease,
                expected: expected(&backend, &later_delivery),
            }),
            CommitResult::CheckpointAdvanced { .. }
        ));

        assert_eq!(
            backend.apply(CommitIntent::Retry {
                failure: failure.identity.clone(),
                expected_version: failure.version,
                expected: failed_expected,
            }),
            CommitResult::Rejected(RejectReason::StaleCheckpoint)
        );
        assert_eq!(backend.failure(&failure.identity).unwrap(), &failure);
    }

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
