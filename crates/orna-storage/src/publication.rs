use std::{fmt, sync::Arc};

use orna_repository::{
    GitCommitRef, HistoricalFormatContext, IndexGeneration, IndexMaintenance, ManagedPath,
    PrivateCommit, PublicationJournal, PublicationJournalEntry, Repository, RepositoryError,
    RepositoryFormat,
};
use orna_runtime::{PublicationCommitId, PublicationFreeze, RuntimeState, TableMutation};

use crate::Error;
use crate::store_root::{FrozenStoreRoot, build_frozen_publication_store_root};
use crate::{map_publication_repository_error, repository_format_error};

/// Index churn and reuse one publication measured while maintaining the row
/// index. Index nodes and stored payload objects are counted apart, so index
/// churn is never read as media reuse (ORP-1 Gate C).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PublicationChurn {
    index_objects_created: usize,
    index_objects_reused: usize,
    payload_objects_reused: usize,
}

impl PublicationChurn {
    /// New index nodes this publication wrote.
    pub const fn index_objects_created(&self) -> usize {
        self.index_objects_created
    }

    /// Previous index nodes the publication's root still names unchanged.
    pub const fn index_objects_reused(&self) -> usize {
        self.index_objects_reused
    }

    /// Stored value and Blob-descriptor objects reused unchanged.
    pub const fn payload_objects_reused(&self) -> usize {
        self.payload_objects_reused
    }

    pub(crate) fn add(&mut self, maintenance: &IndexMaintenance) {
        self.index_objects_created += maintenance.index_objects_created().len();
        self.index_objects_reused += maintenance.index_objects_reused().len();
        self.payload_objects_reused += maintenance.payload_objects_reused().len();
    }
}

/// The shared maintenance lease a prepared publication holds from preparation
/// until its candidate is anchored (ORNA-CAP-006). An exclusive maintenance
/// boundary (GC or repack) is refused while it is held, so the unreferenced
/// candidate objects cannot be pruned. Cloned handles share one lease.
#[derive(Clone)]
struct PreparedMaintenanceLease(Arc<orna_repository::MaintenanceLease>);

impl fmt::Debug for PreparedMaintenanceLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedMaintenanceLease")
    }
}

impl PartialEq for PreparedMaintenanceLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PreparedMaintenanceLease {}

/// A prepared, runtime-bound publication. Preparation creates only private
/// Git objects and a durable journal; `publish` and `complete` are separate so
/// a crash can be recovered at each PUB-1 boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePublicationCoordinator {
    expected_index: IndexGeneration,
    candidate: PrivateCommit,
    journal: PublicationJournal,
    // Retain protected transfer evidence across publication until both runtime
    // and journal completion have succeeded.
    protected_source_mutations: Vec<TableMutation>,
    // Held from preparation until publication succeeds; see
    // `PreparedMaintenanceLease`. `None` once the candidate is anchored.
    maintenance: Option<PreparedMaintenanceLease>,
    // Index churn of the frozen prefix's row-index maintenance.
    churn: PublicationChurn,
}

impl RuntimePublicationCoordinator {
    /// Prepares the exact prefix issued by `runtime` as a format-3 candidate
    /// and fails when that prefix has no visible change. Protected source
    /// mutations are retained from that prefix before any repository
    /// candidate is written.
    pub async fn prepare(
        repository: &Repository,
        runtime: &RuntimeState,
        expected_head: &GitCommitRef,
        expected_index: IndexGeneration,
        freeze: &PublicationFreeze,
        message: &str,
    ) -> Result<Self, Error> {
        Self::prepare_allow_noop(
            repository,
            runtime,
            expected_head,
            expected_index,
            freeze,
            message,
        )
        .await?
        .ok_or(Error::IncompleteStaging)
    }

