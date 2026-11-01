//! The CLI's production candidate assertion gate (ORNA-ASSERT-042, ORNA-ASSERT-044,
//! ORNA-ASSERT-045, ORNA-ASSERT-048). `orna commit` and `orna publish` both
//! refuse a candidate through this one gate before any ref, pin or journal
//! names it. A checkout target is checked by the same validator through
//! `validate_committed_target`, so no command disagrees about what an admitted
//! assertion means.

use orna_repository_v1::{CandidateAssertionGate, CandidateAssertionRefusal, PrivateCommit};

use super::*;

/// The candidate assertion gate that `orna commit` and `orna publish` run
/// before any ref, pin or journal names a candidate (ORNA-ASSERT-042,
/// ORNA-ASSERT-045, ORNA-ASSERT-048, ORNA-ASSERT-050): the candidate's own rows
/// must satisfy every admitted assertion first.
pub(crate) struct AssertionGate {
    pub(crate) command: GatedCommand,
}

impl CandidateAssertionGate for AssertionGate {
    fn validate(
        &self,
        repository: &orna_repository_v1::Repository,
        candidate: &PrivateCommit,
    ) -> Result<(), CandidateAssertionRefusal> {
        validate_candidate_assertions(repository, candidate, self.command)
    }
}

/// The command whose assertion gate refused a candidate. Its remedy names the
/// retry of that command and what the refusal left unchanged.
#[derive(Clone, Copy)]
pub(crate) enum GatedCommand {
    Commit,
    Publish,
    Checkout,
}

impl GatedCommand {
    /// The word that names the candidate's rows in a diagnostic: a commit
    /// refuses staged rows, a publication refuses pending rows, and a checkout
    /// refuses the rows of its target commit.
    fn candidate_word(self) -> &'static str {
        match self {
            Self::Commit => "staged",
            Self::Publish => "pending",
            Self::Checkout => "target",
        }
    }
}

fn unvalidated(cause: &str) -> CandidateAssertionRefusal {
    CandidateAssertionRefusal::Unvalidated(cause.to_owned())
}

/// Admits the project's tables: their names and schemas, each analysed under
/// the production providers. Fails with the cause when the project cannot be
/// analysed or two admitted tables share a name.
fn admitted_tables_of(
    project: &orna_project_v1::LoadedProject,
) -> Result<(Vec<String>, BTreeMap<String, orna_semantic_v1::TableSchema>), &'static str> {
    let catalogue = semantic_catalogue(project).map_err(|_| "project has no semantic catalogue")?;
    let analysis = orna_semantic_v1::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_semantic_v1::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err("project failed semantic analysis");
    }
    admitted_project_tables(&analysis).ok_or("project tables could not be identified")
}

/// The evaluator rows of every admitted table at `HEAD`, read under `HEAD`'s
/// own schemas and identities. An unborn branch has no rows.
///
/// Interim cost (bead ornadb-t5tz): every gated commit or publish in a project
/// that declares assertions runs this read, which encodes every admitted table
/// at `HEAD` and is bounded by `ApplicationAuthority::committed_read_limits`.
/// Past a limit, or when `HEAD` cannot be read, `changed_tables` treats every
/// candidate table as changed. A cheaper change signal is not adopted yet; the
/// bead names the proof it requires first.
fn head_table_rows(
    repository: &orna_repository_v1::Repository,
) -> Result<orna_runtime_v1::RuntimeTableRows, &'static str> {
    let Some(head) = repository
        .head()
        .map_err(|_| "HEAD could not be resolved")?
    else {
        return Ok(orna_runtime_v1::RuntimeTableRows::new());
    };
    let project = orna_project_v1::ProjectLoader::default()
        .load_committed_snapshot(repository, &head)
        .map_err(|_| "HEAD project could not be loaded")?;
    Ok(match committed_admitted_rows(repository, &head, &project)? {
        Some(committed) => committed.rows,
        None => orna_runtime_v1::RuntimeTableRows::new(),
    })
}

/// One committed commit's admitted tables, read under that commit's own
/// project, object identities and schemas.
struct CommittedRows {
    manifest: orna_project_v1::ObjectIdentityManifest,
    table_object_ids: BTreeMap<String, orna_runtime_v1::TableObjectId>,
    rows: orna_runtime_v1::RuntimeTableRows,
}

