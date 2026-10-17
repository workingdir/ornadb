//! Row-scoped staging against the ordinary Git index.

use super::{Diagnostic, Endpoint, local_project_path};
use orna_repository_v1::{ManagedPath, Repository};

fn error(title: &'static str, detail: impl Into<String>) -> Diagnostic {
    Diagnostic::target_with_detail("E2300", title, "run this command inside a local Orna Git worktree", detail)
}

fn repository(endpoint: &Endpoint) -> Result<Repository, Diagnostic> {
    let root = local_project_path(endpoint)?;
    Repository::discover(root)
        .map_err(|e| error("Git worktree could not be discovered", format!("{e:?}")))
}


/// Resolve the single loose-row source candidate through the project loader,
/// then stage only its repository-relative source file in the ordinary index.
pub(super) fn add_row(endpoint: &Endpoint, table: &str, key: &str) -> Result<(), Diagnostic> {
    let repository = repository(endpoint)?;
    let project = orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(|e| error("project rows could not be loaded", format!("{e:?}")))?;
    let key = serde_json::from_str::<String>(key).unwrap_or_else(|_| key.to_owned());
    let mut candidates = project.loose_rows().iter().filter(|row| {
        let table_matches = row.table_path().rsplit('/').next().is_some_and(|name| {
            name == table || name.eq_ignore_ascii_case(table)
        });
        table_matches && row.key_path().last().is_some_and(|component| {
            component.strip_suffix(".orna").is_some_and(|name| name == key)
        })
    });
    let candidate = candidates.next().ok_or_else(|| error(
        "logical row was not found", format!("no worktree row matches {table} {key:?}"),
    ))?;
    if candidates.next().is_some() {
        return Err(error("logical row is ambiguous", format!("more than one worktree row matches {table} {key:?}")));
    }
    let path = ManagedPath::new(candidate.logical_path())
        .map_err(|e| error("row path is unsafe", format!("{e:?}")))?;
    let index = repository.index_generation()
        .map_err(|e| error("Git index could not be inspected", format!("{e:?}")))?;
    repository.stage_managed(&index, &[path])
        .map_err(|e| error("row could not be staged", format!("{e:?}")))?;
    println!("staged row {table} {key:?}: {}", candidate.logical_path());
    Ok(())
}

