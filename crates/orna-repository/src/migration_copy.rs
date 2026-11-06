//! Disposable-copy preparation for an explicit format-1/2 to format-3
//! migration.
//!
//! ACCEPTANCE Gate F requires the migration to run "in a disposable copy": the
//! populated legacy workspace keeps its dirty index/source/submodule work, and
//! the migration is prepared in a separate working directory. MIGRATION.md adds
//! the coordinates such a migration must *report* rather than infer: the
//! derived content-versus-commit tree identity, the annotation default legacy
//! rows take, and the consumer identity map a resumed stream keeps.
//!
//! This module prepares that copy and reports those coordinates from committed
//! objects and caller-declared evidence only. It never decodes a legacy row and
//! never reads a payload byte: the annotation-default coordinate is declined
//! when no decoded legacy inventory exists, because the format-1/2 commits this
//! module can read record no row inventory at all. `prepare_format3_migration`
//! therefore accepts the coordinate as a declared input, and
//! [`LegacyAnnotationInventory`] is where the transitional path derives it once
//! a legacy decoder has supplied the rows.
//!
//! The conversion itself stays a separate step, and the boundary is explicit
//! rather than implied. `prepare_format3_migration` migrates content and binds
//! the migration journal, which is what makes the transition identifiable and
//! resumable. The caller supplies the migration store (the admitted relations,
//! identities, watermarks and retired legacy paths) for the database; the
//! candidate is built from that store, so the candidate commit carries a
//! `.orna/store` root and publication never sees a store-less candidate.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::init::format_context::{
    FORMAT_CONTEXT_MAX_METADATA_BYTES, LEGACY_FORMAT_PATH, RepositoryFormatContext,
    canonical_database_bytes,
};
use crate::native_graph::NativeOid;
use crate::relation_admission::{
    AllocatorWatermarks, RelationAdmissionError, RelationCandidateContent, StagedRelation,
    StoredObjectIdentity,
};
use crate::{
    DatabaseId, GitCommitRef, IndexGeneration, LegacyAnnotationCandidate,
    MigrationAnnotationDefaults, MigrationContinuityRecord, PublicationJournal,
    PublicationJournalEntry, Repository, RepositoryError, RepositoryFormat,
};
use crate::{ManagedFileChange, ManagedPath, PrivateCommit};

/// One legacy `.orna` file the migration candidate deletes: the path and the
/// exact bytes the source commit records there. The candidate is refused unless
/// the source holds those bytes, so a retirement names what it removes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetiredLegacyPath {
    pub path: String,
    pub committed: Vec<u8>,
}

/// The store content a migration candidate admits: the relations and identities
/// of its fresh `.orna/store`, the allocator watermarks, and the legacy `.orna`
/// files it retires. The caller builds it from the source snapshot; the
/// repository admits exactly what it is handed.
#[derive(Clone, Debug, Default)]
pub struct MigrationStore {
    pub relations: Vec<StagedRelation>,
    pub identities: Vec<StoredObjectIdentity>,
    pub watermarks: AllocatorWatermarks,
    pub retired: Vec<RetiredLegacyPath>,
    /// The number of legacy Blob values the store's rows carry. Each takes the
    /// UPGRADE-004 default annotation, so this is the count a migration derives
    /// when the operator declares none, and the count a declared one must equal.
    pub annotation_default_entries: u32,
    /// The check the admitted candidate must pass before the migration is
    /// accepted. `None` accepts without a semantic check; the store builder that
    /// reads the legacy snapshot always installs one.
    pub candidate_gate: Option<CandidateGate>,
}

/// The check a [`CandidateGate`] runs over the admitted migration candidate.
type CandidateCheck = dyn Fn(&Repository, &PrivateCommit) -> Result<(), String>;

/// A check over the admitted migration candidate, supplied with its store. The
/// migration refuses the candidate with the check's message when it fails.
#[derive(Clone)]
pub struct CandidateGate(Arc<CandidateCheck>);

impl CandidateGate {
    /// Wraps `check`, which receives the repository and the admitted candidate.
    pub fn new(
        check: impl Fn(&Repository, &PrivateCommit) -> Result<(), String> + 'static,
    ) -> Self {
        Self(Arc::new(check))
    }

    /// Runs the check over `candidate`, the way the migration does before it
    /// accepts the candidate.
    pub fn check(&self, repository: &Repository, candidate: &PrivateCommit) -> Result<(), String> {
        (self.0)(repository, candidate)
    }
}

impl std::fmt::Debug for CandidateGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CandidateGate")
    }
}

/// Supplies the [`MigrationStore`] of a migration once its database identity is
/// known.
pub type MigrationStoreSource<'a> =
    &'a dyn Fn(&Repository, DatabaseId) -> Result<MigrationStore, RepositoryError>;

/// The one frozen content format an unannotated legacy row transitions to
/// during an explicit migration. UPGRADE-004 fixes this coordinate.
pub const LEGACY_ANNOTATION_DEFAULT_FORMAT: &str = "MIME-1";

/// The Git trailer key a journalled migration commit uses to name its own
/// format coordinate. Git only recognizes a trailer after a blank line, so the
/// candidate message keeps the summary, a blank line, then this key.
pub const MIGRATION_MARKER_TRAILER: &str = "Orna-Migration";

/// The copy-local record of the coordinates a prepared migration carries.
pub const MIGRATION_RECORD: &str = ".orna/migration.json";

/// The filename prefix of the private copy a preparation builds beside its
/// destination before installing it.
const STAGING_PREFIX: &str = ".orna-migration-copy-";

/// The suffix of the owner marker that pairs a staging copy with the lock its
/// preparation holds on it.
const OWNER_SUFFIX: &str = ".owner";

/// The legacy annotation metadata of one source snapshot, as decoded from the
/// source's own recorded objects without reading a payload byte.
///
/// `Present` carries the metadata a decoder recovered from a complete legacy
/// snapshot. `Unavailable` is the state of a format-1/2 commit this module can
/// read: those commits record no row inventory, so the coordinate a migration
/// must report cannot be derived from them. The two states stay distinct so a
/// caller can never read "no row's annotated value identity changed" out of
/// "no row is known".
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyAnnotationInventory {
    Present(Vec<LegacyAnnotationCandidate>),
    Unavailable,
}

impl LegacyAnnotationInventory {
    /// The decoded rows, empty when the inventory is unavailable.
    pub fn rows(&self) -> &[LegacyAnnotationCandidate] {
        match self {
            Self::Present(rows) => rows,
            Self::Unavailable => &[],
        }
    }

    /// Whether the source snapshot actually recorded a row inventory.
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    /// Derives the annotation-default coordinate from the decoded rows.
    ///
    /// An unavailable inventory is refused: reporting a default of zero would
    /// claim that no annotated value identity changed when nothing about the
    /// rows is known.
    pub fn annotation_defaults(&self) -> Result<MigrationAnnotationDefaults, RepositoryError> {
        match self {
            Self::Present(rows) => MigrationAnnotationDefaults::from_legacy_inventory(rows),
            Self::Unavailable => Err(RepositoryError::InvalidFormatMigration),
        }
    }
}

/// One prepared format-3 disposable copy.
///
/// The source repository is untouched: it keeps its legacy format, its commits
/// and its index. Everything reported here belongs to the copy, except
/// `source_tree_identity`, `source_index_generation`, `annotation_defaults`
/// and `continuity`, which describe the transition the copy is prepared to
/// publish.
#[derive(Clone, Debug)]
pub struct MigratedCopy {
    source_root: PathBuf,
    destination: PathBuf,
    source_format: RepositoryFormat,
    database_id: DatabaseId,
    source_commit: GitCommitRef,
    source_tree_identity: [u8; 32],
    source_index_generation: IndexGeneration,
    candidate_journal: PublicationJournal,
    working_tree_identity: [u8; 32],
    working_tree_entries: usize,
    dependency_entries: usize,
    annotation_defaults: MigrationAnnotationDefaults,
    continuity: MigrationContinuityRecord,
}

impl MigratedCopy {
    /// The legacy source the copy was taken from.
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    /// The disposable copy's working directory.
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// The source's recorded format, which the copy preserves for old readers.
    pub const fn source_format(&self) -> RepositoryFormat {
        self.source_format
    }

    /// The numeric source format coordinate, 1 or 2.
    pub const fn source_format_number(&self) -> u8 {
        self.source_format.number()
    }

    /// The stable database identity written into the candidate's canonical
    /// `.orna/database.orna`. A recorded legacy sidecar is preserved; older
    /// repositories without one receive a new identity.
    pub const fn database_id(&self) -> DatabaseId {
        self.database_id
    }

    /// The exact immutable source commit, which the copy carries under its
    /// original decoder and identity.
    pub fn source_commit(&self) -> &GitCommitRef {
        &self.source_commit
    }

    /// The derived content-versus-commit identity of the source tree.
    ///
    /// This is the comparison against the source's own recorded index tree, so
    /// an ordinary `git status` or index refresh cannot produce it. That is why
    /// a migration must derive it rather than infer it.
    pub const fn source_tree_identity(&self) -> &[u8; 32] {
        &self.source_tree_identity
    }

    /// The source's index generation at capture time.
    ///
    /// The copy cannot inherit it: the generation is read from the source's own
    /// index, so the copy settles and reports its own instead.
    pub const fn source_index_generation(&self) -> &IndexGeneration {
        &self.source_index_generation
    }

    /// The prepared migration journal binding the copy's legacy head to its
    /// format-3 candidate.
    ///
    /// The journal is what makes the candidate reachable: it holds its commit
    /// identity before the copy's branch head moves, so the migration is
    /// resumable and the copy's branch head stays on the legacy commit.
    pub fn candidate_journal(&self) -> &PublicationJournal {
        &self.candidate_journal
    }

    /// The copy's settled working-tree identity: its content-versus-commit
    /// comparison over the whole copied tree.
    pub const fn working_tree_identity(&self) -> &[u8; 32] {
        &self.working_tree_identity
    }

    /// The number of paths compared to settle the working-tree identity.
    pub const fn working_tree_entries(&self) -> usize {
        self.working_tree_entries
    }

    /// The number of gitlink entries carried into the copy's working tree.
    pub const fn dependency_entries(&self) -> usize {
        self.dependency_entries
    }

    /// The annotation-default coordinate the migration must report.
    pub const fn annotation_defaults(&self) -> MigrationAnnotationDefaults {
        self.annotation_defaults
    }

    /// The stream/checkpoint predecessor inventory the migration carries.
    ///
    /// Carrying it is what lets a resumed stream keep its consumer and
    /// checkpoint identity instead of resetting: the publisher looks up the
    /// recorded successor rather than re-deriving it.
    pub fn continuity(&self) -> &MigrationContinuityRecord {
        &self.continuity
    }
}

