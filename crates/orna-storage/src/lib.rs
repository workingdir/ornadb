//! Orna storage: format-3 publication through the runtime publication
//! coordinator, plus the exact-key decoder for verified compact segments.
//!
//! Publication writes only the format-3 native row store. Format-1/2 commits
//! keep their versioned readers, but their loose and compact row writers are
//! retired (ORNA-UPGRADE-003, ORNA-ROW-013): publishing onto such a snapshot
//! fails with [`Error::LegacyReadOnly`] until the repository is migrated.
//!
//! Format-1/2 compact histories are read by `orna-repository`'s verified
//! reader (ORNA-ROW-013, ORNA-UPGRADE-002): it walks the committed manifest,
//! shards and segments and resolves one authoritative row per key. This crate
//! decodes key columns of segment bytes that reader has already verified; it
//! walks no history and decodes no row.

mod compact;
mod compact_parquet;
mod error;
mod loose_path;
mod projection;
mod publication;
mod publication_policy;
mod store_root;

pub use error::{Error, RepositoryUnavailableCause};

pub(crate) use error::{
    graph_operation_error, map_publication_repository_error, repository_format_error,
};

pub use compact::{
    COMPACT_STORAGE_PROFILE, CompactBaseProjectionError, CompactBaseRow, CompactBaseState,
    CompactExactKeyIndex, CompactExactKeySource, CompactKeyError, CompactKeyIdentity,
    CompactLogicalKeyError, CompactLogicalReader, CompactOvbProfile, OVB_PROFILE,
    fold_compact_committed_base,
};
// Retired compact and loose row writers. Nothing in production reaches them:
// publication writes ORP rows and a legacy HEAD is refused before any freeze
// (ORNA-UPGRADE-003). Test crates that need the writer boundary to build a
// base enable `compact-writer-fixtures`.
#[cfg(any(test, feature = "compact-writer-fixtures"))]
pub use compact::{
    CompactLoweringError, CompactWriterBatch, CompactWriterInput, CompactWriterMutation,
    CompactWriterMutationState, CompactWriterRow, apply_migration_plan_to_compact,
    lower_publication_freeze,
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
            let error = graph_operation_error(operational.clone());
            assert!(
                matches!(&error, Error::RepositoryUnavailable(_)),
                "operational graph fault must stay unavailable: {operational:?}"
            );
            assert_eq!(
                std::error::Error::source(&error).map(ToString::to_string),
                Some(operational.to_string()),
                "the operational graph fault is kept as the source: {operational:?}"
            );
        }
    }

    /// The CLI renders `RepositoryUnavailable` with its stable text and code,
    /// so the cause belongs on the source chain, not in the message.
    #[test]
    fn repository_unavailable_keeps_its_cause_out_of_the_rendered_text() {
        let error =
            repository_format_error(orna_repository::FormatContextError::MetadataUnavailable);
        assert_eq!(error.to_string(), "repository projection is unavailable");
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some(orna_repository::FormatContextError::MetadataUnavailable.to_string())
        );
    }
}
