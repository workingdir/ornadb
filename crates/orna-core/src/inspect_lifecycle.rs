//! Typed lifecycle bindings for the client Inspector seam.

use std::{error::Error, fmt};

use crate::{
    InspectEpochId, InvocationId, PrincipalId,
    revision::RevisionPair,
};

/// The versions of the eight Inspector projections captured by an epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InspectProjectionVersions([u16; 8]);

impl InspectProjectionVersions {
    /// Returns the v1 version set for all eight projections.
    pub const fn v1() -> Self {
        Self([1; 8])
    }

    /// Returns the versions in projection order.
    pub const fn values(self) -> [u16; 8] {
        self.0
    }
}

impl Default for InspectProjectionVersions {
    fn default() -> Self {
        Self::v1()
    }
}

/// The immutable identity and provenance of one live Inspector epoch.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InspectEpochBinding {
    client_epoch_id: InvocationId,
    server_epoch_id: u64,
    target_invocation_id: InvocationId,
    principal: PrincipalId,
    revision: RevisionPair,
    observer_root_invocation_id: InvocationId,
    observer_parent_invocation_id: InvocationId,
    projection_versions: InspectProjectionVersions,
    generation: u64,
}

impl InspectEpochBinding {
    /// Binds the complete client-side identity and provenance for an epoch.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        client_epoch_id: InvocationId,
        server_epoch_id: u64,
        target_invocation_id: InvocationId,
        principal: PrincipalId,
        revision: RevisionPair,
        observer_root_invocation_id: InvocationId,
        observer_parent_invocation_id: InvocationId,
        projection_versions: InspectProjectionVersions,
        generation: u64,
    ) -> Self {
        Self {
            client_epoch_id,
            server_epoch_id,
            target_invocation_id,
            principal,
            revision,
            observer_root_invocation_id,
            observer_parent_invocation_id,
            projection_versions,
            generation,
        }
    }

    /// Returns the client epoch identity.
    pub const fn client_epoch_id(self) -> InvocationId {
        self.client_epoch_id
    }

    /// Returns the server epoch identity from ORNA-INSPECT/1.
    pub const fn server_epoch_id(self) -> u64 {
        self.server_epoch_id
    }

    /// Returns the invocation being inspected.
    pub const fn target_invocation_id(self) -> InvocationId {
        self.target_invocation_id
    }

    /// Returns the authenticated principal identity.
    pub const fn principal(self) -> PrincipalId {
        self.principal
    }

    /// Returns the captured source and catalogue revision pair.
    pub const fn revision(self) -> RevisionPair {
        self.revision
    }

    /// Returns the Inspector observer root invocation identity.
    pub const fn observer_root_invocation_id(self) -> InvocationId {
        self.observer_root_invocation_id
    }

    /// Returns the Inspector observer parent invocation identity.
    pub const fn observer_parent_invocation_id(self) -> InvocationId {
        self.observer_parent_invocation_id
    }

    /// Returns the projection versions captured by this epoch.
    pub const fn projection_versions(self) -> InspectProjectionVersions {
        self.projection_versions
    }

    /// Returns this epoch's monotonically increasing generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Checks that this binding is valid against the currently expected epoch.
    pub fn validate_against(&self, expected: &Self) -> Result<(), InspectLifecycleError> {
        if self.generation < expected.generation {
            return Err(InspectLifecycleError::StaleEpoch {
                expected: expected.generation,
                actual: self.generation,
            });
        }
        if self.generation > expected.generation {
            return Err(InspectLifecycleError::FutureEpoch {
                expected: expected.generation,
                actual: self.generation,
            });
        }
        if self.principal != expected.principal {
            return Err(InspectLifecycleError::PrincipalMismatch);
        }
        if self.revision != expected.revision {
            return Err(InspectLifecycleError::RevisionMismatch {
                expected: expected.revision,
                actual: self.revision,
            });
        }
        if self.client_epoch_id != expected.client_epoch_id
            || self.server_epoch_id != expected.server_epoch_id
            || self.target_invocation_id != expected.target_invocation_id
            || self.observer_root_invocation_id != expected.observer_root_invocation_id
            || self.observer_parent_invocation_id != expected.observer_parent_invocation_id
            || self.projection_versions != expected.projection_versions
        {
            return Err(InspectLifecycleError::EpochMismatch);
        }

        Ok(())
    }
}

/// A transient exact-match freeze token for one live Inspector binding.
///
/// This token is authority-free and must not be persisted or transported as
/// authority. Clients must compare the exact token in a live lifecycle before
/// resuming a frozen Inspector model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InspectFreezeToken {
    binding: InspectEpochBinding,
    generation: u64,
    nonce: InspectEpochId,
}

impl InspectFreezeToken {
    /// Issues a fresh transient token for the supplied binding.
    pub fn issue(binding: InspectEpochBinding) -> Self {
        Self {
            binding,
            generation: binding.generation(),
            nonce: InspectEpochId::new(),
        }
    }

