use super::*;

/// Runtime authority retained by one CLI REPL session.
///
/// Project loading and later snapshot selection share this opened repository;
/// callers can also use it as the explicit injection point for repository
/// capabilities without process-global state.
pub(crate) struct ReplRuntime {
    pub(crate) repository: Option<orna_repository::Repository>,
    pub(crate) session: AdmittedReplSession,
    pub(crate) transactions: Option<ReplTransactions>,
}

pub(crate) struct ReplTransactions {
    pub(crate) project: orna_project::LoadedProject,
    pub(crate) commit: orna_repository::GitCommitRef,
    pub(crate) format_context: orna_repository::RepositoryFormatContext,
    pub(crate) authority: ApplicationAuthority,
    pub(crate) runtime: tokio::runtime::Runtime,
    pub(crate) identity: RuntimeIdentity,
    pub(crate) initial_digest: [u8; 32],
    pub(crate) tables: Vec<RuntimeTableIdentity>,
    pub(crate) identities: orna_project::ObjectIdentityManifest,
    pub(crate) table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    pub(crate) table_schemas: BTreeMap<String, orna_check::TableSchema>,
    pub(crate) capture_relation_id: [u8; 16],
    pub(crate) session_id: [u8; 16],
    pub(crate) host_bindings: Rc<RefCell<Option<SysHostBindingRegistry>>>,
    pub(crate) faults: Arc<dyn FaultInjector>,
    // Drop the runtime owner lock only after capture candidates release their
    // refs, so the writer holding it stays the last field.
    pub(crate) writer: Option<ReplWriter>,
}

impl ReplTransactions {
    /// The runtime state this session's first input opened, for tests that
    /// inspect the request rows it left behind.
    #[cfg(test)]
    pub(crate) fn writer_state(&self) -> Option<&RuntimeState> {
        self.writer.as_ref().map(|writer| &writer.state)
    }
}

/// Reads the committed identity of each named admitted table from `commit`.
/// Each name must resolve to exactly one live table record; the error names the
/// table that does not, so the caller fails closed with a reason. The REPL and
/// one-shot `run` both bind tables through this function, so neither resolves a
/// table by name alone.
/// The stored field order of every admitted table at HEAD.
///
/// A published row is positional in this order, which is not the declaration
/// order, so the run path rebuilds seeded records from it (bead ornadb-jm0d).
pub(crate) fn committed_storage_orders(
    repository: &orna_repository::Repository,
    project: &orna_project::LoadedProject,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, Diagnostic> {
    let (table_names, schemas) = crate::assertion_gate::admitted_tables_of(project)
        .map_err(|_| unavailable_table_identity("project tables could not be identified".into()))?;
    let Ok(Some(head)) = repository.head() else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Ok(tables) = committed_table_identities(repository, &head, table_names) else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Ok((_rows, storage_orders)) = committed_rows_at(repository, &head, &schemas, &tables)
    else {
        return Ok(std::collections::BTreeMap::new());
    };
    Ok(storage_orders)
}

pub(crate) fn committed_table_object_ids(
    repository: &orna_repository::Repository,
    project: &orna_project::LoadedProject,
) -> Result<std::collections::BTreeMap<String, TableObjectId>, Diagnostic> {
    let (table_names, _schemas) = crate::assertion_gate::admitted_tables_of(project)
        .map_err(|_| unavailable_table_identity("project tables could not be identified".into()))?;
    // A repository with no commit yet has no admitted identities, and the run
    // still has to execute (bead ornadb-jm0d).
    let Ok(Some(head)) = repository.head() else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Ok(identities) = committed_table_identities(repository, &head, table_names) else {
        return Ok(std::collections::BTreeMap::new());
    };
    Ok(identities
        .into_iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect())
}

pub(crate) fn committed_table_identities(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    table_names: Vec<String>,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let identities =
        orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| {
                unavailable_table_identity(
                    "the committed object identity manifest could not be read".to_owned(),
                )
            })?
            .ok_or_else(|| {
                unavailable_table_identity(
                    "the commit carries no object identity inventory".to_owned(),
                )
            })?;
    table_identities_from_manifest(&identities, table_names)
}