    /// Reads the exact typed prefix named by `freeze` and reports the result as
    /// `None` when the frozen prefix is a no-op.
    ///
    /// Only a format-3 snapshot has a row writer. A format-1/2 `expected_head`
    /// keeps its versioned readers but fails with [`Error::LegacyReadOnly`]
    /// before the runtime prefix is read or any Git object is written
    /// (ORNA-UPGRADE-003, ORNA-ROW-013). A prefix whose native store root is
    /// unchanged from the committed snapshot has no visible change: the caller
    /// completes the frozen intent without a commit instead of preparing an
    /// empty publication.
    pub async fn prepare_allow_noop(
        repository: &Repository,
        runtime: &RuntimeState,
        expected_head: &GitCommitRef,
        expected_index: IndexGeneration,
        freeze: &PublicationFreeze,
        message: &str,
    ) -> Result<Option<Self>, Error> {
        // ORNA-CAP-006: candidate objects written below stay unreferenced until
        // the journal anchors them, so a maintenance boundary (GC or repack) must
        // not run while this prepare is in flight. The shared lease is kept on the
        // returned coordinator until publication anchors the candidate.
        let maintenance = repository
            .hold_maintenance_lease()
            .map_err(|error| match error {
                RepositoryError::RepositoryBusy => Error::RepositoryBusy,
                other => Error::repository_unavailable(other),
            })?;
        let format = writable_snapshot_context(repository, expected_head)?;
        let mutations = runtime
            .pending_table_mutations_through(freeze)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        if mutations.is_empty() || freeze.checkpoint.mutation_sequence == 0 {
            return Err(Error::IncompleteStaging);
        }
        let current_store_root = format
            .load_row_map(
                mutations[0]
                    .table_object_id()
                    .ok_or(Error::IncompleteStaging)?
                    .bytes(),
            )
            .map_err(repository_format_error)?
            .version()
            .store_root()
            .clone();
        let FrozenStoreRoot {
            graph,
            store_root,
            logical_rekeys,
            churn,
        } = build_frozen_publication_store_root(repository, expected_head, &mutations)?;
        if store_root == current_store_root && logical_rekeys.is_empty() {
            return Ok(None);
        }
        let candidate = repository
            .build_private_commit_with_store_root(expected_head, &graph, &store_root, message)
            .map_err(Error::repository_unavailable)?;
        // Store-only candidates still need a durable journal anchor. Keep the
        // format metadata file unchanged rather than projecting row keys into
        // legacy loose paths.
        let marker =
            ManagedPath::new(".orna/database.orna").map_err(Error::repository_unavailable)?;
        let paths = [marker.clone()];
        let captured = repository
            .capture_managed_publication_state(expected_head, &expected_index, &paths)
            .map_err(map_publication_repository_error)?;
        let expected = captured.into_iter().next().ok_or_else(|| {
            Error::repository_unavailable_reason("publication capture returned no managed state")
        })?;
        let entry = PublicationJournalEntry::new(marker, expected.clone(), expected);
        let base_index_tree = expected_index
            .tree()
            .cloned()
            .ok_or(Error::IncompleteStaging)?;
        let journal = PublicationJournal::new_with_runtime_intent(
            expected_head.clone(),
            candidate.commit().clone(),
            base_index_tree,
            freeze.intent_id,
            vec![entry],
        )
        .and_then(|journal| journal.with_store_root(&store_root))
        .and_then(|journal| journal.with_logical_rekeys(logical_rekeys))
        .map_err(Error::repository_unavailable)?;
        let protected_source_mutations = mutations
            .iter()
            .filter(|mutation| !mutation.protected_content_transfers().is_empty())
            .cloned()
            .collect();
        Ok(Some(Self {
            expected_index,
            candidate,
            journal,
            protected_source_mutations,
            maintenance: Some(PreparedMaintenanceLease(Arc::new(maintenance))),
            churn,
        }))
    }

    pub fn candidate(&self) -> &PrivateCommit {
        &self.candidate
    }

    pub fn journal(&self) -> &PublicationJournal {
        &self.journal
    }

    /// Index churn and payload reuse measured while this publication's row
    /// index was maintained. Index nodes and stored payloads are reported
    /// apart, so index churn is never read as media reuse.
    pub fn churn(&self) -> &PublicationChurn {
        &self.churn
    }

    /// Executes repository ref, index, and worktree boundaries. The journal
    /// intentionally remains at `WorktreeReconciled` after this returns.
    pub fn publish(&mut self, repository: &Repository) -> Result<IndexGeneration, Error> {
        self.publish_with_stage_hook(repository, |_| {})
    }

    /// Observes durable ref and index journal stages.
    pub fn publish_with_stage_hook(
        &mut self,
        repository: &Repository,
        on_stage: impl FnMut(orna_repository::PublicationJournalStage),
    ) -> Result<IndexGeneration, Error> {
        let generation = repository
            .publish_candidate_with_stage_hook(
                &self.expected_index,
                &self.candidate,
                &mut self.journal,
                on_stage,
            )
            .map_err(map_publication_repository_error)?;
        // The ref and journal now anchor the candidate, so the lease taken at
        // preparation is no longer needed (ORNA-CAP-006).
        self.maintenance = None;
        Ok(generation)
    }

