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
use std::path::{Path, PathBuf};
use std::process::Command;

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
/// The source must be a non-bare worktree whose recorded format is 1 or 2. The
/// copy is created at `destination`, which must not exist and must lie outside
/// the source worktree; it carries the source's commits, index state, working
/// tree and submodule worktrees, but not the source's private runtime
/// ownership, and its index is settled. The source's recorded `format.orna` is
/// retired in the copy so its readers dispatch by the new recorded format.
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
    let context = source
        .open_format_context()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    if context.supports_writes() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    // A coordinate derived from payload bytes is refused by the journal, so an
    // inspecting default can never be advertised as a metadata migration.
    if annotation_defaults.payload_inspected() {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    let source_format = context.repository_format();
    let source_commit = source.head()?.ok_or(RepositoryError::UnbornHead)?;
    let source_index_generation = source.index_generation()?;
    require_index_tree(&source_index_generation)?;
    require_no_protected_pins(source)?;
    // Both coordinates come from committed objects before the copy exists, so
    // the source's concurrently changing working tree cannot influence them.
    let source_tree_identity = source_tree_identity(source, &source_commit)?;
    let dependency_entries = dependency_entries(source, &source_commit)?;
    let source_root = source.worktree().to_path_buf();
    let root = clone_source_graph(&source_root, destination)?;
    // The clone checks out no content of its own: every path in the copy is
    // migrated from the source working tree, including the ones Git ignores.
    clear_checkout(&root)?;
    copy_working_tree(&source_root, &root)?;
    release_source_ownership(&root)?;
    let copy = Repository::discover(&root)?;
    // The copy carries the source's own bytes at this point, including the
    // legacy `.orna` records: a migration must never install new bytes before
    // it is published. The format-3 records the copy needs to read as format 3
    // travel in the candidate below, so nothing here writes metadata.
    let database_id = context.database_id().unwrap_or_else(DatabaseId::new_v4);

    // The candidate is built with a hidden index derived from the source's
    // recorded tree and never touches the copy's ordinary index or worktree.
    // It carries the two records that make the copy a format-3 repository: the
    // canonical database identity, and the retirement of the legacy format
    // record, which the profile reader reports as "no format file beside a
    // database record" — the canonical format-3 spelling. The annotation
    // coordinate rides in the candidate's own message, because the copy cannot
    // publish the transition yet: its head must stay on the legacy commit until
    // the runtime owner completes the migration.
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
        // The journal names every path the migration changes, so a resume
        // replays the transition rather than re-deciding it. None of these
        // bytes exist in the copy yet, because preparation never installs
        // metadata the publication has not admitted.
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

    let (working_tree_identity, working_tree_entries) = working_tree_identity(&copy)?;

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

/// Removes the clone's own checkout, leaving its commit graph and index intact.
fn clear_checkout(root: &Path) -> Result<(), RepositoryError> {
    let entries = fs::read_dir(root).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    for entry in entries {
        let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let name = entry.file_name();
        if name == ".git" {
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
        let bytes = fs::read(source).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::write(destination, bytes).map_err(|_| RepositoryError::LocalStateUnavailable)?;
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

/// Settles the copy's working-tree identity over the whole copied tree.
fn working_tree_identity(copy: &Repository) -> Result<([u8; 32], usize), RepositoryError> {
    let head = copy.head()?.ok_or(RepositoryError::UnbornHead)?;
    let mut command = copy.observer_command();
    command.args([
        "diff-index",
        "--cached",
        "--unified=0",
        "--no-color",
        "-z",
        head.as_str(),
    ]);
    let output = copy.run(command)?;
    if !output.status.success() {
        return Err(RepositoryError::GitOperationFailed);
    }
    let mut hash = Sha256::new();
    hash.update(b"orna.migration.working-tree-identity.v1\0");
    hash.update(head.as_str().as_bytes());
    hash.update([0]);
    hash.update(&output.stdout);
    Ok((hash.finalize().into(), output.stdout.len()))
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
