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
    CandidateAssertionGate, CandidateAssertionRefusal, FormatContextError, PrivateCommit,
    relation_admission::{
        CommittedRelationChanges, OBJECT_IDENTITY_RELATION_ID, RelationAdmissionError,
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

const NOTHING_TO_COMMIT_TITLE: &str = "nothing is staged to commit";
const NOTHING_TO_COMMIT_HELP: &str =
    "stage changes with `orna add` or `git add`, then retry `orna commit -m MESSAGE`";

/// The single diagnostic for a commit with no staged tree, shared by the index
/// (`SysGitNothingToCommit`) and relation-admission (`EmptyIndex`) paths (F9).
fn nothing_to_commit() -> Diagnostic {
    Diagnostic::target("E2304", NOTHING_TO_COMMIT_TITLE, NOTHING_TO_COMMIT_HELP)
}

fn message(arguments: &[String]) -> Result<&str, Diagnostic> {
    if arguments.len() != 2
        || !matches!(arguments[0].as_str(), "-m" | "--message")
        || arguments[1].is_empty()
    {
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

/// The candidate tracks a path its recorded repository format does not
/// admit: an `.orna/` entry other than `database.orna` and `store`, or a
/// loose row file a format-3 candidate writes (ORNA-META-001, ORNA-REPO-004).
/// Capture, the source loader and the commit gate all refuse it with format
/// admission's own code (ORNA-REPO-CONTEXT-004) and the path in the title,
/// since attached details are shown only with `--debug`.
fn mixed_formats(error: &FormatContextError) -> Diagnostic {
    Diagnostic {
        code: error.code(),
        title: Cow::Owned(error.to_string()),
        help: "a format-3 snapshot tracks only `.orna/database.orna` and `.orna/store`, and rows change through `orna publish`; unstage that path with `git restore --staged PATH`, then retry; no commit was created",
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}

/// The committed `HEAD` mixes formats, so `orna run` refuses before any runtime
/// state is opened. Same code and title as [`mixed_formats`], but the remedy
/// is true for a committed snapshot: unstaging does not change `HEAD`.
pub(crate) fn mixed_formats_at_head(error: &FormatContextError) -> Diagnostic {
    Diagnostic {
        code: error.code(),
        title: Cow::Owned(error.to_string()),
        help: "the committed snapshot tracks a path a format-3 repository does not admit; commit a snapshot without that path (for example `git rm --cached PATH` then `git commit`), then retry; `orna run` reads the committed HEAD",
        exit: Exit::Target,
        detail: None,
        body: None,
    }
}

/// A retained publication or checkout recovery record owns the index, so
/// staging refuses without touching it; the diagnostic names that condition
/// and its remedy before any generic staging failure.
fn relation_failure(error: RelationAdmissionError, title: &'static str) -> Diagnostic {
    use orna_repository_v1::RepositoryError;
    match error {
        RelationAdmissionError::Repository(RepositoryError::PublicationPending) => {
            Diagnostic::target(
                "E2300",
                "an interrupted publication has not been recovered",
                "run `orna publish` to finish recovery first, then retry `orna commit -m MESSAGE`; no commit was created",
            )
        }
        RelationAdmissionError::Repository(RepositoryError::CheckoutRecoveryRequired) => {
            Diagnostic::target(
                "E2300",
                "an interrupted checkout has not been recovered",
                "resolve the interrupted checkout first, then retry `orna commit -m MESSAGE`; no commit was created",
            )
        }
        other @ RelationAdmissionError::CommittedRelationUnstaged { .. } => {
            Diagnostic::target_with_detail(
                "E2302",
                "staged candidate is not a valid format-3 snapshot",
                "restore the staged `.orna` metadata, `.orna/store` and `main.orna`, then retry; no commit was created",
                other.to_string(),
            )
        }
        RelationAdmissionError::EmptyIndex => nothing_to_commit(),
        RelationAdmissionError::Format(error @ FormatContextError::MixedFormats { .. }) => {
            mixed_formats(&error)
        }
        other => staging_error(title, other.to_string()),
    }
}

/// Admits every table of the staged source into the staged `.orna/store` and
/// returns one line per table this commit admits, for printing once the
/// commit exists.
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
) -> Result<Vec<String>, Diagnostic> {
    repository
        .refuse_unretained_committed_store()
        .map_err(|error| relation_failure(error, "staged candidate could not be captured"))?;
    let staged = repository
        .capture_staged_candidate()
        .map_err(|error| relation_failure(error, "staged candidate could not be captured"))?;
    let project = orna_project_v1::ProjectLoader::default()
        .load_private_candidate(repository, staged.candidate())
        .map_err(|error| match error {
            orna_project_v1::ProjectLoadError::FormatContext(
                error @ FormatContextError::MixedFormats { .. },
            ) => mixed_formats(&error),
            error => staging_error("staged source could not be loaded", format!("{error:?}")),
        })?;
    let catalogue = semantic_catalogue(&project)?;
    let analysis = orna_semantic_v1::analyze_with_catalogue(project.modules(), &catalogue);
    if !analysis.is_ok() {
        let first = analysis
            .diagnostics
            .first()
            .expect("non-empty diagnostics when semantic analysis fails");
        return Err(Diagnostic::target_with_detail(
            "E2101",
            "staged source semantic analysis failed",
            "fix the first reported source contract error, stage it, then commit again",
            format!("{}: {}", first.code(), first.message()),
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
    let staged_relations = orna_application_v1::build_format3_store_relations(
        &project,
        &analysis,
        &identities,
        database_id,
    )
    .map_err(|error| {
        staging_error(
            "staged table schema could not be built",
            format!("{}: {:?}", error.table(), error.error()),
        )
    })?;
    let table_name = |relation: &[u8; 16]| {
        staged_relations
            .table_name(relation)
            .map(str::to_owned)
            .unwrap_or_else(|| {
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
        .stage_store_admission(
            &staged,
            staged_relations.relations(),
            &identities.to_stored(),
            changes,
        )
        .map_err(|error| admission_error(error, &table_name))?;
    let mut lines = admission
        .admitted()
        .iter()
        .filter(|relation| **relation != OBJECT_IDENTITY_RELATION_ID)
        .map(|relation| {
            let name = staged_relations.table_name(relation).unwrap_or("");
            format!(
                "admitted table {name} as relation {}",
                orna_repository_v1::Uuid::from_bytes(*relation).simple()
            )
        })
        .collect::<Vec<_>>();
    for evolved in admission.evolved() {
        lines.push(format!(
            "evolved table {} ({}): {} rows rewritten",
            table_name(evolved.relation_id()),
            describe_changes(evolved.changes()),
            evolved.rows()
        ));
    }
    for removed in admission.removed() {
        lines.push(format!(
            "removed table {} with {} rows",
            table_name(removed.relation_id()),
            removed.rows()
        ));
    }
    Ok(lines)
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
    error: RelationAdmissionError,
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
                    table_name(&relation_id)
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
            format!("table {}: {refusal}", table_name(&relation_id)),
        ),
        other => relation_failure(other, "staged store could not admit the staged tables"),
    }
}

/// The commit's table-assertion gate (ORNA-ASSERT-042, ORNA-ASSERT-046,
/// ORNA-ASSERT-048): the staged candidate's own rows must satisfy every
/// admitted table assertion before any ref advances.
struct CommitAssertionGate;

impl CandidateAssertionGate for CommitAssertionGate {
    fn validate(
        &self,
        repository: &orna_repository_v1::Repository,
        candidate: &PrivateCommit,
    ) -> Result<(), CandidateAssertionRefusal> {
        validate_candidate_assertions(repository, candidate)
    }
}

fn unvalidated(cause: &str) -> CandidateAssertionRefusal {
    CandidateAssertionRefusal::Unvalidated(cause.to_owned())
}

/// Evaluates every admitted table assertion over the candidate's own rows.
/// The transactional seam admits only scalar primary keys (interim gap, bead
/// ornadb-gq4g), so an asserting project with any other key is refused whole
/// rather than validated over rows that were never read.
fn validate_candidate_assertions(
    repository: &orna_repository_v1::Repository,
    candidate: &PrivateCommit,
) -> Result<(), CandidateAssertionRefusal> {
    let project = orna_project_v1::ProjectLoader::default()
        .load_private_candidate(repository, candidate)
        .map_err(|_| unvalidated("staged project could not be loaded"))?;
    if !orna_conformance_v1::project_declares_assertions(&project) {
        return Ok(());
    }
    let catalogue = semantic_catalogue(&project)
        .map_err(|_| unvalidated("staged project has no semantic catalogue"))?;
    let analysis = orna_semantic_v1::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic_v1::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(unvalidated("staged project failed semantic analysis"));
    }
    let (table_names, schemas) = admitted_project_tables(&analysis)
        .ok_or_else(|| unvalidated("staged project tables could not be identified"))?;
    if table_names.is_empty() {
        return Ok(());
    }
    if !schemas.values().all(has_scalar_keys) {
        return Err(unvalidated("staged table primary keys are not scalar yet"));
    }
    let manifest = orna_project_v1::ObjectIdentityManifest::read_private_candidate(
        repository, candidate,
    )
    .ok()
    .flatten()
    .ok_or_else(|| unvalidated("staged object identities are unavailable"))?;
    let identities = table_identities_from_manifest(&manifest, table_names)
        .map_err(|_| unvalidated("staged table identities are unavailable"))?;
    let table_object_ids: BTreeMap<String, orna_runtime_v1::TableObjectId> = identities
        .iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect();
    let format = repository
        .open_private_candidate_format_context(candidate)
        .map_err(|_| unvalidated("staged format context could not be opened"))?;
    let committed = orna_project_v1::private_candidate_tables(
        repository,
        candidate,
        &format,
        &schemas,
        &table_object_ids,
        orna_application_v1::ApplicationAuthority::committed_read_limits(),
    )
    .map_err(|_| unvalidated("staged committed rows could not be read"))?;
    let mut rows = orna_runtime_v1::RuntimeTableRows::new();
    for (name, schema) in &schemas {
        let table = committed
            .get(name)
            .ok_or_else(|| unvalidated("staged table rows are missing"))?;
        let encoded = orna_project_v1::evaluator_rows(table, schema)
            .map_err(|_| unvalidated("staged table rows could not be encoded"))?;
        rows.insert(name.clone(), encoded);
    }
    let mut validator = orna_conformance_v1::transactional_loaded_project_candidate_validator(
        &project,
        &manifest,
        None,
        std::sync::Arc::new(table_object_ids),
        orna_evaluator_v1::Limits::default(),
    )
    .map_err(|outcome| unvalidated(&stage_outcome_cause(&outcome)))?;
    validator.validate(&rows).map_err(|diagnostic| {
        if orna_conformance_v1::is_table_assertion_false(diagnostic) {
            CandidateAssertionRefusal::AssertionFalse
        } else {
            unvalidated("table assertion could not be evaluated over the staged rows")
        }
    })
}

fn has_scalar_keys(schema: &orna_semantic_v1::TableSchema) -> bool {
    schema.admission.as_ref().is_some_and(|admission| {
        admission.keys.iter().all(|(_, ty)| {
            matches!(
                ty,
                orna_semantic_v1::Type::Bool
                    | orna_semantic_v1::Type::Int
                    | orna_semantic_v1::Type::Decimal
                    | orna_semantic_v1::Type::Text
            )
        })
    })
}

/// Reports one review or commit failure with its own code and remedy
/// (16-administration failure classes, ORNA-UX-004). Failures without a
/// dedicated class keep the command's generic code with `generic_title`.
fn failure(error: &orna_repository_v1::RepositoryError, generic_title: &'static str) -> Diagnostic {
    use orna_repository_v1::RepositoryError;
    let (code, title, help) = match error {
        RepositoryError::InvalidCommitMessage => {
            return Diagnostic::usage(
                "E1002",
                "commit message is invalid",
                "use `orna commit -m MESSAGE` with a non-empty message",
            );
        }
        RepositoryError::MissingGitIdentity => (
            "E2301",
            "Git author identity is not configured",
            "set `user.name` and `user.email` with `git config`, then retry; no commit was created",
        ),
        RepositoryError::FormatContext(mixed @ FormatContextError::MixedFormats { .. }) => {
            return mixed_formats(mixed);
        }
        // sys.admin.assertion_failed
        RepositoryError::InvalidCommitCandidate => (
            "E2302",
            "staged candidate is not a valid format-3 snapshot",
            "restore the staged `.orna` metadata, `.orna/store` and `main.orna`, then retry; no commit was created",
        ),
        // sys.git.stale_review
        RepositoryError::StaleReviewedCandidate | RepositoryError::StaleCwd => (
            "E2303",
            "staged candidate changed while it was being committed",
            "review the staged changes again, then retry `orna commit -m MESSAGE`",
        ),
        // sys.git.nothing_to_commit
        RepositoryError::SysGitNothingToCommit => (
            "E2304",
            NOTHING_TO_COMMIT_TITLE,
            NOTHING_TO_COMMIT_HELP,
        ),
        // ORNA-ASSERT-046: a table assertion evaluates false over the staged rows.
        RepositoryError::CandidateAssertionRefused(CandidateAssertionRefusal::AssertionFalse) => (
            "ORNA-A091-009",
            "table assertion is false",
            "change the staged rows so every table assertion holds, stage them, then retry `orna commit -m MESSAGE`; no commit was created",
        ),
        // ORNA-ASSERT-048, ORNA-ASSERT-050: the candidate could not be validated whole, so it is refused whole.
        RepositoryError::CandidateAssertionRefused(CandidateAssertionRefusal::Unvalidated(_)) => (
            "E2300",
            "staged candidate could not be validated against its table assertions",
            "fix the staged source or store, then retry `orna commit -m MESSAGE`; no commit was created",
        ),
        _ => (
            "E2300",
            generic_title,
            "check the repository state, then retry `orna commit -m MESSAGE`",
        ),
    };
    Diagnostic::target_with_detail(code, title, help, format!("{error:?}"))
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
    repository
        .require_configured_commit_identity()
        .map_err(|error| failure(&error, "Git identity could not be checked"))?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2300",
                "local runtime could not be started",
                "retry `orna commit -m MESSAGE`",
                format!("{error:?}"),
            )
        })?;
    let state = runtime
        .block_on(RuntimeState::open(&repository, identity, initial_digest))
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2300",
                "local runtime could not be opened",
                "repair the local Orna runtime state before committing",
                format!("{error:?}"),
            )
        })?;
    // An interrupted publication is recovered before the index is reviewed:
    // recovery restores the exact state the review's intent is derived from,
    // and a stranded Orna index lock would otherwise block this commit.
    let pending = repository.read_publication_journal().map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300",
            "publication journal could not be read",
            "repair the local Orna publication state before committing",
            format!("{error:?}"),
        )
    })?;
    let reviewed = match pending.as_ref() {
        Some(journal) => repository
            .publication_journal_is_reviewed(journal)
            .map_err(|error| {
                Diagnostic::target_with_detail(
                    "E2300",
                    "publication journal could not be classified",
                    "repair the local Orna publication state before committing",
                    format!("{error:?}"),
                )
            })?,
        None => false,
    };
    if reviewed {
        match repository.recover_publication() {
            Ok(_) | Err(orna_repository_v1::RepositoryError::RuntimeCompletionRequired) => {}
            Err(error) => {
                return Err(Diagnostic::target_with_detail(
                    "E2300",
                    "pending reviewed commit could not be recovered",
                    "repair the local Orna publication state and retry",
                    format!("{error:?}"),
                ));
            }
        }
    } else {
        runtime
            .block_on(orna_storage_v1::RuntimePublicationCoordinator::recover(
                &repository,
                &state,
            ))
            .map_err(|error| {
                super::cli_publish::describe_storage_failure(
                    "pending publication could not be recovered",
                    &error,
                )
            })?;
    }
    // Unpublished runtime rows were encoded under the committed schemas, so
    // no committed relation may change under them.
    let pending = runtime.block_on(state.pending_count()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300",
            "pending runtime rows could not be counted",
            "retry after the local runtime state is available",
            format!("{error:?}"),
        )
    })?;
    let changes = if pending == 0 {
        CommittedRelationChanges::Allowed
    } else {
        CommittedRelationChanges::PendingRuntimeRows
    };
    let admitted = stage_store(&repository, changes)?;
    let capture = runtime.block_on(state.capture()).map_err(|error| {
        Diagnostic::target_with_detail(
            "E2300",
            "runtime candidate could not be captured",
            "retry after the local runtime state is available",
            format!("{error:?}"),
        )
    })?;
    let review = repository
        .review_current_candidate(&capture)
        .map_err(|error| failure(&error, "staged candidate could not be reviewed"))?;
    let commit = repository
        .commit_reviewed_candidate(&review, message, &CommitAssertionGate)
        .map_err(|error| failure(&error, "reviewed candidate could not be committed"))?;
    for line in admitted {
        println!("{line}");
    }
    println!("[{}] {message}", commit.as_str());
    Ok(())
}