    /// Completes the runtime transaction first, then advances and clears the
    /// repository journal. Runtime completion is idempotent, so a failure
    /// between these two durable steps remains recoverable.
    pub async fn complete(
        &mut self,
        repository: &Repository,
        runtime: &RuntimeState,
        freeze: &PublicationFreeze,
    ) -> Result<(), Error> {
        if freeze.intent_id
            != self
                .journal
                .runtime_intent_id()
                .ok_or(Error::InvalidTransition)?
        {
            return Err(Error::InvalidTransition);
        }
        let commit = publication_commit_id(self.candidate.commit())?;
        if self.journal.stage() != orna_repository::PublicationJournalStage::WorktreeReconciled {
            return Err(Error::PublicationPending);
        }
        verify_reconciled_publication(repository, &self.journal)?;
        runtime
            .complete_publication(freeze, &commit)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        // Gate D boundary: the runtime tail is consumed but the repository
        // journal still names WorktreeReconciled; recovery must complete the
        // runtime step idempotently before clearing the journal (ORNA-PUB-006).
        orna_repository::publication_crash_barrier("runtime-complete");
        repository
            .mark_runtime_complete(freeze.intent_id, &mut self.journal)
            .map_err(Error::repository_unavailable)?;
        self.protected_source_mutations.clear();
        Ok(())
    }

    /// Recovers a persisted runtime publication after a process interruption.
    /// Repository reconciliation is completed before the runtime prefix is
    /// consumed; every mismatch leaves the journal and pending tail intact.
    ///
    /// A journal left by a retired loose or compact writer fails with
    /// [`Error::LegacyReadOnly`] before any ref, index, worktree or runtime
    /// change: this build cannot finish a legacy publication.
    pub async fn recover(
        repository: &Repository,
        runtime: &RuntimeState,
    ) -> Result<Option<IndexGeneration>, Error> {
        let Some(journal) = repository
            .read_publication_journal()
            .map_err(map_publication_repository_error)?
        else {
            // Without a journal, an Orna-marked lock may still have been left
            // before Prepared; the repository reclaims it before reporting none.
            return repository
                .recover_publication()
                .map_err(map_publication_repository_error);
        };
        refuse_retired_writer_journal(repository, &journal)?;
        if repository
            .publication_journal_is_reviewed(&journal)
            .map_err(map_publication_repository_error)?
        {
            // A reviewed commit is completed by `commit_reviewed_candidate`,
            // which records its runtime intent with `mark_runtime_complete`.
            // It never had a runtime freeze, so none is requested here.
            return match repository.recover_publication() {
                Ok(index) => Ok(index),
                Err(
                    RepositoryError::PublicationPending
                    | RepositoryError::RuntimeCompletionRequired,
                ) => Err(Error::PublicationPending),
                Err(error) => Err(map_publication_repository_error(error)),
            };
        }
        let intent_id = journal
            .runtime_intent_id()
            .ok_or(Error::InvalidTransition)?;
        let freeze = runtime
            .publication_freeze(intent_id)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        let commit = publication_commit_id(journal.new_head())?;

        if matches!(
            journal.stage(),
            orna_repository::PublicationJournalStage::RuntimeCompleted
                | orna_repository::PublicationJournalStage::Complete
        ) {
            runtime
                .complete_publication(&freeze, &commit)
                .await
                .map_err(|_| Error::RuntimeUnavailable)?;
            return repository
                .recover_publication()
                .map_err(map_publication_repository_error);
        }

        match repository.recover_publication() {
            Ok(index) => return Ok(index),
            Err(RepositoryError::PublicationPending) => {
                return Err(Error::PublicationPending);
            }
            Err(RepositoryError::RuntimeCompletionRequired) => {}
            Err(error) => return Err(map_publication_repository_error(error)),
        }

        let mut journal = repository
            .read_publication_journal()
            .map_err(map_publication_repository_error)?
            .ok_or(Error::InvalidTransition)?;
        if journal.stage() != orna_repository::PublicationJournalStage::WorktreeReconciled
            || journal.runtime_intent_id() != Some(intent_id)
        {
            return Err(Error::InvalidTransition);
        }
        verify_reconciled_publication(repository, &journal)?;
        runtime
            .complete_publication(&freeze, &commit)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        repository
            .mark_runtime_complete(intent_id, &mut journal)
            .map_err(Error::repository_unavailable)?;
        repository
            .index_generation()
            .map(Some)
            .map_err(Error::repository_unavailable)
    }
}

fn publication_commit_id(commit: &GitCommitRef) -> Result<PublicationCommitId, Error> {
    PublicationCommitId::new(commit.as_str().as_bytes().to_vec())
        .map_err(|_| Error::InvalidObjectId)
}