/// Resolves each named table to its one live identity in `identities`. Each
/// name must resolve to exactly one live table record, so no caller binds a
/// table by name alone.
pub(crate) fn table_identities_from_manifest(
    identities: &orna_project::ObjectIdentityManifest,
    table_names: Vec<String>,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let mut tables = Vec::with_capacity(table_names.len());
    for name in table_names {
        let mut matching = identities.records().iter().filter(|record| {
            record.kind() == orna_project::ObjectKind::Table
                && !record.is_retired()
                && record.qualified_name() == name.as_str()
        });
        let record = matching.next().ok_or_else(|| {
            unavailable_table_identity(format!("table `{name}` has no live identity at HEAD"))
        })?;
        if matching.next().is_some() {
            return Err(unavailable_table_identity(format!(
                "table `{name}` has more than one live identity at HEAD"
            )));
        }
        let identity = RuntimeTableIdentity::new(name.clone(), TableObjectId::new(*record.id()))
            .map_err(|_| {
                unavailable_table_identity(format!(
                    "table `{name}` has an invalid identity at HEAD"
                ))
            })?;
        tables.push(identity);
    }
    Ok(tables)
}

pub(crate) fn unavailable_table_identity(detail: String) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2200",
        "committed table identities are unavailable",
        "run `orna commit -m MESSAGE` so every table has exactly one identity",
        detail,
    )
}

/// Returns the first module table whose rows still sit under its bare name. A
/// module table `library.Book` is stored as `library.Book`; earlier builds
/// stored it as `Book`, which the qualified table cannot see: reads miss those
/// rows and writes fork the relation. `candidates` comes from
/// [`legacy_bare_module_candidates`].
pub(crate) fn legacy_bare_module_table(
    state: &RuntimeState,
    runtime: &tokio::runtime::Runtime,
    candidates: &[(String, String)],
) -> Result<Option<String>, orna_runtime::RuntimeError> {
    for (qualified, bare) in candidates {
        if runtime.block_on(state.has_table_rows(bare))? {
            return Ok(Some(qualified.clone()));
        }
    }
    Ok(None)
}

/// The module tables whose rows may sit under a bare name, as `(qualified,
/// bare)` pairs. `declared` are the qualified names the caller admits; when
/// `commit` is given, every table recorded in its committed identity manifest
/// is added, retired ones included. A live `ns.B` is a candidate unless a table
/// named `B`, live or retired, exists: a bare name a root table owns, even one
/// since removed, is that table's data, never a legacy module row.
pub(crate) fn legacy_bare_module_candidates(
    repository: &orna_repository::Repository,
    commit: Option<&orna_repository::GitCommitRef>,
    declared: &[&str],
) -> Result<Vec<(String, String)>, orna_project::ObjectIdentityError> {
    let mut tables = declared
        .iter()
        .map(|name| ((*name).to_owned(), false))
        .collect::<Vec<_>>();
    let manifest = match commit {
        Some(commit) => Some(orna_project::ObjectIdentityManifest::read_committed(
            repository, commit,
        )?),
        None => None,
    };
    if let Some(manifest) = manifest {
        tables.extend(
            manifest
                .records()
                .iter()
                .filter(|record| record.kind() == orna_project::ObjectKind::Table)
                .map(|record| (record.qualified_name().to_owned(), record.is_retired())),
        );
    }
    let owned = tables
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let mut candidates = BTreeSet::new();
    for (qualified, retired) in &tables {
        if *retired {
            continue;
        }
        let Some((_, bare)) = qualified.rsplit_once('.') else {
            continue;
        };
        if !owned.contains(bare) {
            candidates.insert((qualified.clone(), bare.to_owned()));
        }
    }
    Ok(candidates.into_iter().collect())
}

