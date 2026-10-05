//! Repository-owned format admission and pinned metadata context.
//!
//! This module is deliberately narrower than the repository's storage and
//! graph implementations. It admits the tracked format coordinate, keeps
//! legacy readers explicitly read-only, and issues opaque snapshot-bound
//! handles for later value/schema/store work. It does not write rows, convert
//! data, or interpret native graph objects.

use std::{fmt, str::FromStr};

use orna_syntax_v1::{Expr, LiteralKind, RecordField, parse_row};

use crate::{CommittedTreeEntryKind, Repository, RepositoryError};

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
}

/// Opaque proof that the pinned `.orna/store` tree had a valid native-tree
/// entry shape. Graph decoding is intentionally outside this slice.
#[derive(Clone, Debug)]
pub struct StoreRootPin {
    snapshot: RepositorySnapshotPin,
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
        match self.repository.read_committed_file(
            &self.snapshot.commit,
            MAIN_SOURCE_PATH,
            64 * 1024,
        ) {
            Ok(_) => Ok(SchemaRootPin {
                snapshot: self.snapshot.clone(),
            }),
            Err(RepositoryError::GitUnavailable) => Err(FormatContextError::SchemaRootUnavailable),
            Err(_) => Err(FormatContextError::SchemaRootInvalid),
        }
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
            if !entry
                .path()
                .as_path()
                .to_str()
                .is_some_and(|path| path.starts_with(STORE_PATH_PREFIX))
            {
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
        Ok(StoreRootPin {
            snapshot: self.snapshot.clone(),
        })
    }

    fn require_format3(&self) -> Result<(), FormatContextError> {
        if self.format.supports_writes() {
            Ok(())
        } else {
            Err(FormatContextError::UnknownFormat)
        }
    }
}

/// Compatibility spelling for code in this crate's transition window.
///
/// This alias adds no construction path; the only public issuer remains
/// [`Repository::open_format_context`].
pub type FormatContext = RepositoryFormatContext;

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

    let fields = parse_record(bytes).map_err(|_| FormatContextError::MetadataInvalid)?;
    if fields.len() != 2
        || fields
            .iter()
            .any(|field| field.name != "repository_format" && field.name != "storage_profile")
        || fields
            .iter()
            .filter(|field| field.name == "repository_format")
            .count()
            != 1
        || fields
            .iter()
            .filter(|field| field.name == "storage_profile")
            .count()
            != 1
    {
        return Err(FormatContextError::MetadataInvalid);
    }
    let format = fields
        .iter()
        .find(|field| field.name == "repository_format")
        .and_then(|field| integer_literal(&field.value))
        .ok_or(FormatContextError::MetadataInvalid)?;
    let profile = fields
        .iter()
        .find(|field| field.name == "storage_profile")
        .and_then(|field| string_literal(&field.value))
        .ok_or(FormatContextError::MetadataInvalid)?;
    if profile.is_empty() {
        return Err(FormatContextError::MetadataInvalid);
    }
    match format {
        1 => Ok(RepositoryFormat::Legacy1),
        2 => Ok(RepositoryFormat::Legacy2),
        3 => Err(FormatContextError::UnknownFormat),
        _ => Err(FormatContextError::UnknownFormat),
    }
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