/// Prepares a format-3 disposable copy of one populated legacy repository and
/// the migration journal that sponsors it.
///
/// The source must be a non-bare worktree whose recorded format is 1 or 2.
/// The copy is built beside `destination`, fully synchronized, and installed
/// by one directory rename. A retry recovers an installed prepared copy only
/// when its source snapshot and migration coordinates still match. It carries
/// the source commits, index state, working tree and submodule worktrees, but
/// not the source's private runtime ownership. The source's recorded format
/// remains unchanged; retirement is represented only in the candidate commit.
/// Its journal and Git refs are synced before installation, pinning the
/// candidate commit and staged-index objects for restart recovery.
///
/// A preparation killed before installation leaves its private copy and an
/// owner marker in the directory that holds `destination`. The marker stays
/// locked while the preparation runs, so a later call reclaims a copy and its
/// marker only when it can take that lock. A copy without a marker is never
/// reclaimed.
///
/// `continuity` is the coordinate the conversion established. The annotation
/// default is derived from the decoded rows that `store_for` builds
/// ([`MigrationStore::annotation_default_entries`]). A declared
/// `annotation_defaults` is recorded as given, and the caller asserts it
/// against that decoded count; `None` records the decoded count.
pub fn prepare_format3_migration(
    source: &Repository,
    destination: &Path,
    continuity: MigrationContinuityRecord,
    annotation_defaults: Option<MigrationAnnotationDefaults>,
    store_for: MigrationStoreSource<'_>,
) -> Result<MigratedCopy, RepositoryError> {
    let destination = canonical_destination(destination)?;
    let source_root = source
        .worktree()
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    if destination == source_root || destination.starts_with(&source_root) {
        return Err(RepositoryError::InvalidSelector);
    }
    if path_exists(&quarantine_for(&destination)?)? {
        // A pending abort owns the reserved quarantine name; finish it first.
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    let parent = destination
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?
        .to_path_buf();
    reclaim_dead_staging(&parent, &destination)?;
    if path_exists(&destination)? {
        return recover_prepared_copy(
            source,
            &destination,
            continuity,
            annotation_defaults,
            false,
            Some(store_for),
        );
    }

    let claim = claim_staging(&parent)?;
    let result = (|| {
        let mut copy = prepare_format3_migration_in(
            source,
            &claim.staging,
            continuity,
            annotation_defaults,
            store_for,
        )?;
        let repository = Repository::discover(&claim.staging)?;
        pin_migration_index(
            &repository,
            copy.source_index_generation
                .tree()
                .ok_or(RepositoryError::InvalidFormatMigration)?
                .as_str(),
        )?;
        pin_migration_candidate(&repository, copy.candidate_journal.new_head())?;
        repository.write_publication_journal(&copy.candidate_journal)?;
        // Boundary: the staged copy holds its journal, which is what a restart
        // reads to validate the copy instead of rebuilding it.
        crate::publication_crash_barrier("migration-journal");
        sync_tree(&claim.staging)?;
        // Boundary: every staged byte and directory entry is durable, and the
        // copy is still private under its staging name.
        crate::publication_crash_barrier("migration-synced");
        publish_copy_noreplace(&parent, &claim.staging, &destination)?;
        claim.retire()?;
        // Boundary: the copy is installed under its final name. A restart finds
        // it here and recovers it rather than building a second one.
        crate::publication_crash_barrier("migration-published");
        copy.destination = destination;
        Ok(copy)
    })();
    if result.is_err() {
        claim.abandon();
    }
    result
}

/// What [`discard_prepared_format3_migration`] found at the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardOutcome {
    /// The prepared copy, or the quarantine an interrupted discard left behind,
    /// was removed.
    Discarded,
    /// Neither the destination nor its quarantine exists; nothing changed.
    NothingToDiscard,
    /// The destination is an activated format-3 repository. An earlier
    /// activation finished, so the copy is kept and nothing changed.
    AlreadyActivated,
}

/// The reserved sibling a prepared copy is renamed to before it is removed.
///
/// The name depends only on the destination's final component, so a discard
/// interrupted after the rename finds the same quarantine again.
pub fn discard_quarantine_path(destination: &Path) -> Result<PathBuf, RepositoryError> {
    quarantine_for(&canonical_destination(destination)?)
}

fn quarantine_for(destination: &Path) -> Result<PathBuf, RepositoryError> {
    let parent = destination
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    let name = destination
        .file_name()
        .ok_or(RepositoryError::InvalidSelector)?;
    let mut hash = Sha256::new();
    hash.update(b"orna.migration-abort-quarantine.v1\0");
    hash.update(name.as_encoded_bytes());
    let digest: [u8; 32] = hash.finalize().into();
    let suffix: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(parent.join(format!(".orna-migration-abort-{suffix}")))
}

/// Discards a prepared format-3 migration copy at `destination`, leaving the
/// source worktree and its snapshot untouched.
///
/// The destination must be the disposable copy this module prepared from
/// `source`: a worktree root of its own that still holds the prepared journal
/// and the working tree that journal recorded. Anything else is refused before
/// a byte is removed, so a directory the operator owns is never a discard
/// target.
///
/// Removal survives a crash at every step. A verified copy is first renamed to
/// its quarantine in one atomic step, which is the commit point: from then on
/// `destination` is absent and only the quarantine's removal remains. Running
/// the discard again finishes an interrupted removal, and reports
/// [`DiscardOutcome::NothingToDiscard`] once nothing is left.
///
/// A journal past the prepared stage means activation has begun. Only rolling
/// it forward is safe, so such a copy is refused with
/// [`RepositoryError::MigrationCopyAdvanced`]. A repository with no journal
/// that is already an activated format-3 store is kept and reported as
/// [`DiscardOutcome::AlreadyActivated`].
pub fn discard_prepared_format3_migration(
    source: &Repository,
    destination: &Path,
) -> Result<DiscardOutcome, RepositoryError> {
    let destination = canonical_destination(destination)?;
    let source_root = source
        .worktree()
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    let quarantine = quarantine_for(&destination)?;
    if destination == source_root
        || destination.starts_with(&source_root)
        || source_root.starts_with(&destination)
        || quarantine == source_root
        || source_root.starts_with(&quarantine)
    {
        return Err(RepositoryError::InvalidSelector);
    }
    let parent = destination
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    match (path_exists(&destination)?, path_exists(&quarantine)?) {
        (false, false) => {
            sync_directory(parent)?;
            Ok(DiscardOutcome::NothingToDiscard)
        }
        (true, true) => Err(RepositoryError::PublicationRecoveryConflict),
        (true, false) => {
            require_directory(&destination)?;
            let Some(journal) = copy_journal(&destination)? else {
                // A finished activation leaves no journal behind. Discard keeps
                // such a repository rather than removing it.
                let activated = match owned_copy_repository(&destination)? {
                    Some(copy) => is_activated_format3_copy(&copy)?,
                    None => false,
                };
                return if activated {
                    Ok(DiscardOutcome::AlreadyActivated)
                } else {
                    Err(RepositoryError::PublicationRecoveryConflict)
                };
            };
            if journal.stage() != crate::PublicationJournalStage::Prepared {
                return Err(RepositoryError::MigrationCopyAdvanced);
            }
            let (continuity, annotation_defaults) = migration_coordinates(&journal)?;
            // Discard removes a copy whatever its era; only preparation and
            // publication refuse the pre-change shape.
            recover_prepared_copy(
                source,
                &destination,
                continuity,
                Some(annotation_defaults),
                true,
                None,
            )?;
            publish_copy_noreplace(parent, &destination, &quarantine)?;
            // Boundary: the copy now exists only under the quarantine name. A
            // restart sees no destination and finishes the removal.
            crate::publication_crash_barrier("migration-discard-renamed");
            remove_quarantined_copy(parent, &quarantine)?;
            Ok(DiscardOutcome::Discarded)
        }
        (false, true) => {
            // The rename that created this quarantine validated the copy and is
            // the commit point. The quarantine is not re-validated: a source
            // changed since then must not strand the removal.
            require_directory(&quarantine)?;
            crate::publication_crash_barrier("migration-discard-renamed");
            remove_quarantined_copy(parent, &quarantine)?;
            Ok(DiscardOutcome::Discarded)
        }
    }
}

/// The candidate a prepared copy at `destination` publishes: the new head its
/// publication journal records, or the copy's head once no journal remains.
pub fn prepared_candidate(destination: &Path) -> Result<GitCommitRef, RepositoryError> {
    let destination = canonical_destination(destination)?;
    require_directory(&destination)?;
    let copy =
        owned_copy_repository(&destination)?.ok_or(RepositoryError::PublicationRecoveryConflict)?;
    match copy.read_publication_journal()? {
        Some(journal) => Ok(journal.new_head().clone()),
        None => copy.head()?.ok_or(RepositoryError::UnbornHead),
    }
}

/// What [`activate_prepared_format3_migration`] did to the copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// The confirmed candidate was published into the copy and its runtime
    /// intent completed. The copy is now the activated format-3 repository.
    Activated,
    /// An earlier activation completed: the copy holds the confirmed candidate
    /// as its head and no publication journal remains.
    AlreadyActivated,
}