/// Reads the admitted tables of `commit` as evaluator rows under the commit's
/// own schemas and identities (ORNA-ASSERT-048: one logical state). `Ok(None)`
/// when `project` admits no table; any unreadable part fails with its cause.
fn committed_admitted_rows(
    repository: &orna_repository_v1::Repository,
    commit: &orna_repository_v1::GitCommitRef,
    project: &orna_project_v1::LoadedProject,
) -> Result<Option<CommittedRows>, &'static str> {
    let (table_names, schemas) = admitted_tables_of(project)?;
    if table_names.is_empty() {
        return Ok(None);
    }
    let manifest =
        orna_project_v1::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .ok()
            .flatten()
            .ok_or("commit carries no object identity inventory")?;
    let identities = table_identities_from_manifest(&manifest, table_names)
        .map_err(|_| "table identities are unavailable")?;
    let table_object_ids: BTreeMap<String, orna_runtime_v1::TableObjectId> = identities
        .iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect();
    let format = repository
        .open_pinned_format_context(commit.as_str())
        .map_err(|_| "format context could not be opened")?;
    let mut committed = orna_project_v1::committed_tables(
        repository,
        &format,
        commit,
        &schemas,
        &table_object_ids,
        orna_application_v1::ApplicationAuthority::committed_read_limits(),
    )
    .map_err(|_| "committed rows could not be read")?;
    let mut rows = orna_runtime_v1::RuntimeTableRows::new();
    for (name, schema) in &schemas {
        let table = committed.remove(name).ok_or("table rows are missing")?;
        let encoded = orna_project_v1::evaluator_rows(&table, schema)
            .map_err(|_| "table rows could not be encoded")?;
        rows.insert(name.clone(), encoded);
    }
    Ok(Some(CommittedRows {
        manifest,
        table_object_ids,
        rows,
    }))
}

