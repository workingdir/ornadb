//! The live identity of a committed table schema, shared by the mount's save
//! path and the media commit test harness (ORNA-VFS-019).

use orna_project_v1::ProjectLoader;
use orna_repository_v1::{GitCommitRef, Repository};
use orna_runtime_v1::{TableObjectId, TableSchemaIdentity};

/// The live identity of `table`'s committed schema: a digest of the descriptor
/// the repository head resolves the table to. A save binds the identity it was
/// admitted under and is refused once the committed schema moves (ORNA-VFS-019).
pub fn committed_table_schema_identity(
    repository: &Repository,
    table: TableObjectId,
) -> Result<TableSchemaIdentity, String> {
    let head = committed_head(repository)?;
    resolve_at(repository, &head, table)
}

/// The live identity of `table` read by one save, for the save's two committed
/// schema checks (`with_live_schema_identity` in `orna-runtime-v1` `vfs.rs`).
///
/// The identity is a pure function of the commit `HEAD` names: the resolver
/// loads that commit's immutable tree. So when `HEAD` still names the commit
/// the previous read resolved, the previous identity is also the identity at
/// this read, and the second check costs one `HEAD` lookup and no second
/// project load. A moved `HEAD` is resolved again.
pub struct LiveTableSchemaIdentity {
    table: TableObjectId,
    resolved: Option<(GitCommitRef, TableSchemaIdentity)>,
}

impl LiveTableSchemaIdentity {
    pub fn new(table: TableObjectId) -> Self {
        Self {
            table,
            resolved: None,
        }
    }

    /// The identity at the current committed head, or `None` when it cannot be
    /// resolved. The save treats `None` as a mismatch and refuses the save.
    pub fn read(&mut self, repository: &Repository) -> Option<TableSchemaIdentity> {
        let head = committed_head(repository).ok()?;
        if let Some((resolved_head, identity)) = &self.resolved {
            if *resolved_head == head {
                return Some(identity.clone());
            }
        }
        let identity = resolve_at(repository, &head, self.table).ok()?;
        self.resolved = Some((head, identity.clone()));
        Some(identity)
    }
}

fn committed_head(repository: &Repository) -> Result<GitCommitRef, String> {
    repository
        .head()
        .map_err(|error| format!("committed head is unavailable: {error:?}"))?
        .ok_or_else(|| "repository has no committed head".to_owned())
}

fn resolve_at(
    repository: &Repository,
    head: &GitCommitRef,
    table: TableObjectId,
) -> Result<TableSchemaIdentity, String> {
    use sha2::Digest;
    let resolved = ProjectLoader::default()
        .resolve_committed_source_table_schema(repository, head, table)
        .map_err(|error| format!("committed table schema is unavailable: {error:?}"))?;
    let descriptor = orna_value_v1::schema_identity(resolved.descriptor())
        .map_err(|error| format!("committed table schema is invalid: {error:?}"))?;
    let mut digest = sha2::Sha256::new();
    digest.update(b"orna.vfs.table-schema.v1");
    digest.update(table.bytes());
    digest.update(descriptor);
    Ok(TableSchemaIdentity::new(digest.finalize().into()))
}
