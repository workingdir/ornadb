use super::*;

pub(super) enum ResourceProducerReady {
    Accepted(AuthenticatedServerResourceAccepted),
    Failed {
        stream_id: u64,
        request_id: InvocationId,
        failure: CallFailure,
    },
}

/// Requests cancellation if startup is dropped before the worker publishes
/// acceptance or a pre-acceptance failure. The worker must not be aborted here:
/// it owns the reserved request finalizer.
pub(super) struct ResourceProducerStartGuard {
    cancellation: ResourceCancellation,
    armed: bool,
}

impl ResourceProducerStartGuard {
    pub(super) fn new(cancellation: ResourceCancellation) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ResourceProducerStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.request_cancel();
        }
    }
}

/// Internal command sent to the task which owns the transaction.
#[derive(Debug)]
pub(crate) enum ResourceProducerCommand {
    Pull(ResourceProducerPull),
}

#[derive(Debug)]
pub(crate) struct ResourceProducerPull {
    pub(crate) credit: ResourceCredit,
    pub(crate) response:
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
}

/// The task exit used to finalize audit and transaction state.
pub(crate) enum ResourceProducerExit {
    Completed(ResourceProducerCompleted),
    Cancelled(ResourceProducerCancelled),
    Failed(ResourceProducerFailed),
    SealedFailed(ResourceProducerSealedFailed),
}

pub(crate) struct ResourceProducerCompleted {
    pub(crate) response:
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    pub(crate) final_batch_sequence: u64,
    pub(crate) total_items: u64,
    pub(crate) total_bytes: u64,
}

pub(crate) struct ResourceProducerCancelled {
    pub(crate) response: Option<
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    >,
}

pub(crate) struct ResourceProducerFailed {
    pub(crate) response: Option<
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    >,
    pub(crate) error: PostgresKernelError,
}
pub(crate) struct ResourceProducerSealedFailed {
    pub(super) response: Option<
        tokio::sync::oneshot::Sender<Result<AuthenticatedServerResourceEvent, PostgresKernelError>>,
    >,
    pub(super) failure: SealedInvocationFailureClass,
}

#[derive(Default)]
pub(super) struct ResourceProducerLifecycle {
    pub(super) invocation: Option<InvocationId>,
    pub(super) target: Option<InvocationTarget>,
    pub(super) acceptance_committed: bool,
    pub(super) failure: Option<CallFailure>,
    pub(super) cancelled: bool,
    pub(super) terminal_commit_started: bool,
    pub(super) acceptance_commit_attempted: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResourceProducerFailureStage {
    None,
    PreAcceptance,
    PostAcceptance,
    #[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
    PostAcceptanceAudit,
    PostAcceptanceAuditCancellation,
    PostAcceptanceCancelledExitAudit,
}