/// Activates the prepared format-3 copy at `destination` by publishing the
/// candidate `confirmed` names through the copy's own PUB-3 journal (UPGRADE-3
/// steps 7 and 8). `confirmed` must be the journal's new head; anything else is
/// refused before the copy changes.
///
/// A prepared copy is re-derived from `source` through `store_for` before its
/// ref moves. Once the ref has advanced, activation only rolls forward: it
/// resumes the journal to completion and never rewinds the copy.
pub fn activate_prepared_format3_migration(
    source: &Repository,
    destination: &Path,
    confirmed: &GitCommitRef,
    owner_id: [u8; 16],
    store_for: MigrationStoreSource<'_>,
) -> Result<ActivationOutcome, RepositoryError> {
    let destination = canonical_destination(destination)?;
    let source_root = source
        .worktree()
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    if destination == source_root
        || destination.starts_with(&source_root)
        || source_root.starts_with(&destination)
    {
        return Err(RepositoryError::InvalidSelector);
    }
    require_directory(&destination)?;
    let copy =
        owned_copy_repository(&destination)?.ok_or(RepositoryError::PublicationRecoveryConflict)?;
    let _owner_lock = copy.acquire_runtime_owner_lock(owner_id)?;
    let Some(mut journal) = copy.read_publication_journal()? else {
        require_activated_copy(&copy, confirmed)?;
        return Ok(ActivationOutcome::AlreadyActivated);
    };
    let intent = journal
        .runtime_intent_id()
        .ok_or(RepositoryError::PublicationRecoveryConflict)?;
    if journal.new_head() != confirmed {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    if journal.stage() == crate::PublicationJournalStage::Prepared
        && copy.head()?.as_ref() == Some(journal.old_head())
    {
        let (continuity, annotation_defaults) = migration_coordinates(&journal)?;
        recover_prepared_copy(
            source,
            &destination,
            continuity,
            Some(annotation_defaults),
            false,
            Some(store_for),
        )?;
        let base_tree = journal
            .base_index_tree()
            .cloned()
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let candidate = copy.candidate_from_journal(&journal)?;
        let expected = IndexGeneration {
            head: Some(journal.old_head().clone()),
            tree: Some(base_tree),
        };
        // Boundary: the prepared copy is verified and its ref has not moved.
        crate::publication_crash_barrier("upgrade-activating");
        copy.publish_legacy_format_migration(&expected, &candidate, &mut journal)?;
    } else {
        match copy.recover_publication() {
            Ok(_) | Err(RepositoryError::RuntimeCompletionRequired) => {}
            Err(error) => return Err(error),
        }
        let Some(resumed) = copy.read_publication_journal()? else {
            require_activated_copy(&copy, confirmed)?;
            return Ok(ActivationOutcome::Activated);
        };
        if resumed.new_head() != confirmed {
            return Err(RepositoryError::PublicationRecoveryConflict);
        }
        journal = resumed;
    }
    match journal.stage() {
        crate::PublicationJournalStage::WorktreeReconciled => {
            // Boundary: the candidate is reconciled; the runtime intent is not
            // yet consumed.
            crate::publication_crash_barrier("upgrade-published");
            let (continuity, annotation_defaults) = migration_coordinates(&journal)?;
            let source_format = source
                .open_format_context()
                .map_err(RepositoryError::FormatContext)?
                .repository_format();
            let message = candidate_message(source_format, annotation_defaults, &continuity);
            let intent_hex = hex(&intent);
            let commit = confirmed.as_str();
            let message_digest_hex = hex(Sha256::digest(message.as_bytes()).as_slice());
            let record = format!("{intent_hex}\n{message_digest_hex}\n{commit}");
            let result_path = copy
                .runtime_paths()
                .root()
                .join(format!("reviewed-result-{intent_hex}"));
            copy.complete_reviewed_publication(intent, &result_path, &record, &mut journal)?;
        }
        crate::PublicationJournalStage::RuntimeCompleted
        | crate::PublicationJournalStage::Complete => {
            copy.recover_publication()?;
        }
        _ => return Err(RepositoryError::RuntimeCompletionRequired),
    }
    require_activated_copy(&copy, confirmed)?;
    Ok(ActivationOutcome::Activated)
}

fn require_activated_copy(
    copy: &Repository,
    confirmed: &GitCommitRef,
) -> Result<(), RepositoryError> {
    if copy.read_publication_journal()?.is_some()
        || copy.head()?.as_ref() != Some(confirmed)
        || !is_activated_format3_copy(copy)?
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    Ok(())
}

/// Whether `copy` holds a valid format-3 store. Callers establish separately
/// that no publication journal remains.
fn is_activated_format3_copy(copy: &Repository) -> Result<bool, RepositoryError> {
    let context = copy
        .open_format_context()
        .map_err(RepositoryError::FormatContext)?;
    Ok(context.repository_format() == RepositoryFormat::Format3
        && context.validate_store_root().is_ok())
}

/// Installs `staging` as `destination` without replacing anything there. A
/// concurrent occupant of `destination` is a recovery conflict.
fn publish_copy_noreplace(
    parent: &Path,
    staging: &Path,
    destination: &Path,
) -> Result<(), RepositoryError> {
    crate::init::publish_directory_noreplace(parent, staging, destination).map_err(|error| {
        match error {
            crate::RepositoryInitError::MetadataChanged => {
                RepositoryError::PublicationRecoveryConflict
            }
            crate::RepositoryInitError::PlatformUnsupported => RepositoryError::PlatformUnsupported,
            _ => RepositoryError::LocalStateUnavailable,
        }
    })
}

/// Publishes the activated copy in place of its source worktree (the one-step
/// `orna upgrade --to-format 3`). `prepared` must be the copy's preparation
/// and the copy must now hold its candidate as head with no publication
/// journal, as an activated format-3 store. The source must still match what
/// was prepared: a source that moved since is refused and left untouched.
///
/// The two directories swap with one `renameat2(RENAME_EXCHANGE)`, so the
/// worktree path holds either the source or the activated copy, never a mix.
/// The source snapshot then sits at the copy's path, as the rollback copy.
///
/// Only a standalone worktree (a `.git` directory) is exchanged; a linked
/// worktree's administrative directory would be orphaned by the swap.
pub fn exchange_activated_copy(
    source: &Repository,
    prepared: &MigratedCopy,
) -> Result<(), RepositoryError> {
    let destination = canonical_destination(prepared.destination())?;
    let source_root = source
        .worktree()
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    if destination == source_root
        || destination.starts_with(&source_root)
        || source_root.starts_with(&destination)
    {
        return Err(RepositoryError::InvalidSelector);
    }
    let git_metadata = fs::symlink_metadata(source_root.join(".git"))
        .map_err(|_| RepositoryError::NotAWorktree)?;
    if !git_metadata.is_dir() {
        return Err(RepositoryError::NotAWorktree);
    }
    let parent = source_root
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    if destination.parent() != Some(parent) {
        return Err(RepositoryError::InvalidSelector);
    }
    if source.head()?.as_ref() != Some(&prepared.source_commit)
        || source.index_generation()? != prepared.source_index_generation
        || working_tree_identity(source)?.0 != prepared.working_tree_identity
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    require_directory(&destination)?;
    let copy =
        owned_copy_repository(&destination)?.ok_or(RepositoryError::PublicationRecoveryConflict)?;
    if copy.read_publication_journal()?.is_some()
        || copy.head()?.as_ref() != Some(prepared.candidate_journal.new_head())
        || !is_activated_format3_copy(&copy)?
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    let source_name = source_root
        .file_name()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    let copy_name = destination
        .file_name()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    // Boundary: the copy is activated; the worktree path still holds the source.
    crate::publication_crash_barrier("upgrade-exchanging");
    exchange_directories(parent, Path::new(source_name), Path::new(copy_name))?;
    // Boundary: the worktree path holds the activated copy; the copy's path
    // holds the source snapshot.
    crate::publication_crash_barrier("upgrade-exchanged");
    sync_directory(parent)
}

/// Atomically swaps the entries `first` and `second` of `parent`.
#[cfg(target_os = "linux")]
fn exchange_directories(parent: &Path, first: &Path, second: &Path) -> Result<(), RepositoryError> {
    use rustix::fs::{RenameFlags, renameat_with};

    let parent_file = fs::File::open(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    renameat_with(
        &parent_file,
        first,
        &parent_file,
        second,
        RenameFlags::EXCHANGE,
    )
    .map_err(|error| match error {
        rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL | rustix::io::Errno::OPNOTSUPP => {
            RepositoryError::PlatformUnsupported
        }
        _ => RepositoryError::LocalStateUnavailable,
    })
}

#[cfg(not(target_os = "linux"))]
fn exchange_directories(
    _parent: &Path,
    _first: &Path,
    _second: &Path,
) -> Result<(), RepositoryError> {
    Err(RepositoryError::PlatformUnsupported)
}

/// Reads the journal of a copy that is its own repository. A path with no Git
/// metadata has no journal. So does a repository without a runtime directory:
/// that directory is not created here, because opening the journal would.
fn copy_journal(copy: &Path) -> Result<Option<PublicationJournal>, RepositoryError> {
    let Some(repository) = owned_copy_repository(copy)? else {
        return Ok(None);
    };
    if !repository.runtime_paths().root().is_dir() {
        return Ok(None);
    }
    repository.read_publication_journal()
}

fn migration_coordinates(
    journal: &PublicationJournal,
) -> Result<(MigrationContinuityRecord, MigrationAnnotationDefaults), RepositoryError> {
    let continuity = journal
        .migration_continuity()
        .cloned()
        .ok_or(RepositoryError::PublicationRecoveryConflict)?;
    let annotation_defaults = journal
        .migration_annotation_defaults()
        .ok_or(RepositoryError::PublicationRecoveryConflict)?;
    Ok((continuity, annotation_defaults))
}

fn require_directory(path: &Path) -> Result<(), RepositoryError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    Ok(())
}

/// Opens `path` as a Git worktree of its own, or returns `None` when it has no
/// Git metadata. Discovery walks up to any enclosing repository, so a worktree
/// that is not `path` itself is refused rather than mistaken for the copy.
fn owned_copy_repository(path: &Path) -> Result<Option<Repository>, RepositoryError> {
    if !path_exists(&path.join(".git"))? {
        return Ok(None);
    }
    let repository = Repository::discover(path)?;
    if repository.worktree().canonicalize().ok().as_deref() != Some(path) {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    Ok(Some(repository))
}

/// Removes a quarantined copy and flushes the removal to `parent`. Only a
/// discard creates the quarantine name, so a directory there is a copy the
/// discard already committed to removing. An interrupted removal is finished
/// by running the discard again.
fn remove_quarantined_copy(parent: &Path, copy: &Path) -> Result<(), RepositoryError> {
    fs::remove_dir_all(copy).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    sync_directory(parent)
}

fn prepare_format3_migration_in(
    source: &Repository,
    destination: &Path,
    continuity: MigrationContinuityRecord,
    declared_annotation_defaults: Option<MigrationAnnotationDefaults>,
    store_for: MigrationStoreSource<'_>,
) -> Result<MigratedCopy, RepositoryError> {
    let context = source
        .open_format_context()
        .map_err(RepositoryError::FormatContext)?;
    if context.supports_writes() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    if declared_annotation_defaults.is_some_and(|declared| declared.payload_inspected()) {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    let source_format = context.repository_format();
    require_legacy_worktree_metadata_matches(source, &context)?;
    let source_commit = source.head()?.ok_or(RepositoryError::UnbornHead)?;
    let source_index_generation = source.index_generation()?;
    require_index_tree(&source_index_generation)?;
    require_no_protected_pins(source)?;
    let source_tree_identity = source_tree_identity(source, &source_commit)?;
    let (source_working_tree_identity, source_working_tree_entries) =
        working_tree_identity(source)?;
    let dependency_entries = dependency_entries(source, &source_commit)?;
    let source_root = source.worktree().to_path_buf();
    let root = clone_source_graph(&source_root, destination)?;
    copy_submodule_admin(&source_root, &root)?;
    clear_checkout(&root)?;
    copy_working_tree(&source_root, &root)?;
    copy_index_blobs(&source_root, &root)?;
    copy_source_index(&source_root, &root)?;
    release_source_ownership(&root)?;

    let source_unchanged = || -> Result<bool, RepositoryError> {
        Ok(source.head()?.as_ref() != Some(&source_commit)
            || source.index_generation()? != source_index_generation
            || working_tree_identity(source)?.0 != source_working_tree_identity)
    };
    if source_unchanged()? {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    // Boundary: the staged graph, submodule administration, working tree and
    // index are on disk, and the source still matches what they were copied
    // from. Nothing is published yet and the staged copy has no candidate.
    crate::publication_crash_barrier("migration-staging");

    configure_migration_commit_identity(&root)?;
    let copy = Repository::discover(&root)?;
    let (working_tree_identity, working_tree_entries) = working_tree_identity(&copy)?;
    if working_tree_identity != source_working_tree_identity
        || working_tree_entries != source_working_tree_entries
        || copy.head()?.as_ref() != Some(&source_commit)
        || copy.index_generation()? != source_index_generation
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    let database_id = context.database_id().unwrap_or_else(DatabaseId::new_v4);
    let database_bytes = canonical_database_bytes(&database_id);
    let committed_database = committed_database_bytes(&context)?;
    // The store is built from the staged copy's decoded rows, so the annotation
    // coordinate below is derived from them, not trusted from the operator.
    let store = store_for(&copy, database_id)?;
    let annotation_defaults = match declared_annotation_defaults {
        Some(declared) => declared,
        None => MigrationAnnotationDefaults::new(store.annotation_default_entries)?,
    };
    // ORNA-META-001: the record is not a tracked file. Its bytes still feed the
    // runtime intent, so the identity covers the same coordinates; provenance
    // lives in the journal (continuity, annotation defaults) and in the
    // commit's `Orna-Migration` trailer.
    let record = migration_record(
        source_format,
        &source_commit,
        &source_tree_identity,
        annotation_defaults,
        &continuity,
    );
    // A migration commit is a synthetic record of one explicit transition, so
    // its time comes from the snapshot it converts rather than from this
    // process. Two independent preparations of one source then build the same
    // commit object, which is what lets a recovered copy be compared byte for
    // byte with a fresh one.
    let commit_time = source_commit_time(source, &source_commit)?;
    let message = candidate_message(source_format, annotation_defaults, &continuity);
    require_retired_paths(source, &source_commit, &store.retired)?;
    // The store was built while the source was still free to move. The source
    // must match what the candidate records before any candidate is built or
    // pinned; a source that changed is refused and left untouched.
    if source_unchanged()? {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    // Boundary: the migration store is admitted for this database, and no
    // candidate has been built from it yet.
    crate::publication_crash_barrier("migration-store");
    let (candidate, store_root) = build_migration_candidate(
        &copy,
        &source_commit,
        database_id,
        &store,
        &message,
        commit_time,
    )?;
    // Boundary: the candidate commit object exists in the staged copy and is
    // not yet pinned or journalled.
    crate::publication_crash_barrier("migration-candidate");
    let runtime_intent_id = migration_runtime_intent_id(
        source_index_generation
            .tree()
            .ok_or(RepositoryError::InvalidFormatMigration)?,
        &source_working_tree_identity,
        &record,
        &continuity,
    );
    let candidate_journal = PublicationJournal::new_with_runtime_intent(
        source_commit.clone(),
        candidate.commit().clone(),
        source_index_generation
            .tree()
            .cloned()
            .ok_or(RepositoryError::InvalidFormatMigration)?,
        runtime_intent_id,
        migration_entries(
            committed_database.as_deref(),
            &database_bytes,
            &committed_legacy_format(&context)?,
            &store,
        )?,
    )?
    .with_migration_continuity(continuity.clone())?
    .with_migration_annotation_defaults(annotation_defaults)?
    .with_store_root(&store_root)?;

    Ok(MigratedCopy {
        source_root,
        destination: root,
        source_format,
        database_id,
        source_commit,
        source_tree_identity,
        source_index_generation,
        candidate_journal,
        working_tree_identity,
        working_tree_entries,
        dependency_entries,
        annotation_defaults,
        continuity,
    })
}

fn require_legacy_worktree_metadata_matches(
    source: &Repository,
    context: &RepositoryFormatContext,
) -> Result<(), RepositoryError> {
    let format_path = source.worktree().join(LEGACY_FORMAT_PATH);
    let worktree_format =
        read_regular_metadata_file(&format_path)?.ok_or(RepositoryError::InvalidFormatMigration)?;
    let committed_format = committed_legacy_format(context)?;
    if worktree_format != committed_format {
        return Err(RepositoryError::InvalidFormatMigration);
    }

    let database_path = source.worktree().join(".orna/database.orna");
    let worktree_database = read_regular_metadata_file(&database_path)?;
    if worktree_database != committed_database_bytes(context)? {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    Ok(())
}

fn read_regular_metadata_file(path: &Path) -> Result<Option<Vec<u8>>, RepositoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let file = fs::File::open(path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let read_limit = u64::try_from(FORMAT_CONTEXT_MAX_METADATA_BYTES)
                .map_err(|_| RepositoryError::InvalidFormatMigration)?
                .checked_add(1)
                .ok_or(RepositoryError::InvalidFormatMigration)?;
            let mut bytes = Vec::new();
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if bytes.len() > FORMAT_CONTEXT_MAX_METADATA_BYTES {
                return Err(RepositoryError::InvalidFormatMigration);
            }
            Ok(Some(bytes))
        }
        Ok(_) => Err(RepositoryError::InvalidFormatMigration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(RepositoryError::LocalStateUnavailable),
    }
}

fn canonical_destination(destination: &Path) -> Result<PathBuf, RepositoryError> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    let name = destination
        .file_name()
        .ok_or(RepositoryError::InvalidSelector)?;
    Ok(parent.join(name))
}

/// The exclusive claim one preparation holds on its private staging copy.
///
/// The copy and its `<copy>.owner` marker are paired from creation until the
/// copy is installed (`retire`) or discarded (`abandon`). The marker stays
/// locked while this value lives, so a later preparation that can take the lock
/// knows that the owner has died.
struct StagingClaim {
    staging: PathBuf,
    owner: PathBuf,
    _lock: fs::File,
}

impl StagingClaim {
    /// Drops the marker once the copy is installed under its final name, so the
    /// installed destination is never paired with a staging claim.
    fn retire(&self) -> Result<(), RepositoryError> {
        fs::remove_file(&self.owner).map_err(|_| RepositoryError::LocalStateUnavailable)
    }

    /// Removes the copy, then its marker. A marker is kept when its copy could
    /// not be removed, so the pair stays reclaimable.
    fn abandon(&self) {
        if remove_staging_if_present(&self.staging).is_ok() {
            let _ = fs::remove_file(&self.owner);
        }
    }
}

/// Names a private staging copy under `parent` and locks its owner marker. The
/// copy itself is created later, by the preparation that holds the claim.
fn claim_staging(parent: &Path) -> Result<StagingClaim, RepositoryError> {
    let staging = parent.join(format!("{STAGING_PREFIX}{}", crate::Uuid::new_v4()));
    let mut owner_name = staging.as_os_str().to_owned();
    owner_name.push(OWNER_SUFFIX);
    let owner = PathBuf::from(owner_name);
    let lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&owner)
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    lock.try_lock_exclusive()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    // A reclaimer that ran between create and lock may already have removed it.
    if !is_regular_file(&owner)? {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    Ok(StagingClaim {
        staging,
        owner,
        _lock: lock,
    })
}

/// Removes a staging copy if one exists; a missing copy is already removed.
fn remove_staging_if_present(staging: &Path) -> Result<(), RepositoryError> {
    match fs::remove_dir_all(staging) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RepositoryError::LocalStateUnavailable),
    }
}

/// The staging name that an owner marker's file name pairs with, when the name
/// is one this module writes: a canonical lowercase UUID, which is how
/// `Uuid::new_v4` renders it. Other spellings of the same UUID are not reclaimed.
fn staged_name_of_owner(name: &str) -> Option<&str> {
    let staged = name.strip_suffix(OWNER_SUFFIX)?;
    let uuid = staged.strip_prefix(STAGING_PREFIX)?;
    let parsed = crate::Uuid::parse_str(uuid).ok()?;
    (parsed.to_string() == uuid).then_some(staged)
}

/// Reclaims each staging copy beside `parent` whose owner marker no live
/// preparation holds. A name equal to `destination` is never reclaimed, even
/// when it looks like a staging name.
fn reclaim_dead_staging(parent: &Path, destination: &Path) -> Result<(), RepositoryError> {
    let entries = fs::read_dir(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    for entry in entries {
        let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let file_name = entry.file_name();
        let Some(staged) = file_name.to_str().and_then(staged_name_of_owner) else {
            continue;
        };
        let staging = parent.join(staged);
        if staging.as_path() == destination {
            continue;
        }
        reclaim_owner(&entry.path(), &staging)?;
    }
    Ok(())
}

/// Removes an owner marker and the staging copy it pairs with, but only when no
/// live preparation holds the marker's lock.
fn reclaim_owner(owner: &Path, staging: &Path) -> Result<(), RepositoryError> {
    let Some(marker) = open_owner_marker(owner)? else {
        return Ok(());
    };
    match marker.try_lock_exclusive() {
        Ok(()) => {}
        // A live preparation holds its claim, so its pair is not ours to remove.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            return Ok(());
        }
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    }
    // The marker may have been retired between the open and the lock.
    if !is_regular_file(owner)? {
        return Ok(());
    }
    remove_staging_if_present(staging)?;
    match fs::remove_file(owner) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RepositoryError::LocalStateUnavailable),
    }
}

