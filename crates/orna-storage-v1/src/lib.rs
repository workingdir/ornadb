//! A bounded Orna 1.0 storage/publication slice.
//!
//! This crate models the deterministic portion of loose-row materialisation
//! and PUB-1.  It deliberately performs neither provider nor Git I/O: an
//! adapter must supply observations and enact returned plans.  Unknown or
//! changed external state is a typed conflict, never permission to overwrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orna_foundation_v1::{CwdCapture, RepositoryGenerationAdapter, RepositoryIdentity};
use orna_repository_v1::{
    GitCommitRef, IndexGeneration, ManagedFileChange, ManagedPath, PrivateCommit,
    PublicationJournal, PublicationJournalEntry, Repository, RepositoryError,
};
use orna_runtime_v1::{PublicationCommitId, PublicationFreeze, RuntimeState, TableMutation};
use orna_value_v1::{
    path_decode_key_components, path_encode_key_components, path_validate_relative_components,
};
use sha2::{Digest, Sha256};

/// An opaque, deterministic mutation identity supplied by the runtime.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MutationId(String);

impl MutationId {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 || !value.is_ascii() {
            return Err(Error::InvalidMutationId);
        }
        Ok(Self(value))
    }
}

/// A content digest; this is used only for equality/revalidation, not as a
/// substitute for an adapter's durable Git object validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RowHash([u8; 32]);

impl RowHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

/// A validated ordinary loose-row path derived with the v1 path codec.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LoosePath(ManagedPath);

impl LoosePath {
    /// Admits a discovered table-relative encoded row path. The components
    /// exclude the table root and include the final `.orna` extension.
    /// Decoding rejects aliases before construction; this does not perform
    /// filesystem I/O, resolve symlinks, or interpret the key's logical type.
    pub fn from_encoded_key(
        table_root: impl AsRef<str>,
        components: &[String],
    ) -> Result<Self, Error> {
        let key = path_decode_key_components(components).map_err(|_| Error::InvalidKey)?;
        Self::for_key(table_root, &key)
    }

    pub fn for_key(table_root: impl AsRef<str>, key: &[String]) -> Result<Self, Error> {
        let root = table_root.as_ref();
        if root.is_empty() || root.starts_with('.') || root.contains('/') || root.contains('\\') {
            return Err(Error::InvalidTableRoot);
        }
        let components = path_encode_key_components(key).map_err(|_| Error::InvalidKey)?;
        // Logical keys may contain separators, traversal spellings, or empty
        // strings. Only their encoded filesystem components require this check.
        path_validate_relative_components(&components).map_err(|_| Error::InvalidKey)?;
        let path = format!("{root}/{}", components.join("/"));
        ManagedPath::new(path)
            .map(Self)
            .map_err(|_| Error::UnsafePath)
    }

    pub fn as_managed_path(&self) -> &ManagedPath {
        &self.0
    }
}

impl Ord for LoosePath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_path().cmp(other.0.as_path())
    }
}

impl PartialOrd for LoosePath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LooseRow {
    bytes: Vec<u8>,
    hash: RowHash,
}
impl LooseRow {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > 8 * 1024 * 1024 {
            return Err(Error::InvalidRow);
        }
        let hash = RowHash::of(&bytes);
        Ok(Self { bytes, hash })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub const fn hash(&self) -> RowHash {
        self.hash
    }
}

/// One final, already-folded runtime mutation.  `expected` is the captured
/// loose projection state and prevents an editor's later change being replaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LooseMutation {
    pub id: MutationId,
    pub path: LoosePath,
    pub expected: Option<RowHash>,
    pub next: Option<LooseRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenBatch {
    pub id: MutationId,
    pub mutations: Vec<LooseMutation>,
    pub watermark: u64,
}

