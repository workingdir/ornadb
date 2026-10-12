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
//! the migration journal — which is what makes the transition identifiable and
//! resumable — but the repository has no legacy-rows-to-store-root converter,
//! so the candidate it binds carries metadata only. A publication must also
//! resolve a valid `.orna/store` root (MIGRATION.md steps 4 to 6), and
//! `validate_store_root` is what refuses a candidate that has none. Until that
//! converter exists, the returned journal describes a migration whose store
//! conversion is still outstanding; it never claims an admission that the
//! publication boundary would reject.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::init::format_context::{LEGACY_FORMAT_PATH, canonical_database_bytes};
use crate::{
    DatabaseId, GitCommitRef, IndexGeneration, LegacyAnnotationCandidate,
    MigrationAnnotationDefaults, MigrationContinuityRecord, PublicationJournal,
    PublicationJournalEntry, Repository, RepositoryError, RepositoryFormat,
};

/// The one frozen content format an unannotated legacy row transitions to
/// during an explicit migration. UPGRADE-004 fixes this coordinate.
pub const LEGACY_ANNOTATION_DEFAULT_FORMAT: &str = "MIME-1";

/// The Git trailer key a journalled migration commit uses to name its own
/// format coordinate. Git only recognizes a trailer after a blank line, so the
/// candidate message keeps the summary, a blank line, then this key.
pub const MIGRATION_MARKER_TRAILER: &str = "Orna-Migration";