/// Opens an owner marker for locking and returns it only when it is a regular
/// file. A symbolic link is an error, and a FIFO is never waited on.
fn open_owner_marker(owner: &Path) -> Result<Option<fs::File>, RepositoryError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let marker = match options.open(owner) {
        Ok(marker) => marker,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    };
    let is_file = marker
        .metadata()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?
        .is_file();
    Ok(is_file.then_some(marker))
}

/// Whether `path` is a regular file, judged without following a symbolic link.
fn is_regular_file(path: &Path) -> Result<bool, RepositoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(RepositoryError::LocalStateUnavailable),
    }
}

fn path_exists(path: &Path) -> Result<bool, RepositoryError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(RepositoryError::LocalStateUnavailable),
    }
}

fn migration_index_ref(tree: &str) -> String {
    format!("refs/orna/migration-indexes/{tree}")
}

fn migration_index_blob_ref(object_id: &str) -> String {
    format!(
        "refs/orna/migration-index-blobs/{}",
        object_id.to_ascii_lowercase()
    )
}

fn pin_migration_index_blob(root: &Path, object_id: &str) -> Result<(), RepositoryError> {
    let object_id = object_id.to_ascii_lowercase();
    let reference = migration_index_blob_ref(&object_id);
    let zero = "0".repeat(object_id.len());
    let mut command = git_command(root);
    command.args(["update-ref", "--no-deref", &reference, &object_id, &zero]);
    if command
        .status()
        .map_err(|_| RepositoryError::GitUnavailable)?
        .success()
    {
        return Ok(());
    }
    let mut verify = git_command(root);
    verify.args(["rev-parse", "--verify", &reference]);
    let output = verify
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if output.status.success() && crate::trim_output(&output.stdout) == object_id {
        Ok(())
    } else {
        Err(RepositoryError::InvalidPublicationJournal)
    }
}

fn pin_migration_index(repository: &Repository, tree: &str) -> Result<(), RepositoryError> {
    let reference = migration_index_ref(tree);
    let zero = "0".repeat(tree.len());
    let mut command = git_command(repository.worktree());
    command.args(["update-ref", "--no-deref", &reference, tree, &zero]);
    if command
        .status()
        .map_err(|_| RepositoryError::GitUnavailable)?
        .success()
    {
        return Ok(());
    }
    require_migration_index_pin(repository, tree)
}

