use super::{
    Arc, BTreeMap, BlobField, Cow, EvaluatorLimits, GitCommitRef, HISTORICAL_RESOLVE_BUDGET,
    HISTORICAL_SCAN_RETRY, HistoricalState, HistoricalTableSchemas, Instant, MountRowBaselines,
    Mutex, ObjectIdentityManifest, OnceLock, OrpBlobBinding, ProjectLoader, Repository,
    RowLoadError, RowSaveContext, RuntimeState, RuntimeTableActivationSnapshot,
    RuntimeTableIdentity, SCAN_PENDING, SchemaDescriptor, SourceRowRequest, TableSource, TypedKey,
    VfsRepositoryCache, VfsRetainedDraftStore, VfsSaveCandidate, VfsSaveCoordinates,
    VfsScratchStore, VfsTableRowReplacement, admit_row_under_descriptor, admit_source_row,
    runtime_row_baseline_from_rows, settle_historical_scan, source_without_fields,
    substitute_text_fields, transactional_loaded_project_candidate_validator,
    validate_blob_descriptors,
};

pub(crate) struct MountWriter {
    pub(crate) runtime: tokio::runtime::Runtime,
    pub(crate) state: RuntimeState,
    pub(crate) repository: Repository,
    pub(crate) identities: ObjectIdentityManifest,
    pub(crate) project: Arc<orna_project::LoadedProject>,
    pub(crate) runtime_commit: GitCommitRef,
    pub(crate) mount_snapshot: OnceLock<RuntimeTableActivationSnapshot>,
    pub(crate) cache: Arc<VfsRepositoryCache>,
    pub(crate) scratch_store: VfsScratchStore,
    pub(crate) retained_drafts: VfsRetainedDraftStore,
    pub(crate) owner_id: [u8; 16],
    pub(crate) row_baselines: Option<MountRowBaselines>,
    /// Committed schema history of each table on `HEAD`'s lineage. Each table is scanned on a
    /// background thread the first time a CWD row the mounted schema cannot validate needs it, in
    /// passes bounded by `HISTORICAL_RESOLVE_BUDGET` that resume where the previous pass paused
    /// (ORNA-EVOL-002).
    pub(crate) historical_generations: Arc<Mutex<BTreeMap<[u8; 16], HistoricalState>>>,
}

/// Test seam: `ORNA_DRAFT_QUOTA_BYTES` replaces the aggregate draft quota of a
/// mounted editor so an end-to-end test can exhaust it with small candidates.
/// Compiled only with the `test-seams` feature; the default build ignores the
/// variable and always charges the production default. The value is parsed once
/// per process, so every editor opened by one mount shares one budget; a
/// malformed value is an error, never a silent fallback.
#[cfg(feature = "test-seams")]
pub(crate) fn mount_draft_quota() -> Result<Arc<orna_vfs::DraftQuota>, String> {
    const DRAFT_QUOTA_ENV: &str = "ORNA_DRAFT_QUOTA_BYTES";
    static MOUNT_DRAFT_QUOTA: std::sync::LazyLock<
        Result<Arc<orna_vfs::DraftQuota>, String>,
    > = std::sync::LazyLock::new(|| match std::env::var(DRAFT_QUOTA_ENV) {
        Err(std::env::VarError::NotPresent) => Ok(orna_vfs::default_draft_quota()),
        Err(error) => Err(format!("{DRAFT_QUOTA_ENV} is not valid: {error}")),
        Ok(value) => value
            .parse::<u64>()
            .map(|bytes| Arc::new(orna_vfs::DraftQuota::new(bytes)))
            .map_err(|error| format!("{DRAFT_QUOTA_ENV} is not a byte count: {error}")),
    });
    MOUNT_DRAFT_QUOTA.clone()
}

/// Default build: no environment seam exists; the production budget is used.
/// The `Result` matches the seam variant above, which can fail on a malformed
/// value, so both call sites stay identical.
#[cfg(not(feature = "test-seams"))]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn mount_draft_quota() -> Result<Arc<orna_vfs::DraftQuota>, String> {
    Ok(orna_vfs::default_draft_quota())
}

