use super::{
    Arc, BTreeMap, GitCommitRef, HISTORY_LIMIT, HashSet, Instant, ObjectIdentityManifest,
    ObjectKind, ProjectLoader, Raw, Repository, SchemaDescriptor, StoredSchema, TableObjectId, hex,
};

/// The committed schema history of one table.
pub(crate) struct HistoricalTableSchemas {
    /// The distinct schema generations on `HEAD`'s lineage, newest first.
    pub(crate) generations: Vec<SchemaDescriptor>,
    /// The stored constant default of each field that schema evolution introduced, as frozen by
    /// the commit that introduced it (ORNA-FIELD-003, ORNA-EVOL-009).
    pub(crate) introductions: BTreeMap<[u8; 16], Raw>,
}

pub(crate) enum HistoricalState {
    /// A scan thread is running.
    Resolving,
    Ready(Arc<HistoricalTableSchemas>),
    /// A scan pass ran out of its budget. The next pass resumes the scan at its next commit.
    Paused(Box<HistoricalScan>),
    /// The last scan failed; it is retried once `HISTORICAL_SCAN_RETRY` has elapsed.
    Failed {
        reason: String,
        at: Instant,
    },
}

/// The committed-schema scan of one table: the history walked when it started, and the progress of
/// its passes over that history.
pub(crate) type HistoricalScan = ScanProgress<GitCommitRef, SchemaDescriptor>;

/// What one commit of a table's history contributes to the scan.
pub(crate) struct HistoricalDeclaration<G> {
    /// The schema generation the commit declares for the table.
    pub(crate) generation: G,
    /// The IDs of the table's stored fields at that commit.
    pub(crate) field_ids: HashSet<[u8; 16]>,
    /// The stored constant default of each field, as frozen by that commit.
    pub(crate) defaults: BTreeMap<[u8; 16], Raw>,
}

/// How one commit of a table's history relates to the table.
pub(crate) enum CommitDeclaration<G> {
    /// The commit declares the table.
    Declares(HistoricalDeclaration<G>),
    /// The commit has committed identities but does not declare the table.
    Undeclared,
    /// The commit has no committed identities, so it cannot declare the table.
    Unidentified,
}

/// A scan of one table's committed history, oldest commit first, which stops when its budget runs
/// out and resumes at the next commit.
pub(crate) struct ScanProgress<C, G> {
    pub(crate) commits: Vec<C>,
    /// The index of the next commit to visit.
    pub(crate) next: usize,
    /// The field IDs at the last commit that declared the table.
    pub(crate) previous_ids: HashSet<[u8; 16]>,
    pub(crate) introductions: BTreeMap<[u8; 16], Raw>,
    /// The generation declared by each declaring commit, oldest first.
    pub(crate) declared: Vec<G>,
    /// Commits without committed identities. They declare nothing and are counted, not skipped
    /// silently (ORNA-EVOL-002, F7).
    pub(crate) unidentified: usize,
}

impl<C, G: PartialEq> ScanProgress<C, G> {
    pub(crate) fn new(commits: Vec<C>) -> Self {
        Self {
            commits,
            next: 0,
            previous_ids: HashSet::new(),
            introductions: BTreeMap::new(),
            declared: Vec::new(),
            unidentified: 0,
        }
    }

    /// Visits the commits from `next` on. Returns `true` once the history is exhausted, or `false`
    /// when `within_budget` refuses the next commit. `declare` classifies one commit; commits that
    /// do not declare the table, and commits without committed identities, are skipped.
    pub(crate) fn advance(
        &mut self,
        mut within_budget: impl FnMut() -> bool,
        declare: &impl Fn(&C) -> Result<CommitDeclaration<G>, String>,
    ) -> Result<bool, String> {
        while self.next < self.commits.len() {
            if !within_budget() {
                return Ok(false);
            }
            let declaration = match declare(&self.commits[self.next])? {
                CommitDeclaration::Declares(declaration) => declaration,
                CommitDeclaration::Undeclared => {
                    self.next += 1;
                    continue;
                }
                CommitDeclaration::Unidentified => {
                    self.unidentified += 1;
                    self.next += 1;
                    continue;
                }
            };
            let previous_ids = &self.previous_ids;
            for id in declaration
                .field_ids
                .iter()
                .filter(|id| !previous_ids.contains(*id))
            {
                match declaration.defaults.get(id) {
                    Some(value) => self.introductions.insert(*id, value.clone()),
                    None => self.introductions.remove(id),
                };
            }
            self.previous_ids = declaration.field_ids;
            self.declared.push(declaration.generation);
            self.next += 1;
        }
        Ok(true)
    }

    /// The declared generations, newest first and each distinct, with the introduced values. Only
    /// meaningful once `advance` has returned `true`.
    pub(crate) fn finish(self) -> (Vec<G>, BTreeMap<[u8; 16], Raw>, usize) {
        let mut generations: Vec<G> = Vec::new();
        for generation in self.declared.into_iter().rev() {
            if !generations.contains(&generation) {
                generations.push(generation);
            }
        }
        (generations, self.introductions, self.unidentified)
    }
}

