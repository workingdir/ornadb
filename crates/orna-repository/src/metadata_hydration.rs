//! On-demand hydration of promised blobs (ORNA-GIT-002, ORNA-GIT-006,
//! ORNA-GIT-012; reference "Lazy reads").
//!
//! A blobless partial clone defers blob payloads. Repository reads run with
//! `GIT_NO_LAZY_FETCH=1`, so a promised `.orna/database.orna`, `.orna/format.orna`
//! or source module reads as absent and the repository would refuse to open.
//! This module materializes exactly that one promised object from its promisor
//! remote before it is read. Any other absence, and any failed fetch, remains an
//! error.

use crate::{FetchError, GitObjectState, Repository, RepositoryError};

impl Repository {
    /// Ensures that the blob `oid` is locally readable. An object the object
    /// reader already holds is left untouched. An object that is absent but
    /// proven promised is fetched exactly by its ID. Every other state is
    /// returned unchanged so the caller's read reports its own error.
    pub(crate) fn ensure_blob_readable(&self, oid: &str) -> Result<(), RepositoryError> {
        if self.objects().info(oid)?.is_some() {
            return Ok(());
        }
        match self.observe_git_object(oid)? {
            GitObjectState::Promised => self.hydrate_promised_object(oid).map_err(hydration_error),
            _ => Ok(()),
        }
    }
}

/// Maps a failed exact-object fetch onto the repository error vocabulary: a
/// remote or promisor that cannot supply the object is unavailable, and
/// anything else is an operation failure.
fn hydration_error(error: FetchError) -> RepositoryError {
    match error {
        FetchError::Repository(error) => error,
        FetchError::RemoteUnavailable
        | FetchError::ObjectUnavailable
        | FetchError::PromisorUnavailable => RepositoryError::GitUnavailable,
        _ => RepositoryError::GitOperationFailed,
    }
}
