use super::{
    AdmittedReplSession, ApplicationAuthority, Arc, BTreeMap, BTreeSet, Diagnostic, Digest,
    Endpoint, Limits, NoFault, Rc, RefCell, ReplError, ReplRuntime, ReplTransactions, RuntimeError,
    RuntimeIdentity, RuntimeMode, RuntimeState, Sha256, SysHostBindingRegistry, SysHostContext,
    TableObjectId, committed_table_identities, project_load_diagnostic, repl, request_nonce,
    runtime_identity, semantic_catalogue, sys_host,
};

pub(crate) fn repl_transactions(
    repository: &orna_repository::Repository,
    workspace_project: &orna_project::LoadedProject,
) -> Result<ReplTransactions, repl::ReplFailure> {
    let session_id = request_nonce().to_be_bytes();
    let (commit, format_context) = repository
        .open_pinned_format_context_with_commit("HEAD")
        .map_err(|_| {
            transactions_unavailable(
                "the repository has no committed HEAD with a readable store",
                STORE_COMMIT_REMEDY,
            )
        })?;
    let format_context = format_context.with_runtime_owner_id(session_id);
    check_writable_format(&format_context)?;
    let project = orna_project::ProjectLoader::default()
        .load_committed_snapshot(repository, &commit)
        .map_err(|_| {
            transactions_unavailable(
                "the committed project could not be loaded",
                STORE_COMMIT_REMEDY,
            )
        })?;
    if !same_repl_project_inputs(workspace_project, &project) {
        return Err(transactions_unavailable(
            "the project has changes that are not committed at HEAD",
            "commit the project with `orna commit -m MESSAGE` or restore it to HEAD, then restart the REPL",
        ));
    }
    let catalogue = semantic_catalogue(&project).map_err(|_| {
        transactions_unavailable(
            "the committed project does not load its semantic catalogue",
            "fix the first reported source error, commit it, then restart the REPL",
        )
    })?;
    let analysis = orna_check::analyze_with_providers(
        project.modules(),
        &catalogue,
        orna_check::SystemProviders::Production,
    );
    if !analysis.is_ok() {
        return Err(transactions_unavailable(
            "the committed project does not pass semantic analysis",
            "fix the first reported source error, commit it, then restart the REPL",
        ));
    }
    let (table_names, table_schemas) = admitted_project_tables(&analysis).ok_or_else(|| {
        transactions_unavailable(
            "two admitted tables share a qualified name",
            "rename one of the tables, commit it, then restart the REPL",
        )
    })?;
    if table_names.is_empty() {
        return Err(transactions_unavailable(
            "the project declares no admitted table for REPL writes",
            "declare an admitted table, commit it, then restart the REPL",
        ));
    }
    let tables = committed_table_identities(repository, &commit, table_names).map_err(|_| {
        transactions_unavailable(
            "committed table identities are unavailable",
            STORE_COMMIT_REMEDY,
        )
    })?;
    let identities = orna_project::ObjectIdentityManifest::read_committed(repository, &commit)
        .map_err(|_| {
            transactions_unavailable(
                "committed object identities are unavailable",
                STORE_COMMIT_REMEDY,
            )
        })?;
    let table_object_ids: Arc<BTreeMap<String, TableObjectId>> = Arc::new(
        tables
            .iter()
            .map(|table| (table.table().to_owned(), table.object_id()))
            .collect(),
    );
    let capture_relation_id = tables
        .first()
        .ok_or_else(|| {
            transactions_unavailable(
                "the committed project has no table identity to capture",
                STORE_COMMIT_REMEDY,
            )
        })?
        .object_id()
        .bytes();
    let (identity, initial_digest) = committed_store_identity(repository, &format_context)?;
    let runtime = repl_tokio_runtime()?;
    Ok(ReplTransactions {
        project,
        commit,
        format_context,
        authority: ApplicationAuthority::new(catalogue, Limits::default()).with_sys_host_context(
            sys_host::sys_host_context(
                repository.worktree(),
                identity,
                RuntimeMode::Repl,
                sys_host::std_snapshot_pin(workspace_project),
            ),
        ),
        runtime,
        identity,
        initial_digest,
        tables,
        identities,
        table_object_ids,
        table_schemas,
        capture_relation_id,
        session_id,
        host_bindings: Rc::new(RefCell::new(None)),
        faults: Arc::new(NoFault),
        writer: None,
    })
}