impl MountWriter {
    pub(crate) fn open(
        repository: &Repository,
        commit: &GitCommitRef,
        row_baselines: Option<MountRowBaselines>,
    ) -> Result<Arc<Self>, String> {
        let (identity, initial_digest) = super::runtime_identity(repository)
            .map_err(|error| format!("repository runtime identity is unavailable: {error}"))?;
        let owner_id = orna_runtime::process_owner_id();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| format!("mounted editor runtime could not be created: {error}"))?;
        let state = runtime
            .block_on(RuntimeState::open(repository, identity, initial_digest))
            .map_err(|error| format!("mounted editor runtime could not be opened: {error:?}"))?;
        let identities =
            ObjectIdentityManifest::read_committed(repository, commit).map_err(|error| {
                format!("committed source identities could not be loaded: {error:?}")
            })?;
        let runtime_commit = repository
            .head()
            .map_err(|error| {
                format!("mounted editor runtime source commit is unavailable: {error:?}")
            })?
            .unwrap_or_else(|| commit.clone());
        let project = ProjectLoader::default()
            .load_committed_snapshot(repository, commit)
            .map_err(|error| format!("committed source project could not be loaded: {error:?}"))?;
        let scratch_store = VfsScratchStore::open(repository.runtime_paths().root())?;
        let cache = Arc::new(
            VfsRepositoryCache::in_runtime_root_with_quota(
                repository.runtime_paths().root(),
                mount_draft_quota()?,
            )
            .map_err(|error| format!("draft spool unavailable: {error}"))?,
        );
        let retained_drafts =
            VfsRetainedDraftStore::open(repository.runtime_paths().root(), cache.draft_quota())?;
        Ok(Arc::new(Self {
            runtime,
            state,
            repository: repository.clone(),
            identities,
            project: Arc::new(project),
            runtime_commit,
            mount_snapshot: OnceLock::new(),
            cache,
            scratch_store,
            retained_drafts,
            owner_id,
            row_baselines,
            historical_generations: Arc::new(Mutex::new(BTreeMap::new())),
        }))
    }

    /// Admits a CWD row the mounted schema cannot validate (ORNA-VFS-001, ORNA-EVOL-002).
    /// Exactly one committed schema generation of the table must validate the row; the row is
    /// then rewritten to the mounted generation, with the fields schema evolution introduced
    /// filled from the defaults frozen by the commits that introduced them (ORNA-EVOL-009). A
    /// row that no generation, or more than one, validates is refused with the mounted-schema
    /// error. A row that needs the committed history while it is being scanned fails transiently.
    pub(crate) fn admit_runtime_row<'row>(
        &self,
        source: &TableSource,
        canonical_key: &[u8],
        row: &'row [u8],
    ) -> Result<Cow<'row, [u8]>, RowLoadError> {
        let relation_id = *source.binding.relation_id();
        let descriptor = source.schema.descriptor();
        let mounted_error = match descriptor.validate_stored_row(relation_id, canonical_key, row) {
            Ok(()) => return Ok(Cow::Borrowed(row)),
            Err(error) => error,
        };
        let historical = self.historical_descriptors(relation_id)?;
        let matching: Vec<&SchemaDescriptor> = historical
            .generations
            .iter()
            .filter(|generation| {
                generation
                    .validate_stored_row(relation_id, canonical_key, row)
                    .is_ok()
            })
            .collect();
        let [generation] = matching.as_slice() else {
            return Err(RowLoadError::Fatal(format!(
                "the CWD row failed validation against the mounted schema ({mounted_error:?}) \
                 and {} committed schema generation(s) validate it",
                matching.len()
            )));
        };
        admit_row_under_descriptor(
            relation_id,
            generation,
            descriptor,
            &historical.introductions,
            canonical_key,
            row,
        )
        .map(Cow::Owned)
        .map_err(|refusal| {
            RowLoadError::Fatal(format!(
                "the CWD row cannot be admitted to the mounted schema: {refusal}"
            ))
        })
    }

    /// The committed schema history of `relation_id`, cached per table for this mount. The first
    /// request starts a background scan pass and fails transiently. A pass that runs out of
    /// `HISTORICAL_RESOLVE_BUDGET` keeps its progress, and the next request resumes it. Requests
    /// fail transiently until the scan completes, after which they succeed. A failed scan is
    /// retried from the start once `HISTORICAL_SCAN_RETRY` has elapsed.
    pub(crate) fn historical_descriptors(
        &self,
        relation_id: [u8; 16],
    ) -> Result<Arc<HistoricalTableSchemas>, RowLoadError> {
        let mut states = self
            .historical_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match states.get(&relation_id) {
            Some(HistoricalState::Ready(schemas)) => return Ok(Arc::clone(schemas)),
            Some(HistoricalState::Resolving) => {
                return Err(RowLoadError::Transient(SCAN_PENDING.to_owned()));
            }
            Some(HistoricalState::Failed { reason, at })
                if at.elapsed() < HISTORICAL_SCAN_RETRY =>
            {
                return Err(RowLoadError::Transient(format!(
                    "the committed schema history could not be scanned: {reason}"
                )));
            }
            Some(HistoricalState::Paused(_) | HistoricalState::Failed { .. }) | None => {}
        }
        let previous = states.insert(relation_id, HistoricalState::Resolving);
        drop(states);
        let scan = match previous {
            Some(HistoricalState::Paused(scan)) => Some(scan),
            _ => None,
        };
        let repository = self.repository.clone();
        let shared = Arc::clone(&self.historical_generations);
        let deadline = Instant::now() + HISTORICAL_RESOLVE_BUDGET;
        let spawned = std::thread::Builder::new()
            .name("orna-historical-scan".to_owned())
            .spawn(move || {
                let state = settle_historical_scan(&repository, relation_id, scan, deadline);
                shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(relation_id, state);
            });
        match spawned {
            Ok(_) => Err(RowLoadError::Transient(SCAN_PENDING.to_owned())),
            Err(error) => {
                let reason = format!("the scan thread could not start: {error}");
                self.historical_generations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        relation_id,
                        HistoricalState::Failed {
                            reason: reason.clone(),
                            at: Instant::now(),
                        },
                    );
                Err(RowLoadError::Transient(format!(
                    "the committed schema history could not be scanned: {reason}"
                )))
            }
        }
    }

    /// Advances the committed schema history of `relation_id` on the calling thread until
    /// `deadline`. It runs before the mount serves requests, so persisted temporary scratch of the
    /// table can be restored (ORNA-VFS-019). A table whose history is resolving or settled keeps
    /// that state.
    pub(crate) fn resolve_historical_blocking(&self, relation_id: [u8; 16], deadline: Instant) {
        let mut states = self
            .historical_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            states.get(&relation_id),
            Some(HistoricalState::Ready(_) | HistoricalState::Resolving)
        ) {
            return;
        }
        let scan = match states.insert(relation_id, HistoricalState::Resolving) {
            Some(HistoricalState::Paused(scan)) => Some(scan),
            _ => None,
        };
        drop(states);
        let state = settle_historical_scan(&self.repository, relation_id, scan, deadline);
        self.historical_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(relation_id, state);
    }

    pub(crate) fn capture_mount_activation(
        &self,
        tables: &[RuntimeTableIdentity],
    ) -> Result<(), String> {
        let snapshot = self
            .runtime
            .block_on(self.state.begin_admitted_table_activation(tables))
            .map_err(|error| format!("mounted editor row admission failed: {error:?}"))?;
        self.mount_snapshot
            .set(snapshot)
            .map_err(|_| "mounted editor runtime snapshot was already captured".to_owned())
    }

    pub(crate) fn row_baseline<'a>(
        &'a self,
        table: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Result<Option<&'a [u8]>, String> {
        let snapshot = self
            .mount_snapshot
            .get()
            .ok_or_else(|| "mounted editor runtime snapshot was not captured".to_owned())?;
        runtime_row_baseline_from_rows(snapshot.table_rows(), table, key)
            .ok_or_else(|| format!("runtime table {} was not admitted", table.table()))
    }

    /// The CWD row the runtime holds for `key` now, read as one point lookup
    /// rather than from the snapshot captured at mount time. A stale save uses
    /// it to show and save against the row another writer accepted.
    pub(crate) fn live_row(
        &self,
        table: &RuntimeTableIdentity,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, String> {
        self.runtime
            .block_on(self.state.admitted_table_row(table, key))
            .map_err(|error| format!("mounted editor CWD row could not be read: {error:?}"))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn candidate(
        &self,
        row: &RowSaveContext,
        coordinates: &VfsSaveCoordinates,
        bytes: &[u8],
        replacement_binding: Option<(String, OrpBlobBinding)>,
        text_replacement: Option<(&str, &[u8])>,
    ) -> Result<(VfsSaveCandidate, Vec<u8>), orna_runtime::TableActivationError> {
        let source = std::str::from_utf8(bytes).map_err(|_| {
            orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            )
        })?;
        let (text_fields, blob_fields): (Vec<BlobField>, Vec<BlobField>) = row
            .blob_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .into_iter()
            .partition(|field| field.text);
        let substituted =
            substitute_text_fields(source, &text_fields, text_replacement).map_err(|error| {
                eprintln!("mounted row text field was refused: {error}");
                orna_runtime::TableActivationError::Runtime(
                    orna_runtime::RuntimeError::InvalidTableMutation,
                )
            })?;
        let source = substituted.as_str();
        let absent_blob_fields =
            validate_blob_descriptors(source, &blob_fields).map_err(|error| {
                eprintln!("mounted row document was refused: {error}");
                orna_runtime::TableActivationError::Runtime(
                    orna_runtime::RuntimeError::InvalidTableMutation,
                )
            })?;
        // An optional Blob written as `null` is absent: its binding is retired and
        // `null` stays in the source, as the unlink path commits it.
        let blob_fields: Vec<BlobField> = blob_fields
            .into_iter()
            .filter(|field| !absent_blob_fields.contains(&field.name))
            .collect();
        let omitted_blob_fields = blob_fields
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>();
        let source = source_without_fields(source, &omitted_blob_fields).map_err(|error| {
            eprintln!("mounted row source projection failed: {error}");
            orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            )
        })?;
        let mut replacement_binding = replacement_binding;
        let mut blob_bindings = Vec::with_capacity(blob_fields.len());
        if !blob_fields.is_empty() {
            let scope = row.source.graph.open_read_scope().map_err(|_| {
                orna_runtime::TableActivationError::Runtime(
                    orna_runtime::RuntimeError::InvalidTableMutation,
                )
            })?;
            for field in &blob_fields {
                let binding = if replacement_binding
                    .as_ref()
                    .is_some_and(|(name, _)| name == &field.name)
                {
                    replacement_binding
                        .take()
                        .map(|(_, binding)| binding)
                        .ok_or(orna_runtime::TableActivationError::Runtime(
                            orna_runtime::RuntimeError::InvalidTableMutation,
                        ))?
                } else if let Some(bytes) = &field.replacement_bytes {
                    let capture = self
                        .repository
                        .capture_capability(*row.source.binding.relation_id())
                        .map_err(|_| {
                            orna_runtime::TableActivationError::Runtime(
                                orna_runtime::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    let candidate = capture
                        .capture_blob_candidate(bytes.reader(), bytes.len())
                        .map_err(|_| {
                            orna_runtime::TableActivationError::Runtime(
                                orna_runtime::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    capture
                        .accept_captured_blob(candidate, &field.media_type, field.suffix.as_deref())
                        .map_err(|_| {
                            orna_runtime::TableActivationError::Runtime(
                                orna_runtime::RuntimeError::InvalidTableMutation,
                            )
                        })?
                } else {
                    let reference = field.reference.as_ref().ok_or(
                        orna_runtime::TableActivationError::Runtime(
                            orna_runtime::RuntimeError::InvalidTableMutation,
                        ),
                    )?;
                    let pin = row
                        .source
                        .graph
                        .protect_admitted_blob_reference(reference, &scope)
                        .map_err(|_| {
                            orna_runtime::TableActivationError::Runtime(
                                orna_runtime::RuntimeError::InvalidTableMutation,
                            )
                        })?;
                    row.source
                        .graph
                        .accept_protected_blob_pin_with_annotation(
                            pin,
                            &field.media_type,
                            field.suffix.as_deref(),
                        )
                        .map_err(|_| {
                            orna_runtime::TableActivationError::Runtime(
                                orna_runtime::RuntimeError::InvalidTableMutation,
                            )
                        })?
                };
                blob_bindings.push((field.name.clone(), binding));
            }
        }
        if replacement_binding.is_some() {
            return Err(orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            ));
        }
        let mut mutation_id = [0_u8; 16];
        getrandom::fill(&mut mutation_id).map_err(|_| {
            orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            )
        })?;
        let admitted = admit_source_row(
            &self.project,
            &row.source.schema,
            &row.source.value_context,
            SourceRowRequest {
                logical_path: &row.logical_path,
                source: &source,
                blob_bindings,
                owner_field_values: Vec::new(),
                key_components: &row.key_components,
                mutation_id,
            },
        )
        .map_err(|error| {
            eprintln!("mounted row source admission failed: {error:?}");
            orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            )
        })?;
        let (source_mutation, bindings, resolved_schema) = admitted.into_mutation_and_bindings();
        // Text primary keys use their UTF-8 bytes in admitted mutations;
        // other key kinds keep the canonical typed-key encoding.
        let mutation_key_matches = match &row.key {
            TypedKey::Text(key) => source_mutation.key() == key.as_bytes(),
            _ => source_mutation.key() == coordinates.key(),
        };
        if resolved_schema.table_object_id() != row.source.schema.table_object_id()
            || !mutation_key_matches
            || source_mutation.table() != coordinates.table().table()
            || source_mutation.table_object_id() != Some(coordinates.table().object_id())
        {
            return Err(orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            ));
        }
        // Source-row admission creates insert-only mutations; this durable
        // save already admitted the existing baseline, so write a replacement.
        let replacement_value =
            source_mutation
                .value()
                .ok_or(orna_runtime::TableActivationError::Runtime(
                    orna_runtime::RuntimeError::InvalidTableMutation,
                ))?;
        let mut mutation = orna_runtime::TableMutation::new(
            source_mutation.id(),
            source_mutation.table().to_owned(),
            coordinates.key().to_vec(),
            Some(replacement_value.to_vec()),
        )
        .map_err(orna_runtime::TableActivationError::Runtime)?
        .with_table_object_id(coordinates.table().object_id());
        let mut pins = Vec::with_capacity(bindings.len());
        for binding in bindings {
            let (_, _, pin) = binding.into_parts();
            mutation = mutation
                .with_protected_content_pin(&pin)
                .map_err(orna_runtime::TableActivationError::Runtime)?;
            pins.push(pin);
        }
        let content_transfers = pins
            .iter()
            .map(orna_repository::ProtectedContentPin::transfer_record)
            .collect::<Vec<_>>();
        let replacement_bytes = mutation
            .value()
            .ok_or(orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            ))?
            .to_vec();
        let replacement = VfsTableRowReplacement::from_admitted_mutation(
            coordinates.table().clone(),
            mutation,
            pins,
        )
        .map_err(orna_runtime::TableActivationError::Runtime)?;
        let validator = transactional_loaded_project_candidate_validator(
            &self.project,
            &self.identities,
            None,
            Arc::clone(&row.table_object_ids),
            EvaluatorLimits::default(),
        )
        .map_err(|_| {
            orna_runtime::TableActivationError::Runtime(
                orna_runtime::RuntimeError::InvalidTableMutation,
            )
        })?;
        let live_repository = self.repository.clone();
        let live_table = row.table.object_id();
        let mut live_identity =
            orna_cli_support::table_schema::LiveTableSchemaIdentity::new(live_table);
        let candidate = VfsSaveCandidate::new(replacement, validator)
            .with_protected_content_transfers(replacement_bytes.clone(), content_transfers)
            .with_live_schema_identity(move || live_identity.read(&live_repository));
        Ok((candidate, replacement_bytes))
    }
}
