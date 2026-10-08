//! Repository-owned format admission and pinned metadata context.
//!
//! This module is deliberately narrower than the repository's storage and
//! graph implementations. It admits the tracked format coordinate, keeps
//! legacy readers explicitly read-only, and issues opaque snapshot-bound
//! handles for later value/schema/store work. It does not write rows, convert
//! data, or interpret native graph objects.

use std::{fmt, process::Command, str::FromStr};

use orna_syntax_v1::{Expr, LiteralKind, RecordField, parse_row};
use sha2::{Digest, Sha256};

use crate::{Repository, RepositoryError};
use crate::{
    native_graph::{GitHashAlgorithm, GraphError, NativeGraphContext, NativeOid, RepositoryCaptureCapability},
    row_store::RowMapSnapshot,
};

use super::DatabaseId;

/// The final repository writer coordinate from the consolidated publication.
pub const FINAL_REPOSITORY_FORMAT: u8 = 3;

/// The bounded size of the tracked `database.orna` record.
pub const FORMAT_CONTEXT_MAX_METADATA_BYTES: usize = 64 * 1024;

const DATABASE_PATH: &str = ".orna/database.orna";
const LEGACY_FORMAT_PATH: &str = ".orna/format.orna";
const MAIN_SOURCE_PATH: &str = "main.orna";
// Used only to classify a failed metadata-path lookup. The valid format-3
// store path never recursively enumerates this tree.
const MAX_TREE_ENTRIES_FOR_METADATA_DIAGNOSTIC: usize = 65_536;
const MAX_METADATA_READ_BYTES: usize = FORMAT_CONTEXT_MAX_METADATA_BYTES * 2;

/// Repository-format dispatch selected from tracked metadata.
///
/// `Legacy1` and `Legacy2` are compatibility reader inputs only. They are not
/// relabelled as format 3 and cannot authorize a new-format write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryFormat {
    Legacy1,
    Legacy2,
    Format3,
}

impl RepositoryFormat {
    /// The recorded numeric repository coordinate.
    pub const fn number(self) -> u8 {
        match self {
            Self::Legacy1 => 1,
            Self::Legacy2 => 2,
            Self::Format3 => FINAL_REPOSITORY_FORMAT,
        }
    }

    /// Whether this context is a compatibility reader and therefore
    /// explicitly read-only.
    pub const fn is_read_only(self) -> bool {
        !matches!(self, Self::Format3)
    }

    /// Whether the selected coordinate admits format-3 writes.
    pub const fn supports_writes(self) -> bool {
        matches!(self, Self::Format3)
    }
}

/// Fail-closed outcomes for repository format admission and root seams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatContextError {
    MetadataUnavailable,
    MetadataInvalid,
    UnknownFormat,
    MixedFormats,
    SnapshotUnavailable,
    SnapshotInvalid,
    PinMismatch,
    SchemaRootUnavailable,
    SchemaRootInvalid,
    StoreRootUnavailable,
    StoreRootInvalid,
    RowMapMismatch,
    GraphContextUnavailable,
    GraphContextInvalid,
}

impl FormatContextError {
    /// Stable diagnostic code without paths, Git commands, or object IDs.
    pub const fn code(self) -> &'static str {
        match self {
            Self::MetadataUnavailable => "ORNA-REPO-CONTEXT-001",
            Self::MetadataInvalid => "ORNA-REPO-CONTEXT-002",
            Self::UnknownFormat => "ORNA-REPO-CONTEXT-003",
            Self::MixedFormats => "ORNA-REPO-CONTEXT-004",
            Self::SnapshotUnavailable => "ORNA-REPO-CONTEXT-005",
            Self::SnapshotInvalid => "ORNA-REPO-CONTEXT-006",
            Self::PinMismatch => "ORNA-REPO-CONTEXT-007",
            Self::SchemaRootUnavailable => "ORNA-REPO-CONTEXT-008",
            Self::SchemaRootInvalid => "ORNA-REPO-CONTEXT-009",
            Self::StoreRootUnavailable => "ORNA-REPO-CONTEXT-010",
            Self::StoreRootInvalid => "ORNA-REPO-CONTEXT-011",
            Self::RowMapMismatch => "ORNA-REPO-CONTEXT-012",
            Self::GraphContextUnavailable => "ORNA-REPO-CONTEXT-013",
            Self::GraphContextInvalid => "ORNA-REPO-CONTEXT-014",
        }
    }
}

impl fmt::Display for FormatContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MetadataUnavailable => "repository metadata is unavailable",
            Self::MetadataInvalid => "repository metadata is invalid",
            Self::UnknownFormat => "repository format is unknown",
            Self::MixedFormats => "repository metadata mixes format authorities",
            Self::SnapshotUnavailable => "repository snapshot is unavailable",
            Self::SnapshotInvalid => "repository snapshot is invalid",
            Self::PinMismatch => "snapshot pin belongs to another repository",
            Self::SchemaRootUnavailable => "repository schema root is unavailable",
            Self::SchemaRootInvalid => "repository schema root is invalid",
            Self::StoreRootUnavailable => "repository store root is unavailable",
            Self::StoreRootInvalid => "repository store root is invalid",
            Self::RowMapMismatch => "row map does not belong to the pinned format-3 context",
            Self::GraphContextUnavailable => "native graph repository data is unavailable",
            Self::GraphContextInvalid => "native graph repository data is invalid",
        })
    }
}

impl std::error::Error for FormatContextError {}

/// An immutable Git snapshot selected through the owning [`Repository`].
///
/// The native commit ID is intentionally not exposed here. Consumers receive
/// this handle from repository validation and pass it back to repository-owned
/// APIs; a caller cannot construct snapshot authority from a raw OID.
#[derive(Clone)]
pub struct RepositorySnapshotPin {
    repository: Repository,
    commit: crate::GitCommitRef,
    private_candidate: Option<crate::PrivateCommit>,
}

impl fmt::Debug for RepositorySnapshotPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositorySnapshotPin")
            .finish_non_exhaustive()
    }
}

impl RepositorySnapshotPin {
    fn belongs_to(&self, repository: &Repository) -> bool {
        self.repository.worktree() == repository.worktree()
            && self.repository.runtime_paths().root() == repository.runtime_paths().root()
    }

    fn read_file(
        &self,
        repository: &Repository,
        path: impl AsRef<std::path::Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RepositoryError> {
        match &self.private_candidate {
            Some(candidate) => repository.read_private_candidate_file(candidate, path, max_bytes),
            None => repository.read_committed_file(&self.commit, path, max_bytes),
        }
    }
}

/// Opaque proof that the pinned `main.orna` source/schema root was present in
/// the admitted snapshot. The native commit identity remains repository-owned.
#[derive(Clone, Debug)]
pub struct SchemaRootPin {
    snapshot: RepositorySnapshotPin,
    oid: NativeOid,
    digest: [u8; 32],
}

/// Opaque proof that the pinned `.orna/store` tree had a valid native-tree
/// entry shape. Graph decoding is intentionally outside this slice.
#[derive(Clone, Debug)]
pub struct StoreRootPin {
    snapshot: RepositorySnapshotPin,
    oid: NativeOid,
}

/// A validated, immutable repository metadata context.
///
/// The only public issuance path is [`Repository::open_format_context`]. The
/// type retains the actual database identity, the selected format dispatch,
/// and the immutable snapshot pin used to admit the metadata. Snapshot
/// selection and alternate-snapshot parsing stay crate-internal so callers
/// cannot manufacture a context from a raw selector or object ID.
pub struct RepositoryFormatContext {
    repository: Repository,
    format: RepositoryFormat,
    database_id: Option<DatabaseId>,
    snapshot: RepositorySnapshotPin,
}

impl fmt::Debug for RepositoryFormatContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryFormatContext")
            .field("format", &self.format)
            .field("database_id", &self.database_id)
            .finish_non_exhaustive()
    }
}