/// The outcome of one pass of a table's committed-schema scan.
pub(crate) enum HistoricalStep {
    /// The pass visited the whole history.
    Settled(HistoricalTableSchemas),
    /// The pass ran out of budget; the scan resumes from here.
    Paused(Box<HistoricalScan>),
}

/// Runs one pass of a table's committed-schema scan on the calling thread, until `deadline`, and
/// returns the table's resulting state. A panicking pass is a failed scan, so a table is never left
/// resolving.
pub(crate) fn settle_historical_scan(
    repository: &Repository,
    relation_id: [u8; 16],
    scan: Option<Box<HistoricalScan>>,
    deadline: Instant,
) -> HistoricalState {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        advance_historical_scan(repository, relation_id, scan, deadline)
    }))
    .unwrap_or_else(|_| Err("the committed schema scan panicked".to_owned()));
    match outcome {
        Ok(HistoricalStep::Settled(schemas)) => HistoricalState::Ready(Arc::new(schemas)),
        Ok(HistoricalStep::Paused(scan)) => HistoricalState::Paused(scan),
        Err(reason) => HistoricalState::Failed {
            reason,
            at: Instant::now(),
        },
    }
}

/// Advances the committed-schema scan of `relation_id` along `repository`'s `HEAD` lineage until
/// `deadline`. The first pass walks the history and starts the scan; a later pass resumes the
/// history that the first pass walked, so a `HEAD` that moves afterwards is not rescanned (F8).
pub(crate) fn advance_historical_scan(
    repository: &Repository,
    relation_id: [u8; 16],
    scan: Option<Box<HistoricalScan>>,
    deadline: Instant,
) -> Result<HistoricalStep, String> {
    let mut scan = if let Some(scan) = scan {
        *scan
    } else {
        let history = repository
            .committed_history(HISTORY_LIMIT + 1)
            .map_err(|error| format!("committed history could not be read: {error:?}"))?;
        if history.len() > HISTORY_LIMIT {
            return Err(format!(
                "the table's committed history is longer than the {HISTORY_LIMIT}-commit scan bound"
            ));
        }
        ScanProgress::new(
            history
                .iter()
                .rev()
                .map(|entry| entry.commit().clone())
                .collect(),
        )
    };
    let complete = scan.advance(|| Instant::now() < deadline, &|commit: &GitCommitRef| {
        declare_historical_commit(repository, relation_id, commit)
    })?;
    if !complete {
        return Ok(HistoricalStep::Paused(Box::new(scan)));
    }
    let (generations, introductions, unidentified) = scan.finish();
    if unidentified > 0 {
        eprintln!(
            "committed schema scan of table {}: skipped {unidentified} commit(s) without committed identities",
            hex(&relation_id)
        );
    }
    Ok(HistoricalStep::Settled(HistoricalTableSchemas {
        generations,
        introductions,
    }))
}

/// Reads one commit of a table's history. A commit with no committed identity for the table
/// predates it and does not declare it (F7: identity-less commits are skipped and counted). Any
/// other commit that cannot be read or resolved fails the scan: a generation is never silently
/// left out (ORNA-EVOL-002).
pub(crate) fn declare_historical_commit(
    repository: &Repository,
    relation_id: [u8; 16],
    commit: &GitCommitRef,
) -> Result<CommitDeclaration<SchemaDescriptor>, String> {
    let identities = ObjectIdentityManifest::read_committed_if_present(repository, commit)
        .map_err(|error| {
            format!(
                "committed identities at {} could not be read: {error:?}",
                commit.as_str()
            )
        })?;
    let Some(identities) = identities else {
        return Ok(CommitDeclaration::Unidentified);
    };
    let declared = identities.records().iter().any(|record| {
        record.kind() == ObjectKind::Table && !record.is_retired() && *record.id() == relation_id
    });
    if !declared {
        return Ok(CommitDeclaration::Undeclared);
    }
    let (schema, defaults) = ProjectLoader::default()
        .resolve_committed_source_table_schema_with_introductions(
            repository,
            commit,
            TableObjectId::new(relation_id),
        )
        .map_err(|error| {
            format!(
                "the table's committed schema at {} could not be resolved: {error:?}",
                commit.as_str()
            )
        })?;
    let stored = StoredSchema::new(schema.descriptor()).map_err(|error| {
        format!(
            "the table's committed schema at {} is invalid: {error:?}",
            commit.as_str()
        )
    })?;
    let field_ids = stored
        .fields()
        .iter()
        .filter(|field| field.role() == 1)
        .map(|field| *field.id())
        .collect();
    Ok(CommitDeclaration::Declares(HistoricalDeclaration {
        generation: schema.descriptor().clone(),
        field_ids,
        defaults,
    }))
}
