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

use crate::{CommittedTreeEntryKind, Repository, RepositoryError};
use crate::{
    native_graph::{GitHashAlgorithm, NativeGraphContext, NativeOid},
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
const STORE_PATH_PREFIX: &str = ".orna/store/";
const MAX_TREE_ENTRIES_FOR_ROOT_VALIDATION: usize = 65_536;
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
        let source = match self.repository.read_committed_file(
            &self.snapshot.commit,
            MAIN_SOURCE_PATH,
            64 * 1024,
        ) {
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

    /// A non-authoritative store-root validation seam for later OGS/ORP
    /// layers. It checks only the bounded native-tree entry shape and does not
    /// decode or write graph nodes.
    pub fn validate_store_root(&self) -> Result<StoreRootPin, FormatContextError> {
        self.require_format3()?;
        let entries = self
            .repository
            .list_committed_tree(&self.snapshot.commit, MAX_TREE_ENTRIES_FOR_ROOT_VALIDATION)
            .map_err(|error| match error {
                RepositoryError::GitUnavailable => FormatContextError::StoreRootUnavailable,
                _ => FormatContextError::StoreRootInvalid,
            })?;
        let mut found = false;
        for entry in entries {
            let Some(path) = entry.path().as_path().to_str() else {
                if entry
                    .path()
                    .as_path()
                    .starts_with(std::path::Path::new(".orna"))
                {
                    return Err(FormatContextError::StoreRootInvalid);
                }
                continue;
            };
            if path == DATABASE_PATH {
                continue;
            }
            if path.starts_with(".orna/") && !path.starts_with(STORE_PATH_PREFIX) {
                return Err(FormatContextError::StoreRootInvalid);
            }
            if !path.starts_with(STORE_PATH_PREFIX) {
                continue;
            }
            found = true;
            if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. }) {
                return Err(FormatContextError::StoreRootInvalid);
            }
        }
        if !found {
            return Err(FormatContextError::StoreRootUnavailable);
        }
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
        })
    }

    /// Opens and validates the format metadata at the current immutable HEAD.
    /// Worktree-only metadata is not silently treated as a committed snapshot.
    pub fn open_format_context(&self) -> Result<RepositoryFormatContext, FormatContextError> {
        let snapshot = self.pin_snapshot("HEAD")?;
        self.open_format_context_at(&snapshot)
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
    match repository.read_committed_file(&snapshot.commit, path, MAX_METADATA_READ_BYTES) {
        Ok(bytes) if bytes.len() <= FORMAT_CONTEXT_MAX_METADATA_BYTES => Ok(Some(bytes)),
        Ok(_) => Err(FormatContextError::MetadataInvalid),
        Err(RepositoryError::GitUnavailable) => Err(FormatContextError::MetadataUnavailable),
        // `read_committed_file` intentionally redacts whether a path was
        // absent. Inspect the bounded committed tree before treating that
        // error as absence; otherwise a malformed database record could be
        // downgraded into legacy dispatch.
        Err(_) => match repository
            .list_committed_tree(&snapshot.commit, MAX_TREE_ENTRIES_FOR_ROOT_VALIDATION)
            .map_err(|error| match error {
                RepositoryError::GitUnavailable => FormatContextError::MetadataUnavailable,
                _ => FormatContextError::MetadataInvalid,
            })?
            .into_iter()
            .find(|entry| {
                entry.path().as_path().to_str().is_some_and(|entry_path| {
                    entry_path == path
                        || entry_path
                            .strip_prefix(path)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
            }) {
            Some(_) => Err(FormatContextError::MetadataInvalid),
            None => Ok(None),
        },
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

fn parse_legacy_format(bytes: &[u8]) -> Result<RepositoryFormat, FormatContextError> {
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
        native_graph::{ByteIndexEntry, GitHashAlgorithm, NativeOid, NodeData},
        row_store::{RowMapSnapshot, RowMapVersion, SchemaGeneration},
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
}