impl RepositoryFormatContext {
    /// The selected repository-format dispatch.
    pub const fn repository_format(&self) -> RepositoryFormat {
        self.format
    }

    /// The numeric coordinate, useful to adapters that do not need the enum.
    pub const fn repository_format_number(&self) -> u8 {
        self.format.number()
    }

    /// The admitted stable database identity, when the source format carries
    /// one. Legacy repositories without the historical sidecar remain
    /// readable but report identity as unavailable rather than fabricating it.
    pub const fn database_id(&self) -> Option<DatabaseId> {
        self.database_id
    }

    /// Requires an actual database identity without inventing one for a
    /// legacy input that did not record it.
    pub fn require_database_id(&self) -> Result<DatabaseId, FormatContextError> {
        self.database_id
            .ok_or(FormatContextError::MetadataUnavailable)
    }

    /// Whether the selected context is a legacy read-only input.
    pub const fn is_read_only(&self) -> bool {
        self.format.is_read_only()
    }

    /// Whether format-3 write admission is available. This is only a format
    /// capability; row/publication admission remains a later repository seam.
    pub const fn supports_writes(&self) -> bool {
        self.format.supports_writes()
    }

    /// The opaque immutable snapshot pin retained by this context.
    pub fn snapshot_pin(&self) -> &RepositorySnapshotPin {
        &self.snapshot
    }

    /// A non-authoritative schema/root validation seam for later values and
    /// row layers. It validates only the fixed source root in this slice.
    pub fn validate_schema_root(&self) -> Result<SchemaRootPin, FormatContextError> {
        self.require_format3()?;
        let source = match self
            .snapshot
            .read_file(&self.repository, MAIN_SOURCE_PATH, 64 * 1024)
        {
            Ok(source) => source,
            Err(RepositoryError::GitUnavailable) => {
                return Err(FormatContextError::SchemaRootUnavailable);
            }
            Err(_) => return Err(FormatContextError::SchemaRootInvalid),
        };
        let algorithm = repository_hash_algorithm(&self.repository)?;
        let oid = committed_path_oid(
            &self.repository,
            &self.snapshot.commit,
            algorithm,
            MAIN_SOURCE_PATH,
            "blob",
        )
        .map_err(|error| match error {
            FormatContextError::GraphContextUnavailable => {
                FormatContextError::SchemaRootUnavailable
            }
            _ => FormatContextError::SchemaRootInvalid,
        })?;
        Ok(SchemaRootPin {
            snapshot: self.snapshot.clone(),
            oid,
            digest: Sha256::digest(&source).into(),
        })
    }

    /// Pins the committed `.orna/store` native tree without recursively
    /// enumerating it. OGS-1 validates reachable node envelopes; recursively
    /// listing the complete graph here would make large row maps unopenable.
    pub fn validate_store_root(&self) -> Result<StoreRootPin, FormatContextError> {
        self.require_format3()?;
        let algorithm = repository_hash_algorithm(&self.repository)?;
        let oid = committed_path_oid(
            &self.repository,
            &self.snapshot.commit,
            algorithm,
            ".orna/store",
            "tree",
        )
        .map_err(|error| match error {
            FormatContextError::GraphContextUnavailable => FormatContextError::StoreRootUnavailable,
            _ => FormatContextError::StoreRootInvalid,
        })?;
        Ok(StoreRootPin {
            snapshot: self.snapshot.clone(),
            oid,
        })
    }

    /// Resolves one stable relation ID through the committed format-3 store
    /// root and seals its schema/root/count identity without loading table
    /// rows. Point/range access is available only through the matching
    /// repository-issued `NativeGraphContext`.
    pub fn load_row_map(
        &self,
        relation_id: [u8; 16],
    ) -> Result<RowMapSnapshot, FormatContextError> {
        self.require_format3()?;
        let database_id = self.require_database_id()?;
        self.validate_schema_root()?;
        let store = self.validate_store_root()?;
        let mut snapshot_hash = Sha256::new();
        snapshot_hash.update(b"orna.repository.graph.snapshot.v1\0");
        snapshot_hash.update(self.snapshot.commit.as_str().as_bytes());
        let snapshot_id: [u8; 32] = snapshot_hash.finalize().into();
        crate::native_graph::load_format3_row_map(
            &self.repository,
            store.oid.algorithm(),
            store.oid,
            *database_id.as_bytes(),
            snapshot_id,
            relation_id,
        )
        .map_err(|_| FormatContextError::GraphContextInvalid)
    }

    /// Issues native Git graph authority only for a sealed row-map snapshot
    /// whose database, schema identity and store root match this pinned
    /// format-3 repository snapshot.
    pub fn open_native_graph(
        &self,
        rows: &RowMapSnapshot,
    ) -> Result<NativeGraphContext, FormatContextError> {
        self.require_format3()?;
        let database_id = self.require_database_id()?;
        self.validate_schema_root()?;
        let store = self.validate_store_root()?;
        let version = rows.version();
        if version.database_id() != database_id.as_bytes()
            || version.schema().database_id() != database_id.as_bytes()
            || version.store_root() != &store.oid
        {
            return Err(FormatContextError::RowMapMismatch);
        }
        let schema_digest = *version.schema().schema_digest();
        let algorithm = store.oid.algorithm();
        if version.schema().schema_oid().algorithm() != algorithm {
            return Err(FormatContextError::GraphContextInvalid);
        }
        let repository_path = self
            .repository
            .worktree()
            .canonicalize()
            .map_err(|_| FormatContextError::GraphContextUnavailable)?;
        let mut repository_hash = Sha256::new();
        repository_hash.update(b"orna.repository.graph.repository.v1\0");
        repository_hash.update(repository_path.to_string_lossy().as_bytes());
        repository_hash.update(database_id.as_bytes());
        let repository_id: [u8; 32] = repository_hash.finalize().into();

        let mut snapshot_hash = Sha256::new();
        snapshot_hash.update(b"orna.repository.graph.snapshot.v1\0");
        snapshot_hash.update(self.snapshot.commit.as_str().as_bytes());
        let snapshot_id: [u8; 32] = snapshot_hash.finalize().into();

        let authority = rows.authority();
        let mut owner_hash = Sha256::new();
        owner_hash.update(b"orna.repository.graph.owner.v1\0");
        owner_hash.update(authority);
        let owner_digest = owner_hash.finalize();
        let mut owner_id = [0u8; 16];
        owner_id.copy_from_slice(&owner_digest[..16]);

        let mut identity_hash = Sha256::new();
        identity_hash.update(b"orna.repository.graph.context.v1\0");
        identity_hash.update(repository_id);
        identity_hash.update(database_id.as_bytes());
        identity_hash.update(owner_id);
        identity_hash.update(snapshot_id);
        identity_hash.update(store.oid.as_bytes());
        identity_hash.update(schema_digest);
        identity_hash.update(authority);
        let identity: [u8; 32] = identity_hash.finalize().into();

        NativeGraphContext::issue(
            self.repository.clone(),
            repository_id,
            identity,
            *database_id.as_bytes(),
            owner_id,
            snapshot_id,
            algorithm,
            store.oid,
            schema_digest,
            rows.clone(),
        )
        .map_err(|_| FormatContextError::GraphContextInvalid)
    }