/// Both completion paths consume the runtime tail only after the repository
/// confirms that HEAD holds the published commit and that the index still
/// matches the journal, so a moved index leaves the pending batch intact
/// (ORNA-PUB-009, ORNA-PUB-011).
fn verify_reconciled_publication(
    repository: &Repository,
    journal: &PublicationJournal,
) -> Result<(), Error> {
    if !repository
        .head_contains_commit(journal.new_head())
        .map_err(map_publication_repository_error)?
    {
        return Err(Error::RefConflict);
    }
    repository
        .verify_reconciled_index(journal)
        .map_err(map_publication_repository_error)
}
/// Opens the committed format context at `snapshot` only when that snapshot
/// has a row writer. Format-1/2 commits keep their versioned readers, but the
/// loose and compact row writers are retired (ORNA-UPGRADE-003, ORNA-ROW-013).
/// They fail with [`Error::LegacyReadOnly`], so publication onto them writes
/// nothing until the repository is migrated to format 3.
pub fn writable_snapshot_context(
    repository: &Repository,
    snapshot: &GitCommitRef,
) -> Result<HistoricalFormatContext, Error> {
    let context = repository
        .open_format_context_at_selector(snapshot.as_str())
        .map_err(Error::repository_unavailable)?;
    match context.repository_format() {
        RepositoryFormat::Format3 => Ok(context),
        legacy => Err(Error::LegacyReadOnly {
            format: legacy.number(),
        }),
    }
}

