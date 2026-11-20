//! The table-assertion and stored-reference validator for served commits.
//!
//! Each committed write to a mounted row is checked against the served
//! project's table assertions and stored references before it publishes
//! (ORNA-ASSERT-042, ORNA-MUT-007, ORNA-MUT-008).

use super::{Arc, BTreeMap, Limits, Repository, RuntimeGeneration};
use std::collections::BTreeSet;

/// Builds the factory that checks each committed mounted-row write against
/// the served project's table assertions and stored references. The factory
/// runs once per commit, against that commit's own generation, so the
/// validator and the row it checks share one HEAD.
pub(super) fn served_table_candidate_validator_factory(
    repository: Repository,
    project: orna_project::LoadedProject,
) -> orna_live::TableCandidateValidatorFactory {
    Box::new(
        move |context: &orna_runtime::RuntimeActivationContext,
              mutations: &[orna_runtime::TableMutation]| {
            served_table_candidate_validator(&repository, &project, context, mutations)
        },
    )
}

/// The rows a staged commit writes, each as its table and stored key. An
/// insert, replacement or delete names its row; a rekey names both the row it
/// leaves and the row it moves to.
fn touched_rows(mutations: &[orna_runtime::TableMutation]) -> Vec<(String, Vec<u8>)> {
    let mut rows = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        rows.push((mutation.table().to_owned(), mutation.key().to_vec()));
        if let Some(rekeyed) = mutation.rekey_to() {
            rows.push((mutation.table().to_owned(), rekeyed.to_vec()));
        }
    }
    rows
}

/// Builds the table-assertion validator for one staged commit. Any failure to
/// read the generation, its committed identities or the admitted project
/// fails the commit closed as `RuntimeUnavailable`, so the write is refused
/// before any row is published.
fn served_table_candidate_validator(
    repository: &Repository,
    project: &orna_project::LoadedProject,
    context: &orna_runtime::RuntimeActivationContext,
    mutations: &[orna_runtime::TableMutation],
) -> orna_live::Result<Box<dyn orna_runtime::TableActivationCandidateValidator>> {
    let generation = u64::try_from(context.capture().generation())
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let cwd_generation = repository
        .cwd_generation(RuntimeGeneration::new(generation))
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let commit = cwd_generation
        .head()
        .ok_or(orna_live::Error::RuntimeUnavailable)?;
    let identities =
        orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| orna_live::Error::RuntimeUnavailable)?
            .ok_or(orna_live::Error::RuntimeUnavailable)?;
    ensure_unambiguous_live_table_names(&identities)?;
    // The validator admits exactly the identities this activation admitted, so a
    // commit carrying them passes and a commit under any other identity is refused.
    // A name-addressed activation admits none.
    let table_object_ids = Arc::new(context.table_object_ids().clone());
    // Each row the commit wrote, replaced, deleted or rekeyed must still resolve
    // its stored references in the candidate state (ORNA-MUT-007, ORNA-MUT-008).
    // The catalogue binds the committed HEAD identities, not the identities this
    // activation admitted: a name-addressed Eval admits none, and an empty binding
    // would leave every reference unchecked. A project declaring no stored
    // reference has no catalogue to check.
    let saved_row = orna_conformance::committed_reference_catalogue(
        repository,
        project,
        &head_table_object_ids(&identities),
        Limits::default(),
    )
    .map_err(|_| orna_live::Error::RuntimeUnavailable)?
    .map(|catalogue| orna_conformance::SavedRowReferences {
        catalogue,
        rows: touched_rows(mutations),
    });
    orna_conformance::transactional_loaded_project_candidate_validator(
        project,
        &identities,
        orna_conformance::committed_database_id(repository),
        Some(cwd_generation),
        table_object_ids,
        Limits::default(),
        saved_row,
    )
    .map_err(|_| orna_live::Error::RuntimeUnavailable)
}

/// Fails closed when two live table identities share one qualified name, so
/// the commit cannot pick one committed identity for that name. This host binds
/// every live table, so a duplicate under any name fails the commit.
fn ensure_unambiguous_live_table_names(
    identities: &orna_project::ObjectIdentityManifest,
) -> orna_live::Result<()> {
    let mut names = BTreeSet::new();
    for record in identities.records() {
        if record.kind() != orna_project::ObjectKind::Table || record.is_retired() {
            continue;
        }
        if !names.insert(record.qualified_name()) {
            return Err(orna_live::Error::RuntimeUnavailable);
        }
    }
    Ok(())
}

/// The committed identity of each live table at `identities`' commit, keyed by
/// qualified name. Retired records bind no live table, so they are left out.
fn head_table_object_ids(
    identities: &orna_project::ObjectIdentityManifest,
) -> BTreeMap<String, orna_runtime::TableObjectId> {
    identities
        .records()
        .iter()
        .filter(|record| record.kind() == orna_project::ObjectKind::Table && !record.is_retired())
        .map(|record| {
            (
                record.qualified_name().to_owned(),
                orna_runtime::TableObjectId::new(*record.id()),
            )
        })
        .collect()
}
