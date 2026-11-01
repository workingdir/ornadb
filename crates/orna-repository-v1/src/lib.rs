//! Git-valid repository state for Orna 1.0.
//!
//! This crate intentionally models the repository boundary. It can initialize
//! canonical `.orna/` metadata, but never advances an ordinary ref. In particular,
//! [`Repository::stage_managed`] delegates to Git's ordinary index and touches
//! only the supplied worktree-relative paths.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt, fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

#[cfg(target_os = "linux")]
use std::{
    ffi::{OsStr, OsString},
    mem::MaybeUninit,
    os::unix::ffi::OsStrExt,
};

#[cfg(target_os = "linux")]
use rustix::fs::StatExt;

use fs2::FileExt;
use sha2::{Digest, Sha256};
pub use uuid::Uuid;

pub mod allocator;
mod blob_store;
mod crash_barrier;
mod compact;
pub mod complete_copy;
pub mod format2_orr;
pub mod format2_rov;
pub mod format2_ogb;
pub mod format2_read;
mod init;
pub mod migration_copy;
mod mount;
mod native_graph;
mod object_reader;
pub mod offline_copy;
mod orp;
mod overflow;
pub mod publication_describe;
mod publication_transaction;
pub mod relation_admission;
mod row_store;
pub mod schema_evolution;
#[cfg(test)]
pub(crate) mod test_support;
mod transport;

pub use blob_store::ContentIdentity;
pub use compact::{
    COMPACT_MANIFEST_SHARD_LIMIT, COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES, CompactCommittedRow,
    CompactCommittedSegmentProjection, CompactManifest, CompactManifestEntry,
    CompactManifestWitness, CompactPublicationPending, CompactRuntimeReceipt, CompactSegmentRole,
    LegacyAuthoritativeRow, LegacyAuthoritativeTable, validate_compact_page_uncompressed_sizes,
};
#[cfg(any(test, feature = "legacy-fixture-writer"))]
pub use compact::{CompactPublicationPlan, CompactSegment};
#[cfg(feature = "legacy-fixture-writer")]
pub use compact::write_legacy_format1_fixture;
pub use init::format_context::{
    FINAL_REPOSITORY_FORMAT, REPOSITORY_LAYOUT_VERSION, CaptureCapabilityError,
    CommittedTableBinding, FinalFormatCapability, FormatContextError, HistoricalFormatContext,
    LegacyWriteRefused, RepositoryFormat, RepositoryFormatContext, RepositorySnapshotPin,
    SchemaRootPin, StoreRootPin,
};
pub use init::{
    STORAGE_PROFILE, DatabaseId, RepositoryInitError, RepositoryInitialization, RepositoryMetadata,
    inspect_metadata, inspect_metadata_for_runtime, initialize_repository,
};
pub use mount::{MountError, MountRowBaselines, MountStatus, MountView};
pub use native_graph::RowRevision;
pub use native_graph::{
    AdmittedBlobReference, AdmittedRowField, AdmittedRowFields, CapturedBlobCandidate,
    GitHashAlgorithm, GraphError, IndexMaintenance, NativeGraphContext, NativeObjectKind,
    NativeOid, OrpBlobBinding, OrpGraphCandidate, ProtectedBlobMetadata, ProtectedContentPin,
    ProtectedContentTransfer, Pub3ReleaseReceipt, RangeVerification, RepositoryCaptureCapability,
    RepositoryReadScope, RowIndexMutation, VerifiedBlobRange,
};
pub use orna_value_v1::BlobMetadata;
pub use publication_transaction::{
    ProtectedBlobRowInsert, PublicationTransactionError, commit_protected_blob_row,
};
pub use crash_barrier::publication_crash_barrier;
pub use row_store::{
    AdmittedRow, KeyRange, KeyValue, RepositoryVfsFieldIdentity, RepositoryVfsRowIdentity,
    RowDependency, RowMapSnapshot, RowMapVersion, RowValue, SchemaGeneration, TypedKey,
};
pub use transport::{FetchError, FetchReport, FetchRequest, FetchedRef, PushRequest, RequestedRef};

/// A verified native Git commit ID. It is intentionally Git-local: the
/// shared foundation owns the portable Orna `SnapshotRef` row identity and
/// canonical committed snapshot representation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitCommitRef(String);

impl GitCommitRef {
    fn from_verified_commit(
        value: String,
        object_id_length: usize,
    ) -> Result<Self, RepositoryError> {
        if value.len() != object_id_length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RepositoryError::InvalidObjectId);
        }
        Ok(Self(value))
    }

    /// The object ID in Git's native hexadecimal spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GitCommitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One record discovered from the reachable Git commit history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedLogEntry {
    commit: GitCommitRef,
    committed_at: String,
    author: String,
    subject: String,
}

impl CommittedLogEntry {
    /// The immutable Git commit represented by this history row.
    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }

    /// The commit timestamp in Git's ISO 8601 spelling.
    pub fn committed_at(&self) -> &str {
        &self.committed_at
    }

    /// The author recorded in the Git commit.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// The first line of the commit message.
    pub fn subject(&self) -> &str {
        &self.subject
    }
}

/// One local Git branch and the commit currently named by its ref.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedBranch {
    name: String,
    commit: GitCommitRef,
}

impl CommittedBranch {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }
}

/// One recursively listed entry in an immutable committed Git tree.
///
/// This deliberately retains only the repository-relative path and Git entry
/// kind. Callers that need bytes must use [`Repository::read_committed_file`]
/// so each body has an explicit size bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTreeEntry {
    path: ManagedPath,
    kind: CommittedTreeEntryKind,
}

impl CommittedTreeEntry {
    /// The normalized repository-relative path of this entry.
    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    /// The entry's Git object kind and supported file mode.
    pub const fn kind(&self) -> CommittedTreeEntryKind {
        self.kind
    }
}

/// The Git tree entry kinds a committed snapshot loader must classify.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommittedTreeEntryKind {
    /// An ordinary regular file. `executable` records Git's executable bit.
    File { executable: bool },
    /// A symbolic-link blob, which snapshot source loaders must not follow.
    Symlink,
    /// A gitlink/submodule entry, which is not source content in this tree.
    Submodule,
}

/// A verified Git tree ID produced from the ordinary Git index.
///
/// This is intentionally distinct from [`GitCommitRef`]: a tree must never be
/// accepted where a committed historical database snapshot is required.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IndexTreeRef(String);

impl IndexTreeRef {
    fn from_verified_tree(value: String, object_id_length: usize) -> Result<Self, RepositoryError> {
        if value.len() != object_id_length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RepositoryError::InvalidObjectId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One managed file entry for a private publication candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedFileChange {
    path: ManagedPath,
    bytes: Option<Vec<u8>>,
}

impl ManagedFileChange {
    pub fn new(path: ManagedPath, bytes: Option<Vec<u8>>) -> Self {
        Self { path, bytes }
    }

    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }
}

/// The Git objects for a candidate publication.  The candidate is private:
/// building it never changes the ordinary index, worktree, or ref namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateCommit {
    tree: IndexTreeRef,
    commit: GitCommitRef,
}

/// Refuses a private candidate commit before it becomes a commit. The
/// repository crate cannot evaluate table or module assertions itself, so each
/// reviewed commit path (`commit_reviewed_candidate` and its stage-hook variant)
/// asks this gate over the serialized candidate before accepting it
/// (ORNA-ASSERT-042). The publish entry points (`publish_candidate`,
/// `publish_candidate_with_stage_hook`, `publish_legacy_format_migration`) take
/// no gate: the CLI runs its gate before calling them, so an assertion is
/// evaluated before graph publication (ORNA-ASSERT-045). The production
/// implementation is `AssertionGate` in `orna-cli-v1`'s `assertion_gate`
/// module; the test-only `AcceptAllAssertions` accepts without evaluating them.
pub trait CandidateAssertionGate {
    fn validate(
        &self, repository: &Repository, candidate: &PrivateCommit,
    ) -> Result<(), CandidateAssertionRefusal>;
}

/// Why the assertion gate refused a candidate commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateAssertionRefusal {
    /// A table or module assertion evaluated false over the serialized candidate.
    /// Carries the module assertion's site; a table assertion carries none.
    AssertionFalse(Option<String>),
    /// Validation could not complete, so the candidate is refused whole. The
    /// string is the cause, which the E2300 title carries (ORNA-ASSERT-049).
    Unvalidated(String),
}

/// Accepts every candidate. Used only by the repository's commit-path tests
/// (`lib.rs` and `relation_admission.rs`), which stage no assertion.
#[cfg(test)]
pub(crate) struct AcceptAllAssertions;

#[cfg(test)]
impl CandidateAssertionGate for AcceptAllAssertions {
    fn validate(&self, _: &Repository, _: &PrivateCommit) -> Result<(), CandidateAssertionRefusal> {
        Ok(())
    }
}

/// Opaque, repository-local review of one exact staged index generation.
/// Construction is restricted to this crate so callers cannot forge a review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewedCandidateReview {
    database_id: Uuid,
    runtime_id: Uuid,
    pending_generation: u64,
    repository_format: RepositoryFormat,
    head_ref: Option<String>,
    cwd: CwdGeneration,
    cwd_snapshot_descriptor: Vec<u8>,
    candidate_paths: Vec<String>,
    index_bytes_digest: [u8; 32],
    index_tree: IndexTreeRef,
    token: [u8; 32],
}

impl ReviewedCandidateReview {
    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub fn index_tree(&self) -> &IndexTreeRef {
        &self.index_tree
    }

    pub const fn pending_generation(&self) -> u64 {
        self.pending_generation
    }
}
fn append_cbor_head(bytes: &mut Vec<u8>, major: u8, value: u64) {
    let prefix = major << 5;
    match value {
        0..=23 => bytes.push(prefix | value as u8),
        24..=0xff => bytes.extend_from_slice(&[prefix | 24, value as u8]),
        0x100..=0xffff => { bytes.push(prefix | 25); bytes.extend_from_slice(&(value as u16).to_be_bytes()); }
        0x1_0000..=0xffff_ffff => { bytes.push(prefix | 26); bytes.extend_from_slice(&(value as u32).to_be_bytes()); }
        _ => { bytes.push(prefix | 27); bytes.extend_from_slice(&value.to_be_bytes()); }
    }
}

fn append_cbor_bytes(bytes: &mut Vec<u8>, value: &[u8]) { append_cbor_head(bytes, 2, value.len() as u64); bytes.extend_from_slice(value); }
fn append_cbor_text(bytes: &mut Vec<u8>, value: &str) { append_cbor_head(bytes, 3, value.len() as u64); bytes.extend_from_slice(value.as_bytes()); }
fn append_cbor_optional_text(bytes: &mut Vec<u8>, value: Option<&str>) { match value { Some(value) => append_cbor_text(bytes, value), None => bytes.push(0xf6) } }

fn reviewed_candidate_token(review: &ReviewedCandidateReview) -> [u8; 32] {
    let mut payload = Vec::with_capacity(256 + review.cwd_snapshot_descriptor.len());
    append_cbor_head(&mut payload, 4, 11);
    append_cbor_bytes(&mut payload, review.database_id.as_bytes());
    append_cbor_bytes(&mut payload, review.runtime_id.as_bytes());
    append_cbor_optional_text(&mut payload, review.cwd.head().map(GitCommitRef::as_str));
    append_cbor_optional_text(&mut payload, review.head_ref.as_deref());
    append_cbor_text(&mut payload, review.index_tree.as_str());
    append_cbor_bytes(&mut payload, &review.index_bytes_digest);
    append_cbor_head(&mut payload, 4, review.candidate_paths.len() as u64);
    for path in &review.candidate_paths { append_cbor_text(&mut payload, path); }
    append_cbor_bytes(&mut payload, &review.cwd_snapshot_descriptor);
    append_cbor_head(&mut payload, 0, review.pending_generation);
    append_cbor_head(&mut payload, 0, u64::from(review.repository_format.number()));
    let mut digest = Sha256::new();
    digest.update(b"orna.commit-review-worktree.v1\0");
    digest.update(payload);
    digest.finalize().into()
}
fn reviewed_candidate_runtime_intent_id(review: &ReviewedCandidateReview, message: &str) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(b"orna.reviewed-candidate-runtime-intent.v1\0");
    digest.update(review.database_id.as_bytes());
    digest.update(review.runtime_id.as_bytes());
    let tree = review.index_tree.as_str();
    digest.update((tree.len() as u64).to_be_bytes());
    digest.update(tree.as_bytes());
    digest.update(review.pending_generation.to_be_bytes());
    digest.update(Sha256::digest(message.as_bytes()));
    let digest: [u8; 32] = digest.finalize().into();
    let mut intent = [0; 16];
    intent.copy_from_slice(&digest[..16]);
    intent
}

/// The commit gate's refusal as the commit's failure. A candidate that mixes
/// formats keeps its format error, which names the tracked path that does not
/// belong to format 3 (ORNA-META-001, ORNA-REPO-CONTEXT-004); any other
/// invalid candidate is `InvalidCommitCandidate` (`sys.admin.assertion_failed`).
fn commit_gate_refusal(error: FormatContextError) -> RepositoryError {
    match error {
        mixed @ FormatContextError::MixedFormats { .. } => RepositoryError::FormatContext(mixed),
        _ => RepositoryError::InvalidCommitCandidate,
    }
}

const JOURNAL_MAGIC: &[u8] = b"ORNA-PUB-JOURNAL\0";
const CHECKOUT_JOURNAL_MAGIC: &[u8] = b"ORNA-CHECKOUT-JOURNAL\0";
const GIT_INDEX_LOCK_MAGIC: &[u8] = b"ORNA-GIT-INDEX-LOCK\0";
const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_MIGRATION_IDENTITY_RECORDS: usize = 65_536;
/// Longest a publication waits for the repository coordination lock before it
/// reports `RepositoryBusy`. Bounded so a hung holder or a same-thread
/// re-entry cannot block publication forever; long enough to queue behind any
/// short coordination section that holds the lock.
const COORDINATION_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Sleep between try-lock attempts while the coordination lock is contended.
const COORDINATION_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(10);
const MAX_MIGRATION_IDENTITY_COMPONENT_BYTES: usize = 1024 * 1024;
const MAX_MIGRATION_ANNOTATION_DEFAULTS: usize = 1_000_000_000;

/// The one canonical MIME-1 media type a legacy Blob row takes when its old
/// annotation is not representable as a format-3 annotated value. UPGRADE-004
/// fixes this default; a migration never chooses another one.
pub const LEGACY_ANNOTATION_DEFAULT_MEDIA_TYPE: &str = "application/octet-stream";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationMaterializationPhase {
    Clean,
    QuarantinePrepared,
    Quarantined,
    Installed,
}

impl PublicationMaterializationPhase {
    fn code(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::QuarantinePrepared => 1,
            Self::Quarantined => 2,
            Self::Installed => 3,
        }
    }

    fn from_code(code: u8) -> Result<Self, RepositoryError> {
        match code {
            0 => Ok(Self::Clean),
            1 => Ok(Self::QuarantinePrepared),
            2 => Ok(Self::Quarantined),
            3 => Ok(Self::Installed),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicationMaterialization {
    phase: PublicationMaterializationPhase,
    quarantine: Option<String>,
}

impl PublicationMaterialization {
    const fn clean() -> Self {
        Self {
            phase: PublicationMaterializationPhase::Clean,
            quarantine: None,
        }
    }
}

/// The recovery stages persisted for one publication attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationJournalStage {
    Prepared,
    RefAdvanced,
    IndexReconciled,
    WorktreeReconciled,
    RuntimeCompleted,
    Complete,
}

impl PublicationJournalStage {
    fn code(self) -> u8 {
        match self {
            Self::Prepared => 1,
            Self::RefAdvanced => 2,
            Self::IndexReconciled => 3,
            Self::WorktreeReconciled => 4,
            Self::RuntimeCompleted => 5,
            Self::Complete => 6,
        }
    }

    fn from_code(code: u8) -> Result<Self, RepositoryError> {
        match code {
            1 => Ok(Self::Prepared),
            2 => Ok(Self::RefAdvanced),
            3 => Ok(Self::IndexReconciled),
            4 => Ok(Self::WorktreeReconciled),
            5 => Ok(Self::RuntimeCompleted),
            6 => Ok(Self::Complete),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }
}

/// One managed path's captured and desired worktree bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationJournalEntry {
    path: ManagedPath,
    expected: Option<Vec<u8>>,
    next: Option<Vec<u8>>,
    materialization: PublicationMaterialization,
}

impl PublicationJournalEntry {
    pub fn new(path: ManagedPath, expected: Option<Vec<u8>>, next: Option<Vec<u8>>) -> Self {
        Self {
            path,
            expected,
            next,
            materialization: PublicationMaterialization::clean(),
        }
    }

    pub fn path(&self) -> &ManagedPath {
        &self.path
    }

    pub fn expected(&self) -> Option<&[u8]> {
        self.expected.as_deref()
    }

    pub fn next(&self) -> Option<&[u8]> {
        self.next.as_deref()
    }

    fn materialization(&self) -> &PublicationMaterialization {
        &self.materialization
    }
}

/// One checkpoint natural-key predecessor across the legacy-to-format-3
/// consumer identity transition. Source and partition values are their exact
/// canonical typed identity bytes; `None` remains distinct from every encoded
/// partition value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StreamCheckpointIdentityPredecessor {
    legacy_consumer_identity: [u8; 32],
    legacy_source_identity: Vec<u8>,
    legacy_partition_identity: Option<Vec<u8>>,
    format3_consumer_identity: [u8; 32],
    format3_source_identity: Vec<u8>,
    format3_partition_identity: Option<Vec<u8>>,
}

impl StreamCheckpointIdentityPredecessor {
    pub fn new(
        legacy_consumer_identity: [u8; 32],
        legacy_source_identity: Vec<u8>,
        legacy_partition_identity: Option<Vec<u8>>,
        format3_consumer_identity: [u8; 32],
        format3_source_identity: Vec<u8>,
        format3_partition_identity: Option<Vec<u8>>,
    ) -> Result<Self, RepositoryError> {
        validate_migration_identity_component(&legacy_source_identity, false)?;
        validate_migration_identity_component(
            legacy_partition_identity.as_deref().unwrap_or_default(),
            legacy_partition_identity.is_none(),
        )?;
        validate_migration_identity_component(&format3_source_identity, false)?;
        validate_migration_identity_component(
            format3_partition_identity.as_deref().unwrap_or_default(),
            format3_partition_identity.is_none(),
        )?;
        Ok(Self {
            legacy_consumer_identity,
            legacy_source_identity,
            legacy_partition_identity,
            format3_consumer_identity,
            format3_source_identity,
            format3_partition_identity,
        })
    }

    pub const fn legacy_consumer_identity(&self) -> &[u8; 32] {
        &self.legacy_consumer_identity
    }

    pub fn legacy_source_identity(&self) -> &[u8] {
        &self.legacy_source_identity
    }

    pub fn legacy_partition_identity(&self) -> Option<&[u8]> {
        self.legacy_partition_identity.as_deref()
    }

    pub const fn format3_consumer_identity(&self) -> &[u8; 32] {
        &self.format3_consumer_identity
    }

    pub fn format3_source_identity(&self) -> &[u8] {
        &self.format3_source_identity
    }

    pub fn format3_partition_identity(&self) -> Option<&[u8]> {
        self.format3_partition_identity.as_deref()
    }

    fn legacy_key(&self) -> ([u8; 32], Vec<u8>, Option<Vec<u8>>) {
        (
            self.legacy_consumer_identity,
            self.legacy_source_identity.clone(),
            self.legacy_partition_identity.clone(),
        )
    }

    fn format3_key(&self) -> ([u8; 32], Vec<u8>, Option<Vec<u8>>) {
        (
            self.format3_consumer_identity,
            self.format3_source_identity.clone(),
            self.format3_partition_identity.clone(),
        )
    }
}

/// Complete bounded predecessor inventory supplied with an explicit
/// format-1/2 to format-3 publication. An empty inventory is explicit evidence
/// that the migration has no persistent stream checkpoints to carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationContinuityRecord {
    predecessors: Vec<StreamCheckpointIdentityPredecessor>,
}

impl MigrationContinuityRecord {
    pub fn new(
        mut predecessors: Vec<StreamCheckpointIdentityPredecessor>,
    ) -> Result<Self, RepositoryError> {
        if predecessors.len() > MAX_MIGRATION_IDENTITY_RECORDS {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        predecessors.sort();
        let mut encoded_bytes = 5usize; // record version and entry count
        let mut legacy_keys = HashSet::new();
        let mut format3_keys = HashSet::new();
        let mut consumer_forward = BTreeMap::new();
        let mut consumer_reverse = BTreeMap::new();
        for predecessor in &predecessors {
            validate_migration_identity_component(&predecessor.legacy_source_identity, false)?;
            validate_migration_identity_component(
                predecessor
                    .legacy_partition_identity
                    .as_deref()
                    .unwrap_or_default(),
                predecessor.legacy_partition_identity.is_none(),
            )?;
            validate_migration_identity_component(&predecessor.format3_source_identity, false)?;
            validate_migration_identity_component(
                predecessor
                    .format3_partition_identity
                    .as_deref()
                    .unwrap_or_default(),
                predecessor.format3_partition_identity.is_none(),
            )?;
            encoded_bytes = encoded_bytes
                .checked_add(80)
                .and_then(|length| length.checked_add(predecessor.legacy_source_identity.len()))
                .and_then(|length| {
                    length.checked_add(
                        predecessor
                            .legacy_partition_identity
                            .as_ref()
                            .map_or(0, Vec::len),
                    )
                })
                .and_then(|length| length.checked_add(predecessor.format3_source_identity.len()))
                .and_then(|length| {
                    length.checked_add(
                        predecessor
                            .format3_partition_identity
                            .as_ref()
                            .map_or(0, Vec::len),
                    )
                })
                .ok_or(RepositoryError::InvalidFormatMigration)?;
            if encoded_bytes > MAX_JOURNAL_BYTES {
                return Err(RepositoryError::InvalidFormatMigration);
            }
            if !legacy_keys.insert(predecessor.legacy_key())
                || !format3_keys.insert(predecessor.format3_key())
            {
                return Err(RepositoryError::InvalidFormatMigration);
            }
            match consumer_forward.get(&predecessor.legacy_consumer_identity) {
                Some(mapped) if mapped != &predecessor.format3_consumer_identity => {
                    return Err(RepositoryError::InvalidFormatMigration);
                }
                Some(_) => {}
                None => {
                    consumer_forward.insert(
                        predecessor.legacy_consumer_identity,
                        predecessor.format3_consumer_identity,
                    );
                }
            }
            match consumer_reverse.get(&predecessor.format3_consumer_identity) {
                Some(mapped) if mapped != &predecessor.legacy_consumer_identity => {
                    return Err(RepositoryError::InvalidFormatMigration);
                }
                Some(_) => {}
                None => {
                    consumer_reverse.insert(
                        predecessor.format3_consumer_identity,
                        predecessor.legacy_consumer_identity,
                    );
                }
            }
        }
        Ok(Self { predecessors })
    }

    pub fn predecessors(&self) -> &[StreamCheckpointIdentityPredecessor] {
        &self.predecessors
    }

    /// The format-3 successor identity of one legacy checkpoint key.
    ///
    /// A resumed publisher looks up the identity the migration recorded
    /// instead of re-deriving it: re-deriving is exactly the reset this
    /// coordinate exists to prevent. The returned source and partition are the
    /// successor's canonical typed identity bytes, and `None` means the
    /// successor has no partition, which stays distinct from an encoded empty
    /// partition. A legacy key the migration never carried has no successor,
    /// so a caller cannot invent one.
    pub fn successor_for(
        &self,
        legacy_consumer_identity: [u8; 32],
        legacy_source_identity: &[u8],
        legacy_partition_identity: Option<&[u8]>,
    ) -> Option<([u8; 32], &[u8], Option<&[u8]>)> {
        self.predecessors
            .iter()
            .find(|predecessor| {
                predecessor.legacy_consumer_identity == legacy_consumer_identity
                    && predecessor.legacy_source_identity == legacy_source_identity
                    && predecessor.legacy_partition_identity.as_deref() == legacy_partition_identity
            })
            .map(|predecessor| {
                (
                    predecessor.format3_consumer_identity,
                    predecessor.format3_source_identity.as_slice(),
                    predecessor.format3_partition_identity.as_deref(),
                )
            })
    }

    fn encode(&self, bytes: &mut Vec<u8>) -> Result<(), RepositoryError> {
        bytes.push(1); // continuity-record version
        put_u32(bytes, self.predecessors.len())?;
        for predecessor in &self.predecessors {
            bytes.extend_from_slice(&predecessor.legacy_consumer_identity);
            put_migration_identity_component(bytes, &predecessor.legacy_source_identity)?;
            put_optional_bytes(bytes, predecessor.legacy_partition_identity.as_deref())?;
            bytes.extend_from_slice(&predecessor.format3_consumer_identity);
            put_migration_identity_component(bytes, &predecessor.format3_source_identity)?;
            put_optional_bytes(bytes, predecessor.format3_partition_identity.as_deref())?;
            if bytes.len() > MAX_JOURNAL_BYTES {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        }
        Ok(())
    }

    fn decode(bytes: &[u8], cursor: &mut usize) -> Result<Self, RepositoryError> {
        if take_byte(bytes, cursor)? != 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let count = usize::try_from(take_u32(bytes, cursor)?)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if count > MAX_MIGRATION_IDENTITY_RECORDS {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut predecessors = Vec::with_capacity(count);
        for _ in 0..count {
            let legacy_consumer_identity = take_fixed_array::<32>(bytes, cursor)?;
            let legacy_source_identity = take_migration_identity_component(bytes, cursor)?;
            let legacy_partition_identity =
                take_optional_migration_identity_component(bytes, cursor)?;
            let format3_consumer_identity = take_fixed_array::<32>(bytes, cursor)?;
            let format3_source_identity = take_migration_identity_component(bytes, cursor)?;
            let format3_partition_identity =
                take_optional_migration_identity_component(bytes, cursor)?;
            let predecessor = StreamCheckpointIdentityPredecessor::new(
                legacy_consumer_identity,
                legacy_source_identity,
                legacy_partition_identity,
                format3_consumer_identity,
                format3_source_identity,
                format3_partition_identity,
            )
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
            if predecessors
                .last()
                .is_some_and(|previous| previous >= &predecessor)
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            predecessors.push(predecessor);
        }
        Self::new(predecessors).map_err(|_| RepositoryError::InvalidPublicationJournal)
    }
}

/// The reported semantic annotation coordinate of one format-1/2 to format-3
/// migration.
///
/// Legacy rows carry no MIME-1 annotation. Format 3 always annotates a Blob,
/// so the migration assigns one and must say so: the raw bytes are preserved,
/// but the annotated value identity of `entry_count` rows is not. UPGRADE-004
/// fixes the assigned media type and forbids consulting payload bytes, so this
/// record is a count of newly defaulted rows together with an explicit
/// confirmation that no payload was read. Both are required for publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationAnnotationDefaults {
    entry_count: u32,
    payload_inspected: bool,
}

impl MigrationAnnotationDefaults {
    /// Reports `entry_count` legacy rows that take the fixed default
    /// annotation during an explicit migration.
    pub fn new(entry_count: u32) -> Result<Self, RepositoryError> {
        if usize::try_from(entry_count).map_err(|_| RepositoryError::InvalidFormatMigration)?
            > MAX_MIGRATION_ANNOTATION_DEFAULTS
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        Ok(Self {
            entry_count,
            payload_inspected: false,
        })
    }

    /// The number of legacy rows whose annotated value identity changes.
    pub const fn entry_count(self) -> u32 {
        self.entry_count
    }

    /// Whether the migration's defaults were derived from payload bytes.
    /// A conforming migration derives them from metadata alone and reports
    /// `false`; a `true` value is refused rather than reported.
    pub const fn payload_inspected(self) -> bool {
        self.payload_inspected
    }

    /// Derives the reported transition from the legacy rows the migration
    /// actually decoded.
    ///
    /// A row recorded without an annotation takes the fixed UPGRADE-004
    /// default and is counted as a changed annotated value identity. A row
    /// whose recorded legacy annotation is not already a representable
    /// canonical MIME-1 value fails the migration: ORNA-UPGRADE-010 forbids
    /// silently downcasting an unsupported annotated value to bytes. No
    /// payload byte is read here; the inventory is decoded metadata.
    pub fn from_legacy_inventory(
        legacy_rows: &[LegacyAnnotationCandidate],
    ) -> Result<Self, RepositoryError> {
        let mut entry_count = 0u32;
        for row in legacy_rows {
            match row.annotation_media_type() {
                Some(media_type) => {
                    orna_value_v1::MediaAnnotation::new(media_type, row.suffix())
                        .map_err(|_| RepositoryError::InvalidFormatMigration)?;
                }
                None => entry_count = entry_count.saturating_add(1),
            }
        }
        Self::new(entry_count)
    }

    fn encode(self, bytes: &mut Vec<u8>) -> Result<(), RepositoryError> {
        bytes.push(1); // annotation-defaults record version
        put_u32(
            bytes,
            usize::try_from(self.entry_count)
                .map_err(|_| RepositoryError::InvalidFormatMigration)?,
        )?;
        bytes.push(u8::from(self.payload_inspected));
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(())
    }

    fn decode(bytes: &[u8], cursor: &mut usize) -> Result<Self, RepositoryError> {
        if take_byte(bytes, cursor)? != 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let entry_count = take_u32(bytes, cursor)?;
        let payload_inspected = match take_byte(bytes, cursor)? {
            0 => false,
            1 => true,
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let defaults =
            Self::new(entry_count).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if payload_inspected != defaults.payload_inspected {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(defaults)
    }
}

/// One legacy authoritative row's recorded annotation, as decoded from its
/// original format-1/2 value profile. This is metadata only: payload bytes are
/// never read to build it.
///
/// A legacy row that already carries a representable annotation keeps it and
/// is not part of the transition; a row recorded without one takes the fixed
/// UPGRADE-004 default during the migration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyAnnotationCandidate {
    key: Vec<u8>,
    annotation_media_type: Option<String>,
    suffix: Option<String>,
}

impl LegacyAnnotationCandidate {
    pub fn new(
        key: Vec<u8>,
        annotation_media_type: Option<String>,
        suffix: Option<String>,
    ) -> Self {
        Self {
            key,
            annotation_media_type,
            suffix,
        }
    }

    pub fn key(&self) -> &[u8] {
        &self.key
    }

    pub fn annotation_media_type(&self) -> Option<&str> {
        self.annotation_media_type.as_deref()
    }

    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }
}

fn validate_migration_identity_component(
    value: &[u8],
    nullable: bool,
) -> Result<(), RepositoryError> {
    if value.len() > MAX_MIGRATION_IDENTITY_COMPONENT_BYTES || (!nullable && value.is_empty()) {
        return Err(RepositoryError::InvalidFormatMigration);
    }
    Ok(())
}

fn put_migration_identity_component(
    bytes: &mut Vec<u8>,
    value: &[u8],
) -> Result<(), RepositoryError> {
    validate_migration_identity_component(value, false)?;
    put_optional_bytes(bytes, Some(value))
}

fn take_migration_identity_component(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Vec<u8>, RepositoryError> {
    let value = take_optional_migration_identity_component(bytes, cursor)?
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    validate_migration_identity_component(&value, false)
        .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    Ok(value)
}

fn take_optional_migration_identity_component(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    let length = take_u32(bytes, cursor)?;
    if length == u32::MAX {
        return Ok(None);
    }
    let length = usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    if length == 0 || length > MAX_MIGRATION_IDENTITY_COMPONENT_BYTES {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?
        .to_vec();
    *cursor = end;
    Ok(Some(value))
}

/// One stable relation identity's exact logical address change retained for
/// publication recovery and downstream replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationLogicalRowRekey {
    relation_id: [u8; 16],
    old_address: Vec<u8>,
    new_address: Vec<u8>,
}

impl PublicationLogicalRowRekey {
    pub fn new(
        relation_id: [u8; 16],
        old_address: Vec<u8>,
        new_address: Vec<u8>,
    ) -> Result<Self, RepositoryError> {
        crate::row_store::TypedKey::decode_canonical(&old_address)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        crate::row_store::TypedKey::decode_canonical(&new_address)
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
        if old_address == new_address {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Self {
            relation_id,
            old_address,
            new_address,
        })
    }

    pub const fn relation_id(&self) -> [u8; 16] {
        self.relation_id
    }

    pub fn old_address(&self) -> &[u8] {
        &self.old_address
    }

    pub fn new_address(&self) -> &[u8] {
        &self.new_address
    }
}
/// A restart-safe publication record stored in the private runtime area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationJournal {
    old_head: GitCommitRef,
    new_head: GitCommitRef,
    base_index_tree: Option<IndexTreeRef>,
    runtime_intent_id: Option<[u8; 16]>,
    compact_manifest: Option<CompactManifestWitness>,
    store_root: Option<IndexTreeRef>,
    migration_continuity: Option<MigrationContinuityRecord>,
    migration_annotation_defaults: Option<MigrationAnnotationDefaults>,
    logical_rekeys: Vec<PublicationLogicalRowRekey>,
    entries: Vec<PublicationJournalEntry>,
    stage: PublicationJournalStage,
    wire_version: u8,
    binding_version: u8,
}

impl PublicationJournal {
    pub fn new(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        Self::new_with_base_index_tree(old_head, new_head, None, entries)
    }

    pub fn new_with_index_tree(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: IndexTreeRef,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        Self::new_with_base_index_tree(old_head, new_head, Some(base_index_tree), entries)
    }

    pub fn new_with_runtime_intent(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: IndexTreeRef,
        runtime_intent_id: [u8; 16],
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        let mut journal =
            Self::new_with_base_index_tree(old_head, new_head, Some(base_index_tree), entries)?;
        if runtime_intent_id == [0; 16] {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        journal.runtime_intent_id = Some(runtime_intent_id);
        Ok(journal)
    }

    fn new_with_base_index_tree(
        old_head: GitCommitRef,
        new_head: GitCommitRef,
        base_index_tree: Option<IndexTreeRef>,
        entries: Vec<PublicationJournalEntry>,
    ) -> Result<Self, RepositoryError> {
        if entries.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let mut paths = HashSet::new();
        if entries.iter().any(|entry| {
            entry.path.as_path().to_str().is_none() || !paths.insert(entry.path.clone())
        }) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(Self {
            old_head,
            new_head,
            base_index_tree,
            runtime_intent_id: None,
            compact_manifest: None,
            store_root: None,
            migration_continuity: None,
            migration_annotation_defaults: None,
            logical_rekeys: Vec::new(),
            entries,
            stage: PublicationJournalStage::Prepared,
            wire_version: 5,
            binding_version: 5,
        })
    }

    pub fn old_head(&self) -> &GitCommitRef {
        &self.old_head
    }

    pub fn new_head(&self) -> &GitCommitRef {
        &self.new_head
    }

    pub fn base_index_tree(&self) -> Option<&IndexTreeRef> {
        self.base_index_tree.as_ref()
    }

    pub fn entries(&self) -> &[PublicationJournalEntry] {
        &self.entries
    }

    pub const fn runtime_intent_id(&self) -> Option<[u8; 16]> {
        self.runtime_intent_id
    }

    /// Binds a compact manifest identity and all referenced immutable objects
    /// into this restart record. Only the test-only legacy fixture writer
    /// still records one; recovery refuses such journals.
    #[cfg(any(test, feature = "legacy-fixture-writer"))]
    pub fn with_compact_manifest(
        mut self,
        witness: CompactManifestWitness,
    ) -> Result<Self, RepositoryError> {
        witness.validate(None)?;
        self.compact_manifest = Some(witness);
        Ok(self)
    }

    pub fn compact_manifest(&self) -> Option<&CompactManifestWitness> {
        self.compact_manifest.as_ref()
    }
    /// Binds the format-3 store tree selected for index reconciliation and
    /// candidate recovery. Older journal versions remain store-root-free.
    pub fn with_store_root(mut self, store_root: &NativeOid) -> Result<Self, RepositoryError> {
        let store_root = store_root.to_hex();
        if self.store_root.is_some()
            || self.stage != PublicationJournalStage::Prepared
            || store_root.len() != self.new_head.as_str().len()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        self.store_root = Some(IndexTreeRef(store_root));
        self.wire_version = self.wire_version.max(8);
        self.binding_version = self.binding_version.max(8);
        Ok(self)
    }

    pub fn store_root(&self) -> Option<&IndexTreeRef> {
        self.store_root.as_ref()
    }
    /// Retains exact rekey identities in the restart journal for recovery and replay.
    pub fn with_logical_rekeys(
        mut self,
        logical_rekeys: Vec<PublicationLogicalRowRekey>,
    ) -> Result<Self, RepositoryError> {
        if self.runtime_intent_id.is_none()
            || self.stage != PublicationJournalStage::Prepared
            || !self.logical_rekeys.is_empty()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if logical_rekeys.is_empty() {
            return Ok(self);
        }
        self.logical_rekeys = logical_rekeys;
        self.wire_version = self.wire_version.max(9);
        self.binding_version = self.binding_version.max(9);
        Ok(self)
    }

    pub fn logical_rekeys(&self) -> &[PublicationLogicalRowRekey] {
        &self.logical_rekeys
    }

    /// Binds the complete stream/checkpoint predecessor inventory to an
    /// explicit legacy-format migration journal. This promotes only that
    /// journal to wire version 6; ordinary publication remains version 5.
    pub fn with_migration_continuity(
        mut self,
        continuity: MigrationContinuityRecord,
    ) -> Result<Self, RepositoryError> {
        if self.migration_continuity.is_some()
            || self.compact_manifest.is_some()
            || self.stage != PublicationJournalStage::Prepared
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        self.migration_continuity = Some(continuity);
        self.wire_version = self.wire_version.max(6);
        self.binding_version = self.binding_version.max(6);
        Ok(self)
    }

    pub fn migration_continuity(&self) -> Option<&MigrationContinuityRecord> {
        self.migration_continuity.as_ref()
    }

    /// Binds the reported annotation-default transition to an explicit legacy
    /// migration journal. A representable legacy annotation is carried
    /// unchanged, so only the newly defaulted rows are reported; UPGRADE-005
    /// requires that report to accompany preservation of the raw bytes.
    ///
    /// This promotes the journal to wire version 7 and requires the consumer
    /// identity map to be present as well, because a migration that changes
    /// annotated value identity cannot be published without both coordinates.
    pub fn with_migration_annotation_defaults(
        mut self,
        defaults: MigrationAnnotationDefaults,
    ) -> Result<Self, RepositoryError> {
        if self.migration_annotation_defaults.is_some()
            || self.compact_manifest.is_some()
            || self.stage != PublicationJournalStage::Prepared
            || self.migration_continuity.is_none()
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        if defaults.payload_inspected() {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        self.migration_annotation_defaults = Some(defaults);
        self.wire_version = self.wire_version.max(7);
        self.binding_version = self.binding_version.max(7);
        Ok(self)
    }

    /// Re-admits a migration journal before runtime completion whose
    /// annotation-default coordinate was already durably recorded, so a
    /// resumed publisher reports the same semantic transition without
    /// re-deriving it.
    ///
    /// This is the resume counterpart of
    /// [`Self::with_migration_annotation_defaults`]: only a wire-version-7
    /// legacy migration that already carries a non-inspecting coordinate is
    /// admitted, and the coordinate must be supplied exactly as it was
    /// persisted. It returns the record's own value so a caller cannot
    /// substitute a second, unrecorded transition for the durable evidence.
    pub fn resume_migration_annotation_defaults(
        &self,
        defaults: MigrationAnnotationDefaults,
    ) -> Result<MigrationAnnotationDefaults, RepositoryError> {
        let recorded = self
            .migration_annotation_defaults
            .ok_or(RepositoryError::InvalidFormatMigration)?;
        if self.wire_version != 7
            || self.binding_version != 7
            || !self.is_resumable_migration_stage()
            || self.compact_manifest.is_some()
            || self.migration_continuity.is_none()
            || defaults.payload_inspected()
            || recorded != defaults
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        Ok(recorded)
    }
    fn is_resumable_migration_stage(&self) -> bool {
        matches!(
            self.stage,
            PublicationJournalStage::Prepared
                | PublicationJournalStage::RefAdvanced
                | PublicationJournalStage::IndexReconciled
                | PublicationJournalStage::WorktreeReconciled
        )
    }

    /// The reported annotation-default coordinate of this migration. `None`
    /// means the caller has not declared one, and publication refuses.
    pub const fn migration_annotation_defaults(&self) -> Option<MigrationAnnotationDefaults> {
        self.migration_annotation_defaults
    }

    /// Re-admits a migration journal before runtime completion whose
    /// stream/checkpoint identity inventory was already durably recorded, so
    /// a resumed publisher resolves the same successor identities instead
    /// of re-deriving them.
    ///
    /// This is the resume counterpart of [`Self::with_migration_continuity`],
    /// which refuses a journal that already carries an inventory. A resumed
    /// publisher must supply the inventory exactly as it was persisted, and a
    /// substituted one is rejected: re-deriving a successor identity is the
    /// reset this coordinate exists to prevent. It returns the record's own
    /// value, so the resumed publisher reports the durable evidence rather
    /// than the copy the caller supplied.
    ///
    /// The returned record is the only supported source of successor
    /// identities; [`MigrationContinuityRecord::successor_for`] answers the
    /// legacy-format checkpoint key this migration carried, and answers nothing
    /// for a key it did not carry.
    pub fn resume_migration_continuity(
        &self,
        continuity: MigrationContinuityRecord,
    ) -> Result<MigrationContinuityRecord, RepositoryError> {
        let recorded = self
            .migration_continuity
            .as_ref()
            .ok_or(RepositoryError::InvalidFormatMigration)?;
        if self.compact_manifest().is_some()
            || !self.is_resumable_migration_stage()
            || recorded != &continuity
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        Ok(recorded.clone())
    }

    pub const fn stage(&self) -> PublicationJournalStage {
        self.stage
    }

    pub fn advance(&mut self, next: PublicationJournalStage) -> Result<(), RepositoryError> {
        if next.code() != self.stage.code() + 1 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        self.stage = next;
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, RepositoryError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(JOURNAL_MAGIC);
        bytes.push(self.wire_version);
        put_string(&mut bytes, self.old_head.as_str())?;
        put_string(&mut bytes, self.new_head.as_str())?;
        put_optional_string(
            &mut bytes,
            self.base_index_tree.as_ref().map(IndexTreeRef::as_str),
        )?;
        match self.runtime_intent_id {
            Some(intent_id) => {
                bytes.push(1);
                bytes.extend_from_slice(&intent_id);
            }
            None => bytes.push(0),
        }
        match &self.compact_manifest {
            Some(witness) => {
                bytes.push(1);
                witness.encode(&mut bytes)?;
            }
            None => bytes.push(0),
        }
        if self.wire_version >= 8 {
            match &self.migration_continuity {
                Some(continuity) => {
                    bytes.push(1);
                    continuity.encode(&mut bytes)?;
                }
                None => bytes.push(0),
            }
        } else if self.wire_version >= 6 {
            self.migration_continuity
                .as_ref()
                .ok_or(RepositoryError::InvalidPublicationJournal)?
                .encode(&mut bytes)?;
        } else if self.migration_continuity.is_some() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if self.wire_version >= 8 {
            match &self.migration_annotation_defaults {
                Some(defaults) => {
                    bytes.push(1);
                    defaults.encode(&mut bytes)?;
                }
                None => bytes.push(0),
            }
            put_optional_string(
                &mut bytes,
                self.store_root.as_ref().map(IndexTreeRef::as_str),
            )?;
        } else if self.wire_version >= 7 {
            self.migration_annotation_defaults
                .ok_or(RepositoryError::InvalidPublicationJournal)?
                .encode(&mut bytes)?;
            if self.store_root.is_some() {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        } else if self.migration_annotation_defaults.is_some() || self.store_root.is_some() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if self.wire_version >= 9 {
            put_u32(&mut bytes, self.logical_rekeys.len())?;
            for rekey in &self.logical_rekeys {
                bytes.extend_from_slice(&rekey.relation_id);
                put_optional_bytes(&mut bytes, Some(rekey.old_address()))?;
                put_optional_bytes(&mut bytes, Some(rekey.new_address()))?;
            }
        } else if !self.logical_rekeys.is_empty() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        bytes.push(self.stage.code());
        put_u32(&mut bytes, self.entries.len())?;
        for entry in &self.entries {
            let path = entry
                .path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::InvalidPublicationJournal)?;
            put_string(&mut bytes, path)?;
            put_optional_bytes(&mut bytes, entry.expected.as_deref())?;
            put_optional_bytes(&mut bytes, entry.next.as_deref())?;
            bytes.push(entry.materialization.phase.code());
            put_optional_string(&mut bytes, entry.materialization.quarantine.as_deref())?;
        }
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        Ok(bytes)
    }

    fn lock_binding(&self) -> Result<[u8; 32], RepositoryError> {
        let mut stable = self.clone();
        stable.stage = PublicationJournalStage::Prepared;
        stable.wire_version = stable.binding_version;
        for entry in &mut stable.entries {
            entry.materialization = PublicationMaterialization::clean();
        }
        Ok(Sha256::digest(stable.encode()?).into())
    }

    fn decode(bytes: &[u8], object_id_length: usize) -> Result<Self, RepositoryError> {
        if bytes.len() > MAX_JOURNAL_BYTES || !bytes.starts_with(JOURNAL_MAGIC) {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut cursor = JOURNAL_MAGIC.len();
        let version = take_byte(bytes, &mut cursor)?;
        if version != 5 && version != 6 && version != 7 && version != 8 && version != 9 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let old_head =
            GitCommitRef::from_verified_commit(take_string(bytes, &mut cursor)?, object_id_length)?;
        let new_head =
            GitCommitRef::from_verified_commit(take_string(bytes, &mut cursor)?, object_id_length)?;
        let base_index_tree = take_optional_string(bytes, &mut cursor)?
            .map(|value| IndexTreeRef::from_verified_tree(value, object_id_length))
            .transpose()?;
        let runtime_intent_id = match take_byte(bytes, &mut cursor)? {
            0 => None,
            1 => Some(take_fixed_array::<16>(bytes, &mut cursor)?),
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let compact_manifest = match take_byte(bytes, &mut cursor)? {
            0 => None,
            1 => Some(CompactManifestWitness::decode(
                bytes,
                &mut cursor,
                object_id_length,
            )?),
            _ => return Err(RepositoryError::InvalidPublicationJournal),
        };
        let migration_continuity = if version >= 8 {
            match take_byte(bytes, &mut cursor)? {
                0 => None,
                1 => Some(MigrationContinuityRecord::decode(bytes, &mut cursor)?),
                _ => return Err(RepositoryError::InvalidPublicationJournal),
            }
        } else if version >= 6 {
            Some(MigrationContinuityRecord::decode(bytes, &mut cursor)?)
        } else {
            None
        };
        let migration_annotation_defaults = if version >= 8 {
            match take_byte(bytes, &mut cursor)? {
                0 => None,
                1 => Some(MigrationAnnotationDefaults::decode(bytes, &mut cursor)?),
                _ => return Err(RepositoryError::InvalidPublicationJournal),
            }
        } else if version >= 7 {
            Some(MigrationAnnotationDefaults::decode(bytes, &mut cursor)?)
        } else {
            None
        };
        let store_root = if version >= 8 {
            take_optional_string(bytes, &mut cursor)?
                .map(|value| IndexTreeRef::from_verified_tree(value, object_id_length))
                .transpose()?
        } else {
            None
        };
        let logical_rekeys = if version >= 9 {
            let count = take_u32(bytes, &mut cursor)? as usize;
            if count == 0 {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            let mut rekeys = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                let relation_id = take_fixed_array::<16>(bytes, &mut cursor)?;
                let old_address = take_optional_bytes(bytes, &mut cursor)?
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                let new_address = take_optional_bytes(bytes, &mut cursor)?
                    .ok_or(RepositoryError::InvalidPublicationJournal)?;
                rekeys.push(PublicationLogicalRowRekey::new(
                    relation_id,
                    old_address,
                    new_address,
                )?);
            }
            if runtime_intent_id.is_none() {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            rekeys
        } else {
            Vec::new()
        };
        if migration_continuity.is_some() && compact_manifest.is_some() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if migration_annotation_defaults.is_some() && migration_continuity.is_none() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let stage = PublicationJournalStage::from_code(take_byte(bytes, &mut cursor)?)?;
        let count = take_u32(bytes, &mut cursor)? as usize;
        if count == 0 {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let mut entries = Vec::with_capacity(count.min(1024));
        let mut paths = HashSet::new();
        for _ in 0..count {
            let path = ManagedPath::new(take_string(bytes, &mut cursor)?)?;
            let expected = take_optional_bytes(bytes, &mut cursor)?;
            let next = take_optional_bytes(bytes, &mut cursor)?;
            if !paths.insert(path.clone()) {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            let phase = PublicationMaterializationPhase::from_code(take_byte(bytes, &mut cursor)?)?;
            let quarantine = take_optional_string(bytes, &mut cursor)?;
            if (phase == PublicationMaterializationPhase::Clean) == quarantine.is_some()
                || phase != PublicationMaterializationPhase::Clean && quarantine.is_none()
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            let materialization = PublicationMaterialization { phase, quarantine };
            let mut entry = PublicationJournalEntry::new(path, expected, next);
            entry.materialization = materialization;
            entries.push(entry);
        }
        if cursor != bytes.len() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let journal = Self {
            old_head,
            new_head,
            base_index_tree,
            runtime_intent_id,
            compact_manifest,
            store_root,
            migration_continuity,
            migration_annotation_defaults,
            logical_rekeys,
            entries,
            stage,
            wire_version: version,
            binding_version: version,
        };
        for (index, entry) in journal.entries.iter().enumerate() {
            if entry.materialization.phase != PublicationMaterializationPhase::Clean
                && entry.materialization.quarantine.as_deref()
                    != Some(journal.quarantine_name(index)?.as_str())
            {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
        }
        Ok(journal)
    }

    fn quarantine_name(&self, index: usize) -> Result<String, RepositoryError> {
        let mut stable = self.clone();
        stable.stage = PublicationJournalStage::Prepared;
        stable.wire_version = stable.binding_version;
        for entry in &mut stable.entries {
            entry.materialization = PublicationMaterialization::clean();
        }
        let mut digest = Sha256::new();
        digest.update(stable.encode()?);
        digest.update(index.to_le_bytes());
        let digest = digest.finalize();
        let suffix = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(format!(".orna-quarantine-journal-{suffix}"))
    }

    fn set_materialization(
        &mut self,
        index: usize,
        phase: PublicationMaterializationPhase,
        quarantine: Option<String>,
    ) -> Result<(), RepositoryError> {
        let expected_quarantine = self.quarantine_name(index)?;
        if (phase == PublicationMaterializationPhase::Clean && quarantine.is_some())
            || (phase != PublicationMaterializationPhase::Clean && quarantine.is_none())
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if phase != PublicationMaterializationPhase::Clean
            && quarantine.as_deref() != Some(expected_quarantine.as_str())
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let entry = self
            .entries
            .get_mut(index)
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        entry.materialization = PublicationMaterialization { phase, quarantine };
        Ok(())
    }
}

impl PrivateCommit {
    pub fn tree(&self) -> &IndexTreeRef {
        &self.tree
    }

    pub fn commit(&self) -> &GitCommitRef {
        &self.commit
    }
}

/// A generation supplied by the embedded runtime state layer.
///
/// The repository layer does not open `state.db`: doing so would conflate the
/// Git/index boundary with the single-writer runtime ownership boundary.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Git's current index generation.  Both fields are retained so a `HEAD`
/// change is observable even when the index tree happens to be identical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexGeneration {
    head: Option<GitCommitRef>,
    tree: Option<IndexTreeRef>,
}

impl IndexGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
    }
    pub fn tree(&self) -> Option<&IndexTreeRef> {
        self.tree.as_ref()
    }
}

/// A lossless, deterministic representation of Git's ordinary worktree
/// status.  It deliberately is not a content hash; callers can compare it but
/// must not treat it as a cryptographic integrity assertion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeState(Vec<u8>, [u8; 32]);

/// Stat identity of one managed worktree path, captured before its bytes are read.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedFileStamp {
    file_type: fs::FileType,
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    change: (u64, u64, i64, i64, u32),
}

/// Stamp of one managed path; `None` records that the path is absent.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ManagedStamp {
    path: ManagedPath,
    stamp: Option<ManagedFileStamp>,
}

/// Worktree status and content digest, plus the stamps of every digested path.
struct WorktreeScan {
    state: WorktreeState,
    stamps: Vec<ManagedStamp>,
}

/// The digest of one regular file's bytes, valid while its stamp is unchanged.
#[derive(Clone, Debug)]
struct CachedFileDigest {
    stamp: ManagedFileStamp,
    digest: [u8; 32],
}

/// How long before a read a file's mtime and ctime must lie for its stamp to
/// prove its bytes unchanged. Filesystems store timestamps coarsely, so a write
/// in the same tick as an earlier read can leave the stamp equal.
const CONTENT_RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
thread_local! {
    /// Regular files whose bytes a worktree observation read on this thread.
    static CONTENT_READS: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl WorktreeState {
    pub fn as_porcelain_v2_z(&self) -> &[u8] {
        &self.0
    }

    fn content_digest(&self) -> &[u8; 32] {
        &self.1
    }

    pub fn is_clean(&self) -> bool {
        self.0.is_empty()
    }
}

/// The four independently observable parts of an Orna CWD generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwdGeneration {
    head: Option<GitCommitRef>,
    branch: Option<String>,
    index: IndexGeneration,
    worktree: WorktreeState,
    runtime: RuntimeGeneration,
}

impl CwdGeneration {
    pub fn head(&self) -> Option<&GitCommitRef> {
        self.head.as_ref()
    }
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    pub fn index(&self) -> &IndexGeneration {
        &self.index
    }
    pub fn worktree(&self) -> &WorktreeState {
        &self.worktree
    }
    pub const fn runtime(&self) -> RuntimeGeneration {
        self.runtime
    }
}

/// The immutable target selected by a checkout preview. A local branch keeps
/// its attachment; every other accepted selector is a detached commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckoutTarget {
    Branch { name: String, commit: GitCommitRef },
    Detached { commit: GitCommitRef },
}

impl CheckoutTarget {
    pub fn commit(&self) -> &GitCommitRef {
        match self {
            Self::Branch { commit, .. } | Self::Detached { commit } => commit,
        }
    }

    pub fn branch_name(&self) -> Option<&str> {
        match self {
            Self::Branch { name, .. } => Some(name),
            Self::Detached { .. } => None,
        }
    }
}

/// Checkout preconditions. The bounded same-commit operation can consume this
/// plan, but forceful/divergent checkout, logical validation, and durable
/// recovery remain separate boundaries owned by higher repository layers.
/// Capturing it may initialize private coordination metadata, but never
/// changes visible Git state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutPreflight {
    target: CheckoutTarget,
    expected_head: Option<GitCommitRef>,
    cwd: CwdGeneration,
    git: CheckoutGitSubplan,
    dirty_content_digest: [u8; 32],
}

/// The read-only Git portion of a checkout plan. Paths are repository-relative
/// and classified from the current worktree and target commit without
/// changing the ref, index, or worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutGitSubplan {
    affected_paths: Vec<ManagedPath>,
    conflicting_paths: Vec<ManagedPath>,
    discardable_paths: Vec<ManagedPath>,
}

impl CheckoutGitSubplan {
    pub fn affected_paths(&self) -> &[ManagedPath] {
        &self.affected_paths
    }

    pub fn conflicting_paths(&self) -> &[ManagedPath] {
        &self.conflicting_paths
    }

    pub fn discardable_paths(&self) -> &[ManagedPath] {
        &self.discardable_paths
    }
}

/// A domain-separated authorization witness for one exact checkout
/// preflight. It is opaque so callers cannot assemble a force token without
/// first observing the repository state it authorizes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CheckoutPlanToken([u8; 32]);

impl CheckoutPlanToken {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An opaque admission capability for one exact force-discard checkout.
///
/// The capability can only be produced after the locked preflight, force
/// witness, logical validation, and canonical discard set all agree. Its
/// private contents prevent callers from replacing any of those bindings
/// before a future journaled executor consumes the capability.
#[must_use]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedCheckoutDiscard {
    preflight: CheckoutPreflight,
    force_token: CheckoutPlanToken,
    discard_paths: Vec<ManagedPath>,
}

/// A durable, opaque record that fences recovery of a force-discard checkout.
///
/// It is written before an eventual destructive executor crosses a Git-visible
/// boundary. Until that executor exists, recovery can only prove that the
/// original preflight is still current and discard the no-mutation intent; it
/// must never infer permission to resume a discard after state drift.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CheckoutRecoveryJournal {
    target: CheckoutTarget,
    runtime: RuntimeGeneration,
    force_token: CheckoutPlanToken,
    discard_paths: Vec<ManagedPath>,
    phase: CheckoutRecoveryPhase,
    before: Option<CwdGeneration>,
    discarded: Option<CwdGeneration>,
    after: Option<CwdGeneration>,
}

/// Durable Git-visible boundaries of one force-discard transition.
///
/// Each state is written only after the preceding Git operation has reached a
/// fully observable CWD generation. Recovery therefore either recognizes an
/// exact recorded generation or stops without applying a broad reset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckoutRecoveryPhase {
    Prepared,
    Discarded,
    Applied,
}

impl CheckoutRecoveryJournal {
    fn from_validated(discard: &ValidatedCheckoutDiscard) -> Self {
        Self {
            target: discard.preflight.target.clone(),
            runtime: discard.preflight.cwd.runtime,
            force_token: discard.force_token,
            discard_paths: discard.discard_paths.clone(),
            phase: CheckoutRecoveryPhase::Prepared,
            before: Some(discard.preflight.cwd.clone()),
            discarded: None,
            after: None,
        }
    }

    fn matches_validated(&self, discard: &ValidatedCheckoutDiscard) -> bool {
        self.target == discard.preflight.target
            && self.runtime == discard.preflight.cwd.runtime
            && self.force_token == discard.force_token
            && self.discard_paths == discard.discard_paths
            && self.before.as_ref() == Some(&discard.preflight.cwd)
    }

    fn record_discarded(&mut self, cwd: CwdGeneration) -> Result<(), RepositoryError> {
        if self.phase != CheckoutRecoveryPhase::Prepared || self.discarded.is_some() {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        self.phase = CheckoutRecoveryPhase::Discarded;
        self.discarded = Some(cwd);
        Ok(())
    }

    fn record_applied(&mut self, cwd: CwdGeneration) -> Result<(), RepositoryError> {
        if self.phase != CheckoutRecoveryPhase::Discarded || self.after.is_some() {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        self.phase = CheckoutRecoveryPhase::Applied;
        self.after = Some(cwd);
        Ok(())
    }

    /// Ensures every journalled generation belongs to the transition it
    /// claims to record. Recovery must not treat independently decodable CWD
    /// snapshots as interchangeable: the discard boundary retains the old
    /// attachment, while the applied boundary must carry the selected target
    /// attachment. This is deliberately checked before recovery can clear a
    /// journal or invoke Git.
    fn validate_generation_transition(&self) -> Result<(), RepositoryError> {
        match (
            self.phase,
            self.before.as_ref(),
            self.discarded.as_ref(),
            self.after.as_ref(),
        ) {
            (CheckoutRecoveryPhase::Prepared, None, None, None) => Ok(()),
            (CheckoutRecoveryPhase::Prepared, Some(_), None, None) => Ok(()),
            (CheckoutRecoveryPhase::Discarded, Some(before), Some(discarded), None)
                if discarded.head == before.head && discarded.branch == before.branch =>
            {
                Ok(())
            }
            (CheckoutRecoveryPhase::Applied, Some(before), Some(discarded), Some(after))
                if discarded.head == before.head
                    && discarded.branch == before.branch
                    && after.head.as_ref() == Some(self.target.commit())
                    && after.branch.as_deref() == self.target.branch_name() =>
            {
                Ok(())
            }
            _ => Err(RepositoryError::InvalidCheckoutJournal),
        }
    }

    fn encode(&self) -> Result<Vec<u8>, RepositoryError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(CHECKOUT_JOURNAL_MAGIC);
        bytes.push(3);
        bytes.extend_from_slice(&self.runtime.get().to_be_bytes());
        match &self.target {
            CheckoutTarget::Branch { name, commit } => {
                bytes.push(1);
                put_string(&mut bytes, name)?;
                put_string(&mut bytes, commit.as_str())?;
            }
            CheckoutTarget::Detached { commit } => {
                bytes.push(2);
                put_string(&mut bytes, commit.as_str())?;
            }
        }
        bytes.extend_from_slice(self.force_token.as_bytes());
        put_u32(&mut bytes, self.discard_paths.len())?;
        for path in &self.discard_paths {
            put_string(
                &mut bytes,
                path.as_path()
                    .to_str()
                    .ok_or(RepositoryError::InvalidCheckoutJournal)?,
            )?;
        }
        bytes.push(match self.phase {
            CheckoutRecoveryPhase::Prepared => 1,
            CheckoutRecoveryPhase::Discarded => 2,
            CheckoutRecoveryPhase::Applied => 3,
        });
        encode_checkout_cwd(
            &mut bytes,
            self.before
                .as_ref()
                .ok_or(RepositoryError::InvalidCheckoutJournal)?,
        )?;
        encode_optional_checkout_cwd(&mut bytes, self.discarded.as_ref())?;
        encode_optional_checkout_cwd(&mut bytes, self.after.as_ref())?;
        self.validate_generation_transition()?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8], object_id_length: usize) -> Result<Self, RepositoryError> {
        if bytes.len() > MAX_JOURNAL_BYTES || !bytes.starts_with(CHECKOUT_JOURNAL_MAGIC) {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let mut cursor = CHECKOUT_JOURNAL_MAGIC.len();
        let version = take_byte(bytes, &mut cursor)?;
        if version != 1 && version != 2 && version != 3 {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let runtime = RuntimeGeneration::new(u64::from_be_bytes(take_fixed_array::<8>(
            bytes,
            &mut cursor,
        )?));
        let target = match take_byte(bytes, &mut cursor)? {
            1 => CheckoutTarget::Branch {
                name: take_string(bytes, &mut cursor)?,
                commit: GitCommitRef::from_verified_commit(
                    take_string(bytes, &mut cursor)?,
                    object_id_length,
                )?,
            },
            2 => CheckoutTarget::Detached {
                commit: GitCommitRef::from_verified_commit(
                    take_string(bytes, &mut cursor)?,
                    object_id_length,
                )?,
            },
            _ => return Err(RepositoryError::InvalidCheckoutJournal),
        };
        let force_token = CheckoutPlanToken(take_fixed_array::<32>(bytes, &mut cursor)?);
        let count = usize::try_from(take_u32(bytes, &mut cursor)?)
            .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
        if count > bytes.len().saturating_sub(cursor) {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let mut discard_paths = Vec::with_capacity(count);
        for _ in 0..count {
            discard_paths.push(ManagedPath::new(take_string(bytes, &mut cursor)?)?);
        }
        if discard_paths.is_empty()
            || discard_paths
                .windows(2)
                .any(|paths| paths[0].as_path() >= paths[1].as_path())
        {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        if version == 1 {
            if cursor != bytes.len() {
                return Err(RepositoryError::InvalidCheckoutJournal);
            }
            return Ok(Self {
                target,
                runtime,
                force_token,
                discard_paths,
                phase: CheckoutRecoveryPhase::Prepared,
                before: None,
                discarded: None,
                after: None,
            });
        }
        let phase = match take_byte(bytes, &mut cursor)? {
            1 => CheckoutRecoveryPhase::Prepared,
            2 => CheckoutRecoveryPhase::Discarded,
            3 => CheckoutRecoveryPhase::Applied,
            _ => return Err(RepositoryError::InvalidCheckoutJournal),
        };
        let before = Some(decode_checkout_cwd(
            bytes,
            &mut cursor,
            object_id_length,
            version >= 3,
        )?);
        let discarded =
            decode_optional_checkout_cwd(bytes, &mut cursor, object_id_length, version >= 3)?;
        let after =
            decode_optional_checkout_cwd(bytes, &mut cursor, object_id_length, version >= 3)?;
        if cursor != bytes.len()
            || before.as_ref().is_none_or(|cwd| cwd.runtime != runtime)
            || discarded.as_ref().is_some_and(|cwd| cwd.runtime != runtime)
            || after.as_ref().is_some_and(|cwd| cwd.runtime != runtime)
        {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let journal = Self {
            target,
            runtime,
            force_token,
            discard_paths,
            phase,
            before,
            discarded,
            after,
        };
        journal.validate_generation_transition()?;
        Ok(journal)
    }
}

/// The failure boundary for a checkout that validates its logical candidate
/// while the repository mutation lock is held.
#[derive(Debug)]
pub enum CheckoutExecutionError<E> {
    Repository(RepositoryError),
    Validation(E),
}

impl CheckoutPreflight {
    pub fn target(&self) -> &CheckoutTarget {
        &self.target
    }

    pub fn expected_head(&self) -> Option<&GitCommitRef> {
        self.expected_head.as_ref()
    }

    pub fn cwd(&self) -> &CwdGeneration {
        &self.cwd
    }

    pub fn git(&self) -> &CheckoutGitSubplan {
        &self.git
    }

    /// Derives a stable authorization witness over the complete preflight,
    /// including target attachment kind, Git state, raw worktree status, and
    /// runtime generation.
    pub fn force_token(&self) -> CheckoutPlanToken {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"ORNA-CHECKOUT-PLAN-TOKEN\0");
        bytes.push(2);
        match &self.target {
            CheckoutTarget::Branch { name, commit } => {
                bytes.push(1);
                token_string(&mut bytes, name);
                token_string(&mut bytes, commit.as_str());
            }
            CheckoutTarget::Detached { commit } => {
                bytes.push(2);
                token_string(&mut bytes, commit.as_str());
            }
        }
        token_optional_string(
            &mut bytes,
            self.expected_head.as_ref().map(GitCommitRef::as_str),
        );
        token_optional_string(&mut bytes, self.cwd.branch.as_deref());
        token_optional_string(
            &mut bytes,
            self.cwd.index.head.as_ref().map(GitCommitRef::as_str),
        );
        token_optional_string(
            &mut bytes,
            self.cwd.index.tree.as_ref().map(IndexTreeRef::as_str),
        );
        token_bytes(&mut bytes, self.cwd.worktree.as_porcelain_v2_z());
        token_bytes(&mut bytes, self.cwd.worktree.content_digest());
        token_bytes(&mut bytes, &self.dirty_content_digest);
        bytes.extend_from_slice(&self.cwd.runtime.get().to_be_bytes());
        token_paths(&mut bytes, &self.git.affected_paths);
        token_paths(&mut bytes, &self.git.conflicting_paths);
        token_paths(&mut bytes, &self.git.discardable_paths);
        CheckoutPlanToken(Sha256::digest(bytes).into())
    }

    /// Requires explicit force authorization for this exact, still-current
    /// preflight. A missing or stale witness never permits checkout mutation.
    pub fn authorize_force(
        &self,
        force: bool,
        token: Option<&CheckoutPlanToken>,
    ) -> Result<(), RepositoryError> {
        if force && token == Some(&self.force_token()) {
            Ok(())
        } else {
            Err(RepositoryError::CheckoutPlanStale)
        }
    }
}

fn token_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
}

fn token_string(output: &mut Vec<u8>, value: &str) {
    token_bytes(output, value.as_bytes());
}

fn token_optional_string(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            token_string(output, value);
        }
        None => output.push(0),
    }
}

fn token_paths(output: &mut Vec<u8>, paths: &[ManagedPath]) {
    output.extend_from_slice(&(paths.len() as u64).to_be_bytes());
    for path in paths {
        token_string(
            output,
            path.as_path()
                .to_str()
                .expect("validated checkout path is UTF-8"),
        );
    }
}

fn sorted_paths(paths: HashSet<ManagedPath>) -> Vec<ManagedPath> {
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| left.as_path().cmp(right.as_path()));
    paths
}

fn is_loose_path_components(components: &[Vec<u8>]) -> bool {
    components.len() >= 2
        && !components[0].starts_with(b".")
        && components
            .last()
            .is_some_and(|component| component.ends_with(b".orna"))
}

fn record_case_folded_path_components(
    prefixes: &mut BTreeMap<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>,
    components: &[Vec<u8>],
) {
    let mut prefix = Vec::new();
    for component in components {
        prefix.push(component.to_ascii_lowercase());
        prefixes
            .entry(prefix.clone())
            .or_default()
            .insert(component.clone());
    }
}

fn case_folded_path_collides(
    prefixes: &BTreeMap<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>,
    components: &[Vec<u8>],
) -> bool {
    let mut prefix = Vec::new();
    for component in components {
        prefix.push(component.to_ascii_lowercase());
        if prefixes
            .get(&prefix)
            .is_some_and(|siblings| siblings.iter().any(|sibling| sibling != component))
        {
            return true;
        }
    }
    false
}

/// Git-resolved local paths for one worktree's private Orna runtime area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePaths {
    root: PathBuf,
}

impl RuntimePaths {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn state_db(&self) -> PathBuf {
        self.root.join("state.db")
    }
    pub fn cache_db(&self) -> PathBuf {
        self.root.join("cache.db")
    }
    pub fn socket(&self) -> PathBuf {
        self.root.join("runtime.sock")
    }
    pub fn locks(&self) -> PathBuf {
        self.root.join("locks")
    }
    /// The runtime-initialized Ed25519 public key used only to authenticate
    /// compact completion receipts. Runtime initialization creates this fixed
    /// private-state record; repository code never provisions or replaces it.
    pub fn compact_runtime_receipt_public_key(&self) -> PathBuf {
        self.root.join("compact-runtime-receipt-public-key-v1")
    }

    /// Creates the private local directory, never a tracked `.orna/` path.
    pub fn ensure_exists(&self) -> Result<(), RepositoryError> {
        fs::create_dir_all(&self.root).map_err(|_| RepositoryError::LocalStateUnavailable)
    }
}

/// A path whose index mutation is deliberately limited to that one path.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ManagedPath(PathBuf);

impl ManagedPath {
    /// Creates a normalized, repository-relative path for a write, a stage, or
    /// a committed-tree entry.
    /// Absolute paths, `.git` administration components at any depth, tracked
    /// `.orna/cache.db`, parent traversal, backslashes, NUL, and empty
    /// components are rejected before they reach Git. No length limit applies
    /// here: Git stores committed names of any length, so an overlong committed
    /// entry must still list. The filesystem reports an overlong name when Orna
    /// creates it.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        Self::normalize(path.as_ref(), true).map(Self)
    }

    /// Wraps a path Git reports for the local worktree (untracked, ignored, or
    /// changed). A user-created directory such as `Sub/.GIT/` is ordinary local
    /// data that Git lists, so this keeps the structural checks but not the Git
    /// metadata refusal. It only feeds conflict classification, never a write.
    fn from_worktree_listing(path: &str) -> Result<Self, RepositoryError> {
        Self::normalize(Path::new(path), false).map(Self)
    }

    fn normalize(path: &Path, strict: bool) -> Result<PathBuf, RepositoryError> {
        if path.as_os_str().is_empty() || path.is_absolute() {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        if strict {
            // Git treats `\` as ordinary, but Windows checkouts and sync tools do
            // not, so `..\x` would traverse there. NUL cannot name a file at all.
            // Empty components (`a//b`, `a/`) are refused as Git's verify_path does.
            let bytes = path.as_os_str().as_encoded_bytes();
            if bytes.iter().any(|byte| matches!(*byte, b'\\' | 0))
                || bytes.split(|byte| *byte == b'/').any(<[u8]>::is_empty)
            {
                return Err(RepositoryError::UnsafeManagedPath);
            }
        }
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(value) => {
                    if strict
                        && orna_foundation_v1::is_git_metadata_component(value.as_encoded_bytes())
                    {
                        return Err(RepositoryError::UnsafeManagedPath);
                    }
                    normalized.push(value);
                }
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
            }
        }
        if normalized.components().next().is_none() || normalized == Path::new(".orna/cache.db") {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        Ok(normalized)
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Git materialisation mode observed from local configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitRepositoryMode {
    /// No sparse-checkout or partial/promisor configuration was observed.
    Ordinary,
    /// Git's working-tree sparsity is enabled without partial/promisor state.
    SparseCheckout,
    /// Git's object store is configured for partial/promisor operation without
    /// sparse-checkout state.
    PartialClone,
    /// Both working-tree sparsity and partial/promisor state are enabled.
    Combined,
    /// The relevant local Git configuration could not be interpreted safely.
    Malformed,
}

/// A redacted, read-only observation of a repository's Git materialisation
/// capabilities. Remote URLs and filesystem paths are intentionally absent;
/// only configured remote names are retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRepositoryCapabilities {
    mode: GitRepositoryMode,
    sparse_checkout: bool,
    partial_clone: bool,
    promisor_remotes: Vec<String>,
}

impl GitRepositoryCapabilities {
    /// The combined sparse/partial classification.
    pub const fn mode(&self) -> GitRepositoryMode {
        self.mode
    }

    /// Whether Git has enabled sparse worktree materialisation.
    pub const fn sparse_checkout(&self) -> bool {
        self.sparse_checkout
    }

    /// Whether partial/promisor object configuration was observed.
    pub const fn partial_clone(&self) -> bool {
        self.partial_clone
    }

    /// Configured promisor remote names. URLs and credentials are never
    /// returned by this observation.
    pub fn promisor_remotes(&self) -> &[String] {
        &self.promisor_remotes
    }
}

/// The fixed Git object kinds accepted by the capability observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitObjectKind {
    Commit,
    Tree,
    Blob,
    Tag,
}

/// Safe local availability information for one complete native Git object ID.
/// A promised object is known only when a local promisor walk proves that the
/// missing ID is reachable; no fetch or lazy hydration is attempted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitObjectState {
    Materialized { kind: GitObjectKind, size: u64 },
    Promised,
    Unavailable,
    Malformed,
}

/// Read-only completeness classification for a caller-declared set of native
/// Git object IDs. A complete result means every supplied ID is either locally
/// materialized or proven reachable as a promised object. It does not infer
/// whether the caller supplied every object required by a logical snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitDeclaredObjectSetState {
    Complete,
    Incomplete,
    Malformed,
}

/// A validated Orna-managed remote reference.  Only refs in `refs/orna/*`
/// participate in continuity checks; ordinary Git refs remain outside this
/// observer's authority.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OrnaInternalRef(String);

impl OrnaInternalRef {
    /// Validates one fully qualified internal reference name.
    pub fn new(reference: impl Into<String>) -> Result<Self, RepositoryError> {
        let reference = reference.into();
        if !valid_orna_internal_ref(&reference) {
            return Err(RepositoryError::InvalidInternalRef);
        }
        Ok(Self(reference))
    }

    /// The fully qualified `refs/orna/*` name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated native Git object ID supplied to a remote-continuity check.
/// The observer additionally verifies that its length agrees with the local
/// repository's native object format before contacting a remote.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativeObjectId(String);

impl NativeObjectId {
    /// Accepts complete SHA-1 or SHA-256 hexadecimal object IDs only.
    pub fn new(object_id: impl Into<String>) -> Result<Self, RepositoryError> {
        let mut object_id = object_id.into();
        if !valid_native_object_id(&object_id) {
            return Err(RepositoryError::InvalidObjectId);
        }
        object_id.make_ascii_lowercase();
        Ok(Self(object_id))
    }

    /// The native hexadecimal object ID.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One remote internal ref whose exact native object ID is required before an
/// allocator or checkpoint can claim continuity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredInternalRef {
    reference: OrnaInternalRef,
    expected: NativeObjectId,
}

impl RequiredInternalRef {
    pub fn new(reference: OrnaInternalRef, expected: NativeObjectId) -> Self {
        Self {
            reference,
            expected,
        }
    }

    pub fn reference(&self) -> &OrnaInternalRef {
        &self.reference
    }

    pub fn expected(&self) -> &NativeObjectId {
        &self.expected
    }
}

/// Read-only continuity evidence for a configured remote's `refs/orna/*`
/// namespace. `InvalidEvidence` and `Unverifiable` are deliberately
/// fail-closed: callers must not allocate IDs or claim checkpoint/allocator
/// continuity from either outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteContinuity {
    Continuous,
    Missing,
    Stale,
    /// Caller-supplied required refs cannot form one unambiguous native
    /// continuity claim, so no remote observation was attempted.
    InvalidEvidence,
    Unverifiable,
}

impl RemoteContinuity {
    /// Whether this observation is sufficient to claim remote allocator or
    /// checkpoint continuity. Every non-continuous observation is fail-closed.
    pub const fn permits_continuity_claim(self) -> bool {
        matches!(self, Self::Continuous)
    }
}

/// A normal Git repository and its selected worktree.
///
/// Clones share one object reader, so a handle and every context opened from
/// it read committed objects through the same long-lived Git process.
#[derive(Clone, Debug)]
pub struct Repository {
    worktree: PathBuf,
    runtime: RuntimePaths,
    objects: std::sync::Arc<object_reader::ObjectReader>,
    coordination_lock_wait: std::time::Duration,
    /// Regular-file digest sections from the last worktree observation. Derived
    /// and disposable: a section is reused only while its file stamp is unchanged.
    section_cache: std::sync::Arc<std::sync::Mutex<HashMap<Vec<u8>, CachedFileDigest>>>,
}

/// Binds a candidate tree entry at its exact index position.
///
/// A regular entry keeps the ordinary `--cacheinfo` path. A tree entry cannot:
/// Git refuses a directory entry that overlaps the file entries a base commit
/// already contributed under the same path, and a `--cacheinfo` directory entry
/// is re-expanded from the worktree. The base's entries are removed first, then
/// the subtree is bound leaf by leaf with `read-tree --prefix`, which reproduces
/// the tree exactly for later index and tree reads.
pub(crate) fn bind_tree_entry_in_index(
    mut git: impl FnMut() -> Command,
    index: &Path,
    path: &str,
    mode: &str,
    object: &str,
) -> Result<(), RepositoryError> {
    if mode != "040000" {
        let cacheinfo = format!("{mode},{object},{path}");
        let mut update = git();
        update.env("GIT_INDEX_FILE", index).args([
            "update-index",
            "--add",
            "--cacheinfo",
            &cacheinfo,
        ]);
        run_command(update)?;
        return Ok(());
    }
    let mut listed = git();
    listed
        .env("GIT_INDEX_FILE", index)
        .args(["ls-files", "-z", "--", path]);
    let output = run_command(listed)?;
    for stale in output.stdout.split(|byte| *byte == 0) {
        if stale.is_empty() {
            continue;
        }
        let stale = std::str::from_utf8(stale).map_err(|_| RepositoryError::GitOperationFailed)?;
        let mut remove = git();
        remove
            .env("GIT_INDEX_FILE", index)
            .args(["update-index", "--force-remove", "--", stale]);
        run_command(remove)?;
    }
    let prefix = format!("{path}/");
    let mut bind = git();
    bind.env("GIT_INDEX_FILE", index)
        .args(["read-tree", "--prefix", &prefix, object]);
    run_command(bind)?;
    Ok(())
}

impl Repository {
    /// Discovers the Git worktree containing `path` and resolves the *per
    /// worktree* local runtime directory using `git rev-parse --git-path`.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let input = path.as_ref();
        let directory = if input.is_dir() {
            input
        } else {
            input.parent().ok_or(RepositoryError::NotAWorktree)?
        };
        let worktree = PathBuf::from(Self::git_at(directory, ["rev-parse", "--show-toplevel"])?);
        let runtime = PathBuf::from(Self::git_at(
            &worktree,
            ["rev-parse", "--git-path", "orna"],
        )?);
        // Git intentionally prints a path relative to its current directory
        // for an ordinary worktree, but a linked worktree may resolve through
        // an absolute administrative path. Store one absolute filesystem path
        // in either case so later local I/O never depends on the caller's CWD.
        let runtime = if runtime.is_absolute() {
            runtime
        } else {
            worktree.join(runtime)
        };
        Ok(Self {
            objects: std::sync::Arc::new(object_reader::ObjectReader::new(worktree.clone())),
            worktree,
            runtime: RuntimePaths { root: runtime },
            coordination_lock_wait: COORDINATION_LOCK_WAIT,
            section_cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Overrides how long publication waits for the coordination lock before
    /// reporting `RepositoryBusy`. Production uses `COORDINATION_LOCK_WAIT`;
    /// tests use this to keep contended-lock cases short or to allow a slow
    /// winner on a loaded runner.
    #[doc(hidden)]
    #[must_use]
    pub fn with_coordination_lock_wait(mut self, wait: std::time::Duration) -> Self {
        self.coordination_lock_wait = wait;
        self
    }

    pub fn worktree(&self) -> &Path {
        &self.worktree
    }
    pub fn runtime_paths(&self) -> &RuntimePaths {
        &self.runtime
    }

    /// The actual selected committed Git snapshot. `None` represents an
    /// unborn `HEAD`; it never substitutes CWD/runtime state.
    pub fn head(&self) -> Result<Option<GitCommitRef>, RepositoryError> {
        self.commit_optional("HEAD^{commit}")
    }

    /// The content of the single index entry for `path`, or `None` when the
    /// path is not in the index. A conflicted path (several stages) is an error.
    pub fn index_file_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, RepositoryError> {
        let mut command = self.command();
        command.args(["--literal-pathspecs", "ls-files", "--stage", "-z", "--"]).arg(path);
        let output = self.run(command)?.stdout;
        let records = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .collect::<Vec<_>>();
        match records.as_slice() {
            [] => Ok(None),
            [record] => {
                let header = record
                    .split(|byte| *byte == b'\t')
                    .next()
                    .ok_or(RepositoryError::GitOperationFailed)?;
                let oid = header
                    .split(|byte| *byte == b' ')
                    .nth(1)
                    .ok_or(RepositoryError::GitOperationFailed)?;
                let oid = std::str::from_utf8(oid).map_err(|_| RepositoryError::GitOperationFailed)?;
                Ok(Some(self.git_bytes(["cat-file", "blob", oid])?))
            }
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    /// The content of `path` in `HEAD`, or `None` when `HEAD` has no such path
    /// or the repository has no commit yet.
    pub fn head_file_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, RepositoryError> {
        if self.head()?.is_none() {
            return Ok(None);
        }
        let output = self.git_bytes(["ls-tree", "-z", "HEAD", "--", path])?;
        let Some(record) = output.split(|byte| *byte == 0).find(|record| !record.is_empty()) else {
            return Ok(None);
        };
        let header = record
            .split(|byte| *byte == b'\t')
            .next()
            .ok_or(RepositoryError::GitOperationFailed)?;
        let oid = header
            .split(|byte| *byte == b' ')
            .nth(2)
            .ok_or(RepositoryError::GitOperationFailed)?;
        let oid = std::str::from_utf8(oid).map_err(|_| RepositoryError::GitOperationFailed)?;
        Ok(Some(self.git_bytes(["cat-file", "blob", oid])?))
    }

    /// Lists the newest commits reachable from `HEAD`, newest first.
    ///
    /// The count and output size are bounded before Git output is accumulated.
    /// An unborn `HEAD` produces an empty history.
    pub fn committed_history(
        &self,
        max_commits: usize,
    ) -> Result<Vec<CommittedLogEntry>, RepositoryError> {
        const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

        if max_commits == 0 || self.head()?.is_none() {
            return Ok(Vec::new());
        }

        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["log", "-z"])
            .arg(format!("--max-count={max_commits}"))
            .arg("--format=format:%H%x00%cI%x00%an%x00%s%x00")
            .arg("HEAD")
            .stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let mut output = Vec::new();
        let stdout = child
            .stdout
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?;
        let read_result = stdout
            .take((MAX_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut output);
        if read_result.is_err() || output.len() > MAX_OUTPUT_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RepositoryError::GitOperationFailed);
        }
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }

        let object_id_length = self.native_object_id_length()?;
        let mut fields = output.split(|byte| *byte == 0);
        let mut entries = Vec::new();
        loop {
            let Some(object_id) = fields.next() else {
                break;
            };
            let committed_at = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let author = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let subject = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let record_end = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            if !record_end.is_empty() {
                return Err(RepositoryError::GitOperationFailed);
            }
            let object_id = String::from_utf8(object_id.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            entries.push(CommittedLogEntry {
                commit: GitCommitRef::from_verified_commit(object_id, object_id_length)?,
                committed_at: String::from_utf8_lossy(committed_at).into_owned(),
                author: String::from_utf8_lossy(author).into_owned(),
                subject: String::from_utf8_lossy(subject).into_owned(),
            });
        }
        if fields.any(|field| !field.is_empty()) || entries.len() > max_commits {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(entries)
    }

    /// Lists local branches in Git's ref order, bounded by `max_branches`.
    pub fn local_branches(
        &self,
        max_branches: usize,
    ) -> Result<Vec<CommittedBranch>, RepositoryError> {
        const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

        if max_branches == 0 {
            return Ok(Vec::new());
        }

        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["for-each-ref", "--count"])
            .arg(max_branches.to_string())
            .arg("--format=%(refname:strip=2)%00%(objectname)")
            .arg("refs/heads")
            .stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let mut output = Vec::new();
        let stdout = child
            .stdout
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?;
        let read_result = stdout
            .take((MAX_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut output);
        if read_result.is_err() || output.len() > MAX_OUTPUT_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RepositoryError::GitOperationFailed);
        }
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }

        let object_id_length = self.native_object_id_length()?;
        let mut branches = Vec::new();
        for record in output
            .split(|byte| *byte == b'\n')
            .filter(|record| !record.is_empty())
        {
            let separator = record
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(RepositoryError::GitOperationFailed)?;
            let (name, object_id_with_separator) = record.split_at(separator);
            let object_id = &object_id_with_separator[1..];
            let name = String::from_utf8(name.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            if !valid_branch_name(&name) {
                return Err(RepositoryError::GitOperationFailed);
            }
            let object_id = String::from_utf8(object_id.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            branches.push(CommittedBranch {
                name,
                commit: GitCommitRef::from_verified_commit(object_id, object_id_length)?,
            });
        }
        if branches.len() > max_branches {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(branches)
    }

    /// Builds a real Git tree and commit from `expected_head` plus the supplied
    /// managed-file changes, using a private temporary index. The ordinary
    /// index and worktree are never inputs to this operation and the ordinary
    /// refs are not advanced; unreachable candidate objects are permitted when
    /// a later publication step fails.
    pub fn build_private_commit(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        self.build_private_commit_inner(expected_head, changes, None, message)
    }

    /// Builds a private format-3 candidate from a validated native store root.
    /// Format-3 rows are persisted only in the store tree, so this operation
    /// cannot accept loose-row file projections.
    pub fn build_private_commit_with_store_root(
        &self,
        expected_head: &GitCommitRef,
        graph: &NativeGraphContext,
        store_root: &NativeOid,
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        self.build_private_commit_inner(expected_head, &[], Some((graph, store_root)), message)
    }

    fn build_private_commit_inner(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
        store_projection: Option<(&NativeGraphContext, &NativeOid)>,
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        if changes.is_empty() && store_projection.is_none() {
            return Err(RepositoryError::NoManagedPaths);
        }
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        if let Some((graph, store_root)) = store_projection
            && store_root.algorithm() != graph.algorithm()
        {
            return Err(RepositoryError::Graph(GraphError::InvalidOidWidth {
                expected: graph.algorithm().width(),
                actual: store_root.as_bytes().len(),
            }));
        }
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        let mut paths = HashSet::new();
        for change in changes {
            if !paths.insert(change.path.clone()) || change.path.as_path().to_str().is_none() {
                return Err(RepositoryError::UnsafeManagedPath);
            }
        }
        self.objects()
            .check_expansion(expected_head.as_str(), "")?;
        self.reject_final_loose_path_collisions(expected_head, changes)?;

        self.runtime.ensure_exists()?;
        fs::create_dir_all(self.runtime.locks())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let index = self.runtime.locks().join(format!(
            "private-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::remove_file(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;

        let result = (|| {
            let mut read_tree = self.command();
            read_tree
                .env("GIT_INDEX_FILE", &index)
                .args(["read-tree", expected_head.as_str()]);
            self.run(read_tree)?;

            for change in changes {
                if let Some(bytes) = change.bytes() {
                    let object = self.hash_object(bytes)?;
                    let cacheinfo = format!(
                        "100644,{object},{}",
                        change
                            .path
                            .as_path()
                            .to_str()
                            .ok_or(RepositoryError::UnsafeManagedPath)?
                    );
                    let mut update = self.command();
                    update.env("GIT_INDEX_FILE", &index).args([
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &cacheinfo,
                    ]);
                    self.run(update)?;
                } else {
                    let mut update = self.command();
                    update
                        .env("GIT_INDEX_FILE", &index)
                        .args(["update-index", "--force-remove", "--"])
                        .arg(change.path.as_path());
                    self.run(update)?;
                }
            }
            if let Some((graph, store_root)) = store_projection {
                graph
                    .pin_store_subtree_in_index(&index, store_root)
                    .map_err(RepositoryError::Graph)?;
            }

            let mut write_tree = self.command();
            write_tree
                .env("GIT_INDEX_FILE", &index)
                .args(["write-tree"]);
            let tree =
                self.index_tree_from_native_oid(trim_output(&self.run(write_tree)?.stdout))?;

            let mut commit_tree = self.command();
            commit_tree
                .args([
                    "commit-tree",
                    tree.as_str(),
                    "-p",
                    expected_head.as_str(),
                    "-m",
                ])
                .arg(message);
            let commit =
                self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?;
            if self.head()?.as_ref() != Some(expected_head) {
                return Err(RepositoryError::StaleHead);
            }
            Ok(PrivateCommit { tree, commit })
        })();
        let _ = fs::remove_file(&index);
        result
    }

    /// Builds one publication candidate `N = H + P` from the captured base and
    /// the frozen native store root (ORNA-PUB-010).
    ///
    /// The candidate is built entirely in a private index seeded from
    /// `expected_head`; the ordinary index and worktree are never read, so no
    /// human staged or unstaged content can reach the candidate. The frozen
    /// batch contributes its encoded store root, staged at the fixed
    /// `.orna/store` position the format-3 reader resolves. Because the
    /// argument is an encoded OGS-1 store tree rather than a set of managed
    /// file bytes, a caller cannot smuggle an unrelated edit into a
    /// publication commit.
    ///
    /// Every written object stays unreachable until the returned commit names
    /// it, and the commit is a direct child of the captured `HEAD`. The
    /// symbolic branch and ordinary index are untouched; publication advances
    /// them later under its own lock and compare-and-set.

    pub fn build_frozen_publication_candidate(
        &self,
        expected_head: &GitCommitRef,
        graph: &NativeGraphContext,
        store_root: &NativeOid,
        message: &str,
    ) -> Result<PrivateCommit, RepositoryError> {
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_no_git_operation_in_progress()?;
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        self.objects()
            .check_expansion(expected_head.as_str(), "")?;
        if store_root.algorithm() != graph.algorithm() {
            return Err(RepositoryError::Graph(GraphError::InvalidOidWidth {
                expected: graph.algorithm().width(),
                actual: store_root.as_bytes().len(),
            }));
        }
        self.runtime.ensure_exists()?;
        fs::create_dir_all(self.runtime.locks())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let index = self.runtime.locks().join(format!(
            "private-index-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::remove_file(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;

        let result = (|| {
            let mut read_tree = self.command();
            read_tree
                .env("GIT_INDEX_FILE", &index)
                .args(["read-tree", expected_head.as_str()]);
            self.run(read_tree)?;

            // The store root must be a real decodable OGS-1 tree, so a raw
            // object ID cannot be planted at the store position.
            graph
                .pin_store_subtree_in_index(&index, store_root)
                .map_err(RepositoryError::Graph)?;

            let mut write_tree = self.command();
            write_tree
                .env("GIT_INDEX_FILE", &index)
                .args(["write-tree"]);
            let tree =
                self.index_tree_from_native_oid(trim_output(&self.run(write_tree)?.stdout))?;

            let mut commit_tree = self.command();
            commit_tree.args([
                "commit-tree",
                tree.as_str(),
                "-p",
                expected_head.as_str(),
                "-m",
            ]);
            commit_tree.arg(message);
            let commit =
                self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?;
            if self.head()?.as_ref() != Some(expected_head) {
                return Err(RepositoryError::StaleHead);
            }
            Ok(PrivateCommit { tree, commit })
        })();
        let _ = fs::remove_file(&index);
        result
    }

    /// Advances the symbolic current branch to a previously built private
    /// candidate using Git's compare-and-set ref transaction. It does not
    /// reconcile the ordinary index or worktree; callers must keep the
    /// publication journal until that separate recovery boundary is complete.
    pub fn advance_current_ref(
        &self,
        expected_head: &GitCommitRef,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        self.advance_current_ref_bound(expected_head, candidate, None)?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(())
    }

    fn symbolic_head_ref(&self) -> Result<String, RepositoryError> {
        let reference = self
            .git(["symbolic-ref", "--quiet", "HEAD"])
            .map_err(|_| RepositoryError::DetachedHead)?;
        if !reference.starts_with("refs/heads/") {
            return Err(RepositoryError::DetachedHead);
        }
        Ok(reference)
    }

    /// Resolves the branch a publication may advance. `HEAD` must still name
    /// `expected_head` through an attached `refs/heads/*` symbolic ref, and a
    /// branch bound by the journal must be that ref (ORNA-PUB-009).
    fn advance_target_ref(
        &self,
        expected_head: &GitCommitRef,
        expected_ref: Option<&str>,
    ) -> Result<String, RepositoryError> {
        let actual = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        if &actual != expected_head {
            return Err(RepositoryError::StaleHead);
        }
        let reference = self.symbolic_head_ref()?;
        if expected_ref.is_some_and(|expected| expected != reference) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(reference)
    }

    fn advance_current_ref_bound(
        &self,
        expected_head: &GitCommitRef,
        candidate: &PrivateCommit,
        expected_ref: Option<&str>,
    ) -> Result<(), RepositoryError> {
        let reference = self.advance_target_ref(expected_head, expected_ref)?;
        // ORNA-PUB-005: every object the candidate adds is durable before the
        // ref names it, and the ref is durable before a journal stage or the
        // runtime's tail consumption relies on it. A ref backend whose refs
        // cannot be flushed is refused before the ref moves.
        native_graph::require_files_ref_backend(self).map_err(RepositoryError::Graph)?;
        self.sync_candidate_objects(Some(expected_head), candidate.commit())?;
        self.advance_ref_transaction(&reference, expected_head, candidate.commit())?;
        native_graph::sync_git_ref_durably(self, &reference).map_err(RepositoryError::Graph)?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        Ok(())
    }

    /// Flushes every object `candidate` adds over `base`: the commit, its root
    /// tree, and each tree and blob on a changed path. Without a base (a root
    /// commit) every tree and blob of the candidate is flushed. `write-tree`
    /// and `commit-tree` leave their loose objects unflushed under Git's
    /// default `core.fsync`, so without this a crash after the ref names them
    /// could lose or truncate them.
    fn sync_candidate_objects(
        &self,
        base: Option<&GitCommitRef>,
        candidate: &GitCommitRef,
    ) -> Result<(), RepositoryError> {
        let algorithm = match self.native_object_id_length()? {
            40 => GitHashAlgorithm::Sha1,
            64 => GitHashAlgorithm::Sha256,
            _ => return Err(RepositoryError::UnsupportedObjectFormat),
        };
        let oid = |hex: &str| NativeOid::from_hex(algorithm, hex).map_err(RepositoryError::Graph);
        let root_tree = format!("{}^{{tree}}", candidate.as_str());
        let mut objects = BTreeSet::from([
            oid(candidate.as_str())?,
            oid(&self.git(["rev-parse", "--verify", &root_tree])?)?,
        ]);
        self.objects().check_expansion(candidate.as_str(), "")?;
        if let Some(base) = base {
            self.objects().check_expansion(base.as_str(), "")?;
        }
        let changes = match base {
            Some(base) => self.git_bytes_no_lazy_fetch([
                "diff-tree",
                "-r",
                "-t",
                "-z",
                "--no-renames",
                base.as_str(),
                candidate.as_str(),
            ])?,
            // `--root` reports every path of a parentless commit as added.
            None => self.git_bytes_no_lazy_fetch([
                "diff-tree",
                "--root",
                "--no-commit-id",
                "-r",
                "-t",
                "-z",
                "--no-renames",
                candidate.as_str(),
            ])?,
        };
        // Raw `-z` output alternates `:<old mode> <new mode> <old> <new> <status>`
        // records with their paths.
        let mut fields = changes
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        while let Some(record) = fields.next() {
            fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let record =
                std::str::from_utf8(record).map_err(|_| RepositoryError::GitOperationFailed)?;
            let mut parts = record
                .strip_prefix(':')
                .ok_or(RepositoryError::GitOperationFailed)?
                .split(' ');
            let new_mode = parts.nth(1).ok_or(RepositoryError::GitOperationFailed)?;
            let new_object = parts.nth(1).ok_or(RepositoryError::GitOperationFailed)?;
            // A deletion names no object, and a gitlink names a commit in
            // another repository.
            if new_mode != "000000" && new_mode != "160000" {
                objects.insert(oid(new_object)?);
            }
        }
        let objects_dir = self.git_path("objects")?;
        native_graph::sync_object_files(&objects_dir, &objects, || Ok(()))
            .map_err(RepositoryError::Graph)
    }

    /// Persists the ref `HEAD` resolves through: its branch when attached,
    /// otherwise `HEAD` itself.
    fn sync_head_ref(&self) -> Result<(), RepositoryError> {
        let reference = self.git(["rev-parse", "--symbolic-full-name", "HEAD"])?;
        native_graph::sync_git_ref_durably(self, &reference).map_err(RepositoryError::Graph)
    }

    /// Whether `HEAD` names `commit` or descends from it. A commit that is not
    /// in the object database is contained by no ref.
    pub fn head_contains_commit(&self, commit: &GitCommitRef) -> Result<bool, RepositoryError> {
        let Some(head) = self.head()? else {
            return Ok(false);
        };
        if &head == commit {
            return Ok(true);
        }
        if self
            .commit_optional(&format!("{}^{{commit}}", commit.as_str()))?
            .is_none()
        {
            return Ok(false);
        }
        let mut command = self.command();
        command.args([
            "merge-base",
            "--is-ancestor",
            commit.as_str(),
            head.as_str(),
        ]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    /// Whether a local branch names `commit` or descends from it. A commit
    /// that is not in the object database is contained by no branch.
    fn branch_contains_commit(&self, commit: &GitCommitRef) -> Result<bool, RepositoryError> {
        if self
            .commit_optional(&format!("{}^{{commit}}", commit.as_str()))?
            .is_none()
        {
            return Ok(false);
        }
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "--contains",
            commit.as_str(),
            "refs/heads/",
        ]);
        Ok(self
            .run(command)?
            .stdout
            .iter()
            .any(|byte| !byte.is_ascii_whitespace()))
    }

    /// Installs the ordinary index image for a published candidate. Only the
    /// supplied managed paths are replaced from the candidate tree; unrelated
    /// staged entries remain in the captured index. The worktree is not
    /// modified, so managed file replacement is a later recovery boundary.
    pub fn reconcile_published_index(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        paths: &[ManagedPath],
    ) -> Result<IndexGeneration, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_atomic_index_install_supported()?;
        self.ensure_no_git_index_lock()?;
        let index = self.git_path("index")?;
        let base_index = self.capture_index_for_expected(expected_index, &index)?;
        let git_lock = GitIndexLock::acquire(index.with_extension("lock"))?;
        self.reconcile_published_index_locked(
            expected_index,
            candidate,
            paths,
            &base_index,
            &git_lock,
        )
    }

    fn reconcile_published_index_locked(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        paths: &[ManagedPath],
        base_index: &[u8],
        _git_lock: &GitIndexLock,
    ) -> Result<IndexGeneration, RepositoryError> {
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        let index = self.git_path("index")?;
        let mut unique = HashSet::new();
        if paths.iter().any(|path| !unique.insert(path.clone())) {
            return Err(RepositoryError::UnsafeManagedPath);
        }

        let candidate_index = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .join(format!("index-candidate-{}", Uuid::new_v4()));
        write_new_index_candidate(&candidate_index, base_index)?;
        let result = (|| {
            for path in paths {
                let encoded = path
                    .as_path()
                    .to_str()
                    .ok_or(RepositoryError::UnsafeManagedPath)?;
                match self.candidate_tree_entry(candidate, path)? {
                    Some((mode, object)) => bind_tree_entry_in_index(
                        || self.command(),
                        &candidate_index,
                        encoded,
                        &mode,
                        &object,
                    )?,
                    None => {
                        let mut update = self.command();
                        update
                            .env("GIT_INDEX_FILE", &candidate_index)
                            .args(["update-index", "--force-remove", "--"])
                            .arg(path.as_path());
                        self.run(update)?;
                    }
                }
            }
            // Reconcile store leaves before atomically installing the candidate index.
            self.reconcile_store_worktree_transition(
                expected_index,
                candidate,
                true,
            )?;
            // Gate D boundary: the store worktree has been rewritten but the
            // candidate index is not yet installed (ORNA-PUB-005/PUB-016).
            crate::publication_crash_barrier("store-worktree");
            self.configure_store_tree_skip_worktree(&candidate_index)?;
            fs::File::open(&candidate_index)
                .and_then(|file| file.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let current_index =
                fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if current_index != base_index || self.head()?.as_ref() != Some(candidate.commit()) {
                return Err(RepositoryError::StaleHead);
            }
            fs::rename(&candidate_index, &index)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::File::open(
                index
                    .parent()
                    .ok_or(RepositoryError::LocalStateUnavailable)?,
            )
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            Ok(IndexGeneration {
                head: self.head()?,
                tree: self.index_tree_from_file(&index)?,
            })
        })();
        let _ = fs::remove_file(&candidate_index);
        result
    }

    /// OGS envelope metadata stays materialized, but OGB-2 raw chunks remain
    /// only in Git's object store instead of being copied into the worktree.
    fn configure_store_tree_skip_worktree(&self, index: &Path) -> Result<(), RepositoryError> {
        let mut list = self.command();
        list.env("GIT_INDEX_FILE", index)
            .args(["ls-files", "-t", "-z", "--", ".orna/store"]);
        let output = self.run(list)?.stdout;
        let mut skip_paths = Vec::new();
        let mut clear_paths = Vec::new();
        for record in output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            if record.len() < 3 || record[1] != b' ' {
                return Err(RepositoryError::GitOperationFailed);
            }
            let path = std::str::from_utf8(&record[2..])
                .map_err(|_| RepositoryError::UnsafeManagedPath)?;
            if !path.starts_with(".orna/store/") {
                return Err(RepositoryError::UnsafeManagedPath);
            }
            ManagedPath::new(path)?;
            let should_skip = Self::is_store_payload_blob_path(path);
            if (record[0] == b'S') == should_skip {
                continue;
            }
            if should_skip {
                skip_paths.push(path.to_owned());
            } else {
                clear_paths.push(path.to_owned());
            }
        }
        self.update_store_skip_worktree_paths(index, false, &clear_paths)?;
        self.update_store_skip_worktree_paths(index, true, &skip_paths)
    }

    fn update_store_skip_worktree_paths(
        &self,
        index: &Path,
        skip: bool,
        paths: &[String],
    ) -> Result<(), RepositoryError> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut encoded_paths = Vec::new();
        for path in paths {
            encoded_paths.extend_from_slice(path.as_bytes());
            encoded_paths.push(0);
        }
        let mut update = self.command();
        update
            .env("GIT_INDEX_FILE", index)
            .args([
                "update-index",
                if skip { "--skip-worktree" } else { "--no-skip-worktree" },
                "-z",
                "--stdin",
            ])
            .stdin(Stdio::piped());
        let mut child = update
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .write_all(&encoded_paths)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }
    /// Keeps newly introduced compact files out of ordinary worktree diffs while
    /// their post-ref recovery journal is pending. The worktree bytes remain
    /// checked directly by recovery, and the bits are cleared once reconciliation
    /// succeeds. The caller holds the publisher's Git index lock.
    fn set_compact_recovery_skip_worktree_locked(
        &self,
        journal: &PublicationJournal,
        skip: bool,
    ) -> Result<(), RepositoryError> {
        if journal.compact_manifest().is_none() {
            return Ok(());
        }

        let mut encoded_paths = Vec::new();
        for entry in journal.entries() {
            if entry.expected().is_none() && entry.next().is_some() {
                encoded_paths.extend_from_slice(
                    entry.path.as_path().as_os_str().as_encoded_bytes(),
                );
                encoded_paths.push(0);
            }
        }
        if encoded_paths.is_empty() {
            return Ok(());
        }

        self.ensure_atomic_index_install_supported()?;
        let index = self.git_path("index")?;
        let base_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let parent = index.parent().ok_or(RepositoryError::LocalStateUnavailable)?;
        let candidate_index = parent.join(format!(".orna-index-recovery-{}", Uuid::new_v4()));
        let result = (|| {
            write_new_index_candidate(&candidate_index, &base_index)?;
            let mut update = self.command();
            update
                .env("GIT_INDEX_FILE", &candidate_index)
                .args([
                    "update-index",
                    if skip { "--skip-worktree" } else { "--no-skip-worktree" },
                    "-z",
                    "--stdin",
                ])
                .stdin(Stdio::piped());
            let mut child = update
                .spawn()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            child
                .stdin
                .take()
                .ok_or(RepositoryError::LocalStateUnavailable)?
                .write_all(&encoded_paths)
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            let output = child
                .wait_with_output()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if !output.status.success() {
                return Err(RepositoryError::GitOperationFailed);
            }
            fs::File::open(&candidate_index)
                .and_then(|file| file.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let current_index =
                fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if current_index != base_index
                || self.head()?.as_ref() != Some(journal.new_head())
            {
                return Err(RepositoryError::StaleHead);
            }
            fs::rename(&candidate_index, &index)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)
        })();
        let _ = fs::remove_file(&candidate_index);
        result
    }
    /// Enumerates regular blobs in a committed native-store tree.
    /// Tree entries are validated before they can affect the worktree.
    fn native_store_tree_blobs(
        &self,
        treeish: &str,
    ) -> Result<BTreeMap<String, String>, RepositoryError> {
        self.objects().check_expansion(treeish, ".orna/store")?;
        let mut list = self.command();
        list.env("GIT_NO_LAZY_FETCH", "1").args([
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            treeish,
            "--",
            ".orna/store",
        ]);
        let output = self.run(list)?.stdout;
        let mut entries = BTreeMap::new();
        for record in output.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or(RepositoryError::GitOperationFailed)?;
            let mut fields = record[..tab].split(|byte| *byte == b' ');
            let mode = fields.next().unwrap_or_default();
            let kind = fields.next().unwrap_or_default();
            let object = fields.next().unwrap_or_default();
            if mode != b"100644" || kind != b"blob" || fields.next().is_some() {
                return Err(RepositoryError::GitOperationFailed);
            }
            let path = String::from_utf8(record[tab + 1..].to_vec())
                .map_err(|_| RepositoryError::UnsafeManagedPath)?;
            if !path.starts_with(".orna/store/") {
                return Err(RepositoryError::UnsafeManagedPath);
            }
            ManagedPath::new(path.as_str())?;
            let object = String::from_utf8(object.to_vec())
                .map_err(|_| RepositoryError::GitOperationFailed)?;
            if entries.insert(path, object).is_some() {
                return Err(RepositoryError::GitOperationFailed);
            }
        }
        Ok(entries)
    }

    fn store_skip_worktree_paths(
        &self,
        index: &Path,
    ) -> Result<BTreeSet<String>, RepositoryError> {
        let mut list = self.command();
        list.env("GIT_INDEX_FILE", index)
            .args(["ls-files", "-t", "-z", "--", ".orna/store"]);
        let output = self.run(list)?.stdout;
        let mut paths = BTreeSet::new();
        for record in output.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
            if record.len() < 3 || record[0] != b'S' || record[1] != b' ' {
                continue;
            }
            let path = std::str::from_utf8(&record[2..])
                .map_err(|_| RepositoryError::UnsafeManagedPath)?;
            if !path.starts_with(".orna/store/") {
                return Err(RepositoryError::UnsafeManagedPath);
            }
            ManagedPath::new(path)?;
            paths.insert(path.to_owned());
        }
        Ok(paths)
    }
    /// In an OGS envelope, regular leaves named data are bounded node
    /// metadata. Other regular leaves are OGB-2 chunks named by their Git OID.
    fn is_store_payload_blob_path(path: &str) -> bool {
        path.strip_prefix(".orna/store/")
            .and_then(|relative| relative.rsplit('/').next())
            .is_some_and(|name| name != "data")
    }

    fn reconcile_store_payload_worktree_file(
        &self,
        path: &ManagedPath,
        object: &str,
        target: &Path,
        quarantine: &Path,
        install: bool,
    ) -> Result<(), RepositoryError> {
        let current = self.read_managed_file(target)?;
        let displaced = self.read_managed_file(quarantine)?;
        if current.is_some() && displaced.is_some() {
            return Err(RepositoryError::ManagedContentConflict);
        }
        let expected = if current.is_some() || displaced.is_some() {
            Some(self.git_bytes_no_lazy_fetch(["cat-file", "blob", object])?)
        } else {
            None
        };
        if current
            .as_deref()
            .is_some_and(|bytes| Some(bytes) != expected.as_deref())
            || displaced
                .as_deref()
                .is_some_and(|bytes| Some(bytes) != expected.as_deref())
        {
            return Err(RepositoryError::ManagedContentConflict);
        }
        if !install {
            return Ok(());
        }
        let Some(expected) = expected.as_deref() else {
            return Ok(());
        };
        if current.is_some() {
            self.materialize_managed_file_impl_with_quarantine_locked(
                path,
                Some(expected),
                None,
                Some(quarantine),
                None,
                None,
            )
        } else {
            let parent = target
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?;
            self.remove_verified_quarantine(parent, quarantine, target, Some(expected), None)
        }
    }
    fn reconcile_store_worktree_transition(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        install: bool,
    ) -> Result<(), RepositoryError> {
        let index = self.git_path("index")?;
        let skipped_paths = self.store_skip_worktree_paths(&index)?;
        let old = expected_index
            .tree()
            .map(|tree| self.native_store_tree_blobs(tree.as_str()))
            .transpose()?
            .unwrap_or_default();
        let new = self.native_store_tree_blobs(candidate.commit().as_str())?;
        let paths = old.keys().chain(new.keys()).collect::<BTreeSet<_>>();
        for relative in paths {
            let relative = relative.as_str();
            let old_object = old.get(relative);
            let new_object = new.get(relative);
            let unchanged = old_object == new_object;
            let skipped = skipped_paths.contains(relative);
            let is_payload = Self::is_store_payload_blob_path(relative);
            if unchanged && !skipped && !is_payload {
                // Node metadata is not rewritten here, but an edit to it is still an external change
                // and must be refused rather than left in place (ORNA-PUB-010).
                let path = ManagedPath::new(relative)?;
                let (target, _) = self.store_reconciliation_paths(&path, candidate)?;
                if let Some(current) = self.read_managed_file(&target)? {
                    let object = new_object.ok_or(RepositoryError::GitOperationFailed)?;
                    let next = self.git_bytes_no_lazy_fetch(["cat-file", "blob", object])?;
                    if current != next {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                }
                continue;
            }

            let path = ManagedPath::new(relative)?;
            let (target, quarantine) = self.store_reconciliation_paths(&path, candidate)?;
            if is_payload {
                if let Some(object) = new_object {
                    self.reconcile_store_payload_worktree_file(
                        &path,
                        object,
                        &target,
                        &quarantine,
                        install,
                    )?;
                    continue;
                }
            }
            let current = self.read_managed_file(&target)?;
            // Older publications could leave skipped leaves absent; restore them before
            // clearing those bits so reconciliation does not invent deletions.
            if unchanged {
                let displaced = self.read_managed_file(&quarantine)?;
                let next = new_object
                    .map(|object| self.git_bytes_no_lazy_fetch(["cat-file", "blob", object]))
                    .transpose()?
                    .ok_or(RepositoryError::GitOperationFailed)?;
                if current.as_deref().is_some_and(|bytes| bytes != next.as_slice()) {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                if current.is_none() || displaced.is_some() {
                    if displaced.as_deref().is_some_and(|bytes| bytes != next.as_slice()) {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                    if install {
                        let expected = displaced.as_deref().map(|_| next.as_slice());
                        self.materialize_store_worktree_file(
                            &path,
                            expected,
                            Some(&next),
                            candidate,
                        )?;
                    }
                }
                continue;
            }

            let old_bytes = old_object
                .map(|object| self.git_bytes_no_lazy_fetch(["cat-file", "blob", object]))
                .transpose()?;
            let new_bytes = new_object
                .map(|object| self.git_bytes_no_lazy_fetch(["cat-file", "blob", object]))
                .transpose()?;
            let displaced = self.read_managed_file(&quarantine)?;
            if displaced.as_deref().is_some_and(|bytes| {
                Some(bytes) != old_bytes.as_deref()
                    || current.as_deref().is_some_and(|current_bytes| {
                        Some(current_bytes) != new_bytes.as_deref()
                    })
            }) {
                return Err(RepositoryError::ManagedContentConflict);
            }
            let matches_old = current.as_deref() == old_bytes.as_deref();
            let matches_new = current.as_deref() == new_bytes.as_deref();
            let recovering_quarantine = !matches_old
                && !matches_new
                && current.is_none()
                && old_bytes.is_some()
                && new_bytes.is_some()
                && displaced.as_deref() == old_bytes.as_deref();
            let skipped_missing = skipped && current.is_none() && displaced.is_none();
            if !matches_old && !matches_new && !recovering_quarantine && !skipped_missing {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if install {
                let expected = if skipped_missing { None } else { old_bytes.as_deref() };
                self.materialize_store_worktree_file(
                    &path,
                    expected,
                    new_bytes.as_deref(),
                    candidate,
                )?;
            }
        }
        Ok(())
    }

    fn store_reconciliation_paths(
        &self,
        path: &ManagedPath,
        candidate: &PrivateCommit,
    ) -> Result<(PathBuf, PathBuf), RepositoryError> {
        let target = self.managed_target(path)?;
        let parent = target
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(RepositoryError::UnsafeManagedPath)?;
        let quarantine = parent.join(format!(
            ".orna-store-reconcile-{}-{name}",
            candidate.commit().as_str()
        ));
        Ok((target, quarantine))
    }

    fn materialize_store_worktree_file(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        let (target, quarantine) = self.store_reconciliation_paths(path, candidate)?;
        let parent = target
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let current = self.read_managed_file(&target)?;
        let displaced = self.read_managed_file(&quarantine)?;

        if let Some(displaced) = displaced.as_deref() {
            if Some(displaced) != expected {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if current.as_deref() == next {
                return self.remove_verified_quarantine(
                    parent,
                    &quarantine,
                    &target,
                    expected,
                    next,
                );
            }
            if current.is_none() {
                if let Some(next) = next {
                    self.materialize_managed_file_impl_with_quarantine_locked(
                        path,
                        None,
                        Some(next),
                        None,
                        None,
                        None,
                    )?;
                }
                return self.remove_verified_quarantine(
                    parent,
                    &quarantine,
                    &target,
                    expected,
                    next,
                );
            }
            return Err(RepositoryError::ManagedContentConflict);
        }

        if current.as_deref() == next {
            return Ok(());
        }
        if current.as_deref() != expected {
            return Err(RepositoryError::ManagedContentConflict);
        }
        let quarantine_path = expected.map(|_| quarantine.as_path());
        self.materialize_managed_file_impl_with_quarantine_locked(
            path,
            expected,
            next,
            quarantine_path,
            None,
            None,
        )
    }

    /// Atomically persists the current publication journal in private runtime
    /// state. The ordinary Git namespace is not touched.
    pub fn write_publication_journal(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.write_publication_journal_locked(journal)
    }

    pub(crate) fn write_publication_journal_locked(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        self.require_no_pending_checkout_recovery_locked()?;
        let encoded = journal.encode()?;
        self.runtime.ensure_exists()?;
        let path = self.runtime.root().join("publication-journal.bin");
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let temporary = self.runtime.root().join(format!(
            ".publication-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.write_all(&encoded)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.sync_all()
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::rename(&temporary, &path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::File::open(self.runtime.root())
                .and_then(|directory| directory.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// Reads the last atomically persisted publication journal, if present.
    pub fn read_publication_journal(&self) -> Result<Option<PublicationJournal>, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.read_publication_journal_locked()
    }

    /// Reports whether a persisted journal belongs to a reviewed commit. The
    /// review anchor naming the candidate is pinned before any journal is
    /// written, so a journal whose intent has an anchor at its candidate is
    /// completed by `commit_reviewed_candidate` and never by a runtime freeze.
    pub fn publication_journal_is_reviewed(
        &self,
        journal: &PublicationJournal,
    ) -> Result<bool, RepositoryError> {
        let Some(intent) = journal.runtime_intent_id() else {
            return Ok(false);
        };
        let anchor = format!(
            "refs/orna/review-candidates/{}",
            intent.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
        );
        let pinned = self.commit_optional(&format!("{anchor}^{{commit}}"))?;
        Ok(pinned.is_some_and(|commit| commit.as_str() == journal.new_head().as_str()))
    }

    pub(crate) fn read_publication_journal_locked(
        &self,
    ) -> Result<Option<PublicationJournal>, RepositoryError> {
        let path = self.runtime.root().join("publication-journal.bin");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(Some(PublicationJournal::decode(
            &bytes,
            self.native_object_id_length()?,
        )?))
    }

    /// Removes a completed or abandoned journal idempotently and flushes the
    /// containing private runtime directory.
    pub fn clear_publication_journal(&self) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.clear_publication_journal_locked()
    }

    pub(crate) fn clear_publication_journal_locked(&self) -> Result<(), RepositoryError> {
        self.require_no_pending_checkout_recovery_locked()?;
        let path = self.runtime.root().join("publication-journal.bin");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RepositoryError::InvalidPublicationJournal);
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                fs::File::open(self.runtime.root())
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        self.sweep_publication_pins_locked(&[])?;
        Ok(())
    }

    /// Pins the objects an unfinished publication journal needs for recovery
    /// (ORNA-GIT-010): its base index tree and its candidate commit. The pins
    /// are private and owned by the journal. They are adopted before Git's
    /// index lock is taken or the Prepared journal is written, and every pin
    /// that no journal names is swept, so GC and repack cannot prune the
    /// objects reconciliation reads.
    fn adopt_publication_pins_locked(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let pins = Self::publication_pins(journal);
        let keep: Vec<String> = pins
            .iter()
            .map(|(reference, _)| reference.clone())
            .collect();
        self.sweep_publication_pins_locked(&keep)?;
        for (reference, oid) in &pins {
            self.pin_publication_object_locked(reference, oid)?;
        }
        Ok(())
    }

    /// The pin refs and the objects they keep for one journal.
    fn publication_pins(journal: &PublicationJournal) -> Vec<(String, String)> {
        let mut pins = Vec::new();
        if let Some(tree) = journal.base_index_tree.as_ref() {
            pins.push((
                Self::publication_index_pin_ref(tree.as_str()),
                tree.as_str().to_owned(),
            ));
        }
        pins.push((
            Self::publication_candidate_pin_ref(journal.new_head.as_str()),
            journal.new_head.as_str().to_owned(),
        ));
        pins
    }

    /// Releases the pins of a failed publication attempt. A retained journal
    /// keeps its own pins; with no journal every publication pin is swept.
    /// Acquires the coordination lock, so callers must not hold it.
    fn release_unjournaled_publication_pins(&self) -> Result<(), RepositoryError> {
        let _coordination_lock = self.acquire_coordination_lock()?;
        let keep: Vec<String> = match self.read_publication_journal_locked()? {
            Some(journal) => Self::publication_pins(&journal)
                .into_iter()
                .map(|(reference, _)| reference)
                .collect(),
            None => Vec::new(),
        };
        self.sweep_publication_pins_locked(&keep)
    }

    fn publication_index_pin_ref(tree: &str) -> String {
        format!("refs/orna/publication-indexes/{tree}")
    }

    fn publication_candidate_pin_ref(commit: &str) -> String {
        format!("refs/orna/publication-candidates/{commit}")
    }

    /// Pins one object a journal names. An object Git no longer holds cannot
    /// be kept alive, so it is left unpinned: the recovery path already
    /// classifies a lost candidate or base tree (ORNA-GIT-010).
    fn pin_publication_object_locked(
        &self,
        reference: &str,
        oid: &str,
    ) -> Result<(), RepositoryError> {
        if !self.publication_object_present(oid)? {
            return Ok(());
        }
        let zero = "0".repeat(oid.len());
        let mut update = self.command();
        update.args(["update-ref", "--no-deref", reference, oid, zero.as_str()]);
        if self.run(update).is_ok() {
            return Ok(());
        }
        // A GC that removed the object after the check above makes the create
        // fail. That is the same absent-object case: leave it unpinned.
        if !self.publication_object_present(oid)? {
            return Ok(());
        }
        // Creation is refused when the pin already names this object. Accept
        // only that exact pin; any other value is a conflicting journal pin.
        let mut verify = self.command();
        verify.args(["rev-parse", "--verify", "--quiet", reference]);
        let output = verify
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() && trim_output(&output.stdout) == oid {
            Ok(())
        } else {
            Err(RepositoryError::InvalidPublicationJournal)
        }
    }

    /// `git cat-file -e` exits 0 when the object exists and 1 when it is
    /// absent. Any other status (a fatal repository error, a corrupt object
    /// store) is an error: reading it as "absent" would skip the pin while
    /// the journal naming the object is still written.
    fn publication_object_present(&self, oid: &str) -> Result<bool, RepositoryError> {
        let mut exists = self.command();
        exists.args(["cat-file", "-e", oid]);
        let output = exists
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    /// Deletes every publication pin not named in `keep`. Callers hold the
    /// coordination lock, so no publisher can sit between its pin and its
    /// journal. With no journal no pin may survive; with a journal only its
    /// own pins may (ORNA-GIT-010).
    fn sweep_publication_pins_locked(&self, keep: &[String]) -> Result<(), RepositoryError> {
        let mut list = self.command();
        list.args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/orna/publication-indexes/",
            "refs/orna/publication-candidates/",
        ]);
        let listing = self.run(list)?.stdout;
        for line in listing
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let reference =
                std::str::from_utf8(line).map_err(|_| RepositoryError::GitOperationFailed)?;
            if keep.iter().any(|kept| kept == reference) {
                continue;
            }
            let mut delete = self.command();
            delete.args(["update-ref", "-d", "--no-deref", reference]);
            self.run(delete)?;
        }
        Ok(())
    }

    fn reclaim_abandoned_publication_index_lock(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let current = self
            .read_publication_journal_locked()?
            .ok_or(RepositoryError::GitIndexLockPresent)?;
        if &current != journal {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let index = self.git_path("index")?;
        GitIndexLock::reclaim_abandoned(index.with_extension("lock"), journal.lock_binding()?)
    }

    /// Reclaims an Orna-marked `index.lock` left by a publisher that stopped
    /// after taking Git's lock but before writing its Prepared journal. No
    /// journal binds such a lock, so the Orna marker must carry a publisher
    /// binding (not the all-zero ordinary-writer binding) and the owner must be
    /// proven dead. Any other lock is left in place for Git and does not fail
    /// recovery; the next publisher reports it as present.
    fn reclaim_orphan_publication_index_lock(&self) -> Result<(), RepositoryError> {
        let index = self.git_path("index")?;
        match GitIndexLock::reclaim_unbound_orphan(index.with_extension("lock")) {
            Err(RepositoryError::GitIndexLockPresent) => Ok(()),
            result => result,
        }
    }

    fn write_checkout_recovery_journal_locked(
        &self,
        journal: &CheckoutRecoveryJournal,
    ) -> Result<(), RepositoryError> {
        self.require_no_pending_publication_recovery_locked()?;
        let encoded = journal.encode()?;
        self.runtime.ensure_exists()?;
        let path = self.runtime.root().join("checkout-journal.bin");
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let temporary = self.runtime.root().join(format!(
            ".checkout-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .as_nanos()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.write_all(&encoded)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.sync_all()
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::rename(&temporary, &path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            fs::File::open(self.runtime.root())
                .and_then(|directory| directory.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn read_checkout_recovery_journal_locked(
        &self,
    ) -> Result<Option<CheckoutRecoveryJournal>, RepositoryError> {
        let path = self.runtime.root().join("checkout-journal.bin");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::InvalidCheckoutJournal);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(Some(CheckoutRecoveryJournal::decode(
            &bytes,
            self.native_object_id_length()?,
        )?))
    }

    fn clear_checkout_recovery_journal_locked(&self) -> Result<(), RepositoryError> {
        self.require_no_pending_publication_recovery_locked()?;
        let path = self.runtime.root().join("checkout-journal.bin");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(RepositoryError::InvalidCheckoutJournal);
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                fs::File::open(self.runtime.root())
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        Ok(())
    }

    /// Reviews the exact index candidate using identity, generation, snapshot, and
    /// format observations supplied by the durable runtime and this repository.
    pub fn review_current_candidate(
        &self,
        capture: &orna_foundation_v1::CwdCapture,
    ) -> Result<ReviewedCandidateReview, RepositoryError> {
        let digits = capture.generation().to_u64_digits().1;
        if digits.len() > 1 {
            return Err(RepositoryError::LocalStateUnavailable);
        }
        let generation = digits.first().copied().unwrap_or(0);
        let repository_format = match self.head()? {
            Some(head) => self
                .open_pinned_format_context(head.as_str())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?
                .repository_format(),
            // An unborn branch has no committed format: its first commit takes
            // the format its staged metadata declares.
            None => self.staged_repository_format()?,
        };
        let descriptor = capture.snapshot().encode()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.review_staged_candidate(
            Uuid::from_bytes(capture.database_id()),
            Uuid::from_bytes(capture.runtime_id()),
            generation,
            repository_format,
            RuntimeGeneration::new(generation),
            descriptor,
        )
    }
    /// Captures the actual index tree and CWD observation without mutating
    /// the ordinary index, worktree, or branch.
    pub(crate) fn review_staged_candidate(
        &self,
        database_id: Uuid,
        runtime_id: Uuid,
        pending_generation: u64,
        repository_format: RepositoryFormat,
        runtime: RuntimeGeneration,
        cwd_snapshot_descriptor: Vec<u8>,
    ) -> Result<ReviewedCandidateReview, RepositoryError> {
        let _coordination_lock = self.acquire_coordination_lock()?;
        let cwd = self.cwd_generation_locked(runtime)?;
        let index_tree = cwd.index.tree().cloned().ok_or(RepositoryError::UnbornHead)?;
        let index = self.git_path("index")?;
        let index_bytes = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if self.index_tree_from_file(&index)? != Some(index_tree.clone())
            || self.head()? != cwd.head().cloned()
            || self.current_branch()? != cwd.branch().map(str::to_owned)
        {
            return Err(RepositoryError::StaleCwd);
        }
        self.objects().check_expansion(index_tree.as_str(), "")?;
        if let Some(head) = cwd.head() {
            self.objects().check_expansion(head.as_str(), "")?;
        }
        let mut path_diff = self.command();
        match cwd.head() {
            Some(head) => path_diff
                .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "-z"])
                .arg(head.as_str())
                .arg(index_tree.as_str()),
            // On an unborn branch every staged path is new.
            None => path_diff
                .args(["ls-tree", "-r", "--name-only", "-z", "--full-tree"])
                .arg(index_tree.as_str()),
        };
        let candidate_paths = self.run(path_diff)?.stdout.split(|byte| *byte == 0).filter(|path| !path.is_empty()).map(|path| String::from_utf8(path.to_vec()).map_err(|_| RepositoryError::UnsafeManagedPath)).collect::<Result<Vec<_>, _>>()?;
        let mut review = ReviewedCandidateReview {
            database_id, runtime_id, pending_generation, repository_format,
            head_ref: cwd.branch().map(str::to_owned), cwd, cwd_snapshot_descriptor, candidate_paths,
            index_bytes_digest: Sha256::digest(&index_bytes).into(), index_tree, token: [0; 32],
        };
        review.token = reviewed_candidate_token(&review);
        Ok(review)
    }

    fn validate_candidate_assertions(
        &self, gate: &dyn CandidateAssertionGate, candidate: &PrivateCommit,
    ) -> Result<(), RepositoryError> {
        gate.validate(self, candidate).map_err(RepositoryError::CandidateAssertionRefused)
    }

    pub fn commit_reviewed_candidate(
        &self, review: &ReviewedCandidateReview, message: &str,
        gate: &dyn CandidateAssertionGate,
    ) -> Result<GitCommitRef, RepositoryError> {
        self.commit_reviewed_candidate_with_stage_hook(review, message, gate, |_| {})
    }
    fn resume_reviewed_candidate_publication(
        &self,
        review: &ReviewedCandidateReview,
        runtime_intent_id: [u8; 16],
        result_path: &Path,
        intent_hex: &str,
        message_digest_hex: &str,
    ) -> Result<Option<GitCommitRef>, RepositoryError> {
        let Some(mut journal) = self.read_publication_journal()? else {
            return Ok(None);
        };
        if journal.runtime_intent_id() != Some(runtime_intent_id) {
            return Ok(None);
        }
        if journal.base_index_tree() != Some(&review.index_tree) {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        if matches!(journal.stage(), PublicationJournalStage::Prepared | PublicationJournalStage::RefAdvanced | PublicationJournalStage::IndexReconciled) {
            match self.recover_publication() {
                Ok(_) | Err(RepositoryError::RuntimeCompletionRequired) => {}
                Err(error) => return Err(error),
            }
            let Some(recovered) = self.read_publication_journal()? else {
                return Ok(None);
            };
            journal = recovered;
        }
        let commit = journal.new_head().clone();
        let record = format!("{intent_hex}\n{message_digest_hex}\n{}", commit.as_str());
        match journal.stage() {
            PublicationJournalStage::WorktreeReconciled => {
                fs::write(result_path, &record).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                self.mark_runtime_complete(runtime_intent_id, &mut journal)?;
            }
            PublicationJournalStage::RuntimeCompleted | PublicationJournalStage::Complete => {
                fs::write(result_path, &record).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                self.recover_publication()?;
            }
            _ => return Err(RepositoryError::RuntimeCompletionRequired),
        }
        Ok(Some(commit))
    }
    fn commit_reviewed_candidate_with_stage_hook(
        &self,
        review: &ReviewedCandidateReview,
        message: &str,
        gate: &dyn CandidateAssertionGate,
        mut on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<GitCommitRef, RepositoryError> {
        if message.is_empty() || message.contains('\0') {
            return Err(RepositoryError::InvalidCommitMessage);
        }
        if review.token != reviewed_candidate_token(review) {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        let runtime_intent_id = reviewed_candidate_runtime_intent_id(review, message);
        let intent_hex = runtime_intent_id.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let message_digest_hex = Sha256::digest(message.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let result_path = self.runtime.root().join(format!("reviewed-result-{intent_hex}"));
        let recorded_commit = if let Ok(recorded) = fs::read_to_string(&result_path) {
            let mut fields = recorded.lines();
            let Some(recorded_intent) = fields.next() else {
                return Err(RepositoryError::LocalStateUnavailable);
            };
            let Some(recorded_message_digest) = fields.next() else {
                return Err(RepositoryError::LocalStateUnavailable);
            };
            let Some(recorded_commit) = fields.next() else {
                return Err(RepositoryError::StaleReviewedCandidate);
            };
            if fields.next().is_some() {
                return Err(RepositoryError::LocalStateUnavailable);
            }
            if recorded_intent != intent_hex || recorded_message_digest != message_digest_hex {
                return Err(RepositoryError::StaleReviewedCandidate);
            }
            Some(self.snapshot_from_native_oid(recorded_commit.trim().to_owned())?)
        } else {
            None
        };
        if let Some(recovered) = self.resume_reviewed_candidate_publication(
            review, runtime_intent_id, &result_path, &intent_hex, &message_digest_hex,
        )? {
            if recorded_commit.as_ref().is_some_and(|recorded| recorded != &recovered) {
                return Err(RepositoryError::StaleReviewedCandidate);
            }
            return Ok(recovered);
        }
        if let Some(recorded) = recorded_commit {
            return Ok(recorded);
        }
        if review.candidate_paths.is_empty() {
            return Err(RepositoryError::SysGitNothingToCommit);
        }
        let actual_cwd = self.cwd_generation(review.cwd.runtime())?;
        let index = self.git_path("index")?;
        let index_bytes = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if actual_cwd != review.cwd
            || self.current_branch()? != review.head_ref
            || Sha256::digest(&index_bytes).as_slice() != review.index_bytes_digest
        {
            return Err(RepositoryError::StaleReviewedCandidate);
        }

        // 16-administration `commit`: a missing identity fails before any commit
        // object exists. The table assertions are evaluated over the serialized
        // candidate before its journal or any ref names it, so a refused
        // candidate leaves at most an unreachable commit object and no ref.
        self.require_configured_commit_identity()?;
        if review.repository_format != RepositoryFormat::Format3 {
            return Err(RepositoryError::InvalidCommitCandidate);
        }
        self.validate_format3_index_tree(&review.index_tree)
            .map_err(commit_gate_refusal)?;
        let Some(old_head) = review.cwd.head().cloned() else {
            return self.commit_reviewed_root_candidate(
                review,
                message,
                gate,
                &result_path,
                &intent_hex,
                &message_digest_hex,
            );
        };
        let mut commit_tree = self.configured_identity_command();
        commit_tree
            .args(["commit-tree", review.index_tree.as_str(), "-p", old_head.as_str(), "-m"])
            .arg(message);
        // Like every other publication candidate, the commit stays unreachable
        // until the branch compare-and-set names it; no private ref anchors it
        // (ORNA-PUB-005, ORNA-GIT-008).
        let candidate = PrivateCommit {
            tree: review.index_tree.clone(),
            commit: self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?,
        };
        self.validate_candidate_assertions(gate, &candidate)?;
        let mut diff = self.command();
        diff.args(["diff", "--no-renames", "--name-status", "-z"]).arg(old_head.as_str()).arg(candidate.commit().as_str());
        let changed_paths = self.run(diff)?.stdout;
        let mut fields = changed_paths
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty());
        let mut entries = Vec::new();
        while let Some(status) = fields.next() {
            let raw_path = fields.next().ok_or(RepositoryError::GitOperationFailed)?;
            let path =
                std::str::from_utf8(raw_path).map_err(|_| RepositoryError::UnsafeManagedPath)?;
            // The staged store is bound and reconciled as the one
            // `.orna/store` tree entry, like a publication's store root: its
            // node files are not managed files, and payload chunks are never
            // materialized in the worktree. A path the candidate deletes is
            // already absent from the reviewed index, and the worktree keeps
            // its unstaged state. Neither is a journal entry.
            if path.starts_with(".orna/store/") || status == b"D" {
                continue;
            }
            let path = ManagedPath::new(path)?;
            let target = self.managed_target(&path)?;
            let current = self.read_managed_file(&target)?;
            entries.push(PublicationJournalEntry::new(path, current.clone(), current));
        }
        if entries.is_empty() {
            // A candidate that only changes the store or deletes paths keeps
            // the unchanged metadata file as its durable journal anchor.
            let marker = ManagedPath::new(".orna/database.orna")?;
            let current = self.read_managed_file(&self.managed_target(&marker)?)?;
            entries.push(PublicationJournalEntry::new(
                marker,
                current.clone(),
                current,
            ));
        }
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head, candidate.commit().clone(), review.index_tree.clone(), runtime_intent_id, entries,
        )?;
        self.publish_candidate_impl_with_migration(
            &review.cwd.index, &candidate, &mut journal, false, Some(review), &mut on_stage,
        )?;
        on_stage(PublicationJournalStage::WorktreeReconciled);
        fs::write(&result_path, format!("{intent_hex}\n{message_digest_hex}\n{}", candidate.commit().as_str()))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.mark_runtime_complete(runtime_intent_id, &mut journal)?;
        Ok(candidate.commit().clone())
    }

    /// Commits a reviewed candidate on an unborn branch. "The first actual
    /// commit creates that branch ref" (16-administration, unborn branches):
    /// the commit names exactly the reviewed index tree with no parent, and
    /// the symbolic branch moves from absent to it by compare-and-set. The
    /// ordinary index already equals the new commit's tree and the worktree
    /// keeps its unstaged differences, so there is no index or worktree
    /// reconciliation to journal. Like every other ref advance, the commit
    /// and all of its objects are flushed before the ref names them, and the
    /// ref is flushed before the result is recorded (ORNA-PUB-005).
    fn commit_reviewed_root_candidate(
        &self,
        review: &ReviewedCandidateReview,
        message: &str,
        gate: &dyn CandidateAssertionGate,
        result_path: &Path,
        intent_hex: &str,
        message_digest_hex: &str,
    ) -> Result<GitCommitRef, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_no_git_operation_in_progress()?;
        // The index is not rewritten, so Git's index lock is not taken: a
        // concurrent `git add` after this check stays staged against the new
        // commit, and the branch itself moves only by compare-and-set.
        self.verify_reviewed_candidate_locked(review)?;
        if self.head()?.is_some() {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        let reference = self.symbolic_head_ref()?;

        let mut commit_tree = self.configured_identity_command();
        commit_tree
            .args(["commit-tree", review.index_tree.as_str(), "-m"])
            .arg(message);
        let candidate = PrivateCommit {
            tree: review.index_tree.clone(),
            commit: self.snapshot_from_native_oid(trim_output(&self.run(commit_tree)?.stdout))?,
        };
        let context = self
            .open_private_candidate_format_context(&candidate)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        if context.repository_format() != review.repository_format
            || context.repository_format() != RepositoryFormat::Format3
            || context.validate_schema_root().is_err()
            || context.validate_store_root().is_err()
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        self.validate_candidate_assertions(gate, &candidate)?;
        // PUB-3 step 3: complete objects and their durability barriers
        // precede the visible ref. A ref backend whose refs cannot be
        // flushed is refused before the branch is created.
        native_graph::require_files_ref_backend(self).map_err(RepositoryError::Graph)?;
        self.sync_candidate_objects(None, candidate.commit())?;
        let absent = "0".repeat(self.native_object_id_length()?);
        let mut create = self.command();
        create.args([
            "update-ref",
            reference.as_str(),
            candidate.commit().as_str(),
            absent.as_str(),
        ]);
        self.run(create)?;
        native_graph::sync_git_ref_durably(self, &reference).map_err(RepositoryError::Graph)?;
        if self.head()?.as_ref() != Some(candidate.commit()) {
            return Err(RepositoryError::StaleHead);
        }
        fs::write(
            result_path,
            format!("{intent_hex}\n{message_digest_hex}\n{}", candidate.commit().as_str()),
        )
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(candidate.commit().clone())
    }

    /// A Git command that records only a configured identity: Git refuses to
    /// guess an author or committer from `EMAIL`, the account name or the
    /// host name (`user.useConfigOnly`). Configuration and Git's explicit
    /// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` variables still apply.
    fn configured_identity_command(&self) -> Command {
        let mut command = self.command();
        command.args(["-c", "user.useConfigOnly=true"]);
        command
    }

    /// A Git command that records a private staged-identity carrier (the
    /// unreachable commit `capture_staged_candidate` names for loading). A
    /// carrier is never ref-advanced, so it is written on Orna's behalf and
    /// never reads the ambient identity. A machine or CI runner without
    /// `user.name`/`user.email` therefore builds it the same way. The identity
    /// is fixed here and never written to Git config; timestamps remain Git's.
    /// Publication candidates ref-advance the user's branch and keep
    /// `command()`, so they record the configured identity (ORNA-PUB-010).
    fn private_commit_command(&self) -> Command {
        const NAME: &str = "Orna Internal";
        const EMAIL: &str = "orna-internal@localhost.invalid";
        let mut command = self.command();
        command
            .env("GIT_AUTHOR_NAME", NAME)
            .env("GIT_AUTHOR_EMAIL", EMAIL)
            .env("GIT_COMMITTER_NAME", NAME)
            .env("GIT_COMMITTER_EMAIL", EMAIL);
        command
    }

    /// Requires the author and committer identity a reviewed commit records
    /// to be configured. 16-administration: "Default author information comes
    /// from configured Git identity; missing identity fails before creating a
    /// commit." Both identities are resolved by Git itself, exactly as the
    /// following `commit-tree` resolves them.
    pub fn require_configured_commit_identity(&self) -> Result<(), RepositoryError> {
        for identity in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
            let mut command = self.configured_identity_command();
            command
                .args(["var", identity])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let status = command
                .status()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if !status.success() {
                return Err(RepositoryError::MissingGitIdentity);
            }
        }
        Ok(())
    }

    fn verify_reviewed_candidate_locked(
        &self,
        review: &ReviewedCandidateReview,
    ) -> Result<(), RepositoryError> {
        if review.token != reviewed_candidate_token(review) {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        let actual = self.cwd_generation_locked(review.cwd.runtime())?;
        let index = self.git_path("index")?;
        let bytes = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if actual != review.cwd
            || self.current_branch()? != review.head_ref
            || actual.index.tree() != Some(&review.index_tree)
            || Sha256::digest(bytes).as_slice() != review.index_bytes_digest
        {
            return Err(RepositoryError::StaleReviewedCandidate);
        }
        Ok(())
    }

    /// Publishes a prepared candidate through the ref, index and worktree
    /// boundaries. A refusal before the ref CAS leaves no journal behind; an
    /// error after the CAS leaves the journal available for recovery.
    pub fn publish_candidate(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
    ) -> Result<IndexGeneration, RepositoryError> {
        self.publish_candidate_impl_with_migration(expected_index, candidate, journal, false, None, |_| {})
    }
    /// Executes publication and observes each durable journal stage, from
    /// the Prepared intent through the reconciled index.
    pub fn publish_candidate_with_stage_hook(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
        on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<IndexGeneration, RepositoryError> {
        self.publish_candidate_impl_with_migration(
            expected_index,
            candidate,
            journal,
            false,
            None,
            on_stage,
        )
    }


    /// Explicitly publishes a caller-built format-3 migration from a captured
    /// format-1/2 snapshot through the shared durable PUB-3 journal.
    ///
    /// This method never converts or relabels legacy data. The caller must
    /// provide the complete private candidate and prepared publication
    /// journal. The candidate must be a direct child of the captured legacy
    /// commit and admit as format 3 with valid source and store roots. Until
    /// the runtime owner completes the journal's intent, the old commit and
    /// runtime tail remain available for recovery.
    pub fn publish_legacy_format_migration(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
    ) -> Result<IndexGeneration, RepositoryError> {
        if journal.migration_continuity().is_none()
            || journal.migration_annotation_defaults().is_none()
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        let old_head = expected_index
            .head()
            .ok_or(RepositoryError::InvalidFormatMigration)?;
        if journal.old_head() != old_head || journal.new_head() != candidate.commit() {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        let legacy_snapshot = self
            .pin_snapshot(old_head.as_str())
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        let legacy_context = self
            .open_format_context_at(&legacy_snapshot)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        if !matches!(
            legacy_context.repository_format(),
            RepositoryFormat::Legacy1 | RepositoryFormat::Legacy2
        ) {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        let candidate_context = self
            .open_private_candidate_format_context(candidate)
            .map_err(|_| RepositoryError::InvalidFormatMigration)?;
        let candidate_format3 = candidate_context.repository_format() == RepositoryFormat::Format3;
        let candidate_schema_valid = candidate_context.validate_schema_root().is_ok();
        let candidate_store_valid = candidate_context.validate_store_root().is_ok();
        if !candidate_format3 || !candidate_schema_valid || !candidate_store_valid {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        let parent = self.run({
            let mut command = self.command();
            command.args([
                "rev-list",
                "--parents",
                "--no-walk",
                candidate.commit().as_str(),
            ]);
            command
        })?;
        let parent_line = trim_output(&parent.stdout);
        let parents = parent_line
            .split_ascii_whitespace()
            .skip(1)
            .collect::<Vec<_>>();
        if parents.as_slice() != [old_head.as_str()] {
            return Err(RepositoryError::InvalidFormatMigration);
        }

        self.publish_candidate_impl_with_migration(expected_index, candidate, journal, true, None, |_| {})
    }

    /// The common PUB-1 ref/index/worktree machinery. A compact journal is
    /// refused: its retired writer has no completion path in this build.
    /// Pins adopted by an attempt that no journal names are released before
    /// the error is returned (ORNA-GIT-010).
    fn publish_candidate_impl_with_migration(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
        permit_legacy_migration: bool,
        review: Option<&ReviewedCandidateReview>,
        on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<IndexGeneration, RepositoryError> {
        let published = self.publish_candidate_body(
            expected_index,
            candidate,
            journal,
            permit_legacy_migration,
            review,
            on_stage,
        );
        if published.is_err() {
            // The refusal is the caller's error. If the release itself fails,
            // the pins remain for the next recovery to sweep.
            let _ = self.release_unjournaled_publication_pins();
        }
        published
    }

    /// The body of [`Self::publish_candidate_impl_with_migration`]. It takes
    /// the coordination lock itself and releases it before returning.
    fn publish_candidate_body(
        &self,
        expected_index: &IndexGeneration,
        candidate: &PrivateCommit,
        journal: &mut PublicationJournal,
        permit_legacy_migration: bool,
        review: Option<&ReviewedCandidateReview>,
        mut on_stage: impl FnMut(PublicationJournalStage),
    ) -> Result<IndexGeneration, RepositoryError> {
        if journal.migration_continuity().is_some() != permit_legacy_migration
            || journal.migration_annotation_defaults().is_some() != permit_legacy_migration
            || (permit_legacy_migration && journal.compact_manifest().is_some())
        {
            return Err(RepositoryError::InvalidFormatMigration);
        }
        if journal.stage() != PublicationJournalStage::Prepared
            || journal.runtime_intent_id().is_none()
            || expected_index.head() != Some(journal.old_head())
            || journal.new_head() != candidate.commit()
            || journal.base_index_tree() != expected_index.tree()
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if journal.compact_manifest().is_some() {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        let _coordination_lock = self.acquire_coordination_lock_for_publication()?;
        if let Some(review) = review { self.verify_reviewed_candidate_locked(review)?; }
        self.require_no_pending_checkout_recovery_locked()?;
        self.ensure_no_git_operation_in_progress()?;
        // A retained PUB-1 journal is the sole recovery authority for its
        // candidate. Never replace it with another publisher's prepared
        // journal: doing so would discard the first candidate's exact
        // ref/index/worktree evidence before recovery can reconcile it.
        let resumes_retained_journal = match self.read_publication_journal_locked()? {
            // Compact publication persists this exact prepared journal before
            // it invokes the common PUB-1 boundary, so an exact retry is the
            // same recovery operation, not a second publisher. Any differing
            // journal remains fenced rather than replacing its evidence.
            Some(retained) if retained != *journal => {
                // Ordering (ORNA-PUB-009 before recovery signal): a head that has
                // already moved past this publisher's expected head is an ordinary
                // lost CAS -> StaleHead (retriable). The retained batch is untouched.
                // Only an unmoved head with a foreign retained journal means the
                // earlier publication has not advanced the ref: PublicationPending
                // (ORNA-PUB-006 recovery required).
                if self.head()?.as_ref() != Some(journal.old_head()) {
                    return Err(RepositoryError::StaleHead);
                }
                return Err(RepositoryError::PublicationPending);
            }
            retained => retained.is_some(),
        };
        let paths = self.publication_index_paths(candidate, journal)?;
        self.ensure_atomic_index_install_supported()?;
        let index = self.git_path("index")?;
        let base_index = self.capture_index_for_expected(expected_index, &index)?;
        self.adopt_publication_pins_locked(journal)?;
        let lock_binding = journal.lock_binding()?;
        // Own Git's writer lock before persisting a prepared intent so lock contention
        // cannot strand a timestamped journal that blocks a fresh retry. This closes the
        // post-ref/pre-index race where an ordinary Git writer could publish
        // the captured, stale index.
        let git_lock = GitIndexLock::acquire_owned(index.with_extension("lock"), lock_binding)?;
        // ORNA-PUB-006/PUB-016: the Orna-marked index lock now exists but no
        // Prepared journal does; recovery must reclaim it without a binding.
        crate::publication_crash_barrier("index-lock");
        // PUB-3 step 7: every precondition of the ref CAS is revalidated under
        // the lock before the Prepared journal exists. A refusal here leaves
        // no record that could fence the pending batch (ORNA-PUB-006).
        self.ensure_no_git_operation_in_progress()?;
        let selected_ref = journal
            .compact_manifest()
            .map(|witness| witness.selected_ref());
        self.advance_target_ref(journal.old_head(), selected_ref)?;
        if let Some(review) = review {
            let bytes = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if Sha256::digest(bytes).as_slice() != review.index_bytes_digest
                || self.head()? != review.cwd.head().cloned()
                || self.current_branch()? != review.head_ref
                || self.worktree_state()? != *review.cwd.worktree()
            {
                return Err(RepositoryError::StaleReviewedCandidate);
            }
        }
        // A managed file may have changed after preparation but before the
        // publication boundary.  Refuse that known conflict before advancing
        // the ref; the later worktree check still protects the unavoidable
        // race with an editor that writes during publication.
        for entry in journal.entries() {
            let target = self.managed_target(&entry.path)?;
            let current = self.read_managed_file(&target)?;
            if current.as_deref() != entry.expected() && current.as_deref() != entry.next() {
                return Err(RepositoryError::ManagedContentConflict);
            }
        }
        self.reconcile_store_worktree_transition(expected_index, candidate, false)?;
        self.write_publication_journal_locked(journal)?;
        on_stage(PublicationJournalStage::Prepared);
        // PUB-3 step 7: the reconciled index is built from `base_index`, so
        // the ordinary index must still hold exactly those bytes when the ref
        // moves. A write that landed after capture, including one just before
        // this attempt took Git's lock, is refused here rather than replaced
        // by an index that stages the inverse of the batch (ORNA-PUB-011,
        // ORNA-PUB-017).
        let advanced = self
            .require_index_unchanged(expected_index, &index, &base_index)
            .and_then(|()| {
                self.advance_current_ref_bound(journal.old_head(), candidate, selected_ref)
            });
        if let Err(error) = advanced {
            // After a refused or lost CAS, a candidate that neither HEAD nor
            // any branch contains shows that the journal this attempt wrote
            // names nothing that was published: discard it and keep the batch
            // pending. A journal retained from an earlier attempt stays with
            // recovery.
            if !resumes_retained_journal
                && !self.head_contains_commit(candidate.commit())?
                && !self.branch_contains_commit(candidate.commit())?
            {
                self.clear_publication_journal_locked()?;
            }
            return Err(error);
        }
        // Gate D boundary: the ref CAS has succeeded but the RefAdvanced
        // journal stage is not yet durable. Recovery sees a Prepared journal
        // whose HEAD already names the candidate (ORNA-PUB-009, ORNA-PUB-005).
        crate::publication_crash_barrier("ref-cas-pre-journal");

        journal.advance(PublicationJournalStage::RefAdvanced)?;
        self.write_publication_journal_locked(journal)?;
        on_stage(PublicationJournalStage::RefAdvanced);
        let reconciled = self.reconcile_published_index_locked(
            expected_index,
            candidate,
            &paths,
            &base_index,
            &git_lock,
        )?;
        // Gate D boundary: the reconciled index is installed but its stage is
        // not yet durable, so recovery must recognise the installed index.
        crate::publication_crash_barrier("index-install");
        journal.advance(PublicationJournalStage::IndexReconciled)?;
        self.write_publication_journal_locked(journal)?;
        on_stage(PublicationJournalStage::IndexReconciled);
        for index in 0..journal.entries().len() {
            self.materialize_publication_entry_locked(journal, index)?;
        }
        // Gate D boundary: every managed entry has been materialised but the
        // WorktreeReconciled stage is not yet recorded (ORNA-PUB-016).
        crate::publication_crash_barrier("worktree");

        journal.advance(PublicationJournalStage::WorktreeReconciled)?;
        self.write_publication_journal_locked(journal)?;
        Ok(reconciled)
    }

    /// Records that the separately owned runtime transaction consumed the
    /// exact frozen intent named by the journal. The caller must invoke this
    /// only after runtime completion has committed successfully.
    pub fn mark_runtime_complete(
        &self,
        runtime_intent_id: [u8; 16],
        journal: &mut PublicationJournal,
    ) -> Result<(), RepositoryError> {
        if journal.compact_manifest().is_some()
            || journal.stage() != PublicationJournalStage::WorktreeReconciled
            || journal.runtime_intent_id() != Some(runtime_intent_id)
        {
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        let _coordination_lock = self.acquire_coordination_lock()?;
        self.require_no_pending_checkout_recovery_locked()?;
        // ORNA-PUB-009 and recovery row "Ref no longer H or N": a normal Git
        // writer may advance HEAD past the candidate after reconciliation.
        // Ancestry proves the batch is present, so completion proceeds without
        // rewriting the ref, index or worktree. Any HEAD that does not contain
        // the candidate is a recovery conflict.
        if !self.head_contains_commit(journal.new_head())? {
            return Err(RepositoryError::StaleHead);
        }
        self.verify_index_at_candidate(journal)?;
        journal.advance(PublicationJournalStage::RuntimeCompleted)?;
        self.write_publication_journal_locked(journal)?;
        journal.advance(PublicationJournalStage::Complete)?;
        self.write_publication_journal_locked(journal)?;
        self.clear_publication_journal_locked()
    }

    /// Resumes a persisted publication after a process interruption. The
    /// journal is advanced only after each observable boundary is verified;
    /// unexpected ref, index, or worktree state remains a typed conflict. When
    /// `HEAD` does not contain a prepared publication's candidate, the journal
    /// is discarded and its batch stays pending in the runtime.
    pub fn recover_publication(&self) -> Result<Option<IndexGeneration>, RepositoryError> {
        let _coordination_lock = self.acquire_coordination_lock()?;
        self.require_no_pending_checkout_recovery_locked()?;
        let Some(mut journal) = self.read_publication_journal_locked()? else {
            self.sweep_publication_pins_locked(&[])?;
            self.reclaim_orphan_publication_index_lock()?;
            return Ok(None);
        };
        self.adopt_publication_pins_locked(&journal)?;
        self.reclaim_abandoned_publication_index_lock(&journal)?;
        let base_tree = journal
            .base_index_tree()
            .cloned()
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let expected_index = IndexGeneration {
            head: Some(journal.old_head().clone()),
            tree: Some(base_tree),
        };
        // Recovery row "Ref still at H": Orna changes the ordinary index and
        // worktree only after RefAdvanced is durable. While neither HEAD nor
        // any branch contains the candidate, a Prepared publication journal
        // therefore records nothing that was published. Drop it and keep the
        // batch pending in the runtime, whether or not the unreferenced
        // candidate still exists and whatever the user staged since
        // (ORNA-PUB-006, ORNA-PUB-012). A candidate that a branch holds while
        // HEAD does not may already be published there, so the journal stays
        // as a recovery conflict (row "Ref no longer H or N"). Compact and
        // legacy-migration journals carry evidence their publishers resume
        // from, so they keep their own boundaries below.
        if journal.stage() == PublicationJournalStage::Prepared
            && journal.compact_manifest().is_none()
            && journal.migration_continuity().is_none()
            && !self.head_contains_commit(journal.new_head())?
        {
            if self.branch_contains_commit(journal.new_head())? {
                return Err(RepositoryError::StaleHead);
            }
            self.clear_publication_journal_locked()?;
            return Ok(Some(self.index_generation()?));
        }
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let candidate_commit = self.commit_optional(&commit_expression)?;
        if journal.stage() == PublicationJournalStage::Prepared
            && candidate_commit.is_none()
            && self.head()?.as_ref() == Some(journal.old_head())
        {
            let actual = self.index_generation()?;
            if actual == expected_index {
                self.clear_publication_journal_locked()?;
                return Ok(Some(actual));
            }
        }
        let candidate = self.candidate_from_journal_with_commit(
            &journal,
            candidate_commit.ok_or(RepositoryError::InvalidPublicationJournal)?,
        )?;

        if journal.stage() == PublicationJournalStage::Prepared
            && self.head()?.as_ref() == Some(journal.old_head())
        {
            let actual = self.index_generation()?;
            if actual != expected_index {
                return Err(RepositoryError::StaleIndex {
                    expected: expected_index,
                    actual,
                });
            }
            return Err(RepositoryError::PublicationPending);
        }

        if matches!(
            journal.stage(),
            PublicationJournalStage::Prepared
                | PublicationJournalStage::RefAdvanced
                | PublicationJournalStage::IndexReconciled
        ) {
            if self.head()?.as_ref() != Some(journal.new_head()) {
                return Err(RepositoryError::StaleHead);
            }
            let index = self.git_path("index")?;
            let git_lock =
                GitIndexLock::acquire_owned(index.with_extension("lock"), journal.lock_binding()?)?;

            if journal.stage() == PublicationJournalStage::Prepared {
                // The publisher may have stopped between its CAS and the
                // barriers after it, so the candidate and the ref must be
                // durable before RefAdvanced is recorded (ORNA-PUB-005).
                self.sync_candidate_objects(Some(journal.old_head()), journal.new_head())?;
                self.sync_head_ref()?;
                journal.advance(PublicationJournalStage::RefAdvanced)?;
                self.write_publication_journal_locked(&journal)?;
            }
            let mut store_verified = false;
            if journal.stage() == PublicationJournalStage::RefAdvanced {
                self.reconcile_store_worktree_transition(&expected_index, &candidate, false)?;
                store_verified = true;
                let current = IndexGeneration {
                    head: self.head()?,
                    tree: self.index_tree_from_file(&index)?,
                };
                let _reconciled = if current.tree() == expected_index.tree() {
                    let paths = self.publication_index_paths(&candidate, &journal)?;
                    let base_index =
                        fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                    self.reconcile_published_index_locked(
                        &expected_index,
                        &candidate,
                        &paths,
                        &base_index,
                        &git_lock,
                    )?
                } else if self.index_entries_match_candidate(&candidate, &journal)? {
                    let paths = self.publication_index_paths(&candidate, &journal)?;
                    let base_index =
                        fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
                    self.reconcile_published_index_locked(
                        &expected_index,
                        &candidate,
                        &paths,
                        &base_index,
                        &git_lock,
                    )?
                } else {
                    return Err(RepositoryError::StaleIndex {
                        expected: expected_index,
                        actual: current,
                    });
                };
                journal.advance(PublicationJournalStage::IndexReconciled)?;
                self.write_publication_journal_locked(&journal)?;
            }
            if journal.stage() == PublicationJournalStage::IndexReconciled {
                // The store worktree was reconciled before IndexReconciled. An edit to a store path made since
                // then differs from the recorded candidate state and must be preserved as a conflict, not
                // overwritten by materialisation (recovery table row 6; ORNA-PUB-010).
                if !store_verified {
                    self.reconcile_store_worktree_transition(&expected_index, &candidate, false)?;
                }
                self.recover_publication_worktree_locked(&mut journal)?;
            }
            drop(git_lock);
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        if journal.stage() == PublicationJournalStage::WorktreeReconciled {
            if !self.head_contains_commit(journal.new_head())? {
                return Err(RepositoryError::StaleHead);
            }
            self.verify_reconciled_index(&journal)?;
            return Err(RepositoryError::RuntimeCompletionRequired);
        }
        if matches!(
            journal.stage(),
            PublicationJournalStage::RuntimeCompleted | PublicationJournalStage::Complete
        ) && !self.head_contains_commit(journal.new_head())?
        {
            return Err(RepositoryError::StaleHead);
        }
        self.verify_reconciled_index(&journal)?;
        if journal.stage() == PublicationJournalStage::RuntimeCompleted {
            journal.advance(PublicationJournalStage::Complete)?;
            self.write_publication_journal_locked(&journal)?;
        }
        if journal.stage() == PublicationJournalStage::Complete {
            self.clear_publication_journal_locked()?;
            return Ok(Some(self.index_generation()?));
        }
        Err(RepositoryError::InvalidPublicationJournal)
    }

    /// Verifies, under `index.lock`, that the ordinary index still holds the
    /// managed entries the reconciled candidate recorded. Finalization changes
    /// neither ref, index nor worktree, so an index that moved since
    /// reconciliation is a typed conflict that keeps the journal (ORNA-PUB-011,
    /// ORNA-PUB-017; recovery row "Index or affected path differs from both
    /// recorded states").
    pub fn verify_reconciled_index(
        &self,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let candidate = self.candidate_from_journal(journal)?;
        self.verify_index_matches_candidate(&candidate, journal)
    }

    /// Index-only form of `verify_reconciled_index` for finalizing a journal
    /// whose worktree may hold unstaged edits the candidate does not carry
    /// (reviewed candidates, ORNA-PUB-017). It checks the candidate's identity
    /// and that the index still holds the candidate's entries; it does not
    /// require the journal's recorded worktree contents to equal the commit.
    fn verify_index_at_candidate(&self, journal: &PublicationJournal) -> Result<(), RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let commit = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        let candidate = self.candidate_identity_from_journal(journal, commit)?;
        self.verify_index_matches_candidate(&candidate, journal)
    }

    fn verify_index_matches_candidate(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<(), RepositoryError> {
        let index = self.git_path("index")?;
        let _git_lock =
            GitIndexLock::acquire_owned(index.with_extension("lock"), journal.lock_binding()?)?;
        if self.index_entries_match_candidate(candidate, journal)? {
            return Ok(());
        }
        Err(RepositoryError::StaleIndex {
            expected: IndexGeneration {
                head: Some(journal.new_head().clone()),
                tree: Some(candidate.tree().clone()),
            },
            actual: self.index_generation_while_locked()?,
        })
    }

    fn candidate_from_journal(
        &self,
        journal: &PublicationJournal,
    ) -> Result<PrivateCommit, RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", journal.new_head().as_str());
        let commit = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::InvalidPublicationJournal)?;
        self.candidate_from_journal_with_commit(journal, commit)
    }

    fn candidate_from_journal_with_commit(
        &self,
        journal: &PublicationJournal,
        commit: GitCommitRef,
    ) -> Result<PrivateCommit, RepositoryError> {
        let candidate = self.candidate_identity_from_journal(journal, commit)?;
        if !self.candidate_entries_match_journal(&candidate, journal)? {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        if journal.compact_manifest().is_some() {
            self.verify_compact_publication_binding(journal, &candidate)?;
        }
        Ok(candidate)
    }

    /// The private H + P identity of the journal's candidate: the commit is
    /// the journal's new head, its only parent is the recorded old head, and
    /// its tree is that commit's tree. Entry contents are checked separately
    /// by `candidate_from_journal_with_commit`.
    fn candidate_identity_from_journal(
        &self,
        journal: &PublicationJournal,
        commit: GitCommitRef,
    ) -> Result<PrivateCommit, RepositoryError> {
        if &commit != journal.new_head() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        // Recovery must reconstruct the same private H + P candidate that
        // publication prepared. An existing commit is not enough: accepting
        // a journal whose new head is not the sole child of the recorded old
        // head could reconcile the ordinary index and worktree against an
        // unrelated history after a crash.
        let parents = self.git(["show", "-s", "--format=%P", journal.new_head().as_str()])?;
        let parents = parents.split_ascii_whitespace().collect::<Vec<_>>();
        if parents.len() != 1 || parents[0] != journal.old_head().as_str() {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        let tree_expression = format!("{}^{{tree}}", journal.new_head().as_str());
        let tree = self.index_tree_from_native_oid(self.git([
            "rev-parse",
            "--verify",
            &tree_expression,
        ])?)?;
        Ok(PrivateCommit { tree, commit })
    }

    fn recover_publication_worktree_locked(
        &self,
        journal: &mut PublicationJournal,
    ) -> Result<(), RepositoryError> {
        self.set_compact_recovery_skip_worktree_locked(journal, true)?;
        for index in 0..journal.entries().len() {
            self.materialize_publication_entry_locked(journal, index)?;
        }
        self.set_compact_recovery_skip_worktree_locked(journal, false)?;
        journal.advance(PublicationJournalStage::WorktreeReconciled)?;
        self.write_publication_journal_locked(journal)?;
        Ok(())
    }

    #[cfg(test)]
    fn materialize_publication_entry(
        &self,
        journal: &mut PublicationJournal,
        index: usize,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.materialize_publication_entry_locked(journal, index)
    }

    fn materialize_publication_entry_locked(
        &self,
        journal: &mut PublicationJournal,
        index: usize,
    ) -> Result<(), RepositoryError> {
        let entry = journal
            .entries()
            .get(index)
            .ok_or(RepositoryError::InvalidPublicationJournal)?
            .clone();
        let quarantine_name = journal.quarantine_name(index)?;
        let target = self.managed_target(&entry.path)?;
        let parent = target
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        fs::create_dir_all(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        self.validate_managed_parent(parent)?;
        let quarantine = parent.join(&quarantine_name);
        let quarantine_bytes = self.read_managed_file(&quarantine)?;
        let current = self.read_managed_file(&target)?;
        let phase = entry.materialization().phase;

        if phase == PublicationMaterializationPhase::Clean {
            if quarantine_bytes.is_some() {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if current.as_deref() == entry.next() {
                return Ok(());
            }
            if current.as_deref() != entry.expected() {
                return Err(RepositoryError::ManagedContentConflict);
            }
            journal.set_materialization(
                index,
                PublicationMaterializationPhase::QuarantinePrepared,
                Some(quarantine_name.clone()),
            )?;
            self.write_publication_journal_locked(journal)?;
        } else if let Some(bytes) = quarantine_bytes.as_deref() {
            if Some(bytes) != entry.expected() {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if current.as_deref() == entry.next() {
                self.remove_verified_quarantine(
                    parent,
                    &quarantine,
                    &target,
                    entry.expected(),
                    entry.next(),
                )?;
                journal.set_materialization(index, PublicationMaterializationPhase::Clean, None)?;
                self.write_publication_journal_locked(journal)?;
                return Ok(());
            }
            if current.is_some() {
                return Err(RepositoryError::ManagedContentConflict);
            }
            if entry.next().is_none() {
                if self.read_managed_file(&target)?.is_some()
                    || self.read_managed_file(&quarantine)?.as_deref() != entry.expected()
                {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                journal.set_materialization(
                    index,
                    PublicationMaterializationPhase::Installed,
                    Some(quarantine_name.clone()),
                )?;
                self.write_publication_journal_locked(journal)?;
            } else {
                self.materialize_managed_file_impl_with_quarantine_locked(
                    &entry.path,
                    None,
                    entry.next(),
                    None,
                    None,
                    None,
                )?;
                if self.read_managed_file(&target)?.as_deref() != entry.next()
                    || self.read_managed_file(&quarantine)?.as_deref() != entry.expected()
                {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                journal.set_materialization(
                    index,
                    PublicationMaterializationPhase::Installed,
                    Some(quarantine_name.clone()),
                )?;
                self.write_publication_journal_locked(journal)?;
            }
            self.remove_verified_quarantine(
                parent,
                &quarantine,
                &target,
                entry.expected(),
                entry.next(),
            )?;
            journal.set_materialization(index, PublicationMaterializationPhase::Clean, None)?;
            self.write_publication_journal_locked(journal)?;
            return Ok(());
        } else if current.as_deref() == entry.next() {
            journal.set_materialization(index, PublicationMaterializationPhase::Clean, None)?;
            self.write_publication_journal_locked(journal)?;
            return Ok(());
        } else if phase != PublicationMaterializationPhase::QuarantinePrepared
            || current.as_deref() != entry.expected()
        {
            return Err(RepositoryError::ManagedContentConflict);
        }

        let mut after_boundary = |phase| {
            journal.set_materialization(index, phase, Some(quarantine_name.clone()))?;
            self.write_publication_journal_locked(journal)
        };
        self.materialize_managed_file_impl_with_quarantine_locked(
            &entry.path,
            entry.expected(),
            entry.next(),
            Some(&quarantine),
            None,
            Some(&mut after_boundary),
        )?;
        journal.set_materialization(index, PublicationMaterializationPhase::Clean, None)?;
        self.write_publication_journal_locked(journal)
    }

    fn remove_verified_quarantine(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
    ) -> Result<(), RepositoryError> {
        self.remove_verified_quarantine_with_boundary(
            parent,
            quarantine,
            target,
            expected_quarantine,
            expected_target,
            None,
        )
    }

    #[cfg(test)]
    fn remove_verified_quarantine_with_boundary(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
        after_final_verification: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        self.remove_verified_quarantine_inner(
            parent,
            quarantine,
            target,
            expected_quarantine,
            expected_target,
            after_final_verification,
        )
    }

    #[cfg(not(test))]
    fn remove_verified_quarantine_with_boundary(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
        _after_final_verification: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        self.remove_verified_quarantine_inner(
            parent,
            quarantine,
            target,
            expected_quarantine,
            expected_target,
            None,
        )
    }

    fn remove_verified_quarantine_inner(
        &self,
        parent: &Path,
        quarantine: &Path,
        target: &Path,
        expected_quarantine: Option<&[u8]>,
        expected_target: Option<&[u8]>,
        mut after_final_verification: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        if self.read_managed_file(quarantine)?.as_deref() != expected_quarantine
            || self.read_managed_file(target)?.as_deref() != expected_target
        {
            return Err(RepositoryError::ManagedContentConflict);
        }
        self.runtime.ensure_exists()?;
        let cleanup_parent = self.runtime.root().join("publication-cleanup");
        fs::create_dir_all(&cleanup_parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let cleanup = (0..8)
            .map(|_| {
                cleanup_parent.join(format!(
                    ".orna-quarantine-cleanup-{}-{}",
                    std::process::id(),
                    Uuid::new_v4()
                ))
            })
            .find(|path| !path.exists())
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        match Self::rename_path_without_replacement(quarantine, &cleanup) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(RepositoryError::PublicationRecoveryConflict);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(RepositoryError::PublicationRecoveryConflict);
            }
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        }
        let moved = self.read_managed_file(&cleanup)?;
        if moved.as_deref() != expected_quarantine {
            let _ = Self::rename_path_without_replacement(&cleanup, quarantine);
            return Err(RepositoryError::PublicationRecoveryConflict);
        }
        if fs::symlink_metadata(quarantine).is_ok()
            || self.read_managed_file(target)?.as_deref() != expected_target
        {
            return Err(RepositoryError::PublicationRecoveryConflict);
        }

        // The cleanup object is now in the private runtime area.  Its final
        // removal cannot act on a worktree pathname, so a replacement at the
        // old quarantine name is preserved.  The hook is test-only and
        // represents that replacement at the final public-path boundary.
        if let Some(hook) = after_final_verification.as_mut() {
            hook()?;
        }
        let public_replacement = fs::symlink_metadata(quarantine).is_ok();
        fs::remove_file(&cleanup).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RepositoryError::PublicationRecoveryConflict
            } else {
                RepositoryError::LocalStateUnavailable
            }
        })?;
        if public_replacement {
            return Err(RepositoryError::PublicationRecoveryConflict);
        }
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }

    /// Captures the affected managed worktree bytes only when the selected
    /// HEAD and ordinary index still match the caller's generation and every
    /// affected path agrees with the committed base. This is the PUB-1
    /// capture gate: staged or unstaged human edits on managed paths are a
    /// conflict, while unrelated Git state remains outside this check.
    pub fn capture_managed_publication_state(
        &self,
        expected_head: &GitCommitRef,
        expected_index: &IndexGeneration,
        paths: &[ManagedPath],
    ) -> Result<Vec<Option<Vec<u8>>>, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        self.ensure_no_git_operation_in_progress()?;
        self.ensure_no_git_index_lock()?;
        let actual_head = self.head()?;
        if actual_head.as_ref() != Some(expected_head) {
            return Err(RepositoryError::StaleHead);
        }
        let actual_index = self.index_generation_while_locked()?;
        if &actual_index != expected_index {
            return Err(RepositoryError::StaleIndex {
                expected: expected_index.clone(),
                actual: actual_index,
            });
        }
        self.reject_loose_path_collisions(expected_head, paths)?;

        let mut captured = Vec::with_capacity(paths.len());
        for path in paths {
            let target = self.managed_target(path)?;
            if !self.managed_index_matches_head(expected_head, path)? {
                return Err(RepositoryError::ManagedContentConflict);
            }
            let committed = self.committed_managed_file_bytes(expected_head, path)?;
            let actual = self.read_managed_file(&target)?;
            if actual != committed {
                return Err(RepositoryError::ManagedContentConflict);
            }
            captured.push(actual);
        }
        Ok(captured)
    }

    fn tracked_loose_path_components(
        &self,
        expected_head: &GitCommitRef,
    ) -> Result<Vec<Vec<Vec<u8>>>, RepositoryError> {
        self.objects().check_expansion(expected_head.as_str(), "")?;
        let tracked = self.git_bytes([
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "--full-tree",
            expected_head.as_str(),
            "--",
        ])?;
        let mut tracked_paths = Vec::new();
        for path in tracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let components = path
                .split(|byte| *byte == b'/')
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if !is_loose_path_components(&components) {
                continue;
            }
            tracked_paths.push(components);
        }
        Ok(tracked_paths)
    }

    fn reject_loose_path_collisions(
        &self,
        expected_head: &GitCommitRef,
        paths: &[ManagedPath],
    ) -> Result<(), RepositoryError> {
        let tracked_paths = self.tracked_loose_path_components(expected_head)?;
        let tracked_set = tracked_paths.iter().cloned().collect::<BTreeSet<_>>();
        let planned_paths = paths
            .iter()
            .map(|path| {
                path.as_path()
                    .iter()
                    .map(|component| component.as_encoded_bytes().to_vec())
                    .collect::<Vec<_>>()
            })
            .filter(|components| is_loose_path_components(components))
            .collect::<Vec<_>>();
        let planned_set = planned_paths.iter().cloned().collect::<BTreeSet<_>>();
        let mut known_components = BTreeMap::<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>::new();
        for components in &tracked_paths {
            if !planned_set.contains(components) {
                record_case_folded_path_components(&mut known_components, components);
            }
        }

        for components in planned_paths {
            if tracked_set.contains(&components) {
                continue;
            }
            if case_folded_path_collides(&known_components, &components) {
                return Err(RepositoryError::ManagedContentConflict);
            }
            record_case_folded_path_components(&mut known_components, &components);
        }
        Ok(())
    }

    fn reject_final_loose_path_collisions(
        &self,
        expected_head: &GitCommitRef,
        changes: &[ManagedFileChange],
    ) -> Result<(), RepositoryError> {
        let planned = changes
            .iter()
            .filter(|change| change.bytes().is_some())
            .map(|change| {
                change
                    .path()
                    .as_path()
                    .iter()
                    .map(|component| component.as_encoded_bytes().to_vec())
                    .collect::<Vec<_>>()
            })
            .filter(|components| is_loose_path_components(components))
            .collect::<Vec<_>>();
        if planned.is_empty() {
            return Ok(());
        }
        let deleted = changes
            .iter()
            .filter(|change| change.bytes().is_none())
            .map(|change| {
                change
                    .path()
                    .as_path()
                    .iter()
                    .map(|component| component.as_encoded_bytes().to_vec())
                    .collect::<Vec<_>>()
            })
            .filter(|components| is_loose_path_components(components))
            .collect::<BTreeSet<_>>();
        let mut known_components = BTreeMap::<Vec<Vec<u8>>, BTreeSet<Vec<u8>>>::new();
        for components in self.tracked_loose_path_components(expected_head)? {
            if !deleted.contains(&components) {
                record_case_folded_path_components(&mut known_components, &components);
            }
        }
        for components in planned {
            if case_folded_path_collides(&known_components, &components) {
                return Err(RepositoryError::ManagedContentConflict);
            }
            record_case_folded_path_components(&mut known_components, &components);
        }
        Ok(())
    }

    /// Observes the ordinary Git index without modifying it.
    pub fn index_generation(&self) -> Result<IndexGeneration, RepositoryError> {
        self.ensure_no_git_index_lock()?;
        self.index_generation_while_locked()
    }

    fn index_generation_while_locked(&self) -> Result<IndexGeneration, RepositoryError> {
        // Observe a copy: `write-tree` on the live index contends with the
        // `index.lock` the caller holds and fails with exit 128. With no index
        // file there is nothing to contend with, so git derives the empty tree.
        let index = self.git_path("index")?;
        let tree = if index.exists() {
            self.index_tree_from_file(&index)?
        } else {
            let mut command = self.command();
            command.args(["write-tree"]);
            let output = command
                .output()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if !output.status.success() {
                return Err(RepositoryError::GitOperationFailed);
            }
            Some(self.index_tree_from_native_oid(trim_output(&output.stdout))?)
        };
        Ok(IndexGeneration {
            head: self.head()?,
            tree,
        })
    }

    fn index_tree_from_file(&self, index: &Path) -> Result<Option<IndexTreeRef>, RepositoryError> {
        let bytes = match fs::read(index) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        let parent = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        let temporary = parent.join(format!(
            ".orna-index-observe-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let mut command = self.command();
            command
                .env("GIT_INDEX_FILE", &temporary)
                .args(["write-tree"]);
            let tree = self.index_tree_from_native_oid(trim_output(&self.run(command)?.stdout))?;
            Ok(Some(tree))
        })();
        let _ = fs::remove_file(&temporary);
        result
    }

    fn capture_index_for_expected(
        &self,
        expected: &IndexGeneration,
        index: &Path,
    ) -> Result<Vec<u8>, RepositoryError> {
        let base_index = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let actual = self.index_generation()?;
        let after_capture = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if base_index != after_capture || actual.tree() != expected.tree() {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        Ok(base_index)
    }

    /// Refuses publication when the ordinary index no longer holds the bytes
    /// captured as the reconciliation base.
    fn require_index_unchanged(
        &self,
        expected: &IndexGeneration,
        index: &Path,
        base_index: &[u8],
    ) -> Result<(), RepositoryError> {
        let current = fs::read(index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if current == base_index {
            return Ok(());
        }
        Err(RepositoryError::StaleIndex {
            expected: expected.clone(),
            actual: IndexGeneration {
                head: self.head()?,
                tree: self.index_tree_from_file(index)?,
            },
        })
    }

    /// Captures Git's ordinary worktree/index status in machine-readable form.
    pub fn worktree_state(&self) -> Result<WorktreeState, RepositoryError> {
        Ok(self.worktree_scan()?.state)
    }

    /// Captures status and hashes managed content once for this observation.
    fn worktree_scan(&self) -> Result<WorktreeScan, RepositoryError> {
        let status = self.worktree_status()?;
        let (digest, stamps) = self.worktree_content_scan(&[], false)?;
        Ok(WorktreeScan {
            state: WorktreeState(status, digest),
            stamps,
        })
    }

    /// Reports whether `scan` still describes the worktree. Compares status and
    /// managed-path stamps only, so managed bytes are never read a second time.
    fn worktree_scan_stable(&self, scan: &WorktreeScan) -> Result<bool, RepositoryError> {
        if self.worktree_status()? != scan.state.0 {
            return Ok(false);
        }
        for managed in &scan.stamps {
            if self.managed_stamp(&managed.path)? != managed.stamp {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn worktree_status(&self) -> Result<Vec<u8>, RepositoryError> {
        self.git_bytes(["status", "--porcelain=v2", "-z", "--untracked-files=all"])
    }

    fn managed_stamp(
        &self,
        path: &ManagedPath,
    ) -> Result<Option<ManagedFileStamp>, RepositoryError> {
        match fs::symlink_metadata(self.worktree.join(path.as_path())) {
            Ok(metadata) => Ok(Some(Self::managed_file_stamp(&metadata))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn managed_file_stamp(metadata: &fs::Metadata) -> ManagedFileStamp {
        #[cfg(unix)]
        let change = (
            std::os::unix::fs::MetadataExt::dev(metadata),
            std::os::unix::fs::MetadataExt::ino(metadata),
            std::os::unix::fs::MetadataExt::ctime(metadata),
            std::os::unix::fs::MetadataExt::ctime_nsec(metadata),
            std::os::unix::fs::MetadataExt::mode(metadata),
        );
        ManagedFileStamp {
            file_type: metadata.file_type(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            change,
        }
    }
    #[cfg(unix)]
    fn worktree_executable_marker(metadata: &fs::Metadata) -> u8 {
        use std::os::unix::fs::PermissionsExt;

        u8::from(metadata.permissions().mode() & 0o111 != 0)
    }

    #[cfg(not(unix))]
    const fn worktree_executable_marker(_metadata: &fs::Metadata) -> u8 {
        0
    }

    /// Digest for checkout force plans. Includes git-ignored bytes, so a force
    /// plan fences ignored files that the checkout could overwrite.
    fn worktree_content_digest(
        &self,
        extra_paths: &[ManagedPath],
    ) -> Result<[u8; 32], RepositoryError> {
        Ok(self.worktree_content_scan(extra_paths, true)?.0)
    }

    /// Hashes managed worktree content in bounded chunks and returns each path's
    /// stamp, taken before its bytes are read. Git-ignored files are listed and
    /// read only when `include_ignored` is set; CWD observation omits them.
    fn worktree_content_scan(
        &self,
        extra_paths: &[ManagedPath],
        include_ignored: bool,
    ) -> Result<([u8; 32], Vec<ManagedStamp>), RepositoryError> {
        let mut paths = BTreeSet::new();
        for path in self
            .git_bytes(["diff", "--no-renames", "--name-only", "-z", "--"])?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path.to_vec());
        }
        for path in self
            .git_bytes([
                "diff",
                "--cached",
                "--no-renames",
                "--name-only",
                "-z",
                "--",
            ])?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path.to_vec());
        }
        for path in self
            .git_bytes(["ls-files", "--others", "--exclude-standard", "-z", "--"])?
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path.to_vec());
        }
        if include_ignored {
            for path in self
                .git_bytes([
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                    "--",
                ])?
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
            {
                paths.insert(path.to_vec());
            }
        }
        for path in extra_paths {
            paths.insert(path.as_path().as_os_str().as_encoded_bytes().to_vec());
        }
        // Regular-file sections come from the previous scan when their stamp is
        // unchanged and settled (see `stamp_settled_before`). The cache is
        // replaced by this scan's sections, so it never outlives one observation
        // of each path.
        let previous = self
            .section_cache
            .lock()
            .map(|mut cache| std::mem::take(&mut *cache))
            .unwrap_or_default();
        let mut next = HashMap::new();
        let mut digest = Sha256::new();
        digest.update(b"ORNA-WORKTREE-CONTENT-V2\0");
        let mut stamps = Vec::with_capacity(paths.len());
        for encoded_path in paths {
            let path =
                String::from_utf8(encoded_path).map_err(|_| RepositoryError::UnsafeManagedPath)?;
            // Worktree paths are local user data: untracked `Sub/.GIT/` is legal,
            // so only the structural check applies here.
            let path = ManagedPath::from_worktree_listing(&path)?;
            let encoded_path = path.as_path().as_os_str().as_encoded_bytes();
            digest.update((encoded_path.len() as u64).to_be_bytes());
            digest.update(encoded_path);
            let key = encoded_path.to_vec();
            let target = self.worktree.join(path.as_path());
            let metadata = match fs::symlink_metadata(&target) {
                Ok(metadata) => Some(metadata),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            };
            stamps.push(ManagedStamp {
                path,
                stamp: metadata.as_ref().map(Self::managed_file_stamp),
            });
            match metadata {
                Some(metadata) if metadata.file_type().is_symlink() => {
                    digest.update([1]);
                    let link = fs::read_link(&target)
                        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                    let link = link.as_os_str().as_encoded_bytes();
                    digest.update((link.len() as u64).to_be_bytes());
                    digest.update(link);
                }
                Some(metadata) if metadata.is_file() => {
                    digest.update([2]);
                    digest.update([Self::worktree_executable_marker(&metadata)]);
                    let file_stamp = Self::managed_file_stamp(&metadata);
                    let (section, entry) = match previous.get(&key) {
                        Some(cached) if cached.stamp == file_stamp => {
                            (cached.digest, Some(cached.clone()))
                        }
                        _ => {
                            let read_started = std::time::SystemTime::now();
                            let section = Self::file_section_digest(&target)?;
                            let settled = Self::stamp_settled_before(&file_stamp, read_started);
                            let entry = settled.then(|| CachedFileDigest {
                                stamp: file_stamp,
                                digest: section,
                            });
                            (section, entry)
                        }
                    };
                    if let Some(entry) = entry {
                        next.insert(key, entry);
                    }
                    digest.update(section);
                }
                Some(_) => digest.update([3]),
                None => digest.update([0]),
            }
        }
        if let Ok(mut cache) = self.section_cache.lock() {
            *cache = next;
        }
        Ok((digest.finalize().into(), stamps))
    }

    /// Hashes one regular file's length and bytes as a standalone section, so
    /// the section can be cached against the file's stamp.
    fn file_section_digest(target: &Path) -> Result<[u8; 32], RepositoryError> {
        #[cfg(test)]
        CONTENT_READS.with(|reads| reads.borrow_mut().push(target.to_path_buf()));
        let mut section = Sha256::new();
        Self::digest_file_contents(target, &mut section)?;
        Ok(section.finalize().into())
    }

    /// Reports whether a file's stamp is settled before `read_started`: its mtime
    /// and ctime both precede it by the racy window. Only then can a later write
    /// leave the stamp unchanged, so only settled sections may be reused. A file
    /// written within the window is always read again.
    fn stamp_settled_before(stamp: &ManagedFileStamp, read_started: std::time::SystemTime) -> bool {
        let Some(limit) = read_started.checked_sub(CONTENT_RACY_WINDOW) else {
            return false;
        };
        let modified_settled = stamp.modified.is_some_and(|modified| modified <= limit);
        #[cfg(unix)]
        let changed_settled = {
            let (_, _, ctime, ctime_nsec, _) = stamp.change;
            u64::try_from(ctime)
                .ok()
                .zip(u32::try_from(ctime_nsec).ok())
                .and_then(|(secs, nanos)| {
                    std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(secs, nanos))
                })
                .is_some_and(|changed| changed <= limit)
        };
        #[cfg(not(unix))]
        let changed_settled = true;
        modified_settled && changed_settled
    }

    /// Streams a regular file's length and bytes into `digest` through a fixed
    /// chunk. A file that shrinks while it is read fails the observation.
    fn digest_file_contents(target: &Path, digest: &mut Sha256) -> Result<(), RepositoryError> {
        let file = fs::File::open(target).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let len = file
            .metadata()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?
            .len();
        digest.update(len.to_be_bytes());
        let mut reader = file.take(len);
        let mut chunk = [0_u8; 64 * 1024];
        let mut copied = 0_u64;
        loop {
            let read = reader
                .read(&mut chunk)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            if read == 0 {
                break;
            }
            digest.update(&chunk[..read]);
            copied += read as u64;
        }
        if copied != len {
            return Err(RepositoryError::StaleCwd);
        }
        Ok(())
    }

    /// Observes CWD. The runtime generation must come from the runtime owner,
    /// keeping `state.db` access outside the repository abstraction.
    pub fn cwd_generation(
        &self,
        runtime: RuntimeGeneration,
    ) -> Result<CwdGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.cwd_generation_locked(runtime)
    }

    fn cwd_generation_locked(
        &self,
        runtime: RuntimeGeneration,
    ) -> Result<CwdGeneration, RepositoryError> {
        // Do not return a torn CWD if another Git process changes the index,
        // branch, status, or a managed file while it is observed.  The lock
        // serializes Orna writers; the bounded recheck detects external Git
        // writers by stamp, so managed bytes are hashed once per attempt.
        for _ in 0..3 {
            let before = self.index_generation()?;
            let branch = self.current_branch()?;
            let scan = self.worktree_scan()?;
            let after = self.index_generation()?;
            let branch_after = self.current_branch()?;
            if before == after && branch == branch_after && self.worktree_scan_stable(&scan)? {
                return Ok(CwdGeneration {
                    head: before.head.clone(),
                    branch,
                    index: before,
                    worktree: scan.state,
                    runtime,
                });
            }
        }
        Err(RepositoryError::StaleCwd)
    }

    /// Returns the local branch currently attached to `HEAD`, if any. A
    /// detached `HEAD` is distinct CWD state even when it names the same
    /// commit as a local branch.
    fn current_branch(&self) -> Result<Option<String>, RepositoryError> {
        let mut command = self.command();
        command.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            let branch = trim_output(&output.stdout);
            if valid_branch_name(&branch) {
                Ok(Some(branch))
            } else {
                Err(RepositoryError::GitOperationFailed)
            }
        } else if output.status.code() == Some(1) && output.stderr.is_empty() {
            Ok(None)
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }

    /// Rechecks an earlier CWD observation before an operation that depends on
    /// both staged and unstaged state.  A worktree-only edit is stale too.
    pub fn verify_cwd(&self, expected: &CwdGeneration) -> Result<(), RepositoryError> {
        let actual = self.cwd_generation(expected.runtime)?;
        if &actual == expected {
            Ok(())
        } else {
            Err(RepositoryError::StaleCwd)
        }
    }

    /// Resolves a checkout target and captures all Git CWD preconditions while
    /// holding the repository coordination lock. This operation never writes
    /// refs, the index, the worktree, or the private runtime area.
    pub fn plan_checkout(
        &self,
        selector: &str,
        runtime: RuntimeGeneration,
    ) -> Result<CheckoutPreflight, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.plan_checkout_locked(selector, runtime)
    }

    fn plan_checkout_locked(
        &self,
        selector: &str,
        runtime: RuntimeGeneration,
    ) -> Result<CheckoutPreflight, RepositoryError> {
        if selector.is_empty() || selector.starts_with('-') || selector.contains('\0') {
            return Err(RepositoryError::InvalidSelector);
        }
        let target = self.resolve_checkout_target(selector)?;
        let cwd = self.cwd_generation_locked(runtime)?;
        let git = self.checkout_git_subplan(cwd.head.as_ref(), target.commit())?;
        let dirty_content_digest = self.worktree_content_digest(&git.discardable_paths)?;
        Ok(CheckoutPreflight {
            expected_head: cwd.head.clone(),
            target,
            cwd,
            git,
            dirty_content_digest,
        })
    }

    /// Revalidates a previously captured checkout preflight while holding the
    /// same coordination lock future mutation will require. No ref, index,
    /// worktree, or runtime state is written.
    pub fn verify_checkout_preflight(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.verify_checkout_preflight_locked(plan)
    }

    fn verify_checkout_preflight_locked(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        let current_cwd = self.cwd_generation_locked(plan.cwd.runtime)?;
        if current_cwd != plan.cwd {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        if self.checkout_git_subplan(current_cwd.head.as_ref(), plan.target.commit())? != plan.git {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        if self.worktree_content_digest(&plan.git.discardable_paths)? != plan.dirty_content_digest {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        let selector = plan
            .target
            .branch_name()
            .map_or_else(|| plan.target.commit().as_str().to_owned(), str::to_owned);
        let current_target = self.resolve_checkout_target(&selector)?;
        if current_target != plan.target {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        Ok(())
    }

    /// Switches only between attachments of the already-selected commit.
    ///
    /// This narrow checkout form cannot replace target files, so Git retains
    /// ordinary staged, unstaged, and untracked paths. It deliberately accepts
    /// neither a force flag nor a token: divergent checkout remains outside
    /// this API until its discard set and recovery journal are implemented.
    pub fn execute_same_commit_checkout(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        self.require_no_pending_checkout_recovery_locked()?;
        self.verify_checkout_preflight_locked(plan)?;
        if plan.expected_head.as_ref() != Some(plan.target.commit()) {
            return Err(RepositoryError::CheckoutExecutionUnsafe);
        }

        let mut command = self.command();
        match plan.target() {
            CheckoutTarget::Branch { name, .. } => {
                command.args(["switch", "--"]).arg(name);
            }
            CheckoutTarget::Detached { commit } => {
                command
                    .args(["switch", "--detach", "--"])
                    .arg(commit.as_str());
            }
        }
        self.run(command)?;
        if self.head()?.as_ref() != Some(plan.target.commit()) {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(())
    }

    /// Executes the Git carry-forward phase for a divergent checkout whose
    /// locked preflight found no conflicting local paths.
    ///
    /// This is intentionally narrower than a complete Orna checkout: callers
    /// must validate the target's logical state and coordinate runtime
    /// activation before invoking this repository phase.  It never accepts a
    /// force witness or discard set; a conflict fails before Git is asked to
    /// change the CWD.
    pub fn execute_nonconflicting_git_checkout(
        &self,
        plan: &CheckoutPreflight,
    ) -> Result<(), RepositoryError> {
        self.execute_nonconflicting_git_checkout_with_validation(plan, |_repository, _plan| {
            Ok::<(), std::convert::Infallible>(())
        })
        .map_err(|error| match error {
            CheckoutExecutionError::Repository(error) => error,
            CheckoutExecutionError::Validation(never) => match never {},
        })
    }

    /// Executes the safe Git carry-forward phase only after `validate` has
    /// accepted the isolated logical candidate under the same mutation lock.
    ///
    /// The validator may inspect the target commit and its logical state using
    /// the repository and immutable preflight. A rejected candidate leaves
    /// `HEAD`, the index, and the worktree untouched. This does not authorize
    /// conflicting or forced checkout; journaled destructive recovery remains
    /// a separate higher-level boundary.
    pub fn execute_nonconflicting_git_checkout_with_validation<E, F>(
        &self,
        plan: &CheckoutPreflight,
        validate: F,
    ) -> Result<(), CheckoutExecutionError<E>>
    where
        F: FnOnce(&Repository, &CheckoutPreflight) -> Result<(), E>,
    {
        let _lock = self
            .acquire_coordination_lock()
            .map_err(CheckoutExecutionError::Repository)?;
        self.require_no_pending_publication_recovery_locked()
            .map_err(CheckoutExecutionError::Repository)?;
        self.require_no_pending_checkout_recovery_locked()
            .map_err(CheckoutExecutionError::Repository)?;
        self.verify_checkout_preflight_locked(plan)
            .map_err(CheckoutExecutionError::Repository)?;
        if plan.expected_head.as_ref() == Some(plan.target.commit())
            || !plan.git.conflicting_paths().is_empty()
        {
            return Err(CheckoutExecutionError::Repository(
                RepositoryError::CheckoutExecutionUnsafe,
            ));
        }

        self.validate_checkout_target_context(plan.target.commit())
            .map_err(CheckoutExecutionError::Repository)?;

        validate(self, plan).map_err(CheckoutExecutionError::Validation)?;
        self.verify_checkout_preflight_locked(plan)
            .map_err(CheckoutExecutionError::Repository)?;

        let mut command = self.command();
        match plan.target() {
            CheckoutTarget::Branch { name, .. } => {
                command.args(["switch", "--"]).arg(name);
            }
            CheckoutTarget::Detached { commit } => {
                command
                    .args(["switch", "--detach", "--"])
                    .arg(commit.as_str());
            }
        }
        self.run(command)
            .map(|_| ())
            .map_err(CheckoutExecutionError::Repository)?;
        if self
            .head()
            .map_err(CheckoutExecutionError::Repository)?
            .as_ref()
            != Some(plan.target.commit())
        {
            return Err(CheckoutExecutionError::Repository(
                RepositoryError::GitOperationFailed,
            ));
        }
        Ok(())
    }

    /// Admits the immutable target snapshot before checkout can make it
    /// visible. Format-3 targets must carry the committed source and native
    /// store roots; legacy formats remain read-only checkout targets.
    fn validate_checkout_target_context(
        &self,
        target: &GitCommitRef,
    ) -> Result<(), RepositoryError> {
        let snapshot = self
            .pin_snapshot(target.as_str())
            .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
        let context = self
            .open_format_context_at(&snapshot)
            .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
        if context.supports_writes() {
            context
                .validate_schema_root()
                .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
            context
                .validate_store_root()
                .map_err(|_| RepositoryError::CheckoutTargetInvalid)?;
        } else if !context.is_read_only() {
            return Err(RepositoryError::CheckoutTargetInvalid);
        }
        Ok(())
    }

    fn checkout_git_subplan(
        &self,
        current_head: Option<&GitCommitRef>,
        target: &GitCommitRef,
    ) -> Result<CheckoutGitSubplan, RepositoryError> {
        self.objects().check_expansion(target.as_str(), "")?;
        if let Some(current) = current_head {
            self.objects().check_expansion(current.as_str(), "")?;
        }
        let target_paths = match current_head {
            Some(current) => self.git_nul_paths(&[
                "diff".into(),
                "--name-only".into(),
                "-z".into(),
                current.as_str().into(),
                target.as_str().into(),
                "--".into(),
            ])?,
            None => self.git_nul_paths(&[
                "ls-tree".into(),
                "-r".into(),
                "--name-only".into(),
                "-z".into(),
                target.as_str().into(),
                "--".into(),
            ])?,
        };
        let mut local_paths = HashSet::new();
        for arguments in [
            vec![
                "diff".into(),
                "--name-only".into(),
                "-z".into(),
                "--".into(),
            ],
            vec![
                "diff".into(),
                "--cached".into(),
                "--name-only".into(),
                "-z".into(),
                "--".into(),
            ],
            vec![
                "ls-files".into(),
                "--others".into(),
                "--exclude-standard".into(),
                "-z".into(),
                "--".into(),
            ],
            vec![
                "ls-files".into(),
                "--others".into(),
                "--ignored".into(),
                "--exclude-standard".into(),
                "-z".into(),
                "--".into(),
            ],
        ] {
            local_paths.extend(self.git_worktree_nul_paths(&arguments)?);
        }
        let affected_paths = target_paths
            .union(&local_paths)
            .cloned()
            .collect::<HashSet<_>>();
        let conflicting_paths = target_paths
            .intersection(&local_paths)
            .cloned()
            .collect::<HashSet<_>>();
        let affected_paths = sorted_paths(affected_paths);
        let conflicting_paths = sorted_paths(conflicting_paths);
        let discardable_paths = conflicting_paths.clone();
        Ok(CheckoutGitSubplan {
            affected_paths,
            conflicting_paths,
            discardable_paths,
        })
    }

    /// Revalidates and authorizes the exact preflight for a force-capable
    /// checkout operation. The witness alone is insufficient after any Git
    /// or runtime generation drift.
    pub fn authorize_checkout_force(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
    ) -> Result<(), RepositoryError> {
        self.verify_checkout_preflight(plan)?;
        plan.authorize_force(force, token)
    }

    /// Admits the exact precomputed discard set for a later divergent checkout.
    ///
    /// This is deliberately only an admission boundary: it holds the same
    /// coordination lock as checkout execution, revalidates the complete
    /// preflight, and requires the canonical force witness, but does not alter
    /// the ref, index, worktree, or runtime state. A future journaled executor
    /// must consume this validated set rather than infer a replacement set.
    pub fn validate_checkout_discard_set(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
        discard_paths: &[ManagedPath],
    ) -> Result<ValidatedCheckoutDiscard, RepositoryError> {
        self.validate_checkout_discard_set_with_validation(
            plan,
            force,
            token,
            discard_paths,
            |_repository, _plan| Ok::<(), std::convert::Infallible>(()),
        )
        .map_err(|error| match error {
            CheckoutExecutionError::Repository(error) => error,
            CheckoutExecutionError::Validation(never) => match never {},
        })
    }

    /// Admits a force discard set only after `validate` accepts the isolated
    /// logical candidate while the checkout mutation lock is held.
    ///
    /// This remains an admission boundary: it never changes the ref, index,
    /// worktree, or runtime state. A rejected logical candidate cannot be
    /// converted into a force-authorized plan, and a future journaled executor
    /// must still consume this exact validated discard set.
    pub fn validate_checkout_discard_set_with_validation<E, F>(
        &self,
        plan: &CheckoutPreflight,
        force: bool,
        token: Option<&CheckoutPlanToken>,
        discard_paths: &[ManagedPath],
        validate: F,
    ) -> Result<ValidatedCheckoutDiscard, CheckoutExecutionError<E>>
    where
        F: FnOnce(&Repository, &CheckoutPreflight) -> Result<(), E>,
    {
        let _lock = self
            .acquire_coordination_lock()
            .map_err(CheckoutExecutionError::Repository)?;
        self.verify_checkout_preflight_locked(plan)
            .map_err(CheckoutExecutionError::Repository)?;
        validate(self, plan).map_err(CheckoutExecutionError::Validation)?;
        plan.authorize_force(force, token)
            .map_err(CheckoutExecutionError::Repository)?;
        if discard_paths == plan.git.discardable_paths() {
            Ok(ValidatedCheckoutDiscard {
                preflight: plan.clone(),
                force_token: *token.expect("authorized force checkout has a token"),
                discard_paths: discard_paths.to_vec(),
            })
        } else {
            Err(CheckoutExecutionError::Repository(
                RepositoryError::CheckoutDiscardSetMismatch,
            ))
        }
    }

    /// Rechecks an admitted force-discard capability at the destructive
    /// execution boundary.
    ///
    /// Force admission and checkout execution cannot share a lock across
    /// arbitrary runtime coordination. A capability is therefore not a
    /// reusable right to discard: immediately before a journaled executor
    /// changes Git state, it must prove that the captured target, CWD, exact
    /// discard set, and canonical force witness still describe this CWD. Any
    /// intervening ref, index, worktree, or runtime-generation change fails
    /// closed before mutation.
    pub fn verify_validated_checkout_discard(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.verify_validated_checkout_discard_locked(discard)
    }

    fn verify_validated_checkout_discard_locked(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        self.verify_checkout_preflight_locked(&discard.preflight)?;
        if discard.discard_paths != discard.preflight.git.discardable_paths() {
            return Err(RepositoryError::CheckoutDiscardSetMismatch);
        }
        if discard.force_token != discard.preflight.force_token() {
            return Err(RepositoryError::CheckoutPlanStale);
        }
        discard
            .preflight
            .authorize_force(true, Some(&discard.force_token))
    }

    /// A durable force-discard intent means recovery owns the next checkout
    /// transition. No other Git-visible checkout may interleave with it: doing
    /// so would invalidate the before-state that recovery has to compare and
    /// could turn a recoverable intent into a partial switch.
    fn require_no_pending_checkout_recovery_locked(&self) -> Result<(), RepositoryError> {
        if self.read_checkout_recovery_journal_locked()?.is_some() {
            Err(RepositoryError::CheckoutRecoveryRequired)
        } else {
            Ok(())
        }
    }

    /// A publication journal retains the exact ref/index/worktree generation
    /// that its own recovery protocol must reconcile. A force-checkout cannot
    /// journal or mutate a competing before-state until publication recovery
    /// has resolved that durable boundary.
    fn require_no_pending_publication_recovery_locked(&self) -> Result<(), RepositoryError> {
        if self.read_publication_journal_locked()?.is_some() {
            Err(RepositoryError::PublicationPending)
        } else {
            Ok(())
        }
    }

    /// Durably records a verified force-discard capability before a future
    /// destructive executor can alter Git state. The record is intentionally
    /// not an execution command: restart recovery may only clear it after
    /// proving that the exact original preflight still holds.
    pub fn persist_validated_checkout_discard(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        self.verify_validated_checkout_discard_locked(discard)?;
        let journal = CheckoutRecoveryJournal::from_validated(discard);
        match self.read_checkout_recovery_journal_locked()? {
            Some(existing) if !existing.matches_validated(discard) => {
                Err(RepositoryError::CheckoutRecoveryRequired)
            }
            Some(_) => Ok(()),
            None => self.write_checkout_recovery_journal_locked(&journal),
        }
    }

    /// Executes an admitted force-discard checkout through durable,
    /// repository-local recovery boundaries.
    ///
    /// Only the exact consented paths are restored from the selected target;
    /// the subsequent ordinary `git switch` carries unrelated staged,
    /// unstaged, and untracked state using Git's normal compatibility rules.
    /// Before the scoped discard, after it, and after selection are each
    /// journalled as complete CWD generations. Recovery consequently never
    /// uses a blind reset after an interruption.
    pub fn execute_validated_force_checkout(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        self.execute_validated_force_checkout_impl(discard, None, None)
    }

    /// Test-only interruption seam for the durable discard boundary.
    ///
    /// Production callers must use [`Self::execute_validated_force_checkout`].
    /// The hook runs after the scoped discard generation is durably journalled,
    /// but before Git selects the recorded target. Recovery must resume that
    /// recorded transition without resetting unrelated local state.
    #[doc(hidden)]
    pub fn execute_validated_force_checkout_after_discard_with_test_hook(
        &self,
        discard: &ValidatedCheckoutDiscard,
        hook: &mut dyn FnMut() -> Result<(), RepositoryError>,
    ) -> Result<(), RepositoryError> {
        self.execute_validated_force_checkout_impl(discard, Some(hook), None)
    }

    /// Test-only interruption seam for the post-selection journal boundary.
    ///
    /// Production callers must use [`Self::execute_validated_force_checkout`].
    /// The hook runs after Git has selected the target and its CWD has been
    /// observed, but before that selected generation is durably journalled.
    /// This is the only crash window in which recovery must recognize the
    /// selected attachment without an already-recorded applied generation.
    pub fn execute_validated_force_checkout_with_test_hook(
        &self,
        discard: &ValidatedCheckoutDiscard,
        hook: &mut dyn FnMut() -> Result<(), RepositoryError>,
    ) -> Result<(), RepositoryError> {
        self.execute_validated_force_checkout_impl(discard, None, Some(hook))
    }

    fn execute_validated_force_checkout_impl(
        &self,
        discard: &ValidatedCheckoutDiscard,
        mut after_discard: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
        mut after_applied: Option<&mut dyn FnMut() -> Result<(), RepositoryError>>,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        let mut journal = match self.read_checkout_recovery_journal_locked()? {
            Some(journal)
                if journal.matches_validated(discard)
                    && journal.phase == CheckoutRecoveryPhase::Prepared =>
            {
                journal
            }
            Some(_) | None => return Err(RepositoryError::CheckoutRecoveryRequired),
        };
        self.verify_validated_checkout_discard_locked(discard)?;
        if journal.before.as_ref() != Some(&discard.preflight.cwd) {
            return Err(RepositoryError::CheckoutRecoveryRequired);
        }

        self.restore_checkout_discard_paths(discard)?;
        journal.record_discarded(self.cwd_generation_locked(discard.preflight.cwd.runtime)?)?;
        self.write_checkout_recovery_journal_locked(&journal)?;
        if let Some(hook) = after_discard.as_mut() {
            hook()?;
        }

        self.switch_checkout_target(discard.preflight.target())?;
        let after = self.cwd_generation_locked(discard.preflight.cwd.runtime)?;
        if after.head.as_ref() != Some(discard.preflight.target.commit())
            || after.branch.as_deref() != discard.preflight.target.branch_name()
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        if let Some(hook) = after_applied.as_mut() {
            hook()?;
        }
        journal.record_applied(after)?;
        self.write_checkout_recovery_journal_locked(&journal)?;
        self.clear_checkout_recovery_journal_locked()
    }

    fn restore_checkout_discard_paths(
        &self,
        discard: &ValidatedCheckoutDiscard,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        command
            .args(["restore", "--source"])
            .arg(discard.preflight.target.commit().as_str())
            .args(["--staged", "--worktree", "--"]);
        for path in &discard.discard_paths {
            command.arg(path.as_path());
        }
        self.run(command).map(|_| ())
    }

    fn switch_checkout_target(&self, target: &CheckoutTarget) -> Result<(), RepositoryError> {
        let mut command = self.command();
        match target {
            CheckoutTarget::Branch { name, .. } => {
                command.args(["switch", "--"]).arg(name);
            }
            CheckoutTarget::Detached { commit } => {
                command
                    .args(["switch", "--detach", "--"])
                    .arg(commit.as_str());
            }
        }
        self.run(command).map(|_| ())
    }

    fn verify_prepared_checkout_journal_locked(
        &self,
        journal: &CheckoutRecoveryJournal,
        verify_force_token: bool,
    ) -> Result<(), RepositoryError> {
        let selector = journal.target.branch_name().map_or_else(
            || journal.target.commit().as_str().to_owned(),
            str::to_owned,
        );
        let plan = self.plan_checkout_locked(&selector, journal.runtime)?;
        if plan.target != journal.target
            || plan.git.discardable_paths != journal.discard_paths
            || (verify_force_token && plan.force_token() != journal.force_token)
        {
            return Err(RepositoryError::CheckoutRecoveryRequired);
        }
        Ok(())
    }

    /// Resolves a force-discard journal after restart.
    ///
    /// A prepared transition preserves the recorded old CWD. A journalled
    /// post-selection CWD completes by releasing the journal. A partial or
    /// externally changed transition is retained and reported instead of
    /// resetting files that no longer match its recorded generations.
    pub fn recover_pre_execution_checkout(&self) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.require_no_pending_publication_recovery_locked()?;
        let Some(mut journal) = self.read_checkout_recovery_journal_locked()? else {
            return Ok(());
        };
        if let Some(before) = journal.before.as_ref() {
            let current = self.cwd_generation_locked(journal.runtime)?;
            return match journal.phase {
                CheckoutRecoveryPhase::Prepared if &current == before => {
                    self.verify_prepared_checkout_journal_locked(&journal, true)?;
                    self.clear_checkout_recovery_journal_locked()
                }
                CheckoutRecoveryPhase::Applied if journal.after.as_ref() == Some(&current) => {
                    self.clear_checkout_recovery_journal_locked()
                }
                CheckoutRecoveryPhase::Discarded
                    if journal.discarded.as_ref() == Some(&current) =>
                {
                    // The target may be a movable branch. Re-resolve it while
                    // the recovery lock is held before switching: otherwise a
                    // branch update between discard and restart could make
                    // recovery mutate HEAD/worktree to a different commit
                    // than the one the force witness authorized.
                    self.verify_prepared_checkout_journal_locked(&journal, false)?;
                    self.switch_checkout_target(&journal.target)?;
                    let after = self.cwd_generation_locked(journal.runtime)?;
                    if after.head.as_ref() != Some(journal.target.commit())
                        || after.branch.as_deref() != journal.target.branch_name()
                    {
                        return Err(RepositoryError::GitOperationFailed);
                    }
                    journal.record_applied(after)?;
                    self.write_checkout_recovery_journal_locked(&journal)?;
                    self.clear_checkout_recovery_journal_locked()
                }
                // A process may stop after Git has selected the target but
                // before that observable CWD generation is journalled. The
                // target attachment proves this is the recorded transition,
                // while retaining the observed index and worktree state
                // avoids any broad reset over later local changes.
                CheckoutRecoveryPhase::Discarded
                    if current.head.as_ref() == Some(journal.target.commit())
                        && current.branch.as_deref() == journal.target.branch_name() =>
                {
                    journal.record_applied(current)?;
                    self.write_checkout_recovery_journal_locked(&journal)?;
                    self.clear_checkout_recovery_journal_locked()
                }
                _ => Err(RepositoryError::CheckoutRecoveryRequired),
            };
        }
        // Version-one journals did not contain enough state to recognize a
        // post-mutation CWD. Retain their historical no-mutation recovery
        // behavior rather than assuming a destructive transition completed.
        self.verify_prepared_checkout_journal_locked(&journal, false)?;
        self.clear_checkout_recovery_journal_locked()
    }

    /// Reports whether restart recovery has a retained pre-execution checkout
    /// intent. This exposes no capability or discard details.
    pub fn has_pending_pre_execution_checkout(&self) -> Result<bool, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        Ok(self.read_checkout_recovery_journal_locked()?.is_some())
    }

    /// Explicitly resolves a Git selector to an immutable commit. This is the
    /// repository primitive behind `sys.snapshot(selector)`, not new source
    /// grammar for bare branch expressions.
    pub fn resolve_snapshot(&self, selector: &str) -> Result<GitCommitRef, RepositoryError> {
        if selector.is_empty() || selector.starts_with('-') {
            return Err(RepositoryError::InvalidSelector);
        }
        let expression = format!("{selector}^{{commit}}");
        self.commit_required(&expression)
    }

    /// Resolves one exact native Git object ID as a reachable immutable
    /// commit. This accepts only the repository's native object format and
    /// never creates or hydrates Git objects.
    pub fn resolve_committed_oid(
        &self,
        algorithm: orna_foundation_v1::GitHash,
        oid: &[u8],
    ) -> Result<GitCommitRef, RepositoryError> {
        let native_length = self.native_object_id_length()?;
        let (expected_algorithm, expected_bytes) = match native_length {
            40 => (orna_foundation_v1::GitHash::Sha1, 20),
            64 => (orna_foundation_v1::GitHash::Sha256, 32),
            _ => return Err(RepositoryError::UnsupportedObjectFormat),
        };
        if algorithm != expected_algorithm || oid.len() != expected_bytes {
            return Err(RepositoryError::InvalidObjectId);
        }

        let mut object_id = String::with_capacity(native_length);
        for byte in oid {
            use std::fmt::Write as _;
            write!(object_id, "{byte:02x}").map_err(|_| RepositoryError::GitOperationFailed)?;
        }
        self.commit_required(&format!("{object_id}^{{commit}}"))
    }

    /// Reads one bounded regular file from an already-resolved reachable
    /// commit without consulting or changing the worktree, index, or refs.
    ///
    /// Git tree entries that are directories, symlinks, or submodules are not
    /// source files and are rejected at this boundary. A size limit is
    /// required so a historical snapshot cannot make a caller allocate an
    /// unbounded source buffer.
    pub fn read_committed_file(
        &self,
        commit: &GitCommitRef,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        let resolved = self.commit_required(&format!("{}^{{commit}}", commit.as_str()))?;
        if resolved != *commit {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.read_tree_file(commit.as_str(), path, max_bytes)
    }

    /// Returns the exact commit recorded by one gitlink in a committed tree.
    /// The returned ID belongs to the attached repository; callers must open
    /// that repository and resolve the object there before reading its files.
    pub fn committed_submodule_commit(
        &self,
        commit: &GitCommitRef,
        path: impl AsRef<Path>,
    ) -> Result<GitCommitRef, RepositoryError> {
        let resolved = self.commit_required(&format!("{}^{{commit}}", commit.as_str()))?;
        if resolved != *commit {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.read_submodule_commit(commit.as_str(), path)
    }

    /// Returns the gitlink commit from a private publication candidate.
    pub fn private_candidate_submodule_commit(
        &self,
        candidate: &PrivateCommit,
        path: impl AsRef<Path>,
    ) -> Result<GitCommitRef, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.read_submodule_commit(candidate.tree.as_str(), path)
    }

    fn read_submodule_commit(
        &self,
        treeish: &str,
        path: impl AsRef<Path>,
    ) -> Result<GitCommitRef, RepositoryError> {
        let path = ManagedPath::new(path)?;
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-z", "--full-tree", treeish, "--"])
            .arg(path.as_path());
        let output = self.run(command)?;
        let mut entries = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty());
        let entry = entries.next().ok_or(RepositoryError::GitOperationFailed)?;
        if entries.next().is_some() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(RepositoryError::GitOperationFailed)?;
        if &entry[tab + 1..] != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let mut fields = entry[..tab].split(|byte| *byte == b' ');
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let object = fields.next().unwrap_or_default();
        if fields.next().is_some()
            || mode != b"160000"
            || kind != b"commit"
            || object.len() != self.native_object_id_length()?
            || !object.iter().all(u8::is_ascii_hexdigit)
        {
            return Err(RepositoryError::GitOperationFailed);
        }
        let object =
            String::from_utf8(object.to_vec()).map_err(|_| RepositoryError::GitOperationFailed)?;
        GitCommitRef::from_verified_commit(object, self.native_object_id_length()?)
    }

    /// Reads one bounded regular file from the exact immutable tree carried by
    /// a private candidate. The opaque `PrivateCommit` capability is required:
    /// raw unreachable commit IDs do not become valid committed snapshots.
    pub fn read_private_candidate_file(
        &self,
        candidate: &PrivateCommit,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.read_tree_file(candidate.tree.as_str(), path, max_bytes)
    }

    fn read_tree_file(
        &self,
        treeish: &str,
        path: impl AsRef<Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        let path = ManagedPath::new(path)?;
        let mut tree = self.command();
        tree.env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-z", "--full-tree", treeish, "--"])
            .arg(path.as_path());
        let tree_output = self.run(tree)?;
        let entry = tree_output
            .stdout
            .split(|byte| *byte == 0)
            .find(|entry| !entry.is_empty())
            .ok_or(RepositoryError::GitOperationFailed)?;
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(RepositoryError::GitOperationFailed)?;
        if &entry[tab + 1..] != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let mut fields = entry[..tab].split(|byte| *byte == b' ');
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let object = fields.next().unwrap_or_default();
        if fields.next().is_some() || mode != b"100644" && mode != b"100755" || kind != b"blob" {
            return Err(RepositoryError::GitOperationFailed);
        }
        let object =
            String::from_utf8(object.to_vec()).map_err(|_| RepositoryError::GitOperationFailed)?;
        GitCommitRef::from_verified_commit(object.clone(), self.native_object_id_length()?)?;

        let mut size_command = self.command();
        size_command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "-s", object.as_str()]);
        let size = trim_output(&self.run(size_command)?.stdout)
            .parse::<usize>()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if size > max_bytes {
            return Err(RepositoryError::GitOperationFailed);
        }

        let mut read_command = self.command();
        read_command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["cat-file", "blob", object.as_str()])
            .stdout(Stdio::piped());
        let mut child = read_command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let mut bytes = Vec::with_capacity(size);
        let stdout = child
            .stdout
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?;
        stdout
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !status.success() || bytes.len() > max_bytes || bytes.len() != size {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(bytes)
    }

    /// Lists a committed tree recursively without consulting or changing the
    /// worktree, index, or refs.
    ///
    /// The caller supplies an entry limit before Git output is accumulated.
    /// Symlinks and submodules are returned for explicit rejection by the
    /// caller; malformed or unsupported tree modes fail closed. File bytes
    /// remain unavailable through this listing and require
    /// [`Self::read_committed_file`] with a separate byte bound.
    pub fn list_committed_tree(
        &self,
        commit: &GitCommitRef,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        let resolved = self.commit_required(&format!("{}^{{commit}}", commit.as_str()))?;
        if resolved != *commit {
            return Err(RepositoryError::GitOperationFailed);
        }
        self.list_tree_entries(commit.as_str(), max_entries)
    }

    /// Lists the exact immutable tree carried by a private candidate without
    /// consulting the worktree, index, or refs. The candidate capability is
    /// bound to both its unreachable commit and root tree; callers cannot
    /// authorize arbitrary unreachable objects by selecting an object ID.
    pub fn list_private_candidate_tree(
        &self,
        candidate: &PrivateCommit,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        self.verify_private_candidate(candidate)?;
        self.list_tree_entries(candidate.tree.as_str(), max_entries)
    }

    fn verify_private_candidate(&self, candidate: &PrivateCommit) -> Result<(), RepositoryError> {
        let commit_expression = format!("{}^{{commit}}", candidate.commit.as_str());
        let resolved = self
            .commit_optional(&commit_expression)?
            .ok_or(RepositoryError::GitOperationFailed)?;
        if resolved != candidate.commit {
            return Err(RepositoryError::GitOperationFailed);
        }

        let tree_expression = format!("{}^{{tree}}", candidate.commit.as_str());
        let mut tree_command = self.command();
        tree_command.args(["rev-parse", "--verify", &tree_expression]);
        let resolved_tree =
            self.index_tree_from_native_oid(trim_output(&self.run(tree_command)?.stdout))?;
        if resolved_tree != candidate.tree {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(())
    }

    fn list_tree_entries(
        &self,
        treeish: &str,
        max_entries: usize,
    ) -> Result<Vec<CommittedTreeEntry>, RepositoryError> {
        const MAX_ENTRY_BYTES: usize = 4 * 1024;

        self.objects().check_expansion(treeish, "")?;
        let object_id_length = self.native_object_id_length()?;
        let mut command = self.command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["ls-tree", "-r", "-z", "--full-tree", treeish])
            .stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let listed = {
            let stdout = child
                .stdout
                .take()
                .ok_or(RepositoryError::GitOperationFailed)?;
            let mut stdout = BufReader::new(stdout);
            let mut entries = Vec::new();
            (|| {
                loop {
                    let mut raw_entry = Vec::new();
                    let bytes = (&mut stdout)
                        .take((MAX_ENTRY_BYTES + 1) as u64)
                        .read_until(b'\0', &mut raw_entry)
                        .map_err(|_| RepositoryError::GitOperationFailed)?;
                    if bytes == 0 {
                        break;
                    }
                    if raw_entry.len() > MAX_ENTRY_BYTES || raw_entry.last() != Some(&b'\0') {
                        return Err(RepositoryError::GitOperationFailed);
                    }
                    if entries.len() == max_entries {
                        return Err(RepositoryError::GitOperationFailed);
                    }
                    raw_entry.pop();
                    entries.push(parse_committed_tree_entry(&raw_entry, object_id_length)?);
                }
                Ok(entries)
            })()
        };
        let entries = match listed {
            Ok(entries) => entries,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(entries)
    }

    /// Stages exactly `paths` through normal Git index semantics.
    ///
    /// It checks the caller's observed index generation first, so stale
    /// logical-stage records fail rather than silently being rebased. Git's
    /// pathspec separator ensures no path beginning with `-` is interpreted as
    /// an option. No worktree file is written by this operation.
    pub fn stage_managed(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
    ) -> Result<IndexGeneration, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.index_generation()?;
        if &actual != expected {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        self.update_managed_index(expected, paths, ManagedIndexOperation::Add, None)
    }

    /// Stages the exact managed-file bytes already captured by a caller.
    ///
    /// The candidate index is built off-index and atomically installed only if
    /// the observed index generation remains current and the worktree bytes
    /// still match the supplied contents. Git path filters and the file mode
    /// are preserved while the staged blob comes from those contents, not a
    /// second worktree read.
    pub fn stage_managed_contents(
        &self,
        expected: &IndexGeneration,
        path: &ManagedPath,
        contents: &[u8],
    ) -> Result<IndexGeneration, RepositoryError> {
        self.stage_managed_contents_inner(expected, path, contents, None)
    }

    /// Test-only interleaving seam for worktree changes between candidate
    /// construction and installation.
    #[doc(hidden)]
    pub fn stage_managed_contents_with_test_hook(
        &self,
        expected: &IndexGeneration,
        path: &ManagedPath,
        contents: &[u8],
        hook: &mut dyn FnMut(),
    ) -> Result<IndexGeneration, RepositoryError> {
        self.stage_managed_contents_inner(expected, path, contents, Some(hook))
    }

    fn stage_managed_contents_inner(
        &self,
        expected: &IndexGeneration,
        path: &ManagedPath,
        contents: &[u8],
        before_install: Option<&mut dyn FnMut()>,
    ) -> Result<IndexGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.index_generation()?;
        if &actual != expected {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        self.ensure_managed_contents_match(path, contents)?;
        self.update_managed_index(
            expected,
            std::slice::from_ref(path),
            ManagedIndexOperation::AddContents(contents),
            before_install,
        )
    }

    /// Unstages exactly `paths`, retaining their worktree bytes.  This is the
    /// inverse scoped index primitive and is intentionally not `reset --hard`.
    pub fn unstage_managed(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
    ) -> Result<IndexGeneration, RepositoryError> {
        if paths.is_empty() {
            return Err(RepositoryError::NoManagedPaths);
        }
        let _lock = self.acquire_coordination_lock()?;
        let actual = self.index_generation()?;
        if &actual != expected {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        self.update_managed_index(expected, paths, ManagedIndexOperation::Unstage, None)
    }

    /// Test-only interleaving seam. Production callers use stage_managed or
    /// stage_managed_contents; this makes a normal Git writer race deterministic
    /// without weakening the production protocol.
    #[doc(hidden)]
    pub fn stage_managed_with_test_hook(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        hook: &mut dyn FnMut(),
    ) -> Result<IndexGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.update_managed_index(expected, paths, ManagedIndexOperation::Add, Some(hook))
    }

    /// Test-only equivalent of [`Self::stage_managed_with_test_hook`].
    #[doc(hidden)]
    pub fn unstage_managed_with_test_hook(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        hook: &mut dyn FnMut(),
    ) -> Result<IndexGeneration, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.update_managed_index(expected, paths, ManagedIndexOperation::Unstage, Some(hook))
    }

    /// Creates `name` at the selected commit using Git's nonexistence CAS.
    /// It makes no commit and does not modify the index or worktree.
    pub fn create_branch_at_head(&self, name: &str) -> Result<GitCommitRef, RepositoryError> {
        if !valid_branch_name(name) {
            return Err(RepositoryError::InvalidBranchName);
        }
        let _lock = self.acquire_coordination_lock()?;
        let head = self.head()?.ok_or(RepositoryError::UnbornHead)?;
        let reference = format!("refs/heads/{name}");
        self.update_ref_transaction(&head, &reference)?;
        Ok(head)
    }

    /// Lists ordinary configured remote names. Internal refs are not published
    /// here; publication is deliberately owned by a later layer.
    pub fn remote_names(&self) -> Result<Vec<String>, RepositoryError> {
        let output = self.git(["remote"])?;
        Ok(output
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Passively verifies that a configured ordinary remote has exactly the
    /// required `refs/orna/*` object identities. This invokes only
    /// `git ls-remote --refs`; it never fetches, pushes, hydrates promised
    /// objects, or changes local configuration, refs, index, or worktree.
    ///
    /// A plain-Git transfer that lacks a required ref is `Missing`; a present
    /// ref at another object is `Stale`. Invalid input, unexpected internal
    /// refs, malformed command output, unsupported local object format, or an
    /// unreachable remote are all `Unverifiable` rather than guessed at.
    pub fn observe_remote_continuity(
        &self,
        remote: &str,
        required: &[RequiredInternalRef],
    ) -> RemoteContinuity {
        if required.is_empty() {
            return RemoteContinuity::InvalidEvidence;
        }
        if !valid_remote_name(remote) {
            return RemoteContinuity::Unverifiable;
        }
        let native_length = match self.observer_native_object_id_length() {
            Ok(length) => length,
            Err(_) => return RemoteContinuity::Unverifiable,
        };
        let mut expected = std::collections::BTreeMap::new();
        for requirement in required {
            if requirement.expected.as_str().len() != native_length
                || expected
                    .insert(
                        requirement.reference.as_str(),
                        requirement.expected.as_str(),
                    )
                    .is_some()
            {
                return RemoteContinuity::InvalidEvidence;
            }
        }
        let remotes = match self.observer_remote_names() {
            Ok(remotes) => remotes,
            Err(_) => return RemoteContinuity::Unverifiable,
        };
        if !remotes.iter().any(|candidate| candidate == remote) {
            return RemoteContinuity::Unverifiable;
        }
        let observed = match self.observer_remote_orna_refs(remote, native_length, &expected) {
            Ok(observed) => observed,
            Err(_) => return RemoteContinuity::Unverifiable,
        };
        if expected
            .keys()
            .any(|reference| !observed.contains_key(*reference))
        {
            return RemoteContinuity::Missing;
        }
        if expected.iter().any(|(reference, expected_id)| {
            observed
                .get(*reference)
                .is_none_or(|observed_id| !observed_id.eq_ignore_ascii_case(expected_id))
        }) {
            return RemoteContinuity::Stale;
        }
        RemoteContinuity::Continuous
    }

    /// Observes sparse-checkout and partial/promisor configuration without
    /// changing Git configuration, refs, the index, or the worktree.
    ///
    /// This is the repository-local half of ORNA-GIT-004: sparse checkout is
    /// a working-tree property, while partial clone is an object-materiality
    /// property. The observation does not fetch, hydrate, or infer allocator
    /// or transport semantics.
    pub fn observe_git_capabilities(&self) -> Result<GitRepositoryCapabilities, RepositoryError> {
        let remotes = match self.observer_remote_names() {
            Ok(remotes) => remotes,
            Err(RepositoryError::GitUnavailable) => {
                return Err(RepositoryError::GitUnavailable);
            }
            Err(_) => {
                return Ok(GitRepositoryCapabilities {
                    mode: GitRepositoryMode::Malformed,
                    sparse_checkout: false,
                    partial_clone: false,
                    promisor_remotes: Vec::new(),
                });
            }
        };
        if remotes.iter().any(|remote| !valid_remote_name(remote)) {
            return Ok(GitRepositoryCapabilities {
                mode: GitRepositoryMode::Malformed,
                sparse_checkout: false,
                partial_clone: false,
                promisor_remotes: Vec::new(),
            });
        }

        let mut malformed = false;
        let sparse_checkout = match self.worktree_or_local_config_value("core.sparseCheckout")? {
            ConfigValue::Missing => false,
            ConfigValue::Value(value) => match parse_git_bool(&value) {
                Some(value) => value,
                None => {
                    malformed = true;
                    false
                }
            },
            ConfigValue::Malformed => {
                malformed = true;
                false
            }
        };

        let mut promisor_remotes = Vec::new();
        for remote in &remotes {
            let key = format!("remote.{remote}.promisor");
            match self.local_config_value(&key)? {
                ConfigValue::Missing => {}
                ConfigValue::Value(value) => match parse_git_bool(&value) {
                    Some(true) => promisor_remotes.push(remote.clone()),
                    Some(false) => {}
                    None => malformed = true,
                },
                ConfigValue::Malformed => malformed = true,
            }
        }

        let partial_clone_remote = match self.local_config_value("extensions.partialClone")? {
            ConfigValue::Missing => None,
            ConfigValue::Value(value)
                if valid_remote_name(&value) && remotes.iter().any(|remote| remote == &value) =>
            {
                Some(value)
            }
            ConfigValue::Value(_) | ConfigValue::Malformed => {
                malformed = true;
                None
            }
        };
        if let Some(remote) = &partial_clone_remote
            && !promisor_remotes.iter().any(|candidate| candidate == remote)
        {
            malformed = true;
        }

        let partial_clone = partial_clone_remote.is_some() || !promisor_remotes.is_empty();
        let mode = if malformed {
            GitRepositoryMode::Malformed
        } else {
            match (sparse_checkout, partial_clone) {
                (false, false) => GitRepositoryMode::Ordinary,
                (true, false) => GitRepositoryMode::SparseCheckout,
                (false, true) => GitRepositoryMode::PartialClone,
                (true, true) => GitRepositoryMode::Combined,
            }
        };
        Ok(GitRepositoryCapabilities {
            mode,
            sparse_checkout,
            partial_clone,
            promisor_remotes,
        })
    }

    /// Observes one complete native Git object ID without invoking Git's
    /// fetch or filter machinery. Missing objects are classified as promised
    /// only when valid local promisor configuration and reachability evidence
    /// support that interpretation; otherwise they remain unavailable.
    pub fn observe_git_object(&self, object_id: &str) -> Result<GitObjectState, RepositoryError> {
        // Native Git object IDs are hexadecimal and Git reports them in their
        // canonical lowercase form. Normalize the caller spelling before
        // comparing command output so this observer agrees with
        // `NativeObjectId::new` without weakening exact-ID validation.
        let object_id = object_id.to_ascii_lowercase();
        let object_id_length = self.observer_native_object_id_length()?;
        if object_id.len() != object_id_length
            || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(GitObjectState::Malformed);
        }

        let capabilities = self.observe_git_capabilities()?;
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command
            .args([
                "cat-file",
                "--batch-check=%(objectname) %(objecttype) %(objectsize)",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(format!("{object_id}\n").as_bytes())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if !output.status.success() {
            // `cat-file` reports corrupt loose/packed objects through its
            // exit status.  Keep the observation redacted while preserving
            // the materialisation distinction for callers.
            return Ok(GitObjectState::Malformed);
        }
        let line = trim_output(&output.stdout);
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() == 2 && fields[0] == object_id && fields[1] == "missing" {
            if git_stderr_indicates_malformed_object(&output.stderr) {
                return Ok(GitObjectState::Malformed);
            }
            return Ok(if capabilities.mode == GitRepositoryMode::Malformed {
                GitObjectState::Malformed
            } else if capabilities.partial_clone {
                match self.promisor_reachability(&object_id)? {
                    PromisorReachability::Proven => GitObjectState::Promised,
                    PromisorReachability::NotProven => GitObjectState::Unavailable,
                    PromisorReachability::Malformed => GitObjectState::Malformed,
                }
            } else {
                GitObjectState::Unavailable
            });
        }
        if fields.len() != 3 || fields[0] != object_id {
            return Ok(GitObjectState::Malformed);
        }
        let kind = match fields[1] {
            "commit" => GitObjectKind::Commit,
            "tree" => GitObjectKind::Tree,
            "blob" => GitObjectKind::Blob,
            "tag" => GitObjectKind::Tag,
            _ => return Ok(GitObjectState::Malformed),
        };
        let Ok(size) = fields[2].parse::<u64>() else {
            return Ok(GitObjectState::Malformed);
        };
        Ok(GitObjectState::Materialized { kind, size })
    }

    /// Classifies a caller-declared object set without fetching, hydrating, or
    /// changing local Git state. A promised object preserves logical
    /// completeness; an unavailable or malformed object never does.
    pub fn observe_declared_git_object_set(
        &self,
        object_ids: &[&str],
    ) -> Result<GitDeclaredObjectSetState, RepositoryError> {
        let mut incomplete = false;
        for object_id in object_ids {
            match self.observe_git_object(object_id)? {
                GitObjectState::Materialized { .. } | GitObjectState::Promised => {}
                GitObjectState::Unavailable => incomplete = true,
                GitObjectState::Malformed => return Ok(GitDeclaredObjectSetState::Malformed),
            }
        }
        Ok(if incomplete {
            GitDeclaredObjectSetState::Incomplete
        } else {
            GitDeclaredObjectSetState::Complete
        })
    }

    /// Classifies a manifest-declared set of immutable storage segment blob
    /// IDs without fetching, hydrating, or changing local Git state.
    ///
    /// Unlike [`Self::observe_declared_git_object_set`], this boundary is for
    /// objects that a verified storage manifest has already declared to be
    /// segment blobs. A locally materialized commit, tree, or tag is therefore
    /// invalid evidence for such a declaration. A missing object remains
    /// logically available only when the local promisor walk proves its
    /// declared ID reachable; the manifest retains the segment's blob role
    /// while its bytes are absent.
    pub fn observe_declared_segment_blob_set(
        &self,
        object_ids: &[&str],
    ) -> Result<GitDeclaredObjectSetState, RepositoryError> {
        let mut incomplete = false;
        for object_id in object_ids {
            match self.observe_git_object(object_id)? {
                GitObjectState::Materialized {
                    kind: GitObjectKind::Blob,
                    ..
                }
                | GitObjectState::Promised => {}
                GitObjectState::Materialized { .. } | GitObjectState::Malformed => {
                    return Ok(GitDeclaredObjectSetState::Malformed);
                }
                GitObjectState::Unavailable => incomplete = true,
            }
        }
        Ok(if incomplete {
            GitDeclaredObjectSetState::Incomplete
        } else {
            GitDeclaredObjectSetState::Complete
        })
    }

    /// Hydrates only the promised segment blobs declared by a verified
    /// compact manifest. The complete manifest and shard witness is checked
    /// before any fetch is attempted; materialized segments are checked for
    /// their physical identity and promised segments are fetched through the
    /// exact-object transport boundary. A final verification prevents a
    /// partially hydrated or mismatched segment from being reported as ready.
    ///
    /// Git refs, `HEAD`, `FETCH_HEAD`, the index, worktree, runtime files and
    /// objects outside this manifest are not installation targets of this
    /// operation. Hydration may add the requested immutable object bytes to
    /// the local object store, but it never changes logical repository state.
    pub fn hydrate_compact_manifest_segments(
        &self,
        commit: &GitCommitRef,
        table: Uuid,
    ) -> Result<BTreeMap<String, GitObjectState>, FetchError> {
        let plan = self
            .plan_compact_manifest_hydration(commit, table)
            .map_err(FetchError::Repository)?;
        for (object_id, state) in &plan {
            if matches!(state, GitObjectState::Promised) {
                self.hydrate_promised_object(object_id)?;
            }
        }
        self.plan_compact_manifest_hydration(commit, table)
            .map_err(FetchError::Repository)
    }

    /// Hydrates only the promised compact-segment blobs whose verified
    /// manifest bounds overlap the inclusive key range. The range planner is
    /// the authority for selection and validation; each selected promise is
    /// materialized through the exact-object transport boundary, then the
    /// same range is planned again to verify the postcondition.
    ///
    /// This operation may add selected immutable object bytes to the local
    /// object store, but it never installs refs, changes `HEAD`, writes
    /// `FETCH_HEAD`, updates the index or worktree, or considers segments
    /// outside the requested range.
    pub fn hydrate_compact_manifest_segments_for_key_range(
        &self,
        commit: &GitCommitRef,
        table: Uuid,
        lower_key: &[u8],
        upper_key: &[u8],
    ) -> Result<BTreeMap<String, GitObjectState>, FetchError> {
        let plan = self
            .plan_compact_manifest_hydration_for_key_range(commit, table, lower_key, upper_key)
            .map_err(FetchError::Repository)?;
        for (object_id, state) in &plan {
            if matches!(state, GitObjectState::Promised) {
                self.hydrate_promised_object(object_id)?;
            }
        }
        self.plan_compact_manifest_hydration_for_key_range(commit, table, lower_key, upper_key)
            .map_err(FetchError::Repository)
    }

    fn local_config_value(&self, key: &str) -> Result<ConfigValue, RepositoryError> {
        self.config_value("--local", key)
    }

    fn worktree_or_local_config_value(&self, key: &str) -> Result<ConfigValue, RepositoryError> {
        match self.config_value("--worktree", key)? {
            ConfigValue::Missing => self.local_config_value(key),
            value => Ok(value),
        }
    }

    fn config_value(&self, scope: &str, key: &str) -> Result<ConfigValue, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command.args(["config", scope, "--get", key]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            return Ok(ConfigValue::Value(trim_output(&output.stdout)));
        }
        if output.status.code() == Some(1) {
            return Ok(ConfigValue::Missing);
        }
        Ok(ConfigValue::Malformed)
    }

    fn observer_remote_names(&self) -> Result<Vec<String>, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command.arg("remote");
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(trim_output(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn observer_remote_orna_refs(
        &self,
        remote: &str,
        object_id_length: usize,
        expected: &std::collections::BTreeMap<&str, &str>,
    ) -> Result<std::collections::BTreeMap<String, String>, RepositoryError> {
        let mut command = self.observer_command();
        command
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["ls-remote", "--refs", remote])
            .args(expected.keys());
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let output =
            std::str::from_utf8(&output.stdout).map_err(|_| RepositoryError::GitOperationFailed)?;
        parse_remote_orna_refs(output, object_id_length, expected)
    }

    fn observer_native_object_id_length(&self) -> Result<usize, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1");
        command.args(["rev-parse", "--show-object-format"]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            return Err(RepositoryError::GitOperationFailed);
        }
        match trim_output(&output.stdout).as_str() {
            "sha1" => Ok(40),
            "sha256" => Ok(64),
            _ => Err(RepositoryError::UnsupportedObjectFormat),
        }
    }

    fn promisor_reachability(
        &self,
        object_id: &str,
    ) -> Result<PromisorReachability, RepositoryError> {
        let mut command = self.observer_command();
        command.env("GIT_NO_LAZY_FETCH", "1").args([
            "rev-list",
            "--objects",
            "--missing=print",
            "--no-object-names",
            "--all",
        ]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if !output.status.success() {
            // A local reachability walk cannot safely distinguish an absent
            // object from a malformed object database when Git rejects it.
            return Ok(PromisorReachability::Malformed);
        }
        let promised = format!("?{object_id}");
        Ok(
            if trim_output(&output.stdout)
                .lines()
                .any(|line| line.eq_ignore_ascii_case(&promised))
            {
                PromisorReachability::Proven
            } else {
                PromisorReachability::NotProven
            },
        )
    }

    /// Atomically materialises one repository-managed file while preserving
    /// an ordinary editor's later change. The expected bytes are checked both
    /// before and immediately before replacement or removal; `None` removes
    /// an existing regular file and is a no-op when the file is already absent.
    pub fn materialize_managed_file(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
    ) -> Result<(), RepositoryError> {
        self.materialize_managed_file_impl(path, expected, next, None)
    }

    fn materialize_managed_file_impl(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        before_install: Option<&mut dyn FnMut()>,
    ) -> Result<(), RepositoryError> {
        self.materialize_managed_file_impl_with_quarantine(
            path,
            expected,
            next,
            None,
            before_install,
            None,
        )
    }

    fn materialize_managed_file_impl_with_quarantine(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        quarantine_path: Option<&Path>,
        before_install: Option<&mut dyn FnMut()>,
        after_boundary: Option<
            &mut dyn FnMut(PublicationMaterializationPhase) -> Result<(), RepositoryError>,
        >,
    ) -> Result<(), RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        self.materialize_managed_file_impl_with_quarantine_locked(
            path,
            expected,
            next,
            quarantine_path,
            before_install,
            after_boundary,
        )
    }

    fn materialize_managed_file_impl_with_quarantine_locked(
        &self,
        path: &ManagedPath,
        expected: Option<&[u8]>,
        next: Option<&[u8]>,
        quarantine_path: Option<&Path>,
        mut before_install: Option<&mut dyn FnMut()>,
        mut after_boundary: Option<
            &mut dyn FnMut(PublicationMaterializationPhase) -> Result<(), RepositoryError>,
        >,
    ) -> Result<(), RepositoryError> {
        self.ensure_atomic_worktree_install_supported()?;
        let target = self.managed_target(path)?;
        let current = self.read_managed_file(&target)?;
        if current.as_deref() != expected {
            return Err(RepositoryError::ManagedContentConflict);
        }
        if let Some(bytes) = next {
            let parent = target
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?;
            fs::create_dir_all(parent).map_err(|_| RepositoryError::LocalStateUnavailable)?;
            self.validate_managed_parent(parent)?;
            let candidate = parent.join(format!(
                ".orna-materialize-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?
                    .as_nanos()
            ));
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&candidate)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                file.write_all(bytes)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                file.sync_all()
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
                if self.read_managed_file(&target)?.as_deref() != expected {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                let quarantine = if expected.is_some() {
                    let quarantine = match quarantine_path {
                        Some(path) => {
                            match fs::symlink_metadata(path) {
                                Ok(_) => return Err(RepositoryError::ManagedContentConflict),
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                Err(_) => {
                                    return Err(RepositoryError::LocalStateUnavailable);
                                }
                            }
                            path.to_path_buf()
                        }
                        None => self.new_managed_quarantine(parent)?,
                    };
                    if let Some(hook) = before_install.as_mut() {
                        hook();
                    }
                    match Self::rename_path_without_replacement(&target, &quarantine) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Err(RepositoryError::ManagedContentConflict);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(RepositoryError::ManagedContentConflict);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                            return Err(RepositoryError::PlatformUnsupported);
                        }
                        Err(_) => {
                            return Err(RepositoryError::LocalStateUnavailable);
                        }
                    }
                    if self.read_managed_file(&quarantine)?.as_deref() != expected {
                        return match self.restore_managed_quarantine(&quarantine, &target) {
                            Ok(()) | Err(RepositoryError::ManagedContentConflict) => {
                                Err(RepositoryError::ManagedContentConflict)
                            }
                            Err(error) => Err(error),
                        };
                    }
                    if let Some(hook) = after_boundary.as_mut() {
                        hook(PublicationMaterializationPhase::Quarantined)?;
                    }
                    Some(quarantine)
                } else {
                    None
                };
                if expected.is_none() {
                    if let Some(hook) = before_install.as_mut() {
                        hook();
                    }
                    if self.read_managed_file(&target)?.as_deref() != expected {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                }
                self.install_managed_candidate(&candidate, &target)?;
                if let Some(hook) = after_boundary.as_mut() {
                    hook(PublicationMaterializationPhase::Installed)?;
                }
                if let Some(quarantine) = quarantine {
                    self.remove_verified_quarantine(parent, &quarantine, &target, expected, next)?;
                }
                fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)
            })();
            if result.is_err() {
                let _ = fs::remove_file(&candidate);
            }
            result
        } else {
            if target.exists() {
                if !fs::symlink_metadata(&target)
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?
                    .is_file()
                {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
                if let Some(hook) = before_install.as_mut() {
                    hook();
                }
                if self.read_managed_file(&target)?.as_deref() != expected {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                let parent = target
                    .parent()
                    .ok_or(RepositoryError::LocalStateUnavailable)?;
                let quarantine = match quarantine_path {
                    Some(path) => {
                        match fs::symlink_metadata(path) {
                            Ok(_) => return Err(RepositoryError::ManagedContentConflict),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
                        }
                        path.to_path_buf()
                    }
                    None => self.new_managed_quarantine(parent)?,
                };
                if let Some(hook) = before_install.as_mut() {
                    hook();
                }
                match Self::rename_path_without_replacement(&target, &quarantine) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err(RepositoryError::ManagedContentConflict);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                        return Err(RepositoryError::PlatformUnsupported);
                    }
                    Err(_) => {
                        return Err(RepositoryError::LocalStateUnavailable);
                    }
                }
                if self.read_managed_file(&quarantine)?.as_deref() != expected {
                    return match self.restore_managed_quarantine(&quarantine, &target) {
                        Ok(()) | Err(RepositoryError::ManagedContentConflict) => {
                            Err(RepositoryError::ManagedContentConflict)
                        }
                        Err(error) => Err(error),
                    };
                }
                if let Some(hook) = after_boundary.as_mut() {
                    hook(PublicationMaterializationPhase::Quarantined)?;
                }
                if fs::symlink_metadata(&target).is_ok() {
                    return Err(RepositoryError::ManagedContentConflict);
                }
                if let Some(hook) = after_boundary.as_mut() {
                    hook(PublicationMaterializationPhase::Installed)?;
                }
                self.remove_verified_quarantine(parent, &quarantine, &target, expected, None)?;
                fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            }
            Ok(())
        }
    }

    fn new_managed_quarantine(&self, parent: &Path) -> Result<PathBuf, RepositoryError> {
        for _ in 0..8 {
            let quarantine = parent.join(format!(
                ".orna-quarantine-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            match fs::symlink_metadata(&quarantine) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(quarantine);
                }
                Ok(_) => continue,
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
        Err(RepositoryError::LocalStateUnavailable)
    }

    #[cfg(target_os = "linux")]
    fn rename_path_without_replacement(from: &Path, to: &Path) -> std::io::Result<()> {
        let from_parent = from
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let to_parent = to
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let from_directory = rustix::fs::openat(
            rustix::fs::CWD,
            from_parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let to_directory = rustix::fs::openat(
            rustix::fs::CWD,
            to_parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        rustix::fs::renameat_with(
            &from_directory,
            from.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing source name")
            })?,
            &to_directory,
            to.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing target name")
            })?,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
    }

    #[cfg(not(target_os = "linux"))]
    fn rename_path_without_replacement(_from: &Path, _to: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }

    fn restore_managed_quarantine(
        &self,
        quarantine: &Path,
        target: &Path,
    ) -> Result<(), RepositoryError> {
        self.restore_managed_quarantine_impl(quarantine, target, None)
    }

    fn restore_managed_quarantine_impl(
        &self,
        quarantine: &Path,
        target: &Path,
        mut before_restore: Option<&mut dyn FnMut()>,
    ) -> Result<(), RepositoryError> {
        match fs::symlink_metadata(target) {
            Ok(_) => Err(RepositoryError::ManagedContentConflict),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(hook) = before_restore.as_mut() {
                    hook();
                }
                match fs::hard_link(quarantine, target) {
                    Ok(()) => {
                        let parent = target
                            .parent()
                            .ok_or(RepositoryError::LocalStateUnavailable)?;
                        let target_bytes = self.read_managed_file(target)?;
                        self.remove_verified_quarantine(
                            parent,
                            quarantine,
                            target,
                            target_bytes.as_deref(),
                            target_bytes.as_deref(),
                        )
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        Err(RepositoryError::ManagedContentConflict)
                    }
                    Err(_) => Err(RepositoryError::LocalStateUnavailable),
                }
            }
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn install_managed_candidate(
        &self,
        candidate: &Path,
        target: &Path,
    ) -> Result<(), RepositoryError> {
        match fs::hard_link(candidate, target) {
            Ok(()) => {
                fs::remove_file(candidate).map_err(|_| RepositoryError::LocalStateUnavailable)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(RepositoryError::ManagedContentConflict)
            }
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    /// Observes one managed regular file without exposing host paths. The
    /// observation is serialized with Orna's repository coordination lock;
    /// callers that later materialise content must still pass the observed
    /// bytes back as the expected value for the final revalidation.
    pub fn managed_file_bytes(
        &self,
        path: &ManagedPath,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let _lock = self.acquire_coordination_lock()?;
        let target = self.managed_target(path)?;
        self.read_managed_file(&target)
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.worktree);
        scrub_git_routing_environment(&mut command);
        bind_git_discovery_to_worktree(&mut command, &self.worktree);
        command
    }

    /// Constructs a Git command for a passive observer.  This intentionally
    /// ignores inherited routing and object-store overrides so every query is
    /// bound to this repository's selected worktree.
    pub(crate) fn observer_command(&self) -> Command {
        let mut command = self.command();
        scrub_git_routing_environment(&mut command);
        command
    }
    fn git<const N: usize>(&self, args: [&str; N]) -> Result<String, RepositoryError> {
        Self::git_at(&self.worktree, args)
    }
    fn git_bytes_no_lazy_fetch<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<Vec<u8>, RepositoryError> {
        let mut command = self.command();
        command.env("GIT_NO_LAZY_FETCH", "1").args(args);
        Ok(self.run(command)?.stdout)
    }
    fn git_bytes<const N: usize>(&self, args: [&str; N]) -> Result<Vec<u8>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        Ok(self.run(command)?.stdout)
    }
    /// Parses paths Git reports for a committed tree or diff. Each is a
    /// committed name, so it passes [`ManagedPath::new`] in full.
    fn git_nul_paths(&self, args: &[String]) -> Result<HashSet<ManagedPath>, RepositoryError> {
        self.git_nul_paths_with(args, |path| ManagedPath::new(path))
    }

    /// Parses paths Git reports for the local worktree, which include untracked
    /// user directories such as `Sub/.GIT/`. Those are ordinary local data, so
    /// they take only the structural check.
    fn git_worktree_nul_paths(
        &self,
        args: &[String],
    ) -> Result<HashSet<ManagedPath>, RepositoryError> {
        self.git_nul_paths_with(args, ManagedPath::from_worktree_listing)
    }

    fn git_nul_paths_with(
        &self,
        args: &[String],
        parse: impl Fn(&str) -> Result<ManagedPath, RepositoryError>,
    ) -> Result<HashSet<ManagedPath>, RepositoryError> {
        let mut command = self.command();
        command.args(args);
        self.run(command)?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path = std::str::from_utf8(path)
                    .map_err(|_| RepositoryError::UnsafeManagedPath)?;
                parse(path)
            })
            .collect()
    }

    fn hash_object(&self, bytes: &[u8]) -> Result<String, RepositoryError> {
        self.hash_object_with_path(bytes, None)
    }

    fn hash_object_at_path(
        &self,
        bytes: &[u8],
        path: &ManagedPath,
    ) -> Result<String, RepositoryError> {
        self.hash_object_with_path(bytes, Some(path))
    }

    fn hash_object_with_path(
        &self,
        bytes: &[u8],
        path: Option<&ManagedPath>,
    ) -> Result<String, RepositoryError> {
        let mut command = self.command();
        command.args(["hash-object", "-w"]);
        if let Some(path) = path {
            let path = path
                .as_path()
                .to_str()
                .ok_or(RepositoryError::UnsafeManagedPath)?;
            command.arg(format!("--path={path}"));
        }
        command.arg("--stdin");
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(bytes)
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let output = child
            .wait_with_output()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if output.status.success() {
            Ok(trim_output(&output.stdout))
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }

    fn candidate_tree_entry(
        &self,
        candidate: &PrivateCommit,
        path: &ManagedPath,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        self.tree_entry(candidate.commit.as_str(), path)
    }

    /// Reads the entry at `path` in `treeish`. Only a blob, or the tree at
    /// the fixed `.orna/store` position, is admitted.
    fn tree_entry(
        &self,
        treeish: &str,
        path: &ManagedPath,
    ) -> Result<Option<(String, String)>, RepositoryError> {
        let mut command = self.command();
        command
            .args(["ls-tree", "-z", "--full-tree", treeish, "--"])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        let Some(entry) = output.split(|byte| *byte == 0).next() else {
            return Ok(None);
        };
        if entry.is_empty() {
            return Ok(None);
        }
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(RepositoryError::GitOperationFailed)?;
        let metadata = &entry[..tab];
        let encoded_path = &entry[tab + 1..];
        if encoded_path != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = String::from_utf8(fields.next().unwrap_or_default().to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let kind = fields.next().unwrap_or_default();
        let object = String::from_utf8(fields.next().unwrap_or_default().to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if kind == b"blob" {
            if fields.next().is_some() {
                return Err(RepositoryError::GitOperationFailed);
            }
            GitCommitRef::from_verified_commit(object.clone(), self.native_object_id_length()?)?;
        } else if path.as_path() == Path::new(".orna/store") && kind == b"tree" && mode == "040000"
        {
            if fields.next().is_some() {
                return Err(RepositoryError::GitOperationFailed);
            }
            IndexTreeRef::from_verified_tree(object.clone(), self.native_object_id_length()?)?;
        } else {
            return Err(RepositoryError::GitOperationFailed);
        }
        Ok(Some((mode, object)))
    }

    fn committed_store_tree_path(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<Option<ManagedPath>, RepositoryError> {
        let store_path = ManagedPath::new(".orna/store")?;
        if journal
            .entries()
            .iter()
            .any(|entry| &entry.path == &store_path)
        {
            return Err(RepositoryError::InvalidPublicationJournal);
        }
        match (
            journal.store_root(),
            self.candidate_tree_entry(candidate, &store_path)?,
        ) {
            (Some(expected), Some((mode, object)))
                if mode == "040000" && object == expected.as_str() =>
            {
                Ok(Some(store_path))
            }
            (None, Some((mode, _))) if mode == "040000" => Ok(Some(store_path)),
            (None, None) => Ok(None),
            _ => Err(RepositoryError::InvalidPublicationJournal),
        }
    }


    fn publication_index_paths(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<Vec<ManagedPath>, RepositoryError> {
        let mut paths = journal
            .entries()
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        if let Some(store_path) = self.committed_store_tree_path(candidate, journal)? {
            paths.push(store_path);
        }
        Ok(paths)
    }

    fn index_entries_match_candidate(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<bool, RepositoryError> {
        for path in self.publication_index_paths(candidate, journal)? {
            let expected = self.candidate_tree_entry(candidate, &path)?;
            if expected.as_ref().is_some_and(|(mode, _)| mode == "040000") {
                // The index holds a bound tree as its leaf entries, never as
                // one `040000` record, so compare the subtree the index writes
                // at this path with the candidate's store root (ORNA-PUB-007).
                let index = self.git_path("index")?;
                let Some(tree) = self.index_tree_from_file(&index)? else {
                    return Ok(false);
                };
                if self.tree_entry(tree.as_str(), &path)? != expected {
                    return Ok(false);
                }
                continue;
            }
            let mut command = self.command();
            command
                .args(["ls-files", "--stage", "-z", "--"])
                .arg(path.as_path());
            let output = self.run(command)?.stdout;
            let records = output
                .split(|byte| *byte == 0)
                .filter(|record| !record.is_empty())
                .collect::<Vec<_>>();
            match (expected, records.as_slice()) {
                (None, []) => {}
                (Some((mode, object)), [record]) => {
                    let tab = record
                        .iter()
                        .position(|byte| *byte == b'\t')
                        .ok_or(RepositoryError::GitOperationFailed)?;
                    let metadata = &record[..tab];
                    let encoded_path = &record[tab + 1..];
                    if encoded_path != path.as_path().as_os_str().as_encoded_bytes() {
                        return Ok(false);
                    }
                    let mut fields = metadata.split(|byte| *byte == b' ');
                    let actual_mode = fields.next().unwrap_or_default();
                    let actual_object = fields.next().unwrap_or_default();
                    let stage = fields.next().unwrap_or_default();
                    if actual_mode != mode.as_bytes()
                        || actual_object != object.as_bytes()
                        || stage != b"0"
                        || fields.next().is_some()
                    {
                        return Ok(false);
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
    /// Verifies recovery's persisted file intents and optional store-root
    /// tree witness against the private publication candidate. This prevents
    /// recovery from reconciling the index to a different file or store tree
    /// after the ref-advance crash boundary.
    fn candidate_entries_match_journal(
        &self,
        candidate: &PrivateCommit,
        journal: &PublicationJournal,
    ) -> Result<bool, RepositoryError> {
        let _store_path = self.committed_store_tree_path(candidate, journal)?;
        for entry in journal.entries() {
            match (
                entry.next(),
                self.candidate_tree_entry(candidate, &entry.path)?,
            ) {
                (None, None) => {}
                (Some(next), Some((_mode, object)))
                    if self.git_bytes(["cat-file", "blob", &object])? == next => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
    fn git_at<const N: usize>(
        directory: &Path,
        args: [&str; N],
    ) -> Result<String, RepositoryError> {
        let mut command = Command::new("git");
        command.current_dir(directory).args(args);
        scrub_git_routing_environment(&mut command);
        bind_git_discovery_to_worktree(&mut command, directory);
        let output = run_command(command)?;
        Ok(trim_output(&output.stdout))
    }
    pub(crate) fn run(&self, command: Command) -> Result<Output, RepositoryError> {
        run_command(command)
    }

    fn commit_optional(&self, expression: &str) -> Result<Option<GitCommitRef>, RepositoryError> {
        let mut command = self.command();
        command.args([
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            expression,
        ]);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            return self
                .snapshot_from_native_oid(trim_output(&output.stdout))
                .map(Some);
        }
        // This exact quiet rev-parse probe is the sole absence case. All
        // other exit statuses fail closed instead of treating 128 as absence.
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(None);
        }
        Err(RepositoryError::GitOperationFailed)
    }

    fn commit_required(&self, expression: &str) -> Result<GitCommitRef, RepositoryError> {
        let snapshot = self
            .commit_optional(expression)?
            .ok_or(RepositoryError::SnapshotNotFound)?;
        if self.head()?.as_ref() == Some(&snapshot) || self.is_reachable_from_ref(&snapshot)? {
            Ok(snapshot)
        } else {
            Err(RepositoryError::SnapshotNotReachable)
        }
    }

    fn is_reachable_from_ref(&self, snapshot: &GitCommitRef) -> Result<bool, RepositoryError> {
        // A ref need not name a commit: the internal `refs/orna/pins/*`
        // publication pins name trees. Ancestry is only defined between
        // commits, so a ref that cannot peel to a commit is skipped rather than
        // failing the resolution of a snapshot that is otherwise valid
        // (`orna export DEST --at SELECTOR` freezes and verifies the selected
        // snapshot closure).
        let refs = self.git([
            "for-each-ref",
            "--format=%(objecttype)%00%(*objecttype)%00%(refname)",
        ])?;
        for line in refs.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.split('\0');
            let object_type = fields.next().unwrap_or_default();
            let peeled_type = fields.next().unwrap_or_default();
            let Some(reference) = fields.next().filter(|reference| !reference.is_empty()) else {
                continue;
            };
            if object_type != "commit" && peeled_type != "commit" {
                continue;
            }
            let mut command = self.command();
            command.args(["merge-base", "--is-ancestor", snapshot.as_str(), reference]);
            let output = command
                .output()
                .map_err(|_| RepositoryError::GitUnavailable)?;
            if output.status.success() {
                return Ok(true);
            }
            if output.status.code() != Some(1) {
                return Err(RepositoryError::GitOperationFailed);
            }
        }
        Ok(false)
    }

    fn resolve_checkout_target(&self, selector: &str) -> Result<CheckoutTarget, RepositoryError> {
        let branch =
            valid_branch_name(selector) && self.ref_exists(&format!("refs/heads/{selector}"))?;
        let tag =
            valid_branch_name(selector) && self.ref_exists(&format!("refs/tags/{selector}"))?;
        let other_ref = if branch {
            [
                format!("refs/{selector}"),
                format!("refs/remotes/{selector}"),
                format!("refs/remotes/{selector}/HEAD"),
            ]
            .into_iter()
            .try_fold(false, |found, reference| {
                if found || reference == format!("refs/heads/{selector}") {
                    Ok(found)
                } else {
                    self.ref_exists(&reference)
                }
            })?
        } else {
            false
        };
        if branch && (tag || other_ref) {
            return Err(RepositoryError::InvalidSelector);
        }
        // A full object ID and a local branch name are both valid selectors,
        // but their attachment semantics differ. Reject the collision before
        // resolving the target so callers cannot accidentally select a branch
        // merely because Git gave it precedence during revision parsing.
        if branch && self.is_full_commit_object_id(selector)? {
            return Err(RepositoryError::InvalidSelector);
        }
        let commit = self.resolve_snapshot(selector)?;
        if branch {
            Ok(CheckoutTarget::Branch {
                name: selector.to_owned(),
                commit,
            })
        } else {
            Ok(CheckoutTarget::Detached { commit })
        }
    }

    fn is_full_commit_object_id(&self, selector: &str) -> Result<bool, RepositoryError> {
        let length = self.native_object_id_length()?;
        if selector.len() != length || !selector.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(false);
        }
        Ok(self
            .commit_optional(&format!("{selector}^{{commit}}"))?
            .is_some())
    }

    fn ref_exists(&self, reference: &str) -> Result<bool, RepositoryError> {
        let mut command = self.command();
        command
            .args(["show-ref", "--verify", "--quiet", "--"])
            .arg(reference);
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) && output.stderr.is_empty() {
            Ok(false)
        } else {
            Err(RepositoryError::GitOperationFailed)
        }
    }

    fn snapshot_from_native_oid(&self, oid: String) -> Result<GitCommitRef, RepositoryError> {
        let length = self.native_object_id_length()?;
        GitCommitRef::from_verified_commit(oid, length)
    }

    fn index_tree_from_native_oid(&self, oid: String) -> Result<IndexTreeRef, RepositoryError> {
        IndexTreeRef::from_verified_tree(oid, self.native_object_id_length()?)
    }

    pub(crate) fn native_object_id_length(&self) -> Result<usize, RepositoryError> {
        Ok(self.objects.algorithm()?.width() * 2)
    }

    /// The local branch currently attached to `HEAD`, for callers that must
    /// name it as a transport ref. `None` for a detached `HEAD`.
    pub fn checked_out_branch(&self) -> Result<Option<String>, RepositoryError> {
        self.current_branch()
    }

    /// Runs a repository operation while the worktree coordination lock is held.
    ///
    /// The lock is acquired before invoking the callback and released on return,
    /// including when the callback fails or panics.
    pub fn with_coordination_lock<T, E>(
        &self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<RepositoryError>,
    {
        let _lock = self.acquire_coordination_lock().map_err(E::from)?;
        operation()
    }

    fn acquire_coordination_lock(&self) -> Result<CoordinationLock, RepositoryError> {
        let file = self.open_coordination_lock_file()?;
        file.try_lock_exclusive()
            .map_err(|_| RepositoryError::RepositoryBusy)?;
        Ok(CoordinationLock { file })
    }

    /// Acquires the coordination lock for publication, waiting at most
    /// `COORDINATION_LOCK_WAIT`. A publisher that loses a race queues briefly
    /// behind the winner and then fails its compare-and-set (ORNA-PUB-009).
    /// A held or hung lock, or a same-thread re-entry, is reported as
    /// `RepositoryBusy` once the bound elapses; it never blocks indefinitely.
    fn acquire_coordination_lock_for_publication(
        &self,
    ) -> Result<CoordinationLock, RepositoryError> {
        let file = self.open_coordination_lock_file()?;
        let deadline = std::time::Instant::now() + self.coordination_lock_wait;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(CoordinationLock { file }),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    if std::time::Instant::now() >= deadline {
                        return Err(RepositoryError::RepositoryBusy);
                    }
                    std::thread::sleep(COORDINATION_LOCK_POLL);
                }
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
    }

    fn open_coordination_lock_file(&self) -> Result<fs::File, RepositoryError> {
        let locks = self.runtime.locks();
        fs::create_dir_all(&locks).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let path = locks.join("coordination.lock");
        fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }

    /// Acquires the process-lifetime owner lock for this worktree's local
    /// runtime. The lock is separate from short-lived repository coordination
    /// locks and remains held for the runtime lifetime.
    ///
    /// Current and departed identities are written as one atomic state update.
    /// Replacing the current identity therefore cannot erase a prior owner
    /// before manifest validation and guarded reclamation succeed.
    pub fn acquire_runtime_owner_lock(
        &self,
        owner_id: [u8; 16],
    ) -> Result<RuntimeOwnerLock, RepositoryError> {
        if owner_id == [0; 16] {
            return Err(RepositoryError::InvalidInternalRef);
        }
        let locks = self.runtime.locks();
        fs::create_dir_all(&locks).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(locks.join("runtime-owner.lock"))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.try_lock_exclusive()
            .map_err(|_| RepositoryError::RepositoryBusy)?;
        let state_path = locks.join("runtime-owner.state");
        let (current_owner, mut departed_owners) =
            read_runtime_owner_state(&state_path, &mut file)?;
        let previous_owner = (current_owner != [0; 16]).then_some(current_owner);
        if let Some(previous_owner) = previous_owner
            && !departed_owners.contains(&previous_owner)
        {
            departed_owners.push(previous_owner);
        }
        persist_runtime_owner_state(&state_path, owner_id, &departed_owners)?;
        // Migrate the legacy lock-file payload only after the atomic state is
        // durable; the file itself remains the stable advisory-lock inode.
        file.set_len(0)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        file.sync_all()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok(RuntimeOwnerLock {
            file,
            repository: self.clone(),
            previous_owner,
            current_owner: owner_id,
            departed_owners,
            owner_state_path: state_path,
        })
    }

    fn managed_target(&self, path: &ManagedPath) -> Result<PathBuf, RepositoryError> {
        let target = self.worktree.join(path.as_path());
        self.validate_managed_parent(
            target
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )?;
        if let Ok(metadata) = fs::symlink_metadata(&target)
            && metadata.file_type().is_symlink()
        {
            return Err(RepositoryError::UnsafeManagedPath);
        }
        Ok(target)
    }

    fn read_managed_file(&self, target: &Path) -> Result<Option<Vec<u8>>, RepositoryError> {
        match fs::symlink_metadata(target) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                Err(RepositoryError::UnsafeManagedPath)
            }
            Ok(_) => fs::read(target)
                .map(Some)
                .map_err(|_| RepositoryError::LocalStateUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn ensure_managed_contents_match(
        &self,
        path: &ManagedPath,
        contents: &[u8],
    ) -> Result<(), RepositoryError> {
        let target = self.managed_target(path)?;
        if self.read_managed_file(&target)?.as_deref() == Some(contents) {
            Ok(())
        } else {
            Err(RepositoryError::ManagedContentConflict)
        }
    }

    fn managed_index_matches_head(
        &self,
        expected_head: &GitCommitRef,
        path: &ManagedPath,
    ) -> Result<bool, RepositoryError> {
        let mut command = self.command();
        scrub_git_routing_environment(&mut command);
        command
            .args([
                "diff",
                "--cached",
                "--quiet",
                "--exit-code",
                expected_head.as_str(),
                "--",
            ])
            .arg(path.as_path());
        let output = command
            .output()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(RepositoryError::GitOperationFailed),
        }
    }

    fn committed_managed_file_bytes(
        &self,
        expected_head: &GitCommitRef,
        path: &ManagedPath,
    ) -> Result<Option<Vec<u8>>, RepositoryError> {
        let mut command = self.command();
        scrub_git_routing_environment(&mut command);
        command
            .args(["ls-tree", "-z", "--full-tree", expected_head.as_str(), "--"])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        let Some(entry) = output.split(|byte| *byte == 0).next() else {
            return Ok(None);
        };
        if entry.is_empty() {
            return Ok(None);
        }
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(RepositoryError::GitOperationFailed)?;
        let metadata = &entry[..tab];
        let encoded_path = &entry[tab + 1..];
        if encoded_path != path.as_path().as_os_str().as_encoded_bytes() {
            return Err(RepositoryError::GitOperationFailed);
        }
        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let object = String::from_utf8(fields.next().unwrap_or_default().to_vec())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if fields.next().is_some() || kind != b"blob" || !matches!(mode, b"100644" | b"100755") {
            return Err(RepositoryError::ManagedContentConflict);
        }
        GitCommitRef::from_verified_commit(object.clone(), self.native_object_id_length()?)?;

        let mut command = self.command();
        scrub_git_routing_environment(&mut command);
        command.args(["cat-file", "blob", &object]);
        Ok(Some(self.run(command)?.stdout))
    }

    fn validate_managed_parent(&self, parent: &Path) -> Result<(), RepositoryError> {
        let relative = parent
            .strip_prefix(&self.worktree)
            .map_err(|_| RepositoryError::UnsafeManagedPath)?;
        let mut current = self.worktree.clone();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                return Err(RepositoryError::UnsafeManagedPath);
            };
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    return Err(RepositoryError::UnsafeManagedPath);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
        Ok(())
    }

    fn update_managed_index(
        &self,
        expected: &IndexGeneration,
        paths: &[ManagedPath],
        operation: ManagedIndexOperation<'_>,
        mut before_install: Option<&mut dyn FnMut()>,
    ) -> Result<IndexGeneration, RepositoryError> {
        self.ensure_atomic_index_install_supported()?;
        self.ensure_no_git_index_lock()?;
        let index = self.git_path("index")?;
        let base_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let captured = self.index_generation()?;
        let base_after_capture =
            fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if &captured != expected || base_index != base_after_capture {
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual: captured,
            });
        }
        let candidate = index
            .parent()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .join(format!("index-candidate-{}", Uuid::new_v4()));
        write_new_index_candidate(&candidate, &base_index)?;
        let permissions = fs::metadata(&index)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?
            .permissions();
        fs::set_permissions(&candidate, permissions)
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let mut command = self.command();
        command.env("GIT_INDEX_FILE", &candidate);
        match operation {
            ManagedIndexOperation::Add | ManagedIndexOperation::AddContents(_) => {
                command.arg("add");
            }
            ManagedIndexOperation::Unstage => {
                command.args(["restore", "--staged"]);
            }
        }
        command.arg("--");
        for path in paths {
            command.arg(path.as_path());
        }
        if let Err(error) = self.run(command) {
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
        if let ManagedIndexOperation::AddContents(contents) = operation {
            if let Err(error) = self.prepare_index_contents(&candidate, &paths[0], contents) {
                let _ = fs::remove_file(&candidate);
                return Err(error);
            }
        }
        if let Some(hook) = before_install.as_mut() {
            hook();
        }
        fs::File::open(&candidate)
            .and_then(|file| file.sync_all())
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;

        // Git itself serializes normal index writers through this pathname.
        // The candidate is prepared off-index; only an unchanged observed index
        // is atomically replaced while this lock exists.
        let git_lock = match GitIndexLock::acquire(index.with_extension("lock")) {
            Ok(lock) => lock,
            Err(error) => {
                let _ = fs::remove_file(&candidate);
                return Err(error);
            }
        };
        let current_index = fs::read(&index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let current_head = self.head()?;
        if current_index != base_index || current_head != expected.head {
            let _ = fs::remove_file(&candidate);
            drop(git_lock);
            let actual = self.index_generation()?;
            return Err(RepositoryError::StaleIndex {
                expected: expected.clone(),
                actual,
            });
        }
        let contents_match = match operation {
            ManagedIndexOperation::AddContents(contents) => paths
                .first()
                .ok_or(RepositoryError::NoManagedPaths)
                .and_then(|path| self.ensure_managed_contents_match(path, contents)),
            _ => Ok(()),
        };
        if let Err(error) = contents_match {
            let _ = fs::remove_file(&candidate);
            drop(git_lock);
            return Err(error);
        }
        fs::rename(&candidate, &index).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        fs::File::open(
            index
                .parent()
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        drop(git_lock);
        self.index_generation()
    }
    fn index_file_mode(&self, index: &Path, path: &ManagedPath) -> Result<&'static str, RepositoryError> {
        let mut command = self.command();
        command
            .env("GIT_INDEX_FILE", index)
            .args(["--literal-pathspecs", "ls-files", "--stage", "-z", "--"])
            .arg(path.as_path());
        let output = self.run(command)?.stdout;
        let entry = output
            .split(|byte| *byte == 0)
            .find(|entry| !entry.is_empty())
            .ok_or(RepositoryError::GitOperationFailed)?;
        let header = entry
            .split(|byte| *byte == b'\t')
            .next()
            .ok_or(RepositoryError::GitOperationFailed)?;
        let mode = header
            .split(|byte| *byte == b' ')
            .next()
            .ok_or(RepositoryError::GitOperationFailed)?;
        match mode {
            b"100644" => Ok("100644"),
            b"100755" => Ok("100755"),
            _ => Err(RepositoryError::UnsafeManagedPath),
        }
    }

    fn prepare_index_contents(
        &self,
        index: &Path,
        path: &ManagedPath,
        contents: &[u8],
    ) -> Result<(), RepositoryError> {
        let mode = self.index_file_mode(index, path)?;
        let object = self.hash_object_at_path(contents, path)?;
        let path = path
            .as_path()
            .to_str()
            .ok_or(RepositoryError::UnsafeManagedPath)?;
        let cacheinfo = format!("{mode},{object},{path}");
        let mut command = self.command();
        command
            .env("GIT_INDEX_FILE", index)
            .args(["--literal-pathspecs", "update-index", "--add", "--cacheinfo"])
            .arg(cacheinfo);
        self.run(command)?;
        Ok(())
    }

    fn git_path(&self, name: &str) -> Result<PathBuf, RepositoryError> {
        let path = PathBuf::from(self.git(["rev-parse", "--git-path", name])?);
        Ok(if path.is_absolute() {
            path
        } else {
            self.worktree.join(path)
        })
    }
    fn ensure_no_git_operation_in_progress(&self) -> Result<(), RepositoryError> {
        for marker in ["MERGE_HEAD", "rebase-merge", "rebase-apply"] {
            let path = self.git_path(marker)?;
            match fs::symlink_metadata(path) {
                Ok(_) => return Err(RepositoryError::RepositoryBusy),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            }
        }
        Ok(())
    }

    fn ensure_no_git_index_lock(&self) -> Result<(), RepositoryError> {
        if self.git_path("index")?.with_extension("lock").exists() {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    fn ensure_atomic_index_install_supported(&self) -> Result<(), RepositoryError> {
        Err(RepositoryError::PlatformUnsupported)
    }

    #[cfg(not(windows))]
    fn ensure_atomic_index_install_supported(&self) -> Result<(), RepositoryError> {
        Ok(())
    }

    #[cfg(windows)]
    fn ensure_atomic_worktree_install_supported(&self) -> Result<(), RepositoryError> {
        Err(RepositoryError::PlatformUnsupported)
    }

    #[cfg(not(windows))]
    fn ensure_atomic_worktree_install_supported(&self) -> Result<(), RepositoryError> {
        Ok(())
    }

    fn update_ref_transaction(
        &self,
        head: &GitCommitRef,
        reference: &str,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        command
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let input = format!(
            "start\nverify HEAD {}\ncreate {reference} {}\nprepare\ncommit\n",
            head.as_str(),
            head.as_str()
        );
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(input.as_bytes())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if status.success() {
            Ok(())
        } else {
            Err(RepositoryError::StaleHead)
        }
    }

    fn advance_ref_transaction(
        &self,
        reference: &str,
        old: &GitCommitRef,
        new: &GitCommitRef,
    ) -> Result<(), RepositoryError> {
        let mut command = self.command();
        command
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| RepositoryError::GitUnavailable)?;
        let input = format!(
            "start\nupdate {reference} {} {}\nprepare\ncommit\n",
            new.as_str(),
            old.as_str()
        );
        child
            .stdin
            .take()
            .ok_or(RepositoryError::GitOperationFailed)?
            .write_all(input.as_bytes())
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        let status = child
            .wait()
            .map_err(|_| RepositoryError::GitOperationFailed)?;
        if status.success() {
            Ok(())
        } else {
            Err(RepositoryError::StaleHead)
        }
    }
}

#[derive(Clone, Copy)]
enum ManagedIndexOperation<'contents> {
    Add,
    AddContents(&'contents [u8]),
    Unstage,
}

struct CoordinationLock {
    file: fs::File,
}

impl Drop for CoordinationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

const RUNTIME_OWNER_STATE_MAGIC: &[u8; 8] = b"ORNAOWN2";
const RUNTIME_OWNER_CHAIN_MAX_ENTRIES: usize = 65_536;

fn read_runtime_owner_state(
    path: &Path,
    legacy_lock: &mut fs::File,
) -> Result<([u8; 16], Vec<[u8; 16]>), RepositoryError> {
    let bytes = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::read(path).map_err(|_| RepositoryError::LocalStateUnavailable)?
        }
        Ok(_) => return Err(RepositoryError::LocalStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            legacy_lock
                .seek(SeekFrom::Start(0))
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let mut legacy = Vec::new();
            (&*legacy_lock)
                .take(17)
                .read_to_end(&mut legacy)
                .map_err(|_| RepositoryError::LocalStateUnavailable)?;
            return match legacy.as_slice() {
                bytes if bytes.len() == 16 => Ok((
                    bytes
                        .try_into()
                        .map_err(|_| RepositoryError::LocalStateUnavailable)?,
                    Vec::new(),
                )),
                [] => Ok(([0; 16], Vec::new())),
                _ => Err(RepositoryError::LocalStateUnavailable),
            };
        }
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    };
    if bytes.len() < 28 || &bytes[..8] != RUNTIME_OWNER_STATE_MAGIC {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    let current: [u8; 16] = bytes[8..24]
        .try_into()
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let count = u32::from_le_bytes(
        bytes[24..28]
            .try_into()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?,
    ) as usize;
    let expected_length = 28usize
        .checked_add(
            count
                .checked_mul(16)
                .ok_or(RepositoryError::LocalStateUnavailable)?,
        )
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    if current == [0; 16]
        || count > RUNTIME_OWNER_CHAIN_MAX_ENTRIES
        || bytes.len() != expected_length
    {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    let mut seen = BTreeSet::new();
    let mut departed = Vec::with_capacity(count);
    for chunk in bytes[28..].chunks_exact(16) {
        let owner: [u8; 16] = chunk
            .try_into()
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        if owner == [0; 16] || !seen.insert(owner) {
            return Err(RepositoryError::LocalStateUnavailable);
        }
        departed.push(owner);
    }
    Ok((current, departed))
}

fn persist_runtime_owner_state(
    path: &Path,
    current: [u8; 16],
    departed: &[[u8; 16]],
) -> Result<(), RepositoryError> {
    if current == [0; 16] || departed.len() > RUNTIME_OWNER_CHAIN_MAX_ENTRIES {
        return Err(RepositoryError::LocalStateUnavailable);
    }
    let count =
        u32::try_from(departed.len()).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    let mut bytes = Vec::with_capacity(28 + departed.len() * 16);
    bytes.extend_from_slice(RUNTIME_OWNER_STATE_MAGIC);
    bytes.extend_from_slice(&current);
    bytes.extend_from_slice(&count.to_le_bytes());
    let mut seen = BTreeSet::new();
    for owner in departed {
        if *owner == [0; 16] || !seen.insert(*owner) {
            return Err(RepositoryError::LocalStateUnavailable);
        }
        bytes.extend_from_slice(owner);
    }
    let parent = path
        .parent()
        .ok_or(RepositoryError::LocalStateUnavailable)?;
    let temporary = parent.join("runtime-owner-state.next");
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.file_type().is_file() => {
            fs::remove_file(&temporary).map_err(|_| RepositoryError::LocalStateUnavailable)?;
        }
        Ok(_) => return Err(RepositoryError::LocalStateUnavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(RepositoryError::LocalStateUnavailable),
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    fs::rename(&temporary, path).map_err(|_| RepositoryError::LocalStateUnavailable)?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RepositoryError::LocalStateUnavailable)
}

/// Writes a brand-new index candidate. `create_new` refuses any existing file
/// or symlink at the path, so a candidate is never written through a link or
/// over an attacker-prepared name.
fn write_new_index_candidate(path: &Path, bytes: &[u8]) -> Result<(), RepositoryError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
    file.write_all(bytes)
        .map_err(|_| RepositoryError::LocalStateUnavailable)
}

/// A process-lifetime local runtime owner lock.
pub struct RuntimeOwnerLock {
    file: fs::File,
    repository: Repository,
    previous_owner: Option<[u8; 16]>,
    current_owner: [u8; 16],
    departed_owners: Vec<[u8; 16]>,
    owner_state_path: PathBuf,
}

impl RuntimeOwnerLock {
    /// Returns the owner identity recorded by the previous holder, if any.
    pub fn previous_owner(&self) -> Option<[u8; 16]> {
        self.previous_owner
    }

    /// Returns the persisted departed owners awaiting manifest-backed cleanup.
    pub fn departed_owners(&self) -> &[[u8; 16]] {
        &self.departed_owners
    }

    /// Reconciles every departed owner's refs against a fully validated pin
    /// manifest grouped by owner. Returns `None` while a publication journal
    /// remains authoritative; the durable chain is retained for retry.
    pub fn reclaim_departed_owner_pins(
        &self,
        accepted_by_owner: &BTreeMap<[u8; 16], BTreeMap<[u8; 16], native_graph::NativeOid>>,
    ) -> Result<Option<usize>, RepositoryError> {
        let empty = BTreeMap::new();
        let departed = self
            .departed_owners
            .iter()
            .map(|owner| (*owner, accepted_by_owner.get(owner).unwrap_or(&empty)))
            .collect::<Vec<_>>();
        let reclaimed =
            match native_graph::reclaim_departed_owner_pin_refs(&self.repository, &departed) {
                Ok(count) => count,
                Err(native_graph::GraphError::PublicationNotTerminal) => return Ok(None),
                Err(_) => return Err(RepositoryError::LocalStateUnavailable),
            };
        if !self.departed_owners.is_empty() {
            persist_runtime_owner_state(&self.owner_state_path, self.current_owner, &[])?;
        }
        Ok(Some(reclaimed))
    }

    /// Reconciles the previous owner's pins against an already-validated
    /// accepted-content manifest. Retained entries map durable pin IDs to
    /// their expected Blob descriptor OIDs. Call only after runtime recovery
    /// has verified that manifest; an in-flight publication journal defers
    /// cleanup because it remains authoritative for candidate roots.
    pub fn reclaim_previous_owner_pins(
        &self,
        accepted_pending: &std::collections::BTreeMap<[u8; 16], native_graph::NativeOid>,
    ) -> Result<usize, RepositoryError> {
        let Some(dead_owner) = self.previous_owner else {
            return Ok(0);
        };
        if self.repository.read_publication_journal()?.is_some() {
            return Err(RepositoryError::PublicationPending);
        }
        native_graph::reclaim_dead_owner_pin_refs(&self.repository, dead_owner, accepted_pending)
            .map_err(|_| RepositoryError::LocalStateUnavailable)
    }
}

impl Drop for RuntimeOwnerLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct GitIndexLock {
    path: PathBuf,
    marker: Vec<u8>,
    _file: fs::File,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Eq, PartialEq)]
enum GitIndexLockReclaimPoint {
    BeforeTransfer,
    AfterTransfer,
    BeforePrivateClaimDisposal,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Eq, PartialEq)]
struct GitIndexLockFile {
    bytes: Vec<u8>,
    metadata: GitIndexLockFileMetadata,
}

#[cfg(target_os = "linux")]
impl GitIndexLockFile {
    fn matches_after_transfer(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.metadata.matches_after_rename(other.metadata)
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Eq, PartialEq)]
struct GitIndexLockFileMetadata {
    device: u64,
    inode: u64,
    mode: u32,
    size: i64,
    modified_seconds: i64,
    modified_nanoseconds: u64,
    changed_seconds: i64,
    changed_nanoseconds: u64,
}

#[cfg(target_os = "linux")]
impl GitIndexLockFileMetadata {
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
            mode: stat.st_mode,
            size: stat.st_size,
            modified_seconds: stat.mtime(),
            modified_nanoseconds: stat.st_mtime_nsec,
            changed_seconds: stat.ctime(),
            changed_nanoseconds: stat.st_ctime_nsec,
        }
    }

    fn is_regular(stat: &rustix::fs::Stat) -> bool {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile
    }

    // rename(2) updates ctime, so provenance across the transfer excludes it.
    fn matches_after_rename(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
    }
}

#[cfg(target_os = "linux")]
struct GitIndexLockDirectory {
    fd: rustix::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl GitIndexLockDirectory {
    fn open(path: &Path) -> Result<(Self, OsString), RepositoryError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or(RepositoryError::LocalStateUnavailable)?
            .to_os_string();
        let fd = rustix::fs::openat(
            rustix::fs::CWD,
            parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        Ok((Self { fd }, name))
    }

    fn name_exists(&self, name: &OsStr) -> Result<bool, RepositoryError> {
        match rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn metadata_matches(
        &self,
        name: &OsStr,
        expected: GitIndexLockFileMetadata,
    ) -> Result<bool, RepositoryError> {
        match rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(GitIndexLockFileMetadata::is_regular(&stat)
                && GitIndexLockFileMetadata::from_stat(&stat) == expected),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(RepositoryError::LocalStateUnavailable),
        }
    }

    fn read_regular(&self, name: &OsStr) -> Result<GitIndexLockFile, RepositoryError> {
        let before = rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        if !GitIndexLockFileMetadata::is_regular(&before) {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let opened = rustix::fs::fstat(&fd).map_err(|_| RepositoryError::GitIndexLockPresent)?;
        if !GitIndexLockFileMetadata::is_regular(&opened)
            || GitIndexLockFileMetadata::from_stat(&before)
                != GitIndexLockFileMetadata::from_stat(&opened)
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let mut file = fs::File::from(fd);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let after_open =
            rustix::fs::fstat(&file).map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let after_name = rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let metadata = GitIndexLockFileMetadata::from_stat(&opened);
        if !GitIndexLockFileMetadata::is_regular(&after_open)
            || !GitIndexLockFileMetadata::is_regular(&after_name)
            || GitIndexLockFileMetadata::from_stat(&after_open) != metadata
            || GitIndexLockFileMetadata::from_stat(&after_name) != metadata
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        Ok(GitIndexLockFile { bytes, metadata })
    }

    fn abandoned_marker(
        &self,
        name: &OsStr,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<GitIndexLockFile, RepositoryError> {
        let file = self.read_regular(name)?;
        let Some(marker) = GitIndexLockMarker::decode(&file.bytes) else {
            return Err(RepositoryError::GitIndexLockPresent);
        };
        if !GitIndexLockMarker::binding_permits_reclaim(marker.journal_binding, journal_binding)
            || marker.owner_liveness() != GitIndexLockOwnerLiveness::ProvenDead
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        Ok(file)
    }

    fn rename_no_replace(&self, from: &OsStr, to: &OsStr) -> Result<(), RepositoryError> {
        rustix::fs::renameat_with(
            &self.fd,
            from,
            &self.fd,
            to,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| RepositoryError::GitIndexLockPresent)
    }

    fn unlink(&self, name: &OsStr) -> Result<(), RepositoryError> {
        rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())
            .map_err(|_| RepositoryError::GitIndexLockPresent)
    }

    fn private_entries(&self, canonical: &OsStr) -> Result<Vec<OsString>, RepositoryError> {
        rustix::fs::seek(&self.fd, rustix::fs::SeekFrom::Start(0))
            .map_err(|_| RepositoryError::LocalStateUnavailable)?;
        let prefix = GitIndexLock::private_prefix(canonical);
        let mut entries = Vec::new();
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut directory = rustix::fs::RawDir::new(&self.fd, &mut buffer);
        while let Some(entry) = directory.next() {
            let entry = entry.map_err(|_| RepositoryError::LocalStateUnavailable)?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string();
            if name.as_bytes().starts_with(prefix.as_bytes()) {
                entries.push(name);
            }
        }
        Ok(entries)
    }
}

impl GitIndexLock {
    fn acquire(path: PathBuf) -> Result<Self, RepositoryError> {
        Self::acquire_with_binding(path, [0; 32])
    }

    fn acquire_owned(path: PathBuf, journal_binding: [u8; 32]) -> Result<Self, RepositoryError> {
        Self::acquire_with_binding(path, journal_binding)
    }

    fn acquire_with_binding(
        path: PathBuf,
        journal_binding: [u8; 32],
    ) -> Result<Self, RepositoryError> {
        let marker = GitIndexLockMarker::new(journal_binding)?.encode();
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        let mut file = options.open(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                // Never remove an existing Git lock: it may belong to a live
                // ordinary Git writer or require Git's own recovery.
                RepositoryError::GitIndexLockPresent
            } else {
                RepositoryError::LocalStateUnavailable
            }
        })?;
        if file
            .write_all(&marker)
            .and_then(|_| file.sync_all())
            .is_err()
        {
            let _ = fs::remove_file(&path);
            return Err(RepositoryError::LocalStateUnavailable);
        }
        Ok(Self {
            path,
            marker,
            _file: file,
        })
    }

    fn reclaim_abandoned(path: PathBuf, journal_binding: [u8; 32]) -> Result<(), RepositoryError> {
        Self::reclaim_bound(path, Some(journal_binding))
    }

    /// Reclaims an Orna-marked lock whose publisher stopped before writing its
    /// journal, so no journal binding exists to check. The marker must carry a
    /// non-zero publisher binding and the owner must be proven dead.
    fn reclaim_unbound_orphan(path: PathBuf) -> Result<(), RepositoryError> {
        Self::reclaim_bound(path, None)
    }

    fn reclaim_bound(path: PathBuf, journal_binding: Option<[u8; 32]>) -> Result<(), RepositoryError> {
        #[cfg(target_os = "linux")]
        {
            Self::reclaim_abandoned_linux(path, journal_binding, |_, _| {})
        }

        #[cfg(not(target_os = "linux"))]
        Self::reclaim_abandoned_unavailable(path, journal_binding)
    }

    #[cfg(all(test, target_os = "linux"))]
    fn reclaim_abandoned_with_hook(
        path: PathBuf,
        journal_binding: [u8; 32],
        hook: impl FnMut(GitIndexLockReclaimPoint, &Path),
    ) -> Result<(), RepositoryError> {
        Self::reclaim_abandoned_linux(path, Some(journal_binding), hook)
    }

    #[cfg(target_os = "linux")]
    fn reclaim_abandoned_linux(
        path: PathBuf,
        journal_binding: Option<[u8; 32]>,
        mut hook: impl FnMut(GitIndexLockReclaimPoint, &Path),
    ) -> Result<(), RepositoryError> {
        let (directory, canonical) = GitIndexLockDirectory::open(&path)?;
        let private_entries = directory.private_entries(&canonical)?;
        if private_entries.len() > 1 {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        if let Some(private_entry) = private_entries.first() {
            if directory.name_exists(&canonical)? {
                return Err(RepositoryError::GitIndexLockPresent);
            }
            return Self::recover_private_entry(
                &directory,
                &canonical,
                private_entry,
                journal_binding,
                &path,
            );
        }
        if !directory.name_exists(&canonical)? {
            return Ok(());
        }

        let original = directory.abandoned_marker(&canonical, journal_binding)?;
        let claim = Self::claim_name(&canonical, &original.bytes);
        hook(GitIndexLockReclaimPoint::BeforeTransfer, &path);
        directory.rename_no_replace(&canonical, &claim)?;
        hook(GitIndexLockReclaimPoint::AfterTransfer, &path);

        let transferred = match directory.abandoned_marker(&claim, journal_binding) {
            Ok(transferred) => transferred,
            Err(_) => {
                let transferred = match directory.read_regular(&claim) {
                    Ok(transferred) => transferred,
                    Err(_) => return Err(RepositoryError::GitIndexLockPresent),
                };
                return Self::restore_claim(&directory, &claim, &canonical, &transferred);
            }
        };
        if !original.matches_after_transfer(&transferred) {
            return Self::restore_claim(&directory, &claim, &canonical, &transferred);
        }

        let claim_path = path.with_file_name(&claim);
        Self::dispose_private_claim(
            &directory,
            &canonical,
            &claim,
            &transferred,
            journal_binding,
            &mut hook,
            &claim_path,
        )?;
        if directory.name_exists(&canonical)? {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn recover_private_entry(
        directory: &GitIndexLockDirectory,
        canonical: &OsStr,
        private_entry: &OsStr,
        journal_binding: Option<[u8; 32]>,
        path: &Path,
    ) -> Result<(), RepositoryError> {
        let file = directory.abandoned_marker(private_entry, journal_binding)?;
        let claim = Self::claim_name(canonical, &file.bytes);
        let disposal = Self::disposal_name(canonical, &file.bytes);
        if private_entry == claim {
            let claim_path = path.with_file_name(&claim);
            return Self::dispose_private_claim(
                directory,
                canonical,
                &claim,
                &file,
                journal_binding,
                &mut |_, _| {},
                &claim_path,
            );
        }
        if private_entry == disposal {
            return Self::dispose_tombstone(
                directory,
                canonical,
                &claim,
                &disposal,
                &file,
                journal_binding,
            );
        }
        Err(RepositoryError::GitIndexLockPresent)
    }

    #[cfg(target_os = "linux")]
    fn dispose_private_claim(
        directory: &GitIndexLockDirectory,
        canonical: &OsStr,
        claim: &OsStr,
        expected: &GitIndexLockFile,
        journal_binding: Option<[u8; 32]>,
        hook: &mut impl FnMut(GitIndexLockReclaimPoint, &Path),
        claim_path: &Path,
    ) -> Result<(), RepositoryError> {
        hook(
            GitIndexLockReclaimPoint::BeforePrivateClaimDisposal,
            claim_path,
        );
        let current = directory.abandoned_marker(claim, journal_binding)?;
        if current != *expected
            || directory.private_entries(canonical)?.as_slice() != [claim.to_os_string()]
            || !directory.metadata_matches(claim, current.metadata)?
        {
            return Err(RepositoryError::GitIndexLockPresent);
        }

        let disposal = Self::disposal_name(canonical, &current.bytes);
        directory.rename_no_replace(claim, &disposal)?;
        let transferred = match directory.abandoned_marker(&disposal, journal_binding) {
            Ok(transferred) => transferred,
            Err(_) => {
                let transferred = match directory.read_regular(&disposal) {
                    Ok(transferred) => transferred,
                    Err(_) => return Err(RepositoryError::GitIndexLockPresent),
                };
                return Self::restore_claim(directory, &disposal, claim, &transferred);
            }
        };
        if !current.matches_after_transfer(&transferred) {
            return Self::restore_claim(directory, &disposal, claim, &transferred);
        }
        Self::dispose_tombstone(
            directory,
            canonical,
            claim,
            &disposal,
            &transferred,
            journal_binding,
        )
    }

    #[cfg(target_os = "linux")]
    fn dispose_tombstone(
        directory: &GitIndexLockDirectory,
        canonical: &OsStr,
        claim: &OsStr,
        disposal: &OsStr,
        expected: &GitIndexLockFile,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<(), RepositoryError> {
        let current = directory.abandoned_marker(disposal, journal_binding)?;
        if current != *expected || !directory.metadata_matches(disposal, current.metadata)? {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let external_before = directory.name_exists(canonical)? || directory.name_exists(claim)?;
        let current = directory.abandoned_marker(disposal, journal_binding)?;
        if current != *expected {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        // Only the private tombstone is unlinked after revalidation. A new
        // canonical or claim name is retained and reported as a conflict.
        directory.unlink(disposal)?;
        let external_after = directory.name_exists(canonical)? || directory.name_exists(claim)?;
        if external_before || external_after || !directory.private_entries(canonical)?.is_empty() {
            Err(RepositoryError::GitIndexLockPresent)
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn restore_claim(
        directory: &GitIndexLockDirectory,
        claim: &OsStr,
        canonical: &OsStr,
        expected: &GitIndexLockFile,
    ) -> Result<(), RepositoryError> {
        let current = match directory.read_regular(claim) {
            Ok(current) => current,
            Err(_) => return Err(RepositoryError::GitIndexLockPresent),
        };
        if current != *expected || !directory.metadata_matches(claim, current.metadata)? {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let _ = directory.rename_no_replace(claim, canonical);
        Err(RepositoryError::GitIndexLockPresent)
    }

    #[cfg(target_os = "linux")]
    fn private_prefix(canonical: &OsStr) -> OsString {
        let mut prefix = OsString::from(".orna-reclaim-");
        prefix.push(Self::hex_digest(canonical.as_bytes()));
        prefix.push("-");
        prefix
    }

    #[cfg(target_os = "linux")]
    fn claim_name(canonical: &OsStr, bytes: &[u8]) -> OsString {
        let mut claim = Self::private_prefix(canonical);
        claim.push(Self::hex_digest(bytes));
        claim
    }

    #[cfg(target_os = "linux")]
    fn hex_digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn disposal_name(canonical: &OsStr, bytes: &[u8]) -> OsString {
        let mut disposal = Self::claim_name(canonical, bytes);
        disposal.push(".dispose");
        disposal
    }

    #[cfg(all(test, target_os = "linux"))]
    fn claim_path(path: &Path, bytes: &[u8]) -> Result<PathBuf, RepositoryError> {
        let name = path
            .file_name()
            .ok_or(RepositoryError::LocalStateUnavailable)?;
        Ok(path.with_file_name(Self::claim_name(name, bytes)))
    }

    #[cfg(not(target_os = "linux"))]
    fn reclaim_abandoned_unavailable(
        path: PathBuf,
        journal_binding: Option<[u8; 32]>,
    ) -> Result<(), RepositoryError> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(RepositoryError::LocalStateUnavailable),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        let bytes = fs::read(&path).map_err(|_| RepositoryError::GitIndexLockPresent)?;
        let Some(marker) = GitIndexLockMarker::decode(&bytes) else {
            return Err(RepositoryError::GitIndexLockPresent);
        };
        if !GitIndexLockMarker::binding_permits_reclaim(marker.journal_binding, journal_binding) {
            return Err(RepositoryError::GitIndexLockPresent);
        }
        Err(RepositoryError::GitIndexLockPresent)
    }
}
impl Drop for GitIndexLock {
    fn drop(&mut self) {
        if fs::read(&self.path).ok().as_deref() == Some(self.marker.as_slice()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy)]
struct GitIndexLockMarker {
    pid: u32,
    process_start: u64,
    nonce: [u8; 16],
    journal_binding: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitIndexLockOwnerLiveness {
    ProvenDead,
    Live,
    Unknown,
}

#[cfg(target_os = "linux")]
enum ProcessStartObservation {
    Observed(u64),
    Missing,
    Unknown,
}

#[cfg(target_os = "linux")]
fn process_owner_liveness(
    expected_start: u64,
    observation: ProcessStartObservation,
) -> GitIndexLockOwnerLiveness {
    match observation {
        ProcessStartObservation::Observed(process_start) if process_start == expected_start => {
            GitIndexLockOwnerLiveness::Live
        }
        ProcessStartObservation::Observed(_) | ProcessStartObservation::Missing => {
            GitIndexLockOwnerLiveness::ProvenDead
        }
        ProcessStartObservation::Unknown => GitIndexLockOwnerLiveness::Unknown,
    }
}

impl GitIndexLockMarker {
    /// Journal-bound reclaim requires the exact binding. Unbound reclaim requires
    /// a publisher binding; the all-zero binding marks ordinary Orna writers,
    /// whose locks publication recovery never reclaims.
    fn binding_permits_reclaim(marker_binding: [u8; 32], journal_binding: Option<[u8; 32]>) -> bool {
        match journal_binding {
            Some(expected) => marker_binding == expected,
            None => marker_binding != [0; 32],
        }
    }

    fn new(journal_binding: [u8; 32]) -> Result<Self, RepositoryError> {
        let pid = std::process::id();
        #[cfg(target_os = "linux")]
        let process_start = match process_start_time(pid) {
            ProcessStartObservation::Observed(process_start) => process_start,
            ProcessStartObservation::Missing | ProcessStartObservation::Unknown => {
                return Err(RepositoryError::LocalStateUnavailable);
            }
        };
        #[cfg(not(target_os = "linux"))]
        let process_start = 0;
        Ok(Self {
            pid,
            process_start,
            nonce: Uuid::new_v4().into_bytes(),
            journal_binding,
        })
    }

    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(GIT_INDEX_LOCK_MAGIC.len() + 1 + 4 + 8 + 16 + 32);
        bytes.extend_from_slice(GIT_INDEX_LOCK_MAGIC);
        bytes.push(1);
        bytes.extend_from_slice(&self.pid.to_be_bytes());
        bytes.extend_from_slice(&self.process_start.to_be_bytes());
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.journal_binding);
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut cursor = GIT_INDEX_LOCK_MAGIC.len();
        if !bytes.starts_with(GIT_INDEX_LOCK_MAGIC) || bytes.get(cursor)? != &1 {
            return None;
        }
        cursor += 1;
        let pid = u32::from_be_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?);
        cursor += 4;
        let process_start = u64::from_be_bytes(bytes.get(cursor..cursor + 8)?.try_into().ok()?);
        cursor += 8;
        let nonce = bytes.get(cursor..cursor + 16)?.try_into().ok()?;
        cursor += 16;
        let journal_binding = bytes.get(cursor..cursor + 32)?.try_into().ok()?;
        cursor += 32;
        (pid != 0 && cursor == bytes.len()).then_some(Self {
            pid,
            process_start,
            nonce,
            journal_binding,
        })
    }

    fn owner_liveness(self) -> GitIndexLockOwnerLiveness {
        #[cfg(target_os = "linux")]
        {
            process_owner_liveness(self.process_start, process_start_time(self.pid))
        }
        #[cfg(not(target_os = "linux"))]
        {
            GitIndexLockOwnerLiveness::Unknown
        }
    }
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> ProcessStartObservation {
    // A missing target PID proves death only after a readable, parseable procfs
    // record for this process confirms that procfs itself is available.
    if fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|contents| parse_process_start_time(&contents))
        .is_none()
    {
        return ProcessStartObservation::Unknown;
    }
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(contents) => parse_process_start_time(&contents)
            .map(ProcessStartObservation::Observed)
            .unwrap_or(ProcessStartObservation::Unknown),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ProcessStartObservation::Missing
        }
        Err(_) => ProcessStartObservation::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn parse_process_start_time(contents: &str) -> Option<u64> {
    let fields = contents
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    fields.get(19)?.parse().ok()
}

fn trim_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

fn put_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), RepositoryError> {
    let value = u32::try_from(value).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), RepositoryError> {
    put_u32(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_optional_string(bytes: &mut Vec<u8>, value: Option<&str>) -> Result<(), RepositoryError> {
    match value {
        Some(value) => put_string(bytes, value),
        None => {
            bytes.extend_from_slice(&u32::MAX.to_le_bytes());
            Ok(())
        }
    }
}

fn put_optional_bytes(bytes: &mut Vec<u8>, value: Option<&[u8]>) -> Result<(), RepositoryError> {
    match value {
        Some(value) => {
            let length = u32::try_from(value.len())
                .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(value);
        }
        None => bytes.extend_from_slice(&u32::MAX.to_le_bytes()),
    }
    Ok(())
}

fn take_byte(bytes: &[u8], cursor: &mut usize) -> Result<u8, RepositoryError> {
    let value = *bytes
        .get(*cursor)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor += 1;
    Ok(value)
}

fn take_fixed_array<const N: usize>(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<[u8; N], RepositoryError> {
    let end = cursor
        .checked_add(N)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    value
        .try_into()
        .map_err(|_| RepositoryError::InvalidPublicationJournal)
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, RepositoryError> {
    let end = cursor
        .checked_add(4)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?,
    ))
}

fn checkout_journal_optional_string(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<String>, RepositoryError> {
    take_optional_string(bytes, cursor).map_err(|_| RepositoryError::InvalidCheckoutJournal)
}

fn checkout_journal_optional_bytes(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    take_optional_bytes(bytes, cursor).map_err(|_| RepositoryError::InvalidCheckoutJournal)
}

fn encode_checkout_cwd(bytes: &mut Vec<u8>, cwd: &CwdGeneration) -> Result<(), RepositoryError> {
    put_optional_string(bytes, cwd.head.as_ref().map(GitCommitRef::as_str))
        .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
    put_optional_string(bytes, cwd.branch.as_deref())
        .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
    put_optional_string(bytes, cwd.index.head.as_ref().map(GitCommitRef::as_str))
        .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
    put_optional_string(bytes, cwd.index.tree.as_ref().map(IndexTreeRef::as_str))
        .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
    put_optional_bytes(bytes, Some(cwd.worktree.as_porcelain_v2_z()))
        .map_err(|_| RepositoryError::InvalidCheckoutJournal)?;
    bytes.extend_from_slice(cwd.worktree.content_digest());
    bytes.extend_from_slice(&cwd.runtime.get().to_be_bytes());
    Ok(())
}

fn encode_optional_checkout_cwd(
    bytes: &mut Vec<u8>,
    cwd: Option<&CwdGeneration>,
) -> Result<(), RepositoryError> {
    match cwd {
        Some(cwd) => {
            bytes.push(1);
            encode_checkout_cwd(bytes, cwd)
        }
        None => {
            bytes.push(0);
            Ok(())
        }
    }
}

fn decode_checkout_cwd(
    bytes: &[u8],
    cursor: &mut usize,
    object_id_length: usize,
    has_content_digest: bool,
) -> Result<CwdGeneration, RepositoryError> {
    let head = checkout_journal_optional_string(bytes, cursor)?
        .map(|value| GitCommitRef::from_verified_commit(value, object_id_length))
        .transpose()?;
    let branch = checkout_journal_optional_string(bytes, cursor)?;
    if branch
        .as_deref()
        .is_some_and(|branch| !valid_branch_name(branch))
    {
        return Err(RepositoryError::InvalidCheckoutJournal);
    }
    let index_head = checkout_journal_optional_string(bytes, cursor)?
        .map(|value| GitCommitRef::from_verified_commit(value, object_id_length))
        .transpose()?;
    let index_tree = checkout_journal_optional_string(bytes, cursor)?
        .map(|value| IndexTreeRef::from_verified_tree(value, object_id_length))
        .transpose()?;
    let worktree = checkout_journal_optional_bytes(bytes, cursor)?
        .ok_or(RepositoryError::InvalidCheckoutJournal)?;
    let content_digest = if has_content_digest {
        take_fixed_array::<32>(bytes, cursor)?
    } else {
        [0; 32]
    };
    let runtime = RuntimeGeneration::new(u64::from_be_bytes(take_fixed_array::<8>(bytes, cursor)?));
    if head != index_head {
        return Err(RepositoryError::InvalidCheckoutJournal);
    }
    Ok(CwdGeneration {
        head,
        branch,
        index: IndexGeneration {
            head: index_head,
            tree: index_tree,
        },
        worktree: WorktreeState(worktree, content_digest),
        runtime,
    })
}

fn decode_optional_checkout_cwd(
    bytes: &[u8],
    cursor: &mut usize,
    object_id_length: usize,
    has_content_digest: bool,
) -> Result<Option<CwdGeneration>, RepositoryError> {
    match take_byte(bytes, cursor)? {
        0 => Ok(None),
        1 => decode_checkout_cwd(bytes, cursor, object_id_length, has_content_digest).map(Some),
        _ => Err(RepositoryError::InvalidCheckoutJournal),
    }
}

fn take_string(bytes: &[u8], cursor: &mut usize) -> Result<String, RepositoryError> {
    let length = usize::try_from(take_u32(bytes, cursor)?)
        .map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    String::from_utf8(value.to_vec()).map_err(|_| RepositoryError::InvalidPublicationJournal)
}

fn take_optional_string(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<String>, RepositoryError> {
    let length = take_u32(bytes, cursor)?;
    if length == u32::MAX {
        return Ok(None);
    }
    let length = usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    if length > MAX_JOURNAL_BYTES {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    *cursor = end;
    Ok(Some(
        String::from_utf8(value.to_vec())
            .map_err(|_| RepositoryError::InvalidPublicationJournal)?,
    ))
}

fn take_optional_bytes(
    bytes: &[u8],
    cursor: &mut usize,
) -> Result<Option<Vec<u8>>, RepositoryError> {
    let length = take_u32(bytes, cursor)?;
    if length == u32::MAX {
        return Ok(None);
    }
    let length = usize::try_from(length).map_err(|_| RepositoryError::InvalidPublicationJournal)?;
    if length > MAX_JOURNAL_BYTES {
        return Err(RepositoryError::InvalidPublicationJournal);
    }
    let end = cursor
        .checked_add(length)
        .ok_or(RepositoryError::InvalidPublicationJournal)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(RepositoryError::InvalidPublicationJournal)?
        .to_vec();
    *cursor = end;
    Ok(Some(value))
}
fn run_command(mut command: Command) -> Result<Output, RepositoryError> {
    let output = command
        .output()
        .map_err(|_| RepositoryError::GitUnavailable)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(RepositoryError::GitOperationFailed)
    }
}

/// Stops Git's repository discovery at the parent of a repository root so a
/// corrupt root `.git` cannot fall through to an enclosing repository. Only a
/// directory that carries its own `.git` entry is a root; a plain subdirectory
/// of a worktree (for example `.orna/` or `nested/`) must still discover the
/// enclosing repository, so it is left unbound. The path is canonicalized first
/// so a symlinked root binds to the directory it really resolves to; a relative
/// path cannot yield an empty or relative ceiling.
pub(crate) fn bind_git_discovery_to_worktree(command: &mut Command, worktree: &std::path::Path) {
    let resolved = canonical_or_absolute(worktree);
    if resolved.join(".git").exists() {
        bind_git_ceiling_above(command, &resolved);
    }
}

/// Stops Git's discovery at the parent of `directory` unconditionally. Use this
/// where the caller requires `directory` itself to be a repository root: a plain
/// subdirectory then yields no repository instead of its enclosing one.
pub(crate) fn bind_git_ceiling_above(command: &mut Command, directory: &std::path::Path) {
    let resolved = canonical_or_absolute(directory);
    if let Some(parent) = resolved.parent() {
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
}

fn canonical_or_absolute(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Prevent inherited Git routing from selecting a repository, worktree, or
/// index other than the explicitly supplied command directory.
pub(crate) fn scrub_git_routing_environment(command: &mut Command) {
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_GRAFT_FILE",
        "GIT_SHALLOW_FILE",
    ] {
        command.env_remove(variable);
    }
    for (variable, _) in std::env::vars_os() {
        if variable.as_encoded_bytes().starts_with(b"GIT_CONFIG_KEY_")
            || variable
                .as_encoded_bytes()
                .starts_with(b"GIT_CONFIG_VALUE_")
        {
            command.env_remove(variable);
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
enum ConfigValue {
    Missing,
    Value(String),
    Malformed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromisorReachability {
    Proven,
    NotProven,
    Malformed,
}

fn parse_git_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn git_stderr_indicates_malformed_object(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    [
        "corrupt",
        "unable to unpack",
        "inflate:",
        "bad object",
        "invalid object",
    ]
    .iter()
    .any(|marker| stderr.contains(marker))
}

fn valid_remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        && !name.contains("..")
        && !name.contains('\\')
        && !name.contains("//")
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
}

fn valid_orna_internal_ref(reference: &str) -> bool {
    let Some(suffix) = reference.strip_prefix("refs/orna/") else {
        return false;
    };
    !suffix.is_empty()
        && suffix.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.ends_with('.')
                && !component.ends_with(".lock")
                && !component.bytes().any(|byte| {
                    byte <= b' '
                        || byte == 0x7f
                        || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
                })
        })
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference.ends_with('@')
}

fn parse_remote_orna_refs(
    output: &str,
    object_id_length: usize,
    expected: &std::collections::BTreeMap<&str, &str>,
) -> Result<std::collections::BTreeMap<String, String>, RepositoryError> {
    if output.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let Some(records) = output.strip_suffix('\n') else {
        return Err(RepositoryError::GitOperationFailed);
    };
    let mut refs = std::collections::BTreeMap::new();
    for record in records.split('\n') {
        let Some((object_id, reference)) = record.split_once('\t') else {
            return Err(RepositoryError::GitOperationFailed);
        };
        if object_id.len() != object_id_length
            || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !valid_orna_internal_ref(reference)
            || !expected.contains_key(reference)
            || refs
                .insert(reference.to_owned(), object_id.to_ascii_lowercase())
                .is_some()
        {
            return Err(RepositoryError::GitOperationFailed);
        }
    }
    Ok(refs)
}

fn parse_committed_tree_entry(
    raw_entry: &[u8],
    object_id_length: usize,
) -> Result<CommittedTreeEntry, RepositoryError> {
    let tab = raw_entry
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or(RepositoryError::GitOperationFailed)?;
    let mut fields = raw_entry[..tab].split(|byte| *byte == b' ');
    let mode = fields.next().unwrap_or_default();
    let object_kind = fields.next().unwrap_or_default();
    let object = fields.next().unwrap_or_default();
    if fields.next().is_some()
        || object_kind != b"blob" && object_kind != b"commit"
        || object.len() != object_id_length
        || !object.iter().all(u8::is_ascii_hexdigit)
    {
        return Err(RepositoryError::GitOperationFailed);
    }
    let path = std::str::from_utf8(&raw_entry[tab + 1..])
        .map_err(|_| RepositoryError::GitOperationFailed)?;
    let path = ManagedPath::new(path)?;
    let kind = match (mode, object_kind) {
        (b"100644", b"blob") => CommittedTreeEntryKind::File { executable: false },
        (b"100755", b"blob") => CommittedTreeEntryKind::File { executable: true },
        (b"120000", b"blob") => CommittedTreeEntryKind::Symlink,
        (b"160000", b"commit") => CommittedTreeEntryKind::Submodule,
        _ => return Err(RepositoryError::GitOperationFailed),
    };
    Ok(CommittedTreeEntry { path, kind })
}

fn valid_native_object_id(object_id: &str) -> bool {
    matches!(object_id.len(), 40 | 64) && object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.contains("//")
        && !name.bytes().any(|byte| {
            byte <= b' '
                || byte == 0x7f
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
}

/// Safe public repository-boundary failures. Internal Git command lines,
/// worktree paths, and Git stderr are intentionally not exposed here.
#[derive(Debug)]
pub enum RepositoryError {
    GitUnavailable,
    GitOperationFailed,
    CandidateAssertionRefused(CandidateAssertionRefusal),
    LocalStateUnavailable,
    NotAWorktree,
    InvalidObjectId,
    InvalidInternalRef,
    UnsupportedObjectFormat,
    SnapshotNotFound,
    SnapshotNotReachable,
    InvalidSelector,
    InvalidBranchName,
    InvalidCommitMessage,
    /// No author or committer identity is configured for a reviewed commit,
    /// and Git is not allowed to guess one.
    MissingGitIdentity,
    DetachedHead,
    InvalidPublicationJournal,
    InvalidFormatMigration,
    InvalidCompactManifest,
    UnsafeManagedPath,
    NoManagedPaths,
    SysGitNothingToCommit,
    UnbornHead,
    StaleIndex {
        expected: IndexGeneration,
        actual: IndexGeneration,
    },
    StaleCwd,
    StaleReviewedCandidate,
    /// The staged candidate violates the format-3 snapshot invariants a
    /// reviewed commit validates before creating its commit
    /// (`sys.admin.assertion_failed`).
    InvalidCommitCandidate,
    ManagedContentConflict,
    /// Recovery found a pathname or private quarantine state that changed
    /// while it was being reconciled. The unexpected bytes remain on disk.
    PublicationRecoveryConflict,
    RepositoryBusy,
    GitIndexLockPresent,
    StaleHead,
    PublicationPending,
    CheckoutPlanStale,
    CheckoutDiscardSetMismatch,
    CheckoutExecutionUnsafe,
    CheckoutTargetInvalid,
    InvalidCheckoutJournal,
    CheckoutRecoveryRequired,
    RuntimeCompletionRequired,
    /// Native graph work failed while building or pinning a publication
    /// candidate. The graph context's own error identity is preserved.
    Graph(GraphError),
    /// Source format admission failed. The format context's own error
    /// identity is preserved so callers can report the exact cause.
    FormatContext(FormatContextError),
    /// This profile implements atomic index replacement only on POSIX
    /// filesystems. Windows requires a separately validated replacement path.
    PlatformUnsupported,
    /// A committed source tree exceeds the walk-size or depth limit before
    /// its entries are expanded.
    SourceTreeLimit,
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitUnavailable => f.write_str("Git is unavailable"),
            Self::CandidateAssertionRefused(_) => f.write_str("candidate table assertions refused the commit"),
            Self::GitOperationFailed => f.write_str("Git repository operation failed"),
            Self::LocalStateUnavailable => f.write_str("local Orna state is unavailable"),
            Self::NotAWorktree => f.write_str("not inside a Git worktree"),
            Self::InvalidObjectId => f.write_str("invalid native Git object ID"),
            Self::InvalidInternalRef => f.write_str("invalid Orna internal Git ref"),
            Self::UnsupportedObjectFormat => f.write_str("unsupported Git object format"),
            Self::SnapshotNotFound => f.write_str("committed snapshot was not found"),
            Self::SnapshotNotReachable => {
                f.write_str("committed snapshot is not reachable from this repository")
            }
            Self::InvalidSelector => f.write_str("invalid Git snapshot selector"),
            Self::InvalidBranchName => f.write_str("invalid Git branch name"),
            Self::InvalidCommitMessage => f.write_str("invalid Git commit message"),
            Self::MissingGitIdentity => {
                f.write_str("no Git author or committer identity is configured")
            }
            Self::DetachedHead => f.write_str("publication requires a symbolic Git HEAD"),
            Self::InvalidPublicationJournal => f.write_str("invalid publication journal"),
            Self::InvalidFormatMigration => {
                f.write_str("invalid or unsupported repository format migration")
            }
            Self::InvalidCompactManifest => f.write_str("invalid compact storage manifest"),
            Self::UnsafeManagedPath => f.write_str("unsafe managed path"),
            Self::NoManagedPaths => f.write_str("at least one managed path is required"),
            Self::SysGitNothingToCommit => f.write_str("sys.git.nothing_to_commit"),
            Self::UnbornHead => f.write_str("cannot create a branch from an unborn HEAD"),
            Self::StaleIndex { .. } => {
                f.write_str("Git index changed since the expected generation")
            }
            Self::StaleCwd => f.write_str("Git CWD changed during observation"),
            Self::StaleReviewedCandidate => f.write_str("reviewed commit candidate is stale or forged"),
            Self::InvalidCommitCandidate => {
                f.write_str("staged commit candidate is not a valid format-3 snapshot")
            }
            Self::ManagedContentConflict => {
                f.write_str("managed worktree content changed since capture")
            }
            Self::PublicationRecoveryConflict => {
                f.write_str("publication recovery found unexpected local state")
            }
            Self::RepositoryBusy => {
                f.write_str("repository is busy with an active Git or Orna operation")
            }
            Self::GitIndexLockPresent => {
                f.write_str("Git index lock is present; resolve it with Git before retrying")
            }
            Self::StaleHead => f.write_str("Git HEAD changed during repository operation"),
            Self::PublicationPending => {
                f.write_str("publication remains pending before Git ref advancement")
            }
            Self::CheckoutPlanStale => f.write_str("checkout preflight is stale"),
            Self::CheckoutDiscardSetMismatch => {
                f.write_str("checkout discard set does not match the preflight")
            }
            Self::CheckoutExecutionUnsafe => {
                f.write_str("checkout target does not match the current commit")
            }
            Self::CheckoutTargetInvalid => {
                f.write_str("checkout target does not admit as a repository snapshot")
            }
            Self::InvalidCheckoutJournal => f.write_str("invalid checkout recovery journal"),
            Self::CheckoutRecoveryRequired => {
                f.write_str("checkout recovery requires explicit operator resolution")
            }
            Self::RuntimeCompletionRequired => {
                f.write_str("runtime publication completion is required")
            }
            Self::Graph(error) => write!(f, "native graph operation failed: {error}"),
            Self::FormatContext(error) => write!(f, "repository format context failed: {error}"),
            Self::PlatformUnsupported => {
                f.write_str("atomic Git index replacement is unsupported on this platform")
            }
            Self::SourceTreeLimit => f.write_str("source tree exceeds the walk limit"),
        }
    }
}
impl std::error::Error for RepositoryError {}

/// Bridges real Git observation to the shared Orna CWD contract. The runtime
/// store is the sole authority for database/runtime identity and monotonic
/// logical generation; a Git commit selector stays a Git `SnapshotRef`.
pub struct OrnaRepositoryAdapter<S> {
    repository: Repository,
    runtime: S,
}
impl<S> OrnaRepositoryAdapter<S> {
    pub fn new(repository: Repository, runtime: S) -> Self {
        Self {
            repository,
            runtime,
        }
    }
}
#[derive(Debug)]
pub enum OrnaRepositoryAdapterError<E> {
    Runtime(E),
    Repository(RepositoryError),
    Bare,
    Identity,
    NonMonotonicGeneration,
}
impl<E: fmt::Display> fmt::Display for OrnaRepositoryAdapterError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(f, "runtime identity store: {error}"),
            Self::Repository(error) => error.fmt(f),
            Self::Bare => f.write_str("a bare repository has no CWD"),
            Self::Identity => f.write_str("CWD capture belongs to a different database or runtime"),
            Self::NonMonotonicGeneration => {
                f.write_str("CWD publication must strictly advance logical generation")
            }
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for OrnaRepositoryAdapterError<E> {}
impl<S: orna_foundation_v1::RuntimeIdentityStore> OrnaRepositoryAdapter<S> {
    fn identity(
        &self,
    ) -> Result<orna_foundation_v1::RepositoryIdentity, OrnaRepositoryAdapterError<S::Error>> {
        Ok(orna_foundation_v1::RepositoryIdentity {
            database_id: self
                .runtime
                .database_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
            repository_id: self
                .runtime
                .repository_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
        })
    }
    fn cwd(&self) -> Result<orna_foundation_v1::CwdCapture, OrnaRepositoryAdapterError<S::Error>> {
        let _ = self
            .repository
            .cwd_generation(RuntimeGeneration::new(0))
            .map_err(OrnaRepositoryAdapterError::Repository)?;
        let capture = self
            .runtime
            .capture_cwd()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        self.validate_capture(&capture)?;
        Ok(capture)
    }
    fn validate_capture(
        &self,
        capture: &orna_foundation_v1::CwdCapture,
    ) -> Result<(), OrnaRepositoryAdapterError<S::Error>> {
        let database_id = self
            .runtime
            .database_id()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        let runtime_id = self
            .runtime
            .runtime_id()
            .map_err(OrnaRepositoryAdapterError::Runtime)?;
        if capture.database_id() != database_id || capture.runtime_id() != runtime_id {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        Ok(())
    }
}
impl<S: orna_foundation_v1::RuntimeIdentityStore> orna_foundation_v1::RepositoryGenerationAdapter
    for OrnaRepositoryAdapter<S>
{
    type Error = OrnaRepositoryAdapterError<S::Error>;
    fn require_cwd(&self) -> Result<orna_foundation_v1::RepositoryIdentity, Self::Error> {
        self.identity()
    }
    fn profile(&self) -> Result<orna_foundation_v1::RepositoryProfile, Self::Error> {
        Ok(orna_foundation_v1::RepositoryProfile { bare: false })
    }
    fn committed_snapshot(
        &self,
    ) -> Result<Option<orna_foundation_v1::CanonicalSnapshot>, Self::Error> {
        let Some(head) = self
            .repository
            .head()
            .map_err(OrnaRepositoryAdapterError::Repository)?
        else {
            return Ok(None);
        };
        let oid =
            decode_object_id(head.as_str()).map_err(OrnaRepositoryAdapterError::Repository)?;
        let algorithm = match oid.len() {
            20 => orna_foundation_v1::GitHash::Sha1,
            32 => orna_foundation_v1::GitHash::Sha256,
            _ => {
                return Err(OrnaRepositoryAdapterError::Repository(
                    RepositoryError::InvalidObjectId,
                ));
            }
        };
        Ok(Some(orna_foundation_v1::CanonicalSnapshot::Commit {
            database: self
                .runtime
                .database_id()
                .map_err(OrnaRepositoryAdapterError::Runtime)?,
            algorithm,
            oid,
        }))
    }
    fn capture_cwd(
        &self,
        identity: orna_foundation_v1::RepositoryIdentity,
    ) -> Result<orna_foundation_v1::CwdCapture, Self::Error> {
        if identity != self.identity()? {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        self.cwd()
    }
    fn compare_and_set_cwd(
        &self,
        identity: orna_foundation_v1::RepositoryIdentity,
        expected: &orna_foundation_v1::CwdCapture,
        next: &orna_foundation_v1::CwdCapture,
    ) -> Result<orna_foundation_v1::CwdCas, Self::Error> {
        if identity != self.identity()? {
            return Err(OrnaRepositoryAdapterError::Identity);
        }
        let current = self.cwd()?;
        self.validate_capture(expected)?;
        self.validate_capture(next)?;
        if &current != expected {
            return Ok(orna_foundation_v1::CwdCas::Stale { current });
        }
        if next.generation() <= expected.generation() {
            return Err(OrnaRepositoryAdapterError::NonMonotonicGeneration);
        }
        self.runtime
            .compare_and_set_cwd(expected, next)
            .map_err(OrnaRepositoryAdapterError::Runtime)
    }
}

fn decode_object_id(value: &str) -> Result<Vec<u8>, RepositoryError> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RepositoryError::InvalidObjectId);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let (pairs, []) = value.as_bytes().as_chunks::<2>() else {
        return Err(RepositoryError::InvalidObjectId);
    };
    for pair in pairs {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(RepositoryError::InvalidObjectId)?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(RepositoryError::InvalidObjectId)?;
        bytes.push(u8::try_from((high << 4) | low).map_err(|_| RepositoryError::InvalidObjectId)?);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use crate::AcceptAllAssertions;
    use std::{collections::BTreeMap, fs, path::Path, process::Command};
    use super::{CONTENT_RACY_WINDOW, CONTENT_READS};

    #[cfg(target_os = "linux")]
    use std::{env, os::unix::ffi::OsStringExt, path::PathBuf};

    use super::{
        GitCommitRef, GitIndexLock, ManagedFileChange, ManagedPath, PublicationJournal,
        PublicationJournalEntry, PublicationJournalStage, PublicationMaterializationPhase,
        RemoteContinuity, Repository, RepositoryError, RuntimeGeneration, parse_remote_orna_refs,
    };

    #[cfg(target_os = "linux")]
    use super::{
        GitIndexLockMarker, GitIndexLockOwnerLiveness, GitIndexLockReclaimPoint,
        ProcessStartObservation, process_owner_liveness,
    };

    #[test]
    fn publication_journal_round_trips_durable_logical_rekeys() {
        let mut journal = PublicationJournal::new_with_runtime_intent(
            GitCommitRef("a".repeat(40)),
            GitCommitRef("b".repeat(40)),
            super::IndexTreeRef::from_verified_tree("c".repeat(40), 40).unwrap(),
            [1; 16],
            vec![PublicationJournalEntry::new(
                ManagedPath::new(".orna/database.orna").unwrap(),
                None,
                Some(b"committed".to_vec()),
            )],
        )
        .unwrap();
        let event = super::PublicationLogicalRowRekey::new(
            [2; 16],
            super::row_store::TypedKey::UInt(10)
                .canonical_bytes()
                .unwrap(),
            super::row_store::TypedKey::UInt(50)
                .canonical_bytes()
                .unwrap(),
        )
        .unwrap();
        journal = journal.with_logical_rekeys(vec![event.clone()]).unwrap();

        let recovered = PublicationJournal::decode(&journal.encode().unwrap(), 40).unwrap();
        assert_eq!(recovered.runtime_intent_id(), Some([1; 16]));
        assert_eq!(recovered.logical_rekeys(), &[event]);
    }

    /// Builds `parent/healthy` as a real repository and `parent/healthy/child`
    /// with a corrupt `HEAD`, so discovery from `child` must not reach `healthy`.
    #[cfg(unix)]
    pub(crate) fn corrupt_child_under_healthy_parent(root: &Path) -> std::path::PathBuf {
        let healthy = root.join("healthy");
        fs::create_dir_all(&healthy).unwrap();
        let init = Command::new("git")
            .current_dir(&healthy)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(init.success());
        let child = healthy.join("child");
        fs::create_dir_all(child.join(".git")).unwrap();
        fs::write(child.join(".git").join("HEAD"), b"garbage\n").unwrap();
        child
    }

    /// Runs `rev-parse HEAD` in `directory` under the production discovery bind.
    #[cfg(unix)]
    fn head_probe_succeeds_bound(directory: &Path) -> bool {
        let mut command = Command::new("git");
        command.current_dir(directory).args(["rev-parse", "HEAD"]);
        super::scrub_git_routing_environment(&mut command);
        super::bind_git_discovery_to_worktree(&mut command, directory);
        command
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[test]
    fn git_discovery_symlinked_worktree_does_not_fall_through_to_real_parent() {
        let root = tempfile::tempdir().unwrap();
        let child = corrupt_child_under_healthy_parent(root.path());
        let healthy = child.parent().unwrap();
        let init = Command::new("git")
            .current_dir(healthy)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "healthy",
            ])
            .status()
            .unwrap();
        assert!(init.success());
        let plain = root.path().join("x");
        fs::create_dir_all(&plain).unwrap();
        let link = plain.join("L");
        std::os::unix::fs::symlink(&child, &link).unwrap();
        assert!(!head_probe_succeeds_bound(&link));
    }

    #[cfg(unix)]
    #[test]
    fn git_discovery_relative_path_to_corrupt_child_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let _child = corrupt_child_under_healthy_parent(root.path());
        let previous = env::current_dir().unwrap();
        env::set_current_dir(root.path()).unwrap();
        let relative = Path::new("healthy").join("child");
        let ok = head_probe_succeeds_bound(&relative);
        env::set_current_dir(previous).unwrap();
        assert!(!ok, "relative corrupt child must not adopt healthy parent");
    }

    #[cfg(unix)]
    #[test]
    fn git_at_corrupt_root_under_healthy_parent_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let child = corrupt_child_under_healthy_parent(root.path());
        assert!(super::Repository::git_at(&child, ["rev-parse", "HEAD"]).is_err());
    }

    /// Replaces one length-prefixed string in an encoded journal with another,
    /// keeping the surrounding wire format valid.
    pub(crate) fn splice_length_prefixed(
        bytes: &[u8],
        old: &str,
        new: &str,
        length_prefix: fn(u32) -> [u8; 4],
    ) -> Vec<u8> {
        let mut needle = length_prefix(old.len() as u32).to_vec();
        needle.extend_from_slice(old.as_bytes());
        let mut replacement = length_prefix(new.len() as u32).to_vec();
        replacement.extend_from_slice(new.as_bytes());
        let at = bytes
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
            .expect("old path must be encoded in the journal");
        let mut out = bytes[..at].to_vec();
        out.extend_from_slice(&replacement);
        out.extend_from_slice(&bytes[at + needle.len()..]);
        out
    }

    #[test]
    fn publication_journal_decode_refuses_hostile_managed_paths() {
        let journal = PublicationJournal::new_with_runtime_intent(
            GitCommitRef("a".repeat(40)),
            GitCommitRef("b".repeat(40)),
            super::IndexTreeRef::from_verified_tree("c".repeat(40), 40).unwrap(),
            [1; 16],
            vec![PublicationJournalEntry::new(
                ManagedPath::new(".orna/database.orna").unwrap(),
                None,
                Some(b"committed".to_vec()),
            )],
        )
        .unwrap();
        let encoded = journal.encode().unwrap();
        assert!(PublicationJournal::decode(&encoded, 40).is_ok());
        for hostile in ["../escape", "/etc/passwd", "a\\b", "a\0b", "a//b"] {
            let spliced = splice_length_prefixed(
                &encoded,
                ".orna/database.orna",
                hostile,
                u32::to_le_bytes,
            );
            assert!(
                PublicationJournal::decode(&spliced, 40).is_err(),
                "journal path {hostile:?} must not decode"
            );
        }
    }

    #[test]
    fn checkout_journal_v1_legacy_bytes_decode_and_hostile_discard_paths_refuse() {
        let mut bytes = super::CHECKOUT_JOURNAL_MAGIC.to_vec();
        bytes.push(1);
        bytes.extend_from_slice(&7_u64.to_be_bytes());
        bytes.push(2);
        super::put_string(&mut bytes, &"a".repeat(40)).unwrap();
        bytes.extend_from_slice(&[9; 32]);
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        super::put_string(&mut bytes, "generated/row.orna").unwrap();
        assert!(super::CheckoutRecoveryJournal::decode(&bytes, 40).is_ok());
        let hostile = splice_length_prefixed(
            &bytes,
            "generated/row.orna",
            "../escape",
            u32::to_le_bytes,
        );
        assert!(super::CheckoutRecoveryJournal::decode(&hostile, 40).is_err());
    }

    #[test]
    fn publication_object_present_fails_closed_on_git_error() {
        let root = tempfile::TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(root.path()).unwrap();
        let mut hash = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(root.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(hash.stdin.as_mut().unwrap(), b"pinned").unwrap();
        let present = String::from_utf8(hash.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        let absent = "0123456789abcdef0123456789abcdef01234567";
        assert!(repository.publication_object_present(&present).unwrap());
        assert!(!repository.publication_object_present(absent).unwrap());

        // A corrupt HEAD makes git exit 128. The object may still exist, so
        // the answer is an error, never "absent" (which would skip the pin).
        fs::write(root.path().join(".git/HEAD"), "garbage\n").unwrap();
        assert!(matches!(
            repository.publication_object_present(&present),
            Err(RepositoryError::GitOperationFailed)
        ));
    }

    /// A child repository nested inside a parent repository must not let a
    /// corrupt child HEAD fall through to the parent. Git would then report the
    /// child's pinned object as missing, and the publication pin check would
    /// skip the pin (ORNA-PUB-006). The probe must fail closed instead.
    #[test]
    fn publication_probe_does_not_fall_through_a_corrupt_child_head() {
        let parent = tempfile::TempDir::new().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(parent.path())
                .status()
                .unwrap()
                .success()
        );
        let child = parent.path().join("child");
        fs::create_dir(&child).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&child)
                .status()
                .unwrap()
                .success()
        );
        let repository = Repository::discover(&child).unwrap();
        let mut hash = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(&child)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(hash.stdin.as_mut().unwrap(), b"pinned").unwrap();
        let present = String::from_utf8(hash.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert!(repository.publication_object_present(&present).unwrap());

        fs::write(child.join(".git/HEAD"), "garbage\n").unwrap();
        assert!(matches!(
            repository.publication_object_present(&present),
            Err(RepositoryError::GitOperationFailed)
        ));
    }
    #[test]
    fn managed_path_rejects_tracked_runtime_cache_path() {
        assert!(matches!(
            ManagedPath::new(".orna/cache.db"),
            Err(RepositoryError::UnsafeManagedPath)
        ));
        assert_eq!(
            ManagedPath::new(".orna/format.orna").unwrap().as_path(),
            Path::new(".orna/format.orna")
        );
    }

    #[test]
    fn only_verified_remote_continuity_permits_a_continuity_claim() {
        assert!(RemoteContinuity::Continuous.permits_continuity_claim());
        assert!(!RemoteContinuity::Missing.permits_continuity_claim());
        assert!(!RemoteContinuity::Stale.permits_continuity_claim());
        assert!(!RemoteContinuity::InvalidEvidence.permits_continuity_claim());
        assert!(!RemoteContinuity::Unverifiable.permits_continuity_claim());
    }

    #[test]
    fn remote_ref_parser_rejects_blank_or_unexpected_records() {
        let expected = BTreeMap::from([(
            "refs/orna/ids/0123456789abcdef",
            "0123456789012345678901234567890123456789",
        )]);
        assert!(parse_remote_orna_refs("\n", 40, &expected).is_err());
        assert!(
            parse_remote_orna_refs(
                "0123456789012345678901234567890123456789\trefs/orna/ids/unexpected\n",
                40,
                &expected,
            )
            .is_err()
        );
        assert!(
            parse_remote_orna_refs(
                "0123456789012345678901234567890123456789\trefs/orna/ids/0123456789abcdef\n\n",
                40,
                &expected,
            )
            .is_err()
        );
    }

    #[test]
    fn runtime_owner_lock_excludes_a_second_owner_until_drop() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let first = repository.acquire_runtime_owner_lock([1; 16]).unwrap();
        assert!(matches!(
            repository.acquire_runtime_owner_lock([2; 16]),
            Err(RepositoryError::RepositoryBusy)
        ));
        drop(first);
        let second = repository.acquire_runtime_owner_lock([2; 16]).unwrap();
        assert_eq!(second.previous_owner(), Some([1; 16]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_owner_chain_survives_repeated_startup_crashes() {
        use std::os::unix::process::ExitStatusExt;

        const REPOSITORY_ENV: &str = "ORNA_RUNTIME_OWNER_CRASH_REPOSITORY";
        const OWNER_ENV: &str = "ORNA_RUNTIME_OWNER_CRASH_ID";
        const TEST_FILTER: &str = "runtime_owner_chain_survives_repeated_startup_crashes";
        if let (Ok(path), Ok(owner)) = (std::env::var(REPOSITORY_ENV), std::env::var(OWNER_ENV)) {
            let owner = owner.parse::<u8>().unwrap();
            let repository = Repository::discover(path).unwrap();
            let _lock = repository.acquire_runtime_owner_lock([owner; 16]).unwrap();
            std::process::abort();
        }

        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let binary = std::env::current_exe().unwrap();
        for owner in [1_u8, 2, 3] {
            let status = Command::new("/bin/sh")
                .arg("-c")
                .arg("ulimit -c 0 2>/dev/null; exec \"$0\" \"$@\"")
                .arg(&binary)
                .args([TEST_FILTER, "--nocapture", "--test-threads=1"])
                .env(REPOSITORY_ENV, root.path())
                .env(OWNER_ENV, owner.to_string())
                .status()
                .unwrap();
            assert_eq!(status.signal(), Some(6), "startup child must abort");
        }

        let repository = Repository::discover(root.path()).unwrap();
        let recovered = repository.acquire_runtime_owner_lock([4; 16]).unwrap();
        assert_eq!(recovered.departed_owners(), &[[1; 16], [2; 16], [3; 16]]);
        assert_eq!(
            recovered
                .reclaim_departed_owner_pins(&BTreeMap::new())
                .unwrap(),
            Some(0)
        );
        drop(recovered);

        let retried = repository.acquire_runtime_owner_lock([5; 16]).unwrap();
        assert_eq!(retried.departed_owners(), &[[4; 16]]);
    }

    fn git(directory: &Path, arguments: &[&str]) -> String {
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
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn index_candidate_refuses_to_overwrite_an_existing_path() {
        let root = tempfile::tempdir().unwrap();
        let candidate = root.path().join("index-candidate-prepared");
        fs::write(&candidate, b"attacker-owned").unwrap();

        assert!(matches!(
            super::write_new_index_candidate(&candidate, b"index-bytes"),
            Err(RepositoryError::LocalStateUnavailable)
        ));
        assert_eq!(fs::read(&candidate).unwrap(), b"attacker-owned");
    }

    #[test]
    fn commit_optional_treats_option_shaped_expressions_as_revisions() {
        let root = repository_with_ignored_big_directory();
        let repository = Repository::discover(root.path()).unwrap();
        // Without `--end-of-options`, rev-parse would print this option's value
        // and succeed; as a revision name it is simply absent.
        assert!(matches!(
            repository.commit_optional("--git-dir=/nonexistent"),
            Ok(None)
        ));
    }

    fn repository_with_ignored_big_directory() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        fs::write(root.path().join(".gitignore"), "big/\n").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", ".gitignore", "main.orna"]);
        git(
            root.path(),
            &[
                "-c",
                "user.name=Orna Test",
                "-c",
                "user.email=orna@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        );
        fs::create_dir(root.path().join("big")).unwrap();
        root
    }

    #[test]
    fn cwd_generation_ignores_git_ignored_content() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("big/app.log"), b"one").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let before = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        fs::write(root.path().join("big/app.log"), b"two, longer").unwrap();
        let after = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        assert!(
            before == after,
            "git-ignored content must not enter the CWD observation"
        );
    }

    /// Opens a repository whose HEAD names a commit nesting one file
    /// `depth` directories deep: a tree the committed walk refuses by depth.
    fn repository_on_deep_head(depth: usize) -> (tempfile::TempDir, Repository, GitCommitRef) {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        let commit = crate::test_support::deep_nested_commit(root.path(), depth);
        git(root.path(), &["update-ref", "HEAD", &commit]);
        let repository = Repository::discover(root.path()).unwrap();
        let head = repository.head().unwrap().unwrap();
        (root, repository, head)
    }

    /// G1: the private-commit read-tree of HEAD must be preceded by the
    /// bounded expansion pre-pass, so a too-deep HEAD is refused.
    #[test]
    fn private_commit_refuses_a_head_tree_nested_beyond_the_walk_limit() {
        let (_root, repository, head) = repository_on_deep_head(300);
        let change = ManagedFileChange::new(
            ManagedPath::new("src/x.txt").unwrap(),
            Some(b"x\n".to_vec()),
        );
        assert!(matches!(
            repository.build_private_commit(&head, &[change], "orna: test deep head"),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    /// G3: with a current head present, the target tree is still checked
    /// before the diff against the current tree expands it.
    #[test]
    fn checkout_subplan_refuses_a_target_tree_nested_beyond_the_walk_limit() {
        let (root, repository, deep) = repository_on_deep_head(300);
        let shallow = git(
            root.path(),
            &["commit-tree", "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "-m", "shallow"],
        );
        git(root.path(), &["update-ref", "refs/heads/shallow", &shallow]);
        let current = repository.resolve_snapshot(&shallow).unwrap();
        assert!(matches!(
            repository.checkout_git_subplan(Some(&current), &deep),
            Err(RepositoryError::SourceTreeLimit)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cwd_generation_survives_non_utf8_git_ignored_names() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let root = repository_with_ignored_big_directory();
        fs::write(
            root.path().join("big").join(OsStr::from_bytes(b"\xff.bin")),
            b"x",
        )
        .unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
    }

    #[test]
    fn cwd_generation_detects_managed_dirty_content_change() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// one\n").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let before = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n// two\n").unwrap();
        let after = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        assert!(
            before != after,
            "a dirty managed file must change the CWD observation"
        );
    }

    /// Takes the dirty-file reads this thread recorded, clearing the record.
    fn content_reads_take() -> Vec<PathBuf> {
        CONTENT_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()))
    }

    /// A settled dirty file is read once across the CWD observations of one
    /// operation: the first observation reads it, the rest reuse its section.
    #[test]
    fn settled_dirty_file_is_read_once_across_cwd_observations() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// settled\n").unwrap();
        std::thread::sleep(CONTENT_RACY_WINDOW + std::time::Duration::from_millis(200));
        let repository = Repository::discover(root.path()).unwrap();
        content_reads_take();
        let first = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        repository.verify_cwd(&first).unwrap();
        repository.worktree_state().unwrap();
        let reads = content_reads_take();
        assert_eq!(reads.len(), 1, "a settled dirty file must be read once: {reads:?}");
        assert!(reads[0].ends_with("main.orna"));
    }

    /// Git-ignored bytes never enter the CWD observation, so no read names them.
    #[test]
    fn cwd_observation_reads_no_git_ignored_file() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("big/app.log"), b"ignored").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        content_reads_take();
        repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        repository.worktree_state().unwrap();
        let reads = content_reads_take();
        assert!(
            reads.iter().all(|path| !path.ends_with("app.log")),
            "git-ignored bytes must not be read: {reads:?}"
        );
    }

    /// A file written within the racy window is not settled, so its bytes are
    /// read again and a same-size rewrite still changes the observation.
    #[test]
    fn cwd_generation_detects_same_size_rewrite_within_racy_window() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// aaaa\n").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        content_reads_take();
        let before = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        assert_eq!(content_reads_take().len(), 1);
        fs::write(root.path().join("main.orna"), "module main;\n// bbbb\n").unwrap();
        let after = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        assert_eq!(
            content_reads_take().len(),
            1,
            "an unsettled dirty file must be read again"
        );
        assert!(
            before != after,
            "a same-size rewrite must change the CWD observation"
        );
    }

    /// Deleting a settled dirty file changes the observation; its cached
    /// section is never reused for a path that no longer exists.
    #[test]
    fn settled_dirty_file_deletion_changes_cwd_observation() {
        let root = repository_with_ignored_big_directory();
        fs::write(root.path().join("main.orna"), "module main;\n// keep\n").unwrap();
        std::thread::sleep(CONTENT_RACY_WINDOW + std::time::Duration::from_millis(200));
        let repository = Repository::discover(root.path()).unwrap();
        let before = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        fs::remove_file(root.path().join("main.orna")).unwrap();
        let after = repository.cwd_generation(RuntimeGeneration::new(0)).unwrap();
        assert!(before != after, "a deleted dirty file must change the CWD observation");
    }

    #[cfg(target_os = "linux")]
    fn dead_publisher_lock_bytes(binding: [u8; 32], nonce: [u8; 16]) -> Vec<u8> {
        let mut marker = GitIndexLockMarker::new(binding).unwrap();
        marker.process_start = if marker.process_start == u64::MAX {
            marker.process_start - 1
        } else {
            marker.process_start + 1
        };
        marker.nonce = nonce;
        marker.encode()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owner_liveness_fails_closed_when_procfs_is_indeterminate() {
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Unknown),
            GitIndexLockOwnerLiveness::Unknown
        );
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Observed(7)),
            GitIndexLockOwnerLiveness::Live
        );
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Observed(8)),
            GitIndexLockOwnerLiveness::ProvenDead
        );
        assert_eq!(
            process_owner_liveness(7, ProcessStartObservation::Missing),
            GitIndexLockOwnerLiveness::ProvenDead
        );
    }

    #[test]
    fn recovery_refuses_a_live_publishers_matching_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [7; 32];
        let lock = GitIndexLock::acquire_owned(lock_path.clone(), binding).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert!(lock_path.is_file());

        drop(lock);
        assert!(!lock_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unbound_orphan_reclaim_takes_only_a_dead_orna_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        fs::write(&lock_path, dead_publisher_lock_bytes([10; 32], [3; 16])).unwrap();
        GitIndexLock::reclaim_unbound_orphan(lock_path.clone()).unwrap();
        assert!(!lock_path.exists());

        let foreign = b"foreign Git index lock bytes\n".to_vec();
        fs::write(&lock_path, &foreign).unwrap();
        assert!(matches!(
            GitIndexLock::reclaim_unbound_orphan(lock_path.clone()),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), foreign);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_owner_orna_lock_is_never_reclaimed() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        // The marker names this live process, so its owner is not provably dead.
        let live = GitIndexLockMarker::new([10; 32]).unwrap().encode();
        fs::write(&lock_path, &live).unwrap();
        assert!(matches!(
            GitIndexLock::reclaim_unbound_orphan(lock_path.clone()),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), [10; 32]),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), live);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dead_ordinary_writer_lock_with_zero_binding_is_never_reclaimed() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        // All-zero binding marks an ordinary Orna writer, not a publisher.
        let ordinary = dead_publisher_lock_bytes([0; 32], [4; 16]);
        fs::write(&lock_path, &ordinary).unwrap();
        assert!(matches!(
            GitIndexLock::reclaim_unbound_orphan(lock_path.clone()),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), ordinary);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_refuses_a_lock_with_a_mismatched_journal_binding() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let lock_binding = [8; 32];
        let journal_binding = [7; 32];
        let original = dead_publisher_lock_bytes(lock_binding, [1; 16]);
        fs::write(&lock_path, &original).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), journal_binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_malformed_or_foreign_canonical_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [18; 32];
        let original = b"foreign Git index lock bytes\0\xff\n".to_vec();
        fs::write(&lock_path, &original).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), original);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_replacement_that_arrives_before_lock_transfer() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [9; 32];
        let stale = dead_publisher_lock_bytes(binding, [2; 16]);
        let claim = GitIndexLock::claim_path(&lock_path, &stale).unwrap();
        fs::write(&lock_path, stale).unwrap();
        let replacement = b"ordinary Git writer lock\n".to_vec();

        let result =
            GitIndexLock::reclaim_abandoned_with_hook(lock_path.clone(), binding, |point, path| {
                if point == GitIndexLockReclaimPoint::BeforeTransfer {
                    fs::write(path, &replacement).unwrap();
                }
            });
        assert!(matches!(result, Err(RepositoryError::GitIndexLockPresent)));
        assert_eq!(fs::read(&lock_path).unwrap(), replacement);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(!claim.exists());
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(lock_path).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_replacement_that_arrives_after_lock_transfer() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [10; 32];
        fs::write(&lock_path, dead_publisher_lock_bytes(binding, [3; 16])).unwrap();
        let replacement = b"new publisher lock\n".to_vec();

        let result =
            GitIndexLock::reclaim_abandoned_with_hook(lock_path.clone(), binding, |point, path| {
                if point == GitIndexLockReclaimPoint::AfterTransfer {
                    fs::write(path, &replacement).unwrap();
                }
            });
        assert!(matches!(result, Err(RepositoryError::GitIndexLockPresent)));
        assert_eq!(fs::read(&lock_path).unwrap(), replacement);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(lock_path).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_preserves_a_replacement_before_private_claim_disposal() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [13; 32];
        let stale = dead_publisher_lock_bytes(binding, [5; 16]);
        let claim = GitIndexLock::claim_path(&lock_path, &stale).unwrap();
        fs::write(&lock_path, stale).unwrap();
        let replacement = b"replacement private claim\n".to_vec();

        let result =
            GitIndexLock::reclaim_abandoned_with_hook(lock_path.clone(), binding, |point, path| {
                if point == GitIndexLockReclaimPoint::BeforePrivateClaimDisposal {
                    fs::write(path, &replacement).unwrap();
                }
            });
        assert!(matches!(result, Err(RepositoryError::GitIndexLockPresent)));
        assert!(!lock_path.exists());
        assert_eq!(fs::read(&claim).unwrap(), replacement);
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(claim).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_reclaims_its_interrupted_private_claim_only() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [11; 32];
        let bytes = dead_publisher_lock_bytes(binding, [4; 16]);
        let claim = GitIndexLock::claim_path(&lock_path, &bytes).unwrap();
        fs::write(&claim, &bytes).unwrap();

        GitIndexLock::reclaim_abandoned(lock_path.clone(), binding).unwrap();
        assert!(!lock_path.exists());
        assert!(!claim.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_reclaims_an_interrupted_private_disposal_tombstone() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [19; 32];
        let bytes = dead_publisher_lock_bytes(binding, [20; 16]);
        let disposal = lock_path.with_file_name(GitIndexLock::disposal_name(
            lock_path.file_name().unwrap(),
            &bytes,
        ));
        fs::write(&disposal, &bytes).unwrap();

        GitIndexLock::reclaim_abandoned(lock_path.clone(), binding).unwrap();
        assert!(!disposal.exists());
        assert!(!lock_path.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_retains_an_unknown_private_claim() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let mut claim = GitIndexLock::private_prefix(lock_path.file_name().unwrap());
        claim.push("unknown");
        let claim = root.path().join(claim);
        let unknown = b"do not delete\n".to_vec();
        fs::write(&claim, &unknown).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, [12; 32]),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(claim).unwrap(), unknown);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_retains_a_replacement_at_the_private_disposal_name() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [17; 32];
        let stale = dead_publisher_lock_bytes(binding, [10; 16]);
        let disposal = lock_path.with_file_name(GitIndexLock::disposal_name(
            lock_path.file_name().unwrap(),
            &stale,
        ));
        let replacement = b"replacement private tombstone\n".to_vec();
        fs::write(&disposal, &replacement).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path.clone(), binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(&disposal).unwrap(), replacement);
        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(disposal).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_retains_multiple_private_claims() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let binding = [14; 32];
        let first = dead_publisher_lock_bytes(binding, [6; 16]);
        let second = dead_publisher_lock_bytes(binding, [7; 16]);
        let first_claim = GitIndexLock::claim_path(&lock_path, &first).unwrap();
        let second_claim = GitIndexLock::claim_path(&lock_path, &second).unwrap();
        fs::write(&first_claim, &first).unwrap();
        fs::write(&second_claim, &second).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(first_claim).unwrap(), first);
        assert_eq!(fs::read(second_claim).unwrap(), second);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_rejects_a_symlinked_lock() {
        let root = tempfile::TempDir::new().unwrap();
        let lock_path = root.path().join("index.lock");
        let target = root.path().join("lock-target");
        let binding = [15; 32];
        let bytes = dead_publisher_lock_bytes(binding, [8; 16]);
        fs::write(&target, &bytes).unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();

        assert!(matches!(
            GitIndexLock::reclaim_abandoned(lock_path, binding),
            Err(RepositoryError::GitIndexLockPresent)
        ));
        assert_eq!(fs::read(target).unwrap(), bytes);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_supports_non_utf8_lock_basenames() {
        let root = tempfile::TempDir::new().unwrap();
        let mut name = vec![b'i'; 240];
        name[120] = 0xff;
        let lock_path = root.path().join(std::ffi::OsString::from_vec(name));
        let binding = [16; 32];
        fs::write(&lock_path, dead_publisher_lock_bytes(binding, [9; 16])).unwrap();

        GitIndexLock::reclaim_abandoned(lock_path.clone(), binding).unwrap();
        assert!(!lock_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publisher_index_lock_child() {
        let Some(path) = env::var_os("ORNA_TEST_GIT_INDEX_LOCK_PATH") else {
            return;
        };
        let encoded = env::var("ORNA_TEST_GIT_INDEX_LOCK_BINDING").unwrap();
        let bytes = (0..32)
            .map(|index| u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let binding: [u8; 32] = bytes.try_into().unwrap();
        let lock = GitIndexLock::acquire_owned(PathBuf::from(path), binding).unwrap();
        std::mem::forget(lock);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_reclaims_a_dead_publisher_index_lock() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(root.path().join("ordinary.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        fs::write(root.path().join("ordinary.txt"), "staged human edit\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        fs::write(root.path().join("main.orna"), "unstaged human edit\n").unwrap();
        let managed = super::ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let index_before = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[super::ManagedFileChange::new(
                    managed.clone(),
                    Some(b"candidate row\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index_before.tree().unwrap().clone(),
            [7; 16],
            vec![PublicationJournalEntry::new(
                managed.clone(),
                None,
                Some(b"candidate row\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository.advance_current_ref(&head, &candidate).unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        let lock_path = repository.git_path("index").unwrap().with_extension("lock");
        let binding = journal.lock_binding().unwrap();
        let encoded_binding = binding
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::publisher_index_lock_child",
                "--nocapture",
            ])
            .env("ORNA_TEST_GIT_INDEX_LOCK_PATH", &lock_path)
            .env("ORNA_TEST_GIT_INDEX_LOCK_BINDING", encoded_binding)
            .status()
            .unwrap();
        assert!(child.success());
        assert!(lock_path.is_file());

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        assert!(!lock_path.exists());
        assert_eq!(repository.head().unwrap(), Some(candidate.commit().clone()));
        assert_eq!(
            git(root.path(), &["show", ":ordinary.txt"]),
            "staged human edit"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("main.orna")).unwrap(),
            "unstaged human edit\n"
        );
        assert_eq!(
            fs::read(root.path().join(managed.as_path())).unwrap(),
            b"candidate row\n"
        );
        let mut completed = repository.read_publication_journal().unwrap().unwrap();
        repository
            .mark_runtime_complete([7; 16], &mut completed)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
    }

    #[test]
    fn fresh_recovery_resumes_persisted_index_and_completion_phases() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"first\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [31; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"first\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository.advance_current_ref(&head, &candidate).unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .reconcile_published_index(&index, &candidate, std::slice::from_ref(&path))
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"first\n"
        );
        let mut completed = repository.read_publication_journal().unwrap().unwrap();
        repository
            .mark_runtime_complete([31; 16], &mut completed)
            .unwrap();

        let old_head = repository.head().unwrap().unwrap();
        let old_index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"second\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head.clone(),
            candidate.commit().clone(),
            old_index.tree().unwrap().clone(),
            [32; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"first\n".to_vec()),
                Some(b"second\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&old_head, &candidate)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .reconcile_published_index(&old_index, &candidate, std::slice::from_ref(&path))
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        repository
            .materialize_publication_entry(&mut journal, 0)
            .unwrap();
        journal
            .advance(PublicationJournalStage::WorktreeReconciled)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RuntimeCompleted)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        assert!(repository.recover_publication().unwrap().is_some());
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"second\n"
        );
    }

    /// ORNA-PUB-007: a publisher killed after installing the reconciled index
    /// but before IndexReconciled is durable leaves `.orna/store` bound as
    /// leaf entries. Recovery must recognise that index as the candidate's
    /// rather than report it as stale.
    #[test]
    fn recovery_recognises_an_installed_store_index_before_its_journal_stage() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        let database = include_str!("../tests/fixtures/format-context/database-final.orna");
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), database).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/data").unwrap(),
                        Some(b"store root two".to_vec()),
                    ),
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/refs/node/data").unwrap(),
                        Some(b"child node".to_vec()),
                    ),
                    ManagedFileChange::new(
                        ManagedPath::new(".orna/store/refs/node/chunk").unwrap(),
                        Some(b"payload chunk".to_vec()),
                    ),
                ],
                "orna: publish runtime data",
            )
            .unwrap();
        let store_root = git(
            root.path(),
            &[
                "rev-parse",
                &format!("{}:.orna/store", candidate.commit().as_str()),
            ],
        );
        let algorithm = if store_root.len() == 64 {
            super::GitHashAlgorithm::Sha256
        } else {
            super::GitHashAlgorithm::Sha1
        };
        let marker = ManagedPath::new(".orna/database.orna").unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [61; 16],
            vec![PublicationJournalEntry::new(
                marker.clone(),
                Some(database.as_bytes().to_vec()),
                Some(database.as_bytes().to_vec()),
            )],
        )
        .unwrap()
        .with_store_root(&super::NativeOid::from_hex(algorithm, &store_root).unwrap())
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository.advance_current_ref(&head, &candidate).unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        let installed = repository
            .reconcile_published_index(
                &index,
                &candidate,
                &[marker, ManagedPath::new(".orna/store").unwrap()],
            )
            .unwrap();
        assert!(
            git(root.path(), &["ls-files", "--stage", "--", ".orna/store"])
                .lines()
                .count()
                > 1
        );

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            recovered.stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(repository.index_generation().unwrap(), installed);
        assert_eq!(
            fs::read(root.path().join(".orna/store/data")).unwrap(),
            b"store root two"
        );
        repository
            .mark_runtime_complete([61; 16], &mut recovered)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
    }

    #[test]
    fn recovery_finalizes_runtime_completed_descendant_head() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(root.path().join("ordinary.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let old_head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"published\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [33; 16],
            vec![PublicationJournalEntry::new(
                path,
                None,
                Some(b"published\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&journal.old_head().clone(), &candidate)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RefAdvanced)
            .unwrap();
        journal
            .advance(PublicationJournalStage::IndexReconciled)
            .unwrap();
        fs::create_dir_all(root.path().join("generated")).unwrap();
        fs::write(root.path().join("generated/row.orna"), "published\n").unwrap();
        git(root.path(), &["add", "generated/row.orna"]);
        journal
            .advance(PublicationJournalStage::WorktreeReconciled)
            .unwrap();
        journal
            .advance(PublicationJournalStage::RuntimeCompleted)
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();

        fs::write(root.path().join("ordinary.txt"), "later\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["commit", "-m", "ordinary follow-up"]);
        let follow_up = repository.head().unwrap().unwrap();

        // Spec ORNA-PUB-009 / recovery table row "Ref no longer H or N": the
        // ordinary descendant proves the batch is present, so cleanup finishes
        // under the new ref without rewriting it (ORNA-PUB-017).
        assert!(repository.recover_publication().unwrap().is_some());
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap().unwrap(), follow_up);
    }

    /// Builds a publication whose candidate is reconciled into the ordinary
    /// index and worktree (stage `WorktreeReconciled`) and whose ref is the
    /// candidate, as a crash after reconciliation leaves it.
    fn publication_reconciled_at_candidate(root: &Path) -> (Repository, PublicationJournal) {
        git(root, &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root);
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("main.orna"), "module main;\n").unwrap();
        fs::write(root.join("ordinary.txt"), "base\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "initial"]);

        let repository = Repository::discover(root).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let old_head = repository.head().unwrap().unwrap();
        let index = repository.index_generation().unwrap();
        let candidate = repository
            .build_private_commit(
                &old_head,
                &[ManagedFileChange::new(
                    path.clone(),
                    Some(b"published\n".to_vec()),
                )],
                "orna: publish runtime data",
            )
            .unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            old_head,
            candidate.commit().clone(),
            index.tree().unwrap().clone(),
            [33; 16],
            vec![PublicationJournalEntry::new(
                path,
                None,
                Some(b"published\n".to_vec()),
            )],
        )
        .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        repository
            .advance_current_ref(&journal.old_head().clone(), &candidate)
            .unwrap();
        let row = root.join("generated");
        fs::create_dir_all(&row).unwrap();
        fs::write(row.join("row.orna"), "published\n").unwrap();
        git(root, &["add", "generated/row.orna"]);
        journal.advance(PublicationJournalStage::RefAdvanced).unwrap();
        journal.advance(PublicationJournalStage::IndexReconciled).unwrap();
        journal.advance(PublicationJournalStage::WorktreeReconciled).unwrap();
        repository.write_publication_journal(&journal).unwrap();
        (repository, journal)
    }

    #[test]
    fn worktree_reconciled_descendant_head_completes_via_runtime_completion() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        fs::write(root.path().join("ordinary.txt"), "later\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["commit", "-m", "ordinary follow-up"]);
        let follow_up = repository.head().unwrap().unwrap();

        // The runtime transaction must still complete before cleanup.
        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        repository
            .mark_runtime_complete([33; 16], &mut journal)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap().unwrap(), follow_up);
        assert_eq!(
            fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
            "later\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    /// Recovery table row "Ref no longer H or N" without ancestry proof: a
    /// HEAD that does not contain the candidate is a recovery conflict. The
    /// journal and the newer ref are both preserved.
    #[test]
    fn recovery_fences_head_that_does_not_contain_candidate() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        let tree = git(
            root.path(),
            &["rev-parse", &format!("{}^{{tree}}", old_head.as_str())],
        );
        let sibling = git(
            root.path(),
            &["commit-tree", tree.trim(), "-p", old_head.as_str(), "-m", "sibling"],
        );
        git(root.path(), &["update-ref", "refs/heads/main", sibling.trim()]);

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleHead)
        ));
        assert!(matches!(
            repository.mark_runtime_complete([33; 16], &mut journal),
            Err(RepositoryError::StaleHead)
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(repository.head().unwrap().unwrap().as_str(), sibling.trim());
    }

    /// Recovery row "Index or affected path differs from both recorded
    /// states": a managed entry dropped from the index after reconciliation is
    /// a typed conflict even when a descendant commit exists. The journal and
    /// the managed bytes are preserved (ORNA-PUB-011, ORNA-PUB-017).
    #[test]
    fn descendant_completion_refuses_index_that_dropped_managed_entry() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        git(root.path(), &["rm", "--cached", "-q", "generated/row.orna"]);
        fs::write(root.path().join("ordinary.txt"), "later\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["commit", "-m", "follow-up after index edit"]);

        let result = repository.mark_runtime_complete([33; 16], &mut journal);
        assert!(
            matches!(result, Err(RepositoryError::StaleIndex { .. })),
            "{result:?}"
        );
        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleIndex { .. })
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    /// An unrelated staged edit is outside the published batch and survives
    /// runtime completion.
    #[test]
    fn runtime_completion_preserves_unrelated_staged_change() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        fs::write(root.path().join("ordinary.txt"), "staged\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        repository
            .mark_runtime_complete([33; 16], &mut journal)
            .unwrap();

        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(
            git(root.path(), &["diff", "--cached", "--name-only"]).trim(),
            "ordinary.txt"
        );
    }

    /// A rewound HEAD (an ancestor of the candidate) does not contain the
    /// batch: the journal and the managed bytes are preserved.
    #[test]
    fn recovery_fences_rewound_head_and_keeps_managed_bytes() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        git(root.path(), &["update-ref", "refs/heads/main", old_head.as_str()]);

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleHead)
        ));
        assert!(matches!(
            repository.mark_runtime_complete([33; 16], &mut journal),
            Err(RepositoryError::StaleHead)
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    /// A same-tree commit with a different parent (a rebase) is a different
    /// commit: containment is by commit id, so it is refused.
    #[test]
    fn recovery_fences_rebased_same_tree_commit() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        let tree = git(
            root.path(),
            &["rev-parse", &format!("{}^{{tree}}", journal.new_head().as_str())],
        );
        let rebased = git(
            root.path(),
            &["commit-tree", tree.trim(), "-p", old_head.as_str(), "-m", "rebased"],
        );
        git(root.path(), &["update-ref", "refs/heads/main", rebased.trim()]);

        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::StaleHead)
        ));
        assert!(matches!(
            repository.mark_runtime_complete([33; 16], &mut journal),
            Err(RepositoryError::StaleHead)
        ));
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .stage(),
            PublicationJournalStage::WorktreeReconciled
        );
    }

    /// A merge whose first parent is the candidate descends from it, so the
    /// batch is present and completion leaves ref, index and bytes untouched.
    #[test]
    fn runtime_completion_accepts_merge_descending_from_candidate() {
        let root = tempfile::TempDir::new().unwrap();
        let (repository, mut journal) = publication_reconciled_at_candidate(root.path());

        let old_head = journal.old_head().clone();
        let candidate = journal.new_head().clone();
        let tree = git(
            root.path(),
            &["rev-parse", &format!("{}^{{tree}}", candidate.as_str())],
        );
        let side = git(
            root.path(),
            &["commit-tree", tree.trim(), "-p", old_head.as_str(), "-m", "side"],
        );
        let merge = git(
            root.path(),
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                candidate.as_str(),
                "-p",
                side.trim(),
                "-m",
                "merge",
            ],
        );
        git(root.path(), &["update-ref", "refs/heads/main", merge.trim()]);

        repository
            .mark_runtime_complete([33; 16], &mut journal)
            .unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap().unwrap().as_str(), merge.trim());
        assert_eq!(
            fs::read_to_string(root.path().join("generated/row.orna")).unwrap(),
            "published\n"
        );
    }

    #[test]
    fn checkout_preflight_rejects_same_commit_branch_attachment_drift() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);
        git(root.path(), &["branch", "target"]);
        git(root.path(), &["branch", "interleaved"]);

        let repository = Repository::discover(root.path()).unwrap();
        let plan = repository
            .plan_checkout("target", RuntimeGeneration::new(41))
            .unwrap();
        let token = plan.force_token();

        git(root.path(), &["switch", "interleaved"]);

        assert!(matches!(
            repository.verify_checkout_preflight(&plan),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert!(matches!(
            repository.authorize_checkout_force(&plan, true, Some(&token)),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert_eq!(
            Command::new("git")
                .current_dir(root.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
            b"interleaved\n"
        );
    }

    #[test]
    fn checkout_force_witness_rejects_same_status_content_drift() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        fs::write(root.path().join("ordinary.txt"), "base\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        git(root.path(), &["branch", "experiment"]);
        git(root.path(), &["switch", "experiment"]);
        fs::write(root.path().join("ordinary.txt"), "target\n").unwrap();
        fs::write(root.path().join("ignored.txt"), "ignored target\n").unwrap();
        git(root.path(), &["add", "ordinary.txt"]);
        git(root.path(), &["add", "-f", "ignored.txt"]);
        git(root.path(), &["commit", "-m", "target"]);
        git(root.path(), &["switch", "main"]);
        fs::write(root.path().join("ordinary.txt"), "first local edit\n").unwrap();
        fs::write(root.path().join("ignored.txt"), "first ignored edit\n").unwrap();

        let repository = Repository::discover(root.path()).unwrap();
        let plan = repository
            .plan_checkout("experiment", RuntimeGeneration::new(51))
            .unwrap();
        let token = plan.force_token();
        let discard = repository
            .validate_checkout_discard_set(
                &plan,
                true,
                Some(&token),
                plan.git().discardable_paths(),
            )
            .unwrap();

        fs::write(root.path().join("ordinary.txt"), "second local edit\n").unwrap();
        assert!(matches!(
            repository.verify_validated_checkout_discard(&discard),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        fs::write(root.path().join("ordinary.txt"), "first local edit\n").unwrap();
        repository
            .persist_validated_checkout_discard(&discard)
            .unwrap();

        fs::write(root.path().join("ignored.txt"), "second ignored edit\n").unwrap();
        let head_before = repository.head().unwrap();
        assert!(matches!(
            repository.verify_validated_checkout_discard(&discard),
            Err(RepositoryError::CheckoutPlanStale)
        ));
        assert!(matches!(
            repository.recover_pre_execution_checkout(),
            Err(RepositoryError::CheckoutRecoveryRequired)
        ));
        assert_eq!(repository.head().unwrap(), head_before);
        assert_eq!(
            repository.current_branch().unwrap().as_deref(),
            Some("main")
        );
        assert_eq!(
            fs::read_to_string(root.path().join("ordinary.txt")).unwrap(),
            "first local edit\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("ignored.txt")).unwrap(),
            "second ignored edit\n"
        );
        assert!(repository.has_pending_pre_execution_checkout().unwrap());
    }

    #[test]
    fn journaled_materialization_replaces_and_deletes_without_quarantine_leaks() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            GitCommitRef("b".repeat(40)),
            tree.clone(),
            [21; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                None,
                Some(b"first".to_vec()),
            )],
        )
        .unwrap();
        repository
            .materialize_publication_entry(&mut journal, 0)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"first"
        );

        let mut replacement = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            GitCommitRef("c".repeat(40)),
            tree.clone(),
            [22; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"first".to_vec()),
                Some(b"second".to_vec()),
            )],
        )
        .unwrap();
        let replacement_name = replacement.quarantine_name(0).unwrap();
        repository
            .materialize_publication_entry(&mut replacement, 0)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"second"
        );
        assert!(
            !root
                .path()
                .join("generated")
                .join(replacement_name)
                .exists()
        );

        let mut deletion = PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("d".repeat(40)),
            tree,
            [23; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"second".to_vec()),
                None,
            )],
        )
        .unwrap();
        repository
            .materialize_publication_entry(&mut deletion, 0)
            .unwrap();
        assert!(!root.path().join(path.as_path()).exists());
    }

    #[test]
    fn journaled_materialization_preserves_quarantine_on_install_conflict_and_deletion_cleanup() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let conflict_path = ManagedPath::new("generated/conflict.orna").unwrap();
        let conflict_target = root.path().join(conflict_path.as_path());
        fs::create_dir_all(conflict_target.parent().unwrap()).unwrap();
        fs::write(&conflict_target, b"captured").unwrap();
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        let mut conflict = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            GitCommitRef("f".repeat(40)),
            tree.clone(),
            [25; 16],
            vec![PublicationJournalEntry::new(
                conflict_path.clone(),
                Some(b"captured".to_vec()),
                Some(b"published".to_vec()),
            )],
        )
        .unwrap();
        let conflict_name = conflict.quarantine_name(0).unwrap();
        conflict
            .set_materialization(
                0,
                PublicationMaterializationPhase::QuarantinePrepared,
                Some(conflict_name.clone()),
            )
            .unwrap();
        repository.write_publication_journal(&conflict).unwrap();
        let mut after_boundary = |phase| {
            conflict.set_materialization(0, phase, Some(conflict_name.clone()))?;
            repository.write_publication_journal_locked(&conflict)?;
            if phase == PublicationMaterializationPhase::Quarantined {
                fs::write(&conflict_target, b"external").unwrap();
            }
            Ok(())
        };
        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &conflict_path,
                Some(b"captured"),
                Some(b"published"),
                Some(&conflict_target.parent().unwrap().join(&conflict_name)),
                None,
                Some(&mut after_boundary),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        let conflict_quarantine = conflict_target.parent().unwrap().join(conflict_name);
        assert_eq!(fs::read(&conflict_quarantine).unwrap(), b"captured");
        assert_eq!(fs::read(&conflict_target).unwrap(), b"external");
        let persisted = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(
            persisted.entries()[0].materialization().phase,
            PublicationMaterializationPhase::Quarantined
        );

        let delete_path = ManagedPath::new("generated/delete.orna").unwrap();
        let delete_target = root.path().join(delete_path.as_path());
        let mut deletion = PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("1".repeat(40)),
            tree,
            [26; 16],
            vec![PublicationJournalEntry::new(
                delete_path,
                Some(b"to-delete".to_vec()),
                None,
            )],
        )
        .unwrap();
        let delete_name = deletion.quarantine_name(0).unwrap();
        let delete_quarantine = delete_target.parent().unwrap().join(&delete_name);
        fs::write(&delete_quarantine, b"to-delete").unwrap();
        deletion
            .set_materialization(
                0,
                PublicationMaterializationPhase::Installed,
                Some(delete_name),
            )
            .unwrap();
        repository.write_publication_journal(&deletion).unwrap();
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();
        repository
            .materialize_publication_entry(&mut recovered, 0)
            .unwrap();
        assert!(!delete_target.exists());
        assert!(!delete_quarantine.exists());
        assert_eq!(
            recovered.entries()[0].materialization().phase,
            PublicationMaterializationPhase::Clean
        );
    }

    #[test]
    fn final_quarantine_replacement_is_preserved_and_reports_conflict() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let parent = root.path().join("generated");
        fs::create_dir_all(&parent).unwrap();
        let quarantine = parent.join(".orna-quarantine-boundary");
        let target = parent.join("row.orna");
        fs::write(&quarantine, b"captured").unwrap();
        fs::write(&target, b"published").unwrap();
        let mut replacement_at_boundary = || {
            fs::write(&quarantine, b"external").unwrap();
            Ok(())
        };

        assert!(matches!(
            repository.remove_verified_quarantine_with_boundary(
                &parent,
                &quarantine,
                &target,
                Some(b"captured"),
                Some(b"published"),
                Some(&mut replacement_at_boundary),
            ),
            Err(RepositoryError::PublicationRecoveryConflict)
        ));
        assert_eq!(fs::read(&quarantine).unwrap(), b"external");
        assert!(
            !repository
                .runtime_paths()
                .root()
                .join("publication-cleanup")
                .read_dir()
                .unwrap()
                .any(|entry| entry.is_ok())
        );
    }

    #[test]
    fn journaled_materialization_recovers_a_persisted_quarantine_phase() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let head = repository.head().unwrap().unwrap();
        let tree = repository
            .index_generation()
            .unwrap()
            .tree()
            .unwrap()
            .clone();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head,
            GitCommitRef("e".repeat(40)),
            tree,
            [24; 16],
            vec![PublicationJournalEntry::new(
                path.clone(),
                Some(b"captured".to_vec()),
                Some(b"published".to_vec()),
            )],
        )
        .unwrap();
        let quarantine = root
            .path()
            .join("generated")
            .join(journal.quarantine_name(0).unwrap());
        fs::create_dir_all(quarantine.parent().unwrap()).unwrap();
        fs::write(&quarantine, b"captured").unwrap();
        journal
            .set_materialization(
                0,
                PublicationMaterializationPhase::Quarantined,
                Some(
                    quarantine
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                ),
            )
            .unwrap();
        repository.write_publication_journal(&journal).unwrap();
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();

        repository
            .materialize_publication_entry(&mut recovered, 0)
            .unwrap();
        assert_eq!(
            fs::read(root.path().join(path.as_path())).unwrap(),
            b"published"
        );
        assert!(!quarantine.exists());
        assert_eq!(
            recovered.entries()[0].materialization().phase,
            PublicationMaterializationPhase::Clean
        );
        assert_eq!(
            repository
                .read_publication_journal()
                .unwrap()
                .unwrap()
                .entries()[0]
                .materialization()
                .phase,
            PublicationMaterializationPhase::Clean
        );
    }

    #[test]
    fn replacement_preserves_editor_race_after_final_check() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = super::ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let mut editor_race = || {
            fs::write(&target, b"editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&mut editor_race),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert_eq!(
            fs::read_dir(target.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".orna-quarantine-")
                })
                .count(),
            0
        );
        repository
            .materialize_managed_file(&path, Some(b"editor"), Some(b"second"))
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
    }

    #[test]
    fn quarantine_restore_preserves_newer_editor_race_after_absence_check() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let parent = root.path().join("generated");
        fs::create_dir_all(&parent).unwrap();
        let target = parent.join("row.orna");
        let quarantine = parent.join(".orna-quarantine-test");
        fs::write(&quarantine, b"captured editor").unwrap();
        let mut newer_editor = || {
            fs::write(&target, b"newer editor").unwrap();
        };

        assert!(matches!(
            repository.restore_managed_quarantine_impl(
                &quarantine,
                &target,
                Some(&mut newer_editor),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"newer editor");
        assert_eq!(fs::read(&quarantine).unwrap(), b"captured editor");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn quarantine_replacement_race_preserves_both_files() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-replacement-race");
        let mut replacement = || {
            fs::write(&quarantine, b"unexpected replacement").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                Some(b"second"),
                Some(&quarantine),
                Some(&mut replacement),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"unexpected replacement");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn quarantine_deletion_race_preserves_both_files() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let quarantine = target
            .parent()
            .unwrap()
            .join(".orna-quarantine-deletion-race");
        let mut deletion = || {
            fs::write(&quarantine, b"unexpected deletion race").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl_with_quarantine(
                &path,
                Some(b"first"),
                None,
                Some(&quarantine),
                Some(&mut deletion),
                None,
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(fs::read(&quarantine).unwrap(), b"unexpected deletion race");
    }

    #[test]
    fn fresh_materialization_invokes_before_install_once_at_absence_boundary() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = ManagedPath::new("generated/row.orna").unwrap();
        let target = root.path().join(path.as_path());
        let mut calls = 0;
        let mut before_install = || {
            calls += 1;
            assert!(!target.exists());
            fs::write(&target, b"external editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                None,
                Some(b"generated"),
                Some(&mut before_install),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(calls, 1);
        assert_eq!(fs::read(&target).unwrap(), b"external editor");
    }

    #[test]
    fn deletion_revalidates_editor_race_and_retries_idempotently() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = super::ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let head_before = repository.head().unwrap();
        let index_before = repository.index_generation().unwrap();
        let target = root.path().join(path.as_path());
        let mut editor_race = || {
            fs::write(&target, b"editor").unwrap();
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                Some(b"first"),
                None,
                Some(&mut editor_race),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        assert_eq!(repository.head().unwrap(), head_before);
        assert_eq!(repository.index_generation().unwrap(), index_before);

        repository
            .materialize_managed_file(&path, Some(b"editor"), None)
            .unwrap();
        repository
            .materialize_managed_file(&path, None, None)
            .unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn reviewed_candidate_publication_recovers_and_clears_its_journal() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let staged = b"module main;\n// reviewed\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([4; 16]), super::Uuid::from_bytes([5; 16]), 11,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(12), vec![0x81, 0x00],
        ).unwrap();
        let head = review.cwd.head().unwrap().clone();
        let path = ManagedPath::new("main.orna").unwrap();
        let candidate = repository.build_private_commit(
            &head, &[ManagedFileChange::new(path.clone(), Some(staged.to_vec()))], "reviewed",
        ).unwrap();
        assert_eq!(candidate.tree, review.index_tree);
        let intent = [6; 16];
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head, candidate.commit().clone(), review.index_tree.clone(), intent,
            vec![PublicationJournalEntry::new(path, Some(staged.to_vec()), Some(staged.to_vec()))],
        ).unwrap();
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            repository.publish_candidate_impl_with_migration(
                &review.cwd.index, &candidate, &mut journal, false, Some(&review),
                |stage| if stage == PublicationJournalStage::RefAdvanced { panic!("simulated interruption") },
            ).unwrap();
        }));
        assert!(interrupted.is_err());
        assert_eq!(repository.read_publication_journal().unwrap().unwrap().stage(), PublicationJournalStage::RefAdvanced);
        assert!(matches!(
            repository.recover_publication(),
            Err(RepositoryError::RuntimeCompletionRequired)
        ));
        let mut recovered = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(recovered.stage(), PublicationJournalStage::WorktreeReconciled);
        assert_eq!(recovered.runtime_intent_id(), Some(intent));
        assert!(recovered.compact_manifest().is_none());
        repository.mark_runtime_complete(intent, &mut recovered).unwrap();
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert_eq!(repository.head().unwrap(), Some(candidate.commit().clone()));
    }

    fn overwrite_keeping_mtime(path: &std::path::Path, bytes: &[u8]) {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, bytes).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn reviewed_candidate_detects_same_size_same_mtime_edit_before_ref_advance() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let staged = b"module main;\n// reviewed\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([40; 16]), super::Uuid::from_bytes([41; 16]), 21,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(22), vec![0x81, 0x00],
        ).unwrap();
        let head_before = repository.head().unwrap();

        // Same length, restored mtime: only a content compare can see this edit.
        overwrite_keeping_mtime(&root.path().join("main.orna"), b"module main;\n// edited!\n");
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));
        assert_eq!(repository.head().unwrap(), head_before, "a stale review must not advance the ref");
        assert_eq!(git(root.path(), &["for-each-ref", "refs/orna/"]), "");
    }

    #[test]
    fn reviewed_candidate_detects_same_size_same_mtime_edit_after_ref_advance() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let staged = b"module main;\n// reviewed\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([42; 16]), super::Uuid::from_bytes([43; 16]), 23,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(24), vec![0x81, 0x00],
        ).unwrap();
        let head = review.cwd.head().unwrap().clone();
        let path = ManagedPath::new("main.orna").unwrap();
        let candidate = repository.build_private_commit(
            &head, &[ManagedFileChange::new(path.clone(), Some(staged.to_vec()))], "reviewed",
        ).unwrap();
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head, candidate.commit().clone(), review.index_tree.clone(), [44; 16],
            vec![PublicationJournalEntry::new(path, Some(staged.to_vec()), Some(staged.to_vec()))],
        ).unwrap();
        let root_path = root.path().join("main.orna");
        let result = repository.publish_candidate_impl_with_migration(
            &review.cwd.index, &candidate, &mut journal, false, Some(&review),
            |stage| if stage == PublicationJournalStage::RefAdvanced {
                overwrite_keeping_mtime(&root_path, b"module main;\n// edited!\n");
            },
        );
        // The edit lands after RefAdvanced is durable. Reconciliation must refuse to
        // materialise over bytes that match neither the batch nor the expected state
        // (ORNA-PUB-007: recovery is required, the publication is not reported as complete).
        assert!(
            matches!(result, Err(RepositoryError::ManagedContentConflict)),
            "post-advance same-size edit must fail with ManagedContentConflict, got {result:?}"
        );
        // ORNA-PUB-005: the ref names the candidate because it advanced before the edit.
        assert!(repository.head().unwrap().as_ref() == Some(journal.new_head()), "ref must stay advanced");
        // The journal is durable at IndexReconciled: the edit hook runs at RefAdvanced
        // (lib.rs:4796), index reconciliation then succeeds and IndexReconciled is
        // persisted (lib.rs:4807-4809), and the managed-file conflict is raised while
        // materialising (lib.rs:4811). Recovery resumes from IndexReconciled.
        assert_eq!(
            repository.read_publication_journal().unwrap().unwrap().stage(),
            PublicationJournalStage::IndexReconciled
        );
        // The user's edited bytes are retained, not overwritten by the batch.
        assert_eq!(fs::read(root.path().join("main.orna")).unwrap(), b"module main;\n// edited!\n");
    }

    #[test]
    fn reviewed_candidate_commit_preserves_unstaged_work_and_replays_result() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), include_str!("../tests/fixtures/format-context/database-final.orna")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        let staged = b"module main;\n// admitted\n";
        fs::write(root.path().join("main.orna"), staged).unwrap();
        git(root.path(), &["add", "main.orna"]);
        fs::write(root.path().join("unrelated.txt"), b"unstaged").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([10; 16]), super::Uuid::from_bytes([11; 16]), 3,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(4), vec![0x81, 0x00],
        ).unwrap();
        let committed = repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions).unwrap();
        assert_eq!(repository.head().unwrap(), Some(committed.clone()));
        assert_eq!(repository.index_generation().unwrap().tree(), Some(&review.index_tree));
        assert_eq!(repository.read_committed_file(&committed, "main.orna", 1 << 20).unwrap(), staged);
        assert_eq!(fs::read(root.path().join("unrelated.txt")).unwrap(), b"unstaged");
        assert_eq!(
            git(root.path(), &["for-each-ref", "refs/orna/"]),
            "",
            "a completed reviewed commit leaves no private candidate ref behind"
        );
        assert!(!git(root.path(), &["status", "--porcelain"]).is_empty());
        git(root.path(), &["commit", "--allow-empty", "-m", "later unrelated commit"]);
        assert_ne!(repository.head().unwrap(), Some(committed.clone()));
        let replay = repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions).unwrap();
        assert_eq!(replay, committed);
    }

    #[test]
    fn reviewed_candidate_is_validated_before_any_commit_object_exists() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), include_str!("../tests/fixtures/format-context/database-final.orna")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        // The staged candidate drops the store root every format-3 snapshot has.
        git(root.path(), &["rm", "-r", "--cached", "--quiet", ".orna/store"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([50; 16]), super::Uuid::from_bytes([51; 16]), 2,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(3), vec![0x81, 0x00],
        ).unwrap();
        let objects = || {
            git(root.path(), &["cat-file", "--batch-all-objects", "--batch-check=%(objectname)"])
        };
        let objects_before = objects();
        let head_before = repository.head().unwrap();

        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "", &AcceptAllAssertions),
            Err(RepositoryError::InvalidCommitMessage)
        ));
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "drop the store root", &AcceptAllAssertions),
            Err(RepositoryError::InvalidCommitCandidate)
        ));
        assert_eq!(objects(), objects_before, "a refused candidate creates no Git object");
        assert_eq!(git(root.path(), &["for-each-ref", "refs/orna/"]), "");
        assert_eq!(repository.head().unwrap(), head_before);
    }

    #[test]
    fn reviewed_candidate_replay_requires_the_original_message() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), include_str!("../tests/fixtures/format-context/database-final.orna")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);

        fs::write(root.path().join("main.orna"), "module main;\n// admitted\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([10; 16]), super::Uuid::from_bytes([11; 16]), 3,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(4), vec![0x81, 0x00],
        ).unwrap();
        let committed = repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions).unwrap();

        let replay = repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions).unwrap();
        assert_eq!(replay, committed);
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "corrected message", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));
    }
    #[test]
    fn reviewed_candidate_retry_recovers_after_publish_before_result_write() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), include_str!("../tests/fixtures/format-context/database-final.orna")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        fs::write(root.path().join("main.orna"), "module main;\n// admitted\n").unwrap();
        git(root.path(), &["add", "main.orna"]);

        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([40; 16]), super::Uuid::from_bytes([41; 16]), 12,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(13), vec![0x81, 0x00],
        ).unwrap();
        let intent = super::reviewed_candidate_runtime_intent_id(&review, "reviewed");
        let old_head = repository.head().unwrap();
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            repository.commit_reviewed_candidate_with_stage_hook(
                &review, "reviewed", &AcceptAllAssertions,
                |stage| if stage == PublicationJournalStage::WorktreeReconciled { panic!("simulated crash before reviewed result recording"); },
            ).unwrap();
        }));
        assert!(crashed.is_err());
        let published = repository.head().unwrap().unwrap();
        assert_ne!(Some(published.clone()), old_head);
        let journal = repository.read_publication_journal().unwrap().unwrap();
        assert_eq!(journal.stage(), PublicationJournalStage::WorktreeReconciled);
        assert_eq!(journal.runtime_intent_id(), Some(intent));
        let result_path = repository.runtime.root().join(format!("reviewed-result-{}", intent.iter().map(|byte| format!("{byte:02x}")).collect::<String>()));
        assert!(!result_path.exists());

        let retry_review = repository.review_staged_candidate(
            super::Uuid::from_bytes([40; 16]), super::Uuid::from_bytes([41; 16]), 12,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(13), vec![0x81, 0x00],
        ).unwrap();
        assert!(retry_review.candidate_paths.is_empty());
        assert_eq!(super::reviewed_candidate_runtime_intent_id(&retry_review, "reviewed"), intent);
        let retried = repository.commit_reviewed_candidate(&retry_review, "reviewed", &AcceptAllAssertions).unwrap();
        assert_eq!(retried, published);
        assert!(repository.read_publication_journal().unwrap().is_none());
        assert!(result_path.exists());
    }
    #[test]
    fn reviewed_candidate_retries_after_index_lock_contention() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/database.orna"), include_str!("../tests/fixtures/format-context/database-final.orna")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"store root one").unwrap();
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-m", "initial"]);
        fs::write(root.path().join("main.orna"), "module main;\n// lock retry\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([20; 16]), super::Uuid::from_bytes([21; 16]), 5,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(6), vec![0x81, 0x00],
        ).unwrap();
        let index = repository.git_path("index").unwrap();
        let lock = index.with_extension("lock");
        fs::write(&lock, b"occupied").unwrap();
        assert!(repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions).is_err());
        fs::remove_file(&lock).unwrap();
        assert!(repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions).is_ok());
        assert!(repository.read_publication_journal().unwrap().is_none());
    }

    #[test]
    fn reviewed_candidate_empty_commit_returns_nothing_to_commit() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);
        let repository = Repository::discover(root.path()).unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([30; 16]), super::Uuid::from_bytes([31; 16]), 8,
            super::RepositoryFormat::Format3, RuntimeGeneration::new(9), vec![0x81, 0x00],
        ).unwrap();
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::SysGitNothingToCommit)
        ));
    }

     #[test]
    fn reviewed_candidate_rejects_forged_and_stale_index_reviews() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);
        fs::write(root.path().join("main.orna"), "module main;\n// staged\n").unwrap();
        git(root.path(), &["add", "main.orna"]);

        let repository = Repository::discover(root.path()).unwrap();
        let head_before = repository.head().unwrap();
        let index_before = repository.index_generation().unwrap();
        let worktree_before = repository.worktree_state().unwrap();
        let review = repository.review_staged_candidate(
            super::Uuid::from_bytes([1; 16]),
            super::Uuid::from_bytes([2; 16]),
            7,
            super::RepositoryFormat::Format3,
            RuntimeGeneration::new(9),
            vec![0x81, 0x00],
        ).unwrap();
        assert_eq!(repository.head().unwrap(), head_before);
        assert_eq!(repository.index_generation().unwrap(), index_before);
        assert_eq!(repository.worktree_state().unwrap(), worktree_before);
        assert_eq!(review.index_tree, index_before.tree().unwrap().clone());
        assert_ne!(review.token(), &[0; 32]);

        let mut forged = review.clone();
        forged.pending_generation += 1;
        assert!(matches!(
            repository.commit_reviewed_candidate(&forged, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));

        fs::write(root.path().join("main.orna"), "module main;\n// newer staged\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        assert!(matches!(
            repository.commit_reviewed_candidate(&review, "reviewed", &AcceptAllAssertions),
            Err(RepositoryError::StaleReviewedCandidate)
        ));
        assert_eq!(repository.head().unwrap(), head_before);
    }

    /// ORNA-PUB-010: an unchanged `.orna/store/data` node leaf that was edited in the worktree
    /// is an external change and must refuse the reconcile rather than be left in place.
    #[test]
    fn reconcile_refuses_edited_unchanged_store_data_leaf() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.path().join(".orna/store")).unwrap();
        fs::write(root.path().join(".orna/store/data"), b"committed\n").unwrap();
        git(root.path(), &["add", ".orna/store/data"]);
        git(root.path(), &["commit", "-m", "base"]);

        let repository = Repository::discover(root.path()).unwrap();
        let expected = repository.index_generation().unwrap();
        let candidate = super::PrivateCommit {
            tree: expected.tree().unwrap().clone(),
            commit: expected.head().unwrap().clone(),
        };
        fs::write(root.path().join(".orna/store/data"), b"edited\n").unwrap();

        assert!(matches!(
            repository.reconcile_store_worktree_transition(&expected, &candidate, false),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(fs::read(root.path().join(".orna/store/data")).unwrap(), b"edited\n");
    }

     #[test]
    fn deletion_preserves_editor_race_after_final_check() {
        let root = tempfile::TempDir::new().unwrap();
        git(root.path(), &["init", "-b", "main"]);
        crate::test_support::configure_fixture_git_identity(root.path());
        git(root.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(root.path().join("main.orna"), "module main;\n").unwrap();
        git(root.path(), &["add", "main.orna"]);
        git(root.path(), &["commit", "-m", "initial"]);

        let repository = Repository::discover(root.path()).unwrap();
        let path = super::ManagedPath::new("generated/row.orna").unwrap();
        repository
            .materialize_managed_file(&path, None, Some(b"first"))
            .unwrap();
        let target = root.path().join(path.as_path());
        let mut calls = 0;
        let mut editor_race = || {
            calls += 1;
            if calls == 2 {
                fs::write(&target, b"editor").unwrap();
            }
        };

        assert!(matches!(
            repository.materialize_managed_file_impl(
                &path,
                Some(b"first"),
                None,
                Some(&mut editor_race),
            ),
            Err(RepositoryError::ManagedContentConflict)
        ));
        assert_eq!(calls, 2);
        assert_eq!(fs::read(&target).unwrap(), b"editor");
        repository
            .materialize_managed_file(&path, Some(b"editor"), None)
            .unwrap();
        assert!(!target.exists());
    }
}

#[cfg(test)]
mod git_binding_tests {
    use super::bind_git_discovery_to_worktree;
    use std::process::Command;

    #[test]
    fn discovery_bind_makes_a_relative_ceiling_absolute() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir_all(child.join(".git")).unwrap();
        let mut command = Command::new("git");
        bind_git_discovery_to_worktree(&mut command, &child);
        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CEILING_DIRECTORIES")
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("discovery ceiling is bound");
        assert!(ceiling.is_absolute());
        assert_eq!(ceiling, root.path().canonicalize().unwrap());
    }

    #[test]
    fn discovery_bind_leaves_a_plain_subdirectory_free_to_discover_its_repository() {
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join(".orna");
        std::fs::create_dir_all(&plain).unwrap();
        let mut command = Command::new("git");
        bind_git_discovery_to_worktree(&mut command, &plain);
        assert!(
            command
                .get_envs()
                .all(|(name, _)| name != "GIT_CEILING_DIRECTORIES")
        );
    }

    /// A relative worktree path must still bind to an absolute ceiling. The root
    /// is created inside the process directory so its path can be given relative.
    #[test]
    fn discovery_bind_absolutizes_a_relative_worktree_path() {
        let cwd = std::env::current_dir().unwrap();
        let root = tempfile::Builder::new().tempdir_in(&cwd).unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let relative = std::path::PathBuf::from(root.path().file_name().unwrap());
        let mut command = Command::new("git");
        bind_git_discovery_to_worktree(&mut command, &relative);
        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CEILING_DIRECTORIES")
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("discovery ceiling is bound");
        assert!(ceiling.is_absolute());
        assert_eq!(ceiling, cwd.canonicalize().unwrap());
    }

    /// The unconditional ceiling binds a plain directory's parent, with no `.git`.
    #[test]
    fn unconditional_ceiling_binds_a_plain_directory_parent() {
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let mut command = Command::new("git");
        super::bind_git_ceiling_above(&mut command, &plain);
        let ceiling = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CEILING_DIRECTORIES")
            .and_then(|(_, value)| value)
            .map(std::path::PathBuf::from)
            .expect("discovery ceiling is bound");
        assert_eq!(ceiling, root.path().canonicalize().unwrap());
    }
}
