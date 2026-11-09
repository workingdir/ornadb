//! Orna storage: format-3 publication through the runtime publication
//! coordinator, plus the retained readers for legacy compact segments.
//!
//! Publication writes only the format-3 native row store. Format-1/2 commits
//! keep their versioned readers, but their loose and compact row writers are
//! retired (ORNA-UPGRADE-003, ORNA-ROW-013): publishing onto such a snapshot
//! fails with [`Error::LegacyReadOnly`] until the repository is migrated.

mod compact;
mod compact_parquet;
mod loose_path;
mod projection;
mod publication;
mod publication_policy;
mod store_root;

pub use compact::{
    COMPACT_STORAGE_PROFILE, CompactBaseProjectionError, CompactBaseRow, CompactBaseState,
    CompactEditableRekey, CompactExactKeyIndex, CompactExactKeySource, CompactHybridRekey,
    CompactKeyError, CompactKeyIdentity, CompactLogicalKeyError, CompactLogicalReader,
    CompactOvbProfile, EditableBaseRow, HybridBaseState, OVB_PROFILE, fold_compact_committed_base,
};
// Retired compact and loose row writers. Nothing in production reaches them:
// publication writes ORP rows and a legacy HEAD is refused before any freeze
// (ORNA-UPGRADE-003). Test crates that need the writer boundary to build a
// base enable `compact-writer-fixtures`.
#[cfg(any(test, feature = "compact-writer-fixtures"))]
pub use compact::{
    CompactHybridEvolution, CompactLoweringError, CompactWriterBatch, CompactWriterInput,
    CompactWriterMutation, CompactWriterMutationState, CompactWriterRow,
    apply_migration_plan_to_compact, lower_publication_freeze,
};
pub use compact_parquet::{CompactParquetError, CompactParquetKeySource};
pub use loose_path::LoosePath;
pub use projection::{
    ProjectedRuntimeMutation, canonicalize_runtime_key, project_runtime_mutation_for_descriptor,
};
pub use publication::{PublicationChurn, RuntimePublicationCoordinator, writable_snapshot_context};
pub use publication_policy::{
    COMPACT_FILE_BOUND_BYTES, CompactPublicationPolicy, CompactPublicationPolicyError,
    DEFAULT_COMPRESSED_TARGET_BYTES, MAX_COMPRESSED_TARGET_BYTES, MIN_COMPRESSED_TARGET_BYTES,
};

use std::fmt;

use orna_repository::RepositoryError;

fn map_publication_repository_error(error: RepositoryError) -> Error {
    match error {
        RepositoryError::InvalidPublicationJournal | RepositoryError::InvalidCompactManifest => {
            Error::InvalidPublicationJournal
        }
        RepositoryError::StaleIndex { .. } => Error::RecoveryIndexConflict,
        RepositoryError::StaleHead => Error::RefConflict,
        RepositoryError::ManagedContentConflict => Error::ManagedWorktreeConflict,
        RepositoryError::LegacyPathChanged(path) => Error::LegacyPathChanged { path },
        RepositoryError::PublicationObjectUnavailable => Error::PublicationObjectUnavailable,
        RepositoryError::PublicationPending => Error::PublicationPending,
        RepositoryError::RuntimeCompletionRequired => Error::InvalidTransition,
        _ => Error::RepositoryUnavailable,
    }
}

/// Keeps a malformed native graph distinct from an unavailable one at the
/// storage boundary; every other format fault stays `RepositoryUnavailable`.
fn repository_format_error(error: orna_repository::FormatContextError) -> Error {
    match error {
        orna_repository::FormatContextError::GraphMalformed(_) => Error::RepositoryMalformed(error),
        _ => Error::RepositoryUnavailable,
    }
}

/// Keeps a malformed native graph distinct from an unavailable one for the
/// graph operations a publication runs; every other graph fault, including
/// quota, cancellation and missing objects, stays `RepositoryUnavailable`.
fn graph_operation_error(error: orna_repository::GraphError) -> Error {
    if error.is_malformed() {
        Error::RepositoryMalformed(orna_repository::FormatContextError::GraphMalformed(error))
    } else {
        Error::RepositoryUnavailable
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidTableRoot,
    InvalidKey,
    UnsafePath,
    IncompleteStaging,
    InvalidObjectId,
    /// The publication target is a format-1/2 compatibility snapshot. It
    /// remains readable, but it has no row writer until it is migrated to
    /// format 3.
    LegacyReadOnly {
        format: u8,
    },
    RecoveryIndexConflict,
    RefConflict,
    ManagedWorktreeConflict,
    /// A legacy loose row file the migration retires was edited after capture.
    /// The file is kept on disk and named so the edit is not lost.
    LegacyPathChanged {
        path: String,
    },
    PublicationPending,
    InvalidPublicationJournal,
    InvalidTransition,
    RuntimeUnavailable,
    RepositoryUnavailable,
    /// Another owner holds the declared maintenance boundary (ORNA-CAP-006).
    RepositoryBusy,
    /// A publish step names a candidate or base object that Git no longer
    /// holds. The publish is refused before any ref or journal state changes
    /// (ORNA-CAP-006, ORNA-GIT-010).
    PublicationObjectUnavailable,
    RepositoryMalformed(orna_repository::FormatContextError),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTableRoot => "invalid table root",
            Self::InvalidKey => "invalid loose row key",
            Self::UnsafePath => "unsafe loose row path",
            Self::IncompleteStaging => "incomplete publication staging",
            Self::InvalidObjectId => "invalid object id",
            Self::LegacyReadOnly { format } => {
                return write!(
                    f,
                    "repository format {format} is a read-only legacy snapshot; migrate it to format 3 before publishing"
                );
            }
            Self::RecoveryIndexConflict => "recovery index conflict",
            Self::RefConflict => "publication ref conflict",
            Self::ManagedWorktreeConflict => "managed worktree content conflict",
            Self::LegacyPathChanged { path } => {
                return write!(
                    f,
                    "legacy file {path} was edited after capture; the migration retires it and kept the edit on disk"
                );
            }
            Self::PublicationPending => "publication remains pending before ref advance",
            Self::InvalidPublicationJournal => "invalid publication journal",
            Self::InvalidTransition => "invalid publication transition",
            Self::RuntimeUnavailable => "runtime contract unavailable",
            Self::RepositoryUnavailable => "repository projection is unavailable",
            Self::RepositoryBusy => "repository maintenance boundary is held by another owner",
            Self::PublicationObjectUnavailable => {
                "a publication object is no longer held by the repository"
            }
            Self::RepositoryMalformed(error) => {
                return write!(f, "repository projection is malformed: {error}");
            }
        })
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_operation_faults_keep_malformed_distinct_from_unavailable() {
        use orna_repository::{FormatContextError, GraphError};
        assert_eq!(
            graph_operation_error(GraphError::OverlappingChildRange),
            Error::RepositoryMalformed(FormatContextError::GraphMalformed(
                GraphError::OverlappingChildRange
            ))
        );
        for operational in [
            GraphError::ReadQuotaExceeded,
            GraphError::UnavailableObject,
            GraphError::GitCommandFailed,
        ] {
            assert_eq!(
                graph_operation_error(operational.clone()),
                Error::RepositoryUnavailable,
                "operational graph fault must stay unavailable: {operational:?}"
            );
        }
    }
}