fn require_migration_index_pin(repository: &Repository, tree: &str) -> Result<(), RepositoryError> {
    let reference = migration_index_ref(tree);
    let mut command = git_command(repository.worktree());
    command.args(["rev-parse", "--verify", &reference]);
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() || crate::trim_output(&output.stdout) != tree {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    Ok(())
}
fn migration_candidate_ref(commit: &GitCommitRef) -> String {
    format!("refs/orna/migration-candidates/{}", commit.as_str())
}

fn pin_migration_candidate(
    repository: &Repository,
    commit: &GitCommitRef,
) -> Result<(), RepositoryError> {
    let reference = migration_candidate_ref(commit);
    let zero = "0".repeat(commit.as_str().len());
    let mut command = git_command(repository.worktree());
    command.args([
        "update-ref",
        "--no-deref",
        &reference,
        commit.as_str(),
        &zero,
    ]);
    if command
        .status()
        .map_err(|_| RepositoryError::GitUnavailable)?
        .success()
    {
        return Ok(());
    }
    require_migration_candidate_pin(repository, commit)
}

fn require_migration_candidate_pin(
    repository: &Repository,
    commit: &GitCommitRef,
) -> Result<(), RepositoryError> {
    let reference = migration_candidate_ref(commit);
    let mut command = git_command(repository.worktree());
    command.args(["rev-parse", "--verify", &reference]);
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() || crate::trim_output(&output.stdout) != commit.as_str() {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    Ok(())
}

/// Whether a publication journal records `.orna/migration.json`, the tracked
/// record that copies prepared before ORNA-META-001 carried.
pub(crate) fn journal_lists_migration_record(
    journal: &PublicationJournal,
) -> Result<bool, RepositoryError> {
    let record = crate::ManagedPath::new(MIGRATION_RECORD)?;
    Ok(journal
        .entries()
        .iter()
        .any(|entry| entry.path() == &record))
}

fn recover_prepared_copy(
    source: &Repository,
    destination: &Path,
    continuity: MigrationContinuityRecord,
    declared_annotation_defaults: Option<MigrationAnnotationDefaults>,
    allow_legacy: bool,
    store_for: Option<MigrationStoreSource<'_>>,
) -> Result<MigratedCopy, RepositoryError> {
    let source_context = source
        .open_format_context()
        .map_err(RepositoryError::FormatContext)?;
    if source_context.supports_writes()
        || !matches!(
            source_context.repository_format(),
            RepositoryFormat::Legacy1 | RepositoryFormat::Legacy2
        )
        || declared_annotation_defaults.is_some_and(|declared| declared.payload_inspected())
    {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    let source_format = source_context.repository_format();
    let source_commit = source.head()?.ok_or(RepositoryError::UnbornHead)?;
    let source_index_generation = source.index_generation()?;
    require_index_tree(&source_index_generation)?;
    let source_tree_identity = source_tree_identity(source, &source_commit)?;
    let (source_working_tree_identity, source_working_tree_entries) =
        working_tree_identity(source)?;

    let metadata =
        fs::symlink_metadata(destination).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    let repository = Repository::discover(destination)?;
    let journal = repository
        .read_publication_journal()?
        .ok_or(RepositoryError::PublicationRecoveryConflict)?;
    let legacy_copy = journal_lists_migration_record(&journal)?;
    if legacy_copy && !allow_legacy {
        return Err(RepositoryError::LegacyMigrationCopy);
    }
    // An undeclared coordinate is the one the journal recorded when it was
    // prepared; the store check below re-derives it from the rebuilt rows.
    let annotation_defaults = match declared_annotation_defaults {
        Some(declared) => declared,
        None => journal
            .migration_annotation_defaults()
            .ok_or(RepositoryError::PublicationRecoveryConflict)?,
    };
    if journal.stage() != crate::PublicationJournalStage::Prepared
        || journal.runtime_intent_id().is_none()
        || journal.old_head() != &source_commit
        || journal.base_index_tree() != source_index_generation.tree()
        || journal.migration_continuity() != Some(&continuity)
        || journal.migration_annotation_defaults() != Some(annotation_defaults)
        || repository.head()?.as_ref() != Some(&source_commit)
        || repository.index_generation()? != source_index_generation
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    require_migration_index_pin(
        &repository,
        source_index_generation
            .tree()
            .ok_or(RepositoryError::InvalidFormatMigration)?
            .as_str(),
    )?;
    require_migration_candidate_pin(&repository, journal.new_head())?;
    let candidate = repository.candidate_from_journal(&journal)?;
    let database_id = candidate_database_id(&repository, &candidate)?;
    let committed_database = committed_database_bytes(&source_context)?;
    let expected_entries = [
        PublicationJournalEntry::new(
            crate::ManagedPath::new(".orna/database.orna")?,
            committed_database.clone(),
            Some(canonical_database_bytes(&database_id)),
        ),
        PublicationJournalEntry::new(
            crate::ManagedPath::new(LEGACY_FORMAT_PATH)?,
            Some(committed_legacy_format(&source_context)?),
            None,
        ),
    ];
    let entries = journal.entries();
    let entries_match = if legacy_copy {
        // A pre-change copy journals the migration record. Discard is the only
        // caller that accepts one, and it binds that record by path and shape:
        // the bytes were written by the older writer and are not re-derived.
        entries.len() == 3
            && entries[..2] == expected_entries[..]
            && entries[2].path() == &crate::ManagedPath::new(MIGRATION_RECORD)?
            && entries[2].expected().is_none()
            && entries[2].next().is_some()
    } else if store_for.is_some() {
        // The store-bound branch below re-derives every entry and compares.
        true
    } else {
        // Discard binds the two coordinate entries exactly, then accepts each
        // retired legacy file as a deletion of its committed bytes.
        entries.len() >= 2
            && entries[..2] == expected_entries[..]
            && entries[2..]
                .iter()
                .all(|entry| entry.expected().is_some() && entry.next().is_none())
    };
    if !entries_match {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    if let Some(store_for) = store_for {
        // The recovered candidate must be exactly what a fresh preparation of
        // this source builds: same store, same entries, same commit object.
        let store = store_for(&repository, database_id)?;
        require_retired_paths(source, &source_commit, &store.retired)?;
        let database_bytes = canonical_database_bytes(&database_id);
        let expected = migration_entries(
            committed_database.as_deref(),
            &database_bytes,
            &committed_legacy_format(&source_context)?,
            &store,
        )?;
        if journal.entries() != expected {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        let commit_time = source_commit_time(source, &source_commit)?;
        let message = candidate_message(source_format, annotation_defaults, &continuity);
        let (rebuilt, store_root) = build_migration_candidate(
            &repository,
            &source_commit,
            database_id,
            &store,
            &message,
            commit_time,
        )?;
        let store_root_hex = store_root.to_hex();
        if rebuilt.commit() != journal.new_head()
            || journal.store_root().map(|root| root.as_str()) != Some(store_root_hex.as_str())
        {
            return Err(RepositoryError::PublicationRecoveryConflict);
        }
    }
    let (working_tree_identity, working_tree_entries) = working_tree_identity(&repository)?;
    if working_tree_identity != source_working_tree_identity
        || working_tree_entries != source_working_tree_entries
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }
    let dependency_entries = dependency_entries(source, &source_commit)?;

    Ok(MigratedCopy {
        source_root: source.worktree().to_path_buf(),
        destination: destination.to_path_buf(),
        source_format,
        database_id,
        source_commit,
        source_tree_identity,
        source_index_generation,
        candidate_journal: journal,
        working_tree_identity,
        working_tree_entries,
        dependency_entries,
        annotation_defaults,
        continuity,
    })
}

/// Reads one tracked file from a private candidate's exact tree, or `None` when
/// that path is absent there.
fn candidate_file(
    repository: &Repository,
    tree: &str,
    path: &str,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    let expression = format!("{tree}:{path}");
    let mut exists = git_command(repository.worktree());
    exists
        .args(["cat-file", "-e", &expression])
        .stderr(Stdio::null());
    if !exists
        .status()
        .map_err(|_| RepositoryError::GitUnavailable)?
        .success()
    {
        return Ok(None);
    }
    let mut blob = git_command(repository.worktree());
    blob.args(["cat-file", "blob", &expression]);
    let output = blob.output().map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(Some(output.stdout))
}

/// The canonical format-3 database identity a private migration candidate
/// records, with the legacy format marker required to be gone.
///
/// A prepared migration candidate is deliberately *transitional*: it carries the
/// format-3 `database.orna` and drops `format.orna`, but it still holds the
/// legacy row layout until the store build replaces it (UPGRADE-3.5,
/// MIGRATION.md items 3-5), so it is not a complete format-3 snapshot. Admitting
/// it as one would refuse it as `MixedFormats`, which is the right answer for a
/// repository a reader might otherwise treat as new-format; recovery instead
/// reads the two records that decide the format. The rest of the candidate is
/// not taken on trust: `candidate_from_journal` has already compared this exact
/// tree with the journal's managed entries.
fn candidate_database_id(
    repository: &Repository,
    candidate: &crate::PrivateCommit,
) -> Result<DatabaseId, RepositoryError> {
    let tree = candidate.tree().as_str();
    if candidate_file(repository, tree, LEGACY_FORMAT_PATH)?.is_some() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    let database = candidate_file(repository, tree, ".orna/database.orna")?
        .ok_or(RepositoryError::InvalidFormatMigration)?;
    crate::init::format_context::parse_canonical_database(&database)
        .map_err(|()| RepositoryError::InvalidFormatMigration)
}

/// The tree changes a migration candidate makes: the format-3 database record
/// written, the legacy format marker deleted, and each retired legacy file
/// deleted.
fn migration_changes(
    database_bytes: &[u8],
    store: &MigrationStore,
) -> Result<Vec<ManagedFileChange>, RepositoryError> {
    let mut changes = vec![
        ManagedFileChange::new(
            ManagedPath::new(".orna/database.orna")?,
            Some(database_bytes.to_vec()),
        ),
        ManagedFileChange::new(ManagedPath::new(LEGACY_FORMAT_PATH)?, None),
    ];
    for retired in &store.retired {
        changes.push(ManagedFileChange::new(
            ManagedPath::new(&retired.path)?,
            None,
        ));
    }
    Ok(changes)
}

/// The journal entries a migration candidate records for the same changes, each
/// with the exact bytes the source commits (or the new bytes it writes).
fn migration_entries(
    committed_database: Option<&[u8]>,
    database_bytes: &[u8],
    committed_format: &[u8],
    store: &MigrationStore,
) -> Result<Vec<PublicationJournalEntry>, RepositoryError> {
    let mut entries = vec![
        PublicationJournalEntry::new(
            ManagedPath::new(".orna/database.orna")?,
            committed_database.map(<[u8]>::to_vec),
            Some(database_bytes.to_vec()),
        ),
        PublicationJournalEntry::new(
            ManagedPath::new(LEGACY_FORMAT_PATH)?,
            Some(committed_format.to_vec()),
            None,
        ),
    ];
    for retired in &store.retired {
        entries.push(PublicationJournalEntry::new(
            ManagedPath::new(&retired.path)?,
            Some(retired.committed.clone()),
            None,
        ));
    }
    Ok(entries)
}

/// Builds the migration candidate commit from the store the caller supplied,
/// and returns it with the `.orna/store` root it admitted.
fn build_migration_candidate(
    repository: &Repository,
    source_commit: &GitCommitRef,
    database_id: DatabaseId,
    store: &MigrationStore,
    message: &str,
    commit_time: i64,
) -> Result<(PrivateCommit, NativeOid), RepositoryError> {
    let database_bytes = canonical_database_bytes(&database_id);
    let changes = migration_changes(&database_bytes, store)?;
    let candidate = repository
        .build_private_commit_with_relations_at(
            source_commit,
            &changes,
            RelationCandidateContent {
                relations: &store.relations,
                identities: &store.identities,
                watermarks: &store.watermarks,
            },
            database_id,
            message,
            commit_time,
        )
        .map_err(admission_error)?;
    let store_root = repository
        .tree_store_root(candidate.tree().as_str())
        .map_err(admission_error)?
        .ok_or(RepositoryError::InvalidFormatMigration)?;
    match &store.candidate_gate {
        Some(gate) => gate
            .check(repository, &candidate)
            .map_err(RepositoryError::MigrationStoreRefused)?,
        None if !store.relations.is_empty() => {
            return Err(RepositoryError::MigrationStoreRefused(
                "a migration with declared relations has no candidate gate".to_owned(),
            ));
        }
        None => {}
    }
    Ok((candidate, store_root))
}

/// Refuses a store whose retired paths are not legacy `.orna` files the source
/// commit records with exactly the named bytes.
fn require_retired_paths(
    source: &Repository,
    source_commit: &GitCommitRef,
    retired: &[RetiredLegacyPath],
) -> Result<(), RepositoryError> {
    let mut seen = BTreeSet::new();
    for entry in retired {
        let path = entry.path.as_str();
        let reserved = path == ".orna/database.orna"
            || path == LEGACY_FORMAT_PATH
            || path == ".orna/store"
            || path.starts_with(".orna/store/");
        if !path.starts_with(".orna/") || reserved || !seen.insert(path) {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        if candidate_file(source, source_commit.as_str(), path)?.as_deref()
            != Some(entry.committed.as_slice())
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
    }
    Ok(())
}

/// Maps a relation admission refusal to the repository error a migration
/// reports. The refusal's own display names the row, so no raw dump is kept.
fn admission_error(error: RelationAdmissionError) -> RepositoryError {
    match error {
        RelationAdmissionError::Repository(error) => error,
        RelationAdmissionError::Graph(error) => RepositoryError::Graph(error),
        RelationAdmissionError::Format(error) => RepositoryError::FormatContext(error),
        other => RepositoryError::MigrationStoreRefused(other.to_string()),
    }
}

fn sync_tree(root: &Path) -> Result<(), RepositoryError> {
    let metadata =
        fs::symlink_metadata(root).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        return fs::File::open(root)
            .and_then(|file| file.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable);
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(root).map_err(|_| RepositoryError::LocalStateUnavailable)? {
            let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
            sync_tree(&entry.path())?;
        }
        return sync_directory(root);
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), RepositoryError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepositoryError::LocalStateUnavailable)
}

/// Gives the migration commit a stable identity independent of the caller's
/// Git configuration. The source commit remains unchanged; this identity only
/// names the explicit format-transition commit in the disposable copy.
fn configure_migration_commit_identity(root: &Path) -> Result<(), RepositoryError> {
    for (key, value) in [
        ("user.name", "Orna migration"),
        ("user.email", "migration@orna.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        let mut command = git_command(root);
        command.args(["config", "--local", key, value]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
    }
    Ok(())
}

/// The committer time of the legacy snapshot, in seconds since the Unix epoch.
///
/// The migration commit records one explicit transition of one snapshot. Taking
/// its time from that snapshot, rather than from the process that converts it,
/// makes the candidate commit object a function of the input alone: two
/// independent preparations of the same source then build the same object.
fn source_commit_time(source: &Repository, commit: &GitCommitRef) -> Result<i64, RepositoryError> {
    let mut command = git_command(source.worktree());
    command
        .args(["log", "-1", "--format=%ct"])
        .arg(commit.as_str());
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| RepositoryError::GitOperationFailed)?;
    text.trim()
        .parse::<i64>()
        .map_err(|_| RepositoryError::GitOperationFailed)
}

/// The committed `format.orna` bytes of one legacy source, exactly as its
/// writer recorded them. The journal names these bytes as the file the
/// migration removes, so they are never rebuilt from the format number.
fn committed_legacy_format(context: &RepositoryFormatContext) -> Result<Vec<u8>, RepositoryError> {
    context
        .read_committed_file(LEGACY_FORMAT_PATH, FORMAT_CONTEXT_MAX_METADATA_BYTES)
        .map_err(|_| RepositoryError::InvalidFormatMigration)
}

/// The committed `.orna/database.orna` bytes a migration replaces. A source
/// that tracks a legacy sidecar names those bytes as the path's expected
/// content; a source that tracks none expects the path absent.
fn committed_database_bytes(
    context: &RepositoryFormatContext,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    match context.database_id() {
        Some(_) => context
            .read_committed_file(".orna/database.orna", FORMAT_CONTEXT_MAX_METADATA_BYTES)
            .map(Some)
            .map_err(|_| RepositoryError::InvalidFormatMigration),
        None => Ok(None),
    }
}

/// Names the migration transition in the candidate's own message.
///
/// The source commit this candidate descends from stays immutable, so the
/// message is the only place the copy can state the coordinate that is not
/// recoverable from the legacy objects themselves.
///
/// The coordinate is written as a Git trailer so it stays machine-readable
/// without becoming part of the prose: a `history` walk can say which commits
/// established the migrated representation instead of leaving the reader to
/// infer it from the message text. MIGRATION.md item 9 requires the migration
/// to be one journalled, reviewed commit, and this trailer is how that commit
/// names itself.
fn candidate_message(
    source_format: RepositoryFormat,
    annotation_defaults: MigrationAnnotationDefaults,
    continuity: &MigrationContinuityRecord,
) -> String {
    format!(
        "explicit format {} to {} migration: {} rows take the {} default annotation, {} stream \
         checkpoint identities carried\n\n{}: {}",
        source_format.number(),
        crate::init::format_context::FINAL_REPOSITORY_FORMAT,
        annotation_defaults.entry_count(),
        LEGACY_ANNOTATION_DEFAULT_FORMAT,
        continuity.predecessors().len(),
        MIGRATION_MARKER_TRAILER,
        migration_marker(source_format),
    )
}

/// The marker value one legacy source format's migration commit records.
pub fn migration_marker(source_format: RepositoryFormat) -> String {
    format!(
        "format-{}-to-{}",
        source_format.number(),
        crate::init::format_context::FINAL_REPOSITORY_FORMAT
    )
}

/// Clones the source's commit graph into `destination`.
///
/// A clone carries the source's branches, tags and remote refs byte for byte,
/// which is exactly the immutability an old reader needs. Its checkout is
/// discarded afterwards, so no clone-generated file can be mistaken for
/// migrated content.
fn clone_source_graph(source_root: &Path, destination: &Path) -> Result<PathBuf, RepositoryError> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    let name = destination
        .file_name()
        .ok_or(RepositoryError::InvalidSelector)?;
    let root = parent.join(name);
    match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // An existing destination is never merged into or overwritten: a
        // migration must not write into a directory the caller did not mean.
        Ok(_) => return Err(RepositoryError::PublicationRecoveryConflict),
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    }
    let canonical_source = source_root
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    // A copy inside the source would migrate the migration.
    if root == canonical_source || root.starts_with(&canonical_source) {
        return Err(RepositoryError::InvalidSelector);
    }
    let mut command = git_command(&parent);
    command
        // `--no-local` forces the real transport, so the copy keeps its own
        // object store and neither depends on nor writes into the source's.
        .args(["clone", "--no-local", "--quiet", "--"])
        .arg(&canonical_source)
        .arg(&root);
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(root)
}

/// Removes the clone's checkout while retaining its Git metadata.
fn clear_checkout(root: &Path) -> Result<(), RepositoryError> {
    let entries = fs::read_dir(root).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    for entry in entries {
        let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if orna_foundation::is_git_metadata_component(entry.file_name().as_encoded_bytes()) {
            continue;
        }
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        } else if metadata.is_dir() {
            fs::remove_dir_all(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        }
    }
    Ok(())
}

/// Copies a clone's missing submodule repositories, including each module's
/// private index and uncommitted objects. Gitfile pointers remain relative to
/// the matching superproject layout and therefore resolve inside the copy.
fn copy_submodule_admin(source: &Path, destination: &Path) -> Result<(), RepositoryError> {
    let source_modules = git_path(source, "modules")?;
    match fs::symlink_metadata(&source_modules) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        _ => return Err(RepositoryError::UnsafeManagedPath),
    }
    let destination_modules = git_path(destination, "modules")?;
    match fs::symlink_metadata(&destination_modules) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(&destination_modules)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err(RepositoryError::UnsafeManagedPath),
    }
    copy_entry(&source_modules, &destination_modules)
}