    /// Returns the binding captured by this token.
    pub const fn binding(self) -> InspectEpochBinding {
        self.binding
    }

    /// Returns the generation captured by this token.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// A stable failure from an Inspector lifecycle check.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectLifecycleError {
    /// The actual binding is older than the live expected binding.
    StaleEpoch { expected: u64, actual: u64 },
    /// The actual binding is newer than the live expected binding.
    FutureEpoch { expected: u64, actual: u64 },
    /// The actual and expected bindings belong to different principals.
    PrincipalMismatch,
    /// The actual and expected bindings captured different revisions.
    RevisionMismatch { expected: RevisionPair, actual: RevisionPair },
    /// The epoch identities or projection versions differ.
    EpochMismatch,
    /// A supplied freeze token is not the exact live token.
    TokenMismatch,
    /// A lifecycle operation requires a frozen Inspector model.
    NotFrozen,
    /// The owning invocation or operation was cancelled.
    Cancelled,
    /// The client or runtime lifetime has ended.
    Closed,
}

impl InspectLifecycleError {
    /// Returns the stable public error code for this failure.
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleEpoch { .. } | Self::TokenMismatch | Self::NotFrozen => {
                "inspect.stale_epoch"
            }
            Self::FutureEpoch { .. } => "inspect.future_epoch",
            Self::PrincipalMismatch | Self::RevisionMismatch { .. } | Self::EpochMismatch => {
                "inspect.epoch_mismatch"
            }
            Self::Cancelled => "inspect.cancelled",
            Self::Closed => "inspect.closed",
        }
    }
}

impl fmt::Display for InspectLifecycleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleEpoch { expected, actual } | Self::FutureEpoch { expected, actual } => {
                write!(
                    formatter,
                    "{} (expected generation {expected}, actual {actual})",
                    self.code()
                )
            }
            _ => formatter.write_str(self.code()),
        }
    }
}

impl Error for InspectLifecycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogueRevisionId, SourceRevisionId};

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
    fn projection_versions_default_to_v1() {
        assert_eq!(InspectProjectionVersions::default().values(), [1; 8]);
        assert_eq!(InspectProjectionVersions::v1().values(), [1; 8]);
    }

    #[test]
    fn binding_validation_reports_ordered_lifecycle_mismatches() {
        let expected = binding(4);

        assert_eq!(
            binding(3).validate_against(&expected),
            Err(InspectLifecycleError::StaleEpoch {
                expected: 4,
                actual: 3,
            })
        );
        assert_eq!(
            binding(5).validate_against(&expected),
            Err(InspectLifecycleError::FutureEpoch {
                expected: 4,
                actual: 5,
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
            4,
        );
        assert_eq!(
            principal_mismatch.validate_against(&expected),
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
            4,
        );
        assert_eq!(
            revision_mismatch.validate_against(&expected),
            Err(InspectLifecycleError::RevisionMismatch {
                expected: revision_pair(5),
                actual: revision_pair(9),
            })
        );

        let identity_mismatch = InspectEpochBinding::new(
            invocation_id(1),
            2,
            invocation_id(10),
            principal_id(4),
            revision_pair(5),
            invocation_id(6),
            invocation_id(7),
            InspectProjectionVersions::v1(),
            4,
        );
        assert_eq!(
            identity_mismatch.validate_against(&expected),
            Err(InspectLifecycleError::EpochMismatch)
        );
        assert_eq!(expected.validate_against(&expected), Ok(()));
    }

    #[test]
    fn issued_tokens_require_exact_identity() {
        let binding = binding(4);
        let first = InspectFreezeToken::issue(binding);
        let second = InspectFreezeToken::issue(binding);

        assert_ne!(first, second);
        assert_eq!(first.binding(), binding);
        assert_eq!(first.generation(), 4);
    }

    #[test]
    fn lifecycle_error_codes_are_stable() {
        let revision = revision_pair(1);
        let errors = [
            (
                InspectLifecycleError::StaleEpoch {
                    expected: 2,
                    actual: 1,
                },
                "inspect.stale_epoch",
            ),
            (
                InspectLifecycleError::FutureEpoch {
                    expected: 1,
                    actual: 2,
                },
                "inspect.future_epoch",
            ),
            (
                InspectLifecycleError::PrincipalMismatch,
                "inspect.epoch_mismatch",
            ),
            (
                InspectLifecycleError::RevisionMismatch {
                    expected: revision,
                    actual: revision,
                },
                "inspect.epoch_mismatch",
            ),
            (
                InspectLifecycleError::EpochMismatch,
                "inspect.epoch_mismatch",
            ),
            (
                InspectLifecycleError::TokenMismatch,
                "inspect.stale_epoch",
            ),
            (
                InspectLifecycleError::NotFrozen,
                "inspect.stale_epoch",
            ),
            (InspectLifecycleError::Cancelled, "inspect.cancelled"),
            (InspectLifecycleError::Closed, "inspect.closed"),
        ];

        for (error, code) in errors {
            assert_eq!(error.code(), code);
        }
    }
}
