use super::*;

/// The row and the accepted candidate bytes of one settled replacement. The bytes are the
/// same shared image the vfs published, so a caller never re-reads the scratch to get them.
pub(crate) struct AcceptedReplacement {
    pub(crate) row: Option<Vec<u8>>,
    pub(crate) bytes: Arc<CandidateSnapshot>,
}

/// Outcome of one Blob commit attempt: settled, or refused because the scratch
/// candidate changed between hashing and admission (no row was touched).
pub(crate) enum BlobAttempt {
    Settled(AcceptedReplacement),
    RacingWrite,
}

#[derive(Clone)]
pub(crate) struct BlobField {
    pub(crate) name: String,
    pub(crate) file_name: String,
    pub(crate) media_type: String,
    pub(crate) suffix: Option<String>,
    pub(crate) reference: Option<AdmittedBlobReference>,
    pub(crate) replacement_bytes: Option<Arc<CandidateSnapshot>>,
    /// Whether the row schema requires this field: a required Blob is never unlinked (VFS-013).
    pub(crate) required: bool,
    /// A large Str projected as a sibling text file (profiles/vfs-1.md), not a Blob. Its
    /// `replacement_bytes` always hold the Str's current content.
    pub(crate) text: bool,
}
pub(crate) struct RowSaveContext {
    pub(crate) writer: Arc<MountWriter>,
    pub(crate) source: Arc<TableSource>,
    pub(crate) key: TypedKey,
    pub(crate) target: Arc<ManagedFile<RowMapSnapshot>>,
    pub(crate) table: RuntimeTableIdentity,
    pub(crate) table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    pub(crate) logical_path: String,
    pub(crate) key_components: Vec<KeyComponentText>,
    pub(crate) expected_baseline_row: Mutex<Option<Vec<u8>>>,
    pub(crate) blob_fields: Mutex<Vec<BlobField>>,
}

/// Whether two save contexts address the same stored row, whichever projection
/// of it each one was built from.
pub(crate) fn same_row(left: &RowSaveContext, right: &RowSaveContext) -> bool {
    left.source.binding.relation_id() == right.source.binding.relation_id() && left.key == right.key
}