/// The candidate tables whose evaluator rows differ from `HEAD`. A module
/// assertion is evaluated when its dependency set intersects this set
/// (ORNA-ASSERT-026). A candidate table absent from `HEAD` is changed when it
/// stages rows. When `HEAD` cannot be read, every candidate table counts as
/// changed, so more checks run and none is skipped.
fn changed_tables(
    repository: &orna_repository_v1::Repository,
    rows: &orna_runtime_v1::RuntimeTableRows,
) -> BTreeSet<String> {
    let head = head_table_rows(repository);
    rows.iter()
        .filter(|(name, entries)| match &head {
            Ok(head) => head
                .get(*name)
                .map_or(!entries.is_empty(), |committed| committed != *entries),
            Err(_) => true,
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Evaluates the admitted assertions over the candidate's own rows. Table
/// assertions always run (ORNA-ASSERT-042); a module assertion runs only when a
/// table in its dependency set changed against `HEAD` (ORNA-ASSERT-026). A check
/// that reads a table whose primary key the commit seam cannot lower (bead
/// ornadb-gq4g) is refused as unvalidated, and no other check is refused for
/// it. A candidate that cannot be validated whole is refused whole, never
/// committed unchecked.
fn validate_candidate_assertions(
    repository: &orna_repository_v1::Repository,
    candidate: &PrivateCommit,
    command: GatedCommand,
) -> Result<(), CandidateAssertionRefusal> {
    let word = command.candidate_word();
    let project = orna_project_v1::ProjectLoader::default()
        .load_private_candidate(repository, candidate)
        .map_err(|_| unvalidated(&format!("{word} project could not be loaded")))?;
    if !orna_conformance_v1::project_declares_assertions(&project) {
        return Ok(());
    }
    let (table_names, schemas) =
        admitted_tables_of(&project).map_err(|cause| unvalidated(&format!("{word} {cause}")))?;
    if table_names.is_empty() {
        return Ok(());
    }
    let manifest = orna_project_v1::ObjectIdentityManifest::read_private_candidate(
        repository, candidate,
    )
    .ok()
    .flatten()
    .ok_or_else(|| unvalidated(&format!("{word} object identities are unavailable")))?;
    let identities = table_identities_from_manifest(&manifest, table_names)
        .map_err(|_| unvalidated(&format!("{word} table identities are unavailable")))?;
    let table_object_ids: BTreeMap<String, orna_runtime_v1::TableObjectId> = identities
        .iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect();
    let format = repository
        .open_private_candidate_format_context(candidate)
        .map_err(|_| unvalidated(&format!("{word} format context could not be opened")))?;
    // The candidate's own rows, read from the private candidate rather than HEAD.
    let committed = orna_project_v1::private_candidate_tables(
        repository,
        candidate,
        &format,
        &schemas,
        &table_object_ids,
        orna_application_v1::ApplicationAuthority::committed_read_limits(),
    )
    .map_err(|_| unvalidated(&format!("{word} committed rows could not be read")))?;
    let mut rows = orna_runtime_v1::RuntimeTableRows::new();
    for (name, schema) in &schemas {
        let table = committed
            .get(name)
            .ok_or_else(|| unvalidated(&format!("{word} table rows are missing")))?;
        let encoded = orna_project_v1::evaluator_rows(table, schema)
            .map_err(|_| unvalidated(&format!("{word} table rows could not be encoded")))?;
        rows.insert(name.clone(), encoded);
    }
    let changed = changed_tables(repository, &rows);
    validate_changed_rows(&project, &manifest, table_object_ids, &rows, &changed, word)
}

/// Runs the production validator over the changed tables of `rows`. A false
/// table or module assertion refuses with the owning module's span
/// (ORNA-ASSERT-051); any other failure refuses the candidate as unvalidated,
/// so it is never committed unchecked.
fn validate_changed_rows(
    project: &orna_project_v1::LoadedProject,
    manifest: &orna_project_v1::ObjectIdentityManifest,
    table_object_ids: BTreeMap<String, orna_runtime_v1::TableObjectId>,
    rows: &orna_runtime_v1::RuntimeTableRows,
    changed: &BTreeSet<String>,
    word: &str,
) -> Result<(), CandidateAssertionRefusal> {
    let mut validator = orna_conformance_v1::transactional_loaded_project_commit_validator(
        project,
        manifest,
        std::sync::Arc::new(table_object_ids),
        orna_evaluator_v1::Limits::default(),
    )
    .map_err(|outcome| unvalidated(&stage_outcome_cause(&outcome)))?;
    validator
        .validate_changed(rows, changed)
        .map_err(|diagnostic| {
            if orna_conformance_v1::is_table_assertion_false(diagnostic) {
                CandidateAssertionRefusal::AssertionFalse(validator.failed_module_assertion())
            } else {
                unvalidated(&format!(
                    "an assertion could not be evaluated over the {word} rows: {:?}",
                    diagnostic.code
                ))
            }
        })
}

/// Validates the committed `target` that a checkout would make current
/// (ORNA-ASSERT-044). A target that changes no admitted table's rows against
/// `HEAD` changes no logical state, so it is accepted without evaluation.
/// Otherwise its own rows must satisfy every admitted assertion, as a
/// candidate's rows must before commit.
pub(crate) fn validate_committed_target(
    repository: &orna_repository_v1::Repository,
    target: &orna_repository_v1::GitCommitRef,
) -> Result<(), CandidateAssertionRefusal> {
    let word = GatedCommand::Checkout.candidate_word();
    let project = orna_project_v1::ProjectLoader::default()
        .load_committed_snapshot(repository, target)
        .map_err(|_| unvalidated(&format!("{word} project could not be loaded")))?;
    if !orna_conformance_v1::project_declares_assertions(&project) {
        return Ok(());
    }
    let Some(committed) = committed_admitted_rows(repository, target, &project)
        .map_err(|cause| unvalidated(&format!("{word} {cause}")))?
    else {
        return Ok(());
    };
    let changed = changed_tables(repository, &committed.rows);
    if changed.is_empty() {
        return Ok(());
    }
    validate_changed_rows(
        &project,
        &committed.manifest,
        committed.table_object_ids,
        &committed.rows,
        &changed,
        word,
    )
}

/// The refusal of a candidate by its assertion gate (ORNA-ASSERT-048 to
/// ORNA-ASSERT-051). The title names a false module assertion's owner and span
/// (ORNA-ASSERT-051), and carries the cause an unvalidated candidate could not
/// be checked for (ORNA-ASSERT-049), since attached details show only with
/// `--debug`.
pub(crate) fn refusal_diagnostic(
    refusal: &CandidateAssertionRefusal,
    command: GatedCommand,
    error: &orna_repository_v1::RepositoryError,
) -> Diagnostic {
    let word = command.candidate_word();
    let (code, title, help) = match refusal {
        CandidateAssertionRefusal::AssertionFalse(site) => {
            let title = match site {
                Some(site) => format!("{site} is false over the {word} rows"),
                None => format!("table assertion is false over the {word} rows"),
            };
            let help = match command {
                GatedCommand::Commit => "change the staged rows or source so every assertion holds, stage them, then retry `orna commit -m MESSAGE`; no commit was created",
                GatedCommand::Publish => "change the pending rows or source so every assertion holds, then retry `orna publish`; nothing was published",
                GatedCommand::Checkout => "change the target commit's source or rows so every assertion holds, then retry the checkout; HEAD and the working tree were not changed",
            };
            ("ORNA-A091-009", title, help)
        }
        CandidateAssertionRefusal::Unvalidated(cause) => {
            let help = match command {
                GatedCommand::Commit => "fix the staged source or store, then retry `orna commit -m MESSAGE`; no commit was created",
                GatedCommand::Publish => "fix the pending source or store, then retry `orna publish`; nothing was published",
                GatedCommand::Checkout => "fix the target commit's source or store, then retry the checkout; HEAD and the working tree were not changed",
            };
            (
                "E2300",
                format!("{word} candidate could not be validated against its assertions: {cause}"),
                help,
            )
        }
    };
    Diagnostic {
        code,
        title: std::borrow::Cow::Owned(title),
        help,
        exit: Exit::Target,
        detail: Some(format!("{error:?}")),
        body: None,
    }
}