    fn require_format3(&self) -> Result<(), FormatContextError> {
        if self.format.supports_writes() {
            Ok(())
        } else {
            Err(FormatContextError::UnknownFormat)
        }
    }
}

impl SchemaRootPin {
    /// The snapshot pin from which this schema proof was admitted.
    pub fn snapshot_pin(&self) -> &RepositorySnapshotPin {
        &self.snapshot
    }
}

impl StoreRootPin {
    /// The snapshot pin from which this store proof was admitted.
    pub fn snapshot_pin(&self) -> &RepositorySnapshotPin {
        &self.snapshot
    }
}

impl Repository {
    /// Pins a reachable Git selector without exposing its native object ID.
    pub(crate) fn pin_snapshot(
        &self,
        selector: &str,
    ) -> Result<RepositorySnapshotPin, FormatContextError> {
        let commit = self
            .resolve_snapshot(selector)
            .map_err(map_snapshot_error)?;
        Ok(RepositorySnapshotPin {
            repository: self.clone(),
            commit,
            private_candidate: None,
        })
    }

    /// Opens metadata for a private candidate while it is still unreachable.
    /// The candidate capability is repository-issued and validation reads its
    /// exact private tree; no public snapshot pin is created or exposed.
    pub(crate) fn open_private_candidate_format_context(
        &self,
        candidate: &crate::PrivateCommit,
    ) -> Result<RepositoryFormatContext, FormatContextError> {
        self.verify_private_candidate(candidate)
            .map_err(map_snapshot_error)?;
        let snapshot = RepositorySnapshotPin {
            repository: self.clone(),
            commit: candidate.commit().clone(),
            private_candidate: Some(candidate.clone()),
        };
        self.open_format_context_at(&snapshot)
    }

    /// Opens and validates the format metadata at the current immutable HEAD.
    /// Worktree-only metadata is not silently treated as a committed snapshot.
    pub fn open_format_context(&self) -> Result<RepositoryFormatContext, FormatContextError> {
        let snapshot = self.pin_snapshot("HEAD")?;
        self.open_format_context_at(&snapshot)
    }

    /// Issues capture authority for one format-3 relation at the current
    /// HEAD. The row map, native graph, and read scope all come from this
    /// repository's own format context, so callers supply only the relation.
    pub fn capture_capability(
        &self,
        relation_id: [u8; 16],
    ) -> Result<RepositoryCaptureCapability, CaptureCapabilityError> {
        let format = self.open_format_context().map_err(CaptureCapabilityError::Format)?;
        let row_map = format
            .load_row_map(relation_id)
            .map_err(CaptureCapabilityError::Format)?;
        let graph = std::sync::Arc::new(
            format
                .open_native_graph(&row_map)
                .map_err(CaptureCapabilityError::Format)?,
        );
        let scope = graph.open_read_scope().map_err(CaptureCapabilityError::Graph)?;
        RepositoryCaptureCapability::new(graph, scope).map_err(CaptureCapabilityError::Graph)
    }

    /// Opens and validates metadata at a previously repository-pinned
    /// snapshot. The pin must belong to this repository instance.
    pub(crate) fn open_format_context_at(
        &self,
        snapshot: &RepositorySnapshotPin,
    ) -> Result<RepositoryFormatContext, FormatContextError> {
        if !snapshot.belongs_to(self) {
            return Err(FormatContextError::PinMismatch);
        }
        let database = read_metadata_file(self, snapshot, DATABASE_PATH)?;
        let legacy_format = read_metadata_file(self, snapshot, LEGACY_FORMAT_PATH)?;
        parse_context(self.clone(), snapshot.clone(), database, legacy_format)
    }
}

fn repository_hash_algorithm(
    repository: &Repository,
) -> Result<GitHashAlgorithm, FormatContextError> {
    let output = repository_git_output(repository, &["rev-parse", "--show-object-format=storage"])?;
    match std::str::from_utf8(&output)
        .map_err(|_| FormatContextError::GraphContextInvalid)?
        .trim()
    {
        "sha1" => Ok(GitHashAlgorithm::Sha1),
        "sha256" => Ok(GitHashAlgorithm::Sha256),
        _ => Err(FormatContextError::GraphContextInvalid),
    }
}

fn committed_path_oid(
    repository: &Repository,
    commit: &crate::GitCommitRef,
    algorithm: GitHashAlgorithm,
    path: &str,
    expected_kind: &str,
) -> Result<NativeOid, FormatContextError> {
    let object_spec = format!("{}:{path}", commit.as_str());
    let output = repository_git_output(repository, &["rev-parse", "--verify", &object_spec])?;
    let hex = std::str::from_utf8(&output)
        .map_err(|_| FormatContextError::GraphContextInvalid)?
        .trim();
    let oid =
        NativeOid::from_hex(algorithm, hex).map_err(|_| FormatContextError::GraphContextInvalid)?;
    let kind_output = repository_git_output(repository, &["cat-file", "-t", &oid.to_hex()])?;
    if std::str::from_utf8(&kind_output)
        .map_err(|_| FormatContextError::GraphContextInvalid)?
        .trim()
        != expected_kind
    {
        return Err(FormatContextError::GraphContextInvalid);
    }
    Ok(oid)
}

fn repository_git_output(
    repository: &Repository,
    args: &[&str],
) -> Result<Vec<u8>, FormatContextError> {
    let output = Command::new("git")
        .current_dir(repository.worktree())
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .map_err(|_| FormatContextError::GraphContextUnavailable)?;
    if !output.status.success() {
        return Err(FormatContextError::GraphContextUnavailable);
    }
    Ok(output.stdout)
}

fn map_snapshot_error(error: RepositoryError) -> FormatContextError {
    match error {
        RepositoryError::GitUnavailable
        | RepositoryError::SnapshotNotFound
        | RepositoryError::UnbornHead => FormatContextError::SnapshotUnavailable,
        _ => FormatContextError::SnapshotInvalid,
    }
}

