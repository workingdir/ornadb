//! Authenticated SERVER resource producer interface.

use super::resource_producer::{ResourceProducerCommand, ResourceProducerPull};
use super::*;
pub(super) const MAX_RESOURCE_CREDIT: u64 = 1024 * 1024 * 1024;

/// The owned result of one authenticated SERVER resource request.
///
/// A successful result contains the server-generated nested invocation identity
/// and only values validated against the active SERVER target. A failed result
/// carries the closed protocol failure class and no target, principal, grant,
/// argument, or internal error detail.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedServerResourceResult {
    /// The target executed and produced its complete value sequence.
    Completed {
        /// The connection-local resource stream.
        stream_id: u64,
        /// The caller's request correlation identity.
        request_id: InvocationId,
        /// The server-generated nested invocation identity.
        nested_invocation_id: InvocationId,
        /// The active revision pair used for execution.
        target_revision: RevisionPair,
        /// The validated resource result kind.
        resource_kind: ProtocolResourceKind,
        /// Values in server result order. A scalar has exactly one value.
        values: Vec<RuntimeValue>,
    },
    /// The request was denied or could not safely execute.
    Failed {
        /// The connection-local resource stream.
        stream_id: u64,
        /// The caller's request correlation identity.
        request_id: InvocationId,
        /// The closed public failure class.
        failure: CallFailure,
    },
}

/// The local resource kind retained by an authenticated producer.
///
/// This deliberately does not expose the wire protocol's resource enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticatedServerResourceKind {
    /// One scalar result item.
    Single,
    /// A bounded sequence of result items.
    Stream,
}

/// The metadata established before a resource producer is accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedServerResourceAccepted {
    pub stream_id: u64,
    pub request_id: InvocationId,
    pub nested_invocation_id: InvocationId,
    pub target_revision: RevisionPair,
    pub resource_kind: AuthenticatedServerResourceKind,
}

/// Checked item and byte credit for one producer pull.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceCredit {
    pub item_count: u64,
    pub byte_count: u64,
}

impl ResourceCredit {
    /// Creates non-zero bounded credit.
    pub fn new(item_count: u64, byte_count: u64) -> Option<Self> {
        (item_count != 0
            && byte_count != 0
            && item_count <= MAX_RESOURCE_CREDIT
            && byte_count <= MAX_RESOURCE_CREDIT)
            .then_some(Self {
                item_count,
                byte_count,
            })
    }
}

/// The producer result of one pull command.
#[derive(Clone, Debug, PartialEq)]
pub enum AuthenticatedServerResourceEvent {
    /// One bounded batch of decoded values.
    Values {
        batch_sequence: u64,
        item_count: u64,
        byte_count: u64,
        values: Vec<RuntimeValue>,
    },
    /// The transaction committed after all rows were consumed.
    Completed {
        final_batch_sequence: u64,
        total_items: u64,
        total_bytes: u64,
    },
    /// A redacted execution failure.
    Failed { failure: CallFailure },
    /// The transaction rolled back after cancellation won.
    Cancelled,
    /// The pulled row requires more byte credit and remains pending.
    Waiting { required_bytes: u64 },
}

/// The result of starting an authenticated SERVER resource producer.
#[derive(Debug)]
pub enum AuthenticatedServerResourceStart {
    /// Security and plan validation succeeded; the producer is live.
    Accepted(AuthenticatedServerResourceProducer),
    /// The request failed before acceptance and carries only its redacted class.
    Failed {
        stream_id: u64,
        request_id: InvocationId,
        failure: CallFailure,
    },
}

/// A command-driven producer whose transaction and PostgreSQL row stream are
/// owned by its task.
///
/// Dropping an abandoned producer requests cancellation. The worker remains
/// responsible for terminal audit and transaction ordering, including when the
/// caller drops the producer before it receives a terminal event.
pub struct AuthenticatedServerResourceProducer {
    pub(super) accepted: AuthenticatedServerResourceAccepted,
    pub(super) commands: tokio::sync::mpsc::Sender<ResourceProducerCommand>,
    pub(super) cancellation: ResourceCancellation,
}

impl AuthenticatedServerResourceProducer {
    /// Returns the immutable acceptance metadata.
    pub fn accepted(&self) -> AuthenticatedServerResourceAccepted {
        self.accepted
    }

    /// Requests one bounded batch or terminal result.
    ///
    /// This compatibility surface represents cancellation-caused channel
    /// closure as `Cancelled`. Lifecycle owners that require proof of an
    /// explicit worker acknowledgement must call [`Self::pull_observed`].
    pub async fn pull(
        &self,
        credit: ResourceCredit,
    ) -> Result<AuthenticatedServerResourceEvent, PostgresKernelError> {
        Ok(self.pull_observed(credit).await?.0)
    }