/// Resolves Git's per-worktree administrative path without assuming `.git` is
/// a directory in the source or destination worktree.
fn git_path(root: &Path, name: &str) -> Result<PathBuf, RepositoryError> {
    let mut command = git_command(root);
    command.args(["rev-parse", "--path-format=absolute", "--git-path", name]);
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    let path = String::from_utf8(output.stdout).map_err(|_| RepositoryError::GitOperationFailed)?;
    Ok(PathBuf::from(path.trim_end_matches('\n')))
}

/// Copies every index-referenced file blob absent from the graph clone. A
/// staged blob is not reachable from the source's committed refs, so clone
/// alone cannot guarantee that the exact copied index remains readable.
fn copy_index_blobs(source: &Path, destination: &Path) -> Result<(), RepositoryError> {
    let mut command = git_command(source);
    command.args(["ls-files", "--stage", "-z"]);
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }

    for entry in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let header = entry
            .split(|byte| *byte == b'\t')
            .next()
            .ok_or(RepositoryError::GitOperationFailed)?;
        let mut fields = header.split(|byte| *byte == b' ');
        let mode = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
        let object_id = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
        if !matches!(mode, b"100644" | b"100755" | b"120000") {
            continue;
        }
        if object_id.is_empty() || !object_id.iter().all(u8::is_ascii_hexdigit) {
            return Err(RepositoryError::GitOperationFailed);
        }
        let object_id =
            std::str::from_utf8(object_id).map_err(|_| RepositoryError::GitOperationFailed)?;

        let mut exists = git_command(destination);
        exists
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-e"])
            .arg(format!("{object_id}^{{blob}}"));
        if exists
            .stderr(Stdio::null())
            .status()
            .map_err(|_| RepositoryError::GitUnavailable)?
            .success()
        {
            pin_migration_index_blob(destination, object_id)?;
            continue;
        }

        let mut read = git_command(source);
        read.env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "blob", object_id])
            .stdout(Stdio::piped());
        let mut read = read.spawn().map_err(|_| RepositoryError::GitUnavailable)?;
        let mut write = git_command(destination);
        write
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut write = write.spawn().map_err(|_| RepositoryError::GitUnavailable)?;
        std::io::copy(
            &mut read
                .stdout
                .take()
                .ok_or(RepositoryError::GitOperationFailed)?,
            &mut write
                .stdin
                .take()
                .ok_or(RepositoryError::GitOperationFailed)?,
        )
        .map_err(|_| RepositoryError::GitOperationFailed)?;
        let read_status = read
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let write = write
            .wait_with_output()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !read_status.success()
            || !write.status.success()
            || crate::trim_output(&write.stdout) != object_id
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        pin_migration_index_blob(destination, object_id)?;
    }
    Ok(())
}