fn read_metadata_file(
    repository: &Repository,
    snapshot: &RepositorySnapshotPin,
    path: &str,
) -> Result<Option<Vec<u8>>, FormatContextError> {
    match snapshot.read_file(repository, path, MAX_METADATA_READ_BYTES) {
        Ok(bytes) if bytes.len() <= FORMAT_CONTEXT_MAX_METADATA_BYTES => Ok(Some(bytes)),
        Ok(_) => Err(FormatContextError::MetadataInvalid),
        Err(RepositoryError::GitUnavailable) => Err(FormatContextError::MetadataUnavailable),
        // File readers redact absence. Inspect the selected tree before
        // treating a missing metadata path as a supported layout.
        Err(_) => {
            let path_is_present = if let Some(candidate) = &snapshot.private_candidate {
                let managed_path = crate::ManagedPath::new(path)
                    .map_err(|_| FormatContextError::MetadataInvalid)?;
                repository
                    .candidate_tree_entry(candidate, &managed_path)
                    .map_err(|_| FormatContextError::MetadataInvalid)?
                    .is_some()
            } else {
                repository
                    .list_committed_tree(&snapshot.commit, MAX_TREE_ENTRIES_FOR_METADATA_DIAGNOSTIC)
                    .map_err(|error| match error {
                        RepositoryError::GitUnavailable => FormatContextError::MetadataUnavailable,
                        _ => FormatContextError::MetadataInvalid,
                    })?
                    .into_iter()
                    .any(|entry| {
                        entry.path().as_path().to_str().is_some_and(|entry_path| {
                            entry_path == path
                                || entry_path
                                    .strip_prefix(path)
                                    .is_some_and(|suffix| suffix.starts_with('/'))
                        })
                    })
            };
            if path_is_present {
                Err(FormatContextError::MetadataInvalid)
            } else {
                Ok(None)
            }
        }
    }
}

fn parse_context(
    repository: Repository,
    snapshot: RepositorySnapshotPin,
    database: Option<Vec<u8>>,
    legacy_format: Option<Vec<u8>>,
) -> Result<RepositoryFormatContext, FormatContextError> {
    let legacy = legacy_format
        .as_deref()
        .map(parse_legacy_format)
        .transpose()?;

    match (database.as_deref(), legacy) {
        (None, None) => Err(FormatContextError::MetadataUnavailable),
        (None, Some(format)) => Ok(RepositoryFormatContext {
            repository,
            format,
            database_id: None,
            snapshot,
        }),
        (Some(database), None) => match parse_database_record(database)? {
            DatabaseRecord::Format3(database_id) => Ok(RepositoryFormatContext {
                repository,
                format: RepositoryFormat::Format3,
                database_id: Some(database_id),
                snapshot,
            }),
            DatabaseRecord::LegacySidecar(_) => Err(FormatContextError::MetadataInvalid),
        },
        (Some(database), Some(format)) => match parse_database_record(database)? {
            DatabaseRecord::Format3(_) => Err(FormatContextError::MixedFormats),
            DatabaseRecord::LegacySidecar(database_id) => Ok(RepositoryFormatContext {
                repository,
                format,
                database_id: Some(database_id),
                snapshot,
            }),
        },
    }
}

enum DatabaseRecord {
    Format3(DatabaseId),
    LegacySidecar(DatabaseId),
}

fn parse_database_record(bytes: &[u8]) -> Result<DatabaseRecord, FormatContextError> {
    let fields = parse_record(bytes).map_err(|_| FormatContextError::MetadataInvalid)?;
    if fields.len() == 1 && fields[0].name == "database_id" {
        return Ok(DatabaseRecord::LegacySidecar(parse_database_id(
            &fields[0].value,
        )?));
    }
    if fields.len() != 2
        || fields
            .iter()
            .any(|field| field.name != "repository_format" && field.name != "database_id")
        || fields
            .iter()
            .filter(|field| field.name == "repository_format")
            .count()
            != 1
        || fields
            .iter()
            .filter(|field| field.name == "database_id")
            .count()
            != 1
    {
        return Err(FormatContextError::MetadataInvalid);
    }
    let repository_format = fields
        .iter()
        .find(|field| field.name == "repository_format")
        .and_then(|field| integer_literal(&field.value))
        .ok_or(FormatContextError::MetadataInvalid)?;
    if repository_format != i64::from(FINAL_REPOSITORY_FORMAT) {
        return Err(FormatContextError::UnknownFormat);
    }
    let database_id = fields
        .iter()
        .find(|field| field.name == "database_id")
        .ok_or(FormatContextError::MetadataInvalid)
        .and_then(|field| parse_database_id(&field.value))?;
    if canonical_database_bytes(&database_id).as_slice() != bytes {
        return Err(FormatContextError::MetadataInvalid);
    }
    Ok(DatabaseRecord::Format3(database_id))
}

pub(super) fn parse_legacy_format(bytes: &[u8]) -> Result<RepositoryFormat, FormatContextError> {
    let source = std::str::from_utf8(bytes).map_err(|_| FormatContextError::MetadataInvalid)?;
    let line = source
        .strip_suffix('\n')
        .unwrap_or(source)
        .strip_suffix('\r')
        .unwrap_or_else(|| source.strip_suffix('\n').unwrap_or(source));
    match line {
        "format 1" => return Ok(RepositoryFormat::Legacy1),
        "format 2" => return Ok(RepositoryFormat::Legacy2),
        "format 3" => return Err(FormatContextError::UnknownFormat),
        _ => {}
    }

    // The final publication names legacy reader coordinates as format 1/2,
    // but does not enumerate any legacy `storage_profile` pair. Do not import
    // historical draft/current-writer profile names into final admission.
    Err(FormatContextError::MetadataInvalid)
}

fn parse_database_id(value: &Expr) -> Result<DatabaseId, FormatContextError> {
    let text = string_literal(value).ok_or(FormatContextError::MetadataInvalid)?;
    DatabaseId::from_str(text).map_err(|_| FormatContextError::MetadataInvalid)
}

/// The one shared bounded Orna-record parser used by repository metadata
/// initialization and this format-context admission path.
pub(super) fn parse_record(bytes: &[u8]) -> Result<Vec<RecordField>, ()> {
    let source = std::str::from_utf8(bytes).map_err(|_| ())?;
    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return Err(());
    }
    match parsed.value {
        Expr::Record { fields, .. } => Ok(fields),
        _ => Err(()),
    }
}

pub(super) fn integer_literal(value: &Expr) -> Option<i64> {
    let Expr::Literal {
        text,
        kind: LiteralKind::Integer,
        ..
    } = value
    else {
        return None;
    };
    text.parse().ok()
}

pub(super) fn string_literal(value: &Expr) -> Option<&str> {
    let Expr::Literal {
        text,
        kind: LiteralKind::String,
        ..
    } = value
    else {
        return None;
    };
    text.strip_prefix('"')?.strip_suffix('"')
}

pub(super) fn canonical_database_bytes(database_id: &DatabaseId) -> Vec<u8> {
    format!(
        "{{\n    repository_format: {FINAL_REPOSITORY_FORMAT},\n    database_id: \"{database_id}\",\n}}\n"
    )
    .into_bytes()
}

pub(super) fn parse_canonical_database(bytes: &[u8]) -> Result<DatabaseId, ()> {
    match parse_database_record(bytes).map_err(|_| ())? {
        DatabaseRecord::Format3(database_id) => Ok(database_id),
        DatabaseRecord::LegacySidecar(_) => Err(()),
    }
}

#[cfg(test)]
mod graph_bridge_tests {
    use std::{
        fs,
        io::Write,
        path::Path,
        process::{Command, Stdio},
    };

    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use crate::{
        Repository,
        native_graph::{
            ByteIndexEntry, GitHashAlgorithm, MAX_RANGE_GRAPH_OBJECTS, NativeOid, NodeData,
            OrderedBranchEntry, OrderedLeafEntry,
        },
        row_store::{
            KeyRange, RowMapSnapshot, RowMapVersion, RowValue, SchemaGeneration, TypedKey,
        },
    };

