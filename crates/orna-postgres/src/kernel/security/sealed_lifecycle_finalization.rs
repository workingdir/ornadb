//! Public terminal-finalization boundary for accepted sealed invocations.

use super::sealed_invocation::{
    SealedInvocationFailureClass, SealedInvocationLifecycleTerminal,
    transition_sealed_invocation_lifecycle_after_owner_replacement,
    transition_sealed_invocation_lifecycle_with_admission_context,
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
    /// The invocation owner was replaced after the accepted operation could
    /// no longer prove a terminal execution outcome.  The retained admission
    /// lease is compared with `lost`; `replacement` is recorded as the
    /// one-time PostgreSQL receipt for that owner-loss transition. Both values
    /// must be supplied by trusted recovery code, never protocol input.
    Orphaned {
        lost: SealedInvocationWriterLease,
        replacement: SealedInvocationWriterLease,
    },
}

/// Exact writer-lease evidence used to fence owner-loss recovery.
///
/// This is deliberately a private-kernel carrier rather than a public system
/// value. PostgreSQL retains an exact replacement receipt with its lifecycle
/// CAS; it does not attest to or transact with an external lease store.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SealedInvocationWriterLease {
    pub owner_id: [u8; 16],
    pub epoch: u64,
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
            Self::Orphaned { .. } => SealedInvocationLifecycleTerminal::Orphaned,
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
        self.finalize_sealed_invocation_lifecycle_with_admission_context(
            invocation,
            finalization,
            None,
        )
        .await
    }

    /// Records a terminal outcome while matching the exact runtime admission
    /// evidence retained for the invocation. The transport calls this after
    /// rechecking its live owner/capture fence; PostgreSQL then refuses a
    /// terminal claim for a different admission tuple.
    #[doc(hidden)]
    pub async fn finalize_sealed_invocation_lifecycle_with_admission_context(
        &self,
        invocation: InvocationId,
        finalization: SealedInvocationLifecycleFinalization,
        admission_context: Option<&super::sealed_invocation::SealedInvocationAdmissionContext>,
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
            match finalization {
                SealedInvocationLifecycleFinalization::Orphaned { lost, replacement } => {
                    if let Some(context) = admission_context {
                        let expected_epoch = i64::try_from(lost.epoch).map_err(|_| {
                            PostgresKernelError::DurableInvariant {
                                relation: "_orna_kernel.sealed_invocation_lifecycle",
                                record: invocation.canonical(),
                                rule: "orphan recovery lease epoch exceeds PostgreSQL range",
                            }
                        })?;
                        if context.writer_lease_owner() != Some(lost.owner_id)
                            || context.writer_lease_epoch() != Some(expected_epoch)
                        {
                            return Err(PostgresKernelError::DurableInvariant {
                                relation: "_orna_kernel.sealed_invocation_lifecycle",
                                record: invocation.canonical(),
                                rule: "orphan recovery must retain the admitted lost writer lease",
                            });
                        }
                    }
                    transition_sealed_invocation_lifecycle_after_owner_replacement(
                        &transaction,
                        invocation,
                        lost,
                        replacement,
                    )
                    .await?;
                }
                finalization => {
                    transition_sealed_invocation_lifecycle_with_admission_context(
                        &transaction,
                        invocation,
                        finalization.terminal(),
                        admission_context,
                    )
                    .await?;
                }
            }
            transaction
                .commit()
                .await
                .map_err(PostgresKernelError::Database)
        }
        .await;
        finish_authenticated_dispatch_session(operation, database_session.shutdown().await)
    }
}