/// Refuses a multi-module durable invocation whose store holds rows of a
/// declared module table under its bare name. The storage names are derived
/// from each module's namespace and table declarations, as admission names them.
/// The candidates come from the committed identity manifest at `HEAD` when a
/// commit exists, so a removed root table keeps its bare name.
pub(crate) fn refuse_legacy_bare_module_rows(
    repository: &orna_repository::Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    project: &orna_project::LoadedProject,
) -> Result<(), Diagnostic> {
    let mut declared = Vec::new();
    for (module, module_identity) in project.modules().iter().zip(project.identities()) {
        for item in orna_syntax::parse_module(&module.source).value.items {
            if let orna_syntax_ast::Declaration::Table { name, .. } = item.declaration {
                let mut qualified = module_identity.namespace().to_vec();
                qualified.push(name);
                declared.push(qualified.join("."));
            }
        }
    }
    if declared.is_empty() {
        return Ok(());
    }
    let unreadable = || {
        Diagnostic::target(
            "E2200",
            "committed table rows could not be read",
            "retry the invocation after checking local runtime state",
        )
    };
    // An unborn HEAD has no committed table relation, so only the declared
    // names can name legacy rows. An unreadable HEAD is not evidence of
    // absence: refuse it.
    let Ok(commit) = repository.head() else {
        return Err(Diagnostic::target(
            "E2200",
            "committed HEAD is unreadable",
            "repair the repository HEAD reference, then run again",
        ));
    };
    if let Some(commit) = &commit {
        // A commit without an identity inventory never allocated its tables'
        // identities. Evaluating its table statements would touch no committed
        // relation, so refuse with the remedy that allocates the identities.
        if orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| unreadable())?
            .is_none()
        {
            return Err(unavailable_table_identity(
                "the commit carries no object identity inventory".to_owned(),
            ));
        }
    }
    let declared = declared.iter().map(String::as_str).collect::<Vec<_>>();
    let candidates = legacy_bare_module_candidates(repository, commit.as_ref(), &declared)
        .map_err(|_| unreadable())?;
    if candidates.is_empty() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| unreadable())?;
    let state = runtime
        .block_on(RuntimeState::open(repository, identity, initial_digest))
        .map_err(|_| unreadable())?;
    match legacy_bare_module_table(&state, &runtime, &candidates).map_err(|_| unreadable())? {
        Some(table) => Err(legacy_module_table_diagnostic(&table)),
        None => Ok(()),
    }
}

pub(crate) fn legacy_module_table_diagnostic(table: &str) -> Diagnostic {
    Diagnostic::target_with_detail(
        "E2200",
        "committed table rows are stored under a bare module name",
        "re-create the store from the committed project; rows under bare module names are not migrated",
        format!("module table `{table}` has rows stored under its bare name by an earlier build"),
    )
}

/// Committed `HEAD` rows of every admitted table, pinned to the published
/// generation they were read at. The rows are stored tuples in the runtime
/// keyspace: the runtime merges them with the live tail, so they stay in the
/// stored form. `storage_orders` names the stored fields each table's tuples
/// list, which is what converts the merged rows to evaluator records.
pub(crate) struct PinnedCommittedTables {
    pub(crate) rows: PinnedCommittedRows,
    pub(crate) storage_orders: BTreeMap<String, Vec<String>>,
}

