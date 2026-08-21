//! Transient lifecycle state for one client-side Inspector epoch.

pub use orna_core::inspect_lifecycle::{
    InspectEpochBinding, InspectFreezeToken, InspectLifecycleError, InspectProjectionVersions,
};

/// The current lifetime state of a client-side Inspector model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientInspectLifecycleState {
    /// The model accepts operations and late-result validation.
    Active,
    /// The model has issued a live freeze token and awaits resumption.
    Frozen,
    /// The owning operation was cancelled.
    Cancelled,
    /// The client or runtime lifetime has ended.
    Closed,
}

/// The transient, in-memory lifecycle for one client-side Inspector epoch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientInspectLifecycle {
    binding: InspectEpochBinding,
    state: ClientInspectLifecycleState,
    freeze_token: Option<InspectFreezeToken>,
}

impl ClientInspectLifecycle {
    /// Creates an active lifecycle for one Inspector epoch.
    pub fn new(binding: InspectEpochBinding) -> Self {
        Self {
            binding,
            state: ClientInspectLifecycleState::Active,
            freeze_token: None,
        }
    }

    /// Returns the binding for the current epoch.
    pub const fn binding(&self) -> InspectEpochBinding {
        self.binding
    }

    /// Returns the current lifecycle state.
    pub const fn state(&self) -> ClientInspectLifecycleState {
        self.state
    }

    /// Returns whether this lifecycle is active.
    pub const fn is_active(&self) -> bool {
        matches!(self.state, ClientInspectLifecycleState::Active)
    }

    /// Returns whether this lifecycle is frozen.
    pub const fn is_frozen(&self) -> bool {
        matches!(self.state, ClientInspectLifecycleState::Frozen)
    }

