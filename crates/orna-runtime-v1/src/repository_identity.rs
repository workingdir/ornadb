//! The one derivation of a repository's runtime identity and initial digest.
//! Every caller that opens a repository's `state.db` (the CLI, tests, and the
//! migration carry) uses this derivation so identities cannot drift.

use orna_repository_v1::{Repository, inspect_metadata_for_runtime};

use crate::{RuntimeError, RuntimeIdentity};

/// Derives the runtime identity and initial digest from the repository's
/// stored database identity. Fails closed when the metadata is unavailable.
pub fn repository_runtime_identity(
    repository: &Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), RuntimeError> {
    let metadata = inspect_metadata_for_runtime(repository)
        .map_err(|_| RuntimeError::StorageUnavailable)?;
    let Some(metadata) = metadata else {
        return Err(RuntimeError::InvalidIdentity);
    };
    Ok(runtime_identity_for_database_id(
        *metadata.database_id().as_bytes(),
    ))
}

/// The single derivation from a database identity to its runtime identity and
/// initial digest. Every repository with this database identity maps to the
/// same runtime identity.
pub fn runtime_identity_for_database_id(database_id: [u8; 16]) -> (RuntimeIdentity, [u8; 32]) {
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }

    let mut initial_digest = [0; 32];
    initial_digest[..16].copy_from_slice(&database_id);
    initial_digest[16..].copy_from_slice(&repository_id);
    if initial_digest == [0; 32] {
        initial_digest[0] = 1;
    }
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        initial_digest,
    )
}
