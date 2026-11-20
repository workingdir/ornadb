//! The table-assertion and stored-reference validator for served commits.
//!
//! Each committed write to a mounted row is checked against the served
//! project's table assertions and stored references before it publishes
//! (ORNA-ASSERT-042, ORNA-MUT-007, ORNA-MUT-008).

use super::{Arc, BTreeMap, Limits, Repository, RuntimeGeneration};
use crate::assertion_gate::admitted_tables_of;
use crate::committed::table_identities_from_manifest;
use std::collections::BTreeSet;

/// Builds the factory that checks each committed mounted-row write against
/// the served project's table assertions and stored references. The factory
/// runs once per commit, against that commit's own generation, so the
/// validator and the row it checks share one HEAD. `binds_committed` is the
/// decision that installs each Eval's committed rows plan: an Eval that binds
/// committed tables carries their HEAD identities, and the commit admits them.
pub(super) fn served_table_candidate_validator_factory(
    repository: Repository,
    project: orna_project::LoadedProject,
    binds_committed: bool,
) -> orna_live::TableCandidateValidatorFactory {
    Box::new(
        move |context: &orna_runtime::RuntimeActivationContext,
              mutations: &[orna_runtime::TableMutation]| {
            served_table_candidate_validator(
                &repository,
                &project,
                binds_committed,
                context,
                mutations,
            )
        },
    )
}

/// Builds the factory that plans the committed tables each served Eval binds, at
/// the activation's own generation. The Eval reads the admitted tables' committed
/// rows (ORNA-EVAL-002, ornadb-nb5yj). A project that declares stored references
/// also checks the Eval's delete or rekey against the rows that reference the row
/// (ORNA-MUT-008).
pub(super) fn served_committed_eval_rows_factory(
    repository: Repository,
    project: orna_project::LoadedProject,
) -> orna_live::CommittedEvalRowsFactory {
    Box::new(move |context: &orna_runtime::RuntimeActivationContext| {
        served_committed_eval_plan(&repository, &project, context)
    })
}

/// Whether a served Eval over `project` binds the committed rows of the project's
/// tables: the project declares a table, and HEAD carries the committed identity
/// manifest those tables resolve against. Without a commit or a manifest there is
/// no plan to build, so the Eval runs without one, as it did before (ornadb-jm0d).
/// A project whose tables cannot be read counts as declaring them.
pub(super) fn binds_committed_tables(
    repository: &Repository,
    project: &orna_project::LoadedProject,
) -> bool {
    let declares_tables = match admitted_tables_of(project) {
        Ok((names, _)) => !names.is_empty(),
        Err(_) => true,
    };
    declares_tables && committed_identity_manifest_at_head(repository)
}

/// Whether HEAD carries a committed identity manifest. An unborn HEAD, or a
/// commit that predates the manifest, carries none.
fn committed_identity_manifest_at_head(repository: &Repository) -> bool {
    let Ok(Some(head)) = repository.head() else {
        return false;
    };
    matches!(
        orna_project::ObjectIdentityManifest::read_committed_if_present(repository, &head),
        Ok(Some(_))
    )
}

/// The committed tables plan of the activation's generation: the admitted tables
/// at its HEAD, and the function that encodes the rows the activation captures
/// as evaluator rows under each table's committed schema and storage order. Any
/// failure to read the generation, its HEAD or the admitted tables fails the
/// Eval closed as `RuntimeUnavailable`.
fn served_committed_eval_plan(
    repository: &Repository,
    project: &orna_project::LoadedProject,
    context: &orna_runtime::RuntimeActivationContext,
) -> orna_live::Result<orna_live::CommittedEvalPlan> {
    let generation = u64::try_from(context.capture().generation())
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let cwd_generation = repository
        .cwd_generation(RuntimeGeneration::new(generation))
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let commit = cwd_generation
        .head()
        .ok_or(orna_live::Error::RuntimeUnavailable)?;
    let (table_names, mut schemas) =
        admitted_tables_of(project).map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let (tables, bindings) = if table_names.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let manifest =
            orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
                .map_err(|_| orna_live::Error::RuntimeUnavailable)?
                .ok_or(orna_live::Error::RuntimeUnavailable)?;
        let identities = table_identities_from_manifest(&manifest, table_names)
            .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
        let mut bindings = Vec::with_capacity(identities.len());
        for identity in &identities {
            let name = identity.table().to_owned();
            let schema = schemas
                .remove(&name)
                .ok_or(orna_live::Error::RuntimeUnavailable)?;
            let storage_order = orna_project::ProjectLoader::default()
                .resolve_committed_source_storage_order(repository, commit, identity.object_id())
                .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
            bindings.push((name, schema, storage_order));
        }
        (identities, bindings)
    };
    Ok(orna_live::CommittedEvalPlan {
        tables,
        bind: Box::new(move |live: &orna_runtime::RuntimeTableRows| {
            bind_admitted_rows(&bindings, live)
        }),
    })
}

/// Encodes the live stored rows of each admitted table as evaluator rows, under
/// the table's committed schema and storage order.
fn bind_admitted_rows(
    bindings: &[(String, orna_check::TableSchema, Vec<String>)],
    live: &orna_runtime::RuntimeTableRows,
) -> orna_live::Result<orna_runtime::RuntimeTableRows> {
    let mut rows = orna_runtime::RuntimeTableRows::new();
    for (name, schema, storage_order) in bindings {
        let stored = live.get(name).map_or(&[][..], Vec::as_slice);
        let encoded = orna_project::evaluator_rows_from_stored(schema, storage_order, stored)
            .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
        rows.insert(name.clone(), encoded);
    }
    Ok(rows)
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
    binds_committed: bool,
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
    // An Eval that binds committed tables carries the committed HEAD identity of each
    // live table, so the commit admits exactly those identities. An Eval that binds
    // none keeps the activation's own identities.
    let head_ids = head_table_object_ids(&identities);
    // Each row the commit wrote, replaced, deleted or rekeyed must still resolve
    // its stored references in the candidate state (ORNA-MUT-007, ORNA-MUT-008).
    // The catalogue binds the committed HEAD identities, not the identities this
    // activation admitted: a name-addressed Eval admits none, and an empty binding
    // would leave every reference unchecked. A project declaring no stored
    // reference has no catalogue to check.
    let saved_row = orna_conformance::committed_reference_catalogue(
        repository,
        project,
        &head_ids,
        Limits::default(),
    )
    .map_err(|_| orna_live::Error::RuntimeUnavailable)?
    .map(|catalogue| orna_conformance::SavedRowReferences {
        catalogue,
        rows: touched_rows(mutations),
    });
    // Admission follows the identities the Eval binds to its mutations: the committed
    // HEAD identities when the Eval binds committed tables (ORNA-MUT-008), and the
    // activation's own identities otherwise.
    let table_object_ids = if binds_committed {
        Arc::new(head_ids)
    } else {
        Arc::new(context.table_object_ids().clone())
    };
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