/// Installs the source's exact stage entries in the copy, including split-index
/// backing files. This keeps staged and unstaged versions distinct.
fn copy_source_index(source: &Path, destination: &Path) -> Result<(), RepositoryError> {
    let source_index = git_path(source, "index")?;
    let destination_index = git_path(destination, "index")?;
    let parent = destination_index
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    fs::create_dir_all(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    fs::copy(&source_index, &destination_index)
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;

    let source_parent = source_index
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    for entry in fs::read_dir(source_parent).map_err(|_| RepositoryError::LocalStateUnavailable)? {
        let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("sharedindex.")
        {
            continue;
        }
        fs::copy(entry.path(), parent.join(entry.file_name()))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    }
    Ok(())
}

/// Copies the source working-tree content into the clone.
///
/// A clone carries only tracked, non-ignored paths, so every regular file and
/// symlink the legacy working tree holds is migrated explicitly. The source's
/// own `.git` is never copied: Git's own refs already arrived with the clone,
/// and private runtime state is not payload.
fn copy_working_tree(source_root: &Path, destination_root: &Path) -> Result<(), RepositoryError> {
    let entries = fs::read_dir(source_root).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    for entry in entries {
        let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let name = entry.file_name();
        if orna_foundation::is_git_metadata_component(name.as_encoded_bytes()) {
            continue;
        }
        copy_entry(&entry.path(), &destination_root.join(&name))?;
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> Result<(), RepositoryError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let file_type = metadata.file_type();
    // A pre-existing symlink would redirect the write outside the copy.
    if let Ok(existing) = fs::symlink_metadata(destination)
        && existing.file_type().is_symlink()
    {
        fs::remove_file(destination).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    }
    if file_type.is_symlink() {
        let target = fs::read_link(source).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let _ = fs::remove_file(destination);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, destination)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            let _ = target;
            return Err(RepositoryError::PlatformUnsupported);
        }
    }
    if file_type.is_dir() {
        fs::create_dir_all(destination).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let entries = fs::read_dir(source).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        for entry in entries {
            let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if file_type.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        }
        fs::copy(source, destination).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    }
    // A socket, fifo or device node is not repository payload.
    Ok(())
}

/// Releases the source runtime's private ownership from the copy.
///
/// The copy was cloned fresh, so it owns an empty runtime directory. It is
/// removed rather than inherited: republishing the source's ownership is
/// exactly the "live lease" MIGRATION.md forbids moving, and a fresh runtime
/// re-establishes its own.
fn release_source_ownership(root: &Path) -> Result<(), RepositoryError> {
    let repository = Repository::discover(root)?;
    let runtime = repository.runtime_paths().root().to_path_buf();
    match fs::symlink_metadata(&runtime) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&runtime).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        }
        // A runtime pointer that is a link or a file is not this repository's
        // ownership record, so it is never followed or removed.
        Ok(_) => return Err(RepositoryError::UnsafeManagedPath),
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    }
    settle_index(root)
}

/// Refuses a source whose protected pin refs the copy cannot carry.
///
/// The protected pin namespace is not part of the clone transport, so a pin
/// the migration must preserve could not survive it. Refusing is the only
/// honest outcome: silently dropping the pin would break publication.
fn require_no_protected_pins(source: &Repository) -> Result<(), RepositoryError> {
    let mut command = source.observer_command();
    command.args([
        "for-each-ref",
        "--count=1",
        "--format=%(refname)",
        "refs/orna/pins/",
    ]);
    let output = source.run(command)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    if crate::trim_output(&output.stdout).is_empty() {
        Ok(())
    } else {
        Err(RepositoryError::InvalidFormatMigration)
    }
}

fn require_index_tree(generation: &IndexGeneration) -> Result<(), RepositoryError> {
    if generation.tree().is_none() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    Ok(())
}

/// Names the migration's inputs, so a retried preparation of the same source
/// carries the same runtime intent instead of a fresh random one. The candidate
/// commit is excluded: the intent identifies the migration, not one written commit.
/// The source's working tree identity is included: two sources at the same
/// commit and index that differ only in unstaged or untracked work are different
/// migrations.
fn migration_runtime_intent_id(
    index_tree: &crate::IndexTreeRef,
    source_working_tree_identity: &[u8; 32],
    record: &[u8],
    continuity: &MigrationContinuityRecord,
) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"orna.migration-runtime-intent.v2\0");
    hash_length_prefixed(&mut hash, index_tree.as_str().as_bytes());
    hash.update(source_working_tree_identity);
    hash_length_prefixed(&mut hash, record);
    hash.update((continuity.predecessors().len() as u64).to_be_bytes());
    for predecessor in continuity.predecessors() {
        hash.update(predecessor.legacy_consumer_identity());
        hash_length_prefixed(&mut hash, predecessor.legacy_source_identity());
        hash_optional_length_prefixed(&mut hash, predecessor.legacy_partition_identity());
        hash.update(predecessor.format3_consumer_identity());
        hash_length_prefixed(&mut hash, predecessor.format3_source_identity());
        hash_optional_length_prefixed(&mut hash, predecessor.format3_partition_identity());
    }
    let digest: [u8; 32] = hash.finalize().into();
    let mut intent = [0; 16];
    intent.copy_from_slice(&digest[..16]);
    intent
}

fn hash_length_prefixed(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn hash_optional_length_prefixed(hash: &mut Sha256, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            hash.update([1]);
            hash_length_prefixed(hash, bytes);
        }
        None => hash.update([0]),
    }
}

/// Settles the copy's index, repairing the stat entries its own checkout left
/// behind so the migration does not republish the source's block transfers.
fn settle_index(root: &Path) -> Result<(), RepositoryError> {
    let mut command = git_command(root);
    command.args(["update-index", "--refresh", "--quiet"]);
    // A refresh that reports `needs update` still repaired every stat entry it
    // could, which is the whole purpose here; only a missing Git or a killed
    // process is fatal.
    match command.output() {
        Err(_) => Err(RepositoryError::GitUnavailable),
        Ok(output) if output.status.code().is_none() => Err(RepositoryError::GitOperationFailed),
        Ok(_) => Ok(()),
    }
}

/// Reads the derived content-versus-commit identity of one source commit.
///
/// The comparison is anchored to the commit and to the source's own recorded
/// index tree, so it observes the state a reader would check out rather than a
/// fresh `HEAD` diff that a concurrent working tree could shift.
fn source_tree_identity(
    repository: &Repository,
    commit: &GitCommitRef,
) -> Result<[u8; 32], RepositoryError> {
    let mut command = repository.observer_command();
    command.args([
        "diff-tree",
        "--no-commit-id",
        "--raw",
        "-z",
        commit.as_str(),
    ]);
    let output = repository.run(command)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    let mut hash = Sha256::new();
    hash.update(b"orna.migration.source-tree-identity.v1\0");
    hash.update(commit.as_str().as_bytes());
    hash.update([0]);
    hash.update(&output.stdout);
    Ok(hash.finalize().into())
}

/// Counts the gitlink entries the source commit records.
fn dependency_entries(
    repository: &Repository,
    commit: &GitCommitRef,
) -> Result<usize, RepositoryError> {
    repository.objects().check_expansion(commit.as_str(), "")?;
    let mut command = repository.observer_command();
    command.args(["ls-tree", "-r", "-z", commit.as_str()]);
    let output = repository.run(command)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b"160000 "))
        .count())
}

/// Settles a stable identity over copied content and nested repository state.
/// Main `.git` metadata is excluded; nested HEADs and indexes are included so
/// prepared-copy recovery detects dirty submodule changes.
fn working_tree_identity(copy: &Repository) -> Result<([u8; 32], usize), RepositoryError> {
    let mut hash = Sha256::new();
    hash.update(b"orna.migration.working-tree-identity.v3\0");
    let mut entries = 0;
    hash_working_tree(copy.worktree(), Path::new(""), &mut hash, &mut entries)?;
    Ok((hash.finalize().into(), entries))
}
fn hash_working_tree(
    root: &Path,
    relative: &Path,
    hash: &mut Sha256,
    entry_count: &mut usize,
) -> Result<(), RepositoryError> {
    let directory = root.join(relative);
    let mut entries = fs::read_dir(&directory)
        .map_err(|_| RepositoryError::LocalStateUnavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name == ".git" {
            if !relative.as_os_str().is_empty() {
                *entry_count = entry_count
                    .checked_add(1)
                    .ok_or(RepositoryError::LocalStateUnavailable)?;
                hash_nested_git_state(&directory, hash)?;
            }
            continue;
        }
        if orna_foundation::is_git_metadata_component(name.as_encoded_bytes()) {
            continue;
        }
        let child = relative.join(name);
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let file_type = metadata.file_type();
        if !file_type.is_file() && !file_type.is_dir() && !file_type.is_symlink() {
            continue;
        }
        *entry_count = entry_count
            .checked_add(1)
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let path_bytes = child.as_os_str().as_encoded_bytes();
        hash.update((path_bytes.len() as u64).to_be_bytes());
        hash.update(path_bytes);
        #[cfg(unix)]
        hash.update(std::os::unix::fs::MetadataExt::mode(&metadata).to_be_bytes());
        #[cfg(not(unix))]
        hash.update([u8::from(metadata.permissions().readonly())]);

        if file_type.is_symlink() {
            hash.update(b"symlink\0");
            let target =
                fs::read_link(entry.path()).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let target_bytes = target.as_os_str().as_encoded_bytes();
            hash.update((target_bytes.len() as u64).to_be_bytes());
            hash.update(target_bytes);
        } else if file_type.is_dir() {
            hash.update(b"directory\0");
            hash_working_tree(root, &child, hash, entry_count)?;
        } else {
            hash.update(b"file\0");
            hash.update(metadata.len().to_be_bytes());
            let mut file =
                fs::File::open(entry.path()).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let mut buffer = [0u8; 64 * 1024];
            let mut bytes_read = 0u64;
            loop {
                let length = file
                    .read(&mut buffer)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                if length == 0 {
                    break;
                }
                bytes_read = bytes_read
                    .checked_add(length as u64)
                    .ok_or(RepositoryError::LocalStateUnavailable)?;
                hash.update(&buffer[..length]);
            }
            if bytes_read != metadata.len() {
                return Err(RepositoryError::LocalStateUnavailable);
            }
        }
    }
    Ok(())
}

fn hash_nested_git_state(root: &Path, hash: &mut Sha256) -> Result<(), RepositoryError> {
    hash.update(b"nested-git-state.v1\0");
    let mut head = git_command(root);
    head.args(["rev-parse", "--verify", "HEAD"]);
    let output = head.output().map_err(|_| RepositoryError::GitUnavailable)?;
    if output.status.success() {
        let object = crate::trim_output(&output.stdout);
        hash.update(b"head\0");
        hash.update((object.len() as u64).to_be_bytes());
        hash.update(object);
    } else {
        hash.update(b"unborn-head\0");
    }

    let head_path = git_path(root, "HEAD")?;
    hash_git_admin_file(&head_path, hash)?;
    let index = git_path(root, "index")?;
    hash_git_admin_file(&index, hash)?;
    let parent = index
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    let entries = fs::read_dir(parent)
        .map_err(|_| RepositoryError::LocalStateUnavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let mut shared_indexes = entries
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
        })
        .collect::<Vec<_>>();
    shared_indexes.sort_by_key(|entry| entry.file_name());
    for entry in shared_indexes {
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name);
        hash_git_admin_file(&entry.path(), hash)?;
    }
    Ok(())
}