// Source-row mutations store text primary keys as UTF-8; other keys use their
// canonical typed-key encoding.
pub(crate) fn runtime_table_key_bytes(key: &TypedKey) -> Result<Vec<u8>, String> {
    match key {
        TypedKey::Text(key) => Ok(key.as_bytes().to_vec()),
        _ => key
            .canonical_bytes()
            .map_err(|error| format!("committed row key cannot be encoded: {error:?}")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredFieldShape {
    pub(crate) field_id: [u8; 16],
    pub(crate) field_type: Raw,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredFieldLayout {
    pub(crate) fields: Vec<StoredFieldShape>,
    pub(crate) indexes: HashMap<[u8; 16], usize>,
}

impl StoredFieldLayout {
    pub(crate) fn field_type(&self, field_id: &[u8; 16]) -> Option<&Raw> {
        self.indexes
            .get(field_id)
            .and_then(|index| self.fields.get(*index))
            .map(|field| &field.field_type)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RowProjectionSource {
    RuntimeSnapshot,
    PinnedSnapshot,
}

pub(crate) fn runtime_row_for_exact_key<'a>(
    rows: &'a [(Vec<u8>, Vec<u8>)],
    key: &[u8],
) -> Option<&'a [u8]> {
    rows.binary_search_by(|(candidate, _)| candidate.as_slice().cmp(key))
        .ok()
        .map(|index| rows[index].1.as_slice())
}

#[allow(clippy::option_option)]
pub(crate) fn runtime_row_baseline_from_rows<'a>(
    rows: &'a RuntimeTableRows,
    table: &RuntimeTableIdentity,
    key: &[u8],
) -> Option<Option<&'a [u8]>> {
    Some(runtime_row_for_exact_key(rows.get(table.table())?, key))
}

#[allow(clippy::option_option)]
pub(crate) fn runtime_row_matches_layout(
    runtime_fields: &Raw,
    pinned_layout: &[StoredFieldShape],
    runtime_layout: &[StoredFieldShape],
    row_field_ids: impl Iterator<Item = [u8; 16]>,
) -> bool {
    let Raw::Array(values) = runtime_fields else {
        return false;
    };
    runtime_layout == pinned_layout
        && values.len() == pinned_layout.len()
        && pinned_layout
            .iter()
            .map(|field| field.field_id)
            .eq(row_field_ids)
}

/// Chooses the values `data.orna` projects for one row. A row the CWD holds is
/// what the workspace mount shows (ORNA-VFS-005, W08-08): when that row cannot
/// be decoded or follows another stored layout, the projection fails (EIO)
/// instead of silently showing the pinned HEAD row in its place.
pub(crate) fn select_runtime_row_fields(
    runtime_row: Option<&[u8]>,
    pinned_fields: Raw,
    pinned_layout: &[StoredFieldShape],
    runtime_layout: Option<&[StoredFieldShape]>,
    row_field_ids: impl Iterator<Item = [u8; 16]>,
) -> Result<(Raw, RowProjectionSource), String> {
    let Some(runtime_row) = runtime_row else {
        return Ok((pinned_fields, RowProjectionSource::PinnedSnapshot));
    };
    // The CWD row is validated against the mounted snapshot's schema (ORNA-VFS-001).
    // A HEAD that no longer declares the table must not make the row unreadable.
    let runtime_layout = runtime_layout.unwrap_or(pinned_layout);
    let runtime_fields = decode_canonical_storage_value(runtime_row)
        .map_err(|error| format!("the CWD row cannot be decoded: {error:?}"))?;
    if !runtime_row_matches_layout(
        &runtime_fields,
        pinned_layout,
        runtime_layout,
        row_field_ids,
    ) {
        return Err(
            "the CWD row follows another stored field layout than the mounted schema".into(),
        );
    }
    Ok((runtime_fields, RowProjectionSource::RuntimeSnapshot))
}

/// Captures a fresh owner-snapshot allocation for one projected file image.
///
/// The runtime stamps one projection epoch per snapshot allocation, and every
/// accepted save advances the epoch. An image built over the table's shared
/// row-map `Arc` after a save would find an earlier stamp and be refused
/// (`ImageAlreadyScoped`), so rows first opened after a save became unreadable
/// (W15-02). The clone shares the same immutable row entries, so every image
/// stays pinned to the mounted snapshot.
pub(crate) fn row_image_pin(rows: &Arc<RowMapSnapshot>) -> SnapshotPin<RowMapSnapshot> {
    SnapshotPin::capture(Arc::new(RowMapSnapshot::clone(rows)))
}

/// Builds the save context of one projected row document. `refreshed_baseline`
/// is the durable row a refresh after a stale save projects; it replaces the
/// view's recorded baseline because the document now shows that row.
pub(crate) fn row_save_context(
    writer: &Arc<MountWriter>,
    source: &Arc<TableSource>,
    key: &TypedKey,
    table_object_ids: Arc<BTreeMap<String, TableObjectId>>,
    document_bytes: &[u8],
    blob_fields: Vec<BlobField>,
    refreshed_baseline: Option<Vec<u8>>,
) -> Result<Arc<RowSaveContext>, String> {
    let declaration = source
        .binding
        .declaration_name()
        .ok_or_else(|| "committed table has no source declaration name".to_owned())?;
    // Row mutations address the table by its qualified identity, the same
    // key the admitted validator and the identity manifest use.
    let qualified_name = match source.binding.module_name().unwrap_or_default() {
        "" => declaration.to_owned(),
        module => format!("{module}.{declaration}"),
    };
    let table = RuntimeTableIdentity::new(
        &qualified_name,
        TableObjectId::new(*source.binding.relation_id()),
    )
    .map_err(|error| format!("committed table identity is invalid: {error:?}"))?;
    let key_bytes = runtime_table_key_bytes(key)?;
    let live_baseline_row = source
        .runtime_table
        .as_ref()
        .map(|runtime_table| writer.row_baseline(runtime_table, &key_bytes))
        .transpose()?
        .flatten()
        .map(<[u8]>::to_vec);
    // A row this view already accepted keeps that baseline across the processes
    // that save through it; the live durable row is only the first baseline. A
    // temporary is the exception: its baseline is re-read when it is created.
    let expected_baseline_row = match (
        refreshed_baseline,
        &writer.row_baselines,
        &source.runtime_table,
    ) {
        (Some(row), _, _) => Some(row),
        (None, Some(baselines), Some(table)) => baselines
            .accepted_row(table.table(), &key_bytes)
            .map_err(|error| format!("mounted row baseline is unavailable: {error}"))?
            .or(live_baseline_row),
        _ => live_baseline_row,
    };
    let mut components = Vec::new();
    flatten_key_components(key, &mut components)?;
    let key_components = components
        .iter()
        .map(key_component_text)
        .collect::<Result<Vec<_>, _>>()?;
    let Some((leaf, prefixes)) = components.split_last() else {
        return Err("committed row key has no path components".into());
    };
    let mut logical_path = declaration.to_owned();
    for prefix in prefixes {
        logical_path.push('/');
        logical_path.push_str(&key_component(prefix)?);
    }
    logical_path.push('/');
    logical_path.push_str(&key_component(leaf)?);
    logical_path.push_str(".orna");
    let image = Arc::new(VfsFileSnapshot::new(
        row_image_pin(&source.rows),
        Arc::<[u8]>::from(document_bytes.to_vec()),
    ));
    let target = writer
        .runtime
        .block_on(writer.cache.managed_file(image))
        .map_err(|error| format!("mounted editor row image could not be admitted: {error:?}"))?;
    Ok(Arc::new(RowSaveContext {
        writer: Arc::clone(writer),
        source: Arc::clone(source),
        key: key.clone(),
        target,
        table,
        logical_path,
        key_components,
        table_object_ids,
        expected_baseline_row: Mutex::new(expected_baseline_row),
        blob_fields: Mutex::new(blob_fields),
    }))
}
/// Why a document save did not commit. `NoCandidate` means the handle never
/// submitted bytes, so there is nothing to accept or discard. `Stale` means the
/// mounted baseline no longer matches the durable row and nothing was written.
/// `Rejected` means the submitted draft is malformed or fails semantic
/// admission (profiles/vfs-1 EINVAL). `Stale`, `Rejected` and `Failed` all mean
/// a save was attempted and did not commit, so the draft stays unresolved.
#[derive(Debug)]
pub(crate) enum DocumentCommitError {
    NoCandidate,
    Stale,
    Rejected(String),
    Failed(String),
    /// The aggregate draft quota refused the candidate (41-vfs.md:105, EDQUOT).
    Quota(String),
    /// The save did not commit and recovery storage could not keep its candidate;
    /// `errno` is the retention errno (ORNA-VFS-009, EDIT-1 step 4).
    Unretained {
        errno: Errno,
        cause: &'static str,
    },
}

impl DocumentCommitError {
    /// Errno for a save that did not commit (profiles/vfs-1 errno table):
    /// EINVAL for malformed drafts, EAGAIN for stale CAS, EDQUOT for resource limits,
    /// EIO for backing I/O.
    pub(crate) fn errno(&self) -> Errno {
        match self {
            Self::Rejected(_) => Errno::EINVAL,
            Self::Unretained { errno, .. } => *errno,
            Self::Stale => Errno::EAGAIN,
            Self::Quota(_) => Errno::EDQUOT,
            Self::NoCandidate | Self::Failed(_) => Errno::EIO,
        }
    }

    /// The cause recorded with a candidate that did not commit: the profile's
    /// `sys.vfs.*` code where one exists (ORNA-VFS-009).
    pub(crate) fn cause(&self) -> &'static str {
        match self {
            Self::Stale => "sys.vfs.stale_edit",
            Self::Rejected(_) => "sys.vfs.invalid_document",
            Self::Quota(_) => "draft quota exhausted",
            Self::Unretained { cause, .. } => cause,
            Self::NoCandidate | Self::Failed(_) => "backing I/O failure",
        }
    }
}

/// Cause recorded for a dirty document released without an accepted save: the
/// refusal already recorded for it (ORNA-VFS-009), else `unsynced`.
pub(crate) fn release_retention_cause(refusal: Option<&DraftRefusal>) -> &'static str {
    refusal.map_or("unsynced", |refusal| refusal.cause)
}