/// Evaluates an admitted module over the activation at `state`: the snapshot
/// holds the committed rows of every admitted table, and the staged result
/// carries only the mutations this activation produced. `publication_rows`
/// supplies the durable publication snapshot when the module scans `sys`
/// relations.
///
/// This is the single merge point where stored tuples become evaluator records:
/// the runtime merges the committed base with the live tail in stored form, and
/// the admitted schemas then receive evaluator form only.
pub(crate) fn evaluate_over_committed_tables(
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    authority: &ApplicationAuthority,
    admitted: &AdmittedApplication,
    tables: &[RuntimeTableIdentity],
    committed: PinnedCommittedTables,
    publication_rows: Option<RuntimePublicationMetadataRows>,
) -> Result<(RuntimeTableActivationSnapshot, StagedActivation), Diagnostic> {
    let PinnedCommittedTables {
        rows,
        storage_orders,
    } = committed;
    let snapshot = runtime
        .block_on(state.begin_admitted_table_activation_over_committed(tables, rows))
        .map_err(|_| {
            Diagnostic::target(
                "E2200",
                "committed table rows could not be read",
                "retry the invocation after checking local runtime state",
            )
        })?;
    let evaluator_rows = evaluator_snapshot_rows(&snapshot, admitted, &storage_orders)?;
    let staged = authority
        .evaluate_staged_with_table_snapshot_rows(
            admitted,
            &Environment::new(),
            &snapshot,
            evaluator_rows,
            publication_rows,
        )
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2200",
                "pure project invocation failed",
                "fix the admitted function or retry the invocation",
                error.to_string(),
            )
        })?;
    Ok((snapshot, staged))
}

/// Converts every admitted table's merged stored rows in `snapshot` to
/// evaluator records, keyed by table name.
pub(crate) fn evaluator_snapshot_rows(
    snapshot: &RuntimeTableActivationSnapshot,
    admitted: &AdmittedApplication,
    storage_orders: &BTreeMap<String, Vec<String>>,
) -> Result<RuntimeTableRows, Diagnostic> {
    let mut rows = BTreeMap::new();
    for (name, table) in admitted.table_schemas() {
        let storage_order = storage_orders
            .get(name)
            .ok_or_else(committed_rows_read_error)?;
        let stored = snapshot
            .table_rows()
            .get(name)
            .map_or(&[][..], Vec::as_slice);
        let table_rows =
            orna_project::evaluator_rows_from_stored(table.schema(), storage_order, stored)
                .map_err(|_| committed_rows_read_error())?;
        rows.insert(name.clone(), table_rows);
    }
    Ok(rows)
}

/// Reads the committed rows of every admitted table at `commit` and pins them
/// to the runtime's published generation.
pub(crate) fn committed_invocation_rows(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    admitted: &AdmittedApplication,
    tables: &[RuntimeTableIdentity],
) -> Result<PinnedCommittedTables, Diagnostic> {
    let schemas = admitted_table_schemas(admitted);
    let (rows, storage_orders) =
        committed_rows_at(repository, commit, &schemas, tables).map_err(|error| match error {
            CommittedRowsError::Limit => Diagnostic::target(
                "E2200",
                "committed table rows exceed the read bound",
                "keep each table within the evaluator collection limit and the source byte limit",
            ),
            CommittedRowsError::Unavailable => committed_rows_read_error(),
        })?;
    pin_committed_tables(runtime, state, rows, storage_orders)
        .map_err(|_| committed_rows_read_error())
}

/// Pins committed rows read at a commit to the runtime's published generation.
pub(crate) fn pin_committed_tables(
    runtime: &tokio::runtime::Runtime,
    state: &RuntimeState,
    rows: RuntimeTableRows,
    storage_orders: BTreeMap<String, Vec<String>>,
) -> Result<PinnedCommittedTables, RuntimeError> {
    let pin = runtime.block_on(state.published_pin())?;
    Ok(PinnedCommittedTables {
        rows: PinnedCommittedRows::new(pin, rows)?,
        storage_orders,
    })
}

pub(crate) fn committed_rows_read_error() -> Diagnostic {
    Diagnostic::target(
        "E2200",
        "committed table rows could not be read",
        "retry the invocation after checking local runtime state",
    )
}

/// Why the committed rows of the admitted tables could not be returned.
pub(crate) enum CommittedRowsError {
    /// The committed base could not be read or does not match the admitted schemas.
    Unavailable,
    /// The committed base holds more rows or bytes than the committed read bound allows.
    Limit,
}

