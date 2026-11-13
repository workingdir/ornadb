//! The repository runtime identity, with the refusal kept as data so each
//! surface maps it to its own diagnostic.

use std::fmt;

use orna_repository::Repository;
use orna_runtime::{RuntimeError, RuntimeIdentity};

/// The runtime identity of `repository` and its digest, or the refusal that
/// prevented resolving it.
pub fn repository_runtime_identity(
    repository: &Repository,
) -> Result<(RuntimeIdentity, [u8; 32]), RuntimeIdentityRefusal> {
    orna_runtime::repository_runtime_identity(repository).map_err(RuntimeIdentityRefusal)
}

/// Why the repository runtime identity could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentityRefusal(pub RuntimeError);

impl fmt::Display for RuntimeIdentityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}