/// The refusal text for a validation diagnostic: a false table assertion shows the
/// spec code `ORNA-A091-009` (33-diagnostics.md), exactly as the REPL does; any other
/// class keeps its internal name.
pub(crate) fn refusal_text(
    table_assertion_false: bool,
    diagnostic: impl std::fmt::Debug,
) -> String {
    if table_assertion_false {
        "ORNA-A091-009".to_owned()
    } else {
        format!("{diagnostic:?}")
    }
}

/// Classifies a failed durable save. Semantic admission and malformed-draft
/// refusals are `Rejected`; CAS staleness is `Stale`; everything else is
/// backing I/O `Failed`; a draft-quota refusal is `Quota` (EDQUOT).
pub(crate) fn save_failure(
    context: &str,
    error: VfsSaveError<orna_runtime::TableActivationError>,
) -> DocumentCommitError {
    match error {
        VfsSaveError::Activation(orna_runtime::TableActivationError::ValidationFailed(
            diagnostic,
        ))
        | VfsSaveError::Rejected { diagnostic, .. } => DocumentCommitError::Rejected(format!(
            "{context}: {}",
            refusal_text(
                orna_conformance::is_table_assertion_false(diagnostic),
                diagnostic
            )
        )),
        VfsSaveError::Activation(orna_runtime::TableActivationError::Runtime(
            orna_runtime::RuntimeError::InvalidTableMutation,
        )) => DocumentCommitError::Rejected(format!("{context}: malformed draft")),
        VfsSaveError::Stale { .. } => DocumentCommitError::Stale,
        VfsSaveError::NoCandidate => DocumentCommitError::NoCandidate,
        VfsSaveError::Backing(
            error @ (EditDraftError::QuotaExceeded | EditDraftError::FileTooLarge),
        ) => DocumentCommitError::Quota(format!("{context}: {error:?}")),
        error => DocumentCommitError::Failed(format!("{context}: {error:?}")),
    }
}