impl FrozenBatch {
    /// Enforces the complete staging gate before any materialisation can begin.
    pub fn new(
        id: MutationId,
        mutations: Vec<LooseMutation>,
        watermark: u64,
    ) -> Result<Self, Error> {
        if mutations.is_empty() || watermark == 0 {
            return Err(Error::IncompleteStaging);
        }
        let mut paths = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for mutation in &mutations {
            if mutation.id == id
                || !paths.insert(mutation.path.clone())
                || !ids.insert(mutation.id.clone())
            {
                return Err(Error::IncompleteStaging);
            }
            if mutation.next.is_none() && mutation.expected.is_none() {
                return Err(Error::IncompleteStaging);
            }
        }
        Ok(Self {
            id,
            mutations,
            watermark,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LooseProjection {
    rows: BTreeMap<LoosePath, LooseRow>,
    applied: BTreeMap<MutationId, LooseMutation>,
}

impl LooseProjection {
    pub fn row(&self, path: &LoosePath) -> Option<&LooseRow> {
        self.rows.get(path)
    }
    pub fn contains_applied(&self, id: &MutationId) -> bool {
        self.applied.contains_key(id)
    }

    /// Applies all of the frozen batch or none of it. A repeated, identical
    /// application is a no-op; a reused id with differing intent fails closed.
    pub fn project(&mut self, batch: &FrozenBatch) -> Result<(), Error> {
        let mut candidate = self.clone();
        for mutation in &batch.mutations {
            candidate.apply(mutation)?;
        }
        validate_portable_paths(candidate.rows.keys())?;
        *self = candidate;
        Ok(())
    }

    fn apply(&mut self, mutation: &LooseMutation) -> Result<(), Error> {
        if let Some(previous) = self.applied.get(&mutation.id) {
            return if previous == mutation {
                Ok(())
            } else {
                Err(Error::MutationReplayConflict {
                    id: mutation.id.clone(),
                })
            };
        }
        let actual = self.rows.get(&mutation.path).map(LooseRow::hash);
        if actual != mutation.expected {
            return Err(Error::ExternalConflict {
                path: mutation.path.clone(),
                expected: mutation.expected,
                actual,
            });
        }
        match &mutation.next {
            Some(row) => {
                self.rows.insert(mutation.path.clone(), row.clone());
            }
            None => {
                self.rows.remove(&mutation.path);
            }
        }
        self.applied.insert(mutation.id.clone(), mutation.clone());
        Ok(())
    }
}

/// The result of materialising one loose row. This operation intentionally
/// stops at the worktree projection boundary; it does not stage an index,
/// advance a ref, or claim PUB-1 completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Materialization {
    Applied,
    AlreadyApplied,
}

/// Materialises one validated loose mutation through the repository boundary.
///
/// A matching next value is idempotent. Any other unexpected current bytes are
/// reported as an external conflict and are never overwritten.
pub fn materialize_loose_mutation(
    repository: &Repository,
    mutation: &LooseMutation,
) -> Result<Materialization, Error> {
    let current = repository
        .managed_file_bytes(mutation.path.as_managed_path())
        .map_err(|_| Error::RepositoryUnavailable)?;
    let actual = current.as_deref().map(RowHash::of);
    let desired = mutation.next.as_ref().map(LooseRow::hash);
    if actual == desired {
        return Ok(Materialization::AlreadyApplied);
    }
    if actual != mutation.expected {
        return Err(Error::ExternalConflict {
            path: mutation.path.clone(),
            expected: mutation.expected,
            actual,
        });
    }
    match repository.materialize_managed_file(
        mutation.path.as_managed_path(),
        current.as_deref(),
        mutation.next.as_ref().map(LooseRow::bytes),
    ) {
        Ok(()) => Ok(Materialization::Applied),
        Err(RepositoryError::ManagedContentConflict) => {
            let actual = repository
                .managed_file_bytes(mutation.path.as_managed_path())
                .map_err(|_| Error::RepositoryUnavailable)?
                .as_deref()
                .map(RowHash::of);
            Err(Error::ExternalConflict {
                path: mutation.path.clone(),
                expected: mutation.expected,
                actual,
            })
        }
        Err(_) => Err(Error::RepositoryUnavailable),
    }
}

/// Builds the real Git candidate for a frozen batch without advancing a ref.
/// The runtime batch remains authoritative until the caller journals, performs
/// index/worktree reconciliation, and completes the publication boundary.
pub fn build_private_publication_candidate(
    repository: &Repository,
    expected_head: &GitCommitRef,
    batch: &FrozenBatch,
    message: &str,
) -> Result<PrivateCommit, Error> {
    validate_portable_paths(
        batch
            .mutations
            .iter()
            .filter_map(|mutation| mutation.next.as_ref().map(|_| &mutation.path)),
    )?;
    let changes = batch
        .mutations
        .iter()
        .map(|mutation| {
            ManagedFileChange::new(
                mutation.path.as_managed_path().clone(),
                mutation.next.as_ref().map(|row| row.bytes().to_vec()),
            )
        })
        .collect::<Vec<_>>();
    repository
        .build_private_commit(expected_head, &changes, message)
        .map_err(|_| Error::RepositoryUnavailable)
}

/// Converts the runtime's validated typed table prefix into the loose-row
/// representation. Key decoding remains schema-owned: this function refuses
/// to invent path components for an opaque runtime key.
pub fn lower_runtime_table_mutations(
    freeze: &PublicationFreeze,
    mutations: &[TableMutation],
    path_for: impl Fn(&TableMutation) -> Result<LoosePath, Error>,
    expected_bytes_for: impl Fn(&LoosePath) -> Result<Option<Vec<u8>>, Error>,
) -> Result<FrozenBatch, Error> {
    if mutations.is_empty() {
        return Err(Error::IncompleteStaging);
    }
    let batch_id = MutationId::new(hex_id(freeze.intent_id))?;
    let mut lowered = Vec::with_capacity(mutations.len());
    for mutation in mutations {
        let path = path_for(mutation)?;
        let expected_bytes = expected_bytes_for(&path)?;
        let expected = expected_bytes.as_deref().map(RowHash::of);
        let next = mutation
            .value()
            .map(|bytes| LooseRow::new(bytes.to_vec()))
            .transpose()?;
        lowered.push(LooseMutation {
            id: MutationId::new(hex_id(mutation.id()))?,
            path,
            expected,
            next,
        });
    }
    FrozenBatch::new(batch_id, lowered, freeze.checkpoint.mutation_sequence)
}

/// A prepared, runtime-bound publication. Preparation creates only private
/// Git objects and a durable journal; `publish` and `complete` are separate so
/// a crash can be recovered at each PUB-1 boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePublicationCoordinator {
    expected_index: IndexGeneration,
    candidate: PrivateCommit,
    journal: PublicationJournal,
}

impl RuntimePublicationCoordinator {
    /// Reads the exact typed prefix named by `freeze`, then prepares its
    /// canonical loose-row candidate. The resolver is the schema boundary for
    /// turning a typed table key into representable path components.
    pub async fn prepare_from_runtime(
        repository: &Repository,
        runtime: &RuntimeState,
        expected_head: &GitCommitRef,
        expected_index: IndexGeneration,
        freeze: &PublicationFreeze,
        path_for: impl Fn(&TableMutation) -> Result<LoosePath, Error>,
        message: &str,
    ) -> Result<Self, Error> {
        let mutations = runtime
            .pending_table_mutations_through(freeze)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        let batch = lower_runtime_table_mutations(freeze, &mutations, path_for, |path| {
            repository
                .managed_file_bytes(path.as_managed_path())
                .map_err(|_| Error::RepositoryUnavailable)
        })?;
        Self::prepare(
            repository,
            expected_head,
            expected_index,
            freeze,
            batch,
            message,
        )
    }

    /// Prepares a candidate from a validated runtime freeze and already
    /// lowered loose mutations. The journal captures the runtime intent and
    /// exact worktree expectations before any visible Git change.
    pub fn prepare(
        repository: &Repository,
        expected_head: &GitCommitRef,
        expected_index: IndexGeneration,
        freeze: &PublicationFreeze,
        batch: FrozenBatch,
        message: &str,
    ) -> Result<Self, Error> {
        if batch.watermark != freeze.checkpoint.mutation_sequence {
            return Err(Error::IncompleteStaging);
        }
        let candidate =
            build_private_publication_candidate(repository, expected_head, &batch, message)?;
        let entries = batch
            .mutations
            .iter()
            .map(|mutation| {
                let expected = repository
                    .managed_file_bytes(mutation.path.as_managed_path())
                    .map_err(|_| Error::RepositoryUnavailable)?;
                let actual = expected.as_deref().map(RowHash::of);
                if actual != mutation.expected {
                    return Err(Error::ExternalConflict {
                        path: mutation.path.clone(),
                        expected: mutation.expected,
                        actual,
                    });
                }
                Ok(PublicationJournalEntry::new(
                    mutation.path.as_managed_path().clone(),
                    expected,
                    mutation.next.as_ref().map(|row| row.bytes().to_vec()),
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let base_index_tree = expected_index
            .tree()
            .cloned()
            .ok_or(Error::IncompleteStaging)?;
        let journal = PublicationJournal::new_with_runtime_intent(
            expected_head.clone(),
            candidate.commit().clone(),
            base_index_tree,
            freeze.intent_id,
            entries,
        )
        .map_err(|_| Error::RepositoryUnavailable)?;
        Ok(Self {
            expected_index,
            candidate,
            journal,
        })
    }

    pub fn candidate(&self) -> &PrivateCommit {
        &self.candidate
    }

    pub fn journal(&self) -> &PublicationJournal {
        &self.journal
    }

    /// Executes repository ref, index, and worktree boundaries. The journal
    /// intentionally remains at `WorktreeReconciled` after this returns.
    pub fn publish(&mut self, repository: &Repository) -> Result<IndexGeneration, Error> {
        repository
            .publish_candidate(&self.expected_index, &self.candidate, &mut self.journal)
            .map_err(map_publication_repository_error)
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
        let commit = PublicationCommitId::new(self.candidate.commit().as_str().as_bytes().to_vec())
            .map_err(|_| Error::InvalidObjectId)?;
        runtime
            .complete_publication(freeze, &commit)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        repository
            .mark_runtime_complete(freeze.intent_id, &mut self.journal)
            .map_err(|_| Error::RepositoryUnavailable)
    }

    /// Recovers a persisted runtime publication after a process interruption.
    /// Repository reconciliation is completed before the runtime prefix is
    /// consumed; every mismatch leaves the journal and pending tail intact.
    pub async fn recover(
        repository: &Repository,
        runtime: &RuntimeState,
    ) -> Result<Option<IndexGeneration>, Error> {
        let Some(journal) = repository
            .read_publication_journal()
            .map_err(map_publication_repository_error)?
        else {
            return Ok(None);
        };
        let intent_id = journal
            .runtime_intent_id()
            .ok_or(Error::InvalidTransition)?;
        let freeze = runtime
            .publication_freeze(intent_id)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        let commit = PublicationCommitId::new(journal.new_head().as_str().as_bytes().to_vec())
            .map_err(|_| Error::InvalidObjectId)?;

        if matches!(
            journal.stage(),
            orna_repository_v1::PublicationJournalStage::RuntimeCompleted
                | orna_repository_v1::PublicationJournalStage::Complete
        ) {
            runtime
                .complete_publication(&freeze, &commit)
                .await
                .map_err(|_| Error::RuntimeUnavailable)?;
            return repository
                .recover_publication()
                .map_err(|_| Error::RepositoryUnavailable);
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
        if journal.stage() != orna_repository_v1::PublicationJournalStage::WorktreeReconciled
            || journal.runtime_intent_id() != Some(intent_id)
        {
            return Err(Error::InvalidTransition);
        }
        runtime
            .complete_publication(&freeze, &commit)
            .await
            .map_err(|_| Error::RuntimeUnavailable)?;
        repository
            .mark_runtime_complete(intent_id, &mut journal)
            .map_err(|_| Error::RepositoryUnavailable)?;
        repository
            .index_generation()
            .map(Some)
            .map_err(|_| Error::RepositoryUnavailable)
    }
}

fn map_publication_repository_error(error: RepositoryError) -> Error {
    match error {
        RepositoryError::InvalidPublicationJournal => Error::InvalidPublicationJournal,
        RepositoryError::StaleIndex { .. } => Error::RecoveryIndexConflict,
        RepositoryError::StaleHead => Error::RefConflict,
        RepositoryError::ManagedContentConflict => Error::ManagedWorktreeConflict,
        _ => Error::RepositoryUnavailable,
    }
}

fn hex_id(id: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(32);
    for byte in id {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

fn validate_portable_paths<'a>(
    paths: impl IntoIterator<Item = &'a LoosePath>,
) -> Result<(), Error> {
    let mut siblings = BTreeMap::new();
    for path in paths {
        let mut prefix = Vec::new();
        for (index, component) in path.0.as_path().iter().enumerate() {
            let component = component.to_str().ok_or(Error::UnsafePath)?;
            // Table roots are supplied by the schema adapter; collisions here
            // concern encoded key siblings within the same table.
            prefix.push(if index == 0 {
                component.to_owned()
            } else {
                component.to_ascii_lowercase()
            });
            if let Some(previous) = siblings.insert(prefix.clone(), component)
                && previous != component
            {
                return Err(Error::PathCollision);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefName(String);
impl RefName {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty() || !value.starts_with("refs/") {
            Err(Error::InvalidRef)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitObjectId(String);
impl GitObjectId {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.len() < 4 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Err(Error::InvalidObjectId)
        } else {
            Ok(Self(value))
        }
    }
}

/// A byte-exact, simplified ordinary index image. Its map representation makes
/// the carry-forward rule observable without claiming to implement Git's file format.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexImage(BTreeMap<LoosePath, Option<LooseRow>>);
impl IndexImage {
    pub fn entry(&self, path: LoosePath, row: Option<LooseRow>) -> Self {
        let mut next = self.clone();
        next.0.insert(path, row);
        next
    }
    pub fn get(&self, path: &LoosePath) -> Option<&Option<LooseRow>> {
        self.0.get(path)
    }
}

/// Carry the staged difference from H to the private candidate N. Any staged
/// managed-path difference is an overlap and must have been rejected at capture.
pub fn reconcile_index(
    base: &IndexImage,
    ordinary: &IndexImage,
    publication: &FrozenBatch,
) -> Result<IndexImage, Error> {
    let mut result = ordinary.clone();
    for mutation in &publication.mutations {
        if ordinary.get(&mutation.path) != base.get(&mutation.path) {
            return Err(Error::IndexConflict {
                path: mutation.path.clone(),
            });
        }
        result
            .0
            .insert(mutation.path.clone(), mutation.next.clone());
    }
    validate_portable_paths(
        result
            .0
            .iter()
            .filter_map(|(path, row)| row.as_ref().map(|_| path)),
    )?;
    Ok(result)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PubJournal {
    pub batch: FrozenBatch,
    pub target: RefName,
    pub old: GitObjectId,
    pub new: GitObjectId,
    pub base_index: IndexImage,
    pub reconciled_index: IndexImage,
    pub stage: JournalStage,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalStage {
    Journaled,
    RefAdvanced,
    IndexReconciled,
    Cleaned,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    journal: PubJournal,
}
impl Publication {
    pub fn prepare(
        batch: FrozenBatch,
        target: RefName,
        old: GitObjectId,
        new: GitObjectId,
        base: IndexImage,
        ordinary: IndexImage,
    ) -> Result<Self, Error> {
        let reconciled_index = reconcile_index(&base, &ordinary, &batch)?;
        Ok(Self {
            journal: PubJournal {
                batch,
                target,
                old,
                new,
                base_index: ordinary,
                reconciled_index,
                stage: JournalStage::Journaled,
            },
        })
    }
    pub fn journal(&self) -> &PubJournal {
        &self.journal
    }
    pub fn advance_ref(&mut self, observed: &GitObjectId) -> Result<(), Error> {
        if observed != &self.journal.old {
            return Err(Error::RefConflict);
        }
        self.journal.stage = JournalStage::RefAdvanced;
        Ok(())
    }
    pub fn install_index(&mut self, observed: &IndexImage) -> Result<(), Error> {
        if self.journal.stage != JournalStage::RefAdvanced {
            return Err(Error::InvalidTransition);
        }
        if observed != &self.journal.base_index {
            return Err(Error::RecoveryIndexConflict);
        }
        self.journal.stage = JournalStage::IndexReconciled;
        Ok(())
    }
    pub fn cleanup(&mut self) -> Result<(), Error> {
        if self.journal.stage != JournalStage::IndexReconciled {
            return Err(Error::InvalidTransition);
        }
        self.journal.stage = JournalStage::Cleaned;
        Ok(())
    }
    pub fn complete(&mut self) -> Result<(), Error> {
        if self.journal.stage != JournalStage::Cleaned {
            return Err(Error::InvalidTransition);
        }
        self.journal.stage = JournalStage::Complete;
        Ok(())
    }

    /// Computes the only safe recovery action; Git/filesystem effects remain
    /// unsupported adapter responsibilities.
    pub fn recover(
        &mut self,
        observed_ref: &GitObjectId,
        observed_index: &IndexImage,
    ) -> Result<Recovery, Error> {
        if observed_ref == &self.journal.old {
            if observed_index != &self.journal.base_index {
                return Err(Error::RecoveryIndexConflict);
            }
            self.journal.stage = JournalStage::Journaled;
            return Ok(Recovery::KeepPending);
        }
        if observed_ref != &self.journal.new {
            return Err(Error::RefConflict);
        }
        if observed_index == &self.journal.base_index {
            // The durable journal can prove that reconciliation is still
            // required, but it cannot claim the index was installed before
            // the adapter performs that action.
            self.journal.stage = JournalStage::RefAdvanced;
            return Ok(Recovery::InstallIndex(
                self.journal.reconciled_index.clone(),
            ));
        }
        if observed_index == &self.journal.reconciled_index {
            self.journal.stage = JournalStage::Cleaned;
            return Ok(Recovery::FinishCleanup);
        }
        Err(Error::RecoveryIndexConflict)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Recovery {
    KeepPending,
    InstallIndex(IndexImage),
    FinishCleanup,
}

/// Explicitly unsupported I/O boundary. Consumers may provide an adapter only
/// after separately meeting PUB-1's locking, flushing and Git CAS obligations.
pub trait PublicationProvider {
    type Error: std::error::Error + Send + Sync + 'static;
    fn unsupported_git_io(&self) -> Result<(), Self::Error>;
}

/// A compile-time bridge to the existing v1 repository/runtime direction-only
/// contract, without assuming a provider can perform publication I/O.
pub fn capture_runtime<A: RepositoryGenerationAdapter>(
    adapter: &A,
) -> Result<(RepositoryIdentity, CwdCapture), Error> {
    let identity = adapter
        .require_cwd()
        .map_err(|_| Error::RuntimeUnavailable)?;
    let capture = adapter
        .capture_cwd(identity)
        .map_err(|_| Error::RuntimeUnavailable)?;
    Ok((identity, capture))
}

/// Binds a journal entry to the existing runtime's durable freeze identity.
/// The caller retains responsibility for reading and consuming the frozen range.
pub const fn runtime_freeze_id(freeze: &orna_runtime_v1::PublicationFreeze) -> [u8; 16] {
    freeze.intent_id
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidMutationId,
    InvalidTableRoot,
    InvalidKey,
    UnsafePath,
    InvalidRow,
    PathCollision,
    IncompleteStaging,
    InvalidRef,
    InvalidObjectId,
    MutationReplayConflict {
        id: MutationId,
    },
    ExternalConflict {
        path: LoosePath,
        expected: Option<RowHash>,
        actual: Option<RowHash>,
    },
    IndexConflict {
        path: LoosePath,
    },
    RecoveryIndexConflict,
    RefConflict,
    ManagedWorktreeConflict,
    PublicationPending,
    InvalidPublicationJournal,
    InvalidTransition,
    RuntimeUnavailable,
    RepositoryUnavailable,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidMutationId => "invalid mutation id",
            Self::InvalidTableRoot => "invalid table root",
            Self::InvalidKey => "invalid loose row key",
            Self::UnsafePath => "unsafe loose row path",
            Self::InvalidRow => "invalid loose row",
            Self::PathCollision => "portable loose-row path collision",
            Self::IncompleteStaging => "incomplete publication staging",
            Self::InvalidRef => "invalid ref",
            Self::InvalidObjectId => "invalid object id",
            Self::MutationReplayConflict { .. } => "mutation replay conflicts",
            Self::ExternalConflict { .. } => "external loose-row conflict",
            Self::IndexConflict { .. } => "managed path conflicts with ordinary index",
            Self::RecoveryIndexConflict => "recovery index conflict",
            Self::RefConflict => "publication ref conflict",
            Self::ManagedWorktreeConflict => "managed worktree content conflict",
            Self::PublicationPending => "publication remains pending before ref advance",
            Self::InvalidPublicationJournal => "invalid publication journal",
            Self::InvalidTransition => "invalid publication transition",
            Self::RuntimeUnavailable => "runtime contract unavailable",
            Self::RepositoryUnavailable => "repository projection is unavailable",
        })
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
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
        git(
            temp.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(temp.path(), &["config", "user.name", "Storage test"]);
        fs::write(temp.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(temp.path().join("ordinary.txt"), "base\n").unwrap();
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(temp.path()).unwrap();
        (temp, repository)
    }

    fn path() -> LoosePath {
        LoosePath::for_key("tables", &["sensor".into(), "one".into()]).unwrap()
    }
    fn row(text: &str) -> LooseRow {
        LooseRow::new(text.as_bytes().to_vec()).unwrap()
    }

    fn prepared_publication_plan(repository: &Repository) -> RuntimePublicationCoordinator {
        let freeze = PublicationFreeze {
            intent_id: [41; 16],
            checkpoint: orna_runtime_v1::Checkpoint {
                generation: 1,
                digest: [42; 32],
                mutation_sequence: 1,
            },
        };
        let mutation = LooseMutation {
            id: MutationId::new("publication-conflict").unwrap(),
            path: LoosePath::for_key("Contact", &["Alice".into()]).unwrap(),
            expected: None,
            next: Some(row("published")),
        };
        let batch = FrozenBatch::new(
            MutationId::new("publication-conflict-batch").unwrap(),
            vec![mutation],
            freeze.checkpoint.mutation_sequence,
        )
        .unwrap();
        RuntimePublicationCoordinator::prepare(
            repository,
            &repository.head().unwrap().unwrap(),
            repository.index_generation().unwrap(),
            &freeze,
            batch,
            "orna: publish runtime data",
        )
        .unwrap()
    }

    #[test]
    fn runtime_table_prefix_lowers_into_a_bound_publication() {
        let (_temp, repository) = repository();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let freeze = PublicationFreeze {
            intent_id: [7; 16],
            checkpoint: orna_runtime_v1::Checkpoint {
                generation: 1,
                digest: [8; 32],
                mutation_sequence: 1,
            },
        };
        let mutation =
            TableMutation::new([9; 16], "Contact", b"Alice".to_vec(), Some(b"row".to_vec()))
                .unwrap();
        let batch = lower_runtime_table_mutations(
            &freeze,
            std::slice::from_ref(&mutation),
            |mutation| {
                LoosePath::for_key(
                    mutation.table(),
                    &[String::from_utf8(mutation.key().to_vec()).unwrap()],
                )
            },
            |_path| Ok(None),
        )
        .unwrap();
        let plan = RuntimePublicationCoordinator::prepare(
            &repository,
            &head,
            index,
            &freeze,
            batch,
            "orna: publish runtime data",
        )
        .unwrap();
        assert_ne!(plan.candidate().commit(), &head);
        assert_eq!(plan.journal().runtime_intent_id(), Some([7; 16]));
        assert_eq!(plan.journal().entries().len(), 1);
    }

    #[tokio::test]
    async fn coordinator_publishes_and_completes_a_real_runtime_prefix() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime_v1::RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            [3; 32],
        )
        .await
        .unwrap();
        let lease = runtime.acquire_lease([4; 16]).await.unwrap();
        let context = runtime.begin_activation().await.unwrap();
        let mutation =
            TableMutation::new([5; 16], "Contact", b"Alice".to_vec(), Some(b"row".to_vec()))
                .unwrap();
        runtime
            .commit_table_activation(
                lease,
                &context,
                &[mutation],
                [6; 32],
                &orna_runtime_v1::NoFault,
            )
            .await
            .unwrap();
        let freeze = runtime
            .freeze(
                [7; 16],
                &orna_runtime_v1::Checkpoint {
                    generation: 1,
                    digest: [6; 32],
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let mut plan = RuntimePublicationCoordinator::prepare_from_runtime(
            &repository,
            &runtime,
            &head,
            index,
            &freeze,
            |mutation| {
                LoosePath::for_key(
                    mutation.table(),
                    &[String::from_utf8(mutation.key().to_vec()).unwrap()],
                )
            },
            "orna: publish runtime data",
        )
        .await
        .unwrap();
        plan.publish(&repository).unwrap();
        plan.complete(&repository, &runtime, &freeze).await.unwrap();
        assert!(runtime.pending().await.unwrap().is_empty());
        assert_eq!(repository.read_publication_journal().unwrap(), None);
        let managed = LoosePath::for_key("Contact", &["Alice".into()]).unwrap();
        assert_eq!(
            repository
                .managed_file_bytes(managed.as_managed_path())
                .unwrap(),
            Some(b"row".to_vec())
        );
    }

    #[test]
    fn coordinator_preserves_changed_index_conflict() {
        let (temp, repository) = repository();
        let mut plan = prepared_publication_plan(&repository);
        fs::write(temp.path().join("ordinary.txt"), "changed\n").unwrap();
        git(temp.path(), &["add", "ordinary.txt"]);

        assert!(matches!(
            plan.publish(&repository),
            Err(Error::RecoveryIndexConflict)
        ));
    }

    #[test]
    fn coordinator_preserves_changed_managed_worktree_conflict() {
        let (temp, repository) = repository();
        let mut plan = prepared_publication_plan(&repository);
        let managed = temp.path().join("Contact").join("Alice.orna");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(managed, "edited\n").unwrap();

        assert!(matches!(
            plan.publish(&repository),
            Err(Error::ManagedWorktreeConflict)
        ));
    }

    #[test]
    fn coordinator_preserves_changed_ref_conflict() {
        assert!(matches!(
            map_publication_repository_error(RepositoryError::StaleHead),
            Error::RefConflict
        ));
    }

    #[tokio::test]
    async fn coordinator_recovery_reports_pre_ref_publication_as_pending() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime_v1::RuntimeIdentity {
                database_id: [31; 16],
                repository_id: [32; 16],
            },
            [33; 32],
        )
        .await
        .unwrap();
        let lease = runtime.acquire_lease([34; 16]).await.unwrap();
        let context = runtime.begin_activation().await.unwrap();
        runtime
            .commit_table_activation(
                lease,
                &context,
                &[TableMutation::new(
                    [35; 16],
                    "Contact",
                    b"Carol".to_vec(),
                    Some(b"row".to_vec()),
                )
                .unwrap()],
                [36; 32],
                &orna_runtime_v1::NoFault,
            )
            .await
            .unwrap();
        let freeze = runtime
            .freeze(
                [37; 16],
                &orna_runtime_v1::Checkpoint {
                    generation: 1,
                    digest: [36; 32],
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let plan = RuntimePublicationCoordinator::prepare_from_runtime(
            &repository,
            &runtime,
            &head,
            index.clone(),
            &freeze,
            |mutation| {
                LoosePath::for_key(
                    mutation.table(),
                    &[String::from_utf8(mutation.key().to_vec()).unwrap()],
                )
            },
            "orna: publish runtime data",
        )
        .await
        .unwrap();
        repository
            .write_publication_journal(plan.journal())
            .unwrap();
        let pending = runtime.pending().await.unwrap();

        assert!(matches!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::PublicationPending)
        ));
        assert_eq!(repository.head().unwrap(), Some(head));
        assert_eq!(repository.index_generation().unwrap(), index);
        assert_eq!(runtime.pending().await.unwrap(), pending);
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            orna_repository_v1::PublicationJournalStage::Prepared
        );
    }

    #[tokio::test]
    async fn coordinator_recovery_preserves_a_candidate_byte_binding_failure() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime_v1::RuntimeIdentity {
                database_id: [51; 16],
                repository_id: [52; 16],
            },
            [53; 32],
        )
        .await
        .unwrap();
        let lease = runtime.acquire_lease([54; 16]).await.unwrap();
        let context = runtime.begin_activation().await.unwrap();
        runtime
            .commit_table_activation(
                lease,
                &context,
                &[TableMutation::new(
                    [55; 16],
                    "Contact",
                    b"Alice".to_vec(),
                    Some(b"candidate row".to_vec()),
                )
                .unwrap()],
                [56; 32],
                &orna_runtime_v1::NoFault,
            )
            .await
            .unwrap();
        let freeze = runtime
            .freeze(
                [57; 16],
                &orna_runtime_v1::Checkpoint {
                    generation: 1,
                    digest: [56; 32],
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let plan = RuntimePublicationCoordinator::prepare_from_runtime(
            &repository,
            &runtime,
            &head,
            index.clone(),
            &freeze,
            |mutation| {
                LoosePath::for_key(
                    mutation.table(),
                    &[String::from_utf8(mutation.key().to_vec()).unwrap()],
                )
            },
            "orna: publish runtime data",
        )
        .await
        .unwrap();
        let path = LoosePath::for_key("Contact", &["Alice".into()]).unwrap();
        let journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            plan.candidate().commit().clone(),
            index.tree().unwrap().clone(),
            freeze.intent_id,
            vec![PublicationJournalEntry::new(
                path.as_managed_path().clone(),
                None,
                Some(b"journal row".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&head, plan.candidate())
            .unwrap();

        assert!(matches!(
            RuntimePublicationCoordinator::recover(&repository, &runtime).await,
            Err(Error::InvalidPublicationJournal)
        ));
        assert_eq!(
            repository.head().unwrap(),
            Some(plan.candidate().commit().clone())
        );
        assert_eq!(repository.index_generation().unwrap().tree(), index.tree());
        assert_eq!(runtime.pending().await.unwrap().len(), 1);
        assert_eq!(
            repository.read_publication_journal().unwrap(),
            Some(journal)
        );
    }

    #[tokio::test]
    async fn coordinator_recovery_preserves_a_malformed_journal_read() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime_v1::RuntimeIdentity {
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
    async fn coordinator_recovers_after_repository_publication_before_runtime_completion() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime_v1::RuntimeIdentity {
                database_id: [11; 16],
                repository_id: [12; 16],
            },
            [13; 32],
        )
        .await
        .unwrap();
        let lease = runtime.acquire_lease([14; 16]).await.unwrap();
        let context = runtime.begin_activation().await.unwrap();
        runtime
            .commit_table_activation(
                lease,
                &context,
                &[TableMutation::new(
                    [15; 16],
                    "Contact",
                    b"Alice".to_vec(),
                    Some(b"row".to_vec()),
                )
                .unwrap()],
                [16; 32],
                &orna_runtime_v1::NoFault,
            )
            .await
            .unwrap();
        let freeze = runtime
            .freeze(
                [17; 16],
                &orna_runtime_v1::Checkpoint {
                    generation: 1,
                    digest: [16; 32],
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let mut plan = RuntimePublicationCoordinator::prepare_from_runtime(
            &repository,
            &runtime,
            &head,
            index,
            &freeze,
            |mutation| {
                LoosePath::for_key(
                    mutation.table(),
                    &[String::from_utf8(mutation.key().to_vec()).unwrap()],
                )
            },
            "orna: publish runtime data",
        )
        .await
        .unwrap();
        plan.publish(&repository).unwrap();
        drop(plan);

        assert!(!runtime.pending().await.unwrap().is_empty());
        assert!(
            RuntimePublicationCoordinator::recover(&repository, &runtime)
                .await
                .unwrap()
                .is_some()
        );
        assert!(runtime.pending().await.unwrap().is_empty());
        assert_eq!(repository.read_publication_journal().unwrap(), None);
        let managed = LoosePath::for_key("Contact", &["Alice".into()]).unwrap();
        assert_eq!(
            repository
                .managed_file_bytes(managed.as_managed_path())
                .unwrap(),
            Some(b"row".to_vec())
        );
    }

    #[tokio::test]
    async fn coordinator_recovery_is_idempotent_after_runtime_completion() {
        let (_temp, repository) = repository();
        let runtime = RuntimeState::open(
            &repository,
            orna_runtime_v1::RuntimeIdentity {
                database_id: [21; 16],
                repository_id: [22; 16],
            },
            [23; 32],
        )
        .await
        .unwrap();
        let lease = runtime.acquire_lease([24; 16]).await.unwrap();
        let context = runtime.begin_activation().await.unwrap();
        runtime
            .commit_table_activation(
                lease,
                &context,
                &[
                    TableMutation::new([25; 16], "Contact", b"Bob".to_vec(), Some(b"row".to_vec()))
                        .unwrap(),
                ],
                [26; 32],
                &orna_runtime_v1::NoFault,
            )
            .await
            .unwrap();
        let freeze = runtime
            .freeze(
                [27; 16],
                &orna_runtime_v1::Checkpoint {
                    generation: 1,
                    digest: [26; 32],
                    mutation_sequence: 1,
                },
            )
            .await
            .unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let mut plan = RuntimePublicationCoordinator::prepare_from_runtime(
            &repository,
            &runtime,
            &head,
            index,
            &freeze,
            |mutation| {
                LoosePath::for_key(
                    mutation.table(),
                    &[String::from_utf8(mutation.key().to_vec()).unwrap()],
                )
            },
            "orna: publish runtime data",
        )
        .await
        .unwrap();
        plan.publish(&repository).unwrap();
        let commit =
            PublicationCommitId::new(plan.candidate().commit().as_str().as_bytes().to_vec())
                .unwrap();
        runtime
            .complete_publication(&freeze, &commit)
            .await
            .unwrap();
        drop(plan);

        RuntimePublicationCoordinator::recover(&repository, &runtime)
            .await
            .unwrap()
            .unwrap();
        assert!(runtime.pending().await.unwrap().is_empty());
        assert_eq!(repository.read_publication_journal().unwrap(), None);
    }

    #[test]
    fn logical_keys_are_encoded_before_path_safety_validation() {
        for (key, encoded) in [
            ("", "~ff"),
            (".", "~2e"),
            ("..", "~2e~2e"),
            ("foo/bar", "foo~2fbar"),
            ("foo\\bar", "foo~5cbar"),
            ("/outside", "~2foutside"),
            (".git", "~2egit"),
            ("con", "~63on"),
            ("é", "~c3~a9"),
        ] {
            let path = LoosePath::for_key("Contact", &[key.into()]).unwrap();
            assert_eq!(
                path.as_managed_path().as_path(),
                std::path::Path::new(&format!("Contact/{encoded}.orna"))
            );
            assert_eq!(orna_value_v1::path_decode_component(encoded).unwrap(), key);
        }
        let composite = LoosePath::for_key("Contact", &["..".into(), "a/b".into()]).unwrap();
        assert_eq!(
            composite.as_managed_path().as_path(),
            std::path::Path::new("Contact/~2e~2e/a~2fb.orna")
        );
        assert!(LoosePath::for_key("Contact", &[]).is_err());
        assert!(LoosePath::for_key("Contact", &["a".repeat(201)]).is_err());
        assert!(LoosePath::for_key("Contact", &vec!["a".repeat(200); 6]).is_err());
        for root in ["", ".", "..", "../Contact", "/Contact", "a\\b"] {
            assert!(LoosePath::for_key(root, &["safe".into()]).is_err());
        }
    }
    #[test]
    fn discovered_paths_must_be_canonical_before_admission() {
        for (keys, components) in [
            (vec![""], vec!["~ff.orna"]),
            (vec!["..", "foo/bar"], vec!["~2e~2e", "foo~2fbar.orna"]),
            (
                vec!["first.orna", "last.orna"],
                vec!["first.orna", "last.orna.orna"],
            ),
        ] {
            let keys: Vec<String> = keys.into_iter().map(String::from).collect();
            let components: Vec<String> = components.into_iter().map(String::from).collect();
            let discovered = LoosePath::from_encoded_key("Contact", &components).unwrap();
            assert_eq!(discovered, LoosePath::for_key("Contact", &keys).unwrap());
            assert_eq!(
                discovered.as_managed_path().as_path(),
                std::path::Path::new(&format!("Contact/{}", components.join("/")))
            );
        }
        for components in [
            vec![],
            vec!["alice"],
            vec!["~61lice.orna"],
            vec!["~FF.orna"],
            vec!["..", "alice.orna"],
            vec!["/alice.orna"],
            vec!["a\\alice.orna"],
        ] {
            let components: Vec<String> = components.into_iter().map(String::from).collect();
            assert_eq!(
                LoosePath::from_encoded_key("Contact", &components),
                Err(Error::InvalidKey)
            );
        }
        assert!(LoosePath::from_encoded_key("../Contact", &["alice.orna".into()]).is_err());
    }
    #[test]
    fn portable_sibling_collisions_reject_the_entire_candidate() {
        let mut independent = LooseProjection::default();
        for (name, table, keys) in [
            ("one", "Contact", vec!["Alice", "one"]),
            ("two", "Contact", vec!["Alice", "two"]),
            ("other-table", "Other", vec!["alice", "one"]),
        ] {
            let mutation = LooseMutation {
                id: id(name),
                path: LoosePath::for_key(
                    table,
                    &keys.into_iter().map(String::from).collect::<Vec<_>>(),
                )
                .unwrap(),
                expected: None,
                next: Some(row("body")),
            };
            independent
                .project(&FrozenBatch::new(id("independent"), vec![mutation], 1).unwrap())
                .unwrap();
        }
        assert_eq!(independent.rows.len(), 3);
        for (first, second) in [
            (vec!["Alice"], vec!["alice"]),
            (vec!["Alice", "one"], vec!["alice", "two"]),
        ] {
            let key = |parts: Vec<&str>| {
                LoosePath::for_key(
                    "Contact",
                    &parts.into_iter().map(String::from).collect::<Vec<_>>(),
                )
                .unwrap()
            };
            let first = key(first);
            let second = key(second);
            let insert = |name, path| LooseMutation {
                id: id(name),
                path,
                expected: None,
                next: Some(row("body")),
            };
            let a = insert("first", first.clone());
            let b = insert("second", second.clone());
            for mutations in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
                let mut projection = LooseProjection::default();
                let batch = FrozenBatch::new(id("both"), mutations, 1).unwrap();
                assert_eq!(projection.project(&batch), Err(Error::PathCollision));
                assert_eq!(projection, LooseProjection::default());
            }
            let mut projection = LooseProjection::default();
            projection
                .project(&FrozenBatch::new(id("initial"), vec![a], 1).unwrap())
                .unwrap();
            let before = projection.clone();
            assert_eq!(
                projection.project(&FrozenBatch::new(id("conflict"), vec![b.clone()], 2).unwrap()),
                Err(Error::PathCollision)
            );
            assert_eq!(projection, before);
            let delete = LooseMutation {
                id: id("delete"),
                path: first,
                expected: Some(row("body").hash()),
                next: None,
            };
            for mutations in [vec![b.clone(), delete.clone()], vec![delete, b]] {
                let mut replaced = before.clone();
                replaced
                    .project(&FrozenBatch::new(id("replace"), mutations, 3).unwrap())
                    .unwrap();
                assert!(replaced.row(&second).is_some());
                assert_eq!(replaced.rows.len(), 1);
            }
        }
    }
    fn id(text: &str) -> MutationId {
        MutationId::new(text).unwrap()
    }
    fn batch(expected: Option<RowHash>, next: Option<LooseRow>) -> FrozenBatch {
        FrozenBatch::new(
            id("batch"),
            vec![LooseMutation {
                id: id("mutation"),
                path: path(),
                expected,
                next,
            }],
            1,
        )
        .unwrap()
    }
    fn objects() -> (GitObjectId, GitObjectId) {
        (
            GitObjectId::new("aaaa").unwrap(),
            GitObjectId::new("bbbb").unwrap(),
        )
    }
    fn publication(base: IndexImage, ordinary: IndexImage) -> Publication {
        let (old, new) = objects();
        Publication::prepare(
            batch(None, Some(row("published"))),
            RefName::new("refs/heads/main").unwrap(),
            old,
            new,
            base,
            ordinary,
        )
        .unwrap()
    }

    #[test]
    fn publication_validates_portable_paths_in_its_final_candidate() {
        let path = |key: Vec<&str>| {
            LoosePath::for_key(
                "Contact",
                &key.into_iter().map(String::from).collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let mutation = |name, path, expected, next| LooseMutation {
            id: id(name),
            path,
            expected,
            next,
        };
        let prepare = |batch, base, ordinary| {
            let (old, new) = objects();
            Publication::prepare(
                batch,
                RefName::new("refs/heads/main").unwrap(),
                old,
                new,
                base,
                ordinary,
            )
        };

        let upper = path(vec!["Alice"]);
        let lower = path(vec!["alice"]);
        let same_batch = FrozenBatch::new(
            id("same-batch"),
            vec![
                mutation("same-upper", upper.clone(), None, Some(row("upper"))),
                mutation("same-lower", lower.clone(), None, Some(row("lower"))),
            ],
            1,
        )
        .unwrap();
        assert!(matches!(
            prepare(same_batch, IndexImage::default(), IndexImage::default()),
            Err(Error::PathCollision)
        ));

        let base = IndexImage::default().entry(upper.clone(), Some(row("old")));
        let ordinary = base.clone();
        let pre_existing = FrozenBatch::new(
            id("pre-existing"),
            vec![mutation(
                "pre-existing-lower",
                lower.clone(),
                None,
                Some(row("new")),
            )],
            2,
        )
        .unwrap();
        assert!(matches!(
            prepare(pre_existing, base.clone(), ordinary.clone()),
            Err(Error::PathCollision)
        ));
        assert_eq!(base.get(&upper).unwrap().as_ref().unwrap().bytes(), b"old");
        assert_eq!(ordinary, base);

        let nested_upper = path(vec!["Alice", "one"]);
        let nested_lower = path(vec!["alice", "two"]);
        let base = IndexImage::default().entry(nested_upper, Some(row("old")));
        let composite_collision = FrozenBatch::new(
            id("composite"),
            vec![mutation(
                "composite-lower",
                nested_lower,
                None,
                Some(row("new")),
            )],
            3,
        )
        .unwrap();
        assert!(matches!(
            prepare(composite_collision, base.clone(), base),
            Err(Error::PathCollision)
        ));

        let delete = mutation(
            "rename-delete",
            upper.clone(),
            Some(row("old").hash()),
            None,
        );
        let insert = mutation("rename-insert", lower.clone(), None, Some(row("new")));
        for mutations in [
            vec![delete.clone(), insert.clone()],
            vec![insert.clone(), delete.clone()],
        ] {
            let renamed = prepare(
                FrozenBatch::new(id("case-rename"), mutations, 4).unwrap(),
                IndexImage::default().entry(upper.clone(), Some(row("old"))),
                IndexImage::default().entry(upper.clone(), Some(row("old"))),
            )
            .unwrap();
            assert_eq!(renamed.journal().reconciled_index.get(&upper), Some(&None));
            assert_eq!(
                renamed
                    .journal()
                    .reconciled_index
                    .get(&lower)
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .bytes(),
                b"new"
            );
        }
    }

    #[test]
    fn publication_collision_preserves_unrelated_ordinary_staging() {
        let upper = LoosePath::for_key("Contact", &["Alice".into()]).unwrap();
        let lower = LoosePath::for_key("Contact", &["alice".into()]).unwrap();
        let unrelated = LoosePath::for_key("Contact", &["unrelated".into()]).unwrap();
        let base = IndexImage::default().entry(upper.clone(), Some(row("head")));
        let ordinary = base
            .clone()
            .entry(unrelated.clone(), Some(row("staged-only")));
        let before = ordinary.clone();
        let batch = FrozenBatch::new(
            id("collision"),
            vec![LooseMutation {
                id: id("collision-lower"),
                path: lower,
                expected: None,
                next: Some(row("published")),
            }],
            1,
        )
        .unwrap();
        let (old, new) = objects();

        assert!(matches!(
            Publication::prepare(
                batch,
                RefName::new("refs/heads/main").unwrap(),
                old,
                new,
                base,
                ordinary.clone(),
            ),
            Err(Error::PathCollision)
        ));
        assert_eq!(ordinary, before);
        assert_eq!(
            ordinary.get(&unrelated).unwrap().as_ref().unwrap().bytes(),
            b"staged-only"
        );
        assert!(ordinary.get(&upper).unwrap().is_some());
    }

    #[test]
    fn projection_is_idempotent_under_fault_retry() {
        let mut projection = LooseProjection::default();
        let frozen = batch(None, Some(row("one")));
        projection.project(&frozen).unwrap();
        projection.project(&frozen).unwrap();
        assert_eq!(projection.row(&path()).unwrap().bytes(), b"one");
    }
    #[test]
    fn external_edit_is_a_typed_conflict() {
        let mut projection = LooseProjection::default();
        projection
            .project(&batch(None, Some(row("editor"))))
            .unwrap();
        let later = FrozenBatch::new(
            id("batch-two"),
            vec![LooseMutation {
                id: id("mutation-two"),
                path: path(),
                expected: None,
                next: Some(row("runtime")),
            }],
            2,
        )
        .unwrap();
        let error = projection.project(&later).unwrap_err();
        assert!(matches!(error, Error::ExternalConflict { .. }));
    }
    #[test]
    fn complete_staging_gate_rejects_ambiguous_delete() {
        assert!(matches!(
            FrozenBatch::new(
                id("batch"),
                vec![LooseMutation {
                    id: id("mutation"),
                    path: path(),
                    expected: None,
                    next: None
                }],
                1
            ),
            Err(Error::IncompleteStaging)
        ));
    }
    #[test]
    fn recovery_before_ref_advance_keeps_pending() {
        let base = IndexImage::default();
        let mut publication = publication(base.clone(), base.clone());
        let (old, _) = objects();
        assert_eq!(
            publication.recover(&old, &base).unwrap(),
            Recovery::KeepPending
        );
        assert_eq!(publication.journal().stage, JournalStage::Journaled);
    }
    #[test]
    fn recovery_after_ref_advance_reinstalls_reconciled_index() {
        let base = IndexImage::default();
        let mut publication = publication(base.clone(), base.clone());
        let (_, new) = objects();
        let Recovery::InstallIndex(index) = publication.recover(&new, &base).unwrap() else {
            panic!("expected index recovery")
        };
        assert_eq!(publication.journal().stage, JournalStage::RefAdvanced);
        assert!(matches!(
            publication.cleanup(),
            Err(Error::InvalidTransition)
        ));
        assert_eq!(
            index.get(&path()).unwrap().as_ref().unwrap().bytes(),
            b"published"
        );
        publication.install_index(&base).unwrap();
        assert_eq!(publication.journal().stage, JournalStage::IndexReconciled);
    }
    #[test]
    fn index_reconciliation_cannot_precede_ref_advance() {
        let base = IndexImage::default();
        let mut publication = publication(base.clone(), base.clone());
        assert!(matches!(
            publication.install_index(&base),
            Err(Error::InvalidTransition)
        ));
        assert_eq!(publication.journal().stage, JournalStage::Journaled);
    }
    #[test]
    fn reconciliation_preserves_unrelated_partially_staged_entry() {
        let managed = path();
        let other = LoosePath::for_key("tables", &["other".into()]).unwrap();
        let base = IndexImage::default()
            .entry(managed.clone(), None)
            .entry(other.clone(), Some(row("head")));
        let ordinary = base.clone().entry(other.clone(), Some(row("staged-only")));
        let result =
            reconcile_index(&base, &ordinary, &batch(None, Some(row("published")))).unwrap();
        assert_eq!(
            result.get(&other).unwrap().as_ref().unwrap().bytes(),
            b"staged-only"
        );
        assert_eq!(
            result.get(&managed).unwrap().as_ref().unwrap().bytes(),
            b"published"
        );
    }

    #[test]
    fn frozen_batch_builds_a_real_private_candidate_without_advancing_head() {
        let (root, repository) = repository();
        let head = repository.head().unwrap().unwrap();
        let candidate = build_private_publication_candidate(
            &repository,
            &head,
            &batch(None, Some(row("published"))),
            "orna: publish runtime data",
        )
        .unwrap();

        assert_ne!(candidate.commit(), &head);
        assert_eq!(repository.head().unwrap().unwrap(), head);
        assert_eq!(
            git_output(
                root.path(),
                &[
                    "show",
                    &format!("{}:tables/sensor/one.orna", candidate.commit())
                ]
            ),
            "published"
        );
    }

    #[test]
    fn loose_materialization_is_idempotent_and_does_not_publish_git_state() {
        let (root, repository) = repository();
        let path = path();
        let before_index = repository.index_generation().unwrap();
        let before_head = repository.head().unwrap();
        let first = LooseRow::new(b"first".to_vec()).unwrap();
        let mutation = LooseMutation {
            id: id("materialize-one"),
            path: path.clone(),
            expected: None,
            next: Some(first.clone()),
        };

        assert_eq!(
            materialize_loose_mutation(&repository, &mutation).unwrap(),
            Materialization::Applied
        );
        assert_eq!(
            materialize_loose_mutation(&repository, &mutation).unwrap(),
            Materialization::AlreadyApplied
        );
        assert_eq!(
            fs::read(root.path().join(path.as_managed_path().as_path())).unwrap(),
            b"first"
        );
        assert_eq!(repository.index_generation().unwrap(), before_index);
        assert_eq!(repository.head().unwrap(), before_head);
    }

    #[test]
    fn loose_materialization_preserves_external_edits_and_supports_idempotent_delete() {
        let (root, repository) = repository();
        let path = path();
        let first = LooseRow::new(b"first".to_vec()).unwrap();
        let first_mutation = LooseMutation {
            id: id("materialize-two"),
            path: path.clone(),
            expected: None,
            next: Some(first.clone()),
        };
        materialize_loose_mutation(&repository, &first_mutation).unwrap();

        fs::write(
            root.path().join(path.as_managed_path().as_path()),
            b"editor",
        )
        .unwrap();
        let replacement = LooseRow::new(b"runtime".to_vec()).unwrap();
        let conflict = LooseMutation {
            id: id("materialize-three"),
            path: path.clone(),
            expected: Some(first.hash()),
            next: Some(replacement),
        };
        assert!(matches!(
            materialize_loose_mutation(&repository, &conflict),
            Err(Error::ExternalConflict { .. })
        ));
        assert_eq!(
            fs::read(root.path().join(path.as_managed_path().as_path())).unwrap(),
            b"editor"
        );

        fs::write(
            root.path().join(path.as_managed_path().as_path()),
            first.bytes(),
        )
        .unwrap();
        let deletion = LooseMutation {
            id: id("materialize-four"),
            path: path.clone(),
            expected: Some(first.hash()),
            next: None,
        };
        assert_eq!(
            materialize_loose_mutation(&repository, &deletion).unwrap(),
            Materialization::Applied
        );
        assert_eq!(
            materialize_loose_mutation(&repository, &deletion).unwrap(),
            Materialization::AlreadyApplied
        );
        assert!(!root.path().join(path.as_managed_path().as_path()).exists());
    }
}