/// Refuses the REPL unless the pinned format accepts writes and its final
/// format capability is supported.
fn check_writable_format(
    format_context: &orna_repository::RepositoryFormatContext,
) -> Result<(), repl::ReplFailure> {
    if !format_context.supports_writes() {
        return Err(transactions_unavailable(
            "this repository's format does not accept REPL writes",
            "open a repository whose format supports writes, then restart the REPL",
        ));
    }
    if format_context.final_format_capability().is_err() {
        return Err(transactions_unavailable(
            "this repository's format capability is not supported for REPL writes",
            "open a repository whose format supports writes, then restart the REPL",
        ));
    }
    Ok(())
}

/// Reads the committed store's runtime identity and checks that its database
/// identity matches the format context's database identity.
fn committed_store_identity(
    repository: &orna_repository::Repository,
    format_context: &orna_repository::RepositoryFormatContext,
) -> Result<(RuntimeIdentity, [u8; 32]), repl::ReplFailure> {
    let (identity, initial_digest) = runtime_identity(repository).map_err(|_| {
        transactions_unavailable(
            "the runtime identity of the committed store could not be read",
            STORE_COMMIT_REMEDY,
        )
    })?;
    let database_id = format_context.require_database_id().map_err(|_| {
        transactions_unavailable("the store has no database identity", STORE_COMMIT_REMEDY)
    })?;
    if database_id.as_bytes() != &identity.database_id {
        return Err(transactions_unavailable(
            "the store's database identity does not match its committed runtime identity",
            "restore the committed store or commit it with `orna commit -m MESSAGE`, then restart the REPL",
        ));
    }
    Ok((identity, initial_digest))
}

/// Starts the single-threaded tokio runtime that drives REPL requests.
fn repl_tokio_runtime() -> Result<tokio::runtime::Runtime, repl::ReplFailure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            transactions_unavailable(
                "the REPL runtime could not be started",
                "retry the REPL; if it persists, check the host's resource limits",
            )
        })
}

/// The remedy when the committed store or its identities are missing or
/// unreadable: the store must be committed at HEAD before writes can run.
pub(crate) const STORE_COMMIT_REMEDY: &str =
    "commit the project and its store with `orna commit -m MESSAGE`, then restart the REPL";

/// A located `E2200` failure: the project REPL has no writable transaction
/// capability, and the message and remedy say why and how to recover.
pub(crate) fn transactions_unavailable(message: &str, remedy: &str) -> repl::ReplFailure {
    repl::ReplFailure::located("E2200", message, vec![remedy.to_owned()])
}

/// Collects the admitted tables of an analyzed project: every table whose
/// schema carries an admission, under its qualified name, in module order.
/// Returns `None` when two admitted tables share a qualified name.
pub(crate) fn admitted_project_tables(
    analysis: &orna_check::Analysis,
) -> Option<(Vec<String>, BTreeMap<String, orna_check::TableSchema>)> {
    let mut table_names = Vec::new();
    let mut table_schemas = BTreeMap::new();
    for (namespace, module) in &analysis.modules {
        for (name, symbol) in &module.symbols {
            let Some(schema) = symbol.table_schema.as_ref() else {
                continue;
            };
            if symbol.kind == orna_check::SymbolKind::Table && schema.admission.is_some() {
                let table = match namespace.display().as_str() {
                    "" => name.clone(),
                    prefix => format!("{prefix}.{name}"),
                };
                table_schemas.insert(table.clone(), schema.clone());
                table_names.push(table);
            }
        }
    }
    if table_names.iter().collect::<BTreeSet<_>>().len() != table_names.len() {
        return None;
    }
    Some((table_names, table_schemas))
}

pub(crate) fn same_repl_project_inputs(
    workspace: &orna_project::LoadedProject,
    committed: &orna_project::LoadedProject,
) -> bool {
    workspace.modules() == committed.modules()
        && workspace.identities() == committed.identities()
        && workspace.loose_rows() == committed.loose_rows()
        && workspace.standard_profile() == committed.standard_profile()
        && workspace.standard_sources() == committed.standard_sources()
        && workspace.standard_runtime_modules() == committed.standard_runtime_modules()
        && workspace.has_standard_imports() == committed.has_standard_imports()
        && workspace.standard_modules() == committed.standard_modules()
}

