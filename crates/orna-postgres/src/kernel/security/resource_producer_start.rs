//! Startup and acceptance of authenticated SERVER resource producers.

use super::resource_producer::{
    ResourceProducerFailureStage, ResourceProducerReady, ResourceProducerStartGuard,
};
use super::resource_stream::run_authenticated_server_resource_producer_task;
use super::*;

impl PostgresKernel {
    /// Starts one bounded authenticated SERVER resource producer.
    ///
    /// The returned handle contains only owned command channels and acceptance
    /// metadata. The spawned task owns the RepeatableRead transaction and the
    /// PostgreSQL query stream for its entire lifetime.
    pub async fn start_authenticated_server_resource_producer(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::None,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_pre_acceptance_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PreAcceptance,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptance,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_audit_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptanceAudit,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_audit_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptanceAuditCancellation,
        )
        .await
    }

    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn start_authenticated_server_resource_producer_with_forced_post_acceptance_cancelled_exit_audit_failure(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        self.start_authenticated_server_resource_producer_with_failure_hook(
            authenticated_session,
            request,
            cancellation,
            ResourceProducerFailureStage::PostAcceptanceCancelledExitAudit,
        )
        .await
    }

    async fn start_authenticated_server_resource_producer_with_failure_hook(
        &self,
        authenticated_session: &AuthenticatedSession,
        request: &ResourceRequest,
        cancellation: &ResourceCancellation,
        failure_stage: ResourceProducerFailureStage,
    ) -> Result<AuthenticatedServerResourceStart, PostgresKernelError> {
        validate_resource_lineage(request)?;
        validate_resource_state_context(request)?;
        if !self
            .resource_parent_invocation_is_owned(authenticated_session, request)
            .await?
        {
            return Ok(AuthenticatedServerResourceStart::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            });
        }
        if !self.reserve_resource_request_id(request.request_id).await? {
            return Ok(AuthenticatedServerResourceStart::Failed {
                stream_id: request.stream_id,
                request_id: request.request_id,
                failure: CallFailure::InternalFailure,
            });
        }
        let (commands, command_receiver) = tokio::sync::mpsc::channel(1);
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let request_id = request.request_id;
        let kernel = self.clone();
        let session = authenticated_session.clone();
        let request = request.clone();
        let worker_cancellation = cancellation.clone();
        tokio::spawn(async move {
            let _ = run_authenticated_server_resource_producer_task(
                kernel,
                session,
                request,
                worker_cancellation,
                failure_stage,
                command_receiver,
                ready_sender,
            )
            .await;
        });
        let mut start_guard = ResourceProducerStartGuard::new(cancellation.clone());
        match ready_receiver
            .await
            .map_err(|_| PostgresKernelError::DurableInvariant {
                relation: "resource producer",
                record: request_id.canonical(),
                rule: "producer task terminated before acceptance",
            })?? {
            ResourceProducerReady::Accepted(accepted) => {
                start_guard.disarm();
                Ok(AuthenticatedServerResourceStart::Accepted(
                    AuthenticatedServerResourceProducer {
                        accepted,
                        commands,
                        cancellation: cancellation.clone(),
                    },
                ))
            }
            ResourceProducerReady::Failed {
                stream_id,
                request_id,
                failure,
            } => {
                start_guard.disarm();
                Ok(AuthenticatedServerResourceStart::Failed {
                    stream_id,
                    request_id,
                    failure,
                })
            }
        }
    }
}