/// The schema of every admitted table, keyed by table name.
pub(crate) fn admitted_table_schemas(
    admitted: &AdmittedApplication,
) -> BTreeMap<String, orna_check::TableSchema> {
    admitted
        .table_schemas()
        .iter()
        .map(|(name, table)| (name.clone(), table.schema().clone()))
        .collect()
}

/// The stored tuples of every admitted table at `commit`, read from the
/// committed format-3 base, with the stored field order each table lists.
/// Shared by the one-shot invocation and `serve`. The read stops at the first
/// row past `orna_application::committed_read_limits`.
pub(crate) fn committed_rows_at(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    schemas: &BTreeMap<String, orna_check::TableSchema>,
    tables: &[RuntimeTableIdentity],
) -> Result<(RuntimeTableRows, BTreeMap<String, Vec<String>>), CommittedRowsError> {
    let format = repository
        .open_pinned_format_context(commit.as_str())
        .map_err(|_| CommittedRowsError::Unavailable)?;
    let table_ids: BTreeMap<String, TableObjectId> = tables
        .iter()
        .map(|table| (table.table().to_owned(), table.object_id()))
        .collect();
    let mut committed = orna_project::committed_tables(
        repository,
        &format,
        commit,
        schemas,
        &table_ids,
        orna_application::ApplicationAuthority::committed_read_limits(),
    )
    .map_err(|error| match error {
        orna_project::CommittedReadError::Limit { .. } => CommittedRowsError::Limit,
        orna_project::CommittedReadError::Storage(_) => CommittedRowsError::Unavailable,
    })?;
    let mut rows = BTreeMap::new();
    let mut storage_orders = BTreeMap::new();
    for name in schemas.keys() {
        let table = committed
            .remove(name)
            .ok_or(CommittedRowsError::Unavailable)?;
        rows.insert(name.clone(), table.rows);
        storage_orders.insert(name.clone(), table.storage_order);
    }
    Ok((rows, storage_orders))
}

/// One-shot `run` of a single admitted module that declares tables and reads
/// them. Tables bind to their committed identities at `HEAD`, so reads see the
/// same committed rows as the REPL. The sys host context and publication
/// snapshot follow the pure path: a module that needs publication metadata runs
/// as an embedded host, and any other module runs with the run host. `run` never
/// commits the activation's table mutations.
pub(crate) fn run_committed_table_invocation(
    endpoint: &Endpoint,
    authority: ApplicationAuthority,
    admitted: &AdmittedApplication,
    logical_path: &str,
    source: &str,
    std_snapshot: StdSnapshotPin,
) -> Result<(), Diagnostic> {
    let repository = discover_committed_repository(endpoint)?;
    let commit = committed_head(&repository)?;
    let tables = committed_tables(&repository, &commit, admitted)?;
    let admitted = admit_committed_metadata(&authority, admitted, logical_path, source, &tables)?;
    let (identity, initial_digest) = runtime_identity(&repository)?;
    let authority = invocation_host(authority, &repository, identity, &admitted, std_snapshot);
    let (runtime, state, publication_rows) =
        open_committed_runtime(&repository, identity, initial_digest, &admitted, &commit)?;
    let committed =
        committed_invocation_rows(&repository, &commit, &runtime, &state, &admitted, &tables)?;
    let (_snapshot, activation) = evaluate_over_committed_tables(
        &runtime,
        &state,
        &authority,
        &admitted,
        &tables,
        committed,
        publication_rows,
    )?;
    if !activation.mutations().is_empty() {
        return Err(Diagnostic::unavailable(
            "one-shot invocation cannot commit table mutations",
            "use a supported table transaction; stream roots require the explicit stream runtime",
        ));
    }
    Ok(())
}