/// Refuses a publication journal written by a retired legacy writer. Only the
/// compact writer recorded a compact witness, and a non-migration journal over
/// a format-1/2 snapshot came from the loose writer. This build can finish
/// neither, so the journal, ref, index, worktree and runtime tail stay as
/// they are. A migration journal is the format-3 transition itself.
fn refuse_retired_writer_journal(
    repository: &Repository,
    journal: &PublicationJournal,
) -> Result<(), Error> {
    let compact = journal.compact_manifest().is_some();
    if !compact && journal.migration_continuity().is_some() {
        return Ok(());
    }
    match writable_snapshot_context(repository, journal.old_head()) {
        Err(error @ Error::LegacyReadOnly { .. }) => Err(error),
        // A compact witness over anything but a legacy base is malformed.
        _ if compact => Err(Error::InvalidPublicationJournal),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LoosePath;
    use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
    use bytes::Bytes;
    use orna_foundation::{CanonicalValue, OvbRaw};
    use orna_repository::{
        CompactManifest, CompactPublicationPlan, CompactPublicationRequest, CompactSegment,
        CompactSegmentRecord, CompactSegmentRole, ManagedFileChange, Uuid,
    };
    use parquet::file::{
        metadata::{FileMetaData, KeyValue, ParquetMetaData, ParquetMetaDataWriter},
        reader::{FileReader, SerializedFileReader},
    };
    use sha2::{Digest, Sha256};
    use std::{fs, path::Path, process::Command};
    use tempfile::TempDir;

    fn git(directory: &Path, arguments: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(directory)
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }

    fn git_output(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end_matches(['\r', '\n'])
            .to_owned()
    }

    fn repository() -> (TempDir, Repository) {
        let temp = TempDir::new().unwrap();
        git(temp.path(), &["init", "-b", "main"]);
        git(temp.path(), &["config", "user.email", "kieran@drewett.dev"]);
        git(temp.path(), &["config", "user.name", "kierandrewett"]);
        git(temp.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }

    fn compact_columns() -> Vec<u8> {
        let field_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        CanonicalValue::new(OvbRaw::Array(vec![OvbRaw::Array(vec![
            OvbRaw::Array(vec![OvbRaw::Tag(
                37,
                Box::new(OvbRaw::Bytes(field_id.as_bytes().to_vec())),
            )]),
            OvbRaw::Array(vec![OvbRaw::Text(format!("f_{}", field_id.simple()))]),
            OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".to_owned())]),
            OvbRaw::Text("int64".to_owned()),
            OvbRaw::Array(Vec::new()),
        ])]))
        .unwrap()
        .encode()
        .unwrap()
    }

    fn compact_schema() -> orna_foundation::SchemaDescriptor {
        let table = Uuid::from_u128(1);
        let field_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        orna_foundation::SchemaDescriptor::new(OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (
                OvbRaw::Int(1.into()),
                OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(table.as_bytes().to_vec()))),
            ),
            (
                OvbRaw::Int(2.into()),
                OvbRaw::Array(vec![OvbRaw::Tag(
                    37,
                    Box::new(OvbRaw::Bytes(field_id.as_bytes().to_vec())),
                )]),
            ),
            (
                OvbRaw::Int(3.into()),
                OvbRaw::Array(vec![OvbRaw::Array(vec![
                    OvbRaw::Tag(37, Box::new(OvbRaw::Bytes(field_id.as_bytes().to_vec()))),
                    OvbRaw::Text(format!("f_{}", field_id.simple())),
                    OvbRaw::Array(vec![OvbRaw::Int(0.into()), OvbRaw::Text("Int".into())]),
                    OvbRaw::Int(0.into()),
                    OvbRaw::Array(vec![OvbRaw::Int(0.into())]),
                ])]),
            ),
            (OvbRaw::Int(4.into()), OvbRaw::Array(Vec::new())),
        ]))
        .unwrap()
    }

    fn compact_schema_fingerprint(schema: &orna_foundation::SchemaDescriptor) -> [u8; 32] {
        let bytes = schema.encode().unwrap();
        let mut digest = Sha256::new();
        digest.update(b"orna.schema.v1\0");
        digest.update(bytes);
        digest.finalize().into()
    }

    fn compact_schema_hex(schema: [u8; 32]) -> String {
        schema
            .into_iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn compact_runtime_parquet() -> (Vec<u8>, [u8; 32]) {
        const PARQUET: &str = "UEFSMRUGFRAVEBWRgLnnCkwVAhUAFQIVABUAFQASAAABAAAAAAAAABkSAhkYCAEAAAAAAAAAGRgIAQAAAAAAAAAVAhkWAAAZHBYIFTwWAAAAFQIZLEgGc2NoZW1hFQIAFQQlABgiZl8wMThmMDAwMDAwMDA3MDAwODAwMDAwMDAwMDAwMDAwMQAWAhkcGRwmABwVBBklAAYZGCJmXzAxOGYwMDAwMDAwMDcwMDA4MDAwMDAwMDAwMDAwMDAxFQwWAhY8FkgmCDw2ACgIAQAAAAAAAAAYCAEAAAAAAAAAEREAABaCARUUFkQVPgAWPBYCJggWSBQAABl8GAxvcm5hLnByb2ZpbGUYEmNvbXBhY3Qtc3RvcmFnZS12MQAYCm9ybmEudGFibGUYJDAwMDAwMDAwLTAwMDAtMDAwMC0wMDAwLTAwMDAwMDAwMDAwMQAYEm9ybmEuc2NoZW1hLnNoYTI1NhhANThhMDg5NzBhZGMyMzE4N2U5Nzk0NDVlOTNiODQzN2ExNjI3ZWUxMDk0ZjI2NTg5YTJlZmVmZWE4NTFiMmExZQAYD29ybmEuc2NoZW1hLm92YhiYAXBRQUJBZGdsVUFBQUFBQUFBQUFBQUFBQUFBQUFBQUVDZ2RnbFVBR1BBQUFBQUhBQWdBQUFBQUFBQUFFRGdZWFlKVkFCandBQUFBQndBSUFBQUFBQUFBQUJlQ0ptWHpBeE9HWXdNREF3TURBd01EY3dNREE0TURBd01EQXdNREF3TURBd01EQXhnZ0JqU1c1MEFJRUFCSUE9ABgQb3JuYS5jb2x1bW5zLm92YhhgZ1lXQjJDVlFBWThBQUFBQWNBQ0FBQUFBQUFBQUFZRjRJbVpmTURFNFpqQXdNREF3TURBd056QXdNRGd3TURBd01EQXdNREF3TURBd01ER0NBR05KYm5SbGFXNTBOalNBABgMb3JuYS5lbmNvZGVyGA90ZXN0LWVuY29kZXItdjEAGBFvcm5hLnRlc3QucGF5bG9hZBgOY29tcGFjdCBvYmplY3QAGBlwYXJxdWV0LXJzIHZlcnNpb24gNTkuMy4wGRwcAAAA3AIAAFBBUjE=";
        let original = orna_value::base64_decode(PARQUET).unwrap();
        let reader = SerializedFileReader::new(Bytes::from(original.clone())).unwrap();
        let file = reader.metadata().file_metadata();
        let schema = compact_schema();
        let schema_ovb = schema.encode().unwrap();
        let fingerprint = compact_schema_fingerprint(&schema);
        let metadata = FileMetaData::new(
            1,
            file.num_rows(),
            file.created_by().map(str::to_owned),
            Some(vec![
                KeyValue::new("orna.profile".into(), Some("compact-storage-v1".into())),
                KeyValue::new("orna.table".into(), Some(Uuid::from_u128(1).to_string())),
                KeyValue::new(
                    "orna.schema.sha256".into(),
                    Some(compact_schema_hex(fingerprint)),
                ),
                KeyValue::new("orna.schema.ovb".into(), Some(BASE64.encode(schema_ovb))),
                KeyValue::new(
                    "orna.columns.ovb".into(),
                    Some(BASE64.encode(compact_columns())),
                ),
                KeyValue::new("orna.encoder".into(), Some("test-encoder-v1".into())),
            ]),
            file.schema_descr_ptr(),
            file.column_orders().cloned(),
        );
        let metadata = ParquetMetaData::new(metadata, reader.metadata().row_groups().to_vec());
        let mut footer = Vec::new();
        ParquetMetaDataWriter::new(&mut footer, &metadata)
            .finish()
            .unwrap();
        let footer_start = original.len()
            - 8
            - u32::from_le_bytes(
                original[original.len() - 8..original.len() - 4]
                    .try_into()
                    .unwrap(),
            ) as usize;
        let mut bytes = original[..footer_start].to_vec();
        bytes.extend(footer);
        (bytes, fingerprint)
    }

    /// Builds a compact candidate and its journal with the test-only legacy
    /// fixture writer, exactly as the retired compact writer left them before
    /// its ref advanced.
    fn legacy_compact_plan(
        repository: &Repository,
        freeze: &PublicationFreeze,
    ) -> CompactPublicationPlan {
        let (parquet, schema) = compact_runtime_parquet();
        let table = Uuid::from_u128(1);
        let segment_id = Uuid::from_u64_pair(0x018f_0000_0000_7000, 0x8000_0000_0000_0001);
        let segment_path = ManagedPath::new(format!(
            ".orna/storage/{table}/data/{}/{segment_id}.parquet",
            &segment_id.to_string()[..2]
        ))
        .unwrap();
        let segment = CompactSegment::new(CompactSegmentRecord {
            segment_id,
            role: CompactSegmentRole::Data,
            schema,
            encoder_version: "test-encoder-v1".to_owned(),
            relative_path: segment_path,
            bytes: parquet,
            min_key: CanonicalValue::int(1.into()).encode().unwrap(),
            max_key: CanonicalValue::int(1.into()).encode().unwrap(),
            row_count: 1,
            columns: compact_columns(),
            row_group_index: true,
            bloom: false,
        })
        .unwrap();
        let head = repository.head().unwrap().unwrap();
        repository
            .prepare_compact_publication(CompactPublicationRequest {
                expected_head: &head,
                expected_index: repository.index_generation().unwrap(),
                base: CompactManifest::empty(table, schema),
                runtime_intent_id: freeze.intent_id,
                cleanup_watermark: freeze.checkpoint.digest,
                segments: &[segment],
                message: "orna: publish compact runtime data",
            })
            .unwrap()
    }

    /// A committed format-1/2 snapshot: the legacy `format.orna` coordinate
    /// beside the legacy `database.orna` sidecar.
    fn legacy_repository(format: u8) -> (TempDir, Repository) {
        let (temp, _) = repository();
        fs::create_dir_all(temp.path().join(".orna")).unwrap();
        let record = match format {
            1 => "{repository_format: 1, storage_profile: \"compact-storage-v1\"}\n",
            2 => {
                "{repository_format: 2, row_profile: \"ORR-2\", blob_profile: \"OGB-1\", semantic_profile: \"SOV-2\", compact_profile: \"compact-storage-v2\"}\n"
            }
            other => panic!("no legacy record for format {other}"),
        };
        fs::write(temp.path().join(".orna/format.orna"), record).unwrap();
        fs::write(
            temp.path().join(".orna/database.orna"),
            "{database_id: \"9f237b7e-6844-4498-bcd5-d24641c07449\"}\n",
        )
        .unwrap();
        git(temp.path(), &["add", ".orna"]);
        git(temp.path(), &["commit", "-m", "legacy format metadata"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }

    /// A committed format-3 snapshot: the canonical `database.orna` record.
    fn format3_repository() -> (TempDir, Repository) {
        let (temp, _) = repository();
        fs::create_dir_all(temp.path().join(".orna")).unwrap();
        fs::write(
            temp.path().join(".orna/database.orna"),
            "{\n    repository_format: 3,\n    database_id: \"9f237b7e-6844-4498-bcd5-d24641c07449\",\n}\n",
        )
        .unwrap();
        git(temp.path(), &["add", ".orna"]);
        git(temp.path(), &["commit", "-m", "format-3 metadata"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }

    /// Opens a runtime holding one committed row and freezes it, so a
    /// publication has a real prefix to refuse.
    async fn frozen_runtime(repository: &Repository) -> (RuntimeState, PublicationFreeze) {
        let runtime = RuntimeState::open(
            repository,
            orna_runtime::RuntimeIdentity {
                database_id: [91; 16],
                repository_id: [92; 16],
            },
            [93; 32],
        )
        .await
        .unwrap();
        let lease = runtime.acquire_lease([94; 16]).await.unwrap();
        let context = runtime.begin_activation().await.unwrap();
        runtime
            .commit_table_activation(
                lease,
                &context,
                &[TableMutation::new(
                    [95; 16],
                    "Contact",
                    b"Alice".to_vec(),
                    Some(b"row".to_vec()),
                )
                .unwrap()],
                [96; 32],
                &orna_runtime::NoFault,
            )
            .await
            .unwrap();
        let freeze = runtime
            .freeze(
                [97; 16],
                &orna_runtime::Checkpoint {
                    generation: 1,
                    digest: [96; 32],
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        (runtime, freeze)
    }

    /// Everything a refused publication must leave untouched: the ref, the
    /// index, the worktree, the object store and the publication journal.
    fn publication_state(
        root: &Path,
        repository: &Repository,
    ) -> (
        Option<GitCommitRef>,
        IndexGeneration,
        String,
        String,
        Option<PublicationJournal>,
    ) {
        (
            repository.head().unwrap(),
            repository.index_generation().unwrap(),
            git_output(root, &["status", "--porcelain", "--untracked-files=all"]),
            git_output(root, &["count-objects", "-v"]),
            repository.read_publication_journal().unwrap(),
        )
    }

    #[test]
    fn coordinator_preserves_changed_ref_conflict() {
        assert!(matches!(
            map_publication_repository_error(RepositoryError::StaleHead),
            Error::RefConflict
        ));
    }

    /// ORNA-PUB-009 recovery row "Ref no longer H or N": an ordinary commit
    /// made after worktree reconciliation descends from the candidate, so the
    /// production coordinator completes the runtime tail and clears the
    /// journal without rewriting HEAD, index or worktree. The previous storage
    /// gate refused every HEAD other than the candidate with `RefConflict`.
    #[tokio::test]
    async fn coordinator_recovery_completes_a_descendant_head() {
        let (temp, repository) = repository();
        let (runtime, freeze) = frozen_runtime(&repository).await;
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(path.clone(), Some(b"row".to_vec()))],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            freeze.intent_id,
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"row".to_vec()),
            )],
        )
        .unwrap();
        repository
            .publish_candidate(&index, &candidate, &mut journal)
            .unwrap();

        fs::write(temp.path().join("ordinary.txt"), "later\n").unwrap();
        git(temp.path(), &["add", "ordinary.txt"]);
        git(temp.path(), &["commit", "-m", "ordinary follow-up"]);
        let follow_up = repository.head().unwrap().unwrap();

        RuntimePublicationCoordinator::recover(&repository, &runtime)
            .await
            .unwrap();
        assert_eq!(repository.read_publication_journal().unwrap(), None);
        assert_eq!(repository.head().unwrap().unwrap(), follow_up);
        assert_eq!(
            fs::read(temp.path().join("generated/row.orna")).unwrap(),
            b"row"
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("ordinary.txt")).unwrap(),
            "later\n"
        );
    }

    /// ORNA-PUB-011 and the recovery row "Index or affected path differs from
    /// both recorded states": once the runtime tail is consumed, an index that
    /// moved after reconciliation is a typed index conflict. It keeps the
    /// journal, the moved index and the edited file intact.
    #[tokio::test]
    async fn coordinator_recovery_after_runtime_completion_reports_a_moved_index() {
        let (temp, repository) = repository();
        let (runtime, freeze) = frozen_runtime(&repository).await;
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(path.clone(), Some(b"row".to_vec()))],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            freeze.intent_id,
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"row".to_vec()),
            )],
        )
        .unwrap();
        repository
            .publish_candidate(&index, &candidate, &mut journal)
            .unwrap();
        let commit = publication_commit_id(journal.new_head()).unwrap();
        runtime
            .complete_publication(&freeze, &commit)
            .await
            .unwrap();
        // Crash boundary after the RuntimeCompleted journal write and before
        // the Complete write: recovery must finish the repository step.
        journal
            .advance(orna_repository::PublicationJournalStage::RuntimeCompleted)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        fs::write(temp.path().join("generated/row.orna"), "edited\n").unwrap();
        git(temp.path(), &["add", "generated/row.orna"]);
        let moved_index = repository.index_generation().unwrap();

        assert_eq!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::RecoveryIndexConflict)
        );
        let journal = repository
            .read_publication_journal()
            .unwrap()
            .expect("a typed index conflict keeps the journal");
        assert_eq!(
            journal.stage(),
            orna_repository::PublicationJournalStage::RuntimeCompleted
        );
        assert_eq!(repository.index_generation().unwrap(), moved_index);
        assert_eq!(
            fs::read_to_string(temp.path().join("generated/row.orna")).unwrap(),
            "edited\n"
        );
    }

    #[tokio::test]
    async fn coordinator_recovery_preserves_a_malformed_journal_read() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime::RuntimeIdentity {
                database_id: [61; 16],
                repository_id: [62; 16],
            },
            [63; 32],
        )
        .await
        .unwrap();
        fs::create_dir_all(repository.runtime_paths().root()).unwrap();
        fs::write(
            repository
                .runtime_paths()
                .root()
                .join("publication-journal.bin"),
            b"truncated journal",
        )
        .unwrap();

        assert!(matches!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::InvalidPublicationJournal)
        ));
        assert_eq!(
            fs::read(
                repository
                    .runtime_paths()
                    .root()
                    .join("publication-journal.bin")
            )
            .unwrap(),
            b"truncated journal"
        );
    }

    #[tokio::test]
    async fn legacy_head_is_refused_before_any_publication_write() {
        for format in [1, 2] {
            let (temp, repository) = legacy_repository(format);
            let (runtime, freeze) = frozen_runtime(&repository).await;
            let head = repository.head().unwrap().unwrap();
            let before = publication_state(temp.path(), &repository);
            let pending = runtime.pending().await.unwrap();

            assert_eq!(
                RuntimePublicationCoordinator::prepare_allow_noop(
                    &repository,
                    &runtime,
                    &head,
                    repository.index_generation().unwrap(),
                    &freeze,
                    "orna: publish runtime data",
                )
                .await,
                Err(Error::LegacyReadOnly { format })
            );
            assert_eq!(
                RuntimePublicationCoordinator::prepare(
                    &repository,
                    &runtime,
                    &head,
                    repository.index_generation().unwrap(),
                    &freeze,
                    "orna: publish runtime data",
                )
                .await,
                Err(Error::LegacyReadOnly { format })
            );

            // No candidate object, ref, index, worktree row or journal was
            // written, and the frozen prefix is still pending.
            assert_eq!(publication_state(temp.path(), &repository), before);
            assert!(!temp.path().join("Contact").exists());
            assert_eq!(runtime.pending().await.unwrap(), pending);
            assert_eq!(runtime.pending_through(&freeze).await.unwrap().len(), 1);
            // The legacy commit stays readable through its versioned reader.
            assert_eq!(
                repository
                    .open_format_context_at_selector(head.as_str())
                    .unwrap()
                    .repository_format_number(),
                format
            );
        }
        assert_eq!(
            Error::LegacyReadOnly { format: 2 }.to_string(),
            "repository format 2 is a read-only legacy snapshot; migrate it to format 3 before publishing"
        );
    }

    #[tokio::test]
    async fn recovery_refuses_a_retired_loose_journal_without_touching_it() {
        let (temp, repository) = legacy_repository(2);
        let (runtime, freeze) = frozen_runtime(&repository).await;
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let path = LoosePath::for_key("Contact", &["Alice".into()]).unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(
                    path.as_managed_path().clone(),
                    Some(b"row".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            freeze.intent_id,
            vec![PublicationJournalEntry::new(
                path.as_managed_path().clone(),
                None,
                Some(b"row".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        // The retired loose writer was interrupted after advancing its ref.
        repository.advance_current_ref(&head, &candidate).unwrap();
        let before = publication_state(temp.path(), &repository);
        let pending = runtime.pending().await.unwrap();

        assert_eq!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::LegacyReadOnly { format: 2 })
        );
        assert_eq!(publication_state(temp.path(), &repository), before);
        assert_eq!(
            repository.read_publication_journal().unwrap(),
            Some(journal)
        );
        assert!(!temp.path().join(path.as_managed_path().as_path()).exists());
        assert_eq!(runtime.pending().await.unwrap(), pending);
    }

    #[tokio::test]
    async fn recovery_refuses_a_retired_compact_journal_without_touching_it() {
        let (temp, repository) = legacy_repository(2);
        let (runtime, freeze) = frozen_runtime(&repository).await;
        let plan = legacy_compact_plan(&repository, &freeze);
        repository
            .write_publication_journal(plan.journal())
            .unwrap();
        let before = publication_state(temp.path(), &repository);
        let pending = runtime.pending().await.unwrap();

        assert_eq!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::LegacyReadOnly { format: 2 })
        );
        assert_eq!(publication_state(temp.path(), &repository), before);
        assert_eq!(
            repository.read_publication_journal().unwrap().as_ref(),
            Some(plan.journal())
        );
        assert_eq!(runtime.pending().await.unwrap(), pending);
    }

    /// Only the retired compact writer recorded a compact witness, and it
    /// wrote only onto format-1/2 snapshots. Over a readable format-3 base the
    /// witness is malformed evidence, so recovery refuses the journal.
    #[tokio::test]
    async fn recovery_refuses_a_compact_journal_without_a_legacy_base() {
        let (temp, repository) = format3_repository();
        let (runtime, freeze) = frozen_runtime(&repository).await;
        let plan = legacy_compact_plan(&repository, &freeze);
        assert!(writable_snapshot_context(&repository, plan.journal().old_head()).is_ok());
        repository
            .write_publication_journal(plan.journal())
            .unwrap();
        let before = publication_state(temp.path(), &repository);

        assert_eq!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::InvalidPublicationJournal)
        );
        assert_eq!(publication_state(temp.path(), &repository), before);
        assert_eq!(runtime.pending_through(&freeze).await.unwrap().len(), 1);
    }
}
