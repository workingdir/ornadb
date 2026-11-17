//! The stored table references of the committed database a project run writes
//! (ORNA-REF-001, ORNA-MUT-007, ORNA-MUT-008).

use std::collections::BTreeMap;

use orna_project::{LoadedProject, ReferenceCatalogue};
use orna_repository::Repository;
use orna_runtime::TableObjectId;

use super::Diagnostic;
use crate::assertion_gate::admitted_tables_of;
use crate::committed::unavailable_table_identity;

/// The reference catalogue of `project`'s tables, bound to the database
/// identity the repository's `HEAD` commits: the identity every committed
/// table descriptor records for a table-typed field, and the one publication
/// validates a stored reference against. `None` when no table holds a
/// reference, or when there is no commit yet, so no table has an identity a
/// reference could name.
pub(crate) fn committed_stored_references(
    repository: &Repository,
    project: &LoadedProject,
    table_object_ids: &BTreeMap<String, TableObjectId>,
) -> Result<Option<ReferenceCatalogue>, Diagnostic> {
    if table_object_ids.is_empty() {
        return Ok(None);
    }
    let unavailable = |detail: &str| unavailable_table_identity(detail.to_owned());
    let Ok(Some(head)) = repository.head() else {
        return Ok(None);
    };
    let (_, schemas) = admitted_tables_of(project)
        .map_err(|_| unavailable("project tables could not be identified"))?;
    let database_id = repository
        .open_pinned_format_context(head.as_str())
        .and_then(|format| format.require_database_id())
        .map_err(|_| unavailable("the committed database identity could not be read"))?;
    ReferenceCatalogue::new(*database_id.as_bytes(), &schemas, table_object_ids)
        .map_err(|_| unavailable("a referenced table has no committed identity"))
}