fn hash_git_admin_file(path: &Path, hash: &mut Sha256) -> Result<(), RepositoryError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hash.update(b"missing\0");
            return Ok(());
        }
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        hash.update(b"symlink\0");
        let target = fs::read_link(path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let target = target.as_os_str().as_encoded_bytes();
        hash.update((target.len() as u64).to_be_bytes());
        hash.update(target);
        return Ok(());
    }
    if !file_type.is_file() {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    hash.update(b"file\0");
    #[cfg(unix)]
    hash.update(std::os::unix::fs::MetadataExt::mode(&metadata).to_be_bytes());
    #[cfg(not(unix))]
    hash.update([u8::from(metadata.permissions().readonly())]);
    hash.update(metadata.len().to_be_bytes());
    let mut file = fs::File::open(path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes_read = 0u64;
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if length == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(length as u64)
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        hash.update(&buffer[..length]);
    }
    if bytes_read != metadata.len() {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    Ok(())
}

/// Builds the copy-local record of the migration coordinates.
///
/// The record is JSON with the same literal field spelling the repository's own
/// metadata uses, so a reader parses it with the existing profile scanner
/// rather than a second format definition.
fn migration_record(
    source_format: RepositoryFormat,
    source_commit: &GitCommitRef,
    source_tree_identity: &[u8; 32],
    annotation_defaults: MigrationAnnotationDefaults,
    continuity: &MigrationContinuityRecord,
) -> Vec<u8> {
    format!(
        "{{\n    source_format: {source_format},\n    target_format: {target_format},\n    \
         source_commit: \"{commit}\",\n    source_tree_identity: \"{tree}\",\n    \
         annotation_default_format: \"{annotation}\",\n    annotation_default_entries: {count},\n    \
         payload_inspected: {inspected},\n    continuity_predecessors: {predecessors},\n    \
         stage: \"prepared\",\n}}\n",
        source_format = source_format.number(),
        target_format = crate::init::format_context::FINAL_REPOSITORY_FORMAT,
        commit = source_commit.as_str(),
        tree = hex(source_tree_identity),
        annotation = LEGACY_ANNOTATION_DEFAULT_FORMAT,
        count = annotation_defaults.entry_count(),
        inspected = annotation_defaults.payload_inspected(),
        predecessors = continuity.predecessors().len(),
    )
    .into_bytes()
}

/// A Git command bound to one directory, with inherited routing scrubbed and
/// repository discovery stopped at the directory's parent. Every call site in
/// this module names a repository root (the discovered worktree, a migration
/// copy, a dependency clone, or an index source/destination). The bind is
/// unconditional so a directory that is not a root cannot reach an enclosing
/// repository's refs or objects.
fn git_command(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(directory);
    crate::scrub_git_routing_environment(&mut command);
    crate::bind_git_ceiling_above(&mut command, directory);
    command
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        text.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        OWNER_SUFFIX, STAGING_PREFIX, claim_staging, migration_index_ref, pin_migration_index,
        reclaim_dead_staging, require_migration_index_pin,
    };
    use crate::Repository;
    use std::process::Command;

    /// Git's well-known empty tree; update-ref accepts it without storage.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn git(directory: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(directory)
                .status()
                .unwrap()
                .success()
        );
    }

    /// Drops a claim as process death would, then reclaims until its owner marker
    /// is gone. The marker's flock is released only when every descriptor for it
    /// is closed, and a child forked by a concurrent test holds a duplicate of the
    /// claim's descriptor until that child execs. A reclaim during that window sees
    /// the lock held and skips, which is the fail-safe outcome; a later reclaim
    /// removes the pair. The wait is bounded, so a real leak still fails.
    fn die_and_reclaim(
        claim: super::StagingClaim,
        parent: &std::path::Path,
        destination: &std::path::Path,
    ) {
        let owner = claim.owner.clone();
        drop(claim);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            reclaim_dead_staging(parent, destination).unwrap();
            if !owner.exists() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "dead owner {} stayed locked for 60s",
                owner.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn migration_index_pins_do_not_fall_through_a_corrupt_child_head() {
        let parent = tempfile::TempDir::new().unwrap();
        git(parent.path(), &["init", "-q"]);
        let child = parent.path().join("child");
        std::fs::create_dir(&child).unwrap();
        git(&child, &["init", "-q"]);
        let repository = Repository::discover(&child).unwrap();
        pin_migration_index(&repository, EMPTY_TREE).unwrap();
        require_migration_index_pin(&repository, EMPTY_TREE).unwrap();

        // The enclosing repository carries a pin that the child must not see.
        git(
            parent.path(),
            &["update-ref", &migration_index_ref(EMPTY_TREE), EMPTY_TREE],
        );
        std::fs::write(child.join(".git/HEAD"), "garbage\n").unwrap();
        assert!(require_migration_index_pin(&repository, EMPTY_TREE).is_err());
        assert!(pin_migration_index(&repository, EMPTY_TREE).is_err());
    }

    #[test]
    fn copy_index_blobs_fails_closed_for_corrupt_root_under_healthy_parent() {
        let parent = tempfile::TempDir::new().unwrap();
        let child = crate::tests::corrupt_child_under_healthy_parent(parent.path());
        let destination = parent.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        git(&destination, &["init", "-q"]);
        assert!(super::copy_index_blobs(&child, &destination).is_err());
    }

    #[test]
    fn migration_git_commands_do_not_reach_an_enclosing_repository_from_a_plain_subdirectory() {
        let parent = tempfile::TempDir::new().unwrap();
        git(parent.path(), &["init", "-q"]);
        git(
            parent.path(),
            &["update-ref", &migration_index_ref(EMPTY_TREE), EMPTY_TREE],
        );
        let plain = parent.path().join("plain");
        std::fs::create_dir(&plain).unwrap();

        // Without the unconditional bind, discovery would read the parent's refs.
        let pin = super::git_command(&plain)
            .args(["rev-parse", "--verify", &migration_index_ref(EMPTY_TREE)])
            .output()
            .unwrap();
        assert!(!pin.status.success());
        assert!(super::git_path(&plain, "HEAD").is_err());

        let destination = parent.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        git(&destination, &["init", "-q"]);
        assert!(super::copy_index_blobs(&plain, &destination).is_err());
    }

    #[test]
    fn killed_migration_staging_is_reclaimed_with_its_owner() {
        let parent = tempfile::TempDir::new().unwrap();
        let destination = parent.path().join("destination");
        let claim = claim_staging(parent.path()).unwrap();
        let (staging, owner) = (claim.staging.clone(), claim.owner.clone());
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("partial"), b"copy").unwrap();
        // Both remain until the dead claim is reclaimed.
        die_and_reclaim(claim, parent.path(), &destination);
        assert!(!staging.exists());
        assert!(!owner.exists());
    }

    #[test]
    fn live_migration_staging_survives_reclaim_until_abandoned() {
        let parent = tempfile::TempDir::new().unwrap();
        let destination = parent.path().join("destination");
        let claim = claim_staging(parent.path()).unwrap();
        std::fs::create_dir(&claim.staging).unwrap();

        reclaim_dead_staging(parent.path(), &destination).unwrap();
        assert!(claim.staging.is_dir());
        assert!(claim.owner.is_file());

        claim.abandon();
        assert!(!claim.staging.exists());
        assert!(!claim.owner.exists());
    }

    #[test]
    fn installed_staging_owner_marker_is_reclaimed_alone() {
        let parent = tempfile::TempDir::new().unwrap();
        let destination = parent.path().join("destination");
        let claim = claim_staging(parent.path()).unwrap();
        let owner = claim.owner.clone();
        // The copy was renamed to its destination before its marker was retired.
        std::fs::create_dir(&destination).unwrap();
        die_and_reclaim(claim, parent.path(), &destination);
        assert!(!owner.exists());
        assert!(destination.is_dir());
    }

    #[test]
    fn reclaim_never_removes_a_destination_named_like_staging() {
        let parent = tempfile::TempDir::new().unwrap();
        let name = format!("{STAGING_PREFIX}{}", crate::Uuid::new_v4());
        let destination = parent.path().join(&name);
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("user-data"), b"keep").unwrap();
        std::fs::write(parent.path().join(format!("{name}{OWNER_SUFFIX}")), b"").unwrap();

        reclaim_dead_staging(parent.path(), &destination).unwrap();
        assert!(destination.join("user-data").is_file());
    }

    #[test]
    fn unowned_migration_staging_is_never_reclaimed() {
        let parent = tempfile::TempDir::new().unwrap();
        let destination = parent.path().join("destination");
        let staging = parent
            .path()
            .join(format!("{STAGING_PREFIX}{}", crate::Uuid::new_v4()));
        std::fs::create_dir(&staging).unwrap();

        reclaim_dead_staging(parent.path(), &destination).unwrap();
        assert!(staging.is_dir());
    }

    #[test]
    fn owner_markers_not_named_by_this_module_are_never_reclaimed() {
        let parent = tempfile::TempDir::new().unwrap();
        let destination = parent.path().join("destination");
        // The same UUID, spelled in a form `claim_staging` never writes.
        let spelled = crate::Uuid::new_v4().to_string().to_uppercase();
        let name = format!("{STAGING_PREFIX}{spelled}");
        let staging = parent.path().join(&name);
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(parent.path().join(format!("{name}{OWNER_SUFFIX}")), b"").unwrap();

        reclaim_dead_staging(parent.path(), &destination).unwrap();
        assert!(staging.is_dir());
    }
}

#[cfg(test)]
mod git_metadata_tests {
    use super::*;

    const SPELLINGS: [&str; 4] = [".git", ".GIT", "git~1", ".git. "];

    fn digest_and_entries(root: &Path) -> ([u8; 32], usize) {
        let mut hash = Sha256::new();
        let mut entries = 0;
        hash_working_tree(root, Path::new(""), &mut hash, &mut entries).unwrap();
        (hash.finalize().into(), entries)
    }

    #[test]
    fn clear_checkout_retains_git_metadata_in_every_spelling() {
        let temp = tempfile::tempdir().unwrap();
        for name in SPELLINGS {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        std::fs::write(temp.path().join("tracked.txt"), b"checkout").unwrap();
        clear_checkout(temp.path()).unwrap();
        for name in SPELLINGS {
            assert!(temp.path().join(name).is_dir(), "{name:?} must be retained");
        }
        assert!(!temp.path().join("tracked.txt").exists());
    }

    #[test]
    fn working_tree_copy_never_copies_git_metadata_in_any_spelling() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for name in SPELLINGS {
            let dir = source.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("config"), b"metadata").unwrap();
        }
        std::fs::write(source.path().join("tracked.txt"), b"payload").unwrap();
        copy_working_tree(source.path(), destination.path()).unwrap();
        assert!(destination.path().join("tracked.txt").is_file());
        for name in SPELLINGS {
            assert!(
                !destination.path().join(name).exists(),
                "{name:?} must not be copied"
            );
        }
    }

    #[test]
    fn working_tree_hash_skips_nested_git_metadata_in_every_spelling() {
        // The exact `.git` spelling is hashed as nested repository state, so only
        // the spellings Git does not treat as metadata are exercised here.
        for name in [".GIT", "git~1", ".git. "] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("tracked.txt"), b"payload").unwrap();
            let deps = root.path().join("deps");
            std::fs::create_dir(&deps).unwrap();
            std::fs::write(deps.join("lib.txt"), b"lib").unwrap();
            let metadata = deps.join(name);
            std::fs::create_dir(&metadata).unwrap();
            std::fs::write(metadata.join("junk"), b"first").unwrap();
            let (before, entries) = digest_and_entries(root.path());
            assert_eq!(entries, 3, "{name:?} must not be counted");
            std::fs::write(metadata.join("junk"), b"second").unwrap();
            let (after, _) = digest_and_entries(root.path());
            assert_eq!(before, after, "{name:?} content must not be hashed");
        }
    }
}