    /// Requests one bounded batch or terminal result with cancellation
    /// acknowledgement provenance.
    pub async fn pull_observed(
        &self,
        credit: ResourceCredit,
    ) -> Result<(AuthenticatedServerResourceEvent, bool), PostgresKernelError> {
        let (response, receiver) = tokio::sync::oneshot::channel();
        // Cancellation may close either channel after a pull is queued. Keep
        // the legacy cancellation surface while retaining the absence of a
        // worker acknowledgement for lifecycle owners.
        if self
            .commands
            .send(ResourceProducerCommand::Pull(ResourceProducerPull {
                credit,
                response,
            }))
            .await
            .is_err()
        {
            if self.cancellation.is_requested() {
                return Ok((AuthenticatedServerResourceEvent::Cancelled, false));
            }
            return Err(PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: self.accepted.request_id.canonical(),
                rule: "producer task terminated before pull response",
            });
        }
        match receiver.await {
            Ok(result) => result.map(|event| {
                let cancellation_acknowledged =
                    matches!(event, AuthenticatedServerResourceEvent::Cancelled);
                (event, cancellation_acknowledged)
            }),
            Err(_) if self.cancellation.is_requested() => {
                Ok((AuthenticatedServerResourceEvent::Cancelled, false))
            }
            Err(_) => Err(PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: self.accepted.request_id.canonical(),
                rule: "producer task dropped pull response",
            }),
        }
    }

    /// Requests cancellation and reports whether this call won the race.
    pub fn cancel(&self) -> bool {
        self.cancellation.request_cancel()
    }
}

impl Drop for AuthenticatedServerResourceProducer {
    fn drop(&mut self) {
        self.cancellation.request_cancel();
    }
}

impl std::fmt::Debug for AuthenticatedServerResourceProducer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedServerResourceProducer")
            .field("accepted", &self.accepted)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{CatalogueRevisionId, SourceRevisionId};

    fn producer(
        commands: tokio::sync::mpsc::Sender<ResourceProducerCommand>,
        cancellation: ResourceCancellation,
    ) -> AuthenticatedServerResourceProducer {
        AuthenticatedServerResourceProducer {
            accepted: AuthenticatedServerResourceAccepted {
                stream_id: 1,
                request_id: InvocationId::from_bytes([0x41; 16]),
                nested_invocation_id: InvocationId::from_bytes([0x42; 16]),
                target_revision: RevisionPair::new(
                    SourceRevisionId::from_bytes([0x43; 16]),
                    CatalogueRevisionId::from_bytes([0x44; 16]),
                ),
                resource_kind: AuthenticatedServerResourceKind::Stream,
            },
            commands,
            cancellation,
        }
    }

    #[tokio::test]
    async fn observed_pull_marks_worker_cancelled_event_as_acknowledged() {
        let cancellation = ResourceCancellation::new();
        let (commands, mut worker) = tokio::sync::mpsc::channel(1);
        let producer = producer(commands, cancellation.clone());
        assert!(cancellation.request_cancel());

        let pull = tokio::spawn(async move {
            producer
                .pull_observed(ResourceCredit::new(1, 1).expect("bounded credit"))
                .await
        });
        let Some(ResourceProducerCommand::Pull(command)) = worker.recv().await else {
            panic!("producer sends a pull command");
        };
        command
            .response
            .send(Ok(AuthenticatedServerResourceEvent::Cancelled))
            .expect("worker response remains open");

        let outcome = pull
            .await
            .expect("pull task joins")
            .expect("worker response succeeds");
        assert_eq!(outcome, (AuthenticatedServerResourceEvent::Cancelled, true));
    }

    #[tokio::test]
    async fn observed_pull_marks_closed_response_as_unacknowledged_cancellation() {
        let cancellation = ResourceCancellation::new();
        let (commands, mut worker) = tokio::sync::mpsc::channel(1);
        let producer = producer(commands, cancellation.clone());
        assert!(cancellation.request_cancel());

        let pull = tokio::spawn(async move {
            producer
                .pull_observed(ResourceCredit::new(1, 1).expect("bounded credit"))
                .await
        });
        let Some(ResourceProducerCommand::Pull(command)) = worker.recv().await else {
            panic!("producer sends a pull command");
        };
        drop(command);

        let outcome = pull
            .await
            .expect("pull task joins")
            .expect("closed response preserves cancellation outcome");
        assert_eq!(
            outcome,
            (AuthenticatedServerResourceEvent::Cancelled, false)
        );
    }

    #[tokio::test]
    async fn legacy_pull_preserves_synthetic_cancelled_event() {
        let cancellation = ResourceCancellation::new();
        let (commands, worker) = tokio::sync::mpsc::channel(1);
        drop(worker);
        let producer = producer(commands, cancellation.clone());
        assert!(cancellation.request_cancel());

        assert_eq!(
            producer
                .pull(ResourceCredit::new(1, 1).expect("bounded credit"))
                .await
                .expect("cancelled channel remains legacy cancellation"),
            AuthenticatedServerResourceEvent::Cancelled
        );
    }
}