    use super::RepositoryFormatContext;

    const DATABASE: &str = include_str!("../tests/fixtures/format-context/database-final.orna");
    const DATABASE_TEMPLATE: &str =
        include_str!("../tests/fixtures/format-context/database-template.orna");
    const MAIN_SOURCE: &str = include_str!("../tests/fixtures/git-repository-main.orna");
    const DATABASE_PLACEHOLDER: &str = "00000000-0000-4000-8000-000000000000";
    const OTHER_DATABASE_ID: &str = "a4a0a7d1-4f5c-4dc4-a5bf-f3f7f6a8d7e1";

    fn git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(directory: &Path, arguments: &[&str], input: Option<&[u8]>) -> Vec<u8> {
        let mut child = Command::new("git")
            .current_dir(directory)
            .args(arguments)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn git plumbing command");
        if let Some(input) = input {
            child
                .stdin
                .take()
                .expect("git plumbing stdin")
                .write_all(input)
                .expect("write git plumbing input");
        }
        let output = child.wait_with_output().expect("wait for git plumbing");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn repository() -> TempDir {
        let directory = TempDir::new().expect("create graph bridge repository");
        let root = directory.path();
        git(
            root,
            &["init", "--quiet", "--initial-branch=main", "--template="],
        );
        git(root, &["config", "user.name", "kierandrewett"]);
        git(root, &["config", "user.email", "kieran@drewett.dev"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(root.join(".orna/store")).expect("create store tree");
        fs::write(root.join("main.orna"), MAIN_SOURCE).expect("write source fixture");
        fs::write(root.join(".orna/database.orna"), DATABASE).expect("write database fixture");
        fs::write(root.join(".orna/store/data"), b"store root one")
            .expect("write first store marker");
        git(root, &["add", "."]);
        git(root, &["commit", "--quiet", "-m", "graph bridge fixture"]);
        directory
    }

    fn commit(directory: &Path) {
        git(directory, &["add", "."]);
        git(
            directory,
            &["commit", "--quiet", "-m", "advance bridge fixture"],
        );
    }

    fn context(directory: &Path) -> RepositoryFormatContext {
        Repository::discover(directory)
            .expect("discover fixture repository")
            .open_format_context()
            .expect("admit fixture format context")
    }

    fn write_git_object(
        directory: &Path,
        algorithm: GitHashAlgorithm,
        kind: &str,
        bytes: &[u8],
    ) -> NativeOid {
        let mut child = Command::new("git")
            .current_dir(directory)
            .args(["hash-object", "-w", "-t", kind, "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn git hash-object");
        child
            .stdin
            .take()
            .expect("hash-object stdin")
            .write_all(bytes)
            .expect("write native object bytes");
        let output = child.wait_with_output().expect("wait for hash-object");
        assert!(
            output.status.success(),
            "git hash-object: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        NativeOid::from_hex(
            algorithm,
            std::str::from_utf8(&output.stdout)
                .expect("hash-object OID is UTF-8")
                .trim(),
        )
        .expect("valid native OID")
    }

    fn append_tree_entry(tree: &mut Vec<u8>, mode: &str, name: &str, oid: &NativeOid) {
        tree.extend_from_slice(mode.as_bytes());
        tree.push(b' ');
        tree.extend_from_slice(name.as_bytes());
        tree.push(0);
        tree.extend_from_slice(oid.as_bytes());
    }

    fn write_native_node(
        directory: &Path,
        algorithm: GitHashAlgorithm,
        node: &NodeData,
    ) -> NativeOid {
        let data_oid = write_git_object(
            directory,
            algorithm,
            "blob",
            &node.encode_canonical().expect("canonical node data"),
        );
        let mut dependencies = node.dependencies().expect("typed node dependencies");
        dependencies.sort_by_key(|dependency| dependency.oid().to_hex());
        let refs_oid = if dependencies.is_empty() {
            None
        } else {
            let mut refs_tree = Vec::new();
            for dependency in dependencies {
                let mode = match dependency.kind() {
                    crate::native_graph::NativeObjectKind::Blob => "100644",
                    crate::native_graph::NativeObjectKind::Tree => "40000",
                };
                append_tree_entry(
                    &mut refs_tree,
                    mode,
                    &dependency.oid().to_hex(),
                    dependency.oid(),
                );
            }
            Some(write_git_object(directory, algorithm, "tree", &refs_tree))
        };

        let mut envelope = Vec::new();
        append_tree_entry(&mut envelope, "100644", "data", &data_oid);
        if let Some(refs_oid) = refs_oid {
            append_tree_entry(&mut envelope, "40000", "refs", &refs_oid);
        }
        write_git_object(directory, algorithm, "tree", &envelope)
    }

    fn cbor_head(output: &mut Vec<u8>, major: u8, value: u64) {
        let prefix = major << 5;
        match value {
            0..=23 => output.push(prefix | value as u8),
            24..=255 => output.extend_from_slice(&[prefix | 24, value as u8]),
            256..=65_535 => {
                output.push(prefix | 25);
                output.extend_from_slice(&(value as u16).to_be_bytes());
            }
            65_536..=4_294_967_295 => {
                output.push(prefix | 26);
                output.extend_from_slice(&(value as u32).to_be_bytes());
            }
            _ => {
                output.push(prefix | 27);
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
    }

    fn cbor_bytes(output: &mut Vec<u8>, value: &[u8]) {
        cbor_head(output, 2, value.len() as u64);
        output.extend_from_slice(value);
    }

    fn row_domain(relation_id: [u8; 16], schema_digest: [u8; 32]) -> Vec<u8> {
        let mut domain = vec![0x83, 0x64];
        domain.extend_from_slice(b"rows");
        cbor_bytes(&mut domain, &relation_id);
        cbor_bytes(&mut domain, &schema_digest);
        domain
    }

    fn relations_domain(database_id: [u8; 16]) -> Vec<u8> {
        let mut domain = vec![0x82, 0x69];
        domain.extend_from_slice(b"relations");
        cbor_bytes(&mut domain, &database_id);
        domain
    }

    fn commit_store_root(directory: &Path, algorithm: GitHashAlgorithm, store_root: &NativeOid) {
        let database_oid = write_git_object(directory, algorithm, "blob", DATABASE.as_bytes());
        let main_oid = write_git_object(directory, algorithm, "blob", MAIN_SOURCE.as_bytes());
        let orna_tree_spec = format!(
            "100644 blob {}\tdatabase.orna\n040000 tree {}\tstore\n",
            database_oid.to_hex(),
            store_root.to_hex()
        );
        let orna_tree = git_output(directory, &["mktree"], Some(orna_tree_spec.as_bytes()));
        let orna_tree_oid = NativeOid::from_hex(
            algorithm,
            std::str::from_utf8(&orna_tree)
                .expect("mktree OID UTF-8")
                .trim(),
        )
        .expect("valid .orna tree OID");
        let root_tree_spec = format!(
            "040000 tree {}\t.orna\n100644 blob {}\tmain.orna\n",
            orna_tree_oid.to_hex(),
            main_oid.to_hex()
        );
        let root_tree = git_output(directory, &["mktree"], Some(root_tree_spec.as_bytes()));
        let root_tree_oid = NativeOid::from_hex(
            algorithm,
            std::str::from_utf8(&root_tree)
                .expect("mktree OID UTF-8")
                .trim(),
        )
        .expect("valid root tree OID");
        let parent = git_output(directory, &["rev-parse", "HEAD"], None);
        let parent = std::str::from_utf8(&parent)
            .expect("parent commit UTF-8")
            .trim();
        let commit = git_output(
            directory,
            &[
                "commit-tree",
                &root_tree_oid.to_hex(),
                "-p",
                parent,
                "-m",
                "persist native format3 fixture",
            ],
            None,
        );
        let commit = std::str::from_utf8(&commit)
            .expect("commit OID UTF-8")
            .trim();
        git(directory, &["update-ref", "refs/heads/main", commit]);
    }

    fn schema_descriptor_node(
        directory: &Path,
        algorithm: GitHashAlgorithm,
        schema_bytes: &[u8],
        encoded_length: u64,
        schema_digest: [u8; 32],
    ) -> NativeOid {
        let chunk_oid = write_git_object(directory, algorithm, "blob", schema_bytes);
        let byte_root = write_native_node(
            directory,
            algorithm,
            &NodeData::ByteIndex {
                height: 0,
                total_length: schema_bytes.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: schema_bytes.len() as u64,
                    target: chunk_oid,
                    chunk_sha256: Some(schema_digest),
                }],
            },
        );
        let schema_oid = write_native_node(
            directory,
            algorithm,
            &NodeData::Schema {
                encoded_length,
                schema_digest,
                byte_root,
            },
        );
        schema_oid
    }

    fn schema_node(directory: &Path, context: &RepositoryFormatContext) -> (NativeOid, [u8; 32]) {
        let algorithm = context
            .validate_schema_root()
            .expect("pinned source schema root")
            .oid
            .algorithm();
        let schema_bytes = MAIN_SOURCE.as_bytes();
        let schema_digest: [u8; 32] = Sha256::digest(schema_bytes).into();
        let schema_oid = schema_descriptor_node(
            directory,
            algorithm,
            schema_bytes,
            schema_bytes.len() as u64,
            schema_digest,
        );
        (schema_oid, schema_digest)
    }

    fn install_overflow_row_store(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
        semantic_digest_override: Option<[u8; 32]>,
    ) -> ([u8; 16], [u8; 32]) {
        let algorithm = context
            .validate_store_root()
            .expect("pinned format-3 store")
            .oid
            .algorithm();
        let database_id = context.require_database_id().expect("database identity");
        let (schema_oid, schema_digest) = schema_node(directory, context);
        let overflow_bytes = [0x81, 0x01];
        let semantic_digest: [u8; 32] = Sha256::digest(overflow_bytes).into();
        let stored_digest = semantic_digest_override.unwrap_or(semantic_digest);
        let chunk_oid = write_git_object(directory, algorithm, "blob", &overflow_bytes);
        let byte_root = write_native_node(
            directory,
            algorithm,
            &NodeData::ByteIndex {
                height: 0,
                total_length: overflow_bytes.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: overflow_bytes.len() as u64,
                    target: chunk_oid,
                    chunk_sha256: Some(semantic_digest),
                }],
            },
        );
        let overflow_root = write_native_node(
            directory,
            algorithm,
            &NodeData::ValueOverflow {
                encoded_length: overflow_bytes.len() as u64,
                semantic_digest: stored_digest,
                byte_root,
                dependency_root: None,
            },
        );
        let mut overflow_value = Vec::new();
        cbor_head(&mut overflow_value, 6, 60113);
        overflow_value.push(0x81);
        cbor_bytes(&mut overflow_value, overflow_root.as_bytes());
        let row_root = write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: row_domain(relation_id, schema_digest),
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::UInt(7).canonical_bytes().unwrap(),
                    value: overflow_value,
                }],
            },
        );

        let mut relation_value = vec![0x84];
        cbor_bytes(&mut relation_value, schema_oid.as_bytes());
        cbor_bytes(&mut relation_value, row_root.as_bytes());
        relation_value.push(0xf6);
        relation_value.push(0x01);
        let relation_map = write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: relations_domain(*database_id.as_bytes()),
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::Bytes(relation_id.to_vec())
                        .canonical_bytes()
                        .unwrap(),
                    value: relation_value,
                }],
            },
        );
        let store_root =
            write_native_node(directory, algorithm, &NodeData::StoreRoot { relation_map });
        commit_store_root(directory, algorithm, &store_root);
        (relation_id, semantic_digest)
    }

    fn install_branch_row_store(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
        rows_and_fences: &[(u64, u64)],
    ) {
        let algorithm = context
            .validate_store_root()
            .expect("pinned format-3 store")
            .oid
            .algorithm();
        let database_id = context.require_database_id().expect("database identity");
        let (schema_oid, schema_digest) = schema_node(directory, context);

        let mut branch_entries = Vec::new();
        for &(key, stored_fence) in rows_and_fences {
            let row = write_native_node(
                directory,
                algorithm,
                &NodeData::OrderedLeaf {
                    domain: row_domain(relation_id, schema_digest),
                    entries: vec![OrderedLeafEntry {
                        key: TypedKey::UInt(key).canonical_bytes().unwrap(),
                        value: vec![0x81, 0x01],
                    }],
                },
            );
            branch_entries.push(OrderedBranchEntry {
                inclusive_max_key: TypedKey::UInt(stored_fence).canonical_bytes().unwrap(),
                child: row,
                row_count: 1,
            });
        }
        let primary_root = write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedBranch {
                domain: row_domain(relation_id, schema_digest),
                height: 1,
                entries: branch_entries,
            },
        );

        let mut relation_value = vec![0x84];
        cbor_bytes(&mut relation_value, schema_oid.as_bytes());
        cbor_bytes(&mut relation_value, primary_root.as_bytes());
        relation_value.push(0xf6);
        relation_value.push(u8::try_from(rows_and_fences.len()).unwrap());
        let relation_map = write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: relations_domain(*database_id.as_bytes()),
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::Bytes(relation_id.to_vec())
                        .canonical_bytes()
                        .unwrap(),
                    value: relation_value,
                }],
            },
        );
        let store_root =
            write_native_node(directory, algorithm, &NodeData::StoreRoot { relation_map });
        commit_store_root(directory, algorithm, &store_root);
    }

    fn sealed_rows(
        context: &RepositoryFormatContext,
        schema_oid: &NativeOid,
        schema_digest: [u8; 32],
    ) -> RowMapSnapshot {
        let database_id = context.require_database_id().expect("database identity");
        let store = context.validate_store_root().expect("pinned store root");

        let mut relation_hash = Sha256::new();
        relation_hash.update(b"orna.test.graph-bridge.relation.v1\0");
        relation_hash.update(database_id.as_bytes());
        let relation_digest = relation_hash.finalize();
        let mut relation_id = [0u8; 16];
        relation_id.copy_from_slice(&relation_digest[..16]);

        let schema_generation = SchemaGeneration::issue(
            *database_id.as_bytes(),
            relation_id,
            schema_oid.clone(),
            schema_digest,
            0,
        );
        let version = RowMapVersion::issue(
            *database_id.as_bytes(),
            relation_id,
            store.oid.clone(),
            schema_generation,
            store.oid.clone(),
            0,
            Some(0),
        )
        .expect("consistent row-map identity");

        let mut authority = Sha256::new();
        authority.update(b"orna.test.graph-bridge.row-authority.v1\0");
        authority.update(context.snapshot.commit.as_str().as_bytes());
        authority.update(database_id.as_bytes());
        RowMapSnapshot::issue(version, authority.finalize().into(), Vec::new())
            .expect("seal row-map snapshot")
    }

    #[test]
    fn native_graph_bridge_requires_matching_sealed_snapshot_before_scope() {
        let directory = repository();
        let root = directory.path();
        let original = context(root);
        let (schema_oid, schema_digest) = schema_node(root, &original);
        let matching_rows = sealed_rows(&original, &schema_oid, schema_digest);
        let graph = original
            .open_native_graph(&matching_rows)
            .expect("matching sealed format-3 row snapshot admits graph");
        let _scope = graph
            .open_read_scope()
            .expect("graph owner issues its fixed-budget read scope");

        let alternate_database = DATABASE_TEMPLATE.replace(DATABASE_PLACEHOLDER, OTHER_DATABASE_ID);
        fs::write(root.join(".orna/database.orna"), alternate_database)
            .expect("write alternate database fixture");
        commit(root);
        let other_database_context = context(root);
        let other_database_rows = sealed_rows(&other_database_context, &schema_oid, schema_digest);
        assert!(matches!(
            original.open_native_graph(&other_database_rows),
            Err(super::FormatContextError::RowMapMismatch)
        ));

        fs::write(root.join(".orna/database.orna"), DATABASE)
            .expect("restore original database fixture");
        fs::write(root.join(".orna/store/data"), b"store root two")
            .expect("write different snapshot store root");
        commit(root);
        let other_snapshot_context = context(root);
        let other_snapshot_rows = sealed_rows(&other_snapshot_context, &schema_oid, schema_digest);
        assert!(matches!(
            original.open_native_graph(&other_snapshot_rows),
            Err(super::FormatContextError::RowMapMismatch)
        ));
    }

    #[test]
    fn native_graph_bridge_rejects_tampered_schema_payload_length_and_digest() {
        let directory = repository();
        let root = directory.path();
        let context = context(root);
        let algorithm = context
            .validate_schema_root()
            .expect("pinned source schema root")
            .oid
            .algorithm();
        let valid_bytes = MAIN_SOURCE.as_bytes();
        let valid_digest: [u8; 32] = Sha256::digest(valid_bytes).into();

        let mut tampered_bytes = valid_bytes.to_vec();
        tampered_bytes[0] ^= 1;
        let tampered_payload = schema_descriptor_node(
            root,
            algorithm,
            &tampered_bytes,
            valid_bytes.len() as u64,
            valid_digest,
        );
        let rows = sealed_rows(&context, &tampered_payload, valid_digest);
        assert!(matches!(
            context.open_native_graph(&rows),
            Err(super::FormatContextError::GraphContextInvalid)
        ));

        let wrong_length = schema_descriptor_node(
            root,
            algorithm,
            valid_bytes,
            valid_bytes.len() as u64 + 1,
            valid_digest,
        );
        let rows = sealed_rows(&context, &wrong_length, valid_digest);
        assert!(matches!(
            context.open_native_graph(&rows),
            Err(super::FormatContextError::GraphContextInvalid)
        ));

        let mut wrong_digest = valid_digest;
        wrong_digest[0] ^= 1;
        let descriptor_digest_mismatch = schema_descriptor_node(
            root,
            algorithm,
            valid_bytes,
            valid_bytes.len() as u64,
            wrong_digest,
        );
        let rows = sealed_rows(&context, &descriptor_digest_mismatch, valid_digest);
        assert!(matches!(
            context.open_native_graph(&rows),
            Err(super::FormatContextError::GraphContextInvalid)
        ));
    }

    #[test]
    fn pinned_orp_loader_issues_lazy_snapshot_and_reads_verified_overflow_by_point_and_range() {
        let directory = repository();
        let root = directory.path();
        let initial = context(root);
        let relation_id = [0x72; 16];
        let (relation_id, expected_digest) =
            install_overflow_row_store(root, &initial, relation_id, None);
        let pinned = context(root);
        let snapshot = pinned
            .load_row_map(relation_id)
            .expect("load pinned ORP identity");
        assert!(!snapshot.is_materialized());
        assert!(matches!(
            snapshot.get(&TypedKey::UInt(7)),
            Err(crate::row_store::RowStoreError::PersistedLookupRequiresGraphContext)
        ));
        let graph = pinned
            .open_native_graph(&snapshot)
            .expect("issue graph context from repository snapshot");
        let scope = graph.open_read_scope().expect("owner read scope");
        let row = graph
            .lookup_row(&TypedKey::UInt(7), &scope)
            .expect("bounded point lookup")
            .expect("stored row");
        let RowValue::Overflow(reference) = row.value() else {
            panic!("verified kind-3 row should decode as overflow");
        };
        assert_eq!(reference.encoded_length(), 2);
        assert_eq!(reference.semantic_digest(), &expected_digest);

        let range = KeyRange::new(Some(TypedKey::UInt(7)), None, 1).unwrap();
        let ranged = graph
            .range_rows(&range, &scope)
            .expect("bounded ORP range lookup");
        assert_eq!(ranged, vec![row]);

        let bad_digest = [0x99; 32];
        install_overflow_row_store(root, &pinned, relation_id, Some(bad_digest));
        let tampered_context = context(root);
        let tampered_snapshot = tampered_context
            .load_row_map(relation_id)
            .expect("root identity is lazy until row access");
        let tampered_graph = tampered_context
            .open_native_graph(&tampered_snapshot)
            .expect("schema and map root remain pinned");
        let tampered_scope = tampered_graph.open_read_scope().unwrap();
        assert!(matches!(
            tampered_graph.lookup_row(&TypedKey::UInt(7), &tampered_scope),
            Err(crate::native_graph::GraphError::ContentIdentityMismatch)
        ));
    }

    #[test]
    fn repository_point_lookup_checks_later_fences_after_candidate_miss() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x72; 16];
        let initial = context(root);
        // Key 20 falls within child 2's claimed max (30), but is absent there.
        // The next child is an invalid overlapping range hidden behind fence
        // 50; point lookup must continue far enough to reject it.
        install_branch_row_store(
            root,
            &initial,
            relation_id,
            &[(10, 10), (30, 30), (20, 50), (60, 60)],
        );

        let pinned = context(root);
        let snapshot = pinned.load_row_map(relation_id).unwrap();
        assert!(!snapshot.is_materialized());
        let graph = pinned.open_native_graph(&snapshot).unwrap();
        let scope = graph.open_read_scope().unwrap();

        // The final profile caps each ORP page at 256 direct refs and height
        // at 64. This four-child graph isolates the point-lookup miss path;
        // the production scope additionally caps one operation at 2,048
        // native Git objects, which is measured below.
        let before_point = scope.objects_read_for_test();
        assert!(matches!(
            graph.lookup_row(&TypedKey::UInt(20), &scope),
            Err(crate::native_graph::GraphError::InvalidCount(_))
        ));
        let point_objects = scope.objects_read_for_test() - before_point;
        assert!(point_objects > 0 && point_objects <= MAX_RANGE_GRAPH_OBJECTS);
        println!(
            "git-backed ORP point lookup object reads: {point_objects} (operation_limit={MAX_RANGE_GRAPH_OBJECTS})"
        );
    }

    #[test]
    fn captured_ogb2_protected_pin_is_admitted_through_committed_orp_row() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x73; 16];
        let initial = context(root);
        install_overflow_row_store(root, &initial, relation_id, None);

        let writing_context = context(root);
        let (schema_oid, schema_digest) = schema_node(root, &writing_context);
        let snapshot = writing_context
            .load_row_map(relation_id)
            .expect("load the committed row-map identity");
        let graph = writing_context
            .open_native_graph(&snapshot)
            .expect("admit the repository-owned graph context");
        let write_scope = graph.open_read_scope().expect("owner write scope");

        let payload: Vec<u8> = (0..(2 * crate::blob_store::GEAR_MAXIMUM + 73))
            .map(|index| (index.wrapping_mul(37) & 0xff) as u8)
            .collect();
        let mut input = std::io::Cursor::new(payload.clone());
        let candidate = graph
            .capture_blob_candidate(&mut input, payload.len() as u64, &write_scope)
            .expect("write a private OGB-2 candidate closure");
        let pin = graph
            .protect_captured_blob(candidate, &write_scope)
            .expect("verify and durably protect the OGB-2 closure");
        let binding = graph
            .accept_protected_blob_pin(pin)
            .expect("accept the durable pin as a canonical ORP Blob binding");
        let identity = binding.content_identity();
        let descriptor_oid = binding.descriptor_oid().clone();
        let transfer = binding.transfer_record();
        assert_eq!(identity, crate::blob_store::digest_bytes(&payload));
        assert_eq!(transfer.content_identity(), identity);

        let algorithm = writing_context
            .validate_store_root()
            .expect("pinned native store")
            .oid
            .algorithm();
        let mut stored_fields = vec![0x81];
        stored_fields.extend_from_slice(binding.encoded_value());

        let row_root = write_native_node(
            root,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: row_domain(relation_id, schema_digest),
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::UInt(7).canonical_bytes().unwrap(),
                    value: stored_fields,
                }],
            },
        );
        let database_id = writing_context.require_database_id().unwrap();
        let mut relation_value = vec![0x84];
        cbor_bytes(&mut relation_value, schema_oid.as_bytes());
        cbor_bytes(&mut relation_value, row_root.as_bytes());
        relation_value.push(0xf6);
        relation_value.push(0x01);
        let relation_map = write_native_node(
            root,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: relations_domain(*database_id.as_bytes()),
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::Bytes(relation_id.to_vec())
                        .canonical_bytes()
                        .unwrap(),
                    value: relation_value,
                }],
            },
        );
        let store_root = write_native_node(root, algorithm, &NodeData::StoreRoot { relation_map });
        commit_store_root(root, algorithm, &store_root);

        let admitted_context = context(root);
        let admitted_snapshot = admitted_context
            .load_row_map(relation_id)
            .expect("admit the committed ORP row map");
        let admitted_graph = admitted_context
            .open_native_graph(&admitted_snapshot)
            .expect("bind row reads to the committed graph");
        let read_scope = admitted_graph.open_read_scope().unwrap();
        let row = admitted_graph
            .lookup_row(&TypedKey::UInt(7), &read_scope)
            .unwrap()
            .expect("committed Blob row");
        let reference = admitted_graph
            .admit_blob_reference(&row, &descriptor_oid, &read_scope)
            .expect("admit the descriptor only through its stored row");
        let before_read = read_scope.objects_read_for_test();
        let verified = admitted_graph
            .read_blob_range(&reference, 0..identity.length(), &read_scope)
            .expect("verify the complete OGB-2 closure");
        let objects_read = read_scope.objects_read_for_test() - before_read;
        assert!(objects_read > 0 && objects_read <= MAX_RANGE_GRAPH_OBJECTS);
        assert_eq!(verified.bytes(), payload);
        assert_eq!(
            verified.verification(),
            crate::native_graph::RangeVerification::FullBlob
        );

        let resolved = admitted_graph
            .resolve_row_node(&row, &descriptor_oid, &read_scope)
            .expect("resolve the descriptor named by the committed row");
        assert!(matches!(
            resolved,
            NodeData::BlobDescriptor { length, .. } if length == identity.length()
        ));
        assert!(matches!(
            admitted_graph.resolve_row_node(&row, &store_root, &read_scope),
            Err(crate::native_graph::GraphError::DescriptorNotInRow)
        ));

        // Ranges that start, end, or straddle the GEAR_MAXIMUM boundary must
        // return exactly the matching payload bytes, whatever chunk cuts the
        // Gear chunker chose inside the blob.
        let maximum = crate::blob_store::GEAR_MAXIMUM as u64;
        let length = identity.length();
        for range in [
            0..1,
            maximum - 1..maximum + 1,
            maximum..2 * maximum,
            length - 1..length,
            5..length - 5,
            7..7,
        ] {
            let range_scope = admitted_graph.open_read_scope().unwrap();
            let read = admitted_graph
                .read_blob_range(&reference, range.clone(), &range_scope)
                .unwrap_or_else(|error| panic!("range {range:?} failed: {error:?}"));
            assert_eq!(
                read.bytes(),
                &payload[range.start as usize..range.end as usize],
                "range {range:?} returned the wrong bytes"
            );
        }
    }

    #[test]
    fn revision_walk_lists_commits_and_root_trees_for_admitted_graph() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x74; 16];
        let initial = context(root);
        install_overflow_row_store(root, &initial, relation_id, None);

        let admitted_context = context(root);
        let snapshot = admitted_context
            .load_row_map(relation_id)
            .expect("load the committed row-map context");
        let graph = admitted_context
            .open_native_graph(&snapshot)
            .expect("admit the repository-owned graph context");

        let text = |output: Vec<u8>| String::from_utf8(output).unwrap().trim().to_owned();
        let head = text(git_output(root, &["rev-parse", "HEAD"], None));
        let head_tree = text(git_output(root, &["rev-parse", "HEAD^{tree}"], None));
        let commit_count = text(git_output(root, &["rev-list", "--count", "HEAD"], None))
            .parse::<usize>()
            .expect("count reachable commits");
        assert!(commit_count >= 2, "store install must add a commit");

        let all = graph
            .list_revision_snapshots(commit_count)
            .expect("list every reachable revision");
        assert_eq!(all.len(), commit_count);
        assert_eq!(all[0].commit().to_hex(), head);
        assert_eq!(all[0].tree().to_hex(), head_tree);
        assert_eq!(graph.list_revision_snapshots(1).unwrap(), all[..1].to_vec());
        assert!(graph.list_revision_snapshots(0).unwrap().is_empty());
        assert!(matches!(
            graph.list_revision_snapshots(4097),
            Err(crate::native_graph::GraphError::InventoryQuotaExceeded)
        ));
    }
}

/// Why a repository could not issue capture authority for a relation.
#[derive(Debug)]
pub enum CaptureCapabilityError {
    /// The format context, row map, or native graph was unavailable.
    Format(FormatContextError),
    /// The graph refused to issue the read scope or capability.
    Graph(GraphError),
}
