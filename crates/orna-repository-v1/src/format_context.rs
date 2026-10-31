//! Repository-owned format admission and pinned metadata context.
//!
//! This module is deliberately narrower than the repository's storage and
//! graph implementations. It admits the tracked format coordinate, keeps
//! legacy readers explicitly read-only, and issues opaque snapshot-bound
//! handles for later value/schema/store work. It does not write rows, convert
//! data, or interpret native graph objects.

use std::{fmt, str::FromStr};

use orna_syntax_v1::{Declaration, Expr, LiteralKind, RecordField, parse_module, parse_row};
use sha2::{Digest, Sha256};

use crate::native_graph::{
    GitHashAlgorithm, GraphError, NativeGraphContext, NativeOid, RepositoryCaptureCapability,
};
use crate::object_reader::{ObjectKind, ObjectReader, TreeEntry, TreeEntryMode};
use crate::row_store::RowMapSnapshot;
use crate::{Repository, RepositoryError};
use orna_value_v1::{
    Format3Context, GitHash as ValueGitHash, Raw, SchemaDescriptor, Snapshot as ValueSnapshot,
};

use super::DatabaseId;

/// The final repository writer coordinate from the consolidated publication.
pub const FINAL_REPOSITORY_FORMAT: u8 = 3;

/// The `repository_layout_version` coordinate reported by `sys.rt.info()`: the
/// 1.1.0 `repository_format` value, which equals [`FINAL_REPOSITORY_FORMAT`].
pub const REPOSITORY_LAYOUT_VERSION: &str = "3";

const _: () = assert!(
    REPOSITORY_LAYOUT_VERSION.len() == 1
        && REPOSITORY_LAYOUT_VERSION.as_bytes()[0] == b'0' + FINAL_REPOSITORY_FORMAT
);

/// The bounded size of the tracked `database.orna` record.
pub const FORMAT_CONTEXT_MAX_METADATA_BYTES: usize = 64 * 1024;
/// Per-module source bound compatible with project loading's aggregate budget.
const MAX_SOURCE_MODULE_BYTES: usize = 4 * 1024 * 1024;

/// The tracked metadata directory and the two record files format admission
/// reads from it (ORNA-META-001, ORNA-META-002).
const METADATA_DIRECTORY: &str = ".orna";
const DATABASE_FILE: &str = "database.orna";
const LEGACY_FORMAT_FILE: &str = "format.orna";
pub(crate) const LEGACY_FORMAT_PATH: &str = ".orna/format.orna";
const MAIN_SOURCE_PATH: &str = "main.orna";
/// The bound on the fixed `main.orna` schema root a format-3 snapshot admits.
const MAX_SCHEMA_ROOT_BYTES: usize = 64 * 1024;
/// The fixed position of the format-3 native store root inside a snapshot,
/// and its entry name in the metadata directory.
pub(crate) const STORE_PATH: &str = ".orna/store";
const STORE_ENTRY: &str = "store";
/// Tracked `.orna/` entries a format-3 snapshot may still carry when it is
/// read, although ORNA-META-001 admits only `database.orna` and `store`,
/// because this build still writes them. The list is closed, and each entry
/// names the work that removes it. The commit gate
/// ([`Repository::validate_format3_index_tree`]) admits none of them, so no
/// reviewed commit adds one.
const TRANSITIONAL_FORMAT3_ENTRIES: [&str; 2] = [
    // The object identity inventory from before it moved into the store.
    // LSP rename (`orna-lsp` `server.rs`) still writes it, and `orna commit`
    // migrates a staged copy into the store and drops it from the index.
    // Removed by the store-backed LSP rename, ornadb-5il1.414.
    crate::relation_admission::LEGACY_OBJECT_IDENTITY_SIDECAR_PATH,
    // The record a prepared `orna migrate copy` candidate tracks. Removed when
    // the migration coordinates move into commit trailers, ornadb-5il1.314.
    crate::migration_copy::MIGRATION_RECORD,
];

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatContextError {
    MetadataUnavailable,
    MetadataInvalid,
    UnknownFormat,
    /// A write or row-graph seam was attempted through a format-1/2
    /// compatibility reader. The snapshot is admitted and readable; only the
    /// write seams are refused, and the pinned numeric coordinate says which
    /// reader was asked.
    LegacyReadOnly(u8),
    /// The snapshot mixes repository-format layouts, so no reader is
    /// selected for it (ORNA-META-001, ORNA-META-002). `path` is the
    /// repository-relative tracked path that does not belong to the format
    /// the snapshot records: a legacy `.orna/format.orna` beside a format-3
    /// `database.orna`, the format-3 `.orna/store` beside a legacy record,
    /// another `.orna/` entry of a format-3 snapshot, or a loose row file that
    /// a format-3 commit candidate writes.
    MixedFormats {
        path: String,
    },
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
    /// The native graph is present but malformed (store-format-3 section 2).
    /// The typed graph error stays attached so callers can tell fault classes apart.
    GraphMalformed(crate::native_graph::GraphError),
    TableBindingMismatch,
}

impl FormatContextError {
    /// Stable diagnostic code without paths, Git commands, or object IDs.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MetadataUnavailable => "ORNA-REPO-CONTEXT-001",
            Self::MetadataInvalid => "ORNA-REPO-CONTEXT-002",
            Self::UnknownFormat => "ORNA-REPO-CONTEXT-003",
            Self::LegacyReadOnly(_) => "ORNA-REPO-CONTEXT-015",
            Self::MixedFormats { .. } => "ORNA-REPO-CONTEXT-004",
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
            Self::GraphMalformed(_) => "ORNA-REPO-CONTEXT-017",
            Self::TableBindingMismatch => "ORNA-REPO-CONTEXT-016",
        }
    }
}

impl fmt::Display for FormatContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MetadataUnavailable => "repository metadata is unavailable",
            Self::MetadataInvalid => "repository metadata is invalid",
            Self::UnknownFormat => "repository format is unknown",
            Self::LegacyReadOnly(format) => {
                return write!(
                    formatter,
                    "repository format {format} is a read-only compatibility snapshot; it has no row store to write"
                );
            }
            Self::MixedFormats { path } => {
                return write!(
                    formatter,
                    "repository snapshot mixes repository formats: {path} does not belong to its recorded format"
                );
            }
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
            Self::GraphMalformed(error) => {
                return write!(formatter, "native graph repository data is malformed: {error}");
            }
            Self::TableBindingMismatch => "table declaration and committed schema binding disagree",
        })
    }
}

impl std::error::Error for FormatContextError {}

/// A durable runtime write refused because the format recorded at `HEAD` is
/// 1 or 2. Legacy snapshots keep their readers but have no row writer in this
/// build (ORNA-UPGRADE-003, ORNA-ROW-013), and runtime admission follows the
/// recorded format, not worktree metadata (ORNA-UPGRADE-001). The refusal
/// carries its stable code, the recorded format and the remedy, so a front
/// end can report it without interpreting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyWriteRefused {
    format: u8,
}

impl LegacyWriteRefused {
    /// What a user can do. No in-place format-3 upgrade ships yet, so the
    /// remedy names the work item that delivers it rather than a command.
    pub const REMEDY: &'static str = "this build has no writer or in-place upgrade for format-1/2 snapshots; the format-3 upgrade is tracked as ornadb-5il1.184";

    /// The format recorded at `HEAD`: 1 or 2.
    pub const fn format(self) -> u8 {
        self.format
    }

    /// The stable diagnostic code, shared with the read-only legacy
    /// format-context refusal.
    pub const fn code(self) -> &'static str {
        FormatContextError::LegacyReadOnly(self.format).code()
    }

    /// The condition, without the format number.
    pub const fn title(self) -> &'static str {
        "repository HEAD is a read-only legacy snapshot"
    }

    pub const fn remedy(self) -> &'static str {
        Self::REMEDY
    }
}

impl fmt::Display for LegacyWriteRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "repository format {} at HEAD is a read-only compatibility snapshot; durable runtime writes are refused",
            self.format
        )
    }
}

impl std::error::Error for LegacyWriteRefused {}

/// Owner-authenticated descriptor binding for one format-3 relation in an
/// exact repository UUID and snapshot. A qualified source identity is retained
/// only when its module path and unique table declaration are verified against
/// that snapshot. The descriptor is decoded from its sealed schema object; the
/// complete expected source descriptor must equal it exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommittedTableBinding {
    database_id: DatabaseId,
    snapshot_id: [u8; 32],
    module_name: Option<String>,
    source_path: Option<String>,
    declaration_name: Option<String>,
    relation_id: [u8; 16],
    schema_oid: NativeOid,
    schema_digest: [u8; 32],
    descriptor: SchemaDescriptor,
}

impl CommittedTableBinding {
    /// Stable repository UUID authenticated from the pinned `.orna/database.orna`.
    pub const fn database_id(&self) -> DatabaseId {
        self.database_id
    }

    pub const fn snapshot_id(&self) -> &[u8; 32] {
        &self.snapshot_id
    }

    pub fn declaration_name(&self) -> Option<&str> {
        self.declaration_name.as_deref()
    }

    pub fn module_name(&self) -> Option<&str> {
        self.module_name.as_deref()
    }

    pub fn source_path(&self) -> Option<&str> {
        self.source_path.as_deref()
    }

    pub const fn table_object_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub const fn relation_id(&self) -> &[u8; 16] {
        &self.relation_id
    }

    pub fn schema_oid(&self) -> &NativeOid {
        &self.schema_oid
    }

    pub const fn schema_digest(&self) -> &[u8; 32] {
        &self.schema_digest
    }

    pub fn descriptor(&self) -> &SchemaDescriptor {
        &self.descriptor
    }
}

fn source_module_name(source_path: &str) -> Option<String> {
    if source_path.starts_with('/') || source_path.contains('\\') || source_path.contains('\0') {
        return None;
    }
    let mut components = source_path.split('/').collect::<Vec<_>>();
    let file = components.pop()?;
    let stem = file.strip_suffix(".orna")?;
    if components.iter().any(|component| {
        component.is_empty()
            || *component == "."
            || *component == ".."
            || component.contains('.')
            || component.contains('\0')
    }) || stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains('.')
        || stem.contains('\0')
    {
        return None;
    }
    if stem != "main" {
        components.push(stem);
    }
    Some(components.join("."))
}

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

/// Domain separation for the canonical logical snapshot identity. It keeps the
/// readable snapshot coordinate distinct from every other repository digest.
const SNAPSHOT_ID_DOMAIN: &[u8] = b"orna.repository.graph.snapshot.v1\0";

impl RepositorySnapshotPin {
    fn belongs_to(&self, repository: &Repository) -> bool {
        self.repository.worktree() == repository.worktree()
            && self.repository.runtime_paths().root() == repository.runtime_paths().root()
    }