/// Classifies a temporary commit that the runtime refused to finish. A backing I/O
/// failure while reading the candidate is a failed commit (EIO), never a malformed
/// draft (EINVAL): the candidate was not shown to be invalid.
pub(crate) fn temporary_commit_failure(
    backing_io_failed: bool,
    error: VfsSaveError<orna_runtime::TableActivationError>,
) -> DocumentCommitError {
    if backing_io_failed {
        DocumentCommitError::Failed("temporary mounted row save failed: backing I/O failure".into())
    } else {
        save_failure("temporary mounted row save failed", error)
    }
}

impl From<String> for DocumentCommitError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl std::fmt::Display for DocumentCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCandidate => formatter.write_str("mounted row save failed: NoCandidate"),
            Self::Stale => {
                formatter.write_str("mounted save is stale against its captured baseline")
            }
            Self::Unretained { cause, .. } => formatter.write_str(cause),
            Self::Rejected(message) | Self::Failed(message) | Self::Quota(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl RowSaveContext {
    /// The committed schema identity of the row's table right now. Operations capture it when
    /// they open their handle or scratch and pass that captured value to the commit (ORNA-VFS-019).
    pub(crate) fn live_schema_identity(&self) -> Result<TableSchemaIdentity, String> {
        crate::table_schema::committed_table_schema_identity(
            &self.writer.repository,
            self.table.object_id(),
        )
    }

    pub(crate) fn durable_save_with_baseline(
        &self,
        expected_baseline_row: Option<Vec<u8>>,
        schema_identity: TableSchemaIdentity,
    ) -> Result<VfsDurableSave, String> {
        let key = runtime_table_key_bytes(&self.key)?;
        Ok(VfsDurableSave::new(
            self.writer.owner_id,
            schema_identity,
            self.table.clone(),
            key,
        )
        .with_expected_baseline_row(expected_baseline_row))
    }

    pub(crate) fn record_accepted_row(&self, row: Option<Vec<u8>>) {
        let Some(row) = row else {
            return;
        };
        if let Some(baselines) = &self.writer.row_baselines {
            let recorded = runtime_table_key_bytes(&self.key).and_then(|key| {
                baselines
                    .record_accepted_row(self.table.table(), &key, &row)
                    .map_err(|error| error.to_string())
            });
            if let Err(error) = recorded {
                eprintln!("mounted row baseline could not be recorded: {error}");
            }
        }
        *self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row);
    }

    /// The baseline a new temporary captures (41-vfs.md Temporary-save
    /// replacement, ORNA-VFS-011): the row's current durable image, so a temp
    /// created after another writer's commit renames over that row. A row
    /// another writer deleted keeps the recorded baseline, which refuses it.
    pub(crate) fn temporary_baseline(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let recorded = self
            .expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(table) = self.source.runtime_table.as_ref() else {
            return Ok(recorded);
        };
        Ok(self.writer.live_row(table, key)?.or(recorded))
    }

    /// The row this view last loaded or accepted: the CAS baseline for a commit that does not
    /// re-read the live row.
    pub(crate) fn recorded_baseline(&self) -> Option<Vec<u8>> {
        self.expected_baseline_row
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The CAS baseline an explicit `orna edit` saves against (41-vfs.md:75-77). A grouped
    /// `edit --row` is this mount's own row save and compares against the row the mount recorded
    /// (stale when another writer advanced it). A plain `edit PATH` reads the durable row at commit:
    /// a plain owner-routed edit leaves the recorded baseline at the row before it, so a later
    /// plain edit would be refused against its own predecessor.
    pub(crate) fn explicit_edit_baseline(
        &self,
        grouped_row: bool,
    ) -> Result<Option<Vec<u8>>, String> {
        if grouped_row {
            return Ok(self.recorded_baseline());
        }
        let key = runtime_table_key_bytes(&self.key)?;
        self.temporary_baseline(&key)
    }

    pub(crate) fn temporary_target(
        &self,
        blob_field: Option<&BlobField>,
    ) -> Result<Arc<ManagedFile<RowMapSnapshot>>, String> {
        if blob_field.is_none() {
            return Ok(Arc::clone(&self.target));
        }
        let image = Arc::new(VfsFileSnapshot::new(
            row_image_pin(&self.source.rows),
            Arc::<[u8]>::from([]),
        ));
        self.writer
            .runtime
            .block_on(self.writer.cache.managed_file(image))
            .map_err(|error| {
                format!("mounted replacement scratch target could not be admitted: {error:?}")
            })
    }

    #[allow(clippy::type_complexity)]
    /// Re-admits an unbound scratch's candidate under the Blob `blob_field` target, the same
    /// target a Blob-bound create uses, so the rename commits through that target.
    pub(crate) fn retarget_unbound_scratch(
        &self,
        scratch: &TemporarySave<RowMapSnapshot>,
        blob_field: &BlobField,
    ) -> Result<
        (
            Arc<ManagedFile<RowMapSnapshot>>,
            Arc<TemporarySave<RowMapSnapshot>>,
        ),
        String,
    > {
        let target = self.temporary_target(Some(blob_field))?;
        let retargeted = self
            .writer
            .runtime
            .block_on(target.begin_temporary_replacement(DRAFT_FILE_BOUND_BYTES));
        let len = self.writer.runtime.block_on(scratch.candidate_len());
        let mut offset = 0_u64;
        while offset < len {
            let wanted = usize::try_from(len - offset)
                .unwrap_or(usize::MAX)
                .min(CANDIDATE_CHUNK_BYTES);
            let bytes = self
                .writer
                .runtime
                .block_on(scratch.read_range(offset, wanted))
                .map_err(|error| format!("scratch candidate could not be read: {error:?}"))?;
            if bytes.is_empty() {
                return Err("scratch candidate ended before its length".to_owned());
            }
            self.writer
                .runtime
                .block_on(retargeted.write_at(offset, &bytes))
                .map_err(|error| format!("scratch candidate could not be moved: {error:?}"))?;
            offset += bytes.len() as u64;
        }
        Ok((target, Arc::new(retargeted)))
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn fsync_temporary(
        &self,
        scratch: &TemporarySave<RowMapSnapshot>,
        identity: &TemporaryScratchIdentity,
        schema_identity: &TableSchemaIdentity,
        baseline: Option<&[u8]>,
    ) -> Result<(), String> {
        let store = &self.writer.scratch_store;
        self.writer
            .runtime
            .block_on(scratch.fsync_scratch_with(|_, snapshot| async move {
                store.persist(identity, schema_identity, baseline, &snapshot)
            }))
            .map(|_| ())
            .map_err(|failure| match failure {
                FsyncFailure::Backing(error) => candidate_unreadable(error),
                FsyncFailure::Activation(message) => message,
            })
    }

    /// Commits `draft` against `expected_baseline_row`, the durable row the save compares (CAS).
    /// Returns the row the save accepted, which the committing handle captures as its baseline.
    pub(crate) fn commit_document_against(
        self: &Arc<Self>,
        draft: &EditDraft<RowMapSnapshot>,
        record_baseline: bool,
        expected_baseline_row: Option<Vec<u8>>,
        opened: &TableSchemaIdentity,
    ) -> Result<Option<Vec<u8>>, DocumentCommitError> {
        let save = self
            .durable_save_with_baseline(expected_baseline_row, opened.clone())
            .map_err(DocumentCommitError::Failed)?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(self.target.commit_durable_document_save(
                draft,
                &self.writer.state,
                save,
                move |coordinates, bytes| async move {
                    let (candidate, row_bytes) = row_context.writer.candidate(
                        &row_context,
                        &coordinates,
                        &bytes,
                        None,
                        None,
                    )?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let accepted_row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if record_baseline {
                    self.record_accepted_row(accepted_row.clone());
                }
                Ok(accepted_row)
            }
            Ok(TemporaryRenameOutcome::Rejected { diagnostic }) => {
                Err(DocumentCommitError::Rejected(format!(
                    "mounted row document was rejected by semantic admission: {}",
                    refusal_text(
                        orna_conformance::is_table_assertion_false(diagnostic),
                        diagnostic
                    )
                )))
            }
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(save_failure("mounted row save failed", error)),
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    #[allow(clippy::too_many_arguments)]
    /// Commits a replacement for a large Str's sibling text file (profiles/vfs-1.md). The bytes
    /// must be valid UTF-8 and replace the Str through the durable row transaction; no Blob
    /// binding is created.
    pub(crate) fn commit_text_temporary(
        self: &Arc<Self>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &TemporarySave<RowMapSnapshot>,
        field: &BlobField,
        replacement_bytes: Arc<[u8]>,
        expected_baseline_row: Option<Vec<u8>>,
        schema_identity: TableSchemaIdentity,
        record_baseline: bool,
    ) -> Result<AcceptedReplacement, DocumentCommitError> {
        if std::str::from_utf8(&replacement_bytes).is_err() {
            return Err(DocumentCommitError::Rejected(
                "mounted text replacement must be valid UTF-8".into(),
            ));
        }
        let document = self.writer.runtime.block_on(self.target.open_read());
        let document_length = usize::try_from(document.len())
            .map_err(|_| "mounted row document length is invalid".to_owned())?;
        let document_bytes: Arc<[u8]> = Arc::from(
            document
                .read_at(0, document_length)
                .map_err(|_| "mounted row document could not be read".to_owned())?,
        );
        let save = self.durable_save_with_baseline(expected_baseline_row, schema_identity)?;
        let row_context = Arc::clone(self);
        let field_name = field.name.clone();
        let admitted_text = Arc::clone(&replacement_bytes);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let outcome = self
            .writer
            .runtime
            .block_on(target.commit_durable_temporary_rename(
                scratch,
                &self.writer.state,
                save,
                move |coordinates, _document| async move {
                    let (candidate, row_bytes) = row_context.writer.candidate(
                        &row_context,
                        &coordinates,
                        &document_bytes,
                        None,
                        Some((field_name.as_str(), &admitted_text[..])),
                    )?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        self.settle_replacement(
            outcome,
            &accepted_row,
            field,
            Some(Arc::new(CandidateSnapshot::from(Arc::clone(
                &replacement_bytes,
            )))),
            record_baseline,
        )
    }

    #[allow(clippy::needless_pass_by_value)]
    /// Records an accepted replacement on its field, or maps the save outcome to a refusal. The
    /// accepted row is recorded as the baseline when the caller asked for one.
    pub(crate) fn settle_replacement(
        self: &Arc<Self>,
        outcome: Result<
            TemporaryRenameOutcome<RowMapSnapshot>,
            VfsSaveError<orna_runtime::TableActivationError>,
        >,
        accepted_row: &Mutex<Option<Vec<u8>>>,
        field: &BlobField,
        replacement_bytes: Option<Arc<CandidateSnapshot>>,
        record_baseline: bool,
    ) -> Result<AcceptedReplacement, DocumentCommitError> {
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let replacement_bytes = replacement_bytes.ok_or_else(|| {
                    DocumentCommitError::Failed("replacement Blob bytes were not admitted".into())
                })?;
                if let Some(field) = self
                    .blob_fields
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter_mut()
                    .find(|candidate| candidate.name == field.name)
                {
                    field.replacement_bytes = Some(Arc::clone(&replacement_bytes));
                } else {
                    return Err(DocumentCommitError::Failed(
                        "replaced field disappeared from its row".into(),
                    ));
                }
                let row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if record_baseline {
                    self.record_accepted_row(row.clone());
                }
                Ok(AcceptedReplacement {
                    row,
                    bytes: replacement_bytes,
                })
            }
            Ok(TemporaryRenameOutcome::Rejected { diagnostic }) => {
                Err(DocumentCommitError::Rejected(format!(
                    "mounted replacement was rejected by semantic admission: {}",
                    refusal_text(
                        orna_conformance::is_table_assertion_false(diagnostic),
                        diagnostic
                    )
                )))
            }
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(save_failure("mounted replacement failed", error)),
        }
    }

    pub(crate) fn commit_blob_temporary(
        self: &Arc<Self>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &TemporarySave<RowMapSnapshot>,
        field: &BlobField,
        expected_baseline_row: Option<Vec<u8>>,
        schema_identity: TableSchemaIdentity,
        record_baseline: bool,
    ) -> Result<AcceptedReplacement, DocumentCommitError> {
        if field.text {
            let replacement_bytes = self
                .writer
                .runtime
                .block_on(scratch.candidate_bytes())
                .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
            return self.commit_text_temporary(
                target,
                scratch,
                field,
                replacement_bytes,
                expected_baseline_row,
                schema_identity,
                record_baseline,
            );
        }
        // The digest is streamed outside the vfs operation lock. A write that
        // lands before admission makes the vfs identity check refuse the attempt
        // before any row is touched; that refusal is retried once against a fresh
        // digest, and a second refusal is reported instead of publishing bytes.
        let mut retried = false;
        loop {
            let expected_identity = self.scratch_identity(scratch)?;
            match self.commit_blob_attempt(
                target,
                scratch,
                field,
                expected_identity,
                expected_baseline_row.clone(),
                schema_identity.clone(),
                record_baseline,
            )? {
                BlobAttempt::Settled(accepted) => return Ok(accepted),
                BlobAttempt::RacingWrite if !retried => retried = true,
                BlobAttempt::RacingWrite => {
                    return Err(DocumentCommitError::Failed(
                        "scratch changed during Blob commit; save again".into(),
                    ));
                }
            }
        }
    }

    /// Streams the scratch candidate in bounded ranges to its SHA-256 identity,
    /// so hashing never holds the whole Blob.
    pub(crate) fn scratch_identity(
        self: &Arc<Self>,
        scratch: &TemporarySave<RowMapSnapshot>,
    ) -> Result<ContentIdentity, DocumentCommitError> {
        let length = self.writer.runtime.block_on(scratch.candidate_len());
        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        while offset < length {
            let wanted = usize::try_from(length - offset)
                .unwrap_or(usize::MAX)
                .min(CANDIDATE_CHUNK_BYTES);
            let chunk = self
                .writer
                .runtime
                .block_on(scratch.read_range(offset, wanted))
                .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error)))?;
            if chunk.is_empty() {
                return Err(DocumentCommitError::Failed(
                    "scratch candidate ended before its length".into(),
                ));
            }
            hasher.update(&chunk);
            offset += chunk.len() as u64;
        }
        let digest: [u8; 32] = hasher.finalize().into();
        ContentIdentity::new(length, digest)
            .map_err(|error| format!("replacement Blob identity is invalid: {error:?}").into())
    }

    #[allow(clippy::too_many_arguments)]
    /// One capture-and-commit attempt against `expected_identity`. A racing
    /// scratch write is reported as `RacingWrite`; no row was touched by then.
    pub(crate) fn commit_blob_attempt(
        self: &Arc<Self>,
        target: &Arc<ManagedFile<RowMapSnapshot>>,
        scratch: &TemporarySave<RowMapSnapshot>,
        field: &BlobField,
        expected_identity: ContentIdentity,
        expected_baseline_row: Option<Vec<u8>>,
        schema_identity: TableSchemaIdentity,
        record_baseline: bool,
    ) -> Result<BlobAttempt, DocumentCommitError> {
        let capture = self
            .writer
            .repository
            .capture_capability(*self.source.binding.relation_id())
            .map_err(|error| format!("replacement Blob capture is unavailable: {error:?}"))?;
        let document = self.writer.runtime.block_on(self.target.open_read());
        let document_length = usize::try_from(document.len())
            .map_err(|_| "mounted row document length is invalid".to_owned())?;
        let document_bytes: Arc<[u8]> = Arc::from(
            document
                .read_at(0, document_length)
                .map_err(|_| "mounted row document could not be read".to_owned())?,
        );
        let save = self.durable_save_with_baseline(expected_baseline_row, schema_identity)?;
        let row_context = Arc::clone(self);
        let field_name = field.name.clone();
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let blob_slot: Arc<Mutex<Option<Arc<CandidateSnapshot>>>> = Arc::new(Mutex::new(None));
        let blob_output = Arc::clone(&blob_slot);
        let outcome = self
            .writer
            .runtime
            .block_on(self.writer.cache.commit_durable_blob_rename(
                target,
                scratch,
                &self.writer.state,
                save,
                &capture,
                expected_identity,
                &field.media_type,
                field.suffix.as_deref(),
                move |coordinates, blob_bytes, binding| async move {
                    *blob_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(Arc::new(blob_bytes));
                    let (candidate, row_bytes) = row_context.writer.candidate(
                        &row_context,
                        &coordinates,
                        &document_bytes,
                        Some((field_name, binding)),
                        None,
                    )?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    Ok(candidate)
                },
                &NoFault,
            ));
        if matches!(
            outcome,
            Err(VfsSaveError::BlobCapture(
                VfsBlobReplacementError::IdentityMismatch { .. }
            ))
        ) {
            return Ok(BlobAttempt::RacingWrite);
        }
        self.settle_replacement(
            outcome,
            &accepted_row,
            field,
            blob_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
            record_baseline,
        )
        .map(BlobAttempt::Settled)
    }

    pub(crate) fn commit_temporary(
        self: &Arc<Self>,
        scratch: &TemporarySave<RowMapSnapshot>,
        expected_baseline_row: Option<Vec<u8>>,
        schema_identity: TableSchemaIdentity,
        record_baseline: bool,
    ) -> Result<Arc<CandidateSnapshot>, DocumentCommitError> {
        let save = self.durable_save_with_baseline(expected_baseline_row, schema_identity)?;
        let row_context = Arc::clone(self);
        let accepted_row = Arc::new(Mutex::new(None));
        let accepted_row_output = Arc::clone(&accepted_row);
        let accepted_snapshot: Arc<Mutex<Option<CandidateSnapshot>>> = Arc::new(Mutex::new(None));
        let accepted_snapshot_output = Arc::clone(&accepted_snapshot);
        let backing_io_failure = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let backing_io_failure_output = Arc::clone(&backing_io_failure);
        let outcome = self
            .writer
            .runtime
            .block_on(self.target.commit_durable_temporary_rename(
                scratch,
                &self.writer.state,
                save,
                move |coordinates, bytes| async move {
                    let document = candidate_document_bytes(&bytes).map_err(|_| {
                        backing_io_failure_output.store(true, std::sync::atomic::Ordering::Relaxed);
                        orna_runtime::TableActivationError::Runtime(
                            orna_runtime::RuntimeError::InvalidTableMutation,
                        )
                    })?;
                    let (candidate, row_bytes) = row_context.writer.candidate(
                        &row_context,
                        &coordinates,
                        &document,
                        None,
                        None,
                    )?;
                    *accepted_row_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(row_bytes);
                    // The retained view shares the spool handle; no RAM copy outlives the callback.
                    let retained = bytes.duplicate().map_err(|_| {
                        backing_io_failure_output.store(true, std::sync::atomic::Ordering::Relaxed);
                        orna_runtime::TableActivationError::Runtime(
                            orna_runtime::RuntimeError::InvalidTableMutation,
                        )
                    })?;
                    *accepted_snapshot_output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(retained);
                    Ok(candidate)
                },
                &NoFault,
            ));
        match outcome {
            Ok(TemporaryRenameOutcome::Applied { .. }) => {
                let accepted_row = accepted_row
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if record_baseline {
                    self.record_accepted_row(accepted_row);
                }
                // A replayed save publishes no callback, so the retained view is the
                // scratch's own candidate, shared through its spool handle.
                match accepted_snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    Some(snapshot) => Ok(Arc::new(snapshot)),
                    None => self
                        .writer
                        .runtime
                        .block_on(scratch.candidate_snapshot())
                        .map(Arc::new)
                        .map_err(|error| DocumentCommitError::Failed(candidate_unreadable(error))),
                }
            }
            Ok(TemporaryRenameOutcome::Rejected { diagnostic }) => {
                Err(DocumentCommitError::Rejected(format!(
                    "temporary mounted row document was rejected by semantic admission: {}",
                    refusal_text(
                        orna_conformance::is_table_assertion_false(diagnostic),
                        diagnostic
                    )
                )))
            }
            Ok(TemporaryRenameOutcome::Stale { .. }) => Err(DocumentCommitError::Stale),
            Err(error) => Err(temporary_commit_failure(
                backing_io_failure.load(std::sync::atomic::Ordering::Relaxed),
                error,
            )),
        }
    }
}
