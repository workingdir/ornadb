//! Row-scoped staging against the ordinary Git index.

use super::{Diagnostic, Endpoint, local_project_path};
use orna_repository_v1::{ManagedPath, Repository};

fn error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2300",
        title,
        "run this command inside a local Orna Git worktree",
        detail,
    )
}

fn repository(endpoint: &Endpoint) -> Result<Repository, Diagnostic> {
    let root = local_project_path(endpoint)?;
    Repository::discover(root)
        .map_err(|e| error("Git worktree could not be discovered", format!("{e:?}")))
}

fn matches_key_path(components: &[String], key: &str) -> bool {
    let requested = key.split('/');
    if components.len() != requested.clone().count() {
        return false;
    }
    components
        .iter()
        .zip(requested)
        .enumerate()
        .all(|(index, (component, expected))| {
            if index + 1 == components.len() {
                component.strip_suffix(".orna") == Some(expected)
            } else {
                component == expected
            }
        })
}

/// Resolve the full logical table/key path through the project loader and stage
/// exactly the source bytes captured in that candidate.
pub(super) fn add_row(endpoint: &Endpoint, table: &str, key: &str) -> Result<(), Diagnostic> {
    let repository = repository(endpoint)?;
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(|e| error("project rows could not be loaded", format!("{e:?}")))?;
    let key = serde_json::from_str::<String>(key).unwrap_or_else(|_| key.to_owned());
    let mut candidates = project
        .loose_rows()
        .iter()
        .filter(|row| row.table_path() == table && matches_key_path(row.key_path(), &key));
    let candidate = candidates.next().ok_or_else(|| {
        error(
            "logical row was not found",
            format!("no worktree row matches table {table:?} and key path {key:?}"),
        )
    })?;
    if candidates.next().is_some() {
        return Err(error(
            "logical row is ambiguous",
            format!("more than one worktree row matches table {table:?} and key path {key:?}"),
        ));
    }
    let path = ManagedPath::new(candidate.logical_path())
        .map_err(|e| error("row path is unsafe", format!("{e:?}")))?;
    let index = repository
        .index_generation()
        .map_err(|e| error("Git index could not be inspected", format!("{e:?}")))?;
    repository
        .stage_managed_contents(&index, &path, candidate.source_bytes())
        .map_err(|e| error("row could not be staged", e.to_string()))?;
    println!("staged row {table} {key:?}: {}", candidate.logical_path());
    Ok(())
}