    /// Canonical logical identity of this pinned snapshot.
    ///
    /// Derived from the exact resolved commit under a private domain, so a
    /// historical mount can report and bind the one snapshot it resolved
    /// without disclosing the native Git object ID. Two mounts of different
    /// commits never share an identity, and a repeated open of the same commit
    /// always reproduces it.
    pub fn snapshot_id(&self) -> [u8; 32] {
        self.commit.snapshot_id()
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

impl crate::GitCommitRef {
    /// Canonical logical snapshot identity of this repository-verified commit.
    pub fn snapshot_id(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(SNAPSHOT_ID_DOMAIN);
        hash.update(self.as_str().as_bytes());
        hash.finalize().into()
    }
}

/// Opaque proof that the pinned `main.orna` source/schema root was present in
/// the admitted snapshot as a regular blob. The native commit identity remains
/// repository-owned.
#[derive(Clone, Debug)]
pub struct SchemaRootPin {
    snapshot: RepositorySnapshotPin,
    oid: NativeOid,
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
    runtime_owner_id: Option<[u8; 16]>,
}

/// Owner-issued proof that a repository's admitted context is the final
/// format-3 writer for one database identity and the exact validated schema
/// and store roots of one pinned snapshot. Only
/// `RepositoryFormatContext::final_format_capability` mints it, after format,
/// identity and root validation, so callers cannot forge one from raw metadata
/// or a legacy read-only reader.
#[must_use = "a final format capability retains the roots it authorizes"]
#[derive(Clone, Debug)]
pub struct FinalFormatCapability {
    database_id: DatabaseId,
    snapshot_id: [u8; 32],
    schema_root: SchemaRootPin,
    store_root: StoreRootPin,
}

impl FinalFormatCapability {
    /// The database identity this capability was issued for.
    pub const fn database_id(&self) -> DatabaseId {
        self.database_id
    }

    /// The exact snapshot whose metadata, schema and store roots were admitted.
    pub const fn snapshot_id(&self) -> &[u8; 32] {
        &self.snapshot_id
    }

    /// The validated schema root retained by this capability.
    pub fn schema_root(&self) -> &SchemaRootPin {
        &self.schema_root
    }

    /// The validated store root retained by this capability.
    pub fn store_root(&self) -> &StoreRootPin {
        &self.store_root
    }
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

    /// Issues the final format-3 capability for this admitted context. Legacy
    /// formats 1 and 2 are read-only compatibility inputs and are refused. The
    /// capability also requires both fixed roots to validate in this context's
    /// pinned snapshot.
    pub fn final_format_capability(&self) -> Result<FinalFormatCapability, FormatContextError> {
        self.require_format3()?;
        let database_id = self.require_database_id()?;
        let schema_root = self.validate_schema_root()?;
        let store_root = self.validate_store_root()?;
        Ok(FinalFormatCapability {
            database_id,
            snapshot_id: self.snapshot.snapshot_id(),
            schema_root,
            store_root,
        })
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

    /// Binds graph-scoped values and scratch capture refs to a live runtime
    /// owner. Callers must supply the same identity held by that runtime
    /// owner lock so a later runtime reopen can reclaim refs left by a crash.
    pub fn with_runtime_owner_id(mut self, owner_id: [u8; 16]) -> Self {
        self.runtime_owner_id = Some(owner_id);
        self
    }
    /// The opaque immutable snapshot pin retained by this context.
    pub fn snapshot_pin(&self) -> &RepositorySnapshotPin {
        &self.snapshot
    }

    /// Pins the committed `main.orna` source root by its native blob OID.
    ///
    /// The source bytes are not read here. Store format 3 bounds only
    /// `database.orna` at 64 KiB (store-format-3 §1); a module source is
    /// admitted under the source-module bound when a table is bound to it.
    /// Reading and capping the source on every row-map load would make a
    /// project with a larger `main.orna` unable to open any row.
    pub fn validate_schema_root(&self) -> Result<SchemaRootPin, FormatContextError> {
        self.require_format3()?;
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
        let snapshot_id: [u8; 32] = self.snapshot.snapshot_id();
        crate::native_graph::load_format3_row_map(
            &self.repository,
            store.oid.algorithm(),
            store.oid,
            *database_id.as_bytes(),
            snapshot_id,
            relation_id,
        )
        .map_err(graph_context_error)
    }

    /// Issues native Git graph authority only for a sealed row-map snapshot
    /// whose repository, database, schema, store root, and exact snapshot
    /// authority match this pinned format-3 context. The graph is owned by this
    /// context's runtime owner, or by the repository-derived owner when none is set.
    pub fn open_native_graph(
        &self,
        rows: &RowMapSnapshot,
    ) -> Result<NativeGraphContext, FormatContextError> {
        self.issue_native_graph(rows, self.runtime_owner_id)
    }

    /// Issues the same graph authority as [`Self::open_native_graph`], owned by
    /// `owner_id`. Accepted pins captured under the repository-derived owner
    /// stay admissible through that owner as a fallback.
    pub fn open_native_graph_for_owner(
        &self,
        rows: &RowMapSnapshot,
        owner_id: [u8; 16],
    ) -> Result<NativeGraphContext, FormatContextError> {
        self.issue_native_graph(rows, Some(owner_id))
    }

    fn issue_native_graph(
        &self,
        rows: &RowMapSnapshot,
        runtime_owner_id: Option<[u8; 16]>,
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
        let snapshot_id = self.snapshot.snapshot_id();
        let row_count = version
            .row_count()
            .ok_or(FormatContextError::RowMapMismatch)?;
        let mut row_authority = Sha256::new();
        row_authority.update(b"orna.repository.orp.snapshot-authority.v1\0");
        row_authority.update(database_id.as_bytes());
        row_authority.update(snapshot_id);
        row_authority.update(store.oid.as_bytes());
        row_authority.update(version.relation_id());
        row_authority.update(version.primary_root().as_bytes());
        row_authority.update(schema_digest);
        row_authority.update(row_count.to_be_bytes());
        let expected_authority: [u8; 32] = row_authority.finalize().into();
        if rows.authority() != &expected_authority {
            return Err(FormatContextError::RowMapMismatch);
        }
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

        let authority = rows.authority();
        let derived_owner_id = {
            let mut owner_hash = Sha256::new();
            owner_hash.update(b"orna.repository.graph.owner.v1\0");
            owner_hash.update(authority);
            let owner_digest = owner_hash.finalize();
            let mut owner_id = [0u8; 16];
            owner_id.copy_from_slice(&owner_digest[..16]);
            owner_id
        };
        let owner_id = runtime_owner_id.unwrap_or(derived_owner_id);
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
        .map(|graph| graph.with_accepted_pin_fallback_owner(derived_owner_id))
        .map_err(|_| FormatContextError::GraphContextInvalid)
    }

    /// Issues capture authority using this admitted snapshot and its graph
    /// owner identity. A runtime-scoped context therefore writes scratch refs
    /// under the owner that will reclaim them after a crash. Captured
    /// candidates become lazy values pinned to this context's exact commit.
    pub fn capture_capability(
        &self,
        relation_id: [u8; 16],
    ) -> Result<RepositoryCaptureCapability, CaptureCapabilityError> {
        let row_map = self
            .load_row_map(relation_id)
            .map_err(CaptureCapabilityError::Format)?;
        let graph = std::sync::Arc::new(
            self.open_native_graph(&row_map)
                .map_err(CaptureCapabilityError::Format)?,
        );
        let scope = graph
            .open_read_scope()
            .map_err(CaptureCapabilityError::Graph)?;
        let pin = self.value_pin().map_err(CaptureCapabilityError::Format)?;
        RepositoryCaptureCapability::new(graph, scope)
            .map_err(CaptureCapabilityError::Graph)?
            .with_capture_pin(pin)
            .map_err(|_| CaptureCapabilityError::Format(FormatContextError::GraphContextInvalid))
    }

    /// Opens a snapshot-pinned format-3 value context for one relation.
    ///
    /// The graph-backed resolver stays bound to this repository context's
    /// exact commit, schema and store roots rather than the workspace HEAD.
    pub fn open_value_context(
        &self,
        relation_id: [u8; 16],
    ) -> Result<Format3Context, FormatContextError> {
        self.require_format3()?;
        let rows = self.load_row_map(relation_id)?;
        let graph = std::sync::Arc::new(self.open_native_graph(&rows)?);
        let scope = graph
            .open_read_scope()
            .map_err(|_| FormatContextError::GraphContextInvalid)?;
        let capability = RepositoryCaptureCapability::new(graph, scope)
            .map_err(|_| FormatContextError::GraphContextInvalid)?;
        capability
            .value_context(self.value_pin()?)
            .map_err(|_| FormatContextError::GraphContextInvalid)
    }

    /// The value-layer pin for this context's exact commit.
    fn value_pin(&self) -> Result<ValueSnapshot, FormatContextError> {
        let database_id = self.require_database_id()?;
        let algorithm = repository_hash_algorithm(&self.repository)?;
        let commit = NativeOid::from_hex(algorithm, self.snapshot.commit.as_str())
            .map_err(|_| FormatContextError::SnapshotInvalid)?;
        Ok(ValueSnapshot::Commit {
            database: *database_id.as_bytes(),
            algorithm: match algorithm {
                GitHashAlgorithm::Sha1 => ValueGitHash::Sha1,
                GitHashAlgorithm::Sha256 => ValueGitHash::Sha256,
            },
            oid: commit.as_bytes().to_vec(),
        })
    }

    /// Resolves the committed descriptor selected by its authenticated table ID.
    /// This descriptor is not source-bound until `bind_committed_table`
    /// verifies the pinned module, declaration and expected source schema.
    pub fn resolve_committed_table_by_relation_id(
        &self,
        relation_id: [u8; 16],
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.resolve_committed_relation_descriptor(relation_id)
    }

    fn resolve_committed_relation_descriptor(
        &self,
        relation_id: [u8; 16],
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.require_format3()?;
        let rows = self.load_row_map(relation_id)?;
        let version = rows.version();
        if version.relation_id() != &relation_id {
            return Err(FormatContextError::TableBindingMismatch);
        }
        let graph = self.open_native_graph(&rows)?;
        let scope = graph
            .open_read_scope()
            .map_err(graph_context_error)?;
        let descriptor = graph
            .read_schema_descriptor(&scope)
            .map_err(graph_context_error)?;
        if schema_descriptor_table_id(&descriptor) != Some(relation_id) {
            return Err(FormatContextError::TableBindingMismatch);
        }
        Ok(CommittedTableBinding {
            database_id: self.require_database_id()?,
            snapshot_id: self.snapshot.snapshot_id(),
            module_name: None,
            source_path: None,
            declaration_name: None,
            relation_id: *version.relation_id(),
            schema_oid: version.schema().schema_oid().clone(),
            schema_digest: *version.schema().schema_digest(),
            descriptor,
        })
    }

    fn validate_committed_source_table(
        &self,
        module_name: &str,
        source_path: &str,
        declaration_name: &str,
    ) -> Result<(), FormatContextError> {
        if source_module_name(source_path).as_deref() != Some(module_name) {
            return Err(FormatContextError::TableBindingMismatch);
        }
        let source = self
            .snapshot
            .read_file(&self.repository, source_path, MAX_SOURCE_MODULE_BYTES)
            .map_err(|error| match error {
                RepositoryError::GitUnavailable => FormatContextError::SchemaRootUnavailable,
                _ => FormatContextError::SchemaRootInvalid,
            })?;
        let source =
            std::str::from_utf8(&source).map_err(|_| FormatContextError::SchemaRootInvalid)?;
        let parsed = parse_module(source);
        if !parsed.is_ok() {
            return Err(FormatContextError::SchemaRootInvalid);
        }
        let mut declarations =
            parsed
                .value
                .items
                .iter()
                .filter_map(|item| match &item.declaration {
                    Declaration::Table { name, .. } if name == declaration_name => Some(()),
                    _ => None,
                });
        if declarations.next().is_none() || declarations.next().is_some() {
            return Err(FormatContextError::TableBindingMismatch);
        }
        Ok(())
    }

    /// Binds a qualified source table and its expected schema to this exact
    /// repository UUID and committed snapshot. The caller supplies the module
    /// namespace and repository-relative path from project analysis; the
    /// binding checks source identity, repository UUID, snapshot, table ID,
    /// and complete expected descriptor before issuing source-bound authority.
    pub fn bind_committed_table(
        &self,
        module_name: &str,
        source_path: &str,
        declaration_name: &str,
        table_object_id: [u8; 16],
        expected_database_id: DatabaseId,
        expected_snapshot_id: &[u8; 32],
        expected_schema: &SchemaDescriptor,
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.require_format3()?;
        if self.require_database_id()? != expected_database_id
            || *expected_snapshot_id != self.snapshot.snapshot_id()
            || schema_descriptor_table_id(expected_schema) != Some(table_object_id)
        {
            return Err(FormatContextError::TableBindingMismatch);
        }
        self.validate_committed_source_table(module_name, source_path, declaration_name)?;
        let mut binding = self.resolve_committed_table_by_relation_id(table_object_id)?;
        if binding.descriptor() != expected_schema {
            return Err(FormatContextError::TableBindingMismatch);
        }
        binding.module_name = Some(module_name.to_owned());
        binding.source_path = Some(source_path.to_owned());
        binding.declaration_name = Some(declaration_name.to_owned());
        Ok(binding)
    }

    /// Binds resolved schema to the committed descriptor selected by relation
    /// ID, rejecting any snapshot or descriptor mismatch.
    pub fn bind_committed_table_by_relation_id(
        &self,
        relation_id: [u8; 16],
        expected_database_id: DatabaseId,
        expected_snapshot_id: &[u8; 32],
        expected_schema: &SchemaDescriptor,
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.require_format3()?;
        if self.require_database_id()? != expected_database_id
            || *expected_snapshot_id != self.snapshot.snapshot_id()
            || schema_descriptor_table_id(expected_schema) != Some(relation_id)
        {
            return Err(FormatContextError::TableBindingMismatch);
        }
        let binding = self.resolve_committed_table_by_relation_id(relation_id)?;
        if binding.descriptor() != expected_schema {
            return Err(FormatContextError::TableBindingMismatch);
        }
        Ok(binding)
    }

    /// Reads one committed path's bytes from this context's own pinned
    /// snapshot.
    ///
    /// This is the one read seam every admitted repository format shares. A
    /// legacy format-1/2 input has no native `.orna/store`, so
    /// [`Self::load_row_map`] and [`Self::open_native_graph`] refuse it; its
    /// committed files are still readable through their original path layout,
    /// which is what keeps a legacy view open without converting it. A
    /// format-3 context reads the same path from the format-3 commit, so one
    /// process can hold both views at once without either consulting the
    /// other, the worktree `HEAD`, or the skipped format's objects.
    ///
    /// `max_bytes` bounds the read before the bytes are accumulated.
    pub fn read_committed_file(
        &self,
        path: impl AsRef<std::path::Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, FormatContextError> {
        self.snapshot
            .read_file(&self.repository, path, max_bytes)
            .map_err(|error| match error {
                RepositoryError::GitUnavailable => FormatContextError::GraphContextUnavailable,
                _ => FormatContextError::GraphContextInvalid,
            })
    }

    /// Every write and row-graph seam funnels through here, so an admitted
    /// format-1/2 reader is refused once, with its own pinned numeric
    /// coordinate, instead of reporting the unknown-format admission failure.
    fn require_format3(&self) -> Result<(), FormatContextError> {
        if self.format.supports_writes() {
            Ok(())
        } else {
            Err(FormatContextError::LegacyReadOnly(self.format.number()))
        }
    }
}
/// Classifies a native-graph fault on the admitted row-map path. Unavailable
/// objects stay unavailable (ORNA-ROW-007). Shape faults in the native graph
/// are malformed data (store-format-3 section 2, lines 50-55) and keep their
/// typed diagnostic. Every other fault is an invalid graph context, not a
/// malformed graph: a Git command failure, quota, cancel, durability or
/// context-mismatch failure does not prove the stored graph is malformed.
fn graph_context_error(error: crate::native_graph::GraphError) -> FormatContextError {
    use crate::native_graph::GraphError;
    match error {
        GraphError::UnavailableObject
        | GraphError::UnknownObjectAvailability
        | GraphError::ObjectUnavailable
        | GraphError::MissingObject { .. }
        | GraphError::PromisedObjectNotMaterialized { .. } => {
            FormatContextError::GraphContextUnavailable
        }
        GraphError::MalformedOid
        | GraphError::UnknownNodeKind(_)
        | GraphError::NonCanonicalData
        | GraphError::NodeDataTooLarge(_)
        | GraphError::FanoutExceeded(_)
        | GraphError::HeightExceeded(_)
        | GraphError::InvalidLength(_)
        | GraphError::InvalidCount(_)
        | GraphError::InvalidDomain
        | GraphError::OverlappingChildRange
        | GraphError::UncollapsedRoot
        | GraphError::InvalidDigestLength
        | GraphError::InvalidEmptyBlob
        | GraphError::DuplicateRowKey
        | GraphError::WrongNodeKind { .. }
        | GraphError::InvalidTreeEnvelope
        | GraphError::MissingNodeData
        | GraphError::GitObjectMalformed
        | GraphError::GitObjectHashMismatch
        | GraphError::ContentIdentityMismatch
        | GraphError::ChunkDigestMismatch => FormatContextError::GraphMalformed(error),
        _ => FormatContextError::GraphContextInvalid,
    }
}

impl SchemaRootPin {
    /// The snapshot pin from which this schema proof was admitted.
    pub fn snapshot_pin(&self) -> &RepositorySnapshotPin {
        &self.snapshot
    }
}

/// A read-only format context pinned to one previously committed snapshot.
///
/// Issued only by [`Repository::open_format_context_at_selector`], which
/// resolves its selector exactly once. The context delegates to the same
/// format admission used for `HEAD`, so a caller reads the historical schema,
/// row map, native graph and Blob ranges through the existing graph seam while
/// the readable snapshot coordinate is carried alongside it.
pub struct HistoricalFormatContext {
    snapshot_id: [u8; 32],
    context: RepositoryFormatContext,
}

impl HistoricalFormatContext {
    /// Canonical logical identity of the exact commit this context resolved.
    pub const fn snapshot_id(&self) -> &[u8; 32] {
        &self.snapshot_id
    }
    /// The repository-verified native Git commit resolved for this context.
    pub fn commit(&self) -> &crate::GitCommitRef {
        &self.context.snapshot.commit
    }

    pub(crate) fn commit_oid_hex(&self) -> &str {
        self.context.snapshot.commit.as_str()
    }
    /// The admitted repository-format dispatch at the pinned commit.
    pub const fn repository_format(&self) -> RepositoryFormat {
        self.context.repository_format()
    }

    /// The numeric repository-format coordinate at the pinned commit.
    pub const fn repository_format_number(&self) -> u8 {
        self.context.repository_format_number()
    }

    /// The database identity recorded by the pinned commit, when it carries one.
    pub const fn database_id(&self) -> Option<DatabaseId> {
        self.context.database_id()
    }

    /// Whether the pinned commit is a legacy format-1/2 compatibility input.
    /// Historical format-3 pins report `false` but remain equally immutable.
    pub const fn is_legacy_format(&self) -> bool {
        self.context.is_read_only()
    }

    /// The pinned context's schema root, when the snapshot carries one.
    pub fn validate_schema_root(&self) -> Result<SchemaRootPin, FormatContextError> {
        self.context.validate_schema_root()
    }

    /// The pinned context's `.orna/store` root.
    pub fn validate_store_root(&self) -> Result<StoreRootPin, FormatContextError> {
        self.context.validate_store_root()
    }

    /// Loads one relation's row map from the pinned snapshot's own store root.
    pub fn load_row_map(
        &self,
        relation_id: [u8; 16],
    ) -> Result<RowMapSnapshot, FormatContextError> {
        self.context.load_row_map(relation_id)
    }

    /// Opens a value context bound to this historical snapshot and relation.
    pub fn open_value_context(
        &self,
        relation_id: [u8; 16],
    ) -> Result<Format3Context, FormatContextError> {
        self.context.open_value_context(relation_id)
    }

    /// Resolves the committed descriptor selected by relation ID without
    /// claiming a source binding.
    pub fn resolve_committed_table_by_relation_id(
        &self,
        relation_id: [u8; 16],
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.context
            .resolve_committed_table_by_relation_id(relation_id)
    }

    pub fn bind_committed_table(
        &self,
        module_name: &str,
        source_path: &str,
        declaration_name: &str,
        table_object_id: [u8; 16],
        expected_database_id: DatabaseId,
        expected_snapshot_id: &[u8; 32],
        expected_schema: &SchemaDescriptor,
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.context.bind_committed_table(
            module_name,
            source_path,
            declaration_name,
            table_object_id,
            expected_database_id,
            expected_snapshot_id,
            expected_schema,
        )
    }

    pub fn bind_committed_table_by_relation_id(
        &self,
        relation_id: [u8; 16],
        expected_database_id: DatabaseId,
        expected_snapshot_id: &[u8; 32],
        expected_schema: &SchemaDescriptor,
    ) -> Result<CommittedTableBinding, FormatContextError> {
        self.context.bind_committed_table_by_relation_id(
            relation_id,
            expected_database_id,
            expected_snapshot_id,
            expected_schema,
        )
    }

    /// Issues graph authority bound to this pinned snapshot. Rows, schema and
    /// Blob references read through the returned context resolve against the
    /// pinned commit only, never the workspace `HEAD`.
    pub fn open_native_graph(
        &self,
        rows: &RowMapSnapshot,
    ) -> Result<NativeGraphContext, FormatContextError> {
        self.context.open_native_graph(rows)
    }

    /// Issues the graph authority of [`Self::open_native_graph`], owned by
    /// `owner_id`, for a mount that runs under that lease owner.
    pub fn open_native_graph_for_owner(
        &self,
        rows: &RowMapSnapshot,
        owner_id: [u8; 16],
    ) -> Result<NativeGraphContext, FormatContextError> {
        self.context.open_native_graph_for_owner(rows, owner_id)
    }

    /// Reads one committed path's bytes from the pinned commit only.
    ///
    /// Available for every admitted format, so a legacy format-1/2 pin is
    /// readable through its own path layout even though its row map and native
    /// graph are refused. The bytes come from the resolved commit, never the
    /// workspace `HEAD`, so this read cannot advance while the pin is held.
    pub fn read_committed_file(
        &self,
        path: impl AsRef<std::path::Path>,
        max_bytes: usize,
    ) -> Result<Vec<u8>, FormatContextError> {
        self.context.read_committed_file(path, max_bytes)
    }
}

impl fmt::Debug for HistoricalFormatContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HistoricalFormatContext")
            .field("format", &self.context.repository_format())
            .field("snapshot_id", &self.snapshot_id)
            .finish_non_exhaustive()
    }
}

impl StoreRootPin {
    /// The snapshot pin from which this store proof was admitted.
    pub fn snapshot_pin(&self) -> &RepositorySnapshotPin {
        &self.snapshot
    }
}

impl Repository {
    /// Refuses runtime admission when the format recorded at `HEAD` is not a
    /// format this build knows (ORNA-REPO-CONTEXT-003). It runs before any
    /// worktree metadata is read or any runtime state is opened, so a committed
    /// `HEAD` is never bypassed by the worktree (ORNA-UPGRADE-001).
    ///
    /// Fails closed: when `HEAD` tracks a `.orna/database.orna` or
    /// `.orna/format.orna` record that does not classify as a known format,
    /// whatever the cause (unknown number, string or overflowing integer,
    /// syntax-broken or open record, unreadable tree), the gate refuses with
    /// [`FormatContextError::UnknownFormat`]. A `HEAD` whose metadata mixes
    /// layouts (a legacy record beside `.orna/store`, ORNA-META-001) is refused
    /// with [`FormatContextError::MixedFormats`] (ORNA-REPO-CONTEXT-004), never
    /// reclassified as unknown. It falls through only when `HEAD` is unborn or
    /// tracks no format record at all. Those cases are decided by
    /// [`Self::require_writable_head_format`] and the metadata checks after it.
    pub fn require_known_head_format(&self) -> Result<(), FormatContextError> {
        match self.open_format_context() {
            Ok(_) => Ok(()),
            Err(error @ FormatContextError::MixedFormats { .. }) => Err(error),
            Err(FormatContextError::UnknownFormat) => Err(FormatContextError::UnknownFormat),
            Err(_) => self.refuse_unclassifiable_head_records(),
        }
    }

    /// Decides a `HEAD` whose format context did not open. An unborn `HEAD`
    /// (no pinnable snapshot) falls through. A `HEAD` that tracks any
    /// `.orna/database.orna` or `.orna/format.orna` record, or whose metadata
    /// cannot be read, is refused as an unknown format (fail closed).
    fn refuse_unclassifiable_head_records(&self) -> Result<(), FormatContextError> {
        let snapshot = match self.pin_snapshot("HEAD") {
            Ok(snapshot) => snapshot,
            Err(FormatContextError::SnapshotUnavailable) => return Ok(()),
            Err(_) => return Err(FormatContextError::UnknownFormat),
        };
        match read_snapshot_metadata(self, &snapshot) {
            Ok(metadata) if metadata.database.is_none() && metadata.legacy_format.is_none() => {
                Ok(())
            }
            _ => Err(FormatContextError::UnknownFormat),
        }
    }

    /// Admits durable runtime writes only when the format recorded at `HEAD`
    /// has a writer. A format-1/2 `HEAD` is refused with
    /// [`LegacyWriteRefused`] (ORNA-UPGRADE-001, ORNA-UPGRADE-003), whatever
    /// metadata the worktree holds.
    ///
    /// An unborn `HEAD`, a format-3 `HEAD` and unreadable metadata are not
    /// decided here; the metadata checks that follow report them. A prepared
    /// migration copy is not admitted here: its `HEAD` is still the legacy
    /// format, so ordinary durable writes stay refused (ORNA-UPGRADE-001,
    /// ORNA-UPGRADE-003). Only [`Self::require_migration_carry_head_format`]
    /// admits a prepared copy, for the migration's own checkpoint carry.
    pub fn require_writable_head_format(&self) -> Result<(), LegacyWriteRefused> {
        let Ok(context) = self.open_format_context() else {
            return Ok(());
        };
        if !context.is_read_only() {
            return Ok(());
        }
        Err(LegacyWriteRefused {
            format: context.repository_format_number(),
        })
    }

    /// Admits the migration's checkpoint carry only into a prepared copy
    /// ([`Self::holds_prepared_migration`]) whose recorded `HEAD` format is
    /// legacy (ORNA-UPGRADE-001). A writable `HEAD` is admitted as usual, and
    /// an unreadable context is decided by [`Self::require_writable_head_format`].
    pub fn require_migration_carry_head_format(&self) -> Result<(), LegacyWriteRefused> {
        let Ok(context) = self.open_format_context() else {
            return self.require_writable_head_format();
        };
        if !context.is_read_only() || self.holds_prepared_migration() {
            return Ok(());
        }
        Err(LegacyWriteRefused {
            format: context.repository_format_number(),
        })
    }

    /// Whether this worktree holds a prepared format migration: a readable
    /// publication journal that records migration continuity, as
    /// `orna migrate copy` leaves in its destination. The journal lives in the
    /// runtime directory, so without one nothing is read and nothing, not
    /// even a coordination lock, is created.
    pub fn holds_prepared_migration(&self) -> bool {
        self.runtime_paths().root().exists()
            && self.read_publication_journal().is_ok_and(|journal| {
                journal.is_some_and(|journal| journal.migration_continuity().is_some())
            })
    }

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
    pub fn open_private_candidate_format_context(
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

    /// Reads the parsed repository-format number for an immutable private candidate.
    pub fn private_candidate_repository_format_number(
        &self,
        candidate: &crate::PrivateCommit,
    ) -> Result<u8, FormatContextError> {
        self.open_private_candidate_format_context(candidate)
            .map(|context| context.repository_format_number())
    }

    /// Validates the exact staged index tree a reviewed commit will record,
    /// before any commit object names it. The administration `commit` builds
    /// its candidate, validates it, and only then creates the commit, so a
    /// refused candidate leaves no object or ref behind (16-administration).
    ///
    /// The checks are the ones a pinned format-3 context applies: admitted
    /// format-3 metadata, a bounded `main.orna` schema root, and an
    /// `.orna/store` tree. Unlike reading a snapshot, the gate admits no
    /// transitional `.orna/` entry: a reviewed commit tracks only
    /// `database.orna` and `store` there (ORNA-META-001), so this writer never
    /// produces a mixed snapshot.
    pub(crate) fn validate_format3_index_tree(
        &self,
        tree: &crate::IndexTreeRef,
    ) -> Result<(), FormatContextError> {
        let metadata = read_tracked_metadata(self.objects(), tree.as_str())?;
        let (format, _) = admit_metadata(&metadata, &[])?;
        if !format.supports_writes() {
            return Err(FormatContextError::LegacyReadOnly(format.number()));
        }
        self.read_tree_file(tree.as_str(), MAIN_SOURCE_PATH, MAX_SCHEMA_ROOT_BYTES)
            .map_err(|error| match error {
                RepositoryError::GitUnavailable => FormatContextError::SchemaRootUnavailable,
                _ => FormatContextError::SchemaRootInvalid,
            })?;
        let store =
            crate::ManagedPath::new(STORE_PATH).map_err(|_| FormatContextError::StoreRootInvalid)?;
        match self.tree_entry(tree.as_str(), &store) {
            Ok(Some((mode, _))) if mode == "040000" => Ok(()),
            Err(RepositoryError::GitUnavailable) => Err(FormatContextError::StoreRootUnavailable),
            _ => Err(FormatContextError::StoreRootInvalid),
        }
    }

    /// Opens and validates format metadata at the current immutable HEAD.
    /// Worktree-only metadata is not silently treated as a committed snapshot.
    pub fn open_format_context(&self) -> Result<RepositoryFormatContext, FormatContextError> {
        let snapshot = self.pin_snapshot("HEAD")?;
        self.open_format_context_at(&snapshot)
    }

    /// Pins one reachable selector and opens its format metadata, so a caller
    /// can read a named snapshot instead of whatever `HEAD` becomes later.
    ///
    /// Resolving happens once, here: the returned context is bound to the
    /// commit the selector named, and never follows a branch that advances
    /// during the read.
    pub fn open_pinned_format_context(
        &self,
        selector: &str,
    ) -> Result<RepositoryFormatContext, FormatContextError> {
        let snapshot = self.pin_snapshot(selector)?;
        self.open_format_context_at(&snapshot)
    }

    /// Resolves a named snapshot once and returns its exact commit together with
    /// the format context admitted from that same immutable pin.
    ///
    /// History readers need the commit coordinate for revision walks as well as
    /// the context for row-map reads. Returning both from one repository-issued
    /// pin prevents a moving ref from selecting one commit for the walk and a
    /// second commit for the schema or `.orna/store` root.
    pub fn open_pinned_format_context_with_commit(
        &self,
        selector: &str,
    ) -> Result<(crate::GitCommitRef, RepositoryFormatContext), FormatContextError> {
        let snapshot = self.pin_snapshot(selector)?;
        let commit = snapshot.commit.clone();
        let context = self.open_format_context_at(&snapshot)?;
        Ok((commit, context))
    }

    /// Resolves one commit selector exactly once and opens its immutable
    /// format context read-only.
    ///
    /// This is the CHECKOUT-3 historical pin: `selector` is resolved a single
    /// time to one exact commit, and every later read through the returned
    /// context — schema root, store root, row map, native graph and Blob
    /// ranges — is served from that commit's own objects. A branch or tag name
    /// therefore cannot move during reads, and the current `HEAD`, index and
    /// worktree are never consulted or modified, so one repository can serve a
    /// historical view beside its workspace view (ORNA-CHECKOUT-003).
    ///
    /// Write admission is deliberately unavailable: the returned context is a
    /// historical pin, so [`RepositoryFormatContext::supports_writes`] reports
    /// `false` and every write seam refuses it. Only
    /// [`Repository::open_format_context`] admits the writer coordinate.
    pub fn open_format_context_at_selector(
        &self,
        selector: &str,
    ) -> Result<HistoricalFormatContext, FormatContextError> {
        if selector.is_empty() || selector.starts_with('-') || selector.contains('\0') {
            return Err(FormatContextError::SnapshotInvalid);
        }
        let snapshot = self.pin_snapshot(selector)?;
        let context = self.open_format_context_at(&snapshot)?;
        Ok(HistoricalFormatContext {
            snapshot_id: snapshot.snapshot_id(),
            context,
        })
    }

    /// Issues capture authority for one format-3 relation at the current
    /// HEAD. The row map, native graph, and read scope all come from this
    /// repository's own format context, so callers supply only the relation.
    pub fn capture_capability(
        &self,
        relation_id: [u8; 16],
    ) -> Result<RepositoryCaptureCapability, CaptureCapabilityError> {
        self.open_format_context()
            .map_err(CaptureCapabilityError::Format)?
            .capture_capability(relation_id)
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
        let metadata = read_snapshot_metadata(self, snapshot)?;
        parse_context(self.clone(), snapshot.clone(), &metadata)
    }
}

fn repository_hash_algorithm(
    repository: &Repository,
) -> Result<GitHashAlgorithm, FormatContextError> {
    repository
        .objects()
        .algorithm()
        .map_err(|error| match error {
            RepositoryError::UnsupportedObjectFormat => FormatContextError::GraphContextInvalid,
            _ => FormatContextError::GraphContextUnavailable,
        })
}

/// Resolves one fixed snapshot path to its object through the repository's
/// object reader, and checks the object type.
fn committed_path_oid(
    repository: &Repository,
    commit: &crate::GitCommitRef,
    algorithm: GitHashAlgorithm,
    path: &str,
    expected_kind: &str,
) -> Result<NativeOid, FormatContextError> {
    let object = repository
        .objects()
        .info(&format!("{}:{path}", commit.as_str()))
        .map_err(|_| FormatContextError::GraphContextUnavailable)?
        .ok_or(FormatContextError::GraphContextUnavailable)?;
    if object.kind().name() != expected_kind {
        return Err(FormatContextError::GraphContextInvalid);
    }
    NativeOid::from_hex(algorithm, object.oid())
        .map_err(|_| FormatContextError::GraphContextInvalid)
}

fn map_snapshot_error(error: RepositoryError) -> FormatContextError {
    match error {
        RepositoryError::GitUnavailable
        | RepositoryError::SnapshotNotFound
        | RepositoryError::UnbornHead => FormatContextError::SnapshotUnavailable,
        _ => FormatContextError::SnapshotInvalid,
    }
}

/// One root tree's tracked `.orna` directory as format admission reads it:
/// the two format records and every other entry, in Git tree order.
struct TrackedMetadata {
    database: Option<Vec<u8>>,
    legacy_format: Option<Vec<u8>>,
    others: Vec<TreeEntry>,
}

/// Reads the tracked metadata of one pinned snapshot: its commit's root tree,
/// or a private candidate's exact tree.
fn read_snapshot_metadata(
    repository: &Repository,
    snapshot: &RepositorySnapshotPin,
) -> Result<TrackedMetadata, FormatContextError> {
    let objects = repository.objects();
    let root = match &snapshot.private_candidate {
        Some(candidate) => candidate.tree().as_str().to_owned(),
        None => objects
            .info(&format!("{}^{{tree}}", snapshot.commit.as_str()))
            .map_err(map_metadata_read_error)?
            .filter(|tree| tree.kind() == ObjectKind::Tree)
            .ok_or(FormatContextError::MetadataInvalid)?
            .oid()
            .to_owned(),
    };
    read_tracked_metadata(objects, &root)
}

/// Reads the `.orna` directory of one root tree.
///
/// Admission needs the two format records, `.orna/database.orna` and the
/// legacy `.orna/format.orna` (ORNA-META-002), and the names of the other
/// entries, because the entries a snapshot may track depend on the format the
/// records select (ORNA-META-001). It reads the root tree, the `.orna` tree
/// and at most the two record blobs through the repository's object reader,
/// and never lists the snapshot recursively: the format-3 store beside them
/// grows with the rows it holds, and its size must not decide whether
/// metadata can be read (ORNA-ROW-009).
fn read_tracked_metadata(
    objects: &ObjectReader,
    root: &str,
) -> Result<TrackedMetadata, FormatContextError> {
    let mut metadata = TrackedMetadata {
        database: None,
        legacy_format: None,
        others: Vec::new(),
    };
    let entries = match objects
        .find_tree_entry(root, METADATA_DIRECTORY.as_bytes())
        .map_err(map_metadata_read_error)?
    {
        Some(directory) if directory.mode() == TreeEntryMode::Tree => objects
            .tree_entries(directory.oid())
            .map_err(map_metadata_read_error)?,
        // A snapshot without a `.orna` directory records no metadata.
        _ => return Ok(metadata),
    };
    for entry in entries {
        let record = if entry.name() == DATABASE_FILE.as_bytes() {
            &mut metadata.database
        } else if entry.name() == LEGACY_FORMAT_FILE.as_bytes() {
            &mut metadata.legacy_format
        } else {
            metadata.others.push(entry);
            continue;
        };
        // Git never writes two entries with one name; a tree that does holds
        // malformed metadata rather than either record.
        if record.is_some() {
            return Err(FormatContextError::MetadataInvalid);
        }
        *record = Some(read_metadata_blob(objects, &entry)?);
    }
    Ok(metadata)
}

/// Reads one metadata blob under the record bound. A metadata path that holds
/// a directory, symlink or submodule is present but invalid.
fn read_metadata_blob(
    objects: &ObjectReader,
    entry: &TreeEntry,
) -> Result<Vec<u8>, FormatContextError> {
    if !matches!(
        entry.mode(),
        TreeEntryMode::File | TreeEntryMode::Executable
    ) {
        return Err(FormatContextError::MetadataInvalid);
    }
    objects
        .read_blob(entry.oid(), FORMAT_CONTEXT_MAX_METADATA_BYTES)
        .map_err(map_metadata_read_error)
}

fn map_metadata_read_error(error: RepositoryError) -> FormatContextError {
    match error {
        RepositoryError::GitUnavailable => FormatContextError::MetadataUnavailable,
        _ => FormatContextError::MetadataInvalid,
    }
}

/// Selects the repository format and database identity the tracked metadata
/// records, and refuses a snapshot whose `.orna` directory mixes layouts
/// (ORNA-META-001, ORNA-META-002; Gate A, "unknown/mixed formats fail
/// early"). A format-3 snapshot tracks only `database.orna`, `store` and the
/// `tolerated` paths there. A legacy snapshot never tracks the format-3
/// `store`, which no format-1/2 writer produced, so format-3 bytes relabelled
/// with a legacy record do not open as a legacy snapshot (MIGRATION.md).
fn admit_metadata(
    metadata: &TrackedMetadata,
    tolerated: &[&str],
) -> Result<(RepositoryFormat, Option<DatabaseId>), FormatContextError> {
    let legacy = metadata
        .legacy_format
        .as_deref()
        .map(parse_legacy_format)
        .transpose()?;
    let (format, database_id) = match (metadata.database.as_deref(), legacy) {
        (None, None) => return Err(FormatContextError::MetadataUnavailable),
        (None, Some(format)) => (format, None),
        (Some(database), None) => match parse_database_record(database)? {
            DatabaseRecord::Format3(database_id) => (RepositoryFormat::Format3, Some(database_id)),
            DatabaseRecord::LegacySidecar(_) => return Err(FormatContextError::MetadataInvalid),
        },
        (Some(database), Some(format)) => match parse_database_record(database)? {
            DatabaseRecord::Format3(_) => {
                return Err(mixed_metadata_entry(LEGACY_FORMAT_FILE.as_bytes()));
            }
            DatabaseRecord::LegacySidecar(database_id) => (format, Some(database_id)),
        },
    };
    let foreign = metadata.others.iter().map(TreeEntry::name).find(|name| {
        if format == RepositoryFormat::Format3 {
            *name != STORE_ENTRY.as_bytes()
                && !tolerated.iter().any(|path| {
                    path.strip_prefix(METADATA_DIRECTORY)
                        .and_then(|entry| entry.strip_prefix('/'))
                        .is_some_and(|entry| entry.as_bytes() == *name)
                })
        } else {
            *name == STORE_ENTRY.as_bytes()
        }
    });
    match foreign {
        Some(name) => Err(mixed_metadata_entry(name)),
        None => Ok((format, database_id)),
    }
}

/// Mixed formats at one `.orna` entry, named by its repository-relative path.
fn mixed_metadata_entry(name: &[u8]) -> FormatContextError {
    FormatContextError::MixedFormats {
        path: format!("{METADATA_DIRECTORY}/{}", String::from_utf8_lossy(name)),
    }
}

fn parse_context(
    repository: Repository,
    snapshot: RepositorySnapshotPin,
    metadata: &TrackedMetadata,
) -> Result<RepositoryFormatContext, FormatContextError> {
    let (format, database_id) = admit_metadata(metadata, &TRANSITIONAL_FORMAT3_ENTRIES)?;
    Ok(RepositoryFormatContext {
        repository,
        format,
        database_id,
        snapshot,
        runtime_owner_id: None,
    })
}

enum DatabaseRecord {
    Format3(DatabaseId),
    LegacySidecar(DatabaseId),
}

/// Parses a `.orna/database.orna` record. Only the canonical format-3 record
/// and the one-field legacy sidecar are admitted. A record that names format 1
/// or 2 beside its `database_id` is refused as an unknown format, never read
/// as a legacy layout: legacy layouts are read only from `.orna/format.orna`
/// (ORNA-META-002; pinned by `refuses_legacy_format_numbers_inside_a_database_record`).
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

/// One field value of a closed legacy `format.orna` record.
#[derive(Clone, Copy)]
enum LegacyRecordValue {
    Integer(i64),
    Text(&'static str),
}

/// The `format.orna` records the format-1/2 writers produced. They stay
/// readable inputs (ORNA-UPGRADE-001, ORNA-UPGRADE-002, ORNA-META-002), and
/// retained format-1/2 commits are decoded by their original storage profiles
/// (`compatibility/README.md`). OrnaDB 1.0's `orna init` wrote the format-1
/// record. The format-2 record is the closed capability record of the
/// previous 1.1.0 draft (its ORNA-OGB-001). Each record is closed: it holds
/// exactly these fields with these values, so an unlisted profile is never
/// read as a known layout.
const LEGACY_FORMAT_RECORDS: [(RepositoryFormat, &[(&str, LegacyRecordValue)]); 2] = [
    (
        RepositoryFormat::Legacy1,
        &[
            ("repository_format", LegacyRecordValue::Integer(1)),
            (
                "storage_profile",
                LegacyRecordValue::Text("compact-storage-v1"),
            ),
        ],
    ),
    (
        RepositoryFormat::Legacy2,
        &[
            ("repository_format", LegacyRecordValue::Integer(2)),
            ("row_profile", LegacyRecordValue::Text("ORR-2")),
            ("blob_profile", LegacyRecordValue::Text("OGB-1")),
            ("semantic_profile", LegacyRecordValue::Text("SOV-2")),
            (
                "compact_profile",
                LegacyRecordValue::Text("compact-storage-v2"),
            ),
        ],
    ),
];

pub(super) fn parse_legacy_format(bytes: &[u8]) -> Result<RepositoryFormat, FormatContextError> {
    let fields = parse_record(bytes).map_err(|()| FormatContextError::MetadataInvalid)?;
    LEGACY_FORMAT_RECORDS
        .iter()
        .find(|(_, expected)| is_closed_record(&fields, expected))
        .map(|(format, _)| *format)
        .ok_or(FormatContextError::MetadataInvalid)
}

/// Whether `fields` holds each expected field exactly once, with its value,
/// and nothing else.
fn is_closed_record(fields: &[RecordField], expected: &[(&str, LegacyRecordValue)]) -> bool {
    fields.len() == expected.len()
        && expected.iter().all(|(name, value)| {
            let mut named = fields.iter().filter(|field| field.name == *name);
            let (Some(field), None) = (named.next(), named.next()) else {
                return false;
            };
            match value {
                LegacyRecordValue::Integer(expected) => {
                    integer_literal(&field.value) == Some(*expected)
                }
                LegacyRecordValue::Text(expected) => {
                    string_literal(&field.value) == Some(*expected)
                }
            }
        })
}

fn parse_database_id(value: &Expr) -> Result<DatabaseId, FormatContextError> {
    let text = string_literal(value).ok_or(FormatContextError::MetadataInvalid)?;
    DatabaseId::from_str(text).map_err(|_| FormatContextError::MetadataInvalid)
}

fn schema_descriptor_table_id(descriptor: &SchemaDescriptor) -> Option<[u8; 16]> {
    let Raw::Tag(37, value) = schema_descriptor_entry(descriptor, 1)? else {
        return None;
    };
    let Raw::Bytes(bytes) = value.as_ref() else {
        return None;
    };
    bytes.as_slice().try_into().ok()
}

fn schema_descriptor_entry(descriptor: &SchemaDescriptor, field_number: u8) -> Option<&Raw> {
    let Raw::Map(fields) = descriptor.raw() else {
        return None;
    };
    let key = Raw::Int(field_number.into());
    fields
        .iter()
        .find_map(|(candidate, value)| (candidate == &key).then_some(value))
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

pub(crate) fn canonical_database_bytes(database_id: &DatabaseId) -> Vec<u8> {
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

/// The database identity of either a canonical format-3 record or a preserved
/// legacy sidecar. A pending migration candidate carries the sidecar identity
/// forward, so the runtime identity must read it from there.
pub(super) fn parse_any_database_id(bytes: &[u8]) -> Result<DatabaseId, ()> {
    match parse_database_record(bytes).map_err(|_| ())? {
        DatabaseRecord::Format3(database_id) | DatabaseRecord::LegacySidecar(database_id) => {
            Ok(database_id)
        }
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

    use super::{FormatContextError, RepositoryFormatContext, TRANSITIONAL_FORMAT3_ENTRIES};
    use crate::{
        Repository,
        native_graph::{
            ByteIndexEntry, CborValue, GitHashAlgorithm, MAX_RANGE_GRAPH_OBJECTS, NativeOid,
            NodeData, OrderedBranchEntry, OrderedLeafEntry, RowIndexMutation,
            decode_canonical_cbor,
        },
        row_store::{
            KeyRange, MAX_ROW_RANGE_LIMIT, RowEntry, RowMapSnapshot, RowMapVersion, RowValue,
            SchemaGeneration, TypedKey,
        },
    };
    use orna_value_v1::{Raw, SchemaDescriptor};

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

    #[test]
    fn final_format_capability_is_issued_for_the_admitted_format3_identity() {
        let directory = repository();
        let context = context(directory.path());
        let capability = context
            .final_format_capability()
            .expect("admitted format-3 context issues the final capability");
        assert_eq!(
            capability.database_id(),
            context
                .require_database_id()
                .expect("admitted format-3 identity")
        );
    }

    /// store-format-3 §1 bounds only `database.orna` at 64 KiB. A larger
    /// `main.orna` still pins the schema root, loads the row map and opens
    /// the graph (A07-storage-rows-09).
    #[test]
    fn a_main_source_over_64_kib_still_opens_the_row_map() {
        let directory = repository();
        let root = directory.path();
        let mut source = MAIN_SOURCE.to_owned();
        while source.len() <= 64 * 1024 {
            source.push_str("// Padding that keeps this module source above 64 KiB.\n");
        }
        fs::write(root.join("main.orna"), &source).expect("write the oversized source");
        commit(root);

        let relation_id = [0x5a; 16];
        let initial = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &initial, relation_id);
        let algorithm = initial
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let rows = rows_leaf(
            root,
            algorithm,
            &row_domain(relation_id, schema_digest),
            &[(1, inline_value())],
        );
        install_rows_store(root, &initial, relation_id, &schema_oid, &rows, 1);

        let pinned = context(root);
        let schema_root = pinned
            .validate_schema_root()
            .expect("an oversized source still pins the schema root");
        let head_source = git_output(root, &["rev-parse", "HEAD:main.orna"], None);
        assert_eq!(
            schema_root.oid.to_hex(),
            std::str::from_utf8(&head_source)
                .expect("rev-parse prints an OID")
                .trim()
        );
        let snapshot = pinned
            .load_row_map(relation_id)
            .expect("the row map loads beside an oversized source");
        let graph = pinned
            .open_native_graph(&snapshot)
            .expect("the graph opens beside an oversized source");
        let scope = graph.open_read_scope().expect("owner read scope");
        assert!(
            graph
                .lookup_row(&TypedKey::UInt(1), &scope)
                .expect("bounded point lookup")
                .is_some()
        );
    }

    /// ORNA-META-001: format-3 tracked `.orna/` holds only `database.orna`
    /// and `store`. Any other entry, such as the format-1 compact `storage`
    /// layout, the draft format-2 `blobs` tree or a stray file, mixes formats:
    /// the snapshot fails admission at `HEAD`, as a historical pin and at the
    /// commit gate, with ORNA-REPO-CONTEXT-004 naming the entry (Gate A,
    /// "unknown/mixed formats fail early").
    #[test]
    fn format3_snapshot_with_extra_metadata_child_is_rejected() {
        for (entry, file) in [
            ("storage", "storage/Music/manifest.orna"),
            ("blobs", "blobs/ab/cdef"),
            ("notes.txt", "notes.txt"),
        ] {
            let directory = repository();
            let root = directory.path();
            let path = root.join(".orna").join(file);
            fs::create_dir_all(path.parent().expect("entry parent")).expect("create entry parent");
            fs::write(&path, b"not format-3 metadata").expect("write the extra entry");
            commit(root);
            let repository = Repository::discover(root).expect("discover fixture");
            let expected = FormatContextError::MixedFormats {
                path: format!(".orna/{entry}"),
            };

            let refused = repository
                .open_format_context()
                .expect_err("HEAD mixes formats");
            assert_eq!(refused, expected);
            assert_eq!(refused.code(), "ORNA-REPO-CONTEXT-004");
            assert!(
                refused.to_string().contains(&format!(".orna/{entry}")),
                "the message names the entry: {refused}"
            );
            assert_eq!(
                repository
                    .open_format_context_at_selector("HEAD")
                    .expect_err("the historical pin mixes formats"),
                expected
            );
            let staged = repository
                .index_generation()
                .expect("read the index generation")
                .tree()
                .cloned()
                .expect("the index names a tree");
            assert_eq!(
                repository.validate_format3_index_tree(&staged),
                Err(expected)
            );
        }
    }

    /// `.orna/packages` (the package pin manifest, written by `orna-project-v1`
    /// attachments) and `.orna/checkpoints` (the runtime checkpoint snapshot
    /// directory) are tracked by pre-META-001 writers. ORNA-META-001 admits only
    /// `database.orna` and `store` under format-3 `.orna/`, so a tracked snapshot
    /// carrying either is refused with the offending entry named, not widened
    /// into the transitional allow-list.
    #[test]
    fn format3_snapshot_with_packages_or_checkpoints_is_rejected() {
        for (entry, file) in [
            ("packages", "packages"),
            ("checkpoints", "checkpoints/snapshot.orna"),
        ] {
            let directory = repository();
            let root = directory.path();
            let path = root.join(".orna").join(file);
            fs::create_dir_all(path.parent().expect("entry parent")).expect("create entry parent");
            fs::write(&path, b"not format-3 metadata").expect("write the extra entry");
            commit(root);
            let repository = Repository::discover(root).expect("discover fixture");
            let refused = repository
                .open_format_context()
                .expect_err("HEAD mixes formats");
            assert_eq!(
                refused,
                FormatContextError::MixedFormats {
                    path: format!(".orna/{entry}"),
                }
            );
            assert_eq!(refused.code(), "ORNA-REPO-CONTEXT-004");
            assert!(
                refused.to_string().contains(&format!(".orna/{entry}")),
                "the message names the entry: {refused}"
            );
        }
    }

    /// The transitional `.orna/` entries are exactly the two this build still
    /// writes, each removed by the work named in `TRANSITIONAL_FORMAT3_ENTRIES`.
    /// A snapshot carrying one stays readable, but the commit gate refuses it,
    /// so no reviewed commit adds one. Retiring a writer must retire its entry
    /// here.
    #[test]
    fn transitional_format3_entries_are_pinned_and_refused_by_the_commit_gate() {
        assert_eq!(
            TRANSITIONAL_FORMAT3_ENTRIES,
            [".orna/object-identities.cbor", ".orna/migration.json"]
        );
        for entry in TRANSITIONAL_FORMAT3_ENTRIES {
            let directory = repository();
            let root = directory.path();
            let repository = Repository::discover(root).expect("discover fixture");
            let staged = || {
                repository
                    .index_generation()
                    .expect("read the index generation")
                    .tree()
                    .cloned()
                    .expect("the index names a tree")
            };
            assert_eq!(
                repository.validate_format3_index_tree(&staged()),
                Ok(()),
                "the gate admits a plain format-3 tree"
            );

            fs::write(root.join(entry), b"transitional metadata")
                .expect("write the transitional entry");
            commit(root);
            assert_eq!(
                repository
                    .open_format_context()
                    .expect("a transitional entry stays readable")
                    .repository_format_number(),
                3
            );
            assert_eq!(
                repository.validate_format3_index_tree(&staged()),
                Err(FormatContextError::MixedFormats {
                    path: entry.to_owned()
                })
            );
        }
    }

    /// A legacy marker beside `.orna/store` is a mixed snapshot on every
    /// admission path, with the same error code: the commit gate refuses the
    /// staged tree before any commit object exists, a committed snapshot is
    /// refused by the snapshot reader, and runtime admission refuses `HEAD`
    /// (ORNA-META-001, ORNA-META-002; ORNA-REPO-CONTEXT-004).
    #[test]
    fn legacy_marker_beside_store_is_refused_on_every_admission_path() {
        const FORMAT_ONE: &str =
            include_str!("../tests/fixtures/format-context/format-1-ornadb-1.0.orna");
        const FORMAT_TWO: &str =
            include_str!("../tests/fixtures/format-context/format-2-draft-1.1.0.orna");
        for record in [FORMAT_ONE, FORMAT_TWO] {
            let directory = repository();
            let root = directory.path();
            fs::write(root.join(".orna/format.orna"), record).expect("write legacy marker");
            git(root, &["add", "."]);
            let repository = Repository::discover(root).expect("discover fixture");
            let staged = repository
                .index_generation()
                .expect("read the index generation")
                .tree()
                .cloned()
                .expect("the index names a tree");
            let staged_error = repository
                .validate_format3_index_tree(&staged)
                .expect_err("the commit gate refuses the staged mixed tree");
            assert!(
                matches!(staged_error, FormatContextError::MixedFormats { .. }),
                "{staged_error:?}"
            );
            assert_eq!(staged_error.code(), "ORNA-REPO-CONTEXT-004");

            git(
                root,
                &["commit", "--quiet", "-m", "legacy marker beside store"],
            );
            let refused = repository
                .open_format_context()
                .expect_err("the snapshot reader refuses HEAD");
            assert!(
                matches!(refused, FormatContextError::MixedFormats { .. }),
                "{refused:?}"
            );
            assert_eq!(refused.code(), "ORNA-REPO-CONTEXT-004");
            assert_eq!(
                repository.require_known_head_format(),
                Err(refused),
                "runtime admission refuses HEAD as ORNA-REPO-CONTEXT-004"
            );
        }
    }

    #[test]
    fn committed_table_bindings_pin_imported_source_repository_uuid_and_schema() {
        let relation_id = [0xa1; 16];
        let (directory, descriptor) = seeded_table_binding(relation_id);
        let root = directory.path();
        let repository = Repository::discover(root).expect("discover fixture");
        let context = repository.open_format_context().expect("admit fixture");
        let database_id = context
            .require_database_id()
            .expect("admitted repository UUID");
        let snapshot_id = context.snapshot_pin().snapshot_id();

        let resolved = context
            .resolve_committed_table_by_relation_id(relation_id)
            .expect("resolve descriptor by committed relation ID");
        assert_eq!(resolved.database_id(), database_id);
        assert_eq!(resolved.snapshot_id(), &snapshot_id);
        assert_eq!(resolved.module_name(), None);
        assert_eq!(resolved.source_path(), None);
        assert_eq!(resolved.declaration_name(), None);
        assert_eq!(resolved.table_object_id(), &relation_id);
        assert_eq!(resolved.descriptor(), &descriptor);

        let binding = context
            .bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &snapshot_id,
                &descriptor,
            )
            .expect("bind imported source analysis to committed descriptor");
        assert_eq!(binding.database_id(), database_id);
        assert_eq!(binding.snapshot_id(), &snapshot_id);
        assert_eq!(binding.module_name(), Some("media"));
        assert_eq!(binding.source_path(), Some("media.orna"));
        assert_eq!(binding.declaration_name(), Some("Audio"));
        assert_eq!(binding.descriptor(), &descriptor);

        assert_eq!(
            context.bind_committed_table(
                "other",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &snapshot_id,
                &descriptor,
            ),
            Err(super::FormatContextError::TableBindingMismatch),
            "binding rejects a namespace/path disagreement"
        );
        let mut wrong_snapshot = snapshot_id;
        wrong_snapshot[0] ^= 1;
        assert_eq!(
            context.bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &wrong_snapshot,
                &descriptor,
            ),
            Err(super::FormatContextError::TableBindingMismatch),
            "binding rejects a foreign source snapshot"
        );

        let wrong_descriptor = descriptor_for_table(relation_id, "Str");
        assert_eq!(
            context.bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &snapshot_id,
                &wrong_descriptor,
            ),
            Err(super::FormatContextError::TableBindingMismatch),
            "binding rejects a differing field type under the same table ID"
        );
        let other_id = [0xa2; 16];
        let wrong_table_descriptor = descriptor_for_table(other_id, "Int");
        assert_eq!(
            context.bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &snapshot_id,
                &wrong_table_descriptor,
            ),
            Err(super::FormatContextError::TableBindingMismatch),
            "binding rejects an expected descriptor for another table"
        );
        assert_eq!(
            context.bind_committed_table(
                "media",
                "media.orna",
                "Missing",
                relation_id,
                database_id,
                &snapshot_id,
                &descriptor,
            ),
            Err(super::FormatContextError::TableBindingMismatch),
            "binding rejects a source declaration absent from the imported module"
        );

        let foreign_database_id = super::DatabaseId::new_v4();
        let large_source = format!(
            "{}{}",
            " ".repeat(65 * 1024),
            include_str!("../tests/fixtures/committed-table-binding.orna")
        );
        fs::write(root.join("media.orna"), large_source)
            .expect("write valid imported source above the old bound");
        // `commit_store_root` advances HEAD without refreshing the worktree/index.
        // Reset the index to that committed graph before staging only the source change.
        git(root, &["read-tree", "HEAD"]);
        git(root, &["add", "--", "media.orna"]);
        git(
            root,
            &["commit", "--quiet", "-m", "advance imported source fixture"],
        );
        let large_context = Repository::discover(root)
            .expect("discover large imported module")
            .open_format_context()
            .expect("open large imported-module context");
        let large_snapshot_id = large_context.snapshot_pin().snapshot_id();
        let large_binding = large_context
            .bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &large_snapshot_id,
                &descriptor,
            )
            .expect("bind valid imported source within project byte budget");
        assert_eq!(large_binding.descriptor(), &descriptor);

        fs::write(
            root.join(".orna/database.orna"),
            super::canonical_database_bytes(&foreign_database_id),
        )
        .expect("write another repository UUID");
        commit(root);
        let foreign_context = Repository::discover(root)
            .expect("discover alternate UUID fixture")
            .open_format_context()
            .expect("open alternate UUID context");
        assert_eq!(
            foreign_context.require_database_id(),
            Ok(foreign_database_id),
            "fixture opens the alternate repository UUID"
        );
        assert_ne!(foreign_database_id, database_id);
        assert_eq!(
            context.bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                foreign_database_id,
                &snapshot_id,
                &descriptor,
            ),
            Err(super::FormatContextError::TableBindingMismatch),
            "binding rejects a foreign repository UUID"
        );

        fs::write(root.join("media.orna"), "pub table Audio(id: Int)\n")
            .expect("write malformed imported table source");
        commit(root);
        let malformed_context = Repository::discover(root)
            .expect("discover malformed imported module")
            .open_format_context()
            .expect("open malformed imported-module context");
        let malformed_snapshot_id = malformed_context.snapshot_pin().snapshot_id();
        assert_eq!(
            malformed_context.bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                foreign_database_id,
                &malformed_snapshot_id,
                &descriptor,
            ),
            Err(super::FormatContextError::SchemaRootInvalid),
            "binding rejects a recovered declaration from a malformed module"
        );
    }

    fn context(directory: &Path) -> RepositoryFormatContext {
        Repository::discover(directory)
            .expect("discover fixture repository")
            .open_format_context()
            .expect("admit fixture format context")
    }

    fn descriptor_for_table(relation_id: [u8; 16], key_type: &str) -> SchemaDescriptor {
        let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
        let field = |id: [u8; 16], name: &str, ty: Raw, role: u8| {
            Raw::Array(vec![
                uuid(id),
                Raw::Text(name.to_owned()),
                ty,
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid(relation_id)),
            (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
            (
                Raw::Int(3.into()),
                Raw::Array(vec![
                    field(
                        [0x11; 16],
                        "id",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text(key_type.into())]),
                        0,
                    ),
                    field(
                        [0x22; 16],
                        "payload",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".into())]),
                        1,
                    ),
                ]),
            ),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("valid stored schema descriptor")
    }

    fn seeded_table_binding(relation_id: [u8; 16]) -> (TempDir, SchemaDescriptor) {
        let directory = repository();
        let root = directory.path();
        let source = include_str!("../tests/fixtures/committed-table-binding.orna");
        fs::write(root.join("main.orna"), "use media;\n").expect("write importing root module");
        fs::write(root.join("media.orna"), source).expect("write imported table module");
        commit(root);
        let initial = context(root);
        let algorithm = initial
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let descriptor = descriptor_for_table(relation_id, "Int");
        let schema_bytes = descriptor.encode().expect("encode schema descriptor");
        let schema_digest = crate::native_graph::schema_identity_v3(&schema_bytes);
        let schema_oid = schema_descriptor_node(
            root,
            algorithm,
            &schema_bytes,
            schema_bytes.len() as u64,
            schema_digest,
        );
        let primary_root = write_native_node(
            root,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: row_domain(relation_id, schema_digest),
                entries: Vec::new(),
            },
        );
        install_rows_store(root, &initial, relation_id, &schema_oid, &primary_root, 0);
        (directory, descriptor)
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

    /// Publishes one relation holding a row whose fields are the format-3 Blob
    /// value for `payload`. Returns the descriptor OID the row names, so a
    /// later reader can prove it read that row's own Blob. With `repeat_field`
    /// the row stores that same Blob twice, which no reader may attribute to
    /// one field.
    fn install_annotated_blob_row_store(
        directory: &Path,
        relation_id: [u8; 16],
        key: u64,
        payload: &[u8],
        media_type: &str,
        suffix: Option<&str>,
        repeat_field: bool,
    ) -> NativeOid {
        // A relation must exist before capture resolves the relation map. Seed
        // it only once, so a second call publishes a plain successor commit
        // instead of a commit that drops the earlier row.
        let seeding = context(directory);
        if seeding.load_row_map(relation_id).is_err() {
            install_overflow_row_store(directory, &seeding, relation_id, None);
        }

        let writing = context(directory);
        let (schema_oid, schema_digest) = schema_node_for_relation(directory, &writing, relation_id);
        let snapshot = writing
            .load_row_map(relation_id)
            .expect("load the seeded row-map identity");
        let graph = writing
            .open_native_graph(&snapshot)
            .expect("admit the seeded graph context");
        let scope = graph.open_read_scope().expect("owner scope");

        let mut input = std::io::Cursor::new(payload.to_vec());
        let candidate = graph
            .capture_blob_candidate(&mut input, payload.len() as u64, &scope)
            .expect("write a private OGB-2 candidate closure");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("verify and durably protect the OGB-2 closure");
        let binding = graph
            .accept_protected_blob_pin_with_annotation(pin, media_type, suffix)
            .expect("accept the annotated ORP Blob binding");
        let descriptor_oid = binding.descriptor_oid().clone();

        let algorithm = writing
            .validate_store_root()
            .expect("pinned native store")
            .oid
            .algorithm();
        let fields = if repeat_field { 2u8 } else { 1u8 };
        let mut stored_fields = vec![0x80 | fields];
        for _ in 0..fields {
            stored_fields.extend_from_slice(binding.encoded_value());
        }
        let row_root = write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: row_domain(relation_id, schema_digest),
                entries: vec![OrderedLeafEntry {
                    key: TypedKey::UInt(key).canonical_bytes().unwrap(),
                    value: stored_fields,
                }],
            },
        );
        let database_id = writing.require_database_id().expect("database identity");
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
        descriptor_oid
    }

    /// Encodes one canonical stored value holding no dependency edges.
    fn inline_value() -> Vec<u8> {
        vec![0x81, 0x01]
    }

    /// Writes one ordered rows leaf whose declared entry count is `declared`,
    /// bypassing the writer's own count check so a foreign repository's
    /// miscounted page can reach the object store.
    ///
    /// `entries` is the canonical CBOR body a leaf carries: the array of
    /// `[key, value]` pairs.
    fn raw_rows_leaf(
        directory: &Path,
        algorithm: GitHashAlgorithm,
        domain: &[u8],
        entries: &[u8],
        declared: u64,
    ) -> NativeOid {
        let mut data = Vec::new();
        data.push(0x84);
        data.push(0x01);
        data.push(0x01);
        cbor_bytes(&mut data, domain);
        cbor_head(&mut data, 4, declared);
        data.extend_from_slice(entries);
        let data_oid = write_git_object(directory, algorithm, "blob", &data);
        let mut envelope = Vec::new();
        append_tree_entry(&mut envelope, "100644", "data", &data_oid);
        write_git_object(directory, algorithm, "tree", &envelope)
    }

    /// Installs one relation whose row root is `primary_root` and whose
    /// relation value declares `declared_count` rows, then commits a fresh
    /// store root. A valid leaf passes a `declared_count` equal to its entry
    /// count; the malformed legs use the writer's own checks as the oracle.
    fn install_rows_store(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
        schema_oid: &NativeOid,
        primary_root: &NativeOid,
        declared_count: u64,
    ) {
        let algorithm = context
            .validate_store_root()
            .expect("pinned format-3 store")
            .oid
            .algorithm();
        let database_id = context.require_database_id().expect("database identity");
        let mut relation_value = vec![0x84];
        cbor_bytes(&mut relation_value, schema_oid.as_bytes());
        cbor_bytes(&mut relation_value, primary_root.as_bytes());
        relation_value.push(0xf6);
        cbor_head(&mut relation_value, 0, declared_count);
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

    /// Builds a valid canonical rows leaf from `(key, value)` pairs.
    fn rows_leaf(
        directory: &Path,
        algorithm: GitHashAlgorithm,
        domain: &[u8],
        rows: &[(u64, Vec<u8>)],
    ) -> NativeOid {
        write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedLeaf {
                domain: domain.to_vec(),
                entries: rows
                    .iter()
                    .map(|(key, value)| OrderedLeafEntry {
                        key: TypedKey::UInt(*key).canonical_bytes().unwrap(),
                        value: value.clone(),
                    })
                    .collect(),
            },
        )
    }

    #[test]
    fn orp1_index_maintenance_reports_index_churn_separately_from_payload_reuse() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x6d; 16];

        // Seed a relation so a graph context can capture the Blob the row will
        // store, then publish the real 20-row page that carries it.
        let initial = context(root);
        if initial.load_row_map(relation_id).is_err() {
            install_overflow_row_store(root, &initial, relation_id, None);
        }
        let seeding = context(root);
        let snapshot = seeding
            .load_row_map(relation_id)
            .expect("seeded row-map identity");
        let graph = seeding
            .open_native_graph(&snapshot)
            .expect("admit the seeded graph context");
        let scope = graph.open_read_scope().expect("owner scope");
        let payload = b"london take one".as_slice();
        let mut input = std::io::Cursor::new(payload.to_vec());
        let candidate = graph
            .capture_blob_candidate(&mut input, payload.len() as u64, &scope)
            .expect("capture the OGB-2 candidate closure");
        let pin = graph
            .protect_captured_blob(candidate, &scope)
            .expect("protect the OGB-2 closure");
        let binding = graph
            .accept_protected_blob_pin_with_annotation(pin, "audio/mpeg", None)
            .expect("accept the annotated Blob binding");
        let descriptor = binding.descriptor_oid().clone();

        let (schema_oid, schema_digest) = schema_node_for_relation(root, &seeding, relation_id);
        let algorithm = seeding
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let domain = row_domain(relation_id, schema_digest);
        let mut annotated = vec![0x81];
        annotated.extend_from_slice(binding.encoded_value());
        let mut rows: Vec<(u64, Vec<u8>)> = (1..=20).map(|key| (key, inline_value())).collect();
        rows[4].1 = annotated;
        let leaf = rows_leaf(root, algorithm, &domain, &rows);
        install_rows_store(root, &seeding, relation_id, &schema_oid, &leaf, 20);

        let pinned = context(root);
        let snapshot = pinned
            .load_row_map(relation_id)
            .expect("admit the published 20-row page");
        let graph = pinned.open_native_graph(&snapshot).expect("admit graph");
        let scope = graph.open_read_scope().expect("owner scope");

        // A pass with no mutations rebuilds every page from the same rows, so
        // every index object is byte-identical and nothing is new churn: the
        // pass reports the previous root as its own successor.
        let noop = graph
            .maintain_row_index(&[], &scope)
            .expect("empty maintenance pass");
        assert_eq!(noop.row_count(), 20);
        assert_eq!(noop.primary_root(), noop.previous_root());
        assert!(
            noop.index_objects_created().is_empty(),
            "an unchanged rewrite creates no index object"
        );
        assert!(
            !noop.index_objects_reused().is_empty(),
            "an unchanged rewrite reuses its index nodes"
        );

        // Isolate the production index-node write count for each mutation from
        // the same committed root; the page counter excludes media objects.
        let prepend = graph
            .maintain_row_index(
                &[RowIndexMutation::Insert {
                    key: TypedKey::UInt(0),
                    fields: inline_value(),
                }],
                &scope,
            )
            .expect("measure prepend page writes");
        let delete = graph
            .maintain_row_index(
                &[RowIndexMutation::Delete {
                    key: TypedKey::UInt(2),
                }],
                &scope,
            )
            .expect("measure delete page writes");
        let rekey = graph
            .maintain_row_index(
                &[RowIndexMutation::Rekey {
                    from: TypedKey::UInt(10),
                    to: TypedKey::UInt(50),
                }],
                &scope,
            )
            .expect("measure rekey page writes");
        assert_eq!(
            rekey.logical_rekeys(),
            &[crate::native_graph::LogicalRowRekey {
                old_address: TypedKey::UInt(10),
                new_address: TypedKey::UInt(50),
            }],
        );
        assert!(matches!(
            graph.maintain_row_index(
                &[RowIndexMutation::Rekey {
                    from: TypedKey::UInt(999),
                    to: TypedKey::UInt(50),
                }],
                &scope,
            ),
            Err(crate::native_graph::GraphError::RowKeyAbsent)
        ));
        assert!(matches!(
            graph.maintain_row_index(
                &[RowIndexMutation::Rekey {
                    from: TypedKey::UInt(10),
                    to: TypedKey::UInt(11),
                }],
                &scope,
            ),
            Err(crate::native_graph::GraphError::DuplicateRowKey)
        ));
        let repeated_rekey = graph
            .maintain_row_index(
                &[RowIndexMutation::Rekey {
                    from: TypedKey::UInt(10),
                    to: TypedKey::UInt(50),
                }],
                &scope,
            )
            .expect("repeat rekey against the same pinned source");
        assert_eq!(repeated_rekey.logical_rekeys(), rekey.logical_rekeys());
        assert_eq!(repeated_rekey.logical_rekeys().len(), 1);
        assert_eq!(prepend.row_count(), 21);
        assert_eq!(delete.row_count(), 19);
        assert_eq!(rekey.row_count(), 20);
        for (mutation, pass) in [
            ("prepend", &prepend),
            ("delete", &delete),
            ("rekey", &rekey),
        ] {
            assert_eq!(pass.previous_root(), snapshot.version().primary_root());
            assert_ne!(pass.primary_root(), pass.previous_root());
            assert!(
                !pass.index_objects_created().is_empty(),
                "{mutation} writes at least one ORP-1 index node"
            );
            assert!(
                pass.index_objects_created()
                    .iter()
                    .all(|oid| !pass.index_objects_reused().contains(oid)),
                "{mutation} reports written and reused nodes separately"
            );
            assert!(
                pass.payload_objects_reused().contains(&descriptor),
                "{mutation} preserves the untouched Blob descriptor"
            );
            eprintln!(
                "ORP1_CHURN mutation={mutation} rows={} index_nodes_written={} index_nodes_reused={} payload_objects_reused={}",
                pass.row_count(),
                pass.index_objects_created().len(),
                pass.index_objects_reused().len(),
                pass.payload_objects_reused().len(),
            );
        }

        // A real churn pass: prepend, delete and rekey, with one media row left
        // untouched.
        let row_before_rekey = graph
            .lookup_row(&TypedKey::UInt(10), &scope)
            .expect("read row before rekey")
            .expect("source row exists");
        let rekeyed_value = row_before_rekey.value().clone();
        let mutations = [
            RowIndexMutation::Delete {
                key: TypedKey::UInt(2),
            },
            RowIndexMutation::Rekey {
                from: TypedKey::UInt(10),
                to: TypedKey::UInt(50),
            },
            RowIndexMutation::Insert {
                key: TypedKey::UInt(0),
                fields: inline_value(),
            },
        ];
        let churn = graph
            .maintain_row_index(&mutations, &scope)
            .expect("prepend/delete/rekey maintenance pass");
        assert_eq!(churn.row_count(), 20);
        assert_eq!(churn.previous_root(), snapshot.version().primary_root());
        assert_ne!(churn.primary_root(), churn.previous_root());
        assert!(
            !churn.index_objects_created().is_empty(),
            "an index rewrite reports the tree objects it wrote"
        );
        assert!(
            churn
                .index_objects_created()
                .iter()
                .all(|oid| !churn.index_objects_reused().contains(oid)),
            "created and reused index objects are disjoint"
        );
        assert!(
            churn.payload_objects_reused().contains(&descriptor),
            "the untouched media row keeps its Blob descriptor object"
        );
        assert!(
            !churn.index_objects_created().contains(&descriptor),
            "media is never reported as index churn"
        );

        // The reported successor is the real one: naming it makes the mutated
        // key set readable.
        install_rows_store(
            root,
            &pinned,
            relation_id,
            &schema_oid,
            churn.primary_root(),
            churn.row_count(),
        );
        let successor = context(root);
        let snapshot = successor
            .load_row_map(relation_id)
            .expect("admit the rewritten page");
        let graph = successor.open_native_graph(&snapshot).expect("admit graph");
        let scope = graph.open_read_scope().expect("owner scope");
        let rekey_scope = graph.open_read_scope().expect("rekey read scope");
        let row_after_rekey = graph
            .lookup_row(&TypedKey::UInt(50), &rekey_scope)
            .expect("read row after rekey")
            .expect("destination row exists");
        assert_eq!(row_after_rekey.key(), &TypedKey::UInt(50));
        assert_eq!(row_after_rekey.value(), &rekeyed_value);
        for (key, present) in [(0u64, true), (2, false), (5, true), (10, false), (50, true)] {
            assert_eq!(
                graph
                    .lookup_row(&TypedKey::UInt(key), &scope)
                    .expect("bounded point lookup")
                    .is_some(),
                present,
                "key {key} after the maintenance pass"
            );
        }
    }

    /// store-format-3 section 2: the child ranges of a branch are
    /// nonoverlapping and exhaustive in fence order. A row map whose second
    /// page starts inside the first page's range still opens, because the
    /// loader checks only the root, but a range read across the pages and a
    /// maintenance pass that rewrites the second page both refuse it, so
    /// neither a query nor a publication returns or writes rows from
    /// overlapping pages (ornadb-5il1.129 bead comment, A07-storage-rows-04).
    #[test]
    fn range_reads_and_maintenance_reject_overlapping_child_ranges() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x6e; 16];
        let initial = context(root);
        if initial.load_row_map(relation_id).is_err() {
            install_overflow_row_store(root, &initial, relation_id, None);
        }
        let seeding = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &seeding, relation_id);
        let algorithm = seeding
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let domain = row_domain(relation_id, schema_digest);
        let page = |keys: std::ops::RangeInclusive<u64>| {
            rows_leaf(
                root,
                algorithm,
                &domain,
                &keys.map(|key| (key, inline_value())).collect::<Vec<_>>(),
            )
        };
        let fence = |key: u64| TypedKey::UInt(key).canonical_bytes().expect("fence key");
        // The right page starts at key 15, inside the left page's 1..=20.
        let (left, right) = (page(1..=20), page(15..=34));
        let branch = write_native_node(
            root,
            algorithm,
            &NodeData::OrderedBranch {
                domain: domain.clone(),
                height: 1,
                entries: vec![
                    OrderedBranchEntry {
                        inclusive_max_key: fence(20),
                        child: left,
                        row_count: 20,
                    },
                    OrderedBranchEntry {
                        inclusive_max_key: fence(34),
                        child: right,
                        row_count: 20,
                    },
                ],
            },
        );
        install_rows_store(root, &seeding, relation_id, &schema_oid, &branch, 40);

        let pinned = context(root);
        let snapshot = pinned
            .load_row_map(relation_id)
            .expect("the loader checks the root");
        let graph = pinned.open_native_graph(&snapshot).expect("admit graph");
        let scope = graph.open_read_scope().expect("owner scope");
        assert!(
            graph
                .range_rows(
                    &crate::row_store::KeyRange::new(None, None, 100).expect("whole range"),
                    &scope,
                )
                .is_err(),
            "a range read refuses overlapping pages"
        );
        assert!(
            graph
                .maintain_row_index(
                    &[RowIndexMutation::Delete {
                        key: TypedKey::UInt(30),
                    }],
                    &scope,
                )
                .is_err(),
            "maintenance refuses to rewrite an overlapping page"
        );
    }

    #[test]
    fn orp1_index_maintenance_rejects_malformed_pages_on_read() {
        let directory = repository();
        let root = directory.path();

        // A page over the 256-entry fanout bound is refused on read.
        let fanout_relation = [0xb1; 16];
        let seeded = context(root);
        install_overflow_row_store(root, &seeded, fanout_relation, None);
        let ready = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &ready, fanout_relation);
        let algorithm = ready
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let mut entries = Vec::new();
        cbor_head(&mut entries, 4, 257);
        for key in 1..=257u64 {
            entries.push(0x82);
            entries.extend_from_slice(&TypedKey::UInt(key).canonical_bytes().unwrap());
            entries.extend_from_slice(&inline_value());
        }
        let leaf = raw_rows_leaf(
            root,
            algorithm,
            &row_domain(fanout_relation, schema_digest),
            &entries,
            257,
        );
        install_rows_store(root, &ready, fanout_relation, &schema_oid, &leaf, 257);
        assert!(
            context(root).load_row_map(fanout_relation).is_err(),
            "a 257-entry page is rejected on read"
        );

        // The same single row is admitted under its own domain and refused
        // under a domain carrying another schema digest, so the domain is what
        // decides.
        let domain_relation = [0xc2; 16];
        let seeded = context(root);
        install_overflow_row_store(root, &seeded, domain_relation, None);
        let ready = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &ready, domain_relation);
        let algorithm = ready
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let mut other_digest = schema_digest;
        other_digest[0] ^= 0xff;
        let leaf = rows_leaf(
            root,
            algorithm,
            &row_domain(domain_relation, other_digest),
            &[(1, inline_value())],
        );
        install_rows_store(root, &ready, domain_relation, &schema_oid, &leaf, 1);
        assert!(
            context(root).load_row_map(domain_relation).is_err(),
            "a page from another schema domain is rejected on read"
        );
        let leaf = rows_leaf(
            root,
            algorithm,
            &row_domain(domain_relation, schema_digest),
            &[(1, inline_value())],
        );
        install_rows_store(root, &ready, domain_relation, &schema_oid, &leaf, 1);
        assert!(
            context(root).load_row_map(domain_relation).is_ok(),
            "the same row under its own domain is admitted"
        );

        // A relation whose declared row count disagrees with its page is
        // refused on read, so a caller cannot publish a miscounted relation.
        let count_relation = [0xd3; 16];
        let seeded = context(root);
        install_overflow_row_store(root, &seeded, count_relation, None);
        let ready = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &ready, count_relation);
        let algorithm = ready
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let rows: Vec<(u64, Vec<u8>)> = (1..=200).map(|key| (key, inline_value())).collect();
        let leaf = rows_leaf(
            root,
            algorithm,
            &row_domain(count_relation, schema_digest),
            &rows,
        );
        install_rows_store(root, &ready, count_relation, &schema_oid, &leaf, 199);
        assert!(
            context(root).load_row_map(count_relation).is_err(),
            "a page whose declared count disagrees with its entries is rejected on read"
        );
    }

    #[test]
    fn checkout_of_a_prior_commit_reads_its_rows_and_annotated_blob() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x51; 16];

        let first_payload = b"london take one".as_slice();
        let first_descriptor = install_annotated_blob_row_store(
            root,
            relation_id,
            1,
            first_payload,
            "audio/mpeg",
            Some("mp2"),
            false,
        );
        let second_payload = b"london take two, remastered".as_slice();
        let second_descriptor = install_annotated_blob_row_store(
            root,
            relation_id,
            1,
            second_payload,
            "audio/mpeg",
            Some("mp2"),
            false,
        );
        assert_ne!(first_descriptor, second_descriptor);

        // Mount the prior commit. The selector resolves once; every row, Blob
        // and annotation below comes from that commit's own objects.
        let repository = Repository::discover(root).expect("discover the fixture repository");
        let mounted = repository
            .open_format_context_at_selector("HEAD~1")
            .expect("mount the prior commit read-only");
        assert_eq!(mounted.repository_format_number(), 3);
        assert!(!mounted.is_legacy_format());
        assert_eq!(
            mounted.database_id(),
            repository
                .open_format_context()
                .expect("admit the workspace context")
                .database_id()
        );

        let snapshot = mounted
            .load_row_map(relation_id)
            .expect("historical row map");
        let graph = mounted
            .open_native_graph(&snapshot)
            .expect("historical graph");
        let scope = graph.open_read_scope().expect("historical read scope");
        let row = graph
            .lookup_row(&TypedKey::UInt(1), &scope)
            .expect("read the historical row")
            .expect("the prior commit holds the row");

        let stored = graph
            .read_stored_blob_value(&row, &first_descriptor, &scope)
            .expect("read the Blob the historical row stores");
        assert_eq!(stored.annotation().media_type(), "audio/mpeg");
        assert_eq!(stored.annotation().suffix(), Some("mp2"));
        assert_eq!(
            stored.content_identity(),
            crate::blob_store::digest_bytes(first_payload)
        );
        assert_eq!(scope.payload_bytes_read_for_test(), 0);
        let range = graph
            .read_blob_range(stored.reference(), 0..first_payload.len() as u64, &scope)
            .expect("read the historical payload");
        assert_eq!(range.bytes(), first_payload);
        assert_eq!(
            scope.payload_bytes_read_for_test(),
            first_payload.len() as u64
        );
        assert_eq!(
            range.verification(),
            crate::native_graph::RangeVerification::FullBlob
        );

        // The descriptor of the newer commit is not the one this row stored,
        // so the historical pin never serves the newer content.
        assert!(matches!(
            graph.read_stored_blob_value(&row, &second_descriptor, &scope),
            Err(crate::native_graph::GraphError::DescriptorNotInRow)
        ));

        // The workspace view still reads its own revision, and the two mounts
        // carry distinct pinned snapshot identities.
        let head = repository
            .open_format_context()
            .expect("admit the workspace context");
        let head_snapshot = head.load_row_map(relation_id).expect("workspace row map");
        let head_graph = head
            .open_native_graph(&head_snapshot)
            .expect("workspace graph");
        let head_scope = head_graph.open_read_scope().expect("workspace read scope");
        let head_row = head_graph
            .lookup_row(&TypedKey::UInt(1), &head_scope)
            .expect("read the workspace row")
            .expect("the workspace holds the row");
        let head_stored = head_graph
            .read_stored_blob_value(&head_row, &second_descriptor, &head_scope)
            .expect("read the workspace Blob");
        let head_range = head_graph
            .read_blob_range(
                head_stored.reference(),
                0..second_payload.len() as u64,
                &head_scope,
            )
            .expect("read the workspace payload");
        assert_eq!(head_range.bytes(), second_payload);

        let remounted = repository
            .open_format_context_at_selector("HEAD~1")
            .expect("remount the prior commit");
        assert_eq!(
            mounted.snapshot_id(),
            remounted.snapshot_id(),
            "one commit always resolves to one snapshot identity"
        );
        assert_ne!(
            *mounted.snapshot_id(),
            head.snapshot_pin().snapshot_id(),
            "a historical pin never shares the workspace snapshot identity"
        );
    }

    #[test]
    fn a_row_storing_one_blob_twice_is_refused_rather_than_attributed() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x52; 16];
        let payload = b"london take three".as_slice();
        install_annotated_blob_row_store(
            root,
            relation_id,
            1,
            payload,
            "audio/mpeg",
            Some("mp2"),
            true,
        );

        let repository = Repository::discover(root).expect("discover the fixture repository");
        let context = repository
            .open_format_context()
            .expect("admit the workspace context");
        let snapshot = context.load_row_map(relation_id).expect("row map");
        let graph = context.open_native_graph(&snapshot).expect("graph");
        let scope = graph.open_read_scope().expect("read scope");
        let row = graph
            .lookup_row(&TypedKey::UInt(1), &scope)
            .expect("read the row")
            .expect("the row exists");
        let descriptor = row
            .value()
            .dependencies()
            .first()
            .expect("the row names its Blob")
            .oid()
            .clone();
        assert!(matches!(
            graph.read_stored_blob_value(&row, &descriptor, &scope),
            Err(crate::native_graph::GraphError::UnsupportedRowValueForm)
        ));
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
        let parent_entries = git_output(directory, &["ls-tree", "HEAD"], None);
        let parent_entries = std::str::from_utf8(&parent_entries).expect("root tree entries UTF-8");
        let mut root_tree_spec = format!("040000 tree {}\t.orna\n", orna_tree_oid.to_hex());
        for entry in parent_entries
            .lines()
            .filter(|entry| !entry.ends_with("\t.orna"))
        {
            root_tree_spec.push_str(entry);
            root_tree_spec.push('\n');
        }
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

    /// Writes `schema_bytes` as a kind-6 node over a one-chunk byte index.
    /// The chunk entry carries the chunk's own SHA-256; the node records
    /// `schema_digest`, which a valid node sets to the bytes' schema identity.
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
                    chunk_sha256: Some(Sha256::digest(schema_bytes).into()),
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

    fn graph_bridge_test_relation_id(context: &RepositoryFormatContext) -> [u8; 16] {
        let database_id = context.require_database_id().expect("database identity");
        let mut relation_hash = Sha256::new();
        relation_hash.update(b"orna.test.graph-bridge.relation.v1\0");
        relation_hash.update(database_id.as_bytes());
        let relation_digest = relation_hash.finalize();
        let mut relation_id = [0u8; 16];
        relation_id.copy_from_slice(&relation_digest[..16]);
        relation_id
    }
    fn schema_node(
        directory: &Path,
        context: &RepositoryFormatContext,
    ) -> (NativeOid, [u8; 32]) {
        schema_node_for_relation(directory, context, graph_bridge_test_relation_id(context))
    }

    fn schema_node_for_relation(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
    ) -> (NativeOid, [u8; 32]) {
        let algorithm = context
            .validate_schema_root()
            .expect("pinned source schema root")
            .oid
            .algorithm();
        let descriptor = descriptor_for_table(relation_id, "Int");
        let schema_bytes = descriptor.encode().expect("encode schema descriptor");
        let schema_digest = crate::native_graph::schema_identity_v3(&schema_bytes);
        let schema_oid = schema_descriptor_node(
            directory,
            algorithm,
            &schema_bytes,
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
        let text = vec![b'x'; crate::row_store::ROW_INLINE_LIMIT + 1];
        let mut encoded = vec![0x81];
        cbor_head(&mut encoded, 3, text.len() as u64);
        encoded.extend_from_slice(&text);
        install_overflow_row_store_with_bytes(
            directory,
            context,
            relation_id,
            &encoded,
            semantic_digest_override,
        )
    }

    fn install_overflow_row_store_with_bytes(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
        overflow_bytes: &[u8],
        semantic_digest_override: Option<[u8; 32]>,
    ) -> ([u8; 16], [u8; 32]) {
        let algorithm = context
            .validate_store_root()
            .expect("pinned format-3 store")
            .oid
            .algorithm();
        let database_id = context.require_database_id().expect("database identity");
        let (schema_oid, schema_digest) = schema_node_for_relation(directory, context, relation_id);
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
        let (schema_oid, schema_digest) = schema_node_for_relation(directory, context, relation_id);

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

    /// Builds a real two-level ORP-1 index for one relation: `branches` middle
    /// level nodes, each naming `pages` leaf pages of `page_width` rows. Every
    /// fence, count and domain comes from the node it names, so the graph is
    /// canonical and only the search behaviour decides how many Git objects a
    /// point lookup or a bounded range reads.
    fn install_nested_row_store(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
        branches: u32,
        pages: u32,
        page_width: u64,
    ) -> u64 {
        let algorithm = context
            .validate_store_root()
            .expect("pinned format-3 store")
            .oid
            .algorithm();
        let database_id = context.require_database_id().expect("database identity");
        let (schema_oid, schema_digest) = schema_node_for_relation(directory, context, relation_id);
        let domain = row_domain(relation_id, schema_digest);

        let mut next_key = 0u64;
        let mut root_entries = Vec::new();
        let mut total_count = 0u64;
        for _ in 0..branches {
            let mut branch_entries = Vec::new();
            let mut branch_count = 0u64;
            for _ in 0..pages {
                let mut entries = Vec::new();
                for _ in 0..page_width {
                    next_key += 10;
                    entries.push(OrderedLeafEntry {
                        key: TypedKey::UInt(next_key).canonical_bytes().unwrap(),
                        value: vec![0x81, 0x01],
                    });
                }
                let leaf = write_native_node(
                    directory,
                    algorithm,
                    &NodeData::OrderedLeaf {
                        domain: domain.clone(),
                        entries,
                    },
                );
                branch_entries.push(OrderedBranchEntry {
                    inclusive_max_key: TypedKey::UInt(next_key).canonical_bytes().unwrap(),
                    child: leaf,
                    row_count: page_width,
                });
                branch_count += page_width;
            }
            let branch = write_native_node(
                directory,
                algorithm,
                &NodeData::OrderedBranch {
                    domain: domain.clone(),
                    height: 1,
                    entries: branch_entries,
                },
            );
            root_entries.push(OrderedBranchEntry {
                inclusive_max_key: TypedKey::UInt(next_key).canonical_bytes().unwrap(),
                child: branch,
                row_count: branch_count,
            });
            total_count += branch_count;
        }
        let primary_root = write_native_node(
            directory,
            algorithm,
            &NodeData::OrderedBranch {
                domain: domain.clone(),
                height: 2,
                entries: root_entries,
            },
        );

        let mut relation_value = vec![0x84];
        cbor_bytes(&mut relation_value, schema_oid.as_bytes());
        cbor_bytes(&mut relation_value, primary_root.as_bytes());
        relation_value.push(0xf6);
        cbor_head(&mut relation_value, 0, total_count);
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
        total_count
    }

    /// One node of a hand-built ORP-1 index before it is written: a leaf page
    /// of `UInt` keys, or a branch whose entries carry the fence and row count
    /// the parent claims for each child. A test builds the canonical shape,
    /// tampers with one claim or page, and installs the result, so a lookup
    /// meets one malformed node on its path.
    enum OrpFixture {
        Leaf(Vec<u64>),
        Branch(Vec<OrpFixtureChild>),
    }

    /// A branch entry as its parent claims it.
    struct OrpFixtureChild {
        fence: u64,
        row_count: u64,
        node: OrpFixture,
    }

    impl OrpFixture {
        /// The canonical index of `height` branch levels over leaf pages of
        /// `page_width` keys, `fanout` children per branch, and keys 10, 20,
        /// 30, ... in order. Every fence and count comes from the node it
        /// names, exactly as [`install_nested_row_store`] builds them.
        fn canonical(height: u8, fanout: usize, page_width: u64) -> Self {
            fn build(height: u8, fanout: usize, page_width: u64, next_key: &mut u64) -> OrpFixture {
                if height == 0 {
                    return OrpFixture::Leaf(
                        (0..page_width)
                            .map(|_| {
                                *next_key += 10;
                                *next_key
                            })
                            .collect(),
                    );
                }
                OrpFixture::Branch(
                    (0..fanout)
                        .map(|_| {
                            let node = build(height - 1, fanout, page_width, next_key);
                            OrpFixtureChild {
                                fence: *next_key,
                                row_count: node.row_count(),
                                node,
                            }
                        })
                        .collect(),
                )
            }
            build(height, fanout, page_width, &mut 0)
        }

        /// The row count this node's own entries add up to.
        fn row_count(&self) -> u64 {
            match self {
                Self::Leaf(keys) => keys.len() as u64,
                Self::Branch(children) => children.iter().map(|child| child.row_count).sum(),
            }
        }

        /// Re-derives every claimed row count from the nodes below it, so a
        /// tamper that resizes one page leaves only that page malformed.
        fn recount(&mut self) -> u64 {
            if let Self::Branch(children) = self {
                for child in children.iter_mut() {
                    child.row_count = child.node.recount();
                }
            }
            self.row_count()
        }

        fn child(&mut self, index: usize) -> &mut OrpFixtureChild {
            match self {
                Self::Branch(children) => &mut children[index],
                Self::Leaf(_) => panic!("an ORP fixture leaf has no children"),
            }
        }

        /// Writes this node and everything below it, a branch at `height`.
        fn write(
            &self,
            directory: &Path,
            algorithm: GitHashAlgorithm,
            domain: &[u8],
            height: u8,
        ) -> NativeOid {
            let node = match self {
                Self::Leaf(keys) => NodeData::OrderedLeaf {
                    domain: domain.to_vec(),
                    entries: keys
                        .iter()
                        .map(|key| OrderedLeafEntry {
                            key: TypedKey::UInt(*key).canonical_bytes().unwrap(),
                            value: vec![0x81, 0x01],
                        })
                        .collect(),
                },
                Self::Branch(children) => NodeData::OrderedBranch {
                    domain: domain.to_vec(),
                    height,
                    entries: children
                        .iter()
                        .map(|child| OrderedBranchEntry {
                            inclusive_max_key: TypedKey::UInt(child.fence)
                                .canonical_bytes()
                                .unwrap(),
                            child: child.node.write(directory, algorithm, domain, height - 1),
                            row_count: child.row_count,
                        })
                        .collect(),
                },
            };
            write_native_node(directory, algorithm, &node)
        }
    }

    /// Installs `index` as one relation's primary ORP-1 root of `height`. The
    /// relation records the row count the root's own entries claim, so root
    /// admission accepts it and only a lookup's path meets any tamper below.
    fn install_orp_fixture_row_store(
        directory: &Path,
        context: &RepositoryFormatContext,
        relation_id: [u8; 16],
        index: &OrpFixture,
        height: u8,
    ) {
        let algorithm = context
            .validate_store_root()
            .expect("pinned format-3 store")
            .oid
            .algorithm();
        let database_id = context.require_database_id().expect("database identity");
        let (schema_oid, schema_digest) = schema_node_for_relation(directory, context, relation_id);
        let domain = row_domain(relation_id, schema_digest);
        let primary_root = index.write(directory, algorithm, &domain, height);

        let mut relation_value = vec![0x84];
        cbor_bytes(&mut relation_value, schema_oid.as_bytes());
        cbor_bytes(&mut relation_value, primary_root.as_bytes());
        relation_value.push(0xf6);
        cbor_head(&mut relation_value, 0, index.row_count());
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

        let relation_id = graph_bridge_test_relation_id(context);

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

        let snapshot_id = context.snapshot.snapshot_id();
        let row_count = version.row_count().expect("sealed fixture row count");
        let mut authority = Sha256::new();
        authority.update(b"orna.repository.orp.snapshot-authority.v1\0");
        authority.update(database_id.as_bytes());
        authority.update(snapshot_id);
        authority.update(store.oid.as_bytes());
        authority.update(relation_id);
        authority.update(version.primary_root().as_bytes());
        authority.update(schema_digest);
        authority.update(row_count.to_be_bytes());
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
    fn publication_store_subtree_pin_admits_only_a_real_store_root() {
        let directory = repository();
        let root = directory.path();
        let context = context(root);
        let (schema_oid, schema_digest) = schema_node(root, &context);
        let rows = sealed_rows(&context, &schema_oid, schema_digest);
        let graph = context
            .open_native_graph(&rows)
            .expect("admit the fixture graph");

        // The production writer builds a complete OGS-1 envelope: the data
        // blob, the refs tree naming the relation map, and the envelope tree.
        let database_id = *context
            .require_database_id()
            .expect("admitted format-3 identity")
            .as_bytes();
        let mut written = std::collections::BTreeSet::new();
        let relation_map = graph
            .write_native_node(
                &NodeData::OrderedLeaf {
                    domain: relations_domain(database_id),
                    entries: Vec::new(),
                },
                &mut written,
            )
            .expect("write the relation map node");
        let store_root = graph
            .write_native_node(&NodeData::StoreRoot { relation_map }, &mut written)
            .expect("write the store root node");
        assert!(
            written.contains(&store_root),
            "the envelope object is part of the durability closure"
        );

        // The envelope the writer produced is what a reader decodes, so the
        // store position can be pinned from it.
        let index = root.join(".git/private-store-index");
        let initialized_index = Command::new("git")
            .current_dir(root)
            .env("GIT_INDEX_FILE", &index)
            .args(["read-tree", "--empty"])
            .output()
            .expect("initialize an empty private index");
        assert!(
            initialized_index.status.success(),
            "initialize private index: {}",
            String::from_utf8_lossy(&initialized_index.stderr)
        );
        graph
            .pin_store_subtree_in_index(&index, &store_root)
            .expect("a real store root is pinned at the fixed store path");
        let staged_output = Command::new("git")
            .current_dir(root)
            .env("GIT_INDEX_FILE", &index)
            .args(["ls-files", "--stage", "--", ".orna/store"])
            .output()
            .expect("list the private index");
        assert!(staged_output.status.success(), "list private index subtree");
        let staged = String::from_utf8_lossy(&staged_output.stdout).into_owned();
        let expected_tree = store_root.to_hex();
        let expected = String::from_utf8(git_output(
            root,
            &["ls-tree", "-r", "--full-tree", &expected_tree],
            None,
        ))
        .expect("store tree entries are UTF-8")
        .lines()
        .map(|entry| {
            let (metadata, path) = entry.split_once('\t').expect("tree entry path");
            let mut fields = metadata.split_whitespace();
            let mode = fields.next().expect("tree entry mode");
            let _kind = fields.next().expect("tree entry kind");
            let oid = fields.next().expect("tree entry object id");
            format!("{mode} {oid} 0\t.orna/store/{path}")
        })
        .collect::<Vec<_>>()
        .join("\n");
        assert_eq!(
            staged.trim(),
            expected,
            "the private index expands the encoded store tree exactly at .orna/store"
        );

        // A raw object cannot be planted at the store position.
        let blob = write_git_object(root, graph.algorithm(), "blob", b"not a store root");
        assert!(matches!(
            graph.pin_store_subtree_in_index(&index, &blob),
            Err(crate::native_graph::GraphError::WrongObjectKind {
                expected: crate::native_graph::NativeObjectKind::Tree,
                actual: crate::native_graph::NativeObjectKind::Blob,
            })
        ));
    }

    #[test]
    fn frozen_publication_candidate_carries_only_the_store_root() {
        let directory = repository();
        let root = directory.path();
        // Human state that a publication commit must never contain: an
        // unstaged edit of a managed path and an untracked path.
        fs::write(root.join("main.orna"), b"human unstaged edit").expect("edit a managed path");
        fs::write(root.join("human.txt"), b"untracked human file").expect("add a human path");

        let context = context(root);
        let (schema_oid, schema_digest) = schema_node(root, &context);
        let rows = sealed_rows(&context, &schema_oid, schema_digest);
        let graph = context
            .open_native_graph(&rows)
            .expect("admit the fixture graph");
        let database_id = *context
            .require_database_id()
            .expect("admitted format-3 identity")
            .as_bytes();

        // The frozen batch's encoded store root, built through the writer a
        // publication path uses.
        let mut written = std::collections::BTreeSet::new();
        let relation_map = graph
            .write_native_node(
                &NodeData::OrderedLeaf {
                    domain: relations_domain(database_id),
                    entries: Vec::new(),
                },
                &mut written,
            )
            .expect("write the relation map node");
        let store_root = graph
            .write_native_node(&NodeData::StoreRoot { relation_map }, &mut written)
            .expect("write the store root node");

        let repository = Repository::discover(root).expect("discover fixture repository");
        let head = repository
            .head()
            .expect("read fixture HEAD")
            .expect("fixture HEAD is born");
        let candidate = repository
            .build_frozen_publication_candidate(
                &head,
                &graph,
                &store_root,
                "orna: publish runtime data",
            )
            .expect("build the frozen publication candidate");

        // N = H + P: the candidate's parent is the captured HEAD.
        let parents = Command::new("git")
            .current_dir(root)
            .args([
                "rev-list",
                "--parents",
                "--no-walk",
                candidate.commit().as_str(),
            ])
            .output()
            .expect("read candidate parents");
        let parents = String::from_utf8_lossy(&parents.stdout).into_owned();
        assert_eq!(
            parents.trim(),
            format!("{} {}", candidate.commit().as_str(), head.as_str()),
            "the candidate is a direct child of the captured HEAD"
        );

        // The frozen store root is staged at the fixed format-3 path as a tree.
        let store = Command::new("git")
            .current_dir(root)
            .args(["ls-tree", candidate.commit().as_str(), "--", ".orna/store"])
            .output()
            .expect("read the candidate store entry");
        let store = String::from_utf8_lossy(&store.stdout).into_owned();
        assert_eq!(
            store.trim(),
            format!("040000 tree {}\t.orna/store", store_root.to_hex()),
            "the candidate exposes the encoded store root at .orna/store"
        );

        // Neither the human unstaged edit nor the untracked path is an input.
        let human = Command::new("git")
            .current_dir(root)
            .args(["ls-tree", candidate.commit().as_str(), "--name-only"])
            .output()
            .expect("list the candidate tree");
        let listed = String::from_utf8_lossy(&human.stdout).into_owned();
        assert!(
            !listed.contains("human.txt"),
            "an untracked human path is not committed"
        );
        let managed = Command::new("git")
            .current_dir(root)
            .args([
                "rev-parse",
                &format!("{}:main.orna", candidate.commit().as_str()),
            ])
            .output()
            .expect("read the candidate managed blob");
        let unchanged = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", &format!("{}:main.orna", head.as_str())])
            .output()
            .expect("read the base managed blob");
        assert_eq!(
            managed.stdout, unchanged.stdout,
            "the candidate carries the captured managed base, not the human edit"
        );

        // The candidate never touches the ordinary index, HEAD, or worktree.
        assert_eq!(
            repository.head().expect("read HEAD").expect("born HEAD"),
            head,
            "building a candidate does not move the branch"
        );
        assert_eq!(
            fs::read(root.join("main.orna")).expect("read the human edit"),
            b"human unstaged edit",
            "building a candidate does not rewrite the worktree"
        );
    }

    #[test]
    fn unresolvable_object_id_exits_3_from_the_resolver() {
        let directory = repository();
        let root = directory.path();
        let context = context(root);
        let (schema, digest) = schema_node(root, &context);
        let rows = sealed_rows(&context, &schema, digest);
        let graph = context
            .open_native_graph(&rows)
            .expect("admit the fixture graph");
        let scope = graph.open_read_scope().expect("read scope");
        let missing = NativeOid::from_hex(GitHashAlgorithm::Sha1, &"0".repeat(40))
            .expect("well-formed all-zero SHA-1 object id");
        let error = graph
            .object_stats_since(&scope, &missing)
            .expect_err("an object git does not know must not resolve");
        assert_eq!(error.exit_code(), 3, "unresolved object exits 3: {error}");
    }

    #[test]
    fn object_stats_since_traverses_the_second_fixture_root_against_the_first() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x52; 16];
        let initial_context = context(root);
        install_overflow_row_store(root, &initial_context, relation_id, None);

        let first_context = context(root);
        let first_rows = first_context
            .load_row_map(relation_id)
            .expect("load the first committed row-map fixture");
        let first = first_context
            .open_native_graph(&first_rows)
            .expect("admit the first fixture root");
        let first_scope = first.open_read_scope().expect("first read scope");
        let first_stats = first
            .object_stats(&first_scope)
            .expect("walk the first fixture root");

        let second_text = vec![b'y'; crate::row_store::ROW_INLINE_LIMIT + 1];
        let mut second_encoded = vec![0x81];
        cbor_head(&mut second_encoded, 3, second_text.len() as u64);
        second_encoded.extend_from_slice(&second_text);
        install_overflow_row_store_with_bytes(
            root,
            &first_context,
            relation_id,
            &second_encoded,
            None,
        );
        let second_context = context(root);
        let second_rows = second_context
            .load_row_map(relation_id)
            .expect("load the second committed row-map fixture");
        let second = second_context
            .open_native_graph(&second_rows)
            .expect("admit the second fixture root");
        let second_scope = second.open_read_scope().expect("second read scope");
        let second_stats = second
            .object_stats(&second_scope)
            .expect("walk the second fixture root");

        let added = second
            .object_stats_since(&second_scope, first.store_root())
            .expect("traverse the second root and prune what the first root reaches");
        assert!(first_stats.total_nodes() >= 1);
        assert!(
            added.total_nodes() >= 1,
            "the new store root adds at least one node"
        );
        assert!(
            added.total_nodes() < second_stats.total_nodes(),
            "shared subtrees are pruned, so the added count is smaller than the whole walk"
        );
        assert!(added.blob_references() <= second_stats.blob_references());
    }

    #[test]
    fn object_stats_counts_each_reachable_tree_once_against_git_enumeration() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x53; 16];
        let initial_context = context(root);
        install_overflow_row_store(root, &initial_context, relation_id, None);

        let context = context(root);
        let rows = context
            .load_row_map(relation_id)
            .expect("load the committed row-map fixture");
        let graph = context
            .open_native_graph(&rows)
            .expect("admit the fixture graph");
        let scope = graph.open_read_scope().expect("read scope");
        let stats = graph
            .object_stats(&scope)
            .expect("walk the fixture graph once");
        let again = graph
            .object_stats(&graph.open_read_scope().unwrap())
            .expect("repeat the walk");
        assert_eq!(again, stats, "a repeated walk visits the same nodes");

        // OGS-1 node envelopes are the reachable trees with a top-level data
        // blob. Their `refs` trees are Git trees too, but are not graph nodes.
        let store_root = graph.store_root().to_hex();
        let listing = Command::new("git")
            .current_dir(root)
            .args(["rev-list", "--objects", &store_root])
            .output()
            .expect("list objects reachable from the store root");
        assert!(listing.status.success(), "rev-list failed");
        let oids: Vec<String> = String::from_utf8(listing.stdout)
            .expect("rev-list output is UTF-8")
            .lines()
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect();
        let mut check = Command::new("git")
            .current_dir(root)
            .args(["cat-file", "--batch-check=%(objecttype)"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cat-file --batch-check");
        check
            .stdin
            .take()
            .expect("cat-file stdin")
            .write_all(format!("{}\n", oids.join("\n")).as_bytes())
            .expect("write object ids");
        let types = check.wait_with_output().expect("read object types");
        let types = String::from_utf8(types.stdout).expect("types are UTF-8");
        let mut node_trees = 0u64;
        for (oid, kind) in oids.iter().zip(types.lines()) {
            if kind != "tree" {
                continue;
            }
            let entries = String::from_utf8(git_output(root, &["ls-tree", oid], None))
                .expect("tree entries are UTF-8");
            let is_node_envelope = entries.lines().any(|entry| {
                let Some((metadata, name)) = entry.split_once('\t') else {
                    return false;
                };
                name == "data" && metadata.split_whitespace().nth(1) == Some("blob")
            });
            node_trees += u64::from(is_node_envelope);
        }
        let walked: u64 = stats.nodes().values().sum();
        assert_eq!(
            walked, node_trees,
            "the node walk visits each reachable OGS-1 envelope exactly once"
        );
    }

    #[test]
    fn cross_format_object_id_is_rejected_before_any_lookup() {
        let directory = repository();
        let root = directory.path();
        let context = context(root);
        let (schema, digest) = schema_node(root, &context);
        let rows = sealed_rows(&context, &schema, digest);
        let graph = context
            .open_native_graph(&rows)
            .expect("admit the fixture graph");
        let scope = graph.open_read_scope().expect("read scope");
        // An all-zero SHA-256 id belongs to a different object format than
        // the SHA-1 repository, so it must fail on width, not on lookup.
        let foreign = NativeOid::from_hex(GitHashAlgorithm::Sha256, &"0".repeat(64))
            .expect("well-formed all-zero SHA-256 object id");
        let error = graph
            .object_stats_since(&scope, &foreign)
            .expect_err("a SHA-256 id must not resolve in a SHA-1 graph");
        assert!(
            matches!(
                error,
                crate::native_graph::GraphError::InvalidOidWidth { .. }
            ),
            "cross-format id is an invalid width, not an unresolved object: {error:?}"
        );
        assert_eq!(error.exit_code(), 1, "cross-format id exits 1, not 3");
    }

    #[test]
    fn unresolved_listing_keeps_only_the_ids_git_cannot_resolve() {
        let directory = repository();
        let root = directory.path();
        let context = context(root);
        let (schema, digest) = schema_node(root, &context);
        let rows = sealed_rows(&context, &schema, digest);
        let graph = context
            .open_native_graph(&rows)
            .expect("admit the fixture graph");
        let scope = graph.open_read_scope().expect("read scope");
        let missing = NativeOid::from_hex(GitHashAlgorithm::Sha1, &"0".repeat(40))
            .expect("well-formed all-zero SHA-1 object id");
        let listed = graph
            .unresolved_object_ids(&scope, &[graph.store_root().clone(), missing.clone()])
            .expect("list unresolved ids");
        assert_eq!(
            listed,
            vec![missing],
            "the resolvable store root is left out"
        );
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
        let valid_digest = crate::native_graph::schema_identity_v3(valid_bytes);

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
        let relation_id = [0x72; 16];
        let (directory, _descriptor) = seeded_table_binding(relation_id);
        let root = directory.path();
        let initial = context(root);
        let (_, expected_digest) = install_overflow_row_store(root, &initial, relation_id, None);
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
        let media_before_point = scope.payload_bytes_read();
        let structured_before_point = scope.structured_metadata_bytes_read();
        let row = graph
            .lookup_row(&TypedKey::UInt(7), &scope)
            .expect("bounded point lookup")
            .expect("stored row");
        let RowValue::Overflow(reference) = row.value() else {
            panic!("verified kind-3 row should decode as overflow");
        };
        let encoded_length = reference.encoded_length();
        assert!(encoded_length > crate::row_store::ROW_INLINE_LIMIT as u64);
        assert_eq!(reference.semantic_digest(), &expected_digest);
        assert_eq!(
            scope.structured_metadata_bytes_read() - structured_before_point,
            encoded_length
        );
        assert_eq!(scope.payload_bytes_read() - media_before_point, 0);

        let structured_before_range = scope.structured_metadata_bytes_read();
        let range = KeyRange::new(Some(TypedKey::UInt(7)), None, 1).unwrap();
        let ranged = snapshot
            .range_with_graph_context(&graph, &scope, &range)
            .expect("bounded ORP range lookup");
        assert_eq!(ranged, vec![row]);
        assert_eq!(
            scope.structured_metadata_bytes_read() - structured_before_range,
            encoded_length
        );
        assert_eq!(scope.payload_bytes_read() - media_before_point, 0);
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
    fn admitted_fields_decode_inline_and_overflow_from_bound_manifest_without_media_reads() {
        let relation_id = [0x73; 16];
        let directory = repository();
        let root = directory.path();
        fs::write(root.join("main.orna"), "use media;\n").expect("write importing module");
        fs::write(
            root.join("media.orna"),
            "pub table Audio(id: Int) {\n    payload: Blob,\n    note: Str,\n}\n",
        )
        .expect("write committed table manifest");
        commit(root);

        let initial = context(root);
        let algorithm = initial
            .validate_store_root()
            .expect("pinned store root")
            .oid
            .algorithm();
        let uuid = |id: [u8; 16]| Raw::Tag(37, Box::new(Raw::Bytes(id.to_vec())));
        let field = |id: [u8; 16], name: &str, ty: Raw, role: u8| {
            Raw::Array(vec![
                uuid(id),
                Raw::Text(name.to_owned()),
                ty,
                Raw::Int(role.into()),
                Raw::Array(vec![Raw::Int(0.into())]),
            ])
        };
        let descriptor = SchemaDescriptor::new(Raw::Map(vec![
            (Raw::Int(0.into()), Raw::Int(1.into())),
            (Raw::Int(1.into()), uuid(relation_id)),
            (Raw::Int(2.into()), Raw::Array(vec![uuid([0x11; 16])])),
            (
                Raw::Int(3.into()),
                Raw::Array(vec![
                    field(
                        [0x11; 16],
                        "id",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Int".into())]),
                        0,
                    ),
                    field(
                        [0x22; 16],
                        "payload",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Blob".into())]),
                        1,
                    ),
                    field(
                        [0x33; 16],
                        "note",
                        Raw::Array(vec![Raw::Int(0.into()), Raw::Text("Str".into())]),
                        1,
                    ),
                ]),
            ),
            (Raw::Int(4.into()), Raw::Array(Vec::new())),
        ]))
        .expect("valid three-field table manifest");
        let schema_bytes = descriptor.encode().expect("encode schema descriptor");
        let schema_digest = crate::native_graph::schema_identity_v3(&schema_bytes);
        let schema_oid = schema_descriptor_node(
            root,
            algorithm,
            &schema_bytes,
            schema_bytes.len() as u64,
            schema_digest,
        );

        let media = b"media payload not read by field projection";
        let media_digest = Sha256::digest(media).into();
        let media_chunk = write_git_object(root, algorithm, "blob", media);
        let media_byte_root = write_native_node(
            root,
            algorithm,
            &NodeData::ByteIndex {
                height: 0,
                total_length: media.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: media.len() as u64,
                    target: media_chunk,
                    chunk_sha256: Some(media_digest),
                }],
            },
        );
        let blob_descriptor = write_native_node(
            root,
            algorithm,
            &NodeData::BlobDescriptor {
                length: media.len() as u64,
                raw_sha256: media_digest,
                byte_root: Some(media_byte_root),
            },
        );
        let fields = |note: &str| {
            let mut encoded = vec![0x82];
            cbor_head(&mut encoded, 6, 60111);
            cbor_head(&mut encoded, 4, 5);
            cbor_head(&mut encoded, 0, media.len() as u64);
            cbor_bytes(&mut encoded, &media_digest);
            cbor_head(&mut encoded, 3, "image/png".len() as u64);
            encoded.extend_from_slice(b"image/png");
            encoded.push(0xf6);
            cbor_bytes(&mut encoded, blob_descriptor.as_bytes());
            cbor_head(&mut encoded, 3, note.len() as u64);
            encoded.extend_from_slice(note.as_bytes());
            encoded
        };
        let inline_fields = fields("short");
        let overflow_note = "x".repeat(crate::row_store::ROW_INLINE_LIMIT + 1);
        let overflow_fields = fields(&overflow_note);
        assert!(overflow_fields.len() > crate::row_store::ROW_INLINE_LIMIT);
        let mut semantic_fields = vec![0x82];
        cbor_head(&mut semantic_fields, 6, 60112);
        cbor_head(&mut semantic_fields, 4, 4);
        cbor_head(&mut semantic_fields, 0, media.len() as u64);
        cbor_bytes(&mut semantic_fields, &media_digest);
        cbor_head(&mut semantic_fields, 3, "image/png".len() as u64);
        semantic_fields.extend_from_slice(b"image/png");
        semantic_fields.push(0xf6);
        cbor_head(&mut semantic_fields, 3, overflow_note.len() as u64);
        semantic_fields.extend_from_slice(overflow_note.as_bytes());
        let semantic_digest = Sha256::digest(&semantic_fields).into();
        let overflow_digest = Sha256::digest(&overflow_fields).into();
        let overflow_chunk = write_git_object(root, algorithm, "blob", &overflow_fields);
        let overflow_byte_root = write_native_node(
            root,
            algorithm,
            &NodeData::ByteIndex {
                height: 0,
                total_length: overflow_fields.len() as u64,
                entries: vec![ByteIndexEntry {
                    span_length: overflow_fields.len() as u64,
                    target: overflow_chunk,
                    chunk_sha256: Some(overflow_digest),
                }],
            },
        );
        let dependency_root = write_native_node(
            root,
            algorithm,
            &NodeData::DependencyIndex {
                height: 0,
                children: vec![blob_descriptor.clone()],
            },
        );
        let overflow_root = write_native_node(
            root,
            algorithm,
            &NodeData::ValueOverflow {
                encoded_length: overflow_fields.len() as u64,
                semantic_digest,
                byte_root: overflow_byte_root,
                dependency_root: Some(dependency_root),
            },
        );
        let mut overflow_value = Vec::new();
        cbor_head(&mut overflow_value, 6, 60113);
        cbor_head(&mut overflow_value, 4, 1);
        cbor_bytes(&mut overflow_value, overflow_root.as_bytes());
        let rows = rows_leaf(
            root,
            algorithm,
            &row_domain(relation_id, schema_digest),
            &[(7, inline_fields.clone()), (8, overflow_value)],
        );
        install_rows_store(root, &initial, relation_id, &schema_oid, &rows, 2);

        let pinned = context(root);
        let snapshot_id = pinned.snapshot_pin().snapshot_id();
        let database_id = pinned.require_database_id().expect("repository identity");
        let binding = pinned
            .bind_committed_table(
                "media",
                "media.orna",
                "Audio",
                relation_id,
                database_id,
                &snapshot_id,
                &descriptor,
            )
            .expect("source-qualified committed TableManifest");
        let snapshot = pinned.load_row_map(relation_id).expect("pinned row map");
        let graph = pinned
            .open_native_graph(&snapshot)
            .expect("owner-issued graph context");
        let scope = graph.open_read_scope().expect("owner read scope");
        for (key, expected_fields) in [
            (7, inline_fields.as_slice()),
            (8, overflow_fields.as_slice()),
        ] {
            let row = graph
                .lookup_row(&TypedKey::UInt(key), &scope)
                .expect("read persisted row")
                .expect("row exists");
            let structured_before = scope.structured_metadata_bytes_read();
            let media_before = scope.payload_bytes_read();
            let decoded = graph
                .decode_admitted_row_fields(&row, &binding, &scope)
                .expect("decode against source-bound TableManifest");
            assert_eq!(decoded.fields().len(), 2);
            assert_eq!(decoded.canonical_fields(), expected_fields);
            assert_eq!(decoded.fields()[0].field_id(), &[0x22; 16]);
            assert_eq!(decoded.fields()[1].field_id(), &[0x33; 16]);
            let metadata = decoded.fields()[0]
                .blob_metadata()
                .expect("Blob field identity");
            assert_eq!(metadata.length(), media.len() as u64);
            assert_eq!(metadata.sha256(), media_digest);
            assert_eq!(
                metadata.descriptor_oid().map(|oid| oid.as_bytes()),
                Some(blob_descriptor.as_bytes())
            );
            assert!(!metadata.is_hydrated());
            assert_eq!(
                scope.structured_metadata_bytes_read() - structured_before,
                expected_fields.len() as u64
            );
            assert_eq!(scope.payload_bytes_read() - media_before, 0);
        }
    }

    #[test]
    fn rejects_small_noncanonical_overflow_value() {
        let directory = repository();
        let root = directory.path();
        let initial = context(root);
        let relation_id = [0x79; 16];
        install_overflow_row_store_with_bytes(root, &initial, relation_id, &[0x81, 0x01], None);

        let pinned = context(root);
        let snapshot = pinned.load_row_map(relation_id).unwrap();
        let graph = pinned.open_native_graph(&snapshot).unwrap();
        let scope = graph.open_read_scope().unwrap();
        assert!(matches!(
            graph.lookup_row(&TypedKey::UInt(7), &scope),
            Err(crate::native_graph::GraphError::NonCanonicalData)
        ));
    }

    #[test]
    fn indexed_point_and_range_reads_walk_only_the_pages_they_need() {
        let directory = repository();
        let root = directory.path();

        // Two real ORP-1 indexes over the same key shape, one small and one
        // large. A lookup or range that walks the whole index reads more
        // objects from the large one; one that walks only the pages its
        // interval needs reads the same few objects from both.
        let small = measure_index_reads(root, [0x76; 16], 2, 2, 4);
        let large = measure_index_reads(root, [0x78; 16], 4, 4, 4);
        assert_eq!(small.index_nodes, 1 + 2 + 4);
        assert_eq!(large.index_nodes, 1 + 4 + 16);
        assert_eq!(small.rows, 16);
        assert_eq!(large.rows, 64);

        // Every bounded read is measured against what the same graph costs to
        // walk in full, so "bounded" means strictly fewer objects than an
        // unbounded read, not merely fewer than some arbitrary cap.
        assert!(
            small.point < small.full && large.point < large.full,
            "point lookup must read fewer objects than a whole-index walk: \
             {} and {} of {} and {}",
            small.point,
            large.point,
            small.full,
            large.full
        );
        // The path to the key, not the index size, sets the cost of a point
        // lookup: the same key over an index with twice the branches reads
        // less than one extra level, while a whole-index walk of the large
        // shape reads {} objects against {} for the small one.
        assert!(
            large.point <= small.point + u64::from(large.branches - small.branches) * 2,
            "point reads grew with the index instead of with the path: {} over {} then {} over {}",
            small.point,
            small.full,
            large.point,
            large.full
        );
        // A bounded range reads a page only when a key of the interval can be
        // in it, and stops at the first child whose own minimum reaches the
        // exclusive upper bound. On the large shape the interval covers a
        // small slice, so the range reads fewer objects than the walk that
        // visits every branch; on the small shape the interval already spans
        // most pages, and pruning has nothing left to skip.
        assert!(
            large.range < large.full,
            "bounded range must read fewer objects than a whole-index walk: \
             {} of {} objects, against {} and {} for the small shape",
            large.range,
            large.full,
            small.range,
            small.full
        );
        assert!(
            large.range <= small.range + u64::from(large.branches - small.branches) * 2,
            "range reads grew with the index instead of with the interval: {} over {} then {} over {}",
            small.range,
            small.full,
            large.range,
            large.full
        );
        assert_eq!(small.miss_key, None);
        assert_eq!(large.miss_key, None);
        // A missing key is an absent result rather than an error, and it reads
        // only the path to where the key would be. ORP-1 child ranges are
        // nonoverlapping and exhaustive, so exactly one child per level can
        // hold the key and a miss in it ends the lookup: later siblings are
        // never probed. Both shapes are three levels high (root, middle
        // branch, leaf page), and every native node is read as its envelope
        // tree and data blob, plus a refs tree when it names children, so a
        // miss visits exactly one node per level: two branches and one leaf.
        let tree_height = 3u64;
        let path_objects = (tree_height - 1) * 3 + 2;
        assert_eq!(small.miss, path_objects);
        assert_eq!(large.miss, path_objects);
        assert!(large.miss < large.full && small.miss < small.full);
        // The hit walks the same single path; its inline row value costs no
        // further object read.
        assert_eq!(small.point, path_objects);
        assert_eq!(large.point, path_objects);
        // A row read charges payload bytes only for a media closure, so an
        // index query that stops at root, index and row pages reports zero.
        assert_eq!(small.payload_bytes, 0);
        assert_eq!(large.payload_bytes, 0);
        println!(
            "ORP-1 index reads: small({} nodes) point={} miss={} range={} full={}, \
             large({} nodes) point={} miss={} range={} full={}",
            small.index_nodes,
            small.point,
            small.miss,
            small.range,
            small.full,
            large.index_nodes,
            large.point,
            large.miss,
            large.range,
            large.full
        );

        // A malformed branch whose fence lies about its child's range is still
        // rejected when the lookup's path descends into that child: bounding
        // the read must not silence that check.
        let pinned = context(root);
        let malformed_relation = [0x77; 16];
        install_branch_row_store(
            root,
            &pinned,
            malformed_relation,
            &[(10, 10), (30, 30), (20, 50), (60, 60)],
        );
        let tampered = context(root);
        let tampered_snapshot = tampered.load_row_map(malformed_relation).unwrap();
        let tampered_graph = tampered.open_native_graph(&tampered_snapshot).unwrap();
        let tampered_scope = tampered_graph.open_read_scope().unwrap();
        assert!(matches!(
            tampered_graph.lookup_row(&TypedKey::UInt(50), &tampered_scope),
            Err(crate::native_graph::GraphError::InvalidCount(_))
        ));
        assert!(matches!(
            tampered_graph.range_rows(
                &KeyRange::new(None, Some(TypedKey::UInt(60)), 8).unwrap(),
                &tampered_scope
            ),
            Err(crate::native_graph::GraphError::InvalidCount(_))
        ));
    }

    #[test]
    fn indexed_range_includes_fence_and_excludes_next_child_minimum() {
        let relation_id = [0x7a; 16];
        let (directory, _descriptor) = seeded_table_binding(relation_id);
        let root = directory.path();
        install_nested_row_store(root, &context(root), relation_id, 2, 2, 4);

        let pinned = context(root);
        let snapshot = pinned.load_row_map(relation_id).expect("load ORP identity");
        let graph = pinned.open_native_graph(&snapshot).expect("issue graph");
        let scope = graph.open_read_scope().expect("owner read scope");
        let range = KeyRange::new(Some(TypedKey::UInt(40)), Some(TypedKey::UInt(90)), 8)
            .expect("valid fence-aligned range");

        let rows = snapshot
            .range_with_graph_context(&graph, &scope, &range)
            .expect("bounded ORP range");
        let keys: Vec<_> = rows.iter().map(|row| row.key().clone()).collect();
        assert_eq!(
            keys,
            vec![
                TypedKey::UInt(40),
                TypedKey::UInt(50),
                TypedKey::UInt(60),
                TypedKey::UInt(70),
                TypedKey::UInt(80),
            ]
        );
        let source_row = rows[0].clone();
        assert_eq!(scope.payload_bytes_read_for_test(), 0);

        let limited = KeyRange::new(Some(TypedKey::UInt(40)), Some(TypedKey::UInt(90)), 3)
            .expect("valid limited range");
        let limited_rows = snapshot
            .range_with_graph_context(&graph, &scope, &limited)
            .expect("bounded limited range");
        assert_eq!(
            limited_rows
                .iter()
                .map(|row| row.key().clone())
                .collect::<Vec<_>>(),
            vec![TypedKey::UInt(40), TypedKey::UInt(50), TypedKey::UInt(60)]
        );

        let (later_directory, _descriptor) = seeded_table_binding(relation_id);
        let later_root = later_directory.path();
        install_nested_row_store(later_root, &context(later_root), relation_id, 2, 1, 4);
        let later_pinned = context(later_root);
        let later_snapshot = later_pinned
            .load_row_map(relation_id)
            .expect("load later ORP identity");
        let later_graph = later_pinned
            .open_native_graph(&later_snapshot)
            .expect("issue later graph context");
        let later_scope = later_graph
            .open_read_scope()
            .expect("later owner read scope");
        assert!(matches!(
            snapshot.range_with_graph_context(&later_graph, &later_scope, &range),
            Err(crate::row_store::RowStoreError::BoundaryMismatch)
        ));
        assert_eq!(later_scope.objects_read_for_test(), 0);
        let before_foreign_row = later_scope.objects_read_for_test();
        assert!(matches!(
            later_graph.resolve_row_node(
                &source_row,
                snapshot.version().primary_root(),
                &later_scope
            ),
            Err(crate::native_graph::GraphError::ContextMismatch)
        ));
        assert_eq!(later_scope.objects_read_for_test(), before_foreign_row);
    }

    /// One relation's index shape and the objects a point lookup, a missing-key
    /// lookup and a bounded range read from it.
    struct IndexReads {
        rows: u64,
        index_nodes: u64,
        branches: u32,
        point: u64,
        miss: u64,
        miss_key: Option<crate::row_store::AdmittedRow>,
        range: u64,
        full: u64,
        payload_bytes: u64,
    }

    /// Installs an ORP-1 index of the requested shape and measures the native
    /// Git objects each read costs against the pinned primary root.
    fn measure_index_reads(
        root: &Path,
        relation_id: [u8; 16],
        branches: u32,
        pages: u32,
        page_width: u64,
    ) -> IndexReads {
        let total = install_nested_row_store(
            root,
            &context(root),
            relation_id,
            branches,
            pages,
            page_width,
        );
        assert_eq!(total, u64::from(branches * pages) * page_width);
        let pinned = context(root);
        let snapshot = pinned.load_row_map(relation_id).expect("load ORP identity");
        assert!(!snapshot.is_materialized());
        let graph = pinned.open_native_graph(&snapshot).expect("issue graph");
        let scope = graph.open_read_scope().expect("owner read scope");

        let before = scope.objects_read_for_test();
        let row = graph
            .lookup_row(&TypedKey::UInt(70), &scope)
            .expect("bounded point lookup")
            .expect("row 70 is stored");
        assert_eq!(row.key(), &TypedKey::UInt(70));
        let point = scope.objects_read_for_test() - before;

        let before = scope.objects_read_for_test();
        let miss_key = graph
            .lookup_row(&TypedKey::UInt(75), &scope)
            .expect("bounded point lookup");
        let miss = scope.objects_read_for_test() - before;

        let range = KeyRange::new(Some(TypedKey::UInt(30)), Some(TypedKey::UInt(80)), 256).unwrap();
        let before = scope.objects_read_for_test();
        let ranged = snapshot
            .range_with_graph_context(&graph, &scope, &range)
            .expect("bounded range read");
        let range_reads = scope.objects_read_for_test() - before;
        let keys: Vec<_> = ranged.iter().map(|row| row.key().clone()).collect();
        assert_eq!(
            keys,
            vec![
                TypedKey::UInt(30),
                TypedKey::UInt(40),
                TypedKey::UInt(50),
                TypedKey::UInt(60),
                TypedKey::UInt(70),
            ]
        );

        // The baseline this read is measured against: the same graph walked
        // in full. Reading every page is what an unbounded lookup would cost,
        // so the bounded reads above are only bounded if they stay below it.
        // "No lower and no upper bound" is a range with the maximum permitted
        // limit, so this reads every stored row when the relation holds no
        // more than that many; the shapes below stay well inside it.
        assert!(total <= MAX_ROW_RANGE_LIMIT as u64 * 3);
        let unbounded = KeyRange::new(None, None, MAX_ROW_RANGE_LIMIT).unwrap();
        let before = scope.objects_read_for_test();
        let all = snapshot
            .range_with_graph_context(&graph, &scope, &unbounded)
            .expect("full index walk");
        let full = scope.objects_read_for_test() - before;
        assert_eq!(all.len() as u64, total);

        IndexReads {
            rows: total,
            index_nodes: 1 + u64::from(branches) + u64::from(branches * pages),
            branches,
            point,
            miss,
            miss_key,
            range: range_reads,
            full,
            payload_bytes: scope.payload_bytes_read_for_test(),
        }
    }

    #[test]
    fn repository_point_lookup_miss_stops_at_the_covering_child() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x72; 16];
        let initial = context(root);
        // Key 20 falls within child 2's range (10, 30] but is absent there.
        // ORP-1 child ranges are nonoverlapping and exhaustive, so child 2 is
        // the only child that can hold key 20 and the lookup is a miss. The
        // third child is an invalid overlapping range hidden behind fence 50;
        // it is off the miss's path and is rejected only by a lookup whose
        // path descends into it.
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

        // The graph is two levels high: a root branch (envelope tree, data
        // blob and refs tree) over leaf pages (envelope tree and data blob).
        // A point lookup reads one node per level, whether it hits or misses.
        let path_objects = 3 + 2;
        let before_miss = scope.objects_read_for_test();
        assert_eq!(
            graph
                .lookup_row(&TypedKey::UInt(20), &scope)
                .expect("a miss is an absent row"),
            None
        );
        let miss_objects = scope.objects_read_for_test() - before_miss;
        assert_eq!(miss_objects, path_objects);

        let before_malformed = scope.objects_read_for_test();
        assert!(matches!(
            graph.lookup_row(&TypedKey::UInt(50), &scope),
            Err(crate::native_graph::GraphError::InvalidCount(_))
        ));
        let malformed_objects = scope.objects_read_for_test() - before_malformed;
        assert_eq!(malformed_objects, path_objects);
        println!(
            "git-backed ORP point lookup object reads: miss={miss_objects} \
             malformed={malformed_objects} (operation_limit={MAX_RANGE_GRAPH_OBJECTS})"
        );
    }

    #[test]
    fn point_lookup_rejects_each_malformed_claim_on_its_path() {
        type Lookup = (
            Result<Option<crate::row_store::AdmittedRow>, crate::native_graph::GraphError>,
            u64,
        );
        /// Installs `index`, looks `key` up once, and reports the result with
        /// the native Git objects that lookup read.
        fn lookup(root: &Path, relation_id: [u8; 16], index: &OrpFixture, key: u64) -> Lookup {
            install_orp_fixture_row_store(root, &context(root), relation_id, index, 3);
            let pinned = context(root);
            let snapshot = pinned.load_row_map(relation_id).expect("load ORP identity");
            let graph = pinned.open_native_graph(&snapshot).expect("issue graph");
            let scope = graph.open_read_scope().expect("owner read scope");
            let before = scope.objects_read_for_test();
            let result = graph.lookup_row(&TypedKey::UInt(key), &scope);
            (result, scope.objects_read_for_test() - before)
        }
        fn invalid_count(lookup: &Lookup) -> bool {
            matches!(
                lookup.0,
                Err(crate::native_graph::GraphError::InvalidCount(_))
            )
        }

        let directory = repository();
        let root = directory.path();
        // Three branch levels, two children per branch, pages of two keys:
        // keys 10..=160, root fences 80 and 160, middle fences 40, 80, 120
        // and 160, page fences every 20. A branch node reads as its envelope
        // tree, data blob and refs tree; a leaf page as envelope and data.
        let canonical = || OrpFixture::canonical(3, 2, 2);
        let root_objects = 3;
        let path_objects = 3 + 3 + 3 + 2;

        // The canonical index answers hits and a miss along one path each.
        for (key, found) in [(10, true), (100, true), (160, true), (15, false)] {
            let (result, objects) = lookup(root, [0x60; 16], &canonical(), key);
            let row = result.expect("canonical lookup");
            assert_eq!(
                row.map(|row| row.key().clone()),
                found.then_some(TypedKey::UInt(key)),
                "key {key}"
            );
            assert_eq!(objects, path_objects, "key {key}");
        }

        // (a) The root claims 8 rows for its first middle branch, whose own
        // entries add up to 7.
        let mut index = canonical();
        index.child(0).node.child(0).row_count -= 1;
        let result = lookup(root, [0x61; 16], &index, 30);
        assert!(invalid_count(&result), "count claim: {:?}", result.0);
        assert_eq!(result.1, root_objects + 3);

        // (b) The root's fence for its first middle branch (85) is not that
        // branch's last fence (80).
        let mut index = canonical();
        index.child(0).fence = 85;
        let result = lookup(root, [0x62; 16], &index, 30);
        assert!(invalid_count(&result), "maximum claim: {:?}", result.0);
        assert_eq!(result.1, root_objects + 3);

        // (c) The second middle branch's first fence (80) does not sort above
        // the root's previous fence (80).
        let mut index = canonical();
        index.child(1).node.child(0).fence = 80;
        let result = lookup(root, [0x63; 16], &index, 140);
        assert!(invalid_count(&result), "branch lower bound: {:?}", result.0);
        assert_eq!(result.1, root_objects + 3);

        // (d) The leaf under the first child of the second middle branch
        // starts at 70, at or below the root-level lower bound (80) that the
        // lookup passes down through two first-child steps. Its count and
        // maximum still match their claims.
        let mut index = canonical();
        index.child(1).node.child(0).node.child(0).node = OrpFixture::Leaf(vec![70, 100]);
        let result = lookup(root, [0x64; 16], &index, 100);
        assert!(
            invalid_count(&result),
            "inherited lower bound: {:?}",
            result.0
        );
        assert_eq!(result.1, path_objects);

        // (e) An empty leaf under a branch. Every count above it is re-derived,
        // so the only defect is a page with no maximum to equal its fence.
        let mut index = canonical();
        index.child(0).node.child(0).node.child(0).node = OrpFixture::Leaf(Vec::new());
        index.recount();
        let result = lookup(root, [0x65; 16], &index, 10);
        assert!(invalid_count(&result), "empty leaf: {:?}", result.0);
        assert_eq!(result.1, path_objects);

        // (f) A key above the root's maximum fence is absent after reading
        // the root alone.
        let (result, objects) = lookup(root, [0x66; 16], &canonical(), 170);
        assert!(matches!(result, Ok(None)), "key above maximum: {result:?}");
        assert_eq!(objects, root_objects);
    }

    #[test]
    fn canonical_page_bytes_reproduce_the_admitted_overflow_row() {
        let directory = repository();
        let root = directory.path();
        let initial = context(root);
        let relation_id = [0x75; 16];
        let (relation_id, _) = install_overflow_row_store(root, &initial, relation_id, None);

        let admitted = context(root);
        let snapshot = admitted
            .load_row_map(relation_id)
            .expect("load pinned ORP identity");
        let graph = admitted
            .open_native_graph(&snapshot)
            .expect("issue graph context from repository snapshot");
        let scope = graph.open_read_scope().expect("owner read scope");
        let row = graph
            .lookup_row(&TypedKey::UInt(7), &scope)
            .expect("bounded point lookup")
            .expect("stored row");
        let RowValue::Overflow(reference) = row.value().clone() else {
            panic!("a kind-3 row decodes as an overflow reference");
        };

        // A verified reference already names its graph root, so the value has
        // exactly one canonical page spelling.
        let entry = RowEntry::new(TypedKey::UInt(7), RowValue::overflow(reference.clone()));
        let encoded = RowValue::page_bytes(std::slice::from_ref(&entry))
            .expect("encode the admitted row as one ORP-1 page");

        // The encoded page must decode back to the same tag and the same graph
        // root the reader resolved. Substituting the byte root would name a
        // different node and republishing the page would lose the row.
        let CborValue::Array(pairs) = decode_canonical_cbor(&encoded).expect("canonical page")
        else {
            panic!("a page is a CBOR array");
        };
        assert_eq!(pairs.len(), 1);
        let CborValue::Array(pair) = &pairs[0] else {
            panic!("a page entry is a CBOR pair");
        };
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0], CborValue::Unsigned(7));
        let CborValue::Tag(tag, payload) = &pair[1] else {
            panic!("an overflow value is a kind-3 tag");
        };
        assert_eq!(*tag, 60_113);
        let CborValue::Array(root_field) = payload.as_ref() else {
            panic!("the overflow tag payload is a one-element array");
        };
        assert_eq!(root_field.len(), 1);
        let CborValue::Bytes(root_bytes) = &root_field[0] else {
            panic!("the overflow payload names one graph root");
        };
        let resolved = reference
            .native_root()
            .expect("a verified reference has a graph root");
        assert_eq!(root_bytes.as_slice(), resolved.as_bytes());

        // A reference without a resolved root has no page spelling, so the
        // byte root can never be substituted for the graph root.
        let unresolved = RowValue::overflow(
            crate::row_store::ValueOverflowRef::new(
                reference.encoded_length(),
                *reference.semantic_digest(),
                reference.byte_root().clone(),
                reference.dependency_root().cloned(),
            )
            .expect("construct an unresolved reference"),
        );
        assert!(matches!(
            RowValue::page_bytes(&[RowEntry::new(TypedKey::UInt(7), unresolved)]),
            Err(crate::row_store::RowStoreError::UnresolvedValueOverflow)
        ));
    }

    #[test]
    fn captured_ogb2_protected_pin_is_admitted_through_committed_orp_row() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x73; 16];
        let initial = context(root);
        install_overflow_row_store(root, &initial, relation_id, None);

        let writing_context = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &writing_context, relation_id);
        let snapshot = writing_context
            .load_row_map(relation_id)
            .expect("load the committed row-map identity");
        let graph = writing_context
            .open_native_graph(&snapshot)
            .expect("admit the repository-owned graph context");
        let write_scope = graph.open_read_scope().expect("owner write scope");

        let payload_length = 2 * crate::blob_store::GEAR_MAXIMUM + 73;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let payload: Vec<u8> = (0..payload_length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let chunk_spans = crate::blob_store::GearChunker::new()
            .spans(&payload)
            .expect("chunk the test payload");
        assert!(
            chunk_spans.len() > 1,
            "fixture must contain multiple chunks"
        );
        let chunk_boundary = chunk_spans[0].offset() + chunk_spans[0].length();
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
            chunk_boundary - 1..chunk_boundary + 1,
            maximum - 1..maximum + 1,
            maximum..2 * maximum,
            length - 1..length,
            5..length - 5,
            7..7,
        ] {
            let range_scope = admitted_graph.open_read_scope().unwrap();
            let expected_payload_reads = if range.is_empty() {
                0
            } else {
                chunk_spans
                    .iter()
                    .filter(|span| {
                        span.offset() < range.end && span.offset() + span.length() > range.start
                    })
                    .map(crate::blob_store::ChunkSpan::length)
                    .sum::<u64>()
            };
            let read = admitted_graph
                .read_blob_range(&reference, range.clone(), &range_scope)
                .unwrap_or_else(|error| panic!("range {range:?} failed: {error:?}"));
            assert_eq!(
                read.bytes(),
                &payload[range.start as usize..range.end as usize],
                "range {range:?} returned the wrong bytes"
            );
            assert_eq!(
                range_scope.payload_bytes_read_for_test(),
                expected_payload_reads,
                "range {range:?} reads only payload from intersecting chunks"
            );
        }

        // Pack boundary: move every reachable object, including the OGB-2
        // chunks, into a packfile and drop the loose copies. Reads must still
        // verify full and straddling ranges against the same committed row.
        let loose_stats = admitted_graph
            .object_stats(&admitted_graph.open_read_scope().unwrap())
            .expect("count loose objects before repacking");
        git(root, &["repack", "-a", "-d", "--quiet"]);
        git(root, &["prune-packed"]);
        let packed_scope = admitted_graph.open_read_scope().unwrap();
        let packed_stats = admitted_graph
            .object_stats(&packed_scope)
            .expect("count packed objects after repacking");
        assert_eq!(
            packed_stats, loose_stats,
            "repacking must not change reachable objects"
        );
        let packed = admitted_graph
            .read_blob_range(&reference, 0..identity.length(), &packed_scope)
            .expect("verify the complete OGB-2 closure after repacking");
        assert_eq!(packed.bytes(), payload);
        for range in [0..1, maximum - 1..maximum + 1, length - 1..length] {
            let range_scope = admitted_graph.open_read_scope().unwrap();
            let read = admitted_graph
                .read_blob_range(&reference, range.clone(), &range_scope)
                .unwrap_or_else(|error| panic!("packed range {range:?} failed: {error:?}"));
            assert_eq!(
                read.bytes(),
                &payload[range.start as usize..range.end as usize],
                "packed range {range:?} returned the wrong bytes"
            );
        }

        // Advance to a different, valid OGS-1 store root while keeping a
        // committed row at the same key. A loose worktree marker under
        // `.orna/store` would instead turn the root into a non-OGS tree.
        let later_text = vec![b'y'; crate::row_store::ROW_INLINE_LIMIT + 1];
        let mut later_value = vec![0x81];
        cbor_head(&mut later_value, 3, later_text.len() as u64);
        later_value.extend_from_slice(&later_text);
        install_overflow_row_store_with_bytes(
            root,
            &admitted_context,
            relation_id,
            &later_value,
            None,
        );
        let later_context = context(root);
        let later_rows = later_context
            .load_row_map(relation_id)
            .expect("load the later committed row-map identity");
        let later_graph = later_context
            .open_native_graph(&later_rows)
            .expect("admit the later committed graph");
        assert_ne!(
            admitted_context.validate_store_root().unwrap().oid,
            later_context.validate_store_root().unwrap().oid,
            "the committed OGS-1 store root advanced"
        );
        let later_scope = later_graph.open_read_scope().expect("later read scope");
        assert!(matches!(
            later_graph.resolve_row_node(&row, &descriptor_oid, &later_scope),
            Err(crate::native_graph::GraphError::ContextMismatch)
        ));
    }

    #[test]
    fn nested_byte_index_ranges_read_only_intersecting_chunks() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x7b; 16];
        let payload_length = 112 * 1024 * 1024;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let payload: Vec<u8> = (0..payload_length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let chunk_spans = crate::blob_store::GearChunker::new()
            .spans(&payload)
            .expect("chunk the nested-index fixture");
        assert!(
            chunk_spans.len() > crate::native_graph::MAX_REFS,
            "fixture must require nested byte-index nodes"
        );

        let descriptor_oid = install_annotated_blob_row_store(
            root,
            relation_id,
            7,
            &payload,
            "application/octet-stream",
            None,
            false,
        );
        let admitted_context = context(root);
        let snapshot = admitted_context
            .load_row_map(relation_id)
            .expect("load the committed ORP row map");
        let graph = admitted_context
            .open_native_graph(&snapshot)
            .expect("admit the committed graph context");
        let scope = graph.open_read_scope().expect("read scope");
        let row = graph
            .lookup_row(&TypedKey::UInt(7), &scope)
            .unwrap()
            .expect("committed Blob row");
        let reference = graph
            .admit_blob_reference(&row, &descriptor_oid, &scope)
            .expect("admit only the descriptor stored in the row");

        let boundary = chunk_spans[crate::native_graph::MAX_REFS - 1].offset()
            + chunk_spans[crate::native_graph::MAX_REFS - 1].length();
        let range_start = boundary - 4 * 1024 * 1024;
        let range_end = (boundary + 4 * 1024 * 1024).min(payload.len() as u64);
        let range = range_start..range_end;
        let expected_payload_reads = chunk_spans
            .iter()
            .filter(|span| span.offset() < range.end && span.offset() + span.length() > range.start)
            .map(crate::blob_store::ChunkSpan::length)
            .sum::<u64>();
        let before_objects = scope.objects_read_for_test();
        let read = graph
            .read_blob_range(&reference, range.clone(), &scope)
            .unwrap_or_else(|error| panic!("nested range {range:?} failed: {error:?}"));

        assert_eq!(
            read.bytes(),
            &payload[range.start as usize..range.end as usize],
            "nested-index range returned the wrong bytes"
        );
        assert_eq!(
            scope.payload_bytes_read_for_test(),
            expected_payload_reads,
            "range reads payload only from intersecting chunks"
        );
        assert!(
            scope.objects_read_for_test() - before_objects > 2,
            "range must traverse the descriptor and nested byte-index objects"
        );
        assert_eq!(
            read.verification(),
            crate::native_graph::RangeVerification::ReturnedChunks
        );
    }

    #[test]
    fn metadata_only_projection_reads_the_row_and_no_media_payload() {
        let directory = repository();
        let root = directory.path();
        let relation_id = [0x7a; 16];
        let initial = context(root);
        install_overflow_row_store(root, &initial, relation_id, None);

        let writing_context = context(root);
        let (schema_oid, schema_digest) = schema_node_for_relation(root, &writing_context, relation_id);
        let snapshot = writing_context
            .load_row_map(relation_id)
            .expect("load the committed row-map identity");
        let graph = writing_context
            .open_native_graph(&snapshot)
            .expect("admit the repository-owned graph context");
        let write_scope = graph.open_read_scope().expect("owner write scope");

        // Two GEAR chunks, so the payload length is not a single-chunk special
        // case and the write-seam counter has to sum a real stream.
        let payload: Vec<u8> = (0..(crate::blob_store::GEAR_MAXIMUM + 4096))
            .map(|index| (index.wrapping_mul(11) & 0xff) as u8)
            .collect();
        let mut input = std::io::Cursor::new(payload.clone());
        let candidate = graph
            .capture_blob_candidate(&mut input, payload.len() as u64, &write_scope)
            .expect("write a private OGB-2 candidate closure");
        let pin = graph
            .protect_captured_blob(candidate, &write_scope)
            .expect("verify and durably protect the OGB-2 closure");
        let binding = graph
            .accept_protected_blob_pin_with_annotation(pin, "audio/mpeg", Some("mp2"))
            .expect("accept the durable pin as an annotated ORP Blob binding");
        let identity = binding.content_identity();
        let descriptor_oid = binding.descriptor_oid().clone();

        // The write seam charged exactly the payload chunks: the byte index,
        // descriptor and tree objects around them are never payload.
        assert_eq!(
            write_scope.payload_bytes_written(),
            payload.len() as u64,
            "capture charges the payload bytes and nothing else"
        );

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

        // Metadata-only projection: the row's own field tuple answers with the
        // committed length, digest and annotation.
        let scope = admitted_graph.open_read_scope().unwrap();
        let (key, metadata) = admitted_graph
            .project_blob_metadata(&TypedKey::UInt(7), 0, &scope)
            .expect("project the committed row's Blob metadata")
            .expect("the committed row has a Blob field");
        assert_eq!(key, TypedKey::UInt(7));
        assert_eq!(metadata.length(), identity.length());
        assert_eq!(metadata.sha256(), identity.sha256());
        assert_eq!(metadata.media_type(), "audio/mpeg");
        assert_eq!(metadata.suffix(), Some("mp2"));
        assert!(!metadata.is_hydrated(), "projection never holds the bytes");
        assert_eq!(
            scope.payload_bytes_read(),
            0,
            "a metadata-only projection reads no media payload byte"
        );
        assert!(
            scope.objects_read() > 0,
            "the projection still walked the committed row pages"
        );

        // The same row and graph, read for its bytes, must charge the payload.
        // This is what makes the zero above meaningful rather than vacuous.
        let read_scope = admitted_graph.open_read_scope().unwrap();
        let row = admitted_graph
            .lookup_row(&TypedKey::UInt(7), &read_scope)
            .unwrap()
            .expect("committed Blob row");
        let reference = admitted_graph
            .admit_blob_reference(&row, &descriptor_oid, &read_scope)
            .expect("admit the descriptor through its stored row");
        let expected_objects = read_scope.objects_read();
        let verified = admitted_graph
            .read_blob_range(&reference, 0..identity.length(), &read_scope)
            .expect("verify the complete OGB-2 closure");
        assert_eq!(verified.bytes(), payload);
        assert_eq!(
            read_scope.payload_bytes_read(),
            payload.len() as u64,
            "reading the bytes charges every payload byte"
        );
        assert!(read_scope.objects_read() > expected_objects);

        // A bounded range projection answers in canonical key order and, like
        // the point projection, charges no payload byte.
        let range_scope = admitted_graph.open_read_scope().unwrap();
        let range = crate::row_store::KeyRange::new(Some(TypedKey::UInt(0)), None, 8).unwrap();
        let listed = admitted_graph
            .project_blob_metadata_range(&range, 0, &range_scope)
            .expect("project the committed row interval");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, TypedKey::UInt(7));
        assert_eq!(listed[0].1, metadata);
        assert_eq!(
            range_scope.payload_bytes_read(),
            0,
            "a range listing reads no media payload byte"
        );

        // The projection is per field, and a tuple without that field yields
        // nothing rather than a fabricated summary.
        assert_eq!(
            admitted_graph
                .project_blob_metadata(&TypedKey::UInt(7), 1, &scope)
                .ok()
                .flatten(),
            None
        );
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
            .list_revision_snapshots("HEAD", commit_count)
            .expect("list every reachable revision");
        assert_eq!(all.len(), commit_count);
        assert_eq!(all[0].commit().to_hex(), head);
        assert_eq!(all[0].tree().to_hex(), head_tree);
        assert_eq!(
            graph.list_revision_snapshots("HEAD", 1).unwrap(),
            all[..1].to_vec()
        );
        assert!(graph.list_revision_snapshots("HEAD", 0).unwrap().is_empty());
        assert!(matches!(
            graph.list_revision_snapshots("HEAD", 4097),
            Err(crate::native_graph::GraphError::InventoryQuotaExceeded)
        ));

        let scope = graph.open_read_scope().unwrap();
        let stats = graph
            .object_stats(&scope)
            .expect("count objects reachable from the admitted store root");
        assert_eq!(
            stats.node_count(crate::native_graph::NodeKind::StoreRoot),
            1
        );
        assert!(stats.node_count(crate::native_graph::NodeKind::OrderedLeaf) >= 1);
        let total: u64 = stats.nodes().values().sum();
        assert!(
            total >= 3,
            "store root, relation map, and row leaf are reachable"
        );
        let again = graph
            .object_stats(&graph.open_read_scope().unwrap())
            .unwrap();
        assert_eq!(again, stats, "object counts are deterministic");
        let rows = stats.rows_by_type_name();
        assert!(
            rows.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "rows sort by type name"
        );
        assert_eq!(rows.iter().map(|(_, count)| count).sum::<u64>(), total);
        assert!(
            rows.iter()
                .any(|(name, count)| *name == "StoreRoot" && *count == 1)
        );
        assert_eq!(stats.to_quiet_line(), total.to_string());
        assert_eq!(stats.total_nodes(), total);
        let since_self = graph
            .object_stats_since(&scope, graph.store_root())
            .expect("count objects added since the current root");
        assert!(
            since_self.nodes().is_empty(),
            "nothing is added since the current root"
        );
        assert_eq!(since_self.blob_references(), 0);
        assert!(since_self.is_empty());
        assert_eq!(since_self.exit_code(), 3, "an empty store exits 3");
        assert_eq!(stats.exit_code(), 0, "a populated store exits 0");
        let total_nodes = stats.nodes().values().sum::<u64>();
        let limited = graph
            .object_stats_limited(&scope, total_nodes)
            .expect("a limit equal to the node count admits the walk");
        assert_eq!(limited, stats);
        assert!(matches!(
            graph.object_stats_limited(&scope, total_nodes - 1),
            Err(crate::native_graph::GraphError::InventoryQuotaExceeded)
        ));
        assert_eq!(stats.kind_count(), stats.rows_by_type_name().len());
        assert!(
            stats.kind_count() >= 1 && stats.kind_count() <= 8,
            "at most eight kinds exist"
        );
        let table = stats.to_table();
        let expected_header = format!(
            "{:<width$}  COUNT\n",
            "KIND",
            width = "blob_references".len()
        );
        assert!(table.starts_with(&expected_header), "table header: {table}");
        let expected_store_root = format!(
            "{:<width$}  1\n",
            "StoreRoot",
            width = "blob_references".len()
        );
        assert!(table.contains(&expected_store_root), "table row: {table}");
        assert!(table.ends_with(&format!("blob_references  {}\n", stats.blob_references())));
        let json = stats.to_json();
        assert!(
            json.starts_with("{\"nodes\":{\"StoreRoot\":1,"),
            "store root sorts first: {json}"
        );
        assert!(json.ends_with(&format!(
            "}},\"blob_references\":{},\"schema_version\":{}}}",
            stats.blob_references(),
            crate::native_graph::STATS_SCHEMA_VERSION
        )));
        assert_eq!(
            stats.schema_version(),
            1,
            "--version reports the stats schema"
        );
    }

    #[test]
    fn complete_copy_reopens_orp_blob_row_offline_after_source_removal() {
        let source = repository();
        let source_root = source.path();
        let relation_id = [0x7d; 16];
        let key = 7;
        let payload: Vec<u8> = (0..32_768)
            .map(|index| ((index * 29 + index / 13) % 251) as u8)
            .collect();
        let descriptor_oid = install_annotated_blob_row_store(
            source_root,
            relation_id,
            key,
            &payload,
            "audio/mpeg",
            Some("mp2"),
            false,
        );
        let source_repository = Repository::discover(source_root).expect("discover source");
        let archive_parent = TempDir::new().expect("create archive parent");
        let archive = archive_parent.path().join("archive");
        crate::complete_copy::export_complete_copy(&source_repository, "HEAD", &archive, &[])
            .expect("export committed ORP and Blob closure");

        // Delete the entire source object database before reconstructing or
        // reading the row. No source alternate or remote can answer this path.
        drop(source_repository);
        drop(source);

        let copy_parent = TempDir::new().expect("create copy parent");
        let copy = copy_parent.path().join("restored");
        let manifest = crate::complete_copy::restore_complete_copy(&archive, &copy)
            .expect("restore the complete copy offline");
        crate::complete_copy::materialize_complete_copy(&copy, &manifest)
            .expect("materialize the copied snapshot");
        assert!(git_output(&copy, &["remote"], None).is_empty());
        assert!(!copy.join(".git/objects/info/alternates").exists());

        let copied_repository = Repository::discover(&copy).expect("discover restored copy");
        let copied_context = copied_repository
            .open_format_context()
            .expect("admit the copied format-3 context");
        let rows = copied_context
            .load_row_map(relation_id)
            .expect("load the copied committed ORP row map");
        let graph = copied_context
            .open_native_graph(&rows)
            .expect("admit the copied native graph");

        let metadata_scope = graph.open_read_scope().expect("metadata scope");
        let (copied_key, metadata) = graph
            .project_blob_metadata(&TypedKey::UInt(key), 0, &metadata_scope)
            .expect("project copied row metadata")
            .expect("copied row exists");
        assert_eq!(copied_key, TypedKey::UInt(key));
        assert_eq!(metadata.length(), payload.len() as u64);
        let expected_sha: [u8; 32] = Sha256::digest(&payload).into();
        assert_eq!(metadata.sha256(), expected_sha);
        assert_eq!(metadata.media_type(), "audio/mpeg");
        assert_eq!(metadata.suffix(), Some("mp2"));
        assert_eq!(metadata_scope.payload_bytes_read(), 0);

        let payload_scope = graph.open_read_scope().expect("payload scope");
        let row = graph
            .lookup_row(&TypedKey::UInt(key), &payload_scope)
            .expect("lookup copied ORP row")
            .expect("copied row exists");
        let reference = graph
            .admit_blob_reference(&row, &descriptor_oid, &payload_scope)
            .expect("admit the descriptor named by the copied row");
        let read = graph
            .read_blob_range(&reference, 0..payload.len() as u64, &payload_scope)
            .expect("read the copied Blob payload");
        assert_eq!(read.bytes(), payload);
        assert_eq!(payload_scope.payload_bytes_read(), payload.len() as u64);
    }

    #[test]
    fn operational_graph_errors_are_not_classified_as_malformed() {
        use crate::native_graph::GraphError;
        for error in [
            GraphError::ReadQuotaExceeded,
            GraphError::MetadataQuotaExceeded,
            GraphError::InventoryQuotaExceeded,
            GraphError::ReadCancelled,
            GraphError::DurabilityFailed,
            GraphError::ObjectDurabilityUnavailable,
            GraphError::GitCommandFailed,
        ] {
            let mapped = super::graph_context_error(error.clone());
            assert!(
                !matches!(mapped, FormatContextError::GraphMalformed(_)),
                "operational graph error must not be GraphMalformed: {error:?} -> {mapped:?}"
            );
        }
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