/// The activation facts a project REPL session reports through
/// `sys.rt.info()`. `None` leaves the session unattached, so the evaluator
/// reports `sys.version.incompatible` rather than inventing host facts.
pub(crate) fn repl_sys_host_context(
    repository: &orna_repository::Repository,
    project: &orna_project::LoadedProject,
) -> Option<SysHostContext> {
    let (identity, _) = runtime_identity(repository).ok()?;
    Some(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        RuntimeMode::Repl,
        sys_host::std_snapshot_pin(project),
    ))
}

pub(crate) fn repl_runtime(
    repository: Option<orna_repository::Repository>,
) -> Result<ReplRuntime, Diagnostic> {
    let (session, transactions) = if let Some(repository) = &repository {
        let project = orna_project::ProjectLoader::default()
            .load(repository)
            .map_err(project_load_diagnostic)?;
        let session = AdmittedReplSession::from_loaded_project(
            &project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .map_err(|error| repl_session_error(&error))?;
        let session = match repl_sys_host_context(repository, &project) {
            Some(context) => session.with_sys_host_context(context),
            None => session,
        };
        let transactions = repl_transactions(repository, &project);
        (session, transactions)
    } else {
        // Preserve the core-only REPL when the current directory has no Git
        // repository or the endpoint is not local.
        (
            AdmittedReplSession::new(Limits::default()),
            Err(transactions_unavailable(
                "no project repository is bound to this REPL",
                "run the REPL inside a Git worktree to write project tables",
            )),
        )
    };
    Ok(ReplRuntime {
        repository,
        session,
        transactions,
    })
}

pub(crate) fn repl_session(endpoint: &Endpoint) -> Result<ReplRuntime, Diagnostic> {
    repl_runtime(repl_repository(endpoint)?)
}

/// The repository a REPL endpoint binds to, or `None` for the core-only session.
pub(crate) fn repl_repository(
    endpoint: &Endpoint,
) -> Result<Option<orna_repository::Repository>, Diagnostic> {
    let repository = match endpoint {
        // The legacy implicit local REPL remains core-only when no worktree is
        // present. Keep the successful discovery result as session authority.
        Endpoint::ManagedLocal => orna_repository::Repository::discover(".").ok(),
        Endpoint::Path(path) => {
            Some(orna_repository::Repository::discover(path).map_err(|_| {
                Diagnostic::target(
                    "E2100",
                    "project Git worktree could not be discovered",
                    "run the command inside a Git worktree or provide a local project path",
                )
            })?)
        }
        // Snapshot selectors are deliberately repository-backed. Keep a
        // non-local REPL core-only so the loader can reject `:at` uniformly as
        // ORNA-REPL-AT instead of failing before the command loop starts.
        Endpoint::UnixSocket(_) | Endpoint::RemoteTls(_) => None,
    };
    Ok(repository)
}

pub(crate) fn repl_session_error(error: &ReplError) -> Diagnostic {
    if matches!(error.code(), "ORNA-REPL-STANDARD" | "ORNA-S010-IMPORT") {
        Diagnostic::target(
            "ORNA-S010-IMPORT",
            "imported module is unavailable",
            "use a captured standard dependency or remove the import",
        )
    } else if error.code().starts_with("ORNA-S") || error.code() == "ORNA-REPL-SEMANTIC" {
        Diagnostic::target(
            "E2101",
            "project semantic analysis failed",
            "fix the first reported source contract error before starting a project REPL session",
        )
    } else {
        Diagnostic::target_with_detail(
            "E2200",
            "project REPL session is not available",
            "use a project supported by the configured REPL runtime",
            error.code().to_owned(),
        )
    }
}

/// Read-only bridge from REPL `:at` selectors to project admission.
///
/// The REPL owns replacement semantics: this loader only returns an owned,
/// fully admitted candidate once the selected source set has loaded. In
/// particular, it never checks out a commit or changes Git's index, worktree,
/// HEAD, or refs.
pub(crate) struct ReplSnapshotSessionLoader<'a> {
    pub(crate) repository: Option<&'a orna_repository::Repository>,
    pub(crate) host_bindings: Rc<RefCell<Option<SysHostBindingRegistry>>>,
}

