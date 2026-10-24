//! The one derivation of a repository's runtime identity and initial digest.
//! Every caller that opens a repository's `state.db` (the CLI, tests, and the
//! migration carry) uses this derivation so identities cannot drift.

use orna_repository_v1::{Repository, inspect_metadata_for_runtime};

use crate::{RuntimeError, RuntimeIdentity};

/// Derives the runtime identity and initial digest from the repository's
/// stored database identity. Fails closed when the metadata is unavailable.
///
/// Durable writes follow the format recorded at `HEAD`, not worktree
/// metadata (ORNA-UPGRADE-001). A format-1/2 `HEAD` has no row writer
/// (ORNA-UPGRADE-003), so it is refused with
/// [`RuntimeError::LegacyHeadReadOnly`] before any runtime state is opened.
pub fn repository_runtime_identity(
    repository: &Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), RuntimeError> {
    repository
        .require_writable_head_format()
        .map_err(RuntimeError::LegacyHeadReadOnly)?;
    migration_metadata_identity(repository)
}

/// Derives the identity for the migration's own checkpoint carry into a
/// prepared copy. This is the only sanctioned entry for migration carry: it
/// is the sole caller that admits a prepared copy whose `HEAD` is still
/// legacy. Every other caller uses [`repository_runtime_identity`].
///
/// Residual API risk (recorded on ornadb-5il1.396): this function is `pub`
/// and nothing in the type system restricts it to the migration, so a
/// downstream crate could call it directly. No token machinery is added here;
/// the restriction is by convention and by the single call site in
/// `migration_copy`.
pub fn migration_copy_runtime_identity(
    repository: &Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), RuntimeError> {
    repository
        .require_migration_carry_head_format()
        .map_err(RuntimeError::LegacyHeadReadOnly)?;
    migration_metadata_identity(repository)
}

fn migration_metadata_identity(
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
