//! Public terminal-finalization boundary for accepted sealed invocations.

use super::sealed_invocation::{
    SealedInvocationFailureClass, SealedInvocationLifecycleTerminal,
    transition_sealed_invocation_lifecycle,
};
use super::*;

/// The closed, redaction-safe terminal outcome reported by a sealed adapter.
///
/// This boundary deliberately does not accept execution errors, values, or
/// target identities. Those remain private to the authenticated producer.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SealedInvocationLifecycleFinalization {
    /// The producer durably committed a completed invocation.
    Completed,
    /// The producer durably terminated with its bounded failure class.
    Failed { target_unavailable: bool },
    /// Cancellation won before the producer's terminal commit.
    Cancelled,
    /// The invocation owner was lost after the accepted operation could no
    /// longer prove a terminal execution outcome.
    Orphaned,
}

impl SealedInvocationLifecycleFinalization {
    fn terminal(self) -> SealedInvocationLifecycleTerminal {
        match self {
            Self::Completed => SealedInvocationLifecycleTerminal::Succeeded,
            Self::Failed {
                target_unavailable: true,
            } => SealedInvocationLifecycleTerminal::Failed(SealedInvocationFailureClass::Target),
            Self::Failed {
                target_unavailable: false,
            } => SealedInvocationLifecycleTerminal::Failed(SealedInvocationFailureClass::Internal),
            Self::Cancelled => SealedInvocationLifecycleTerminal::Cancelled,
            Self::Orphaned => SealedInvocationLifecycleTerminal::Orphaned,
        }
    }
}

impl PostgresKernel {
    /// Records the terminal lifecycle outcome of an accepted sealed producer.
    ///
    /// The producer's transaction has already committed or rolled back when
    /// its terminal event is reported. The transport adapter must await this
    /// method before exposing the matching terminal protocol action.
    #[doc(hidden)]
    pub async fn finalize_sealed_invocation_lifecycle(
        &self,
        invocation: InvocationId,
        finalization: SealedInvocationLifecycleFinalization,
    ) -> Result<(), PostgresKernelError> {
        let mut database_session = self.open().await?;
        let operation = async {
            let transaction = database_session
                .client
                .build_transaction()
                .isolation_level(IsolationLevel::RepeatableRead)
                .start()
                .await
                .map_err(PostgresKernelError::Database)?;
            require_current_migrations(&transaction).await?;
            transition_sealed_invocation_lifecycle(
                &transaction,
                invocation,
                finalization.terminal(),
            )
            .await?;
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)
        }
        .await;
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }
}