impl ReplSnapshotSessionLoader<'_> {
    pub(crate) fn repository(&self) -> Result<&orna_repository::Repository, ()> {
        self.repository.ok_or(())
    }

    pub(crate) fn admit(
        &self,
        project: &orna_project::LoadedProject,
    ) -> Result<AdmittedReplSession, ()> {
        // Admit the project's own pinned standard sources; uncaptured imports
        // remain inadmissible because the project has no standard profile.
        let session = AdmittedReplSession::from_loaded_project(
            project,
            project.standard_sources().iter().cloned(),
            Limits::default(),
        )
        .map_err(|_| ())?;
        let context = self
            .repository
            .and_then(|repository| repl_sys_host_context(repository, project));
        Ok(match context {
            Some(context) => session.with_sys_host_context(context),
            None => session,
        })
    }
}

impl repl::SnapshotSessionLoader for ReplSnapshotSessionLoader<'_> {
    type Error = ();

    fn load_snapshot(
        &self,
        target: repl::SnapshotTarget,
    ) -> Result<AdmittedReplSession, Self::Error> {
        let session = match target {
            repl::SnapshotTarget::Cwd => {
                let project = orna_project::ProjectLoader::default()
                    .load(self.repository()?)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
            repl::SnapshotTarget::Head => {
                let repository = self.repository()?;
                let commit = repository.head().map_err(|_| ())?.ok_or(())?;
                let project = orna_project::ProjectLoader::default()
                    .load_committed_snapshot(repository, &commit)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
            repl::SnapshotTarget::Ref(reference) => {
                let repository = self.repository()?;
                let commit = repository.resolve_snapshot(&reference).map_err(|_| ())?;
                let project = orna_project::ProjectLoader::default()
                    .load_committed_snapshot(repository, &commit)
                    .map_err(|_| ())?;
                self.admit(&project)?
            }
        };
        self.host_bindings.borrow_mut().take();
        Ok(session)
    }
}

/// A located `ORNA-REPL-EFFECT` failure for a project binding the REPL cannot
/// honour, naming what is wrong and the remedy to apply.
pub(crate) fn repl_effect_failure(message: &str, remedy: &str) -> repl::ReplFailure {
    repl::ReplFailure::located("ORNA-REPL-EFFECT", message, vec![remedy.to_owned()])
}

/// Locates a REPL refusal to take the repository writer lease (ORNA-TURSO-005).
/// `LeaseHeld` from a process that could not take the runtime owner lock means a
/// live writer, such as a mounted editor, holds it; otherwise another owner is
/// bound to this lock. Any other lease failure keeps the bare code.
pub(crate) fn repl_lease_failure(error: &RuntimeError, state: &RuntimeState) -> repl::ReplFailure {
    if *error != RuntimeError::LeaseHeld {
        return repl::ReplFailure::from("ORNA-REPL-EFFECT".to_owned());
    }
    if state.holds_runtime_owner_lock() {
        // This process holds the runtime owner lock, so no mount is implicated
        // on this path and no remedy is offered for it.
        return repl::ReplFailure::located(
            "ORNA-REPL-EFFECT",
            "another writer owns the repository writer lease for this runtime owner lock",
            Vec::new(),
        );
    }
    repl_effect_failure(
        "the repository writer lease is held by a live writer: the runtime owner lock is held by another live process (such as a mounted editor), so this REPL cannot take the writer lease",
        "REPL database writes need the mount stopped: run `orna unmount DIR`, then rerun this REPL. File writes while it is mounted go through `orna edit`",
    )
}

/// Binds one REPL request row to its session, query, source text and the
/// project digest it was evaluated against, so a replayed identity cannot
/// adopt a different request's effects.
pub(crate) fn repl_request_fingerprint(
    session_id: [u8; 16],
    query_id: [u8; 16],
    source: &str,
    initial_digest: [u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"orna.repl.request.v1");
    hasher.update(session_id);
    hasher.update(query_id);
    hasher.update(initial_digest);
    hasher.update(
        u64::try_from(source.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(source.as_bytes());
    hasher.finalize().into()
}
