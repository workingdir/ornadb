//! Publishes the exact candidate admitted by an explicit review.
//!
//! `commit` "builds the staged candidate, validates it, creates its commit"
//! (16-administration). Before review, the staged format-3 store is completed
//! for the staged source: tables without a relation get one, a table whose
//! schema changed evolves with its committed rows (ORNA-EVOL-002), a retired
//! table's relation leaves the store, the committed identity inventory is
//! reconciled into the store, and the allocator summary records each
//! automatic-key table's current watermark (ORNA-AUTOID-005). An unborn
//! branch's first commit creates the branch ref.

use orna_repository_v1::{
    relation_admission::{
        CommittedRelationChanges, RelationAdmissionError, StagedRelation, is_system_relation,
    },
    schema_evolution::SchemaChanges,
};

use super::*;

fn usage() -> Diagnostic {
    Diagnostic::usage(
        "E1002",
        "commit requires a message",
        "use `orna commit -m MESSAGE` to publish the reviewed index candidate",
    )
}

fn message(arguments: &[String]) -> Result<&str, Diagnostic> {
    if arguments.len() != 2 || !matches!(arguments[0].as_str(), "-m" | "--message") || arguments[1].is_empty() {
        return Err(usage());
    }
    Ok(&arguments[1])
}

fn staging_error(title: &'static str, detail: String) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2300",
        title,
        "fix the staged source or store, then retry `orna commit -m MESSAGE`; no commit was created",
        detail,
    )
}

/// Admits every table of the staged source into the staged `.orna/store`.
///
/// The staged source is the index tree, not the worktree, so the store stays
/// complete under the staged schema (ORNA-STAGE-002). Identities come from the
/// inventory the staged tree already carries (the committed store, a store a
/// failed earlier commit staged, or a legacy sidecar), so a retried commit
/// keeps its IDs; new declarations get new IDs and removed ones retire
/// (ORNA-OBJECT-001, ORNA-OBJECT-005). A committed relation changes only when
/// `changes` allows it.
fn stage_store(
    repository: &orna_repository_v1::Repository,
    changes: CommittedRelationChanges,
) -> Result<(), Diagnostic> {
    let staged = repository.capture_staged_candidate().map_err(|error| {
        staging_error("staged candidate could not be captured", error.to_string())
    })?;
    let project = orna_project_v1::ProjectLoader::default()
        .load_private_candidate(repository, staged.candidate())
        .map_err(|error| {
            staging_error("staged source could not be loaded", format!("{error:?}"))
        })?;
    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        return Err(Diagnostic::target(
            "E2101",
            "staged source semantic analysis failed",
            "fix the first reported source contract error, stage it, then commit again",
        ));
    }
    let previous = orna_project_v1::ObjectIdentityManifest::read_staged(repository, &staged)
        .map_err(|error| {
            staging_error(
                "staged object identities could not be read",
                format!("{error:?}"),
            )
        })?;
    let identities =
        orna_project_v1::ObjectIdentityManifest::reconcile(project.modules(), previous.as_ref())
            .map_err(|error| {
                staging_error(
                    "staged object identities could not be reconciled",
                    format!("{error:?}"),
                )
            })?;
    let database_id = *staged.database_id().as_bytes();
    let mut relations = Vec::new();
    let mut names = BTreeMap::new();
    for (namespace, module) in &analysis.modules {
        let module_name = namespace.display();
        for (name, symbol) in &module.symbols {
            let Some(admission) = symbol
                .table_schema
                .as_ref()
                .and_then(|schema| schema.admission.as_ref())
                .filter(|_| symbol.kind == orna_semantic_v1::SymbolKind::Table)
            else {
                continue;
            };
            let table = project
                .source_table_schema_in_database(&identities, database_id, &module_name, name)
                .map_err(|error| {
                    staging_error(
                        "staged table schema could not be built",
                        format!("{name}: {error:?}"),
                    )
                })?;
            names.insert(*table.table_object_id(), name.clone());
            let relation =
                StagedRelation::new(*table.table_object_id(), table.descriptor().clone())
                    .with_introductions(project.constant_default_stored_values(&table));
            relations.push(if admission.automatic_key {
                relation.with_automatic_key()
            } else {
                relation
            });
        }
    }
    let table_name = |relation: &[u8; 16]| {
        names.get(relation).cloned().unwrap_or_else(|| {
            // A removed table keeps its name in the retired inventory record.
            previous
                .as_ref()
                .and_then(|manifest| {
                    manifest.records().iter().find(|record| {
                        record.kind() == orna_project_v1::ObjectKind::Table
                            && record.id() == relation
                    })
                })
                .map_or_else(String::new, |record| record.qualified_name().to_owned())
        })
    };
    let admission = repository
        .stage_store_admission(&staged, &relations, &identities.to_stored(), changes)
        .map_err(|error| admission_error(&error, &table_name))?;
    for relation in admission.admitted() {
        if !is_system_relation(relation) {
            println!(
                "admitted table {} as relation {}",
                table_name(relation),
                orna_repository_v1::Uuid::from_bytes(*relation).simple()
            );
        }
    }
    for evolved in admission.evolved() {
        println!(
            "evolved table {} ({}): {} rows rewritten",
            table_name(evolved.relation_id()),
            describe_changes(evolved.changes()),
            evolved.rows()
        );
    }
    for removed in admission.removed() {
        println!(
            "removed table {} with {} rows",
            table_name(removed.relation_id()),
            removed.rows()
        );
    }
    Ok(())
}

