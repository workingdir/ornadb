//! Publishes the exact candidate admitted by an explicit review.
//!
//! `commit` "builds the staged candidate, validates it, creates its commit"
//! (16-administration). Before review, the staged format-3 store is completed
//! for the staged source: tables without a relation get one, and the
//! committed identity inventory is reconciled into the store. An unborn
//! branch's first commit creates the branch ref.

use orna_repository_v1::relation_admission::{OBJECT_IDENTITY_RELATION_ID, StagedRelation};

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
/// (ORNA-OBJECT-001, ORNA-OBJECT-005).
fn stage_store(repository: &orna_repository_v1::Repository) -> Result<(), Diagnostic> {
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
    let previous = orna_project_v1::ObjectIdentityManifest::read_committed_if_present(
        repository,
        staged.candidate().commit(),
    )
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
            let admitted = symbol.kind == orna_semantic_v1::SymbolKind::Table
                && symbol
                    .table_schema
                    .as_ref()
                    .is_some_and(|schema| schema.admission.is_some());
            if !admitted {
                continue;
            }
            let table = project
                .source_table_schema_in_database(&identities, database_id, &module_name, name)
                .map_err(|error| {
                    staging_error(
                        "staged table schema could not be built",
                        format!("{name}: {error:?}"),
                    )
                })?;
            names.insert(*table.table_object_id(), name.clone());
            relations.push(StagedRelation::new(
                *table.table_object_id(),
                table.descriptor().clone(),
            ));
        }
    }
    let admission = repository
        .stage_store_admission(&staged, &relations, &identities.to_stored())
        .map_err(|error| {
            staging_error(
                "staged store could not admit the staged tables",
                error.to_string(),
            )
        })?;
    for relation in admission.admitted() {
        if *relation != OBJECT_IDENTITY_RELATION_ID {
            let name = names.get(relation).map_or("", String::as_str);
            println!(
                "admitted table {name} as relation {}",
                orna_repository_v1::Uuid::from_bytes(*relation).simple()
            );
        }
    }
    Ok(())
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
    stage_store(&repository)?;
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