    /// Returns whether this lifecycle was cancelled.
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.state, ClientInspectLifecycleState::Cancelled)
    }

    /// Returns whether this lifecycle was closed.
    pub const fn is_closed(&self) -> bool {
        matches!(self.state, ClientInspectLifecycleState::Closed)
    }

    /// Freezes the active model and returns its exact transient token.
    pub fn freeze(&mut self) -> Result<InspectFreezeToken, InspectLifecycleError> {
        match self.state {
            ClientInspectLifecycleState::Active => {
                let token = InspectFreezeToken::issue(self.binding);
                self.freeze_token = Some(token);
                self.state = ClientInspectLifecycleState::Frozen;
                Ok(token)
            }
            ClientInspectLifecycleState::Frozen => Err(InspectLifecycleError::TokenMismatch),
            ClientInspectLifecycleState::Cancelled => Err(InspectLifecycleError::Cancelled),
            ClientInspectLifecycleState::Closed => Err(InspectLifecycleError::Closed),
        }
    }

    /// Resumes a frozen model with its exact live token.
    pub fn resume(&mut self, token: InspectFreezeToken) -> Result<(), InspectLifecycleError> {
        match self.state {
            ClientInspectLifecycleState::Active => Err(InspectLifecycleError::NotFrozen),
            ClientInspectLifecycleState::Cancelled => Err(InspectLifecycleError::Cancelled),
            ClientInspectLifecycleState::Closed => Err(InspectLifecycleError::Closed),
            ClientInspectLifecycleState::Frozen => {
                token.binding().validate_against(&self.binding)?;
                if self.freeze_token != Some(token) {
                    return Err(InspectLifecycleError::TokenMismatch);
                }
                self.freeze_token = None;
                self.state = ClientInspectLifecycleState::Active;
                Ok(())
            }
        }
    }

    /// Replaces the current epoch with a newer binding.
    pub fn replace_epoch(
        &mut self,
        binding: InspectEpochBinding,
    ) -> Result<(), InspectLifecycleError> {
        if self.is_cancelled() {
            return Err(InspectLifecycleError::Cancelled);
        }
        if self.is_closed() {
            return Err(InspectLifecycleError::Closed);
        }

        match binding.generation().cmp(&self.binding.generation()) {
            std::cmp::Ordering::Less => Err(InspectLifecycleError::StaleEpoch {
                expected: self.binding.generation(),
                actual: binding.generation(),
            }),
            std::cmp::Ordering::Equal if binding != self.binding => {
                Err(InspectLifecycleError::EpochMismatch)
            }
            std::cmp::Ordering::Equal => Ok(()),
            std::cmp::Ordering::Greater => {
                self.binding = binding;
                self.freeze_token = None;
                self.state = ClientInspectLifecycleState::Active;
                Ok(())
            }
        }
    }

    /// Validates a completion against this lifecycle's current epoch.
    pub fn validate_completion(
        &self,
        binding: &InspectEpochBinding,
    ) -> Result<(), InspectLifecycleError> {
        match self.state {
            ClientInspectLifecycleState::Cancelled => Err(InspectLifecycleError::Cancelled),
            ClientInspectLifecycleState::Closed => Err(InspectLifecycleError::Closed),
            ClientInspectLifecycleState::Active | ClientInspectLifecycleState::Frozen => {
                binding.validate_against(&self.binding)
            }
        }
    }

    /// Cancels this lifecycle and discards its freeze token.
    pub fn cancel(&mut self) {
        self.freeze_token = None;
        self.state = ClientInspectLifecycleState::Cancelled;
    }

    /// Closes this lifecycle and discards its freeze token.
    pub fn shutdown(&mut self) {
        self.freeze_token = None;
        self.state = ClientInspectLifecycleState::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::revision::RevisionPair;
    use orna_core::{CatalogueRevisionId, InvocationId, PrincipalId, SourceRevisionId};

    fn invocation_id(byte: u8) -> InvocationId {
        InvocationId::from_bytes([byte; 16])
    }

    fn principal_id(byte: u8) -> PrincipalId {
        PrincipalId::from_bytes([byte; 16])
    }

    fn revision_pair(byte: u8) -> RevisionPair {
        RevisionPair::new(
            SourceRevisionId::from_bytes([byte; 16]),
            CatalogueRevisionId::from_bytes([byte; 16]),
        )
    }

    fn binding(generation: u64) -> InspectEpochBinding {
        InspectEpochBinding::new(
            invocation_id(1),
            2,
            invocation_id(3),
            principal_id(4),
            revision_pair(5),
            invocation_id(6),
            invocation_id(7),
            InspectProjectionVersions::v1(),
            generation,
        )
    }

    #[test]
    fn freeze_and_resume_require_the_exact_live_token() {
        let mut lifecycle = ClientInspectLifecycle::new(binding(1));
        let token = lifecycle.freeze().expect("active lifecycle freezes");

        assert!(lifecycle.is_frozen());
        assert_eq!(lifecycle.resume(token), Ok(()));
        assert!(lifecycle.is_active());
        assert_eq!(lifecycle.resume(token), Err(InspectLifecycleError::NotFrozen));
    }

    #[test]
    fn wrong_token_is_rejected_without_unfreezing() {
        let mut lifecycle = ClientInspectLifecycle::new(binding(1));
        let token = lifecycle.freeze().expect("active lifecycle freezes");
        let wrong_token = InspectFreezeToken::issue(binding(1));

        assert_ne!(token, wrong_token);
        assert_eq!(lifecycle.freeze(), Err(InspectLifecycleError::TokenMismatch));
        assert_eq!(
            lifecycle.resume(wrong_token),
            Err(InspectLifecycleError::TokenMismatch)
        );
        assert!(lifecycle.is_frozen());
        assert_eq!(lifecycle.resume(token), Ok(()));
    }

    #[test]
    fn refreshing_epoch_invalidates_old_token_and_stale_completion() {
        let old_binding = binding(1);
        let new_binding = binding(2);
        let mut lifecycle = ClientInspectLifecycle::new(old_binding);
        let old_token = lifecycle.freeze().expect("active lifecycle freezes");

        assert_eq!(lifecycle.replace_epoch(new_binding), Ok(()));
        assert!(lifecycle.is_active());
        assert_eq!(
            lifecycle.resume(old_token),
            Err(InspectLifecycleError::NotFrozen)
        );
        assert_eq!(
            lifecycle.validate_completion(&old_binding),
            Err(InspectLifecycleError::StaleEpoch {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(lifecycle.validate_completion(&new_binding), Ok(()));
    }

    #[test]
    fn completion_preserves_future_principal_and_revision_rejections() {
        let current = binding(2);
        let lifecycle = ClientInspectLifecycle::new(current);

        assert_eq!(
            lifecycle.validate_completion(&binding(3)),
            Err(InspectLifecycleError::FutureEpoch {
                expected: 2,
                actual: 3,
            })
        );

        let principal_mismatch = InspectEpochBinding::new(
            invocation_id(1),
            2,
            invocation_id(3),
            principal_id(8),
            revision_pair(5),
            invocation_id(6),
            invocation_id(7),
            InspectProjectionVersions::v1(),
            2,
        );
        assert_eq!(
            lifecycle.validate_completion(&principal_mismatch),
            Err(InspectLifecycleError::PrincipalMismatch)
        );

        let revision_mismatch = InspectEpochBinding::new(
            invocation_id(1),
            2,
            invocation_id(3),
            principal_id(4),
            revision_pair(9),
            invocation_id(6),
            invocation_id(7),
            InspectProjectionVersions::v1(),
            2,
        );
        assert_eq!(
            lifecycle.validate_completion(&revision_mismatch),
            Err(InspectLifecycleError::RevisionMismatch {
                expected: revision_pair(5),
                actual: revision_pair(9),
            })
        );
        assert!(lifecycle.is_active());
    }

    #[test]
    fn replacement_requires_a_new_generation() {
        let current = binding(2);
        let mut lifecycle = ClientInspectLifecycle::new(current);

        assert_eq!(
            lifecycle.replace_epoch(binding(1)),
            Err(InspectLifecycleError::StaleEpoch {
                expected: 2,
                actual: 1,
            })
        );
        assert_eq!(lifecycle.replace_epoch(current), Ok(()));
        let same_generation_different_identity = InspectEpochBinding::new(
            invocation_id(1),
            2,
            invocation_id(9),
            principal_id(4),
            revision_pair(5),
            invocation_id(6),
            invocation_id(7),
            InspectProjectionVersions::v1(),
            2,
        );
        assert_eq!(
            lifecycle.replace_epoch(same_generation_different_identity),
            Err(InspectLifecycleError::EpochMismatch)
        );
    }

    #[test]
    fn cancellation_rejects_late_completion_and_is_idempotent() {
        let current = binding(1);
        let mut lifecycle = ClientInspectLifecycle::new(current);
        let _token = lifecycle.freeze().expect("active lifecycle freezes");

        lifecycle.cancel();
        lifecycle.cancel();
        assert!(lifecycle.is_cancelled());
        assert_eq!(
            lifecycle.validate_completion(&current),
            Err(InspectLifecycleError::Cancelled)
        );
        assert_eq!(
            lifecycle.replace_epoch(binding(2)),
            Err(InspectLifecycleError::Cancelled)
        );
        assert_eq!(
            lifecycle.freeze(),
            Err(InspectLifecycleError::Cancelled)
        );
    }

    #[test]
    fn shutdown_rejects_late_completion_and_is_idempotent() {
        let current = binding(1);
        let mut lifecycle = ClientInspectLifecycle::new(current);
        let _token = lifecycle.freeze().expect("active lifecycle freezes");

        lifecycle.shutdown();
        lifecycle.shutdown();
        assert!(lifecycle.is_closed());
        assert_eq!(
            lifecycle.validate_completion(&current),
            Err(InspectLifecycleError::Closed)
        );
        assert_eq!(
            lifecycle.replace_epoch(binding(2)),
            Err(InspectLifecycleError::Closed)
        );
        assert_eq!(lifecycle.freeze(), Err(InspectLifecycleError::Closed));
    }
}
