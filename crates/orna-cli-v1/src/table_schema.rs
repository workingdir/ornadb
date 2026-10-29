//! The live identity of a committed table schema, shared by the mount's save
//! path and the media commit test harness (ORNA-VFS-019).

use orna_project_v1::ProjectLoader;
use orna_repository_v1::Repository;
use orna_runtime_v1::{TableObjectId, TableSchemaIdentity};

/// The live identity of `table`'s committed schema: a digest of the descriptor
/// the repository head resolves the table to. A save binds the identity it was
/// admitted under and is refused once the committed schema moves (ORNA-VFS-019).
pub fn committed_table_schema_identity(
    repository: &Repository,
    table: TableObjectId,
) -> Result<TableSchemaIdentity, String> {
    use sha2::Digest;
    let head = repository
        .head()
        .map_err(|error| format!("committed head is unavailable: {error:?}"))?
        .ok_or_else(|| "repository has no committed head".to_owned())?;
    let resolved = ProjectLoader::default()
        .resolve_committed_source_table_schema(repository, &head, table)
        .map_err(|error| format!("committed table schema is unavailable: {error:?}"))?;
    let descriptor = orna_value_v1::schema_identity(resolved.descriptor())
        .map_err(|error| format!("committed table schema is invalid: {error:?}"))?;
    let mut digest = sha2::Sha256::new();
    digest.update(b"orna.vfs.table-schema.v1");
    digest.update(table.bytes());
    digest.update(descriptor);
    Ok(TableSchemaIdentity::new(digest.finalize().into()))
}