/// The repository a committed-table invocation reads.
pub(crate) fn discover_committed_repository(
    endpoint: &Endpoint,
) -> Result<orna_repository::Repository, Diagnostic> {
    orna_repository::Repository::discover(local_project_path(endpoint)?).map_err(|_| {
        Diagnostic::target(
            "E2200",
            "committed table identities could not be read",
            "run the command inside an initialized local Orna repository",
        )
    })
}

/// The commit whose table identities bind the invocation.
pub(crate) fn committed_head(
    repository: &orna_repository::Repository,
) -> Result<orna_repository::GitCommitRef, Diagnostic> {
    repository.head().ok().flatten().ok_or_else(|| {
        Diagnostic::target(
            "E2200",
            "table reads need a committed project",
            "commit the project, then run the command again",
        )
    })
}

/// The committed identities of the admitted tables.
pub(crate) fn committed_tables(
    repository: &orna_repository::Repository,
    commit: &orna_repository::GitCommitRef,
    admitted: &AdmittedApplication,
) -> Result<Vec<RuntimeTableIdentity>, Diagnostic> {
    let names = admitted.table_schemas().keys().cloned().collect::<Vec<_>>();
    committed_table_identities(repository, commit, names)
}

/// Re-admits the module with the committed table metadata its reads bind.
pub(crate) fn admit_committed_metadata(
    authority: &ApplicationAuthority,
    admitted: &AdmittedApplication,
    logical_path: &str,
    source: &str,
    tables: &[RuntimeTableIdentity],
) -> Result<AdmittedApplication, Diagnostic> {
    let entry = admitted.entry().to_owned();
    let committed = tables
        .iter()
        .map(|table| CommittedTableIdentity::new(table.table(), table.object_id()));
    authority
        .admit_module_with_committed_table_metadata(logical_path, source, entry, committed)
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2101",
                "project semantic analysis failed",
                "fix the first reported source contract error, then run check again",
                error.to_string(),
            )
        })
}

/// The authority with the host context a committed-table read runs under.
pub(crate) fn invocation_host(
    authority: ApplicationAuthority,
    repository: &orna_repository::Repository,
    identity: RuntimeIdentity,
    admitted: &AdmittedApplication,
    std_snapshot: StdSnapshotPin,
) -> ApplicationAuthority {
    let mode = if admitted.requires_publication_metadata() {
        RuntimeMode::Embedded
    } else {
        RuntimeMode::Run
    };
    authority.with_sys_host_context(sys_host::sys_host_context(
        repository.worktree(),
        identity,
        mode,
        std_snapshot,
    ))
}

/// The runtime and state a committed-table read needs, with the publication
/// metadata rows it must see before any row is read.
pub(crate) fn open_committed_runtime(
    repository: &orna_repository::Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    admitted: &AdmittedApplication,
    commit: &orna_repository::GitCommitRef,
) -> Result<
    (
        tokio::runtime::Runtime,
        RuntimeState,
        Option<RuntimePublicationMetadataRows>,
    ),
    Diagnostic,
> {
    let unavailable = || {
        Diagnostic::target(
            "E2200",
            "committed table rows could not be read",
            "retry the invocation after checking local runtime state",
        )
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| unavailable())?;
    let state = runtime
        .block_on(RuntimeState::open(repository, identity, initial_digest))
        .map_err(|_| unavailable())?;
    let table_names = admitted
        .table_schemas()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let candidates = legacy_bare_module_candidates(repository, Some(commit), &table_names)
        .map_err(|_| unavailable())?;
    if let Some(table) =
        legacy_bare_module_table(&state, &runtime, &candidates).map_err(|_| unavailable())?
    {
        return Err(legacy_module_table_diagnostic(&table));
    }
    let publication_rows = if admitted.requires_publication_metadata() {
        Some(
            runtime
                .block_on(state.publication_metadata_rows())
                .map_err(|_| {
                    Diagnostic::target(
                        "E2200",
                        "local runtime publication metadata could not be read",
                        "retry the invocation after checking local runtime state",
                    )
                })?,
        )
    } else {
        None
    };
    Ok((runtime, state, publication_rows))
}