/// One line naming every field-level change of an evolved table.
fn describe_changes(changes: &SchemaChanges) -> String {
    let mut parts = changes
        .renamed()
        .iter()
        .map(|(old, new)| format!("renamed {old} to {new}"))
        .collect::<Vec<_>>();
    for (verb, fields) in [
        ("added", changes.added()),
        ("removed", changes.removed()),
        ("converted", changes.converted()),
    ] {
        if !fields.is_empty() {
            parts.push(format!("{verb} {}", fields.join(", ")));
        }
    }
    if parts.is_empty() {
        "field metadata changed".to_owned()
    } else {
        parts.join("; ")
    }
}

fn admission_error(
    error: &RelationAdmissionError,
    table_name: &dyn Fn(&[u8; 16]) -> String,
) -> Diagnostic {
    match error {
        RelationAdmissionError::PendingRuntimeRows { relation_id } => {
            Diagnostic::target_with_detail(
                "E2300",
                "staged schema change waits for unpublished rows",
                "publish the pending runtime rows with `orna publish`, then retry `orna commit -m MESSAGE`; no commit was created",
                format!(
                    "table {} would change while runtime rows written under its committed schema are unpublished",
                    table_name(relation_id)
                ),
            )
        }
        // The refusal names the table and field the staged source must
        // change, so it is a source diagnostic shown without `--debug`.
        RelationAdmissionError::Evolution {
            relation_id,
            refusal,
        } => Diagnostic::target_with_detail(
            "E2101",
            "staged schema change does not fit the committed rows",
            "change the staged table so its committed rows fit it (make a new field optional or give it a constant default, keep the primary key, use a lossless type change), stage it, then retry `orna commit -m MESSAGE`; no commit was created",
            format!("table {}: {refusal}", table_name(relation_id)),
        ),
        other => staging_error(
            "staged store could not admit the staged tables",
            other.to_string(),
        ),
    }
}

pub(super) fn run(endpoint: &Endpoint, arguments: &[String]) -> Result<(), Diagnostic> {
    let message = message(arguments)?;
    let path = local_project_path(endpoint)?;
    let repository = orna_repository_v1::Repository::discover(path).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300",
            "Git worktree could not be discovered",
            "run `orna commit -m MESSAGE` inside a local Orna Git worktree",
            format!("{error:?}"),
        )
    })?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Diagnostic::target_with_detail(
            "E2300", "local runtime could not be started", "retry `orna commit -m MESSAGE`", format!("{error:?}"),
        ))?;
    let state = runtime.block_on(RuntimeState::open(&repository, identity, initial_digest)).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "local runtime could not be opened", "repair the local Orna runtime state before committing", format!("{error:?}"),
        )
    })?;
    // Unpublished runtime rows were encoded under the committed schemas, so
    // no committed relation may change under them.
    let pending = runtime.block_on(state.pending_count()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "pending runtime rows could not be counted", "retry after the local runtime state is available", format!("{error:?}"),
        )
    })?;
    let changes = if pending == 0 {
        CommittedRelationChanges::Allowed
    } else {
        CommittedRelationChanges::PendingRuntimeRows
    };
    stage_store(&repository, changes)?;
    let capture = runtime.block_on(state.capture()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "runtime candidate could not be captured", "retry after the local runtime state is available", format!("{error:?}"),
        )
    })?;
    let review = repository.review_current_candidate(&capture).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "staged candidate could not be reviewed", "resolve candidate errors and retry; no commit was created", format!("{error:?}"),
        )
    })?;
    let commit = repository.commit_reviewed_candidate(&review, message).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300", "reviewed candidate could not be committed", "candidate state may have changed; review it again before retrying", format!("{error:?}"),
        )
    })?;
    println!("[{}] {message}", commit.as_str());
    Ok(())
}