/// The copy-local record of the coordinates a prepared migration carries.
pub const MIGRATION_RECORD: &str = ".orna/migration.json";

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
/// `continuity` and `annotation_defaults` are the coordinates the conversion
/// established. Neither is inferred here: a format-1/2 commit this module can
/// read records no row inventory, so the annotation default is a declared
/// input and [`LegacyAnnotationInventory`] is where a caller with a legacy
/// decoder derives it.
pub fn prepare_format3_migration(
    source: &Repository,
    destination: &Path,
    continuity: MigrationContinuityRecord,
    annotation_defaults: MigrationAnnotationDefaults,
) -> Result<MigratedCopy, RepositoryError> {
    let destination = canonical_destination(destination)?;
    let source_root = source
        .worktree()
        .canonicalize()
        .map_err(|_| RepositoryError::NotAWorktree)?;
    if destination == source_root || destination.starts_with(&source_root) {
        return Err(RepositoryError::InvalidSelector);
    }
    if path_exists(&destination)? {
        return recover_prepared_copy(source, &destination, continuity, annotation_defaults);
    }

    let staging = migration_staging_path(&destination)?;
    let result = (|| {
        let mut copy =
            prepare_format3_migration_in(source, &staging, continuity, annotation_defaults)?;
        let repository = Repository::discover(&staging)?;
        pin_migration_index(
            &repository,
            copy.source_index_generation
                .tree()
                .ok_or(RepositoryError::InvalidFormatMigration)?
                .as_str(),
        )?;
        pin_migration_candidate(&repository, copy.candidate_journal.new_head())?;
        repository.write_publication_journal(&copy.candidate_journal)?;
        sync_tree(&staging)?;
        let parent = destination
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        crate::init::publish_directory_noreplace(parent, &staging, &destination).map_err(
            |error| match error {
                crate::RepositoryInitError::MetadataChanged => {
                    RepositoryError::PublicationRecoveryConflict
                }
                crate::RepositoryInitError::PlatformUnsupported => {
                    RepositoryError::PlatformUnsupported
                }
                _ => RepositoryError::LocalStateUnavailable,
            },
        )?;
        copy.destination = destination;
        Ok(copy)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn prepare_format3_migration_in(
    source: &Repository,
    destination: &Path,
    continuity: MigrationContinuityRecord,
    annotation_defaults: MigrationAnnotationDefaults,
) -> Result<MigratedCopy, RepositoryError> {
    let context = source
        .open_format_context()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    if context.supports_writes() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    if annotation_defaults.payload_inspected() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    let source_format = context.repository_format();
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

    if source.head()?.as_ref() != Some(&source_commit)
        || source.index_generation()? != source_index_generation
        || working_tree_identity(source)?.0 != source_working_tree_identity
    {
        return Err(RepositoryError::PublicationRecoveryConflict);
    }

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
    let record = migration_record(
        source_format,
        &source_commit,
        &source_tree_identity,
        annotation_defaults,
        &continuity,
    );
    let candidate = copy.build_private_commit(
        &source_commit,
        &[
            crate::ManagedFileChange::new(
                crate::ManagedPath::new(".orna/database.orna")?,
                Some(database_bytes.clone()),
            ),
            crate::ManagedFileChange::new(crate::ManagedPath::new(LEGACY_FORMAT_PATH)?, None),
            crate::ManagedFileChange::new(
                crate::ManagedPath::new(MIGRATION_RECORD)?,
                Some(record.clone()),
            ),
        ],
        &candidate_message(source_format, annotation_defaults, &continuity),
    )?;
    let candidate_journal = PublicationJournal::new_with_runtime_intent(
        source_commit.clone(),
        candidate.commit().clone(),
        source_index_generation
            .tree()
            .cloned()
            .ok_or(RepositoryError::InvalidFormatMigration)?,
        fresh_runtime_intent_id()?,
        vec![
            PublicationJournalEntry::new(
                crate::ManagedPath::new(".orna/database.orna")?,
                None,
                Some(database_bytes),
            ),
            PublicationJournalEntry::new(
                crate::ManagedPath::new(LEGACY_FORMAT_PATH)?,
                Some(legacy_format_bytes(source_format)),
                None,
            ),
            PublicationJournalEntry::new(
                crate::ManagedPath::new(MIGRATION_RECORD)?,
                None,
                Some(record),
            ),
        ],
    )?
    .with_migration_continuity(continuity.clone())?
    .with_migration_annotation_defaults(annotation_defaults)?;

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

fn migration_staging_path(destination: &Path) -> Result<PathBuf, RepositoryError> {
    let parent = destination
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    Ok(parent.join(format!(".orna-migration-copy-{}", crate::Uuid::new_v4())))
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
    crate::scrub_git_routing_environment(&mut command);
    if command
        .status()
        .map_err(|_| RepositoryError::GitUnavailable)?
        .success()
    {
        return Ok(());
    }
    let mut verify = git_command(root);
    verify.args(["rev-parse", "--verify", &reference]);
    crate::scrub_git_routing_environment(&mut verify);
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
    crate::scrub_git_routing_environment(&mut command);
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
    crate::scrub_git_routing_environment(&mut command);
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
    crate::scrub_git_routing_environment(&mut command);
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
    crate::scrub_git_routing_environment(&mut command);
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if !output.status.success() || crate::trim_output(&output.stdout) != commit.as_str() {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    Ok(())
}

fn recover_prepared_copy(
    source: &Repository,
    destination: &Path,
    continuity: MigrationContinuityRecord,
    annotation_defaults: MigrationAnnotationDefaults,
) -> Result<MigratedCopy, RepositoryError> {
    let source_context = source
        .open_format_context()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    if source_context.supports_writes()
        || !matches!(
            source_context.repository_format(),
            RepositoryFormat::Legacy1 | RepositoryFormat::Legacy2
        )
        || annotation_defaults.payload_inspected()
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
    let candidate_context = repository
        .open_private_candidate_format_context(&candidate)
        .map_err(|_| RepositoryError::InvalidFormatMigration)?;
    let database_id = candidate_context
        .database_id()
        .ok_or(RepositoryError::InvalidFormatMigration)?;
    if candidate_context.repository_format() != RepositoryFormat::Format3 {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    let expected_record = migration_record(
        source_format,
        &source_commit,
        &source_tree_identity,
        annotation_defaults,
        &continuity,
    );
    let expected_entries = vec![
        PublicationJournalEntry::new(
            crate::ManagedPath::new(".orna/database.orna")?,
            None,
            Some(canonical_database_bytes(&database_id)),
        ),
        PublicationJournalEntry::new(
            crate::ManagedPath::new(LEGACY_FORMAT_PATH)?,
            Some(legacy_format_bytes(source_format)),
            None,
        ),
        PublicationJournalEntry::new(
            crate::ManagedPath::new(MIGRATION_RECORD)?,
            None,
            Some(expected_record),
        ),
    ];
    if journal.entries() != expected_entries {
        return Err(RepositoryError::InvalidFormatMigration);
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
        crate::scrub_git_routing_environment(&mut command);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
    }
    Ok(())
}

/// The recorded `format.orna` bytes of one legacy source.
fn legacy_format_bytes(format: RepositoryFormat) -> Vec<u8> {
    format!("format {}\n", format.number()).into_bytes()
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
    crate::scrub_git_routing_environment(&mut command);
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
        if entry.file_name() == ".git" {
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
    crate::scrub_git_routing_environment(&mut command);
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
    crate::scrub_git_routing_environment(&mut command);
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
        crate::scrub_git_routing_environment(&mut exists);
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
        crate::scrub_git_routing_environment(&mut read);
        let mut read = read.spawn().map_err(|_| RepositoryError::GitUnavailable)?;
        let mut write = git_command(destination);
        write
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        crate::scrub_git_routing_environment(&mut write);
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
        if name == ".git" {
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

fn fresh_runtime_intent_id() -> Result<[u8; 16], RepositoryError> {
    let uuid = crate::Uuid::new_v4().into_bytes();
    if uuid == [0; 16] {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    Ok(uuid)
}

/// Settles the copy's index, repairing the stat entries its own checkout left
/// behind so the migration does not republish the source's block transfers.
fn settle_index(root: &Path) -> Result<(), RepositoryError> {
    let mut command = git_command(root);
    command.args(["update-index", "--refresh", "--quiet"]);
    crate::scrub_git_routing_environment(&mut command);
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
    crate::scrub_git_routing_environment(&mut head);
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

/// A Git command bound to one directory, before routing is scrubbed.
fn git_command(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(directory);
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
